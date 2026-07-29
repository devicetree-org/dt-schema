// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! CLI integration tests for `dt-check-compatible`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn dt_check_compatible_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_dt-check-compatible"))
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(dt_check_compatible_bin())
        .args(args)
        .output()
        .expect("run dt-check-compatible");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn assert_documented(schema: &Path) {
    let (rc, stdout, _stderr) = run(&["-s", schema.to_str().unwrap(), "vendor,soc1-ip"]);
    assert_eq!(rc, 0);
    assert_eq!(stdout, "vendor,soc1-ip\n");
}

fn assert_undocumented_invert(schema: &Path) {
    let (rc, stdout, _stderr) = run(&["-s", schema.to_str().unwrap(), "-v", "vendor,missing"]);
    assert_eq!(rc, 0);
    assert_eq!(stdout, "vendor,missing\n");
}

fn write_processed_schema(path: &Path) {
    let version = dtschema::version();
    std::fs::write(
        path,
        format!(
            r#"{{
  "generated-compatibles": {{
    "$id": "generated-compatibles",
    "$filename": "Generated schema of documented compatible strings",
    "select": true,
    "properties": {{
      "compatible": {{
        "items": {{
          "anyOf": [
            {{ "enum": ["vendor,soc1-ip"] }}
          ]
        }}
      }}
    }}
  }},
  "version": "{version}"
}}
"#
        ),
    )
    .unwrap();
}

#[test]
fn schema_directory_and_processed_file() {
    let repo = repo_root();
    let schemas = repo.join("test/schemas");
    assert_documented(&schemas);
    assert_undocumented_invert(&schemas);

    let tmp = std::env::temp_dir().join(format!("dt-check-compatible-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let processed = tmp.join("schema.json");
    write_processed_schema(&processed);

    assert_documented(&processed);
    assert_undocumented_invert(&processed);
}
