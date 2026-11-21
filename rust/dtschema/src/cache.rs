// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Per-DTB diagnostics cache for `dt-validate`.
//!
//! A content-addressed JSON file per DTB, keyed by a SHA-256 of
//! `{cache_version, dtschema_version, dtb_hash, schema_hash, options}`. Stored
//! diagnostics use the `$dtb` filename sentinel so a cache entry is reusable
//! when only the DTB path (not its content) changes.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::diagnostic::display_path;

/// Cache format version shared with the installed tools.
pub const CACHE_VERSION: u64 = 2;
/// Filename sentinel stored in place of the real DTB path.
pub const CACHE_DTB_FILENAME: &str = "$dtb";

/// SHA-256 of a file's contents, as a lowercase hex string.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The cache-key options object.
pub struct CacheOptions {
    pub compatible_match: bool,
    pub limit: Option<Vec<String>>,
    pub show_unmatched: bool,
    pub verbose: bool,
}

impl CacheOptions {
    fn to_json(&self) -> Value {
        json!({
            "compatible_match": self.compatible_match,
            "limit": match &self.limit {
                Some(l) => Value::Array(l.iter().map(|s| json!(s)).collect()),
                None => Value::Null,
            },
            "show_unmatched": self.show_unmatched,
            "verbose": self.verbose,
        })
    }
}

/// A validation-diagnostics cache rooted at `cache_dir`.
pub struct ValidationCache {
    cache_dir: PathBuf,
    schema_hash: Option<String>,
    dtschema_version: String,
    options: Value,
}

impl ValidationCache {
    /// Build a cache handle. `schema_file` is hashed into every key.
    pub fn new(
        cache_dir: PathBuf,
        schema_file: Option<&Path>,
        dtschema_version: String,
        options: &CacheOptions,
    ) -> std::io::Result<Self> {
        let schema_hash = match schema_file {
            Some(p) => Some(sha256_file(p)?),
            None => None,
        };
        Ok(Self {
            cache_dir,
            schema_hash,
            dtschema_version,
            options: options.to_json(),
        })
    }

    fn cache_key(&self, filename: &Path) -> std::io::Result<String> {
        let dtb_hash = sha256_file(filename)?;
        let key = json!({
            "cache_version": CACHE_VERSION,
            "dtschema_version": self.dtschema_version,
            "dtb_hash": dtb_hash,
            "schema_hash": self.schema_hash,
            "options": self.options,
        });
        let data = canonical_json(&key);
        let mut hasher = Sha256::new();
        hasher.update(data.as_bytes());
        Ok(hex(&hasher.finalize()))
    }

    fn cache_file(&self, key: &str) -> PathBuf {
        self.cache_dir.join(format!("{key}.json"))
    }

    /// Load cached diagnostics for `filename`, rewriting the `$dtb` sentinel
    /// back to the file's display path. Returns `None` on any miss/error.
    pub fn load(&self, filename: &Path) -> Option<Vec<Value>> {
        let key = self.cache_key(filename).ok()?;
        let text = fs::read_to_string(self.cache_file(&key)).ok()?;
        let doc: Value = serde_json::from_str(&text).ok()?;
        let diags = doc.get("diagnostics")?.as_array()?.clone();
        let disp = display_path(&filename.to_string_lossy());
        Some(
            diags
                .iter()
                .map(|d| map_diagnostic_filename(d, CACHE_DTB_FILENAME, &disp))
                .collect(),
        )
    }

    /// Store `diagnostics` for `filename`, replacing the display path with the
    /// `$dtb` sentinel. Best-effort: failures are silent.
    pub fn store(&self, filename: &Path, diagnostics: &[Value]) {
        let _ = fs::create_dir_all(&self.cache_dir);
        let Ok(key) = self.cache_key(filename) else {
            return;
        };
        let disp = display_path(&filename.to_string_lossy());
        let mapped: Vec<Value> = diagnostics
            .iter()
            .map(|d| map_diagnostic_filename(d, &disp, CACHE_DTB_FILENAME))
            .collect();
        let doc = json!({
            "cache_version": CACHE_VERSION,
            "diagnostics": mapped,
        });
        // Write atomically via a temp file in the cache dir.
        let tmp = self.cache_dir.join(format!(".dt-validate-{key}.tmp.json"));
        if let Ok(mut text) = serde_json::to_string_pretty(&doc) {
            text.push('\n');
            if fs::write(&tmp, text).is_ok() {
                let _ = fs::rename(&tmp, self.cache_file(&key));
            } else {
                let _ = fs::remove_file(&tmp);
            }
        }
    }
}

/// Replace `file == old` with `new`, and rewrite `formatted` line prefixes,
/// recursively.
fn map_diagnostic_filename(value: &Value, old: &str, new: &str) -> Value {
    match value {
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|v| map_diagnostic_filename(v, old, new))
                .collect(),
        ),
        Value::Object(m) => {
            let mut out = serde_json::Map::new();
            for (k, v) in m {
                if k == "file" && v.as_str() == Some(old) {
                    out.insert(k.clone(), Value::String(new.to_string()));
                } else if k == "formatted" {
                    if let Some(s) = v.as_str() {
                        out.insert(
                            k.clone(),
                            Value::String(crate::diagnostic::replace_filename_prefix(s, old, new)),
                        );
                    } else {
                        out.insert(k.clone(), map_diagnostic_filename(v, old, new));
                    }
                } else {
                    out.insert(k.clone(), map_diagnostic_filename(v, old, new));
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Serialize deterministically for cache-key hashing: sorted object keys and
/// no insignificant whitespace.
fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => write_json_string(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(e, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(k, out);
                out.push(':');
                write_canonical(&m[*k], out);
            }
            out.push('}');
        }
    }
}

fn write_json_string(s: &str, out: &mut String) {
    // Cache-key strings are ASCII hashes, versions, and option keys.
    out.push_str(&serde_json::to_string(s).unwrap());
}
