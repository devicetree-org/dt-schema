// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Differential parity test for the processed-schema pipeline (`dt-mk-schema`).
//!
//! Builds the Rust processed schema set for `test/schemas` (+ bundled core) and
//! compares it against the Python `dtschema` reference as parsed JSON values.
//! The generated type caches and compatible enum/pattern lists are compared as
//! unordered sets, since their order depends on Python's `glob`/`set`
//! iteration.
//!
//! Skipped (not failed) when a working Python `dtschema` isn't importable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use dtschema::process::ProcessedSchemas;
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

fn as_set(v: &Value) -> BTreeSet<String> {
    v.as_array()
        .map(|a| a.iter().map(|x| canon(x).to_string()).collect())
        .unwrap_or_default()
}

#[test]
fn mk_schema_matches_python_oracle() {
    let repo = repo_root();
    let Some(python) = find_python(&repo) else {
        eprintln!("SKIP: no python with `dtschema` importable");
        return;
    };

    // Golden: `dt-mk-schema -j test/schemas` (includes bundled core by default).
    // mk_schema has no __main__ guard, so drive main() via argv.
    let out = Command::new(&python)
        .args([
            "-c",
            "import sys; from dtschema.mk_schema import main; sys.exit(main())",
            "-j",
        ])
        .arg(repo.join("test/schemas"))
        .current_dir(&repo)
        .output()
        .expect("run python dt-mk-schema");
    assert!(
        out.status.success(),
        "python mk-schema failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let golden: Value = serde_json::from_slice(&out.stdout).expect("parse golden json");
    let want = golden.as_object().unwrap();
    let version = golden.get("version").and_then(Value::as_str).unwrap_or("0");

    let ps = ProcessedSchemas::build(&[repo.join("test/schemas")], true, version);
    let got: BTreeMap<String, Value> = ps.schemas.clone();

    let gk: BTreeSet<&String> = got.keys().collect();
    let wk: BTreeSet<&String> = want.keys().collect();
    assert_eq!(
        gk,
        wk,
        "top-level key set differs: only_rust={:?} only_py={:?}",
        gk.difference(&wk).collect::<Vec<_>>(),
        wk.difference(&gk).collect::<Vec<_>>()
    );

    // Generated type caches: per-prop type-list compared as a set.
    for genkey in ["generated-types", "generated-pattern-types"] {
        let gp = got[genkey]["properties"].as_object().unwrap();
        let wp = want[genkey]["properties"].as_object().unwrap();
        let gpk: BTreeSet<&String> = gp.keys().collect();
        let wpk: BTreeSet<&String> = wp.keys().collect();
        assert_eq!(gpk, wpk, "{genkey}: property key set differs");
        for k in gpk {
            assert_eq!(
                as_set(&gp[k]),
                as_set(&wp[k]),
                "{genkey}: {k} type-list differs"
            );
        }
    }

    // generated-compatibles: enum + pattern lists as sets.
    let ga = &got["generated-compatibles"]["properties"]["compatible"]["items"]["anyOf"];
    let wa = &want["generated-compatibles"]["properties"]["compatible"]["items"]["anyOf"];
    assert_eq!(
        as_set(&ga[0]["enum"]),
        as_set(&wa[0]["enum"]),
        "compatibles enum differs"
    );
    let gpat: BTreeSet<&str> = ga.as_array().unwrap()[1..]
        .iter()
        .map(|e| e["pattern"].as_str().unwrap())
        .collect();
    let wpat: BTreeSet<&str> = wa.as_array().unwrap()[1..]
        .iter()
        .map(|e| e["pattern"].as_str().unwrap())
        .collect();
    assert_eq!(gpat, wpat, "compatibles patterns differ");

    // Per-schema entries: parsed-value match modulo absolute $filename.
    let mut entry_diffs = Vec::new();
    for k in got.keys() {
        if k.starts_with("generated-") || k == "version" {
            continue;
        }
        let mut g = got[k].clone();
        let mut w = want[k].clone();
        for v in [&mut g, &mut w] {
            v.as_object_mut().unwrap().remove("$filename");
        }
        if canon(&g) != canon(&w) {
            entry_diffs.push(k.clone());
        }
    }
    assert!(
        entry_diffs.is_empty(),
        "{} processed entries diverged from Python:\n{}",
        entry_diffs.len(),
        entry_diffs.join("\n")
    );
}
