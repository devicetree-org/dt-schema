// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Diagnostic formatting for `dt-validate`.
//!
//! Human-readable error lines, structured JSON diagnostics, and display-path
//! rewriting used by the CLI.

use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::validator::{DtError, PathSeg};

/// Return the path as the CLI shows it — relative to CWD when that stays within
/// the tree, otherwise absolute.
pub fn display_path(filename: &str) -> String {
    let abs = std::path::absolute(filename).unwrap_or_else(|_| Path::new(filename).to_path_buf());
    let cwd = std::env::current_dir().unwrap_or_default();
    match abs.strip_prefix(&cwd) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().into_owned(),
        _ => abs.to_string_lossy().into_owned(),
    }
}

/// Absolute path string used in formatted diagnostics.
fn abs_path(filename: &str) -> String {
    std::path::absolute(filename)
        .unwrap_or_else(|_| Path::new(filename).to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Render a path segment list as `a:b:0:` (trailing colon per segment).
fn path_prefix(path: &[PathSeg]) -> String {
    let mut s = String::new();
    for p in path {
        s.push_str(&p.to_display());
        s.push(':');
    }
    s
}

/// Build the `file: node (compat): path: message` line, with a trailing
/// `from schema $id:` note. `note`/`context` sub-error expansion is not
/// reachable from the `dt-validate` path: notes are always `None` there and
/// nested `context` errors are flattened by the engine.
pub fn format_error(
    filename: &str,
    error: &DtError,
    nodename: Option<&str>,
    compatible: Option<&str>,
) -> String {
    let mut src = format!("{}: ", abs_path(filename));

    if let Some(nn) = nodename {
        src.push_str(nn);
        if let Some(c) = compatible {
            src.push_str(&format!(" ({c})"));
        }
        src.push_str(": ");
    }

    if !error.instance_path.is_empty() {
        src.push_str(&path_prefix(&error.instance_path));
        src.push(' ');
    }

    let mut msg = error.message.clone();
    if !error.schema_file.is_empty() {
        msg.push_str(&format!("\n\tfrom schema $id: {}", error.schema_file));
    }

    src + &msg
}

/// Rewrite `old:`-prefixed line starts (after leading whitespace) to `new`.
/// Used to turn absolute paths into display paths in already-formatted text.
pub fn replace_filename_prefix(text: &str, old: &str, new: &str) -> String {
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        let stripped = line.trim_start();
        let indent = &line[..line.len() - stripped.len()];
        let needle = format!("{old}:");
        if stripped.starts_with(&needle) {
            out.push_str(indent);
            out.push_str(new);
            out.push_str(&stripped[old.len()..]);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Format an error, then rewrite the absolute path to the display path.
pub fn format_error_display(
    filename: &str,
    error: &DtError,
    nodename: Option<&str>,
    compatible: Option<&str>,
) -> String {
    let text = format_error(filename, error, nodename, compatible);
    replace_filename_prefix(&text, &abs_path(filename), &display_path(filename))
}

/// Render a path segment list as a JSON array (`_error_path`).
fn error_path_json(path: &[PathSeg]) -> Value {
    Value::Array(path.iter().map(PathSeg::as_json).collect())
}

/// Diagnostic severity in JSON output.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticLevel {
    Error,
    Warning,
}

/// Structured diagnostics emitted by `dt-validate`.
#[derive(Clone, Serialize)]
#[serde(tag = "type")]
pub enum Diagnostic {
    #[serde(rename = "validation")]
    Validation {
        level: DiagnosticLevel,
        file: String,
        line: Option<u64>,
        column: Option<u64>,
        node: Option<String>,
        nodename: Option<String>,
        compatible: Option<String>,
        property_path: Value,
        schema_path: Value,
        schema: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        formatted: Option<String>,
    },
    #[serde(rename = "unmatched")]
    Unmatched {
        level: DiagnosticLevel,
        file: String,
        line: Option<u64>,
        column: Option<u64>,
        node: String,
        nodename: String,
        compatible: Vec<String>,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        formatted: Option<String>,
    },
    #[serde(rename = "decode")]
    Decode {
        level: DiagnosticLevel,
        file: String,
        line: Option<u64>,
        column: Option<u64>,
        message: String,
    },
}

impl Diagnostic {
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("diagnostic serialization should not fail")
    }

    pub fn text(&self) -> String {
        match self {
            Diagnostic::Validation {
                message, formatted, ..
            } => formatted.clone().unwrap_or_else(|| message.clone()),
            Diagnostic::Unmatched {
                file,
                node,
                message,
                formatted,
                ..
            } => formatted
                .clone()
                .unwrap_or_else(|| format!("{file}: {node}: {message}")),
            Diagnostic::Decode { message, .. } => message.clone(),
        }
    }

    pub fn set_formatted_if_missing(&mut self, text: &str) {
        match self {
            Diagnostic::Validation { formatted, .. } | Diagnostic::Unmatched { formatted, .. } => {
                if formatted.is_none() {
                    *formatted = Some(text.to_string());
                }
            }
            Diagnostic::Decode { .. } => {}
        }
    }
}

/// Build the JSON record for a validation error.
pub fn error_diagnostic(
    filename: &str,
    error: &DtError,
    nodename: Option<&str>,
    fullname: Option<&str>,
    compatible: Option<&str>,
    formatted: Option<&str>,
) -> Diagnostic {
    Diagnostic::Validation {
        level: DiagnosticLevel::Error,
        file: display_path(filename),
        line: None,
        column: None,
        node: fullname.map(str::to_string),
        nodename: nodename.map(str::to_string),
        compatible: compatible.map(str::to_string),
        property_path: error_path_json(&error.instance_path),
        schema_path: error_path_json(&error.schema_path),
        schema: error.schema_file.clone(),
        message: error.message.clone(),
        formatted: formatted.map(str::to_string),
    }
}

/// Build the JSON record for a node whose compatible matched no schema.
pub fn unmatched_diagnostic(filename: &str, fullname: &str, compatible: &[String]) -> Diagnostic {
    let nodename = Path::new(fullname)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fullname.to_string());
    Diagnostic::Unmatched {
        level: DiagnosticLevel::Warning,
        file: display_path(filename),
        line: None,
        column: None,
        node: fullname.to_string(),
        nodename,
        compatible: compatible.to_vec(),
        message: unmatched_message(compatible),
        formatted: None,
    }
}

/// The JSON `message` field for an unmatched compatible diagnostic.
///
/// Keep the legacy list formatting in structured output even if stderr is only
/// expected to be similar.
fn unmatched_message(compatible: &[String]) -> String {
    format!(
        "failed to match any schema with compatible: {}",
        py_list_repr(compatible)
    )
}

/// Legacy single-quoted list representation: `['a', 'b']`.
fn py_list_repr(items: &[String]) -> String {
    let inner: Vec<String> = items
        .iter()
        .map(|s| format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")))
        .collect();
    format!("[{}]", inner.join(", "))
}

/// Build the JSON record for a byte-decode error.
pub fn decode_diagnostic(filename: &str, message: &str) -> Diagnostic {
    Diagnostic::Decode {
        level: DiagnosticLevel::Error,
        file: display_path(filename),
        line: None,
        column: None,
        message: message.to_string(),
    }
}

/// Return the stderr line for a diagnostic (`formatted` if present, else a
/// type-specific fallback).
pub fn diagnostic_text(d: &Value) -> String {
    if let Some(f) = d.get("formatted").and_then(Value::as_str) {
        return f.to_string();
    }
    match d.get("type").and_then(Value::as_str) {
        Some("unmatched") => format!(
            "{}: {}: {}",
            d["file"].as_str().unwrap_or(""),
            d["node"].as_str().unwrap_or(""),
            d["message"].as_str().unwrap_or(""),
        ),
        _ => d
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    }
}
