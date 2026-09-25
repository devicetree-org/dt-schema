// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Parity tests: `dt-extract-example` must reproduce the expected output
//! byte-for-byte for the same input.
//!
//! Expected outputs are checked-in fixtures, so this test is hermetic.

use std::process::Command;

fn run(fixture: &str) -> String {
    let dir = env!("CARGO_MANIFEST_DIR");
    let input = format!("{dir}/tests/fixtures/{fixture}.yaml");
    let output = Command::new(env!("CARGO_BIN_EXE_dt-extract-example"))
        .arg(&input)
        .output()
        .expect("failed to run dt-extract-example");
    assert!(
        output.status.success(),
        "non-zero exit for {fixture}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout not utf-8")
}

#[test]
fn with_examples_and_interrupts() {
    let expected = include_str!("fixtures/with_examples.expected.dts");
    assert_eq!(run("with_examples"), expected);
}

#[test]
fn no_examples_key() {
    let expected = include_str!("fixtures/no_examples.expected.dts");
    assert_eq!(run("no_examples"), expected);
}
