// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Unit tests for the diagnostic layer, mirroring the diagnostic-shaping cases
//! in the Python `test/test-dt-validate.py` (`test_json_error_diagnostic`,
//! `test_json_unmatched_diagnostic`, `test_format_error_rewrites_indented_paths`).
//!
//! The Python error diagnostics carry source `line`/`column` from
//! `error.linecol`; a decoded DTB carries no source positions, so the Rust port
//! reports `line`/`column` as `null` and we assert the remaining fields.

use dtschema::diagnostic::{
    diagnostic_text, error_diagnostic, replace_filename_prefix, unmatched_diagnostic,
};
use dtschema::validator::{DtError, PathSeg};
use serde_json::json;

fn key(s: &str) -> PathSeg {
    PathSeg::Key(s.to_string())
}

#[test]
fn test_json_error_diagnostic() {
    let error = DtError {
        instance_path: vec![key("soc"), key("device@0")],
        schema_path: vec![key("then"), key("required")],
        message: "'foo' is a required property".to_string(),
        schema_file: "http://devicetree.org/schemas/test.yaml#".to_string(),
        instance_is_disabled_node: false,
        has_suppressible_disabled_context: false,
    };

    let d = error_diagnostic(
        "test.dtb",
        &error,
        Some("device@0"),
        Some("/soc/device@0"),
        Some("test,device"),
        None,
    )
    .to_value();

    assert_eq!(d["type"], "validation");
    assert_eq!(d["level"], "error");
    assert_eq!(d["file"], "test.dtb");
    assert_eq!(d["node"], "/soc/device@0");
    assert_eq!(d["nodename"], "device@0");
    assert_eq!(d["compatible"], "test,device");
    assert_eq!(d["property_path"], json!(["soc", "device@0"]));
    assert_eq!(d["schema_path"], json!(["then", "required"]));
    assert_eq!(d["schema"], "http://devicetree.org/schemas/test.yaml#");
    assert_eq!(d["message"], "'foo' is a required property");
}

#[test]
fn test_json_unmatched_diagnostic() {
    let d =
        unmatched_diagnostic("test.dtb", "/soc/device@0", &["test,device".to_string()]).to_value();
    assert_eq!(d["type"], "unmatched");
    assert_eq!(d["level"], "warning");
    assert_eq!(d["file"], "test.dtb");
    assert_eq!(d["node"], "/soc/device@0");
    assert_eq!(d["compatible"], json!(["test,device"]));
    assert_eq!(
        d["message"],
        "failed to match any schema with compatible: ['test,device']"
    );
    assert_eq!(
        diagnostic_text(&d),
        "test.dtb: /soc/device@0: failed to match any schema with compatible: ['test,device']"
    );

    // A root node ("/") keeps "/" as its nodename.
    let d = unmatched_diagnostic("test.dtb", "/", &["test,board".to_string()]).to_value();
    assert_eq!(d["nodename"], "/");
}

#[test]
fn test_format_error_rewrites_indented_paths() {
    // Python's `_format_error` rewrites the absolute-path prefix to the display
    // path on every line, including tab-indented sub-error lines. This is the
    // core behaviour of `replace_filename_prefix`.
    let text = "/abs/test.dtb:1:1: outer problem\n\t/abs/test.dtb:2:1: inner problem\n";
    let rewritten = replace_filename_prefix(text, "/abs/test.dtb", "test.dtb");

    assert!(!rewritten.contains("/abs/test.dtb"));
    assert!(rewritten.contains("test.dtb:1:1"));
    assert!(rewritten.contains("\ttest.dtb:2:1"));
}
