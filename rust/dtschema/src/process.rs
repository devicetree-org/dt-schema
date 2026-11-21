// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Schema processing pipeline: load raw binding YAML directories, meta-validate
//! and fix up each schema, then attach the `generated-types`,
//! `generated-pattern-types`, and `generated-compatibles` cache entries plus a
//! `version` marker.
//!
//! The result is the map that `dt-mk-schema` serializes and that the validator
//! consumes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde_json::Value;

use crate::schema::DTSchema;
use crate::{bundled_dir, types};

/// A fully processed schema set, keyed by `$id` (trailing `#` stripped), plus
/// the generated cache entries and `version`.
pub struct ProcessedSchemas {
    /// Insertion-ordered map of `$id` → processed schema value. Also holds the
    /// `generated-*` and `version` keys once [`finalize`](Self::finalize) runs.
    pub schemas: BTreeMap<String, Value>,
    /// compatible string → schema `$id`.
    pub compat_map: BTreeMap<String, String>,
    /// `$id`s of select-bearing schemas, applied unconditionally as `{if,then}`.
    pub always_schemas: Vec<String>,
}

/// Recursively collect `*.yaml` files under `dir`, sorted for deterministic
/// output. File order only affects commutative type-list merging, so sorting is
/// a safe, reproducible choice.
fn collect_yaml(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
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
}

/// Load one schema, check that meta-validation can run, fix it up, and tag it
/// with `type: object` and `$filename`. Any diagnostic lines are returned in
/// `warnings` so parallel processing can emit them in a deterministic file
/// order.
fn process_schema(path: &Path, warnings: &mut Vec<String>) -> Option<Value> {
    let dtsch = match DTSchema::load(path) {
        Ok(s) => s,
        Err(e) => {
            warnings.push(format!("{}: ignoring, error parsing file", path.display()));
            let _ = e;
            return None;
        }
    };

    match dtsch.check_schema_valid() {
        Ok(()) => {}
        Err(e) => {
            warnings.push(format!(
                "{}: ignoring, error in schema: {e}",
                path.display()
            ));
            return None;
        }
    }

    let mut schema = dtsch.fixup();
    if let Some(obj) = schema.as_object_mut() {
        obj.insert("type".to_string(), Value::String("object".to_string()));
        obj.insert(
            "$filename".to_string(),
            Value::String(path.to_string_lossy().to_string()),
        );
    }
    Some(schema)
}

/// Insert an already-processed schema, deduping by `$id`.
/// Warnings are appended to `warnings` for deterministic ordering.
fn add_processed(
    schemas: &mut BTreeMap<String, Value>,
    sch: Value,
    warnings: &mut Vec<String>,
) -> bool {
    let Some(id) = sch.get("$id").and_then(Value::as_str) else {
        return false;
    };
    let id = id.trim_end_matches('#').to_string();
    if schemas.contains_key(&id) {
        warnings.push(format!(
            "{}: warning: ignoring duplicate '$id' value '{id}'",
            sch.get("$filename").and_then(Value::as_str).unwrap_or("")
        ));
        return false;
    }
    schemas.insert(id, sch);
    true
}

/// Process explicit files and directories, plus the bundled core `schemas/`
/// tree when `core_schema` is set. Files are loaded, meta-validated and fixed
/// up in parallel (each is independent), then inserted in deterministic file
/// order so `$id` dedup and warning output stay stable.
pub fn process_schemas(schema_paths: &[PathBuf], core_schema: bool) -> BTreeMap<String, Value> {
    let mut schemas: BTreeMap<String, Value> = BTreeMap::new();

    // Explicit file arguments (in the given order), processed in parallel.
    let explicit: Vec<&PathBuf> = schema_paths.iter().filter(|p| p.is_file()).collect();
    let processed: Vec<(Option<Value>, Vec<String>)> = explicit
        .par_iter()
        .map(|p| {
            let mut w = Vec::new();
            (process_schema(p, &mut w), w)
        })
        .collect();
    for (sch, warns) in processed {
        for line in &warns {
            eprintln!("{line}");
        }
        if let Some(sch) = sch {
            let mut w = Vec::new();
            add_processed(&mut schemas, sch, &mut w);
            for line in &w {
                eprintln!("{line}");
            }
        }
    }

    let mut dirs: Vec<PathBuf> = schema_paths
        .iter()
        .filter(|p| p.is_dir())
        .cloned()
        .collect();
    if core_schema {
        dirs.push(bundled_dir().join("schemas"));
    }

    for dir in &dirs {
        let mut files = Vec::new();
        collect_yaml(dir, &mut files);
        // Load + fixup every file in parallel, preserving `files` order.
        let processed: Vec<(Option<Value>, Vec<String>)> = files
            .par_iter()
            .map(|f| {
                let mut w = Vec::new();
                (process_schema(f, &mut w), w)
            })
            .collect();
        let mut count = 0;
        for (sch, warns) in processed {
            for line in &warns {
                eprintln!("{line}");
            }
            if let Some(sch) = sch {
                let mut w = Vec::new();
                if add_processed(&mut schemas, sch, &mut w) {
                    count += 1;
                }
                for line in &w {
                    eprintln!("{line}");
                }
            }
        }
        if count == 0 {
            eprintln!("warning: no schema found in path: {}", dir.display());
        }
    }

    schemas
}

impl ProcessedSchemas {
    /// Build the full processed set from raw schema paths.
    pub fn build(schema_paths: &[PathBuf], core_schema: bool, version: &str) -> Self {
        let mut schemas = process_schemas(schema_paths, core_schema);

        types::make_property_type_cache(&mut schemas);
        types::make_compatible_schema(&mut schemas);

        let (compat_map, always_schemas) = Self::assemble_dispatch(&schemas);

        schemas.insert("version".to_string(), Value::String(version.to_string()));

        Self {
            schemas,
            compat_map,
            always_schemas,
        }
    }

    /// Reconstruct a processed set from a loaded processed-schema JSON document
    /// (the `dt-mk-schema -j` output). The `generated-*` entries are kept as-is;
    /// `compat_map`/`always_schemas` are rebuilt from the entries.
    pub fn from_value(value: &Value, version: &str) -> anyhow::Result<Self> {
        let obj = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("processed schema is not a JSON object"))?;
        if obj.contains_key("$id") {
            anyhow::bail!("single schema is not a processed schema set");
        }
        match obj.get("version").and_then(Value::as_str) {
            Some(v) if v == version => {}
            _ => anyhow::bail!("Processed schema out of date, delete and retry"),
        }

        let mut schemas: BTreeMap<String, Value> = BTreeMap::new();
        for (k, v) in obj {
            if k == "version" {
                continue;
            }
            schemas.insert(k.clone(), v.clone());
        }

        let (compat_map, always_schemas) = Self::assemble_dispatch(&schemas);
        schemas.insert("version".to_string(), Value::String(version.to_string()));

        Ok(Self {
            schemas,
            compat_map,
            always_schemas,
        })
    }

    /// Build the `compat_map` and `always_schemas` dispatch tables from the
    /// non-generated schema entries.
    fn assemble_dispatch(
        schemas: &BTreeMap<String, Value>,
    ) -> (BTreeMap<String, String>, Vec<String>) {
        let mut always_schemas = Vec::new();
        let mut compat_map: BTreeMap<String, String> = BTreeMap::new();
        for (key, sch) in schemas {
            if key.starts_with("generated-") && key != crate::GENERATED_COMPATIBLES_SCHEMA {
                continue;
            }
            let id = sch
                .get("$id")
                .and_then(Value::as_str)
                .unwrap_or(key)
                .trim_end_matches('#')
                .to_string();
            if let Some(sel) = sch.get("select") {
                if sel != &Value::Bool(false) {
                    always_schemas.push(id);
                }
            } else if sch
                .get("properties")
                .and_then(|p| p.get("compatible"))
                .is_some()
            {
                let compat_sch = &sch["properties"]["compatible"];
                let mut compatibles: Vec<String> =
                    crate::lib_helpers::extract_node_compatibles_pub(compat_sch)
                        .into_iter()
                        .collect();
                if compatibles.len() > 1 {
                    compatibles.retain(|c| c != "syscon" && c != "simple-mfd" && c != "simple-bus");
                }
                for c in compatibles {
                    compat_map.insert(c, id.clone());
                }
            }
        }
        (compat_map, always_schemas)
    }
}
