// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Differential harness: fixup every bundled/test schema in Rust and diff the
//! canonical (sorted-key) JSON against the Python goldens in `$GOLDEN_DIR`
//! (default `/tmp/golden/fixup`). Prints a per-file PASS/FAIL summary.
//!
//! Run: `cargo run -p dtschema --example fixupdiff`

use dtschema::schema::DTSchema;
use serde_json::Value;
use std::path::Path;

/// Recursively sort object keys so serialization is canonical.
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

fn collect(glob_dir: &str, pattern_ext: &str, out: &mut Vec<std::path::PathBuf>) {
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().and_then(|s| s.to_str()) == Some("yaml") {
                    out.push(p);
                }
            }
        }
    }
    let _ = pattern_ext;
    walk(Path::new(glob_dir), out);
}

fn main() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let golden_dir =
        std::env::var("GOLDEN_DIR").unwrap_or_else(|_| "/tmp/golden/fixup".to_string());

    let mut files = Vec::new();
    collect(
        repo.join("dtschema/schemas").to_str().unwrap(),
        "yaml",
        &mut files,
    );
    collect(
        repo.join("test/schemas").to_str().unwrap(),
        "yaml",
        &mut files,
    );
    files.sort();

    let mut pass = 0;
    let mut fail = 0;
    let mut skip = 0;
    let mut fails: Vec<String> = Vec::new();

    for f in &files {
        let rel = f.strip_prefix(&repo).unwrap().to_str().unwrap();
        let golden_name = rel.replace('/', "__") + ".json";
        let golden_path = Path::new(&golden_dir).join(&golden_name);
        if !golden_path.is_file() {
            skip += 1;
            continue;
        }
        let golden: Value =
            serde_json::from_str(&std::fs::read_to_string(&golden_path).unwrap()).unwrap();

        let sch = match DTSchema::load(f) {
            Ok(s) => s,
            Err(e) => {
                fail += 1;
                fails.push(format!("{rel}: load error: {e}"));
                continue;
            }
        };
        let got = canonicalize(&sch.fixup());
        let want = canonicalize(&golden);
        if got == want {
            pass += 1;
        } else {
            fail += 1;
            fails.push(rel.to_string());
        }
    }

    println!("PASS={pass} FAIL={fail} SKIP={skip} TOTAL={}", files.len());
    if !fails.is_empty() {
        println!("--- failures ---");
        for f in fails.iter().take(60) {
            println!("  {f}");
        }
    }
}
