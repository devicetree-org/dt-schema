// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! YAML loading into `serde_json::Value`.
//!
//! Schema files are written in a JSON-compatible subset of YAML. `serde_yaml`
//! parses `0xff`-style hex scalars as integers, so no post-processing is
//! required.

use serde_json::Value;

/// Errors from loading YAML.
#[derive(Debug, thiserror::Error)]
pub enum YamlError {
    #[error("YAML parse error: {0}")]
    Parse(#[from] serde_yaml::Error),
}

/// Load a YAML document from a string into a `serde_json::Value`.
pub fn from_str(text: &str) -> Result<Value, YamlError> {
    Ok(serde_yaml::from_str(text)?)
}

/// Load a YAML document from a file.
pub fn from_file(path: &std::path::Path) -> anyhow::Result<Value> {
    let text =
        std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{}: {}", path.display(), e))?;
    from_str(&text).map_err(|e| anyhow::anyhow!("{}: {}", path.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_scalars_become_integers() {
        let v = from_str("a: 0xff\nb: 0x1234\nc: 10\n").unwrap();
        assert_eq!(v["a"], serde_json::json!(255));
        assert_eq!(v["b"], serde_json::json!(0x1234));
        assert_eq!(v["c"], serde_json::json!(10));
    }

    #[test]
    fn hashed_keys_and_lists() {
        let v = from_str("'#interrupt-cells':\n  const: 2\nlist:\n  - 1\n  - foo\n").unwrap();
        assert_eq!(v["#interrupt-cells"]["const"], serde_json::json!(2));
        assert_eq!(v["list"], serde_json::json!([1, "foo"]));
    }
}
