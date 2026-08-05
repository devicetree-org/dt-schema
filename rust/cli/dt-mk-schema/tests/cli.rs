// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
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

fn canonical_string(v: &Value) -> String {
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
    serde_json::to_string(&canon(v)).unwrap()
}

fn normalize_generated_order(value: &mut Value) {
    let any_of = &mut value["generated-compatibles"]["properties"]["compatible"]["items"]["anyOf"];
    if let Some(items) = any_of.as_array_mut()
        && items.len() > 1
    {
        items[1..].sort_by(|a, b| a["pattern"].as_str().cmp(&b["pattern"].as_str()));
    }

    for genkey in ["generated-types", "generated-pattern-types"] {
        if let Some(props) = value[genkey]["properties"].as_object_mut() {
            for entries in props.values_mut() {
                if let Some(entries) = entries.as_array_mut() {
                    entries.sort_by_key(canonical_string);
                }
            }
        }
    }
}

#[test]
fn json_output_matches_python_by_value() {
    let repo = repo_root();
    let Some(python) = find_python(&repo) else {
        eprintln!("SKIP: no python with `dtschema` importable");
        return;
    };
    let schemas = repo.join("test/schemas");
    let args_file = std::env::temp_dir().join("dt-mk-schema-args.txt");
    std::fs::write(&args_file, format!("{}\n", schemas.display())).unwrap();
    let response_arg = format!("@{}", args_file.display());

    let rust = Command::new(env!("CARGO_BIN_EXE_dt-mk-schema"))
        .args(["-j", &response_arg])
        .current_dir(&repo)
        .output()
        .expect("run rust dt-mk-schema");
    assert!(
        rust.status.success(),
        "rust dt-mk-schema failed: {}",
        String::from_utf8_lossy(&rust.stderr)
    );

    let py = Command::new(&python)
        .args([
            "-c",
            "import sys; from dtschema.mk_schema import main; sys.exit(main())",
            "-j",
            schemas.to_str().unwrap(),
        ])
        .current_dir(&repo)
        .output()
        .expect("run python dt-mk-schema");
    assert!(
        py.status.success(),
        "python dt-mk-schema failed: {}",
        String::from_utf8_lossy(&py.stderr)
    );

    let mut got: Value = serde_json::from_slice(&rust.stdout).expect("parse rust json");
    let mut want: Value = serde_json::from_slice(&py.stdout).expect("parse python json");
    normalize_generated_order(&mut got);
    normalize_generated_order(&mut want);

    assert_eq!(got, want);
}
