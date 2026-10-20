// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Small schema-shape helpers shared outside the fixup pipeline.

use std::collections::BTreeSet;

use serde_json::Value;

/// Return whether `const`/`enum`/`pattern` (or its first element) holds a
/// string.
pub fn is_string_schema(subschema: &Value) -> bool {
    let Some(obj) = subschema.as_object() else {
        return false;
    };
    for key in ["const", "enum", "pattern"] {
        let matched = match obj.get(key) {
            Some(Value::Array(a)) => a.first().map(Value::is_string).unwrap_or(false),
            Some(v) => v.is_string(),
            None => false,
        };
        if matched {
            return true;
        }
    }
    false
}

/// Return `[min, max]` as a JSON array.
pub fn get_array_range(subschema: &Value) -> Value {
    // Unwrap a single-element list.
    let sub = match subschema {
        Value::Array(a) => {
            if a.len() != 1 {
                return serde_json::json!([0, 0]);
            }
            &a[0]
        }
        other => other,
    };
    let obj = match sub.as_object() {
        Some(o) => o,
        None => return serde_json::json!([1, 0]),
    };

    let items_is_list = matches!(obj.get("items"), Some(Value::Array(_)));
    if items_is_list {
        let max = obj["items"].as_array().unwrap().len() as i64;
        let min = obj.get("minItems").and_then(Value::as_i64).unwrap_or(max);
        serde_json::json!([min, max])
    } else {
        let min = obj.get("minItems").and_then(Value::as_i64).unwrap_or(1);
        let max = obj.get("maxItems").and_then(Value::as_i64).unwrap_or(0);
        serde_json::json!([min, max])
    }
}

/// Collect every value under `lookup_key`, recursively.
fn item_generator<'a>(json_input: &'a Value, lookup_key: &str, out: &mut Vec<&'a Value>) {
    match json_input {
        Value::Object(m) => {
            for (k, v) in m {
                if k == lookup_key {
                    out.push(v);
                } else {
                    item_generator(v, lookup_key, out);
                }
            }
        }
        Value::Array(a) => {
            for item in a {
                item_generator(item, lookup_key, out);
            }
        }
        _ => {}
    }
}

/// Extract compatible strings from one node schema.
pub fn extract_node_compatibles_pub(schema: &Value) -> BTreeSet<String> {
    extract_node_compatibles(schema)
}

/// Extract compatible strings from one node schema.
fn extract_node_compatibles(schema: &Value) -> BTreeSet<String> {
    let mut compat: BTreeSet<String> = BTreeSet::new();
    if !schema.is_object() {
        return compat;
    }

    let mut enums = Vec::new();
    item_generator(schema, "enum", &mut enums);
    for l in enums {
        if let Value::Array(a) = l
            && a.first().map(Value::is_string).unwrap_or(false)
        {
            for v in a {
                if let Some(s) = v.as_str() {
                    compat.insert(s.to_string());
                }
            }
        }
    }

    let mut consts = Vec::new();
    item_generator(schema, "const", &mut consts);
    for l in consts {
        // Stringify const values even when they are not strings.
        let s = match l {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        compat.insert(s);
    }

    let mut patterns = Vec::new();
    item_generator(schema, "pattern", &mut patterns);
    for l in patterns {
        if let Some(s) = l.as_str() {
            compat.insert(s.to_string());
        }
    }

    compat
}

/// Extract compatible strings from every compatible schema in a document.
pub fn extract_compatibles(schema: &Value) -> BTreeSet<String> {
    let mut compat: BTreeSet<String> = BTreeSet::new();
    if !schema.is_object() {
        return compat;
    }
    let mut nodes = Vec::new();
    item_generator(schema, "compatible", &mut nodes);
    for sch in nodes {
        compat.extend(extract_node_compatibles(sch));
    }
    compat
}
