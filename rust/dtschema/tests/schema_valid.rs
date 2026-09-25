// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Schema-set validity tests, mirroring the Python `test/test-dt-validate.py`
//! `TestDTMetaSchema.test_all_metaschema_valid` and `TestDTSchema`
//! (`test_binding_schemas_valid`, `test_binding_schemas_id_is_unique`).
//!
//! These run entirely against the bundled `dtschema/` tree — no Python or `dtc`
//! required.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use dtschema::schema::{DTSchema, DtRetriever};
use jsonschema::Draft;
use serde_json::Value;

/// The bundled `dtschema/` data directory (honours `DTSCHEMA_DIR`).
fn dtschema_dir() -> PathBuf {
    dtschema::bundled_dir()
}

/// Recursively collect `*.yaml` files under `dir`.
fn collect_yaml(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect_yaml(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("yaml") {
            out.push(p);
        }
    }
}

#[test]
fn test_all_metaschema_valid() {
    // Every meta-schema is itself a valid Draft2019-09 schema.
    let mut files = Vec::new();
    collect_yaml(&dtschema_dir().join("meta-schemas"), &mut files);
    assert!(!files.is_empty(), "no meta-schemas found");
    for f in files {
        let value = dtschema::yaml::from_file(&f)
            .unwrap_or_else(|e| panic!("{}: parse error: {e}", f.display()));
        // Meta-schemas cross-reference each other via `http://devicetree.org/`
        // URIs, so building requires the bundled retriever (the Python
        // `check_schema` structural check needs no refs, but the Rust engine
        // resolves them eagerly).
        let built = jsonschema::options()
            .with_draft(Draft::Draft201909)
            .with_retriever(DtRetriever::bundled())
            .build(&value);
        assert!(
            built.is_ok(),
            "{}: not a valid Draft2019-09 schema: {}",
            f.display(),
            built.err().unwrap()
        );
    }
}

#[test]
fn test_binding_schemas_valid() {
    // Every bundled binding meta-validates cleanly against its `$schema`.
    let mut files = Vec::new();
    collect_yaml(&dtschema_dir().join("schemas"), &mut files);
    assert!(!files.is_empty(), "no bundled schemas found");
    for f in files {
        let sch = DTSchema::load(&f).unwrap_or_else(|e| panic!("{}: load error: {e}", f.display()));
        let errors = sch
            .meta_validate()
            .unwrap_or_else(|e| panic!("{}: meta-validate error: {e}", f.display()));
        assert!(
            errors.is_empty(),
            "{}: unexpected meta-validation errors:\n{}",
            f.display(),
            errors.join("\n")
        );
    }
}

#[test]
fn test_binding_schemas_id_is_unique() {
    // No two bundled bindings share a `$id`.
    let mut files = Vec::new();
    collect_yaml(&dtschema_dir().join("schemas"), &mut files);
    let mut seen: HashMap<String, PathBuf> = HashMap::new();
    for f in files {
        let value = dtschema::yaml::from_file(&f)
            .unwrap_or_else(|e| panic!("{}: parse error: {e}", f.display()));
        let id = value
            .get("$id")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{}: missing $id", f.display()))
            .to_string();
        if let Some(prev) = seen.insert(id.clone(), f.clone()) {
            panic!(
                "duplicate $id {id}:\n  {}\n  {}",
                prev.display(),
                f.display()
            );
        }
    }
}
