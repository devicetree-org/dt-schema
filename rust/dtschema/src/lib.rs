// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Devicetree schema validation library.
//!
//! The port keeps `serde_json::Value` as the universal representation for both
//! schema documents and decoded devicetree data.

pub mod cache;
pub mod diagnostic;
pub mod dtb;
pub mod fixups;
pub mod lib_helpers;
pub mod process;
pub mod schema;
pub mod types;
pub mod validator;
pub mod yaml;

use std::path::{Path, PathBuf};

/// Synthetic schema ID for "is this compatible documented anywhere?".
pub const GENERATED_COMPATIBLES_SCHEMA: &str = "generated-compatibles";

/// Locate the bundled `schemas/` and `meta-schemas/` data directories.
///
/// The schema data lives in `dtschema/`; from the Rust workspace
/// (`rust/dtschema/`) it is at `../../dtschema/`. Allow an override via the
/// `DTSCHEMA_DIR` environment variable so the tools work from an installed
/// location too.
pub fn bundled_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("DTSCHEMA_DIR") {
        return PathBuf::from(dir);
    }
    // rust/dtschema/src/lib.rs -> repo/dtschema
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent() // rust/
        .and_then(|p| p.parent()) // repo/
        .map(|p| p.join("dtschema"))
        .unwrap_or_else(|| PathBuf::from("dtschema"))
}

/// The dtschema version string used as a processed-schema cache key.
///
/// A processed schema's `version` field must match to be reused, so read the
/// generated version module when present to keep processed schemas
/// interchangeable with the installed tools. Falls back to the crate version.
pub fn version() -> String {
    let vpy = bundled_dir().join("version.py");
    if let Ok(text) = std::fs::read_to_string(&vpy) {
        for line in text.lines() {
            let line = line.trim_start();
            if line.starts_with("__version__") && line.contains('=') {
                if let Some(start) = line.find('\'')
                    && let Some(end) = line[start + 1..].find('\'')
                {
                    return line[start + 1..start + 1 + end].to_string();
                }
                if let Some(start) = line.find('"')
                    && let Some(end) = line[start + 1..].find('"')
                {
                    return line[start + 1..start + 1 + end].to_string();
                }
            }
        }
    }
    env!("CARGO_PKG_VERSION").to_string()
}
