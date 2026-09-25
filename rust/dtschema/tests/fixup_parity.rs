// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Differential parity test for the fixup pipeline.
//!
//! For every bundled and test schema, run the Rust [`DTSchema::fixup`] and diff
//! the canonical (sorted-key) JSON against the Python reference
//! `dtschema.DTSchema(f).fixup()`. Goldens are produced on the fly by invoking
//! the Python package, so the test tracks the oracle rather than a stale
//! checked-in snapshot.
//!
//! The test is skipped (not failed) when a working Python `dtschema` isn't
//! importable — CI without the Python venv still builds and runs the rest.

use dtschema::schema::DTSchema;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Locate a python interpreter with `dtschema` importable. Prefers the repo
/// `.venv`, falls back to `python3` on PATH.
fn find_python(repo: &Path) -> Option<PathBuf> {
    let venv = repo.join(".venv/bin/python3");
    let candidates = [venv, PathBuf::from("python3")];
    for c in candidates {
        let ok = Command::new(&c)
            .args(["-c", "import dtschema"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Some(c);
        }
    }
    None
}

fn canonicalize(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), canonicalize(&m[k]));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

fn collect_yaml(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_yaml(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("yaml") {
                out.push(p);
            }
        }
    }
}

#[test]
fn fixup_matches_python_oracle() {
    let repo = repo_root();
    let Some(python) = find_python(&repo) else {
        eprintln!("SKIP: no python with `dtschema` importable");
        return;
    };

    let mut files = Vec::new();
    collect_yaml(&repo.join("dtschema/schemas"), &mut files);
    collect_yaml(&repo.join("test/schemas"), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no schema files found");

    // Generate all goldens in one Python invocation, keyed by absolute path.
    let script = r#"
import sys, json, dtschema
out = {}
for f in sys.argv[1:]:
    try:
        out[f] = dtschema.DTSchema(f).fixup()
    except Exception as e:
        out[f] = {"__error__": str(e)}
json.dump(out, sys.stdout, default=str)
"#;
    let file_args: Vec<String> = files.iter().map(|f| f.to_string_lossy().into()).collect();
    let output = Command::new(&python)
        .arg("-c")
        .arg(script)
        .args(&file_args)
        .output()
        .expect("run python oracle");
    assert!(
        output.status.success(),
        "python oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let goldens: std::collections::HashMap<String, Value> =
        serde_json::from_slice(&output.stdout).expect("parse oracle json");

    let mut mismatches = Vec::new();
    for f in &files {
        let key = f.to_string_lossy().to_string();
        let Some(golden) = goldens.get(&key) else {
            continue;
        };
        if golden.get("__error__").is_some() {
            continue; // Python couldn't process it; nothing to compare.
        }
        let sch = DTSchema::load(f).expect("load schema");
        let got = canonicalize(&sch.fixup());
        let want = canonicalize(golden);
        if got != want {
            let rel = f.strip_prefix(&repo).unwrap().display().to_string();
            mismatches.push(rel);
        }
    }

    assert!(
        mismatches.is_empty(),
        "fixup output diverged from Python for {} file(s):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}
