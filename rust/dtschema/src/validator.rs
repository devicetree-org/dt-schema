// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Devicetree data validator built on the processed schema set from
//! [`crate::process::ProcessedSchemas`].
//!
//! The instances being validated are decoded devicetree nodes ([`DtValue`]),
//! not `serde_json::Value`: a decoded integer carries its bit-width
//! (`sized_int`), which the custom `typeSize` keyword reads. To make the
//! `jsonschema` engine validate `DtValue` directly — so `typeSize` sees the
//! real width instead of a lossy JSON re-encoding — we implement a custom
//! in-memory representation ([`DtJson`]) over `&DtValue` and build validators
//! with `jsonschema::options_for::<DtJson>()`.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use jsonschema::error::ValidationErrorKind;
use jsonschema::json::{Array, Json, JsonNumber, Node, NodeIdentity, Object};
use jsonschema::{Draft, JsonType, Keyword, Retrieve, Uri, ValidationError, Validator};
use regex::{Regex, RegexSet};
use serde_json::{Map, Value};

use crate::dtb::{self, DtValue, TypeContext};
use crate::process::ProcessedSchemas;

// ---------------------------------------------------------------------------
// Custom `jsonschema` representation over `&DtValue`.
// ---------------------------------------------------------------------------

/// The `Json` marker type: instances are borrowed [`DtValue`] trees.
pub struct DtJson;

impl Json for DtJson {
    type Node<'a> = &'a DtValue;
    type PreparedKey = String;
    type StringBuffer = DtValue;

    fn prepare_key(key: &str) -> String {
        key.to_owned()
    }

    fn with_string_node<T>(buffer: &mut DtValue, string: &str, f: impl FnOnce(&DtValue) -> T) -> T {
        *buffer = DtValue::Str(string.to_owned());
        f(buffer)
    }
}

impl Default for DtValue {
    fn default() -> Self {
        DtValue::Bool(false)
    }
}

/// A decoded integer presented as a JSON number.
pub struct DtNumber(i128);

impl JsonNumber for DtNumber {
    fn as_u64(&self) -> Option<u64> {
        u64::try_from(self.0).ok()
    }
    fn as_i64(&self) -> Option<i64> {
        i64::try_from(self.0).ok()
    }
    fn as_f64(&self) -> Option<f64> {
        Some(self.0 as f64)
    }
    fn as_str(&self) -> Cow<'_, str> {
        Cow::Owned(self.0.to_string())
    }
    fn to_number(&self) -> Cow<'_, serde_json::Number> {
        // DT integers fit in i128; i64/u64 cover the real range (max uint64).
        let n = if self.0 < 0 {
            serde_json::Number::from(self.0 as i64)
        } else {
            serde_json::Number::from(self.0 as u64)
        };
        Cow::Owned(n)
    }
    fn is_integer(&self) -> bool {
        true
    }
}

impl<'a> Node<'a, DtJson> for &'a DtValue {
    type Object = &'a BTreeMap<String, DtValue>;
    type Array = &'a [DtValue];
    type Number = DtNumber;

    fn as_object(&self) -> Option<&'a BTreeMap<String, DtValue>> {
        match self {
            DtValue::Node(m) => Some(m),
            _ => None,
        }
    }
    fn as_array(&self) -> Option<&'a [DtValue]> {
        match self {
            DtValue::List(l) => Some(l.as_slice()),
            _ => None,
        }
    }
    fn as_string(&self) -> Option<Cow<'a, str>> {
        match self {
            DtValue::Str(s) => Some(Cow::Borrowed(s.as_str())),
            _ => None,
        }
    }
    fn as_number(&self) -> Option<DtNumber> {
        match self {
            DtValue::Int { val, .. } => Some(DtNumber(*val)),
            _ => None,
        }
    }
    fn as_boolean(&self) -> Option<bool> {
        match self {
            DtValue::Bool(b) => Some(*b),
            _ => None,
        }
    }
    fn is_null(&self) -> bool {
        // Raw bytes are not JSON null.
        false
    }
    fn json_type(&self) -> JsonType {
        match self {
            DtValue::Bool(_) => JsonType::Boolean,
            DtValue::Int { .. } => JsonType::Number,
            DtValue::Str(_) => JsonType::String,
            DtValue::List(_) => JsonType::Array,
            DtValue::Node(_) => JsonType::Object,
            // Raw bytes have no JSON type. The `jsonschema` multi-type fast
            // path has no "none" variant, but `JsonType::Number` with
            // `as_number() == None` fails every numeric and non-numeric type
            // check, so undecoded bytes fail every JSON type.
            DtValue::Bytes(_) => JsonType::Number,
        }
    }
    fn to_value(&self) -> Cow<'a, Value> {
        Cow::Owned(self.to_json())
    }
    fn identity(&self) -> Option<NodeIdentity> {
        Some(NodeIdentity::new(
            std::ptr::from_ref::<DtValue>(*self) as usize
        ))
    }
}

/// Iterator over object members exposing `&str` names.
pub struct DtMembers<'a>(std::collections::btree_map::Iter<'a, String, DtValue>);

impl<'a> Iterator for DtMembers<'a> {
    type Item = (&'a str, &'a DtValue);
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(k, v)| (k.as_str(), v))
    }
}

impl<'a> Object<'a, DtJson> for &'a BTreeMap<String, DtValue> {
    type Node = &'a DtValue;
    type MemberName = &'a str;
    type MembersIter = DtMembers<'a>;

    fn len(&self) -> usize {
        BTreeMap::len(self)
    }
    fn get(&self, key: &String) -> Option<&'a DtValue> {
        BTreeMap::get(*self, key)
    }
    fn members(&self) -> DtMembers<'a> {
        DtMembers(self.iter())
    }
}

impl<'a> Array<'a, DtJson> for &'a [DtValue] {
    type Node = &'a DtValue;
    type ElementsIter = std::slice::Iter<'a, DtValue>;

    fn len(&self) -> usize {
        <[DtValue]>::len(self)
    }
    fn elements(&self) -> std::slice::Iter<'a, DtValue> {
        self.iter()
    }
}

// ---------------------------------------------------------------------------
// `typeSize` custom keyword.
// ---------------------------------------------------------------------------

/// Require the decoded integer's bit-width to equal the schema value. Values
/// without an explicit width use the legacy 32-bit default.
struct TypeSizeKeyword {
    expected: u64,
}

impl<'i> Keyword<'i, DtJson> for TypeSizeKeyword {
    fn validate(&self, instance: &'i DtValue) -> Result<(), ValidationError<'i>> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            let size = int_size(instance);
            Err(ValidationError::custom(format!(
                "size is {size}, expected {}",
                self.expected
            )))
        }
    }

    fn is_valid(&self, instance: &'i DtValue) -> bool {
        int_size(instance) == self.expected
    }
}

/// The effective bit-width of a decoded value for `typeSize` purposes.
fn int_size(instance: &DtValue) -> u64 {
    match instance {
        DtValue::Int { size, .. } => *size as u64,
        _ => 32,
    }
}

/// A cheaply-cloneable retriever handle wrapping the shared processed-schema
/// map, so each per-schema validator build gets its own `Retrieve` without
/// deep-copying the map (`jsonschema`'s `with_retriever` takes ownership).
#[derive(Clone)]
struct SharedRetriever(Arc<DtSchemaRetriever>);

impl Retrieve for SharedRetriever {
    fn retrieve(
        &self,
        uri: &Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        self.0.retrieve(uri)
    }
}

/// Build the validation options carrying the `typeSize` keyword and the
/// devicetree retriever, for the custom [`DtJson`] representation.
fn dt_options(
    retriever: SharedRetriever,
) -> jsonschema::ValidationOptions<'static, Arc<dyn Retrieve>, DtJson> {
    jsonschema::options_for::<DtJson>()
        .with_draft(Draft::Draft201909)
        .with_retriever(retriever)
        .with_keyword(
            "typeSize",
            |parent: &Map<String, Value>, _schema: &Value, _path| {
                let expected = parent.get("typeSize").and_then(Value::as_u64).unwrap_or(32);
                Ok(Box::new(TypeSizeKeyword { expected }) as Box<dyn for<'i> Keyword<'i, DtJson>>)
            },
        )
}

// ---------------------------------------------------------------------------
// DTValidator.
// ---------------------------------------------------------------------------

/// A single reported validation error, annotated with its originating schema
/// `$id`.
pub struct DtError {
    /// Instance path in the decoded DT node.
    pub instance_path: Vec<PathSeg>,
    /// Schema path inside the matching schema.
    pub schema_path: Vec<PathSeg>,
    /// Human-readable validation message.
    pub message: String,
    /// The originating schema `$id`.
    pub schema_file: String,
    /// The instance at the error location, if it is a node (used for the
    /// disabled-`status` suppression heuristic).
    pub instance_is_disabled_node: bool,
    /// True when this error, or an `anyOf`/`oneOf` child context, is a
    /// missing-property-style error suppressed for disabled nodes.
    pub has_suppressible_disabled_context: bool,
}

/// A JSON-Pointer path segment: either a property name or an array index.
#[derive(Clone)]
pub enum PathSeg {
    Key(String),
    Index(usize),
}

impl PathSeg {
    /// Render as a JSON diagnostic path: strings stay strings, indices become
    /// integers.
    pub fn as_json(&self) -> Value {
        match self {
            PathSeg::Key(k) => Value::String(k.clone()),
            PathSeg::Index(i) => Value::Number((*i).into()),
        }
    }
    pub fn to_display(&self) -> String {
        match self {
            PathSeg::Key(k) => k.clone(),
            PathSeg::Index(i) => i.to_string(),
        }
    }
}

/// A lazily-built, shareable compiled validator slot. `None` inside the
/// `OnceLock` records a build failure (so it isn't retried or re-warned).
type ValidatorSlot = Arc<OnceLock<Option<Arc<Validator<DtJson>>>>>;

/// The devicetree data validator.
pub struct DTValidator {
    schemas: Arc<BTreeMap<String, Value>>,
    compat_map: BTreeMap<String, String>,
    always_schemas: Vec<String>,
    type_ctx: TypeContext,
    retriever: Arc<DtSchemaRetriever>,
    always_dispatch: AlwaysDispatch,
    vendor_prefixes: Option<VendorPrefixesFastPath>,
    /// Compiled raw per-schema validators, built once and reused across every
    /// node and DTB. The slots are allocated at construction time, so the hot
    /// validation path only needs an immutable map lookup before entering the
    /// `OnceLock`.
    raw_validators: BTreeMap<String, ValidatorSlot>,
    /// Compiled `{if: select, then: schema}` always-schema validators, aligned
    /// with `always_schemas`.
    always_validators: Vec<ValidatorSlot>,
}

/// Retriever that resolves `$ref` URIs against the processed schema map.
struct DtSchemaRetriever {
    schemas: Arc<BTreeMap<String, Value>>,
}

const VENDOR_PREFIXES_SCHEMA: &str = "http://devicetree.org/schemas/vendor-prefixes.yaml";

struct VendorPrefixesFastPath {
    properties: std::collections::BTreeSet<String>,
    patterns: RegexSet,
}

impl VendorPrefixesFastPath {
    fn build(schema: &Value) -> Option<Self> {
        if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
            return None;
        }
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)?
            .keys()
            .cloned()
            .collect();
        let patterns: Vec<&str> = schema
            .get("patternProperties")
            .and_then(Value::as_object)?
            .keys()
            .map(String::as_str)
            .collect();
        let patterns = RegexSet::new(patterns).ok()?;
        Some(Self {
            properties,
            patterns,
        })
    }

    fn is_valid(&self, node: &DtValue) -> bool {
        let DtValue::Node(map) = node else {
            return false;
        };
        map.keys()
            .all(|key| self.properties.contains(key) || self.patterns.is_match(key))
    }
}

#[derive(Default)]
struct AlwaysDispatch {
    fallback: Vec<SelectCandidate>,
    compat_any_exact: BTreeMap<String, Vec<SelectCandidate>>,
    compat_first_exact: BTreeMap<String, Vec<SelectCandidate>>,
    compat_first_patterns: PatternCandidates,
    compat_any_patterns: PatternCandidates,
    nodename_exact: BTreeMap<String, Vec<SelectCandidate>>,
    nodename_patterns: PatternCandidates,
    property_exact: BTreeMap<String, Vec<SelectCandidate>>,
    property_patterns: PatternCandidates,
}

#[derive(Clone)]
struct SelectCandidate {
    schema_index: usize,
    required: Vec<String>,
}

enum SelectKey {
    Never,
    Fallback,
    CompatibleAnyExact(Vec<String>),
    CompatibleFirstExact(Vec<String>),
    CompatibleAnyPattern(String),
    CompatibleFirstPattern(String),
    NodenameExact(String),
    NodenamePattern(String),
    PropertyInterest {
        exact: Vec<String>,
        patterns: Vec<String>,
    },
}

#[derive(Default)]
struct PatternCandidates {
    patterns: Vec<String>,
    candidates: Vec<SelectCandidate>,
    set: Option<RegexSet>,
}

impl PatternCandidates {
    fn push(&mut self, pattern: String, candidate: SelectCandidate) {
        self.patterns.push(pattern);
        self.candidates.push(candidate);
    }

    fn compile(&mut self) {
        self.set = if self.patterns.is_empty() {
            None
        } else {
            RegexSet::new(&self.patterns).ok()
        };
    }

    fn insert_matches(
        &self,
        text: &str,
        node: &BTreeMap<String, DtValue>,
        selected: &mut Vec<usize>,
    ) {
        let Some(set) = &self.set else {
            return;
        };
        for idx in set.matches(text) {
            let candidate = &self.candidates[idx];
            if candidate.required_present(node) {
                selected.push(candidate.schema_index);
            }
        }
    }
}

impl AlwaysDispatch {
    fn build(schemas: &BTreeMap<String, Value>, always_schemas: &[String]) -> Self {
        let mut dispatch = Self::default();
        for (schema_index, schema_id) in always_schemas.iter().enumerate() {
            let Some(schema) = schemas.get(schema_id) else {
                continue;
            };
            let candidate = SelectCandidate {
                schema_index,
                required: select_required(schema),
            };
            match select_key(schema) {
                SelectKey::Never => {}
                SelectKey::Fallback => dispatch.fallback.push(candidate),
                SelectKey::CompatibleAnyExact(values) => {
                    for value in values {
                        dispatch
                            .compat_any_exact
                            .entry(value)
                            .or_default()
                            .push(candidate.clone());
                    }
                }
                SelectKey::CompatibleFirstExact(values) => {
                    for value in values {
                        dispatch
                            .compat_first_exact
                            .entry(value)
                            .or_default()
                            .push(candidate.clone());
                    }
                }
                SelectKey::CompatibleAnyPattern(pattern) => {
                    dispatch.compat_any_patterns.push(pattern, candidate);
                }
                SelectKey::CompatibleFirstPattern(pattern) => {
                    dispatch.compat_first_patterns.push(pattern, candidate);
                }
                SelectKey::NodenameExact(name) => {
                    dispatch
                        .nodename_exact
                        .entry(name)
                        .or_default()
                        .push(candidate);
                }
                SelectKey::NodenamePattern(pattern) => {
                    dispatch.nodename_patterns.push(pattern, candidate);
                }
                SelectKey::PropertyInterest { exact, patterns } => {
                    for prop in exact {
                        dispatch
                            .property_exact
                            .entry(prop)
                            .or_default()
                            .push(candidate.clone());
                    }
                    let mut has_unsupported_pattern = false;
                    for pattern in patterns {
                        if Regex::new(&pattern).is_ok() {
                            dispatch.property_patterns.push(pattern, candidate.clone());
                        } else {
                            has_unsupported_pattern = true;
                        }
                    }
                    if has_unsupported_pattern {
                        dispatch.fallback.push(candidate);
                    }
                }
            }
        }
        dispatch.compat_any_patterns.compile();
        dispatch.compat_first_patterns.compile();
        dispatch.nodename_patterns.compile();
        dispatch.property_patterns.compile();
        dispatch
    }

    fn candidates(&self, node: &BTreeMap<String, DtValue>) -> Vec<usize> {
        let mut selected = Vec::with_capacity(self.fallback.len() + 8);

        for candidate in &self.fallback {
            if candidate.required_present(node) {
                selected.push(candidate.schema_index);
            }
        }

        let compats = compatible_strings(node);
        for compat in &compats {
            if let Some(candidates) = self.compat_any_exact.get(*compat) {
                self.insert_matching_required(node, candidates, &mut selected);
            }
            self.compat_any_patterns
                .insert_matches(compat, node, &mut selected);
        }
        if let Some(first) = compats.first() {
            if let Some(candidates) = self.compat_first_exact.get(*first) {
                self.insert_matching_required(node, candidates, &mut selected);
            }
            self.compat_first_patterns
                .insert_matches(first, node, &mut selected);
        }

        if let Some(nodename) = node_nodename(node) {
            if let Some(candidates) = self.nodename_exact.get(nodename) {
                self.insert_matching_required(node, candidates, &mut selected);
            }
            self.nodename_patterns
                .insert_matches(nodename, node, &mut selected);
        }

        for prop in node.keys() {
            if let Some(candidates) = self.property_exact.get(prop) {
                self.insert_matching_required(node, candidates, &mut selected);
            }
            self.property_patterns
                .insert_matches(prop, node, &mut selected);
        }

        selected.sort_unstable();
        selected.dedup();
        selected
    }

    fn insert_matching_required<'a>(
        &'a self,
        node: &BTreeMap<String, DtValue>,
        candidates: &'a [SelectCandidate],
        selected: &mut Vec<usize>,
    ) {
        for candidate in candidates {
            if candidate.required_present(node) {
                selected.push(candidate.schema_index);
            }
        }
    }
}

impl SelectCandidate {
    fn required_present(&self, node: &BTreeMap<String, DtValue>) -> bool {
        self.required.iter().all(|key| node.contains_key(key))
    }
}

fn select_required(schema: &Value) -> Vec<String> {
    schema
        .get("select")
        .and_then(|select| select.get("required"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn select_key(schema: &Value) -> SelectKey {
    let Some(select) = schema.get("select") else {
        return SelectKey::Fallback;
    };
    if select == &Value::Bool(false) {
        return SelectKey::Never;
    }
    if select == &Value::Bool(true) {
        return property_interest_key(schema).unwrap_or(SelectKey::Fallback);
    }
    let Some(select_obj) = select.as_object() else {
        return SelectKey::Fallback;
    };

    if let Some(compatible) = select_obj
        .get("properties")
        .and_then(|props| props.get("compatible"))
        && select_requires(select, "compatible")
        && let Some(key) = compatible_select_key(compatible)
    {
        return key;
    }

    if let Some(nodename) = select_obj
        .get("properties")
        .and_then(|props| props.get("$nodename"))
        && let Some(key) = nodename_select_key(nodename)
    {
        return key;
    }

    SelectKey::Fallback
}

fn property_interest_key(schema: &Value) -> Option<SelectKey> {
    let obj = schema.as_object()?;
    for key in obj.keys() {
        if key.starts_with('$')
            || matches!(
                key.as_str(),
                "select"
                    | "title"
                    | "description"
                    | "maintainers"
                    | "examples"
                    | "type"
                    | "properties"
                    | "patternProperties"
                    | "dependentRequired"
                    | "dependentSchemas"
            )
        {
            continue;
        }
        return None;
    }

    let mut exact = std::collections::BTreeSet::new();
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        exact.extend(properties.keys().cloned());
    }
    if let Some(deps) = schema.get("dependentRequired").and_then(Value::as_object) {
        exact.extend(deps.keys().cloned());
    }
    if let Some(deps) = schema.get("dependentSchemas").and_then(Value::as_object) {
        exact.extend(deps.keys().cloned());
    }

    let mut patterns = Vec::new();
    if let Some(pattern_props) = schema.get("patternProperties").and_then(Value::as_object) {
        for pattern in pattern_props.keys() {
            patterns.push(pattern.clone());
        }
    }

    if exact.is_empty() && patterns.is_empty() {
        return None;
    }
    Some(SelectKey::PropertyInterest {
        exact: exact.into_iter().collect(),
        patterns,
    })
}

fn select_requires(select: &Value, key: &str) -> bool {
    select
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(key)))
}

fn compatible_select_key(schema: &Value) -> Option<SelectKey> {
    let (target, first_only) = if let Some(target) = schema.get("contains") {
        (target, false)
    } else {
        let items = schema.get("items")?;
        let target = match items {
            Value::Array(items) => items.first()?,
            Value::Object(_) => items,
            _ => return None,
        };
        (target, true)
    };

    if let Some(value) = target.get("const").and_then(Value::as_str) {
        let values = vec![value.to_string()];
        return Some(if first_only {
            SelectKey::CompatibleFirstExact(values)
        } else {
            SelectKey::CompatibleAnyExact(values)
        });
    }
    if let Some(values) = target.get("enum").and_then(Value::as_array) {
        let values: Vec<String> = values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        if !values.is_empty() {
            return Some(if first_only {
                SelectKey::CompatibleFirstExact(values)
            } else {
                SelectKey::CompatibleAnyExact(values)
            });
        }
    }
    if let Some(pattern) = target.get("pattern").and_then(Value::as_str) {
        return Some(if first_only {
            SelectKey::CompatibleFirstPattern(pattern.to_string())
        } else {
            SelectKey::CompatibleAnyPattern(pattern.to_string())
        });
    }
    None
}

fn nodename_select_key(schema: &Value) -> Option<SelectKey> {
    let items = schema.get("items")?;
    let target = match items {
        Value::Array(items) => items.first()?,
        Value::Object(_) => items,
        _ => return None,
    };
    if let Some(value) = target.get("const").and_then(Value::as_str) {
        return Some(SelectKey::NodenameExact(value.to_string()));
    }
    if let Some(pattern) = target.get("pattern").and_then(Value::as_str) {
        return Some(SelectKey::NodenamePattern(pattern.to_string()));
    }
    None
}

fn compatible_strings(node: &BTreeMap<String, DtValue>) -> Vec<&str> {
    match node.get("compatible") {
        Some(DtValue::List(items)) => items
            .iter()
            .filter_map(|item| match item {
                DtValue::Str(s) => Some(s.as_str()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn node_nodename(node: &BTreeMap<String, DtValue>) -> Option<&str> {
    match node.get("$nodename") {
        Some(DtValue::List(items)) => items.first().and_then(|item| match item {
            DtValue::Str(s) => Some(s.as_str()),
            _ => None,
        }),
        Some(DtValue::Str(s)) => Some(s.as_str()),
        _ => None,
    }
}

impl Retrieve for DtSchemaRetriever {
    fn retrieve(
        &self,
        uri: &Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let key = uri.as_str().trim_end_matches('#');
        if let Some(v) = self.schemas.get(key) {
            return Ok(validation_resource(v));
        }
        // A missed reference becomes a `false` schema that rejects everything.
        Ok(Value::Bool(false))
    }
}

impl DTValidator {
    /// Build from raw schema paths (files and/or directories), always adding
    /// the bundled core schemas.
    pub fn new(schema_paths: &[PathBuf]) -> anyhow::Result<Self> {
        let version = crate::version();
        if let [schema_file] = schema_paths
            && schema_file.is_file()
            && let Some(processed) = load_processed_schema_file(schema_file, &version)?
        {
            return Self::from_processed(processed);
        }
        let processed = ProcessedSchemas::build(schema_paths, true, &version);
        Self::from_processed(processed)
    }

    /// Build from an already-assembled processed schema set.
    pub fn from_processed(processed: ProcessedSchemas) -> anyhow::Result<Self> {
        let ProcessedSchemas {
            schemas,
            compat_map,
            always_schemas,
        } = processed;
        let type_ctx = TypeContext::from_processed(&schemas);
        let always_dispatch = AlwaysDispatch::build(&schemas, &always_schemas);
        let vendor_prefixes = schemas
            .get(VENDOR_PREFIXES_SCHEMA)
            .and_then(VendorPrefixesFastPath::build);
        let raw_validators = schemas
            .keys()
            .map(|schema_id| (schema_id.clone(), Arc::new(OnceLock::new())))
            .collect();
        let always_validators = always_schemas
            .iter()
            .map(|_| Arc::new(OnceLock::new()))
            .collect();
        let schemas = Arc::new(schemas);
        let retriever = Arc::new(DtSchemaRetriever {
            schemas: schemas.clone(),
        });
        Ok(Self {
            schemas,
            compat_map,
            always_schemas,
            type_ctx,
            retriever,
            always_dispatch,
            vendor_prefixes,
            raw_validators,
            always_validators,
        })
    }

    /// Decode a DTB into a devicetree tree (delegates to [`crate::dtb`]).
    pub fn decode_dtb(
        &self,
        dtb: &[u8],
        decode_errors: &mut Vec<String>,
    ) -> anyhow::Result<DtValue> {
        dtb::decode_dtb(&self.type_ctx, dtb, decode_errors)
    }

    fn retriever_handle(&self) -> SharedRetriever {
        SharedRetriever(self.retriever.clone())
    }

    /// Keep a schema only if the `$id` contains one of the filter substrings
    /// (or the filter is empty).
    fn filter_match(schema_id: &str, filter: Option<&[String]>) -> bool {
        match filter {
            None => true,
            Some(fs) => fs.is_empty() || fs.iter().any(|f| schema_id.contains(f.as_str())),
        }
    }

    /// Dispatch a node against the compatible-matched schema (first matching
    /// `compatible` string) and, unless `compatible_match`, every
    /// `always_schemas` entry as `{if:select, then:schema}`.
    pub fn iter_errors(
        &self,
        node: &DtValue,
        filter: Option<&[String]>,
        compatible_match: bool,
        show_unmatched: bool,
    ) -> Vec<DtError> {
        let mut out = Vec::new();
        let node_map = match node {
            DtValue::Node(m) => m,
            _ => return out,
        };

        // Compatible dispatch: use the first `compatible` string with a schema.
        if let Some(DtValue::List(compats)) = node_map.get("compatible") {
            for c in compats {
                if let DtValue::Str(cs) = c
                    && let Some(schema_id) = self.compat_map.get(cs)
                {
                    if Self::filter_match(schema_id, filter)
                        && let Some(schema) = self.schemas.get(schema_id)
                        && let Some(slot) = self.raw_validators.get(schema_id)
                    {
                        self.collect(schema, schema_id, false, slot, node, &mut out);
                    }
                    break;
                }
            }
        }

        if compatible_match {
            return out;
        }

        for schema_index in self.always_dispatch.candidates(node_map) {
            let Some(schema_id) = self.always_schemas.get(schema_index) else {
                continue;
            };
            if !show_unmatched && schema_id.as_str() == crate::GENERATED_COMPATIBLES_SCHEMA {
                continue;
            }
            if !Self::filter_match(schema_id, filter) {
                continue;
            }
            let Some(schema) = self.schemas.get(schema_id) else {
                continue;
            };
            let Some(slot) = self.always_validators.get(schema_index) else {
                continue;
            };
            self.collect(schema, schema_id, true, slot, node, &mut out);
        }

        out
    }

    /// Compile `schema` (once, cached) and collect its errors against `node`,
    /// tagging each with `schema_id`. `wrapped` selects the cache slot for the
    /// `{if:select, then:schema}` always-schema form vs the raw compat form.
    fn collect(
        &self,
        schema: &Value,
        schema_id: &str,
        wrapped: bool,
        slot: &ValidatorSlot,
        node: &DtValue,
        out: &mut Vec<DtError>,
    ) {
        if schema_id == VENDOR_PREFIXES_SCHEMA
            && let Some(fast) = &self.vendor_prefixes
            && fast.is_valid(node)
        {
            return;
        }
        let Some(validator) = self.cached_validator(schema, schema_id, wrapped, slot) else {
            return;
        };
        for err in validator.iter_errors(node) {
            out.push(to_dt_error(&err, schema_id));
        }
    }

    /// Return the compiled validator for `(schema_id, wrapped)`, building it on
    /// first use and caching the result. Returns `None` if the schema failed to
    /// compile (warned once).
    fn cached_validator(
        &self,
        schema: &Value,
        schema_id: &str,
        wrapped: bool,
        slot: &ValidatorSlot,
    ) -> Option<Arc<Validator<DtJson>>> {
        slot.get_or_init(|| match self.build_validator(schema, wrapped) {
            Ok(v) => Some(Arc::new(v)),
            Err(e) => {
                eprintln!("{schema_id}: error building validator: {e}");
                None
            }
        })
        .clone()
    }

    fn build_validator(&self, schema: &Value, wrapped: bool) -> anyhow::Result<Validator<DtJson>> {
        let schema = if wrapped {
            wrapped_validation_resource(schema)
        } else {
            validation_resource(schema)
        };
        dt_options(self.retriever_handle())
            .build(&schema)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Return the compatibles that do not match the `generated-compatibles`
    /// schema, i.e. are not documented by any binding.
    pub fn get_undocumented_compatibles(&self, compatibles: &[String]) -> Vec<String> {
        let Some(schema) = self.schemas.get(crate::GENERATED_COMPATIBLES_SCHEMA) else {
            return compatibles.to_vec();
        };
        let validator = match self.build_validator(schema, false) {
            Ok(v) => v,
            Err(_) => return compatibles.to_vec(),
        };
        let mut undoc = Vec::new();
        for c in compatibles {
            let instance = DtValue::Node(BTreeMap::from([(
                "compatible".to_string(),
                DtValue::List(vec![DtValue::Str(c.clone())]),
            )]));
            if !validator.is_valid(&instance) {
                undoc.push(c.clone());
            }
        }
        undoc
    }
}

fn load_processed_schema_file(
    path: &PathBuf,
    version: &str,
) -> anyhow::Result<Option<ProcessedSchemas>> {
    let text = std::fs::read_to_string(path)?;
    let value = match serde_json::from_str::<Value>(&text) {
        Ok(value) => value,
        Err(_) => match serde_yaml::from_str::<Value>(&text) {
            Ok(value) => value,
            Err(_) => {
                anyhow::bail!("preprocessed schema file is not valid JSON or YAML");
            }
        },
    };
    if value.get("$id").is_some() {
        return Ok(None);
    }
    ProcessedSchemas::from_value(&value, version).map(Some)
}

/// Convert a `jsonschema` error into our annotated [`DtError`].
fn to_dt_error(err: &ValidationError<'_>, schema_id: &str) -> DtError {
    let instance_path = location_segments(err.instance_path());
    let schema_path = location_segments(err.schema_path());
    let instance_is_disabled_node = instance_status_disabled(err.instance());
    let has_suppressible_disabled_context = error_has_suppressible_disabled_context(err);
    DtError {
        instance_path,
        schema_path,
        message: err.to_string(),
        schema_file: schema_id.to_string(),
        instance_is_disabled_node,
        has_suppressible_disabled_context,
    }
}

/// Split a `jsonschema` [`jsonschema::paths::Location`] into path segments.
fn location_segments(loc: &jsonschema::paths::Location) -> Vec<PathSeg> {
    loc.iter()
        .map(|seg| match seg {
            jsonschema::paths::LocationSegment::Property(p) => PathSeg::Key(p.to_string()),
            jsonschema::paths::LocationSegment::Index(i) => PathSeg::Index(i),
        })
        .collect()
}

fn error_has_suppressible_disabled_context(err: &ValidationError<'_>) -> bool {
    if schema_location_has_suppressible(err.schema_path()) {
        return true;
    }
    match err.kind() {
        ValidationErrorKind::AnyOf { context }
        | ValidationErrorKind::OneOfMultipleValid { context }
        | ValidationErrorKind::OneOfNotValid { context } => context
            .iter()
            .flatten()
            .any(error_has_suppressible_disabled_context),
        _ => false,
    }
}

fn schema_location_has_suppressible(loc: &jsonschema::paths::Location) -> bool {
    loc.iter().any(|seg| {
        matches!(seg, jsonschema::paths::LocationSegment::Property(p)
            if p == "required" || p == "unevaluatedProperties")
    })
}

/// True if the failing instance is a node carrying `status = "disabled"`.
fn instance_status_disabled(instance: &Value) -> bool {
    instance
        .as_object()
        .and_then(|m| m.get("status"))
        .map(|s| match s {
            Value::String(st) => st.contains("disabled"),
            Value::Array(a) => a
                .iter()
                .any(|v| v.as_str().is_some_and(|st| st.contains("disabled"))),
            _ => false,
        })
        .unwrap_or(false)
}

const DRAFT_2019_09_SCHEMA: &str = "https://json-schema.org/draft/2019-09/schema";

/// The validation engine understands JSON Schema drafts, not dt-schema's
/// custom meta-schema URIs. Compile DT validators as Draft 2019-09 without
/// mutating processed output.
fn validation_resource(schema: &Value) -> Value {
    let mut schema = schema.clone();
    normalize_dt_meta_schema(&mut schema);
    schema
}

fn wrapped_validation_resource(schema: &Value) -> Value {
    let select = schema.get("select").cloned().unwrap_or(Value::Bool(true));
    validation_resource(&serde_json::json!({ "if": select, "then": schema }))
}

fn normalize_dt_meta_schema(value: &mut Value) {
    match value {
        Value::Object(obj) => {
            if obj
                .get("$schema")
                .and_then(Value::as_str)
                .is_some_and(|s| s.starts_with("http://devicetree.org/meta-schemas/"))
            {
                obj.insert(
                    "$schema".to_string(),
                    Value::String(DRAFT_2019_09_SCHEMA.to_string()),
                );
            }
            for child in obj.values_mut() {
                normalize_dt_meta_schema(child);
            }
        }
        Value::Array(items) => {
            for child in items {
                normalize_dt_meta_schema(child);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_validator(schema: &Value) -> Validator<DtJson> {
        test_validator_with_schemas(schema, BTreeMap::new())
    }

    fn test_validator_with_schemas(
        schema: &Value,
        schemas: BTreeMap<String, Value>,
    ) -> Validator<DtJson> {
        let retriever = SharedRetriever(Arc::new(DtSchemaRetriever {
            schemas: Arc::new(schemas),
        }));
        dt_options(retriever).build(schema).unwrap()
    }

    #[test]
    fn dtjson_contains_drives_if_then_branch_selection() {
        let schema = json!({
            "$schema": "http://devicetree.org/meta-schemas/core.yaml#",
            "allOf": [
                {
                    "if": {
                        "properties": {
                            "compatible": {
                                "contains": {
                                    "enum": [
                                        "qcom,rpmcc-apq8060",
                                        "qcom,rpmcc-ipq806x",
                                        "qcom,rpmcc-msm8660"
                                    ]
                                }
                            }
                        }
                    },
                    "then": {
                        "properties": {
                            "clock-names": {
                                "items": [{ "const": "pxo" }]
                            }
                        }
                    }
                },
                {
                    "if": {
                        "properties": {
                            "compatible": {
                                "contains": {
                                    "const": "qcom,rpmcc-apq8064"
                                }
                            }
                        }
                    },
                    "then": {
                        "properties": {
                            "clock-names": {
                                "items": [{ "const": "pxo" }, { "const": "cxo" }]
                            }
                        }
                    }
                },
                {
                    "if": {
                        "properties": {
                            "compatible": {
                                "contains": {
                                    "enum": [
                                        "qcom,rpmcc-mdm9607",
                                        "qcom,rpmcc-msm8226",
                                        "qcom,rpmcc-msm8916"
                                    ]
                                }
                            }
                        }
                    },
                    "then": {
                        "properties": {
                            "clock-names": {
                                "items": [{ "const": "xo" }]
                            }
                        }
                    }
                }
            ]
        });
        let instance = DtValue::Node(BTreeMap::from([
            (
                "compatible".to_string(),
                DtValue::List(vec![
                    DtValue::Str("qcom,rpmcc-msm8916".to_string()),
                    DtValue::Str("qcom,rpmcc".to_string()),
                ]),
            ),
            (
                "clock-names".to_string(),
                DtValue::List(vec![DtValue::Str("xo".to_string())]),
            ),
        ]));

        let validator = test_validator(&schema);
        let errors: Vec<_> = validator.iter_errors(&instance).collect();
        assert!(
            errors.is_empty(),
            "msm8916 should only select the xo branch, got: {errors:#?}"
        );
    }

    #[test]
    fn dtjson_contains_pattern_drives_if_then_branch_selection() {
        let schema = json!({
            "$schema": "http://devicetree.org/meta-schemas/core.yaml#",
            "allOf": [
                {
                    "if": {
                        "properties": {
                            "compatible": {
                                "contains": {
                                    "pattern": "^qcom,adreno-305\\.[0-9]+$"
                                }
                            }
                        }
                    },
                    "then": {
                        "properties": {
                            "clock-names": {
                                "items": [
                                    { "const": "core" },
                                    { "const": "iface" },
                                    { "const": "mem_iface" }
                                ]
                            }
                        }
                    }
                },
                {
                    "if": {
                        "properties": {
                            "compatible": {
                                "contains": {
                                    "pattern": "^qcom,adreno-306\\.[0-9]+$"
                                }
                            }
                        }
                    },
                    "then": {
                        "properties": {
                            "clock-names": {
                                "items": [
                                    { "const": "core" },
                                    { "const": "iface" },
                                    { "const": "mem" },
                                    { "const": "mem_iface" },
                                    { "const": "alt_mem_iface" },
                                    { "const": "gfx3d" }
                                ]
                            }
                        }
                    }
                }
            ]
        });
        let instance = DtValue::Node(BTreeMap::from([
            (
                "compatible".to_string(),
                DtValue::List(vec![DtValue::Str("qcom,adreno-306.0".to_string())]),
            ),
            (
                "clock-names".to_string(),
                DtValue::List(vec![
                    DtValue::Str("core".to_string()),
                    DtValue::Str("iface".to_string()),
                    DtValue::Str("mem".to_string()),
                    DtValue::Str("mem_iface".to_string()),
                    DtValue::Str("alt_mem_iface".to_string()),
                    DtValue::Str("gfx3d".to_string()),
                ]),
            ),
        ]));

        let validator = test_validator(&schema);
        let errors: Vec<_> = validator.iter_errors(&instance).collect();
        assert!(
            errors.is_empty(),
            "adreno-306 should only select the six-clock branch, got: {errors:#?}"
        );
    }

    #[test]
    fn referenced_dt_schema_resource_keeps_validation_vocabularies() {
        let referenced = json!({
            "$id": "http://example.com/schemas/gpu.yaml#",
            "$schema": "http://devicetree.org/meta-schemas/core.yaml#",
            "allOf": [
                {
                    "if": {
                        "properties": {
                            "compatible": {
                                "contains": {
                                    "pattern": "^qcom,adreno-305\\.[0-9]+$"
                                }
                            }
                        }
                    },
                    "then": {
                        "properties": {
                            "clock-names": {
                                "items": [
                                    { "const": "core" },
                                    { "const": "iface" },
                                    { "const": "mem_iface" }
                                ]
                            }
                        }
                    }
                },
                {
                    "if": {
                        "properties": {
                            "compatible": {
                                "contains": {
                                    "pattern": "^qcom,adreno-306\\.[0-9]+$"
                                }
                            }
                        }
                    },
                    "then": {
                        "properties": {
                            "clock-names": {
                                "items": [
                                    { "const": "core" },
                                    { "const": "iface" },
                                    { "const": "mem" },
                                    { "const": "mem_iface" },
                                    { "const": "alt_mem_iface" },
                                    { "const": "gfx3d" }
                                ]
                            }
                        }
                    }
                }
            ]
        });
        let schema = json!({ "$ref": "http://example.com/schemas/gpu.yaml#" });
        let instance = DtValue::Node(BTreeMap::from([
            (
                "compatible".to_string(),
                DtValue::List(vec![DtValue::Str("qcom,adreno-306.0".to_string())]),
            ),
            (
                "clock-names".to_string(),
                DtValue::List(vec![
                    DtValue::Str("core".to_string()),
                    DtValue::Str("iface".to_string()),
                    DtValue::Str("mem".to_string()),
                    DtValue::Str("mem_iface".to_string()),
                    DtValue::Str("alt_mem_iface".to_string()),
                    DtValue::Str("gfx3d".to_string()),
                ]),
            ),
        ]));

        let validator = test_validator_with_schemas(
            &schema,
            BTreeMap::from([(
                "http://example.com/schemas/gpu.yaml".to_string(),
                referenced,
            )]),
        );
        let errors: Vec<_> = validator.iter_errors(&instance).collect();
        assert!(
            errors.is_empty(),
            "referenced dt-schema resource should keep validation vocabularies, got: {errors:#?}"
        );
    }

    #[test]
    fn anyof_required_context_is_suppressible_for_disabled_nodes() {
        let schema = json!({
            "anyOf": [
                { "required": ["foo"] },
                { "required": ["bar"] }
            ]
        });
        let instance = DtValue::Node(BTreeMap::from([(
            "status".to_string(),
            DtValue::Str("disabled".to_string()),
        )]));

        let validator = test_validator(&schema);
        let errors: Vec<_> = validator.iter_errors(&instance).collect();
        assert_eq!(errors.len(), 1, "expected one anyOf wrapper error");

        let error = to_dt_error(&errors[0], "http://example.com/test.yaml");
        assert!(error.instance_is_disabled_node);
        assert!(
            error.has_suppressible_disabled_context,
            "anyOf child required errors should be visible to disabled-node suppression"
        );
        assert!(
            !schema_location_has_suppressible(errors[0].schema_path()),
            "test should exercise nested context, not a top-level required path"
        );
    }

    #[test]
    fn generated_compatibles_respects_show_unmatched() {
        let version = crate::version();
        let processed = crate::process::ProcessedSchemas::from_value(
            &json!({
                "generated-compatibles": {
                    "$id": crate::GENERATED_COMPATIBLES_SCHEMA,
                    "$filename": "Generated schema of documented compatible strings",
                    "select": true,
                    "properties": {
                        "compatible": {
                            "items": {
                                "anyOf": [
                                    { "enum": ["vendor,known"] },
                                    { "pattern": "^test," }
                                ]
                            }
                        }
                    }
                },
                "version": version,
            }),
            &version,
        )
        .unwrap();
        let validator = DTValidator::from_processed(processed).unwrap();
        let instance = DtValue::Node(BTreeMap::from([(
            "compatible".to_string(),
            DtValue::List(vec![DtValue::Str("vendor,missing".to_string())]),
        )]));

        assert!(
            validator
                .iter_errors(&instance, None, false, false)
                .is_empty(),
            "generated-compatibles should stay suppressed unless -m is set"
        );
        let errors = validator.iter_errors(&instance, None, false, true);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].schema_file, crate::GENERATED_COMPATIBLES_SCHEMA);
    }

    #[test]
    fn dtjson_bytes_fail_null_and_multi_type_checks() {
        let bytes = DtValue::Bytes(vec![0x62, 0x61, 0x64, 0x00]);

        let null_validator = test_validator(&json!({ "type": "null" }));
        assert!(
            null_validator.iter_errors(&bytes).next().is_some(),
            "raw bytes must not validate as JSON null"
        );

        let dt_core_type = test_validator(&json!({
            "type": ["object", "integer", "array", "boolean", "null"]
        }));
        assert!(
            dt_core_type.iter_errors(&bytes).next().is_some(),
            "raw bytes must fail dt-core's generic property type union"
        );
    }
}
