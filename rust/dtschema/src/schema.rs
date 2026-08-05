// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Loading and meta-validation of a single binding schema (`DTSchema`).
//!
//! Loads binding YAML files, validates them against the meta-schema named by
//! `$schema`, and checks that `$id` and references resolve.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use jsonschema::{Draft, Retrieve, Uri};
use serde_json::Value;

use crate::{bundled_dir, yaml};

const BASE_URL: &str = "http://devicetree.org/";

/// Retrieve `http://devicetree.org/...` references from the bundled schema
/// tree (or an override root). Falls back to arbitrary extra roots supplied by
/// the caller, used to resolve a binding's sibling files.
pub struct DtRetriever {
    roots: Vec<PathBuf>,
    cache: Mutex<HashMap<String, Value>>,
}

impl DtRetriever {
    /// Retriever rooted at the bundled `dtschema/` directory.
    pub fn bundled() -> Self {
        Self {
            roots: vec![bundled_dir()],
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Add an extra filesystem root to resolve `http://devicetree.org/schemas/`
    /// references from (e.g. a binding's own directory).
    pub fn with_root(mut self, root: PathBuf) -> Self {
        self.roots.insert(0, root);
        self
    }

    /// Map a `http://devicetree.org/<rel>` URI to a filesystem path under each
    /// known root and load the first that exists.
    fn load_uri(&self, uri: &str) -> Option<Value> {
        let uri = uri.trim_end_matches('#');
        let rel = uri.strip_prefix(BASE_URL)?;
        // `roots` point at the bundled `dtschema/` dir; the URL path already
        // carries the `schemas/` or `meta-schemas/` segment.
        for root in &self.roots {
            let candidate = root.join(rel);
            if candidate.is_file()
                && let Ok(v) = yaml::from_file(&candidate)
            {
                return Some(v);
            }
        }
        // A binding's sibling-file roots point directly at the `schemas/`
        // subtree, so also try mapping `schemas/<x>` onto the root itself.
        if let Some(schemas_rel) = rel.strip_prefix("schemas/") {
            for root in &self.roots {
                let candidate = root.join(schemas_rel);
                if candidate.is_file()
                    && let Ok(v) = yaml::from_file(&candidate)
                {
                    return Some(v);
                }
            }
        }
        None
    }
}

impl Retrieve for DtRetriever {
    fn retrieve(
        &self,
        uri: &Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let key = uri.as_str().to_string();
        if let Some(v) = self.cache.lock().unwrap().get(&key) {
            return Ok(v.clone());
        }
        match self.load_uri(&key) {
            Some(v) => {
                self.cache.lock().unwrap().insert(key, v.clone());
                Ok(v)
            }
            None => Err(format!("no schema for {key}").into()),
        }
    }
}

/// A single binding schema file loaded into memory.
pub struct DTSchema {
    pub value: Value,
    pub filename: PathBuf,
}

impl DTSchema {
    /// Load a binding schema from a YAML file.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let value = yaml::from_file(path)?;
        Ok(Self {
            value,
            filename: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        })
    }

    /// The `$id` with any trailing `#` removed.
    pub fn id(&self) -> Option<String> {
        self.value
            .get("$id")
            .and_then(Value::as_str)
            .map(|s| s.trim_end_matches('#').to_string())
    }

    /// The meta-schema URI named by `$schema`, trailing `#` stripped.
    fn meta_schema_id(&self) -> Option<String> {
        self.value
            .get("$schema")
            .and_then(Value::as_str)
            .map(|s| s.trim_end_matches('#').to_string())
    }

    /// Validate this binding against its `$schema` meta-schema.
    ///
    /// Returns the list of human-readable validation errors (empty ⇒ valid).
    pub fn meta_validate(&self) -> anyhow::Result<Vec<String>> {
        let meta_id = self
            .meta_schema_id()
            .ok_or_else(|| anyhow::anyhow!("{}: missing $schema", self.filename.display()))?;

        let retriever = DtRetriever::bundled();
        let meta_schema = retriever
            .load_uri(&meta_id)
            .ok_or_else(|| anyhow::anyhow!("cannot load meta-schema {meta_id}"))?;

        let validator = jsonschema::options()
            .with_draft(Draft::Draft201909)
            .with_retriever(DtRetriever::bundled())
            .build(&meta_schema)
            .map_err(|e| anyhow::anyhow!("building meta-schema validator: {e}"))?;

        let mut errors: Vec<String> = validator
            .iter_errors(&self.value)
            .map(|e| format!("{}: {e}", e.instance_path()))
            .collect();
        errors.sort();
        Ok(errors)
    }

    /// True if the binding meta-validates with no errors.
    pub fn is_valid(&self) -> anyhow::Result<bool> {
        Ok(self.meta_validate()?.is_empty())
    }

    /// Check that the document is structurally valid JSON Schema. This only
    /// rejects bindings that fail the JSON Schema meta-schema; it deliberately
    /// does not compile `self.value` as a validator, because that would eagerly
    /// resolve ordinary binding `$ref`s.
    pub fn check_schema_valid(&self) -> anyhow::Result<()> {
        jsonschema::draft201909::meta::validate(&self.value)
            .map_err(|e| anyhow::anyhow!("{}", e.instance_path()))
    }

    /// Return strict meta-validation errors rendered as `dt-doc-validate`
    /// stderr lines:
    /// `<abspath>: <instance-path segments>: <message>`. A DTB carries no source
    /// positions, and neither does a YAML load here, so the legacy `line:col`
    /// field is omitted (matching the `-n` obsolete flag).
    pub fn format_errors(&self) -> anyhow::Result<Vec<String>> {
        let meta_id = self
            .meta_schema_id()
            .ok_or_else(|| anyhow::anyhow!("{}: missing $schema", self.filename.display()))?;
        let retriever = DtRetriever::bundled();
        let meta_schema = retriever
            .load_uri(&meta_id)
            .ok_or_else(|| anyhow::anyhow!("cannot load meta-schema {meta_id}"))?;
        let validator = jsonschema::options()
            .with_draft(Draft::Draft201909)
            .with_retriever(DtRetriever::bundled())
            .build(&meta_schema)
            .map_err(|e| anyhow::anyhow!("building meta-schema validator: {e}"))?;

        let abs = std::path::absolute(&self.filename)
            .unwrap_or_else(|_| self.filename.clone())
            .to_string_lossy()
            .into_owned();

        let mut errs: Vec<String> = validator
            .iter_errors(&self.value)
            .map(|e| {
                let mut src = format!("{abs}: ");
                for seg in e.instance_path().iter() {
                    match seg {
                        jsonschema::paths::LocationSegment::Property(p) => {
                            src.push_str(&p);
                            src.push(':');
                        }
                        jsonschema::paths::LocationSegment::Index(i) => {
                            src.push_str(&i.to_string());
                            src.push(':');
                        }
                    }
                }
                if e.instance_path().iter().next().is_some() {
                    src.push(' ');
                }
                format!("{src}{e}")
            })
            .collect();
        errs.sort();
        Ok(errs)
    }

    /// Run the fixup pipeline, returning the processed schema. The original
    /// `value` is left untouched.
    pub fn fixup(&self) -> Value {
        let mut processed = self.value.clone();
        crate::fixups::fixup_schema(&mut processed);
        processed
    }

    /// Emit warnings for node subschemas that constrain no undefined
    /// properties (missing `additionalProperties`/
    /// `unevaluatedProperties`). References are resolved against the bundled
    /// tree and the binding's own directory. Warnings go to stderr.
    pub fn check_schema_refs(&self) {
        // Resolve `$ref`s against the bundled tree plus the binding's directory.
        let mut retriever = DtRetriever::bundled();
        if let Some(dir) = self.filename.parent() {
            retriever = retriever.with_root(dir.to_path_buf());
        }
        let filename = self.filename.to_string_lossy().into_owned();
        check_refs_rec(&retriever, &filename, &self.value, None, None, None);
    }
}

/// Return whether a subschema describes a node: `type: object` or any of the
/// object-property keywords.
fn is_node_schema(schema: &Value) -> bool {
    let Some(obj) = schema.as_object() else {
        return false;
    };
    if obj.get("type").and_then(Value::as_str) == Some("object") {
        return true;
    }
    [
        "properties",
        "patternProperties",
        "additionalProperties",
        "unevaluatedProperties",
    ]
    .iter()
    .any(|k| obj.contains_key(*k))
}

/// Return whether the subschema forbids undefined properties, or is not a node
/// schema at all.
fn schema_allows_no_undefined_props(schema: &Value) -> bool {
    if !is_node_schema(schema) {
        return true;
    }
    let obj = schema.as_object().unwrap();
    let additional = obj.get("additionalProperties");
    let uneval = obj.get("unevaluatedProperties");
    let closed = |v: Option<&Value>| match v {
        None => false, // default True ⇒ not closed
        Some(Value::Bool(b)) => !*b,
        Some(_) => true, // a dict constraint ⇒ closed
    };
    closed(additional) || closed(uneval)
}

/// Recurse the schema tree, warning about node subschemas that neither carry
/// nor inherit an `additionalProperties`/
/// `unevaluatedProperties` constraint.
fn check_refs_rec(
    retriever: &DtRetriever,
    filename: &str,
    schema: &Value,
    parent: Option<&str>,
    is_common: Option<bool>,
    has_constraint: Option<bool>,
) {
    // At the root, `is_common` is derived from the top-level schema.
    let is_common = is_common.unwrap_or_else(|| !schema_allows_no_undefined_props(schema));
    let mut has_constraint = has_constraint.unwrap_or(false);

    match schema {
        Value::Object(obj) => {
            if matches!(
                parent,
                Some(
                    "if" | "select"
                        | "definitions"
                        | "$defs"
                        | "then"
                        | "else"
                        | "dependencies"
                        | "dependentSchemas"
                )
            ) {
                return;
            }

            if is_node_schema(schema) && !matches!(parent, Some("oneOf" | "allOf" | "anyOf")) {
                has_constraint = schema_allows_no_undefined_props(schema);
            }

            let mut ref_has_constraint = true;
            if let Some(Value::String(r)) = obj.get("$ref") {
                let uri = r.trim_end_matches('#');
                if let Ok(ref_sch) = retriever.retrieve(&uri_of(uri)) {
                    ref_has_constraint = schema_allows_no_undefined_props(&ref_sch);
                }
            }

            let has_uneval = obj.contains_key("additionalProperties")
                || obj.contains_key("unevaluatedProperties");
            if !(is_common || ref_has_constraint || has_constraint || has_uneval) {
                eprintln!(
                    "{filename}: {}: Missing additionalProperties/unevaluatedProperties constraint",
                    parent.unwrap_or("")
                );
            }

            for (k, v) in obj {
                check_refs_rec(
                    retriever,
                    filename,
                    v,
                    Some(k.as_str()),
                    Some(is_common),
                    Some(has_constraint),
                );
            }
        }
        Value::Array(items) => {
            for v in items {
                check_refs_rec(
                    retriever,
                    filename,
                    v,
                    parent,
                    Some(is_common),
                    Some(has_constraint),
                );
            }
        }
        _ => {}
    }
}

/// Parse a bare URI string into the `jsonschema::Uri` the retriever expects.
fn uri_of(s: &str) -> Uri<String> {
    Uri::parse(s.to_string()).unwrap_or_else(|_| Uri::parse(format!("{BASE_URL}schemas/")).unwrap())
}
