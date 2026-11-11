// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Differential parity test for the DTB decoder (`decode_dtb`).
//!
//! For every `test/*.dts` fixture: compile it with `dtc -Odtb`, decode the blob
//! with the Rust [`dtschema::dtb`] pipeline and with the Python
//! `DTValidator.decode_dtb` reference, canonicalize both to JSON (raw bytes →
//! `{"$bytes":[..]}`, `sized_int` → plain int), and assert they are identical.
//!
//! Skipped (not failed) when a working Python `dtschema` or `dtc` isn't present.

use std::path::{Path, PathBuf};
use std::process::Command;

use dtschema::dtb::{self, TypeContext};
use serde_json::Value;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn find_python(repo: &Path) -> Option<PathBuf> {
    let venv = repo.join(".venv/bin/python3");
    for c in [venv, PathBuf::from("python3")] {
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

fn have_dtc() -> bool {
    Command::new("dtc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Python reference decode → canonical JSON string.
const PY_DECODE: &str = r#"
import json, sys, dtschema
def conv(v):
    if isinstance(v, bool): return v
    if isinstance(v, dict): return {k: conv(x) for k, x in v.items()}
    if isinstance(v, list): return [conv(x) for x in v]
    if isinstance(v, bytes): return {'$bytes': list(v)}
    if isinstance(v, int): return int(v)
    return v
data = sys.stdin.buffer.read()
val = dtschema.DTValidator([]).decode_dtb(data)
json.dump(conv(val[0]), sys.stdout, sort_keys=True)
"#;

fn canon(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let mut o = serde_json::Map::new();
            for k in keys {
                o.insert(k.clone(), canon(&m[k]));
            }
            Value::Object(o)
        }
        Value::Array(a) => Value::Array(a.iter().map(canon).collect()),
        other => other.clone(),
    }
}

#[test]
fn decode_matches_python_oracle() {
    let repo = repo_root();
    let Some(python) = find_python(&repo) else {
        eprintln!("SKIP: no python with `dtschema` importable");
        return;
    };
    if !have_dtc() {
        eprintln!("SKIP: dtc not available");
        return;
    }

    let ctx = TypeContext::new(&[]);

    let mut dts_files: Vec<PathBuf> = std::fs::read_dir(repo.join("test"))
        .expect("read test dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("dts"))
        .collect();
    dts_files.sort();
    assert!(!dts_files.is_empty(), "no .dts fixtures found");

    let mut diffs: Vec<String> = Vec::new();
    for dts in &dts_files {
        // Compile to DTB.
        let dtc = Command::new("dtc")
            .args(["-Odtb", "-o", "-"])
            .arg(dts)
            .output()
            .expect("run dtc");
        if !dtc.status.success() {
            // Some -fail fixtures may still compile; a dtc failure is a real
            // problem for the parity comparison, so record and skip it.
            diffs.push(format!("{}: dtc failed", dts.display()));
            continue;
        }
        let dtb_bytes = dtc.stdout;

        // Rust decode.
        let mut errs = Vec::new();
        let rust_tree = match dtb::decode_dtb(&ctx, &dtb_bytes, &mut errs) {
            Ok(t) => t,
            Err(e) => {
                diffs.push(format!("{}: rust decode error: {e}", dts.display()));
                continue;
            }
        };
        let rust_json = canon(&rust_tree.to_json());

        // Python decode.
        let py = Command::new(&python)
            .args(["-c", PY_DECODE])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(&dtb_bytes).unwrap();
                child.wait_with_output()
            })
            .expect("run python decode");
        if !py.status.success() {
            diffs.push(format!(
                "{}: python decode failed: {}",
                dts.display(),
                String::from_utf8_lossy(&py.stderr)
            ));
            continue;
        }
        let py_json: Value = serde_json::from_slice(&py.stdout).expect("parse python decode json");
        let py_json = canon(&py_json);

        if rust_json != py_json {
            diffs.push(format!("{}: decoded tree differs", dts.display()));
            // Emit a compact first-divergence hint.
            eprintln!("--- {} ---", dts.display());
            eprintln!("rust: {}", rust_json);
            eprintln!("py  : {}", py_json);
        }
    }

    assert!(
        diffs.is_empty(),
        "{} fixtures diverged:\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}
