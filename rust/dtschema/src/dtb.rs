// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Schema-driven flattened-devicetree (DTB) decoder.
//!
//! Walks a DTB with the [`fdt`] crate, decoding each property's raw bytes into a
//! typed value using the property-type caches (`generated-types` /
//! `generated-pattern-types`) the schema pipeline produces, then reshapes GPIO,
//! interrupt, address, and phandle cell arrays into their validation form.
//!
//! The decoded tree is a [`DtValue`] rather than a raw `serde_json::Value` so
//! raw bytes, nodes, booleans, and integer bit widths stay distinguishable for
//! validation.
//! [`DtValue::to_json`] lowers a decoded tree to JSON for output / comparison.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use fancy_regex::Regex as FancyRegex;
use serde_json::{Number, Value};

use crate::process::process_schemas;
use crate::types::get_prop_types;

/// A decoded devicetree value.
#[derive(Clone, Debug, PartialEq)]
pub enum DtValue {
    /// A present-but-empty property (`len(p) == 0`) — a boolean flag.
    Bool(bool),
    /// A `sized_int`: value plus its bit-width (8/16/32/64).
    Int { val: i128, size: u32 },
    /// Undecoded raw property bytes (no known type, or a decode fallback).
    Bytes(Vec<u8>),
    /// A single decoded string.
    Str(String),
    /// An array / matrix row / string list.
    List(Vec<DtValue>),
    /// A node: named properties and child nodes share one namespace.
    Node(BTreeMap<String, DtValue>),
}

impl DtValue {
    fn as_int(&self) -> Option<i128> {
        match self {
            DtValue::Int { val, .. } => Some(*val),
            _ => None,
        }
    }

    fn as_node(&self) -> Option<&BTreeMap<String, DtValue>> {
        match self {
            DtValue::Node(m) => Some(m),
            _ => None,
        }
    }

    /// Lower to `serde_json::Value` for output / comparison. Raw bytes become
    /// `{"$bytes": [..]}` (a tag that can never collide with a decoded int
    /// array); integer widths are omitted from JSON output.
    pub fn to_json(&self) -> Value {
        match self {
            DtValue::Bool(b) => Value::Bool(*b),
            DtValue::Int { val, .. } => Value::Number(int_to_number(*val)),
            DtValue::Bytes(bytes) => {
                let arr = bytes.iter().map(|b| Value::from(*b)).collect();
                let mut m = serde_json::Map::new();
                m.insert("$bytes".to_string(), Value::Array(arr));
                Value::Object(m)
            }
            DtValue::Str(s) => Value::String(s.clone()),
            DtValue::List(l) => Value::Array(l.iter().map(DtValue::to_json).collect()),
            DtValue::Node(m) => {
                let mut o = serde_json::Map::new();
                for (k, v) in m {
                    o.insert(k.clone(), v.to_json());
                }
                Value::Object(o)
            }
        }
    }
}

fn int_to_number(val: i128) -> Number {
    if val < 0 {
        Number::from(val as i64)
    } else if val <= u64::MAX as i128 {
        Number::from(val as u64)
    } else {
        // Out of u64 range shouldn't happen for DT (max is uint64); clamp.
        Number::from(val as i64)
    }
}

/// `struct` size (bytes) and signedness for a base type name.
fn type_format(base: &str) -> Option<(usize, bool)> {
    Some(match base {
        "int8" => (1, true),
        "uint8" => (1, false),
        "int16" => (2, true),
        "uint16" => (2, false),
        "int32" => (4, true),
        "uint32" => (4, false),
        "int64" => (8, true),
        "uint64" => (8, false),
        "phandle" => (4, false),
        "address" => (4, false),
        _ => return None,
    })
}

/// Unpack `data` big-endian into `sized_int`s of the given base type.
fn unpack(data: &[u8], size: usize, signed: bool) -> Vec<DtValue> {
    let bits = (size * 8) as u32;
    let mut out = Vec::with_capacity(data.len() / size);
    for chunk in data.chunks_exact(size) {
        let mut u: u128 = 0;
        for &b in chunk {
            u = (u << 8) | b as u128;
        }
        let val: i128 = if signed {
            // Sign-extend from `size` bytes.
            let shift = 128 - bits;
            ((u as i128) << shift) >> shift
        } else {
            u as i128
        };
        out.push(DtValue::Int { val, size: bits });
    }
    out
}

// ---------------------------------------------------------------------------
// Property-type context (validator.property_get_type / _dim / has_fixed_dims).
// ---------------------------------------------------------------------------

struct PatEntry {
    regex: FancyRegex,
    ptype: Option<String>,
    dim: Option<Value>,
}

/// The subset of `DTValidator` state the decoder consults: the property-type
/// caches, used to resolve a property name to its candidate types and matrix
/// dimensions.
pub struct TypeContext {
    /// Exact property name → type-entry list.
    props: BTreeMap<String, Vec<Value>>,
    /// `pat_props` compiled: pattern → first entry's type/dim.
    pat: Vec<PatEntry>,
}

impl TypeContext {
    /// Build from raw schema paths, always including the bundled core schemas.
    pub fn new(schema_paths: &[PathBuf]) -> Self {
        let schemas = process_schemas(schema_paths, true);
        Self::from_schemas(&schemas)
    }

    /// Build a decode context from an already-processed schema map (the
    /// `generated-types`/`generated-pattern-types` entries and the individual
    /// bindings), avoiding a second processing pass when the validator has
    /// already built [`crate::process::ProcessedSchemas`].
    pub fn from_processed(schemas: &BTreeMap<String, Value>) -> Self {
        Self::from_schemas(schemas)
    }

    fn from_schemas(schemas: &BTreeMap<String, Value>) -> Self {
        let (props, pat_props) = get_prop_types(schemas);
        let pat = pat_props
            .into_iter()
            .filter_map(|(k, list)| {
                let first = list.first()?;
                let regex = FancyRegex::new(&k).ok()?;
                Some(PatEntry {
                    regex,
                    ptype: first
                        .get("type")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    dim: first.get("dim").cloned(),
                })
            })
            .collect();
        Self { props, pat }
    }

    fn pat_matches(re: &FancyRegex, name: &str) -> bool {
        re.is_match(name).unwrap_or(false)
    }

    /// Return the candidate decoded types for a property.
    fn get_type(&self, name: &str) -> BTreeSet<String> {
        let mut types: BTreeSet<String> = BTreeSet::new();
        if let Some(list) = self.props.get(name) {
            for v in list {
                if let Some(t) = v.get("type").and_then(Value::as_str) {
                    types.insert(t.to_string());
                }
            }
        }
        if types.is_empty() {
            for p in &self.pat {
                if let Some(t) = &p.ptype
                    && !types.contains(t)
                    && Self::pat_matches(&p.regex, name)
                {
                    types.insert(t.clone());
                }
            }
        }
        if types.len() > 1 {
            types.remove("node");
        }
        types
    }

    /// Return the matrix dimensions for a property, if known.
    fn get_type_dim(&self, name: &str) -> Option<Value> {
        if let Some(list) = self.props.get(name) {
            for v in list {
                if let Some(dim) = v.get("dim") {
                    return Some(dim.clone());
                }
            }
        }
        for p in &self.pat {
            if p.ptype.is_some()
                && let Some(dim) = &p.dim
                && Self::pat_matches(&p.regex, name)
            {
                return Some(dim.clone());
            }
        }
        None
    }

    /// Return whether a property has fixed matrix dimensions.
    fn has_fixed_dimensions(&self, name: &str) -> bool {
        match self.get_type_dim(name) {
            Some(dim) => {
                let d = |i: usize, j: usize| dim[i][j].as_i64().unwrap_or(0);
                (d(0, 0) > 0 && d(0, 0) == d(0, 1)) || (d(1, 0) > 0 && d(1, 0) == d(1, 1))
            }
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Decoding (prop_value / get_stride / node scan).
// ---------------------------------------------------------------------------

/// Pick a row stride for a matrix of `prop_len` scalars given
/// `dim = [[min,max],[min,max]]`.
fn get_stride(prop_len: i64, dim: &Value) -> i64 {
    let g = |i: usize, j: usize| dim[i][j].as_i64().unwrap_or(0);
    let mut outer_limit = g(0, 1);
    if outer_limit == 0 {
        outer_limit = prop_len;
    }
    let mut inner_limit = g(1, 1);
    if inner_limit == 0 {
        inner_limit = prop_len;
    }
    for outer in g(0, 0)..=outer_limit {
        for inner in g(1, 0)..=inner_limit {
            if outer * inner == prop_len {
                return inner;
            }
        }
    }
    if g(1, 0) > 0 && g(1, 0) == g(1, 1) {
        return g(1, 0);
    }
    if g(0, 0) > 0 && g(0, 0) == g(0, 1) {
        return prop_len / g(0, 0);
    }
    prop_len
}

/// Decode NUL-separated printable ASCII strings, or `None`.
fn bytes_to_string(b: &[u8]) -> Option<Vec<String>> {
    let s = std::str::from_utf8(b).ok()?;
    if !s.is_ascii() {
        return None;
    }
    let strings: Vec<&str> = s.split('\0').collect();
    let count = strings.len() as isize - 1;
    if count > 0 && strings.last() == Some(&"") {
        // Reject empty or non-printable interior strings; otherwise accept all
        // strings before the trailing NUL.
        for st in &strings[..strings.len() - 1] {
            if st.is_empty() {
                return None;
            }
            if !st.chars().all(|c| !c.is_control()) {
                return None;
            }
        }
        return Some(
            strings[..strings.len() - 1]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
    }
    None
}

/// Decode one property's raw bytes.
fn prop_value(
    ctx: &TypeContext,
    decode_errors: &mut Vec<String>,
    nodename: &str,
    name: &str,
    data: &[u8],
) -> DtValue {
    if data.is_empty() {
        return DtValue::Bool(true);
    }

    if name != "phandle" && (nodename == "__fixups__" || nodename == "aliases") {
        return string_list(&data[..data.len().saturating_sub(1)]);
    }

    let mut prop_types = ctx.get_type(name);
    prop_types.remove("node");

    if prop_types.is_empty() {
        return DtValue::Bytes(data.to_vec());
    }

    let plen = data.len();
    // Filter out types impossible for this length.
    if prop_types.len() > 1 {
        let rm = |s: &mut BTreeSet<String>, items: &[&str]| {
            for it in items {
                s.remove(*it);
            }
        };
        if !plen.is_multiple_of(8) {
            rm(
                &mut prop_types,
                &["int64", "uint64", "int64-array", "uint64-array"],
            );
        }
        if !plen.is_multiple_of(4) {
            rm(
                &mut prop_types,
                &[
                    "int32",
                    "uint32",
                    "int32-array",
                    "uint32-array",
                    "phandle",
                    "phandle-array",
                ],
            );
        }
        if !plen.is_multiple_of(2) {
            rm(
                &mut prop_types,
                &["int16", "uint16", "int16-array", "uint16-array"],
            );
        }
        if plen > 4 {
            rm(&mut prop_types, &["int32", "uint32", "phandle"]);
        } else {
            rm(
                &mut prop_types,
                &["int64", "uint64", "int64-array", "uint64-array"],
            );
        }
        if plen > 2 {
            rm(&mut prop_types, &["int16", "uint16"]);
        } else {
            rm(
                &mut prop_types,
                &[
                    "int32",
                    "uint32",
                    "int32-array",
                    "uint32-array",
                    "phandle",
                    "phandle-array",
                ],
            );
        }
        if plen > 1 {
            rm(&mut prop_types, &["int8", "uint8"]);
        } else {
            rm(
                &mut prop_types,
                &["int16", "uint16", "int16-array", "uint16-array"],
            );
        }
        if plen > 0 {
            rm(&mut prop_types, &["flag"]);
        }

        // Drop the unsigned type if both signed and unsigned exist.
        for (s, u) in [
            ("int64", "uint64"),
            ("int32", "uint32"),
            ("int16", "uint16"),
            ("int8", "uint8"),
        ] {
            if prop_types.contains(s) && prop_types.contains(u) {
                prop_types.remove(u);
            }
        }
    }

    let mut dim = ctx.get_type_dim(name);
    let matrix_prop_types: BTreeSet<String> = prop_types
        .iter()
        .filter(|t| t.contains("matrix") || *t == "phandle-array")
        .cloned()
        .collect();

    let mut fmt: Option<String> = None;

    if prop_types.len() > 1 {
        if name == "dma-masters" {
            fmt = Some("phandle-array".to_string());
        } else if name == "gpios" {
            fmt = Some(if nodename.contains("hog") {
                "uint32-matrix".to_string()
            } else {
                "phandle-array".to_string()
            });
        } else if name == "mode-gpios" {
            fmt = Some("phandle-array".to_string());
        } else if name == "cooling-levels" {
            fmt = Some(if data[0] == 0 && data[1] == 0 {
                "uint32-array".to_string()
            } else {
                "uint8-array".to_string()
            });
        } else if prop_types.contains("string") || prop_types.contains("string-array") {
            if let Some(strs) = bytes_to_string(data) {
                return DtValue::List(strs.into_iter().map(DtValue::Str).collect());
            }
            // Assume only one other type.
            let mut rest: Vec<String> = prop_types
                .iter()
                .filter(|t| *t != "string" && *t != "string-array")
                .cloned()
                .collect();
            match rest.pop() {
                Some(t) => fmt = Some(t),
                None => return DtValue::Bytes(data.to_vec()),
            }
        } else if !matrix_prop_types.is_empty() {
            let scalar: Vec<String> = prop_types.difference(&matrix_prop_types).cloned().collect();
            if scalar.len() == 1 {
                let f = scalar[0].clone();
                let base = f.split('-').next().unwrap_or(&f);
                let (fsize, _) = type_format(base).unwrap_or((4, false));
                let mut min_dim = if let Some(d) = &dim {
                    d[1][0].as_i64().unwrap_or(0)
                } else {
                    0
                };
                if min_dim == 0 {
                    min_dim = 1;
                }
                if let Some(d) = &dim {
                    min_dim *= d[0][0].as_i64().unwrap_or(0);
                } else {
                    min_dim *= 0;
                }
                if (plen as f64) / (fsize as f64) >= min_dim as f64 {
                    // Pick the matrix type (arbitrary member of the set).
                    fmt = matrix_prop_types.iter().next().cloned();
                } else {
                    fmt = Some(f);
                    dim = Some(serde_json::json!([[1, 1], [1, 1]]));
                }
            }
        }
    }

    if fmt.is_none() && !prop_types.is_empty() {
        if prop_types.len() > 1 {
            eprintln!("{name}: property has multiple types: {prop_types:?}");
        }
        // Pick the first one (BTreeSet: lexicographically smallest). This only
        // affects genuinely ambiguous multi-type props, which the differential
        // test guards. Remove the chosen type so the `flag` check below sees
        // only the remaining candidates.
        fmt = prop_types.iter().next().cloned();
        if let Some(f) = &fmt {
            prop_types.remove(f);
        }
    }

    let Some(fmt) = fmt else {
        return DtValue::Bytes(data.to_vec());
    };

    if fmt.starts_with("string") {
        if data.last() != Some(&0) {
            return DtValue::Bytes(data.to_vec());
        }
        return string_list(&data[..data.len() - 1]);
    }

    if prop_types.contains("flag") {
        if !data.is_empty() {
            if fmt == "flag" {
                decode_error(
                    decode_errors,
                    format!("{name}: boolean property with value {data:?}"),
                );
                return DtValue::Bytes(data.to_vec());
            }
        } else {
            return DtValue::Bool(true);
        }
    }

    // Decode the integer(s). An unknown base type (e.g. `flag`) or a length
    // that isn't a whole number of elements is a decode error: emit the size
    // error, then re-unpack purely on total length (4→uint32, 2→uint16,
    // 1→uint8, otherwise leave the bytes undecoded).
    let base = fmt.split('-').next().unwrap_or(&fmt);
    let mut val_int = match type_format(base) {
        Some((size, signed)) if plen.is_multiple_of(size) => unpack(data, size, signed),
        _ => {
            decode_error(
                decode_errors,
                format!("{name}: size ({plen}) error for type {fmt}"),
            );
            match plen {
                4 => unpack(data, 4, false),
                2 => unpack(data, 2, false),
                1 => unpack(data, 1, false),
                _ => return DtValue::Bytes(data.to_vec()),
            }
        }
    };

    let is_matrix =
        fmt.contains("matrix") || matches!(fmt.as_str(), "phandle" | "phandle-array" | "address");
    if is_matrix {
        if let Some(dim) = &dim {
            let stride = get_stride(val_int.len() as i64, dim).max(1) as usize;
            let mut rows = Vec::new();
            let mut i = 0;
            while i < val_int.len() {
                let end = (i + stride).min(val_int.len());
                rows.push(DtValue::List(val_int[i..end].to_vec()));
                i += stride;
            }
            DtValue::List(rows)
        } else {
            DtValue::List(vec![DtValue::List(val_int)])
        }
    } else if !fmt.contains("array") && val_int.len() == 1 {
        val_int.pop().unwrap()
    } else {
        DtValue::List(val_int)
    }
}

/// Decode NUL-separated ASCII into a list of strings (already trimmed of the
/// trailing NUL byte by the caller).
fn string_list(data: &[u8]) -> DtValue {
    let s = String::from_utf8_lossy(data);
    DtValue::List(s.split('\0').map(|p| DtValue::Str(p.to_string())).collect())
}

fn decode_error(errors: &mut Vec<String>, msg: String) {
    errors.push(msg);
}

// ---------------------------------------------------------------------------
// Node scanning (fdt walk).
// ---------------------------------------------------------------------------

struct Scanner<'a> {
    ctx: &'a TypeContext,
    decode_errors: &'a mut Vec<String>,
    phandle_loc: Vec<String>,
}

impl<'a> Scanner<'a> {
    fn node_props(
        &mut self,
        node: &fdt::node::FdtNode,
        nodename: &str,
    ) -> BTreeMap<String, DtValue> {
        let mut props = BTreeMap::new();
        for p in node.properties() {
            let v = prop_value(self.ctx, self.decode_errors, nodename, p.name, p.value);
            props.insert(p.name.to_string(), v);
        }
        props
    }

    /// Decode a node's props, then recurse into subnodes. Returns `None` for
    /// the special `__*__` nodes, which are consumed for their side effects.
    fn scan_node(&mut self, node: &fdt::node::FdtNode, nodename: &str) -> Option<DtValue> {
        if nodename == "__fixups__" {
            self.process_fixups(node);
            return None;
        }
        if nodename == "__local_fixups__" {
            self.process_local_fixups(node, "");
            return None;
        }
        if nodename.starts_with("__") {
            return None;
        }

        let mut map = self.node_props(node, nodename);
        for child in node.children() {
            if let Some(sub) = self.scan_node(&child, child.name) {
                map.insert(child.name.to_string(), sub);
            }
        }
        Some(DtValue::Node(map))
    }

    /// Collect every fixup string into `phandle_loc`.
    fn process_fixups(&mut self, node: &fdt::node::FdtNode) {
        let props = self.node_props(node, "__fixups__");
        for v in props.values() {
            if let DtValue::List(items) = v {
                for it in items {
                    if let DtValue::Str(s) = it {
                        self.phandle_loc.push(s.clone());
                    }
                }
            }
        }
    }

    /// For each property, append `path:name:offset` for every uint32 offset,
    /// recursing with `/name` appended.
    fn process_local_fixups(&mut self, node: &fdt::node::FdtNode, path: &str) {
        for p in node.properties() {
            for chunk in p.value.chunks_exact(4) {
                let off = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                self.phandle_loc.push(format!("{path}:{}:{off}", p.name));
            }
        }
        for child in node.children() {
            let child_path = format!("{path}/{}", child.name);
            self.process_local_fixups(&child, &child_path);
        }
    }
}

// ---------------------------------------------------------------------------
// Fixup passes.
// ---------------------------------------------------------------------------

/// phandle+args cell-count sources for properties that don't follow the
/// standard `foos` / `#foo-cells` convention.
enum CellName {
    Named(String),
    Fixed(i64),
    None,
}

fn phandle_args(name: &str) -> Option<CellName> {
    Some(match name {
        "assigned-clocks" | "assigned-clock-parents" => CellName::Named("#clock-cells".into()),
        "cooling-device" => CellName::Named("#cooling-cells".into()),
        "interrupts-extended" => CellName::Named("#interrupt-cells".into()),
        "interconnects" => CellName::Named("#interconnect-cells".into()),
        "mboxes" => CellName::Named("#mbox-cells".into()),
        "sound-dai" => CellName::Named("#sound-dai-cells".into()),
        "msi-parent" => CellName::Named("#msi-cells".into()),
        "msi-ranges" => CellName::Named("#interrupt-cells".into()),
        "dma-masters" => CellName::Named("#dma-cells".into()),
        "gpio-ranges" => CellName::Fixed(3),
        "memory-region" => CellName::None,
        _ => return None,
    })
}

/// Return the number of argument cells for a referenced provider.
fn get_cells_size(node: &BTreeMap<String, DtValue>, cellname: &CellName) -> i64 {
    match cellname {
        CellName::Fixed(n) => *n,
        CellName::None => 0,
        CellName::Named(name) => node
            .get(name)
            .and_then(DtValue::as_int)
            .map(|v| v as i64)
            .unwrap_or(0),
    }
}

/// Return the number of cells for a directly-named cell property.
fn get_named_cells(node: &BTreeMap<String, DtValue>, name: &str) -> i64 {
    node.get(name)
        .and_then(DtValue::as_int)
        .map(|v| v as i64)
        .unwrap_or(0)
}

/// Decode context carrying the `phandles` map built after scanning.
struct Fixups<'a> {
    ctx: &'a TypeContext,
    phandles: BTreeMap<i128, BTreeMap<String, DtValue>>,
    phandle_loc: BTreeSet<String>,
}

impl<'a> Fixups<'a> {
    /// Return whether the cell at `prop_path` is marked as a phandle.
    fn check_is_phandle(&self, prop_path: &str, cell: i64) -> bool {
        self.phandle_loc
            .contains(&format!("{prop_path}:{}", cell * 4))
    }

    /// Return the width of a phandle group starting at `idx`.
    fn phandle_arg_size(
        &self,
        prop_path: &str,
        idx: i64,
        cells: &[DtValue],
        cellname: &CellName,
    ) -> i64 {
        if cells.is_empty() {
            return 0;
        }
        let phandle = cells[0].as_int().unwrap_or(0);
        if phandle == 0 || matches!(cellname, CellName::None) {
            return 1;
        }
        if phandle == 0xffffffff {
            if self.check_is_phandle(prop_path, idx) {
                let mut cell_count = 1i64;
                while (cell_count as usize) < cells.len()
                    && !self.check_is_phandle(prop_path, idx + cell_count)
                {
                    cell_count += 1;
                }
                return cell_count;
            }
            return 0;
        }
        let Some(node) = self.phandles.get(&phandle) else {
            return 0;
        };
        get_cells_size(node, cellname) + 1
    }

    /// Reshape phandle-array properties into phandle argument groups.
    fn fixup_phandles(&self, dt: &mut BTreeMap<String, DtValue>, path: &str) {
        let keys: Vec<String> = dt.keys().cloned().collect();
        for k in keys {
            // Recurse into child nodes first.
            if matches!(dt.get(&k), Some(DtValue::Node(_))) {
                let child_path = format!("{path}/{k}");
                if let Some(DtValue::Node(child)) = dt.get_mut(&k) {
                    let mut child = std::mem::take(child);
                    self.fixup_phandles(&mut child, &child_path);
                    dt.insert(k.clone(), DtValue::Node(child));
                }
                continue;
            }
            if !self.ctx.get_type(&k).contains("phandle-array") {
                continue;
            }
            if k != "dma-masters" && self.ctx.has_fixed_dimensions(&k) {
                continue;
            }
            // Not a matrix or already split, nothing to do.
            let is_single_matrix = match dt.get(&k) {
                Some(DtValue::List(rows)) => rows.len() == 1 && matches!(rows[0], DtValue::List(_)),
                _ => false,
            };
            if !is_single_matrix {
                continue;
            }

            let cellname: CellName;
            let prop_path = format!("{path}:{k}");
            let val = match dt.get(&k) {
                Some(DtValue::List(rows)) => match &rows[0] {
                    DtValue::List(v) => v.clone(),
                    _ => continue,
                },
                _ => continue,
            };

            if let Some(cn) = phandle_args(&k) {
                cellname = cn;
            } else if k.ends_with('s') && !k.contains("gpio") {
                let name = format!("#{}-cells", &k[..k.len() - 1]);
                cellname = CellName::Named(name);
                let i = self.phandle_arg_size(&prop_path, 0, &val, &cellname);
                if i == 0 {
                    continue;
                }
            } else {
                continue;
            }

            // HACK: a dma-masters phandle in 1..=4 that doesn't resolve to a
            // DMA provider is really a uint32, not a phandle.
            let phandle = val[0].as_int().unwrap_or(0);
            if k == "dma-masters"
                && (1..=4).contains(&phandle)
                && self
                    .phandles
                    .get(&phandle)
                    .map(|n| !matches!(&cellname, CellName::Named(nm) if n.contains_key(nm)))
                    .unwrap_or(true)
            {
                dt.insert(
                    k.clone(),
                    DtValue::Int {
                        val: phandle,
                        size: 32,
                    },
                );
                continue;
            }

            let mut out: Vec<DtValue> = Vec::new();
            let mut i = 0i64;
            while (i as usize) < val.len() {
                let slice = &val[i as usize..];
                let mut cells = self.phandle_arg_size(&prop_path, i, slice, &cellname);
                if cells == 0 {
                    break;
                }
                if k == "msi-ranges" {
                    cells += 1;
                }
                if k == "interconnects" {
                    let next = &val[(i + cells) as usize..];
                    cells += self.phandle_arg_size(&prop_path, i + cells, next, &cellname);
                }
                let end = ((i + cells) as usize).min(val.len());
                out.push(DtValue::List(val[i as usize..end].to_vec()));
                i += cells;
            }
            dt.insert(k.clone(), DtValue::List(out));
        }
    }

    /// Reshape GPIO properties into phandle argument groups.
    fn fixup_gpios(&self, dt: &mut BTreeMap<String, DtValue>) {
        if dt.contains_key("gpio-hog") {
            return;
        }
        let keys: Vec<String> = dt.keys().cloned().collect();
        for k in keys {
            if matches!(dt.get(&k), Some(DtValue::Node(_))) {
                if let Some(DtValue::Node(child)) = dt.get_mut(&k) {
                    let mut child = std::mem::take(child);
                    self.fixup_gpios(&mut child);
                    dt.insert(k.clone(), DtValue::Node(child));
                }
                continue;
            }
            let is_gpio =
                (k.ends_with("-gpios") || k.ends_with("-gpio") || k == "gpio" || k == "gpios")
                    && !k.ends_with(",nr-gpios");
            if !is_gpio {
                continue;
            }
            let val = match dt.get(&k) {
                Some(DtValue::List(rows)) => match rows.first() {
                    Some(DtValue::List(v)) => v.clone(),
                    _ => continue,
                },
                _ => continue,
            };

            let mut out: Vec<DtValue> = Vec::new();
            let mut i = 0i64;
            while (i as usize) < val.len() {
                let phandle = val[i as usize].as_int().unwrap_or(0);
                let cells: i64 = if phandle == 0 {
                    0
                } else if phandle == 0xffffffff {
                    // Next 0xffffffff in val[i+1 .. len-1], else len.
                    let mut found = None;
                    let start = (i + 1) as usize;
                    let stop = val.len().saturating_sub(1);
                    for (off, item) in val.iter().enumerate().take(stop).skip(start) {
                        if item.as_int() == Some(0xffffffff) {
                            found = Some(off as i64);
                            break;
                        }
                    }
                    let base = found.unwrap_or(val.len() as i64);
                    base - (i + 1)
                } else {
                    match self.phandles.get(&phandle) {
                        Some(node) => get_named_cells(node, "#gpio-cells"),
                        None => 0,
                    }
                };
                let end = ((i + cells + 1) as usize).min(val.len());
                out.push(DtValue::List(val[i as usize..end].to_vec()));
                i += cells + 1;
            }
            dt.insert(k.clone(), DtValue::List(out));
        }
    }

    /// Reshape interrupt properties using the active interrupt cell count.
    fn fixup_interrupts(&self, dt: &mut BTreeMap<String, DtValue>, mut icells: i64) {
        // interrupt-parent handling.
        if let Some(DtValue::List(rows)) = dt.get("interrupt-parent")
            && let Some(DtValue::List(first)) = rows.first()
        {
            let phandle = first.first().and_then(DtValue::as_int).unwrap_or(0);
            if phandle == 0xffffffff {
                dt.remove("interrupt-parent");
            } else if let Some(node) = self.phandles.get(&phandle) {
                icells = get_named_cells(node, "#interrupt-cells");
            }
        }

        let node_icells = get_named_cells(dt, "#interrupt-cells");
        let has_icells = dt.contains_key("#interrupt-cells");
        let ac = get_named_cells(dt, "#address-cells");

        let keys: Vec<String> = dt.keys().cloned().collect();
        for k in keys {
            if matches!(dt.get(&k), Some(DtValue::Node(_))) {
                let child_icells = if has_icells { node_icells } else { icells };
                if let Some(DtValue::Node(child)) = dt.get_mut(&k) {
                    let mut child = std::mem::take(child);
                    self.fixup_interrupts(&mut child, child_icells);
                    dt.insert(k.clone(), DtValue::Node(child));
                }
                continue;
            }
            if k == "interrupts" {
                if let Some(val) = first_row(dt.get(&k)) {
                    let mut out = Vec::new();
                    let mut i = 0i64;
                    let step = icells.max(1);
                    while (i as usize) < val.len() {
                        let end = ((i + icells) as usize).min(val.len());
                        out.push(DtValue::List(val[i as usize..end].to_vec()));
                        i += step;
                    }
                    dt.insert(k.clone(), DtValue::List(out));
                }
            } else if k == "interrupt-map"
                && let Some(val) = first_row(dt.get(&k))
            {
                let imap_icells = node_icells;
                let out = self.split_interrupt_map(&val, ac, imap_icells);
                dt.insert(k.clone(), DtValue::List(out));
            }
        }
    }

    fn split_interrupt_map(&self, val: &[DtValue], ac: i64, imap_icells: i64) -> Vec<DtValue> {
        let mut out = Vec::new();
        let phandle_idx = (ac + imap_icells) as usize;
        let phandle = val.get(phandle_idx).and_then(DtValue::as_int).unwrap_or(0);
        let mut i = 0i64;
        if phandle == 0xffffffff {
            // Uniform sizes: distance to the next 0xffffffff.
            let start = (ac + imap_icells + 1) as usize;
            let mut next = None;
            for (off, item) in val.iter().enumerate().skip(start) {
                if item.as_int() == Some(0xffffffff) {
                    next = Some(off as i64);
                    break;
                }
            }
            let cells = match next {
                Some(n) => n - (ac + imap_icells),
                None => val.len() as i64,
            };
            let step = cells.max(1);
            while (i as usize) < val.len() {
                let end = ((i + cells) as usize).min(val.len());
                out.push(DtValue::List(val[i as usize..end].to_vec()));
                i += step;
            }
        } else {
            while (i as usize) < val.len() {
                let (p_icells, p_ac) = match self.phandles.get(&phandle) {
                    Some(node) => (
                        get_named_cells(node, "#interrupt-cells"),
                        if node.contains_key("#address-cells") {
                            get_named_cells(node, "#address-cells")
                        } else {
                            0
                        },
                    ),
                    None => (0, 0),
                };
                let cells = ac + imap_icells + 1 + p_ac + p_icells;
                let end = ((i + cells) as usize).min(val.len());
                out.push(DtValue::List(val[i as usize..end].to_vec()));
                i += cells.max(1);
            }
        }
        out
    }

    /// Reshape address-like properties using parent bus cell counts.
    ///
    /// `ac`/`sc` are the *parent* bus's `#address-cells`/`#size-cells`; they
    /// govern how this node's own `reg`/address-type properties reshape. The
    /// node's *own* `#address-cells`/`#size-cells` (`node_ac`/`node_sc`) are
    /// what get passed down to its children and, for `ranges`, supply the child
    /// portion. `ac`/`sc` are only rebound when descending into a child node;
    /// since FDT properties always precede subnodes, a node's own `reg` is
    /// reshaped with the inherited (parent) cell counts, never its own.
    fn fixup_addresses(&self, dt: &mut BTreeMap<String, DtValue>, ac: i64, sc: i64) {
        let node_ac = get_named_cells(dt, "#address-cells");
        let node_sc = get_named_cells(dt, "#size-cells");
        // Cells handed to children: this node's own, or inherited if unset.
        let child_ac = if dt.contains_key("#address-cells") {
            node_ac
        } else {
            ac
        };
        let child_sc = if dt.contains_key("#size-cells") {
            node_sc
        } else {
            sc
        };

        let keys: Vec<String> = dt.keys().cloned().collect();
        for k in keys {
            if matches!(dt.get(&k), Some(DtValue::Node(_))) {
                if let Some(DtValue::Node(child)) = dt.get_mut(&k) {
                    let mut child = std::mem::take(child);
                    self.fixup_addresses(&mut child, child_ac, child_sc);
                    dt.insert(k.clone(), DtValue::Node(child));
                }
                continue;
            }
            if self.ctx.get_type(&k).contains("address") {
                if let Some(val) = first_row(dt.get(&k)) {
                    let step = (ac + sc).max(1);
                    let mut out = Vec::new();
                    let mut i = 0i64;
                    while (i as usize) < val.len() {
                        let end = ((i + ac + sc) as usize).min(val.len());
                        out.push(DtValue::List(val[i as usize..end].to_vec()));
                        i += step;
                    }
                    dt.insert(k.clone(), DtValue::List(out));
                }
            } else if (k == "ranges" || k == "dma-ranges")
                && !matches!(dt.get(&k), Some(DtValue::Bool(_)))
                && let Some(val) = first_row(dt.get(&k))
            {
                let child_cells = node_ac + node_sc;
                let step = (ac + child_cells).max(1);
                let mut out = Vec::new();
                let mut i = 0i64;
                while (i as usize) < val.len() {
                    let end = ((i + ac + child_cells) as usize).min(val.len());
                    out.push(DtValue::List(val[i as usize..end].to_vec()));
                    i += step;
                }
                dt.insert(k.clone(), DtValue::List(out));
            }
        }
    }
}

/// Extract `v[0]` as a row (list of scalars) when `v` is a single-row matrix.
fn first_row(v: Option<&DtValue>) -> Option<Vec<DtValue>> {
    match v {
        Some(DtValue::List(rows)) => match rows.first() {
            Some(DtValue::List(row)) => Some(row.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Recursively populate the phandle map from the scanned tree: any node with a
/// scalar `phandle` property is recorded under its value.
fn collect_phandles(node: &DtValue, out: &mut BTreeMap<i128, BTreeMap<String, DtValue>>) {
    if let DtValue::Node(map) = node {
        if let Some(ph) = map.get("phandle").and_then(DtValue::as_int) {
            out.insert(ph, map.clone());
        }
        for v in map.values() {
            if v.as_node().is_some() {
                collect_phandles(v, out);
            }
        }
    }
}

/// Decode a whole DTB into its root node.
pub fn decode_dtb(
    ctx: &TypeContext,
    dtb: &[u8],
    decode_errors: &mut Vec<String>,
) -> anyhow::Result<DtValue> {
    let fdt = fdt::Fdt::new(dtb).map_err(|e| anyhow::anyhow!("parsing DTB: {e:?}"))?;
    let root = fdt
        .find_node("/")
        .ok_or_else(|| anyhow::anyhow!("DTB has no root node"))?;

    let mut scanner = Scanner {
        ctx,
        decode_errors,
        phandle_loc: Vec::new(),
    };
    let tree = scanner
        .scan_node(&root, "/")
        .ok_or_else(|| anyhow::anyhow!("root node decoded to nothing"))?;

    let mut phandles = BTreeMap::new();
    collect_phandles(&tree, &mut phandles);

    let DtValue::Node(mut map) = tree else {
        return Ok(tree);
    };

    let fixups = Fixups {
        ctx,
        phandles,
        phandle_loc: scanner.phandle_loc.into_iter().collect(),
    };
    fixups.fixup_gpios(&mut map);
    fixups.fixup_interrupts(&mut map, 1);
    fixups.fixup_addresses(&mut map, 2, 1);
    fixups.fixup_phandles(&mut map, "");

    Ok(DtValue::Node(map))
}
