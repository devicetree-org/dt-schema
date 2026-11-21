// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Property-type extraction and generated compatible schema construction.
//! Produces the `generated-types`, `generated-pattern-types`, and
//! `generated-compatibles` cache entries that a processed schema
//! (`dt-mk-schema` output) carries.
//!
//! Each property-type entry is a `serde_json::Value` object:
//! `{"type": <str|null>, "$id": [ids], "regex"?: <pattern>, "dim"?:
//! [[a,b],[c,d]]}`. `regex` is a working-only pattern string and is stripped
//! before serialization.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use serde_json::{Map, Value};

use crate::lib_helpers::{extract_compatibles, get_array_range, is_string_schema};

/// Extract a known DT property type name from `$ref` text.
static TYPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(address|flag|u?int(8|16|32|64)(-(array|matrix))?|string(-array)?|phandle(-array)?)",
    )
    .unwrap()
});
static MICROVOLT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-microvolt$").unwrap());
// '(^(?!opp)).*-hz$' — needs lookahead.
static HZ_RE: LazyLock<FancyRegex> =
    LazyLock::new(|| FancyRegex::new(r"(^(?!opp)).*-hz$").unwrap());
static REF_YAML_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.yaml#?$").unwrap());

/// Map of property name → list of type-entry objects.
pub type PropMap = BTreeMap<String, Vec<Value>>;

/// Merge two matrix dimensions. `dim` is `[[min,max],[min,max]]`.
fn merge_dim(dim1: &Value, dim2: &Value) -> Value {
    let a = dim1.as_array().unwrap();
    let b = dim2.as_array().unwrap();
    let mut d = Vec::with_capacity(2);
    for i in 0..2 {
        let a0 = a[i][0].as_i64().unwrap();
        let a1 = a[i][1].as_i64().unwrap();
        let b0 = b[i][0].as_i64().unwrap();
        let b1 = b[i][1].as_i64().unwrap();
        let mut minimum = a0.min(b0);
        let mut maximum = a1.max(b1);
        if a1.min(b1) == 0 {
            maximum = 0;
        }
        if maximum == 1 {
            minimum = 1;
        }
        d.push(serde_json::json!([minimum, maximum]));
    }
    Value::Array(d)
}

/// The `$id` list of a working prop entry contains `schema_id`.
fn id_list_contains(entry: &Value, id: &str) -> bool {
    entry
        .get("$id")
        .and_then(Value::as_array)
        .map(|a| a.iter().any(|v| v.as_str() == Some(id)))
        .unwrap_or(false)
}

fn push_id(entry: &mut Value, id: &str) {
    if !id_list_contains(entry, id) {
        entry["$id"]
            .as_array_mut()
            .unwrap()
            .push(Value::String(id.to_string()));
    }
}

fn schema_id(schema: &Value) -> &str {
    schema.get("$id").and_then(Value::as_str).unwrap_or("")
}

/// Extract one property's type entry from a subschema.
fn extract_prop_type(
    props: &mut PropMap,
    schema: &Value,
    propname: &str,
    subschema: &Value,
    is_pattern: bool,
) {
    if propname.starts_with('$') {
        return;
    }

    let sid = schema_id(schema).to_string();

    let Some(sub) = subschema.as_object() else {
        // Non-object subschemas seed a default entry only for `true`.
        if subschema == &Value::Bool(true) {
            let mut default_type = Map::new();
            default_type.insert("type".to_string(), Value::Null);
            default_type.insert("$id".to_string(), serde_json::json!([sid]));
            if is_pattern {
                default_type.insert("regex".to_string(), Value::String(propname.to_string()));
            }
            props
                .entry(propname.to_string())
                .or_insert_with(|| vec![Value::Object(default_type)]);
        }
        return;
    };

    let mut prop_type: Option<String> = None;

    // We only support local refs.
    if let Some(Value::String(rf)) = sub.get("$ref") {
        if rf.starts_with("#/") {
            if let Some(existing) = props.get(propname) {
                for p in existing {
                    if id_list_contains(p, &sid) {
                        return;
                    }
                }
            }
            // Walk the local ref path.
            let mut tmp = schema;
            let mut ok = true;
            for p in rf.split('/').skip(1) {
                match tmp.get(p) {
                    Some(v) => tmp = v,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                let tmp = tmp.clone();
                extract_prop_type(props, schema, propname, &tmp, is_pattern);
            }
        } else if rf.contains("/properties/") {
            let last = rf.rsplit('/').next().unwrap_or("");
            if last != propname {
                prop_type = Some(last.to_string());
            }
        }
    }

    // allOf/oneOf/anyOf recursion.
    for k in ["allOf", "oneOf", "anyOf"] {
        if let Some(Value::Array(arr)) = sub.get(k) {
            let arr = arr.clone();
            for v in &arr {
                extract_prop_type(props, schema, propname, v, is_pattern);
            }
        }
    }

    props.entry(propname.to_string()).or_default();

    let is_node = sub.get("type") == Some(&Value::String("object".to_string()))
        || sub.contains_key("properties")
        || sub.contains_key("patternProperties")
        || sub.contains_key("additionalProperties");

    if is_node {
        prop_type = Some("node".to_string());
    } else {
        // Infer the type name from a referenced core type schema.
        let ref_type = sub
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| TYPE_RE.find(r).map(|m| m.as_str().to_string()));
        if let Some(t) = ref_type {
            prop_type = Some(t);
        } else if sub.get("type") == Some(&Value::String("boolean".to_string())) {
            prop_type = Some("flag".to_string());
        } else if let Some(items) = sub.get("items") {
            let items_is_string = match items {
                Value::Array(a) => a.first().map(is_string_schema).unwrap_or(false),
                other => is_string_schema(other),
            };
            if items_is_string {
                prop_type = Some("string-array".to_string());
            } else if MICROVOLT_RE.is_match(propname) {
                // List-shaped matrix wrappers still infer matrix types from
                // unit suffixes.
                prop_type = Some("int32-matrix".to_string());
            } else if HZ_RE.is_match(propname).unwrap_or(false) {
                prop_type = Some("uint32-matrix".to_string());
            } else {
                prop_type = None;
            }
        } else if sub
            .get("$ref")
            .and_then(Value::as_str)
            .map(|r| REF_YAML_RE.is_match(r))
            .unwrap_or(false)
        {
            prop_type = Some("node".to_string());
        }
    }

    // Build new_prop.
    let mut new_prop = Map::new();
    new_prop.insert(
        "type".to_string(),
        match &prop_type {
            Some(t) => Value::String(t.clone()),
            None => Value::Null,
        },
    );
    new_prop.insert("$id".to_string(), serde_json::json!([sid]));
    if is_pattern {
        new_prop.insert("regex".to_string(), Value::String(propname.to_string()));
    }
    let mut new_prop: Option<Value> = Some(Value::Object(new_prop));

    let Some(prop_type) = prop_type else {
        // No type: seed the list only if empty.
        let list = props.get_mut(propname).unwrap();
        if list.is_empty() {
            list.push(new_prop.take().unwrap());
        }
        return;
    };

    // Matrix dimensions.
    let has_size =
        sub.contains_key("items") || sub.contains_key("minItems") || sub.contains_key("maxItems");
    let dim = if (prop_type == "phandle-array" || prop_type.ends_with("-matrix")) && has_size {
        let outer = get_array_range(subschema);
        let inner = if let Some(items) = sub.get("items") {
            match items {
                Value::Array(a) => get_array_range(a.first().unwrap_or(&Value::Null)),
                other => get_array_range(other),
            }
        } else {
            serde_json::json!([0, 0])
        };
        let d = serde_json::json!([outer, inner]);
        new_prop.as_mut().unwrap()["dim"] = d.clone();
        Some(d)
    } else {
        None
    };

    // Merge into existing entries.
    let list = props.get_mut(propname).unwrap();
    let mut dup_idx: Option<usize> = None;
    for (i, p) in list.iter_mut().enumerate() {
        let ptype = p.get("type").cloned().unwrap_or(Value::Null);
        if ptype.is_null() {
            dup_idx = Some(i);
            break;
        }
        let ptype_s = ptype.as_str().unwrap_or("");
        if let Some(dim) = &dim
            && (ptype_s == "phandle-array" || ptype_s.ends_with("-matrix"))
        {
            if p.get("dim").is_some() {
                let merged = merge_dim(&p["dim"], dim);
                p["dim"] = merged;
            } else {
                p["dim"] = dim.clone();
            }
            push_id(p, &sid);
            new_prop = None;
            break;
        }
        if ptype_s == prop_type {
            push_id(p, &sid);
            new_prop = None;
            break;
        }
        if prop_type.contains("string") && ptype_s.contains("string") {
            if prop_type == "string-array" {
                p["type"] = Value::String(prop_type.clone());
            }
            push_id(p, &sid);
            new_prop = None;
            break;
        }
    }

    if let Some(i) = dup_idx {
        list.remove(i);
    }
    if let Some(np) = new_prop {
        list.push(np);
    }

    // Recurse into nested node props.
    if sub.contains_key("properties")
        || sub.contains_key("patternProperties")
        || sub.contains_key("additionalProperties")
    {
        extract_subschema_types(props, schema, subschema);
    }
}

/// Recurse through a subschema and collect property type entries.
fn extract_subschema_types(props: &mut PropMap, schema: &Value, subschema: &Value) {
    let Some(sub) = subschema.as_object() else {
        return;
    };

    if let Some(ap) = sub.get("additionalProperties") {
        let ap = ap.clone();
        extract_subschema_types(props, schema, &ap);
    }

    for k in ["allOf", "oneOf", "anyOf"] {
        if let Some(Value::Array(arr)) = sub.get(k) {
            let arr = arr.clone();
            for v in &arr {
                extract_subschema_types(props, schema, v);
            }
        }
    }

    for k in ["properties", "patternProperties"] {
        if let Some(Value::Object(m)) = sub.get(k) {
            let entries: Vec<(String, Value)> =
                m.iter().map(|(p, v)| (p.clone(), v.clone())).collect();
            for (p, v) in entries {
                extract_prop_type(props, schema, &p, &v, k == "patternProperties");
            }
        }
    }
}

/// Extract property type entries from every schema.
fn extract_types(schemas: &BTreeMap<String, Value>) -> PropMap {
    let mut props: PropMap = BTreeMap::new();
    for sch in schemas.values() {
        extract_subschema_types(&mut props, sch, sch);
    }

    // Second pass: resolve propname-reference types (a type that isn't a known
    // type name refers to another property).
    let snapshot: BTreeMap<String, Option<String>> = props
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                v.first()
                    .and_then(|e| e.get("type"))
                    .and_then(Value::as_str)
                    .map(String::from),
            )
        })
        .collect();
    for prop in props.values_mut() {
        for v in prop.iter_mut() {
            let t = v.get("type").and_then(Value::as_str);
            let Some(prop_type) = t else { continue };
            if prop_type == "node" {
                continue;
            }
            if !TYPE_RE.is_match(prop_type) {
                if let Some(Some(resolved)) = snapshot.get(prop_type) {
                    v["type"] = Value::String(resolved.clone());
                }
                break;
            }
        }
    }

    props
}

/// Return exact-property and pattern-property type maps.
pub fn get_prop_types(schemas: &BTreeMap<String, Value>) -> (PropMap, PropMap) {
    let mut props = extract_types(schemas);
    let mut pat_props: PropMap = BTreeMap::new();

    // Remove aliases/generic pattern.
    props.remove(r"^[a-z][a-z0-9\-]*$");

    // Remove all node types from each list.
    for val in props.values_mut() {
        val.retain(|t| t.get("type").and_then(Value::as_str) != Some("node"));
    }
    // Drop now-empty props.
    props.retain(|_, v| !v.is_empty());

    // Split out pattern properties (those whose first entry carries a regex).
    let pat_keys: Vec<String> = props
        .iter()
        .filter(|(_, v)| v.first().map(|e| e.get("regex").is_some()).unwrap_or(false))
        .map(|(k, _)| k.clone())
        .collect();
    for key in pat_keys {
        let val = props.remove(&key).unwrap();
        // Only keep patternProperties with a non-null type.
        if val[0].get("type").map(|t| !t.is_null()).unwrap_or(false) {
            pat_props.insert(key, val);
        }
    }

    // Delete untyped entries matching a patternProperty.
    let untyped_keys: Vec<String> = props
        .iter()
        .filter(|(_, v)| v.first().map(|e| e["type"].is_null()).unwrap_or(false))
        .map(|(k, _)| k.clone())
        .collect();
    for key in untyped_keys {
        for val in pat_props.values() {
            let pat = val[0].get("regex").and_then(Value::as_str);
            let typed = val[0].get("type").map(|t| !t.is_null()).unwrap_or(false);
            if typed
                && let Some(pat) = pat
                && FancyRegex::new(pat)
                    .ok()
                    .and_then(|re| re.is_match(&key).ok())
                    .unwrap_or(false)
            {
                props.remove(&key);
                break;
            }
        }
    }

    (props, pat_props)
}

/// Strip working-only keys (`$id`, `regex`) from prop entries for
/// serialization.
fn strip_working_keys(props: &PropMap, keep_none: bool) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, list) in props {
        let mut new_list = Vec::with_capacity(list.len());
        for entry in list {
            let mut m = entry.as_object().unwrap().clone();
            m.remove("$id");
            m.remove("regex");
            new_list.push(Value::Object(m));
        }
        let _ = keep_none;
        out.insert(k.clone(), Value::Array(new_list));
    }
    out
}

/// Build the `generated-types` and `generated-pattern-types` schema entries and
/// insert them into `schemas`.
pub fn make_property_type_cache(schemas: &mut BTreeMap<String, Value>) {
    let (props, pat_props) = get_prop_types(schemas);

    let types_props = strip_working_keys(&props, true);
    schemas.insert(
        "generated-types".to_string(),
        serde_json::json!({
            "$id": "generated-types",
            "$filename": "Generated property types",
            "select": false,
            "properties": types_props,
        }),
    );

    let pat_types_props = strip_working_keys(&pat_props, true);
    schemas.insert(
        "generated-pattern-types".to_string(),
        serde_json::json!({
            "$id": "generated-pattern-types",
            "$filename": "Generated pattern property types",
            "select": false,
            "properties": pat_types_props,
        }),
    );
}

static COMPAT_SPECIAL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r".*[\^\[{\(\$].*").unwrap());
static COMPAT_WILDCARD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.[+*]").unwrap());

/// Build the `generated-compatibles` entry.
pub fn make_compatible_schema(schemas: &mut BTreeMap<String, Value>) {
    let mut enum_vals: Vec<String> = Vec::new();
    let mut patterns: Vec<String> = Vec::new();

    let mut compatible_list: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for sch in schemas.values() {
        compatible_list.extend(extract_compatibles(sch));
    }

    for c in &compatible_list {
        if COMPAT_SPECIAL_RE.is_match(c) {
            if c != r"^[a-zA-Z0-9][a-zA-Z0-9,+\-._/]+$"
                && !COMPAT_WILDCARD_RE.is_match(c)
                && c.starts_with('^')
                && c.ends_with('$')
            {
                patterns.push(c.clone());
            }
        } else {
            enum_vals.push(c.clone());
        }
    }

    enum_vals.sort();
    // anyOf: enum first, then the fixed '^foo'/'^test,' patterns, then discovered.
    let mut any_of: Vec<Value> = vec![serde_json::json!({ "enum": enum_vals })];
    any_of.push(serde_json::json!({"pattern": "^foo"}));
    any_of.push(serde_json::json!({"pattern": "^test,"}));
    for p in patterns {
        any_of.push(serde_json::json!({ "pattern": p }));
    }

    schemas.insert(
        crate::GENERATED_COMPATIBLES_SCHEMA.to_string(),
        serde_json::json!({
            "$id": crate::GENERATED_COMPATIBLES_SCHEMA,
            "$filename": "Generated schema of documented compatible strings",
            "select": true,
            "properties": {
                "compatible": { "items": { "anyOf": any_of } }
            }
        }),
    );
}
