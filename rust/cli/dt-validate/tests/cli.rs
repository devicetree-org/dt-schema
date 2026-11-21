// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! CLI integration tests for `dt-validate`.
//!
//! These drive the built `dt-validate` binary against the repo's `test/*.dts`
//! fixtures (compiled on the fly with `dtc`) and the `test/schemas/` bindings.
//! Skipped (not failed) when `dtc` is not on `PATH`.

use std::path::{Path, PathBuf};
use std::process::Command;

use dtschema::process::ProcessedSchemas;
use serde_json::Value;

/// repo root: `rust/cli/dt-validate/` → up three.
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

fn dt_validate_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_dt-validate"))
}

fn have_dtc() -> bool {
    Command::new("dtc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn write_processed_schema(repo: &Path, path: &Path) {
    let schemas = repo.join("test/schemas");
    let processed = ProcessedSchemas::build(&[schemas], true, &dtschema::version());
    let mut text = serde_json::to_string_pretty(&processed.schemas).unwrap();
    text.push('\n');
    std::fs::write(path, text).unwrap();
}

/// Compile a `.dts` fixture to a `.dtb` under `out_dir`, returning its path.
fn compile_dtb(repo: &Path, dts_rel: &str, out: &Path) -> PathBuf {
    let dts = repo.join(dts_rel);
    let dtb = out.join(format!(
        "{}.dtb",
        dts.file_stem().unwrap().to_string_lossy()
    ));
    let status = Command::new("dtc")
        .args(["-Odtb", "-o"])
        .arg(&dtb)
        .arg(&dts)
        .output()
        .expect("run dtc");
    assert!(
        status.status.success(),
        "dtc failed for {dts_rel}:\n{}",
        String::from_utf8_lossy(&status.stderr)
    );
    dtb
}

/// Run `dt-validate -s <test/schemas> <extra...>`, returning (stdout, stderr).
fn run_validate(repo: &Path, extra: &[&str]) -> (String, String) {
    let schemas = repo.join("test/schemas");
    let mut cmd = Command::new(dt_validate_bin());
    cmd.arg("-s").arg(&schemas);
    for a in extra {
        cmd.arg(a);
    }
    let out = cmd.output().expect("run dt-validate");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Number of real diagnostic lines on stderr (ignoring the harmless
/// `bad-example.yaml: ignoring, error in schema` meta-validation notice that the
/// `test/schemas` set always emits).
fn diag_line_count(stderr: &str) -> usize {
    stderr
        .lines()
        .filter(|l| !l.is_empty() && !l.contains("ignoring, error in schema"))
        .count()
}

#[test]
fn test_dtb_validation() {
    if !have_dtc() {
        eprintln!("SKIP: dtc not available");
        return;
    }
    let repo = repo_root();
    let tmp = std::env::temp_dir().join("dt-validate-cli-dtbs");
    std::fs::create_dir_all(&tmp).unwrap();

    let mut entries: Vec<PathBuf> = std::fs::read_dir(repo.join("test"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("dts"))
        .collect();
    entries.sort();
    assert!(!entries.is_empty(), "no .dts fixtures found");

    for dts in entries {
        let rel = format!("test/{}", dts.file_name().unwrap().to_string_lossy());
        let name = dts.file_stem().unwrap().to_string_lossy().into_owned();
        let expect_fail = name.contains("-fail");
        let dtb = compile_dtb(&repo, &rel, &tmp);
        let (stdout, stderr) = run_validate(&repo, &[dtb.to_str().unwrap()]);
        assert_eq!(stdout, "", "{name}: stdout should be empty");
        let diags = diag_line_count(&stderr);
        if expect_fail {
            assert!(
                diags > 0,
                "{name}: expected validation errors, got none.\nstderr:\n{stderr}"
            );
        } else {
            assert_eq!(
                diags, 0,
                "{name}: expected clean validation, got diagnostics:\n{stderr}"
            );
        }
    }
}

#[test]
fn test_json_cli_output_file() {
    if !have_dtc() {
        eprintln!("SKIP: dtc not available");
        return;
    }
    let repo = repo_root();
    let tmp = std::env::temp_dir().join("dt-validate-cli-json");
    std::fs::create_dir_all(&tmp).unwrap();
    let dtb = compile_dtb(&repo, "test/device-fail.dts", &tmp);
    let json_out = tmp.join("out.json");

    let (stdout, stderr) = run_validate(
        &repo,
        &[
            "--json-output",
            json_out.to_str().unwrap(),
            dtb.to_str().unwrap(),
        ],
    );

    assert_eq!(stdout, "");
    assert!(
        stderr.contains("from schema $id:"),
        "stderr missing schema note:\n{stderr}"
    );

    let text = std::fs::read_to_string(&json_out).unwrap();
    let diagnostics: Vec<Value> = serde_json::from_str(&text).unwrap();
    assert!(!diagnostics.is_empty());

    let validation = diagnostics
        .iter()
        .find(|d| d["type"] == "validation")
        .expect("a validation diagnostic");
    assert_eq!(validation["level"], "error");
    assert!(validation.get("message").is_some());
    assert!(validation.get("formatted").is_some());
    assert!(validation.get("schema").is_some());
}

#[test]
fn test_cli_cache_output() {
    if !have_dtc() {
        eprintln!("SKIP: dtc not available");
        return;
    }
    let repo = repo_root();
    let tmp = std::env::temp_dir().join("dt-validate-cli-cache");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let dtb = compile_dtb(&repo, "test/device-fail.dts", &tmp);
    let dtb2 = tmp.join("copy.dtb");
    std::fs::copy(&dtb, &dtb2).unwrap();

    let schema = tmp.join("schema.json");
    write_processed_schema(&repo, &schema);

    let cache_dir = tmp.join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let json_out = tmp.join("out.json");

    let run = |target: &Path| -> (String, String) {
        let out = Command::new(dt_validate_bin())
            .args(["--json-output"])
            .arg(&json_out)
            .arg("--cache-dir")
            .arg(&cache_dir)
            .arg("-s")
            .arg(&schema)
            .arg(target)
            .output()
            .expect("run dt-validate");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    // First run: populates the cache.
    let (stdout, stderr) = run(&dtb);
    assert_eq!(stdout, "");
    assert!(stderr.contains("from schema $id:"));
    assert!(
        stderr.contains("vendor,bool-prop: size (5) error for type flag"),
        "missing decode error:\n{stderr}"
    );
    let first: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(&json_out).unwrap()).unwrap();
    let decode = first
        .iter()
        .find(|d| {
            d["type"] == "decode"
                && d["message"] == "vendor,bool-prop: size (5) error for type flag"
        })
        .expect("decode diagnostic present");
    assert_eq!(decode["file"], dtb.to_string_lossy().into_owned());

    // Second run of the same file: served from cache, identical output.
    let (stdout, stderr) = run(&dtb);
    assert_eq!(stdout, "");
    assert!(stderr.contains("from schema $id:"));
    assert!(stderr.contains("vendor,bool-prop: size (5) error for type flag"));
    let second: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(&json_out).unwrap()).unwrap();
    assert_eq!(second, first);
    assert_eq!(std::fs::read_dir(&cache_dir).unwrap().count(), 1);

    // A byte-identical copy reuses the cache (via the `$dtb` sentinel) but
    // reports its own path.
    let (stdout, stderr) = run(&dtb2);
    assert_eq!(stdout, "");
    assert!(stderr.contains("from schema $id:"));
    let third: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(&json_out).unwrap()).unwrap();
    let validation = third
        .iter()
        .find(|d| d["type"] == "validation")
        .expect("a validation diagnostic");
    assert_eq!(validation["file"], dtb2.to_string_lossy().into_owned());
    let formatted = validation["formatted"].as_str().unwrap();
    assert!(
        formatted.starts_with(&format!("{}:", dtb2.to_string_lossy())),
        "formatted should start with the copy's path: {formatted}"
    );
    assert_eq!(std::fs::read_dir(&cache_dir).unwrap().count(), 1);
}
