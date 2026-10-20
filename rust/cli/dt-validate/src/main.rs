// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! `dt-validate`: validate devicetree DTBs against the schema set.
//!
//! Accepts positional `dtbs`, `-s/--schema`, `-p/--preparse`, `-l/--limit`,
//! `-c/--compatible-match`, `-m/--show-unmatched`, `-n/--line-number`
//! (obsolete), `-v/--verbose`, `--json-output`, `--cache-dir`,
//! `-u/--url-path`, and `-V/--version`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use dtschema::cache::{CacheOptions, ValidationCache};
use dtschema::diagnostic::{
    Diagnostic, decode_diagnostic, diagnostic_text, error_diagnostic, format_error_display,
    unmatched_diagnostic,
};
use dtschema::dtb::DtValue;
use dtschema::process::ProcessedSchemas;
use dtschema::validator::{DTValidator, DtError};
use rayon::prelude::*;
use serde_json::Value;

#[derive(Parser)]
#[command(disable_version_flag = true)]
struct Args {
    /// Filename or directory of devicetree DTB input file(s).
    dtbs: Vec<PathBuf>,
    /// Preparsed schema file or path to schema files.
    #[arg(short = 's', long = "schema")]
    schema: Option<PathBuf>,
    /// Preparsed schema file (deprecated, use '-s').
    #[arg(short = 'p', long = "preparse")]
    preparse: Option<PathBuf>,
    /// Limit validation to schemas with $id matching LIMIT substring(s),
    /// separated by ':'.
    #[arg(short = 'l', long = "limit")]
    limit: Option<String>,
    /// Limit validation to schema matching nodes' most specific compatible.
    #[arg(short = 'c', long = "compatible-match")]
    compatible_match: bool,
    /// Print out node 'compatible' strings which don't match any schema.
    #[arg(short = 'm', long = "show-unmatched")]
    show_unmatched: bool,
    /// Obsolete.
    #[arg(short = 'n', long = "line-number")]
    line_number: bool,
    /// Verbose mode.
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,
    /// Write diagnostics in JSON format to the specified file.
    #[arg(long = "json-output")]
    json_output: Option<PathBuf>,
    /// Cache validation diagnostics in CACHE_DIR.
    #[arg(long = "cache-dir")]
    cache_dir: Option<PathBuf>,
    /// Additional search path for references (deprecated).
    #[arg(short = 'u', long = "url-path")]
    url_path: Option<String>,
    /// Print version number.
    #[arg(short = 'V', long = "version")]
    version: bool,
}

/// Runtime options threaded through the node walk.
struct RunOpts {
    verbose: bool,
    show_unmatched: bool,
    match_schema_file: Option<Vec<String>>,
    compatible_match: bool,
    collect_diagnostics: bool,
}

/// Per-file result, buffered so parallel workers can flush output in the
/// deterministic input order rather than racing on stderr.
#[derive(Default)]
struct FileOutput {
    /// Lines destined for stderr, in emission order.
    stderr: Vec<String>,
    /// Lines destined for stdout (verbose `Check:` notices).
    stdout: Vec<String>,
    /// JSON diagnostics (only populated when collecting).
    diagnostics: Vec<Value>,
}

impl FileOutput {
    fn flush(&self) {
        use std::io::Write;
        // Batch each stream behind a single lock to keep a file's lines
        // contiguous even if another thread's flush interleaves at the syscall
        // level (the ordering across files is still serialized by the caller).
        if !self.stdout.is_empty() {
            let out = std::io::stdout();
            let mut lock = out.lock();
            for l in &self.stdout {
                let _ = writeln!(lock, "{l}");
            }
        }
        if !self.stderr.is_empty() {
            let err = std::io::stderr();
            let mut lock = err.lock();
            for l in &self.stderr {
                let _ = writeln!(lock, "{l}");
            }
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse_from(argfile::expand_args(
        argfile::parse_fromfile,
        argfile::PREFIX,
    )?);

    if args.version {
        println!("{}", dtschema::version());
        return Ok(());
    }
    let _ = args.line_number; // obsolete, accepted for compatibility.

    // Compute the limit list, applying the deprecated url-path stripping.
    let mut match_schema_file = args
        .limit
        .as_ref()
        .map(|l| l.split(':').map(str::to_string).collect::<Vec<_>>());
    if let (Some(url_path), Some(list)) = (&args.url_path, match_schema_file.as_mut()) {
        for m in list.iter_mut() {
            for d in url_path.split(std::path::MAIN_SEPARATOR) {
                if !d.is_empty() && m.starts_with(d) {
                    *m = m[(d.len() + 1)..].to_string();
                }
            }
        }
    }

    // Resolve the schema file: -p wins over -s for compatibility.
    let schema_file: Option<PathBuf> = args.preparse.clone().or_else(|| args.schema.clone());

    // Cache setup.
    let mut cache: Option<ValidationCache> = None;
    if let Some(cache_dir) = &args.cache_dir {
        let ok_schema = schema_file.as_ref().is_some_and(|p| p.is_file());
        if !ok_schema {
            eprintln!("--cache-dir requires a schema file");
            std::process::exit(-1i32 as u8 as i32);
        }
        let opts = CacheOptions {
            compatible_match: args.compatible_match,
            limit: match_schema_file.clone(),
            show_unmatched: args.show_unmatched,
            verbose: args.verbose,
        };
        cache = Some(
            ValidationCache::new(
                cache_dir.clone(),
                schema_file.as_deref(),
                dtschema::version(),
                &opts,
            )
            .context("initializing cache")?,
        );
    }

    let collect_diagnostics = args.json_output.is_some() || cache.is_some();

    let run = RunOpts {
        verbose: args.verbose,
        show_unmatched: args.show_unmatched,
        match_schema_file,
        compatible_match: args.compatible_match,
        collect_diagnostics,
    };

    // Build the validator (once). A missing schema file is fatal.
    if let Some(sf) = &schema_file
        && !sf.exists()
    {
        std::process::exit(-1i32 as u8 as i32);
    }
    let validator = build_validator(schema_file.as_deref())?;

    // Validate every DTB. Files are independent, so run them across a rayon
    // pool (the single built `validator` is `Sync` and its compiled-schema
    // cache is shared), then flush each file's buffered output in the original
    // input order so stderr/stdout and the JSON list stay deterministic.
    let filenames = dtb_filenames(&args.dtbs);
    let outputs: Vec<FileOutput> = filenames
        .par_iter()
        .map(|filename| {
            let mut out = FileOutput::default();
            if run.verbose {
                out.stdout.push(format!("Check:  {}", filename.display()));
            }

            if let Some(cache) = &cache
                && let Some(cached) = cache.load(filename)
            {
                for d in &cached {
                    out.stderr.push(diagnostic_text(d));
                }
                out.diagnostics = cached;
                return out;
            }

            let diags = check_dtb(&validator, filename, &run, &mut out);
            if let Some(cache) = &cache {
                cache.store(filename, &diags);
            }
            out.diagnostics = diags;
            out
        })
        .collect();

    let mut all_diagnostics: Vec<Value> = Vec::new();
    for out in outputs {
        out.flush();
        all_diagnostics.extend(out.diagnostics);
    }

    if let Some(json_path) = &args.json_output {
        let mut text = serde_json::to_string_pretty(&all_diagnostics)?;
        text.push('\n');
        std::fs::write(json_path, text)
            .with_context(|| format!("writing {}", json_path.display()))?;
    }

    Ok(())
}

/// Build a [`DTValidator`] from the schema file/dir (or bundled-only when none).
fn build_validator(schema_file: Option<&Path>) -> Result<DTValidator> {
    let version = dtschema::version();
    match schema_file {
        Some(p) if p.is_file() => {
            // A processed schema JSON file.
            let processed = load_processed_schema(p, &version)?;
            DTValidator::from_processed(processed)
        }
        Some(p) => DTValidator::new(&[p.to_path_buf()]),
        None => DTValidator::new(&[]),
    }
}

/// Load a processed-schema JSON file into [`ProcessedSchemas`] using the fast
/// path for version checks and `generated-*` reuse.
fn load_processed_schema(path: &Path, version: &str) -> Result<ProcessedSchemas> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("{}: not valid JSON", path.display()))?;
    let obj = value
        .as_object()
        .with_context(|| format!("{}: processed schema is not an object", path.display()))?;

    if obj.contains_key("$id") {
        anyhow::bail!(
            "{}: looks like a single schema, not a processed schema set",
            path.display()
        );
    }
    if let Some(v) = obj.get("version").and_then(Value::as_str)
        && v != version
    {
        anyhow::bail!(
            "Processed schema out of date, delete and retry: {}",
            path.display()
        );
    }

    ProcessedSchemas::from_value(&value, version)
}

/// Decode a DTB and walk the resulting tree. Buffered diagnostics are returned;
/// human-readable lines are appended to `out`.
fn check_dtb(
    validator: &DTValidator,
    filename: &Path,
    run: &RunOpts,
    out: &mut FileOutput,
) -> Vec<Value> {
    let mut diagnostics: Vec<Value> = Vec::new();

    let data = match std::fs::read(filename) {
        Ok(d) => d,
        Err(e) => {
            out.stderr.push(format!("{}: {e}", filename.display()));
            return diagnostics;
        }
    };

    let mut decode_errors: Vec<String> = Vec::new();
    let mut tree = match validator.decode_dtb(&data, &mut decode_errors) {
        Ok(t) => t,
        Err(e) => {
            out.stderr.push(format!("{}: {e}", filename.display()));
            return diagnostics;
        }
    };
    let fname = filename.to_string_lossy();
    for msg in &decode_errors {
        // Decode errors always print; they become diagnostics only when
        // collecting (matching the previous serial behaviour).
        out.stderr.push(msg.clone());
        if run.collect_diagnostics {
            diagnostics.push(decode_diagnostic(&fname, msg).to_value());
        }
    }

    // The decoded tree is a single root node.
    check_subtree(
        validator,
        &mut tree,
        false,
        "/",
        "/",
        &fname,
        run,
        &mut diagnostics,
        out,
    );
    diagnostics
}

/// Recurse through the tree, tracking the disabled state via `status`.
#[allow(clippy::too_many_arguments)]
fn check_subtree(
    validator: &DTValidator,
    subtree: &mut DtValue,
    mut disabled: bool,
    nodename: &str,
    fullname: &str,
    filename: &str,
    run: &RunOpts,
    diagnostics: &mut Vec<Value>,
    out: &mut FileOutput,
) {
    if nodename.starts_with("__") {
        return;
    }
    {
        let DtValue::Node(map) = subtree else {
            return;
        };
        map.insert(
            "$nodename".to_string(),
            DtValue::List(vec![DtValue::Str(nodename.to_string())]),
        );
        if let Some(status) = map.get("status") {
            disabled = status_disabled(status);
        }
    }

    check_node(
        validator,
        subtree,
        disabled,
        nodename,
        fullname,
        filename,
        run,
        diagnostics,
        out,
    );

    let base = if fullname == "/" {
        String::from("/")
    } else {
        format!("{fullname}/")
    };
    let child_names: Vec<String> = match subtree {
        DtValue::Node(map) => map
            .iter()
            .filter_map(|(name, value)| {
                if matches!(value, DtValue::Node(_)) {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect(),
        _ => Vec::new(),
    };
    for name in child_names {
        if let DtValue::Node(map) = subtree
            && let Some(value) = map.get_mut(&name)
        {
            let child_full = format!("{base}{name}");
            check_subtree(
                validator,
                value,
                disabled,
                &name,
                &child_full,
                filename,
                run,
                diagnostics,
                out,
            );
        }
    }
}

/// Run the validator against one node, applying disabled-node suppression and
/// unmatched-compatible handling.
#[allow(clippy::too_many_arguments)]
fn check_node(
    validator: &DTValidator,
    node: &DtValue,
    disabled: bool,
    nodename: &str,
    fullname: &str,
    filename: &str,
    run: &RunOpts,
    diagnostics: &mut Vec<Value>,
    out: &mut FileOutput,
) {
    let DtValue::Node(map) = node else {
        return;
    };

    // Skip example nodes; their contents have already been checked elsewhere.
    if map.contains_key("example-0") || nodename.contains("example-") {
        return;
    }

    let errors = validator.iter_errors(
        node,
        run.match_schema_file.as_deref(),
        run.compatible_match,
        run.show_unmatched,
    );

    let compat = first_compatible(map);

    for error in &errors {
        // Disabled-node suppression: drop missing-property style errors.
        if (disabled || node_status_disabled(&error_instance_disabled(node, error)))
            && error.has_suppressible_disabled_context
        {
            continue;
        }

        if error.schema_file == dtschema::GENERATED_COMPATIBLES_SCHEMA {
            let compat_list = compatible_list(map);
            let diag = unmatched_diagnostic(filename, fullname, &compat_list);
            let text = diag.text();
            emit_diagnostic(diagnostics, run, diag, Some(&text), out);
            continue;
        }

        let text = format_error_display(filename, error, Some(nodename), compat.as_deref());
        let diag = error_diagnostic(
            filename,
            error,
            Some(nodename),
            Some(fullname),
            compat.as_deref(),
            Some(&text),
        );
        emit_diagnostic(diagnostics, run, diag, Some(&text), out);
    }
}

/// Emit a diagnostic to the buffered stderr and (when collecting) to the
/// diagnostics list.
fn emit_diagnostic(
    diagnostics: &mut Vec<Value>,
    run: &RunOpts,
    mut diag: Diagnostic,
    text: Option<&str>,
    out: &mut FileOutput,
) {
    if let Some(t) = text {
        diag.set_formatted_if_missing(t);
    }
    let line = text.map(str::to_string).unwrap_or_else(|| diag.text());
    out.stderr.push(line);
    if run.collect_diagnostics {
        diagnostics.push(diag.to_value());
    }
}

/// Whether a `status` value means "disabled".
fn status_disabled(status: &DtValue) -> bool {
    match status {
        DtValue::Str(s) => s.contains("disabled"),
        DtValue::List(l) => l.iter().any(status_disabled),
        _ => false,
    }
}

/// The failing node's own `status`-disabled flag, already carried by
/// [`DtError`].
fn error_instance_disabled(_node: &DtValue, error: &DtError) -> bool {
    error.instance_is_disabled_node
}

fn node_status_disabled(flag: &bool) -> bool {
    *flag
}

/// The node's first compatible string, if any.
fn first_compatible(map: &std::collections::BTreeMap<String, DtValue>) -> Option<String> {
    match map.get("compatible") {
        Some(DtValue::List(l)) => l.first().and_then(|v| match v {
            DtValue::Str(s) => Some(s.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// The node's full compatible string list.
fn compatible_list(map: &std::collections::BTreeMap<String, DtValue>) -> Vec<String> {
    match map.get("compatible") {
        Some(DtValue::List(l)) => l
            .iter()
            .filter_map(|v| match v {
                DtValue::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Expand directory arguments to `**/*.dtb`, then append plain-file arguments
/// in stable yield order.
fn dtb_filenames(dtbs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in dtbs {
        if d.is_dir() {
            collect_dtbs(d, &mut out);
        }
    }
    for f in dtbs {
        if f.is_file() {
            out.push(f.clone());
        }
    }
    out
}

fn collect_dtbs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect_dtbs(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("dtb") {
            out.push(p);
        }
    }
}
