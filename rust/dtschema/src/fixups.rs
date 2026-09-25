// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Expand the compact binding-schema syntax into strict JSON Schema
//! (Draft 2019-09) that the validator can consume.
//!
//! The entry point is [`fixup_schema`], which mutates a loaded binding document
//! in place. Everything operates on `serde_json::Value`. Key ordering is not
//! significant: the golden differential test canonicalises both sides with
//! sorted keys.

use regex::Regex;
use serde_json::{Map, Value};
use std::sync::LazyLock;

// ---- schema-shape helpers --------------------------------------------------

/// Return whether `subschema[key]` (or its first element, if a list) holds a
/// value matching the given predicate.
fn value_is_type(subschema: &Map<String, Value>, key: &str, pred: fn(&Value) -> bool) -> bool {
    match subschema.get(key) {
        None => false,
        Some(Value::Array(a)) => a.first().map(pred).unwrap_or(false),
        Some(v) => pred(v),
    }
}

fn is_integer(v: &Value) -> bool {
    v.is_i64() || v.is_u64()
}

fn is_string(v: &Value) -> bool {
    v.is_string()
}

/// Return whether a schema constrains integer values.
fn is_int_schema(subschema: &Value) -> bool {
    let Some(obj) = subschema.as_object() else {
        return false;
    };
    ["const", "enum", "minimum", "maximum"]
        .iter()
        .any(|k| value_is_type(obj, k, is_integer))
}

/// Return whether a schema constrains string values.
fn is_string_schema(subschema: &Value) -> bool {
    let Some(obj) = subschema.as_object() else {
        return false;
    };
    ["const", "enum", "pattern"]
        .iter()
        .any(|k| value_is_type(obj, k, is_string))
}

// ---- scalar/array/matrix fixups --------------------------------------------

const SCALAR_KEYWORDS: [&str; 6] = [
    "const",
    "enum",
    "pattern",
    "minimum",
    "maximum",
    "multipleOf",
];

/// Pop scalar keywords out into a fresh object.
fn extract_single_schemas(subschema: &mut Map<String, Value>) -> Value {
    let mut out = Map::new();
    for k in SCALAR_KEYWORDS {
        if let Some(v) = subschema.remove(k) {
            out.insert(k.to_string(), v);
        }
    }
    Value::Object(out)
}

/// Wrap string-valued schemas in an array item schema.
fn fixup_string_to_array(subschema: &mut Value) {
    if !is_string_schema(subschema) {
        return;
    }
    let obj = subschema.as_object_mut().unwrap();
    let inner = extract_single_schemas(obj);
    obj.insert("items".to_string(), Value::Array(vec![inner]));
}

/// Reshape `reg` scalar constraints into the matrix form used for DT data.
fn fixup_reg_schema(subschema: &mut Value, path: &[String]) {
    if !path.iter().any(|p| p == "reg") {
        return;
    }
    let Some(obj) = subschema.as_object() else {
        return;
    };

    // Determine the item schema to reshape.
    let item_is_int = if let Some(items) = obj.get("items") {
        match items {
            Value::Array(a) => a.first().map(is_int_schema).unwrap_or(false),
            other => is_int_schema(other),
        }
    } else {
        is_int_schema(subschema)
    };

    let has_items = obj.contains_key("items");
    if has_items && !item_is_int {
        return;
    }
    if !has_items && !is_int_schema(subschema) {
        return;
    }

    let obj = subschema.as_object_mut().unwrap();
    let extracted = if let Some(items) = obj.get("items") {
        // Extract from the item schema before overwriting the `items` entry.
        let mut item_schema = match items {
            Value::Array(a) => a
                .first()
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default(),
            Value::Object(m) => m.clone(),
            _ => Map::new(),
        };
        extract_single_schemas(&mut item_schema)
    } else {
        // When scalar constraints live on the outer `reg` schema, extraction
        // consumes them from that object.
        extract_single_schemas(obj)
    };
    let inner = serde_json::json!({"items": [extracted]});
    obj.insert("items".to_string(), Value::Array(vec![inner]));
}

/// Return whether a schema already has matrix-like item constraints.
fn is_matrix_schema(subschema: &Value) -> bool {
    let Some(obj) = subschema.as_object() else {
        return false;
    };
    let Some(items) = obj.get("items") else {
        return false;
    };
    let has_matrix_key = |m: &Value| {
        m.as_object()
            .map(|o| {
                o.contains_key("items") || o.contains_key("maxItems") || o.contains_key("minItems")
            })
            .unwrap_or(false)
    };
    match items {
        Value::Array(a) => a.iter().any(has_matrix_key),
        other => has_matrix_key(other),
    }
}

/// Remove empty `items` arrays after preserving their fixed length.
fn fixup_remove_empty_items(subschema: &mut Value) {
    let Some(obj) = subschema.as_object_mut() else {
        return;
    };
    match obj.get_mut("items") {
        None => {}
        Some(Value::Object(_)) => {
            // recurse into the single items dict
            let items = obj.get_mut("items").unwrap();
            fixup_remove_empty_items(items);
        }
        Some(Value::Array(_)) => {
            let items_len = obj["items"].as_array().unwrap().len();
            let mut all_empty = true;
            // Stop at the first non-empty item; only an all-empty list is
            // collapsed.
            {
                let arr = obj.get_mut("items").unwrap().as_array_mut().unwrap();
                for item in arr.iter_mut() {
                    if !item.is_object() {
                        continue;
                    }
                    item.as_object_mut().unwrap().remove("description");
                    fixup_remove_empty_items(item);
                    if item.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                        all_empty = false;
                        break;
                    }
                }
            }
            if all_empty {
                obj.entry("type")
                    .or_insert(Value::String("array".to_string()));
                obj.entry("maxItems")
                    .or_insert(Value::Number(items_len.into()));
                obj.entry("minItems")
                    .or_insert(Value::Number(items_len.into()));
                obj.remove("items");
            }
        }
        _ => {}
    }
}

// Keep in sync with property-units.yaml
static UNIT_TYPES_ARRAY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-(kBps|bits|percent|bp|db|mhz|sec|ms|us|ns|ps|mm|nanoamp|(micro-)?ohms|micro(amp|watt)(-hours)?|milliwatt|(femto|pico)farads|(milli)?celsius|kelvin|k?pascal)$").unwrap()
});
static UNIT_TYPES_MATRIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-(hz|microvolt)$").unwrap());

/// Apply implicit types for property names with known unit suffixes.
fn fixup_unit_suffix_props(subschema: &mut Value, path: &[String]) {
    // Scan upward for the nearest container keyword, using the previous path
    // segment as the property name.
    let rev: Vec<&String> = path.iter().rev().collect();
    let mut propname: Option<String> = None;
    for (idx, p) in rev.iter().enumerate() {
        if matches!(p.as_str(), "properties" | "$defs" | "definitions") {
            let prev_idx = if idx == 0 { rev.len() - 1 } else { idx - 1 };
            propname = Some(rev[prev_idx].to_string());
            break;
        }
    }
    let Some(propname) = propname else {
        return;
    };

    if subschema.get("$ref").is_some() {
        return;
    }

    if UNIT_TYPES_ARRAY_RE.is_match(&propname) && is_int_schema(subschema) {
        let obj = subschema.as_object_mut().unwrap();
        let inner = extract_single_schemas(obj);
        obj.insert("items".to_string(), Value::Array(vec![inner]));
    } else if UNIT_TYPES_MATRIX_RE.is_match(&propname) {
        if is_matrix_schema(subschema) {
            return;
        }
        let has_size_key = subschema
            .as_object()
            .map(|o| {
                o.contains_key("items") || o.contains_key("minItems") || o.contains_key("maxItems")
            })
            .unwrap_or(false);
        if has_size_key {
            let clone = subschema.clone();
            let obj = subschema.as_object_mut().unwrap();
            obj.insert("items".to_string(), Value::Array(vec![clone]));
            obj.remove("minItems");
            obj.remove("maxItems");
        } else if is_int_schema(subschema) {
            let obj = subschema.as_object_mut().unwrap();
            let inner = extract_single_schemas(obj);
            let matrix = serde_json::json!({"items": [inner]});
            obj.insert("items".to_string(), Value::Array(vec![matrix]));
        }
    }
}

/// Fill in fixed array sizes implied by `items`.
fn fixup_items_size(schema: &mut Value, path: &[String]) {
    match schema {
        Value::Array(arr) => {
            for l in arr.iter_mut() {
                fixup_items_size(l, path);
            }
        }
        Value::Object(obj) => {
            obj.remove("description");
            if obj.contains_key("items") {
                obj.insert("type".to_string(), Value::String("array".to_string()));

                if let Some(Value::Array(a)) = obj.get("items") {
                    let c = a.len();
                    if !obj.contains_key("minItems") {
                        obj.insert("minItems".to_string(), Value::Number(c.into()));
                    }
                    if !obj.contains_key("maxItems") {
                        obj.insert("maxItems".to_string(), Value::Number(c.into()));
                    }
                }

                let mut new_path = path.to_vec();
                new_path.push("items".to_string());
                let items = obj.get_mut("items").unwrap();
                fixup_items_size(items, &new_path);
            } else if !path.iter().any(|p| p == "then" || p == "else") {
                let has_max = obj.contains_key("maxItems");
                let has_min = obj.contains_key("minItems");
                if has_max && !has_min {
                    let v = obj.get("maxItems").unwrap().clone();
                    obj.insert("minItems".to_string(), v);
                } else if has_min && !has_max {
                    let v = obj.get("minItems").unwrap().clone();
                    obj.insert("maxItems".to_string(), v);
                }
            }
        }
        _ => {}
    }
}

/// Split legacy `dependencies` into `dependentRequired` / `dependentSchemas`.
fn fixup_schema_to_201909(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };
    let Some(Value::Object(deps)) = obj.remove("dependencies") else {
        return;
    };
    for (k, v) in deps {
        if v.is_array() {
            let dr = obj
                .entry("dependentRequired")
                .or_insert_with(|| Value::Object(Map::new()));
            dr.as_object_mut().unwrap().insert(k, v);
        } else {
            let ds = obj
                .entry("dependentSchemas")
                .or_insert_with(|| Value::Object(Map::new()));
            ds.as_object_mut().unwrap().insert(k, v);
        }
    }
}

/// Apply value-level schema fixups.
fn fixup_vals(schema: &mut Value, path: &[String]) {
    if let Some(obj) = schema.as_object_mut() {
        obj.remove("description");
    }
    fixup_reg_schema(schema, path);
    fixup_remove_empty_items(schema);
    fixup_unit_suffix_props(schema, path);
    fixup_string_to_array(schema);
    fixup_items_size(schema, path);
    fixup_schema_to_201909(schema);
}

/// Collapse simple `oneOf: [{const: ...}, ...]` forms into `enum`.
fn fixup_oneof_to_enum(schema: &mut Value, path: &[String]) {
    let Some(obj) = schema.as_object() else {
        return;
    };

    let list_key = if obj.contains_key("anyOf") {
        "anyOf"
    } else if obj.contains_key("oneOf") {
        "oneOf"
    } else if obj.get("items").map(Value::is_object).unwrap_or(false) {
        let mut new_path = path.to_vec();
        new_path.push("items".to_string());
        let items = schema.as_object_mut().unwrap().get_mut("items").unwrap();
        fixup_oneof_to_enum(items, &new_path);
        return;
    } else {
        return;
    };

    let sch_list = obj.get(list_key).unwrap().as_array().unwrap();
    let mut const_list = Vec::new();
    for l in sch_list {
        let Some(lo) = l.as_object() else {
            return;
        };
        // This is a strict-superset test, not "has any key outside this set",
        // so keys like `{const, deprecated}` still get converted.
        let has_strict_allowed_superset =
            lo.contains_key("const") && lo.contains_key("description") && lo.len() > 2;
        if !lo.contains_key("const") || has_strict_allowed_superset {
            return;
        }
        const_list.push(lo.get("const").unwrap().clone());
    }

    let obj = schema.as_object_mut().unwrap();
    obj.remove("anyOf");
    obj.remove("oneOf");
    obj.insert("enum".to_string(), Value::Array(const_list));
}

/// Walk property schemas and apply value-level fixups.
fn walk_properties(schema: &mut Value, path: &[String]) {
    if !schema.is_object() {
        return;
    }

    fixup_oneof_to_enum(schema, path);

    for cond in ["allOf", "oneOf", "anyOf"] {
        if schema.get(cond).map(Value::is_array).unwrap_or(false) {
            let mut new_path = path.to_vec();
            new_path.push(cond.to_string());
            let arr = schema.as_object_mut().unwrap().get_mut(cond).unwrap();
            if let Value::Array(items) = arr {
                for l in items.iter_mut() {
                    walk_properties(l, &new_path);
                }
            }
        }
    }

    if schema.get("then").is_some() {
        let mut new_path = path.to_vec();
        new_path.push("then".to_string());
        let then = schema.as_object_mut().unwrap().get_mut("then").unwrap();
        walk_properties(then, &new_path);
    }

    fixup_vals(schema, path);
}

/// Apply interrupt-specific schema fixups.
fn fixup_interrupts(schema: &mut Value, path: &[String]) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    // properties handling
    if let Some(Value::Object(props)) = obj.get("properties") {
        let has_int_or_ctrl =
            props.contains_key("interrupts") || props.contains_key("interrupt-controller");
        let has_parent = props.contains_key("interrupt-parent");
        let has_interrupts = props.contains_key("interrupts");
        let has_interrupts_ext = props.contains_key("interrupts-extended");
        let interrupts_clone = props.get("interrupts").cloned();

        let props_mut = obj.get_mut("properties").unwrap().as_object_mut().unwrap();
        if has_int_or_ctrl && !has_parent {
            props_mut.insert("interrupt-parent".to_string(), Value::Bool(true));
        }
        if has_interrupts && !has_interrupts_ext {
            props_mut.insert("interrupts-extended".to_string(), interrupts_clone.unwrap());
        }
    }

    // required handling
    let required_has_interrupts = obj
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().any(|v| v.as_str() == Some("interrupts")))
        .unwrap_or(false);
    let last_is_oneof = path.last().map(|s| s == "oneOf").unwrap_or(false);
    if obj.contains_key("required") && required_has_interrupts && !last_is_oneof {
        let reqlist = serde_json::json!([
            {"required": ["interrupts"]},
            {"required": ["interrupts-extended"]}
        ]);
        if obj.contains_key("oneOf") {
            let allof = obj.entry("allOf").or_insert_with(|| Value::Array(vec![]));
            allof
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({"oneOf": reqlist}));
        } else {
            obj.insert("oneOf".to_string(), reqlist);
        }
        // remove 'interrupts' from required
        if let Some(Value::Array(req)) = obj.get_mut("required") {
            if let Some(pos) = req.iter().position(|v| v.as_str() == Some("interrupts")) {
                req.remove(pos);
            }
            if req.is_empty() {
                obj.remove("required");
            }
        }
    }

    // dependentRequired handling
    if obj.contains_key("dependentRequired") {
        let dep_req = obj.get("dependentRequired").unwrap().as_object().unwrap();
        let has_interrupts = dep_req.contains_key("interrupts");
        let has_interrupts_ext = dep_req.contains_key("interrupts-extended");
        let interrupts_val = dep_req.get("interrupts").cloned();

        if has_interrupts && !has_interrupts_ext {
            obj.get_mut("dependentRequired")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("interrupts-extended".to_string(), interrupts_val.unwrap());
        }

        // Iterate props; first one whose value contains 'interrupts' triggers the
        // dependentSchemas rewrite, then break after removing.
        let prop_keys: Vec<String> = obj
            .get("dependentRequired")
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        for prop in prop_keys {
            let contains = obj["dependentRequired"][&prop]
                .as_array()
                .map(|a| a.iter().any(|v| v.as_str() == Some("interrupts")))
                .unwrap_or(false);
            if !contains {
                continue;
            }
            let ds = serde_json::json!({
                prop.clone(): {
                    "oneOf": [
                        {"required": ["interrupts"]},
                        {"required": ["interrupts-extended"]}
                    ]
                }
            });
            obj.insert("dependentSchemas".to_string(), ds);
            // Move the interrupts dependency into dependentSchemas.
            let dr = obj
                .get_mut("dependentRequired")
                .unwrap()
                .as_object_mut()
                .unwrap();
            if let Some(Value::Array(list)) = dr.get_mut(&prop) {
                if let Some(pos) = list.iter().position(|v| v.as_str() == Some("interrupts")) {
                    list.remove(pos);
                }
                if list.is_empty() {
                    dr.remove(&prop);
                    break;
                }
            }
        }

        if obj
            .get("dependentRequired")
            .and_then(Value::as_object)
            .map(|o| o.is_empty())
            .unwrap_or(false)
        {
            obj.remove("dependentRequired");
        }
    }

    // dependentSchemas handling
    if let Some(Value::Object(ds)) = obj.get("dependentSchemas")
        && ds.contains_key("interrupts")
        && !ds.contains_key("interrupts-extended")
    {
        let v = ds.get("interrupts").unwrap().clone();
        obj.get_mut("dependentSchemas")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("interrupts-extended".to_string(), v);
    }
}

static KNOWN_VARIABLE_MATRIX_PROPS: [&str; 2] = ["fsl,pins", "qcom,board-id"];
static PINCTRL_NUM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^pinctrl-[0-9]").unwrap());

/// Apply fixups for a node or nested subschema.
fn fixup_sub_schema(schema: &mut Value, path: &[String]) {
    if !schema.is_object() {
        return;
    }

    if let Some(obj) = schema.as_object_mut() {
        obj.remove("description");
    }
    fixup_schema_to_201909(schema);
    fixup_interrupts(schema, path);
    fixup_node_props(schema);

    if let Some(obj) = schema.as_object_mut()
        && obj.get("additionalProperties") == Some(&Value::Bool(true))
    {
        obj.remove("additionalProperties");
    }

    // Snapshot keys before mutating nested values.
    let keys: Vec<String> = schema.as_object().unwrap().keys().cloned().collect();

    for k in keys {
        if matches!(
            k.as_str(),
            "select" | "if" | "then" | "else" | "not" | "additionalProperties"
        ) {
            let mut new_path = path.to_vec();
            new_path.push(k.clone());
            let v = schema.as_object_mut().unwrap().get_mut(&k).unwrap();
            fixup_sub_schema(v, &new_path);
        }

        if matches!(k.as_str(), "allOf" | "anyOf" | "oneOf") {
            let mut new_path = path.to_vec();
            new_path.push(k.clone());
            if let Some(Value::Array(arr)) = schema.as_object_mut().unwrap().get_mut(&k) {
                for subschema in arr.iter_mut() {
                    fixup_sub_schema(subschema, &new_path);
                }
            }
        }

        if !matches!(
            k.as_str(),
            "dependentRequired"
                | "dependentSchemas"
                | "dependencies"
                | "properties"
                | "patternProperties"
                | "$defs"
                | "definitions"
        ) {
            continue;
        }

        // Iterate the props under this container.
        let prop_keys: Vec<String> = match schema.as_object().unwrap().get(&k) {
            Some(Value::Object(m)) => m.keys().cloned().collect(),
            _ => continue,
        };
        for prop in prop_keys {
            let is_known_matrix = KNOWN_VARIABLE_MATRIX_PROPS.contains(&prop.as_str());
            let prop_is_dict = schema.as_object().unwrap()[&k]
                .get(&prop)
                .map(Value::is_object)
                .unwrap_or(false);
            if is_known_matrix && prop_is_dict {
                let container = schema.as_object_mut().unwrap().get_mut(&k).unwrap();
                let prop_obj = container.get_mut(&prop).unwrap();
                let ref_val = prop_obj.as_object_mut().unwrap().remove("$ref");
                let mut replacement = Map::new();
                if let Some(r) = ref_val {
                    replacement.insert("$ref".to_string(), r);
                }
                container
                    .as_object_mut()
                    .unwrap()
                    .insert(prop.clone(), Value::Object(replacement));
                continue;
            }

            let mut new_path = path.to_vec();
            new_path.push(k.clone());
            new_path.push(prop.clone());
            let prop_val = schema
                .as_object_mut()
                .unwrap()
                .get_mut(&k)
                .unwrap()
                .get_mut(&prop)
                .unwrap();
            walk_properties(prop_val, &new_path);
            fixup_sub_schema(prop_val, &new_path);
        }
    }
}

/// Add implicit node properties and pattern properties.
fn fixup_node_props(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };
    if !obj.contains_key("unevaluatedProperties") && !obj.contains_key("additionalProperties") {
        return;
    }

    let mut keys: Vec<String> = Vec::new();
    if let Some(Value::Object(p)) = obj.get("properties") {
        keys.extend(p.keys().cloned());
    }
    if let Some(Value::Object(pp)) = obj.get("patternProperties") {
        keys.extend(pp.keys().cloned());
    }

    if keys.iter().any(|k| k == "clocks") && !keys.iter().any(|k| k == "assigned-clocks") {
        let props = obj
            .entry("properties")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .unwrap();
        for name in [
            "assigned-clocks",
            "assigned-clock-rates-u64",
            "assigned-clock-rates",
            "assigned-clock-parents",
            "assigned-clock-sscs",
        ] {
            props.insert(name.to_string(), Value::Bool(true));
        }
    }

    if keys.iter().any(|k| k == "ranges") {
        let props = obj
            .entry("properties")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .unwrap();
        props.entry("dma-ranges").or_insert(Value::Bool(true));
    }

    // If no restrictions on undefined properties, no implicit properties needed.
    let addl_true = obj.get("additionalProperties") == Some(&Value::Bool(true));
    let uneval_true = obj.get("unevaluatedProperties") == Some(&Value::Bool(true));
    if addl_true || uneval_true {
        return;
    }

    let props = obj
        .entry("properties")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .unwrap();
    for name in [
        "phandle",
        "status",
        "secure-status",
        "$nodename",
        "bootph-pre-sram",
        "bootph-verify",
        "bootph-pre-ram",
        "bootph-some-ram",
        "bootph-all",
    ] {
        props.entry(name).or_insert(Value::Bool(true));
    }

    let has_pinctrl_num = keys.iter().any(|k| PINCTRL_NUM_RE.is_match(k));
    if !has_pinctrl_num {
        obj.get_mut("properties")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .entry("pinctrl-names")
            .or_insert(Value::Bool(true));
        let pp = obj
            .entry("patternProperties")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .unwrap();
        pp.insert("^pinctrl-[0-9]+$".to_string(), Value::Bool(true));
    }
}

/// Clone a schema value; `serde_json::Value` is already a plain data tree.
fn convert_to_dict(schema: &Value) -> Value {
    schema.clone()
}

/// Add a `select` schema from a constrained `$nodename` when possible.
fn add_select_schema(schema: &mut Value) {
    let Some(obj) = schema.as_object() else {
        return;
    };
    if obj.contains_key("select") {
        return;
    }
    let Some(Value::Object(props)) = obj.get("properties") else {
        return;
    };
    if props.contains_key("compatible") {
        return;
    }
    let Some(nodename) = props.get("$nodename") else {
        return;
    };
    if nodename == &Value::Bool(true) {
        return;
    }
    let nodename_conv = convert_to_dict(nodename);
    let select = serde_json::json!({
        "required": ["$nodename"],
        "properties": {"$nodename": nodename_conv}
    });
    schema
        .as_object_mut()
        .unwrap()
        .insert("select".to_string(), select);
}

/// Apply all schema fixups in place.
pub fn fixup_schema(schema: &mut Value) {
    if let Some(obj) = schema.as_object_mut() {
        obj.remove("examples");
        obj.remove("maintainers");
        obj.remove("historical");
    }
    add_select_schema(schema);
    fixup_sub_schema(schema, &[]);
}
