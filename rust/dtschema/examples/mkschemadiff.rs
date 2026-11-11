// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Differential harness for the processed-schema pipeline: build the Rust
//! processed schema set for `test/schemas` and diff it structurally against a
//! Python `dt-mk-schema -j` golden (`$MK_GOLDEN`, default
//! `/tmp/golden/mkschema-test.json`).
//!
//! Type-lists and the `generated-compatibles` enum/pattern lists are compared as
//! unordered sets (their order is Python glob/set-iteration dependent); the
//! `version` key is ignored.

use dtschema::process::ProcessedSchemas;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn canon(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), canon(&m[k]));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(canon).collect()),
        other => other.clone(),
    }
}

/// Multiset of canonical-JSON strings for an array.
fn as_set(v: &Value) -> BTreeSet<String> {
    v.as_array()
        .map(|a| a.iter().map(|x| canon(x).to_string()).collect())
        .unwrap_or_default()
}

fn main() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let golden_path =
        std::env::var("MK_GOLDEN").unwrap_or_else(|_| "/tmp/golden/mkschema-test.json".to_string());
    let golden: Value =
        serde_json::from_str(&std::fs::read_to_string(&golden_path).unwrap()).unwrap();
    let version = golden.get("version").and_then(Value::as_str).unwrap_or("0");

    let input = std::env::var("MK_INPUT").unwrap_or_else(|_| "test/schemas".to_string());
    let ps = ProcessedSchemas::build(&[repo.join(&input)], true, version);
    let got = &ps.schemas;
    let want = golden.as_object().unwrap();

    let mut problems = 0usize;

    // Key set parity.
    let gk: BTreeSet<&String> = got.keys().collect();
    let wk: BTreeSet<&String> = want.keys().collect();
    for k in wk.difference(&gk) {
        println!("MISSING key: {k}");
        problems += 1;
    }
    for k in gk.difference(&wk) {
        println!("EXTRA key: {k}");
        problems += 1;
    }

    // generated-types / generated-pattern-types: compare each prop's type-list as a set.
    for genkey in ["generated-types", "generated-pattern-types"] {
        let (Some(g), Some(w)) = (got.get(genkey), want.get(genkey)) else {
            continue;
        };
        let gp = g["properties"].as_object().unwrap();
        let wp = w["properties"].as_object().unwrap();
        let gpk: BTreeSet<&String> = gp.keys().collect();
        let wpk: BTreeSet<&String> = wp.keys().collect();
        for k in wpk.symmetric_difference(&gpk) {
            println!("{genkey}: prop key mismatch: {k}");
            problems += 1;
        }
        for k in gpk.intersection(&wpk) {
            if as_set(&gp[*k]) != as_set(&wp[*k]) {
                println!(
                    "{genkey}: {k}:\n  got  {}\n  want {}",
                    canon(&gp[*k]),
                    canon(&wp[*k])
                );
                problems += 1;
            }
        }
    }

    // generated-compatibles: compare enum + patterns as sets.
    if let (Some(g), Some(w)) = (
        got.get("generated-compatibles"),
        want.get("generated-compatibles"),
    ) {
        let ga = &g["properties"]["compatible"]["items"]["anyOf"];
        let wa = &w["properties"]["compatible"]["items"]["anyOf"];
        let genum = as_set(&ga[0]["enum"]);
        let wenum = as_set(&wa[0]["enum"]);
        if genum != wenum {
            let only_w: Vec<_> = wenum.difference(&genum).collect();
            let only_g: Vec<_> = genum.difference(&wenum).collect();
            println!("compatibles enum differs: only_want={only_w:?} only_got={only_g:?}");
            problems += 1;
        }
        let gpat: BTreeSet<String> = ga.as_array().unwrap()[1..]
            .iter()
            .map(|e| e["pattern"].as_str().unwrap().to_string())
            .collect();
        let wpat: BTreeSet<String> = wa.as_array().unwrap()[1..]
            .iter()
            .map(|e| e["pattern"].as_str().unwrap().to_string())
            .collect();
        if gpat != wpat {
            println!("compatibles patterns differ:\n  got  {gpat:?}\n  want {wpat:?}");
            problems += 1;
        }
    }

    // Per-schema-entry structural parity (ignoring $filename absolute paths).
    for k in gk.intersection(&wk) {
        if k.starts_with("generated-") || *k == "version" {
            continue;
        }
        let mut g = got[*k].clone();
        let mut w = want[*k].clone();
        for v in [&mut g, &mut w] {
            if let Some(o) = v.as_object_mut() {
                o.remove("$filename");
            }
        }
        if canon(&g) != canon(&w) {
            println!("entry differs: {k}");
            problems += 1;
        }
    }

    let _ = PathBuf::new();
    println!("\nPROBLEMS={problems}");
}
