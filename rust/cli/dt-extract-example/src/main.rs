// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! Emits the DTS example(s) from a binding YAML file, wrapped in a minimal
//! device tree so the output can be piped to `dtc`. This tool is fully
//! self-contained templating; it does not use the dtschema validation library
//! beyond the shared YAML loader.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use regex::Regex;

// Template strings preserve the legacy output format. Literal braces are
// written directly and `{...}` placeholders are substituted by hand.

// interrupt_template, with `{index}` and `{int_cells}` substituted.
fn interrupt_template(index: usize, int_cells: usize) -> String {
    format!(
        "\n        interrupt-parent = <&fake_intc{index}>;\n        fake_intc{index}: fake-interrupt-controller {{\n            interrupt-controller;\n            #interrupt-cells = < {int_cells} >;\n        }};\n"
    )
}

// example_template, with `{example_num}`, `{interrupt}`, `{example}` substituted.
fn example_template(example_num: usize, interrupt: &str, example: &str) -> String {
    format!(
        "\n    example-{example_num} {{\n        #address-cells = <1>;\n        #size-cells = <1>;\n\n        {interrupt}\n\n        {example}\n    }};\n}};\n"
    )
}

const EXAMPLE_HEADER: &str = "\n/dts-v1/;\n/plugin/; // silence any missing phandle references\n";

const EXAMPLE_START: &str = "\n/{\n    compatible = \"foo\";\n    model = \"foo\";\n    #address-cells = <1>;\n    #size-cells = <1>;\n\n";

#[derive(Parser)]
#[command(version, about = None, long_about = None)]
struct Args {
    /// Filename of YAML encoded schema input file
    yamlfile: PathBuf,
}

/// Split lines while preserving the line terminators that occur in these YAML
/// documents (`\n`, `\r\n`, `\r`).
fn splitlines_keepends(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut result = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                result.push(&s[start..=i]);
                i += 1;
                start = i;
            }
            b'\r' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    result.push(&s[start..=i + 1]);
                    i += 2;
                } else {
                    result.push(&s[start..=i]);
                    i += 1;
                }
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < bytes.len() {
        result.push(&s[start..]);
    }
    result
}

/// Compute `int_cells` for one example.
fn interrupt_cells(ex: &str, int_re: &Regex, paren_re: &Regex) -> usize {
    let Some(caps) = int_re.captures(ex) else {
        return 0;
    };
    let Some(int_val) = caps.get(1) else {
        return 0;
    };
    let int_val = paren_re.replace_all(int_val.as_str(), "0");
    // `split_whitespace` already skips leading/trailing whitespace.
    int_val.split_whitespace().count()
}

fn run(args: &Args) -> ExitCode {
    let value = match dtschema::yaml::from_file(&args.yamlfile) {
        Ok(v) => v,
        Err(e) => {
            // Best-effort: ruamel exposes a problem_mark (line:col) which
            // serde_yml does not surface in the same shape, so the message
            // text differs, but the exit status matches.
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };

    // Non-object YAML documents do not contain binding examples.
    if !value.is_object() {
        return ExitCode::SUCCESS;
    }

    let root_re = Regex::new(r"/\s*\{").unwrap();
    let int_re = Regex::new(r"\sinterrupts\s*=\s*<([0-9a-zA-Z |()_]+)>").unwrap();
    let paren_re = Regex::new(r"\(.+|\)").unwrap();

    let mut example_dts = String::from(EXAMPLE_HEADER);

    if let Some(examples) = value.get("examples") {
        // Real bindings always store examples as a list of strings.
        for (idx, item) in examples.as_array().into_iter().flatten().enumerate() {
            let ex = item.as_str().unwrap_or("");

            if root_re.is_match(ex) {
                example_dts.push_str(ex);
            } else {
                let int_cells = interrupt_cells(ex, &int_re, &paren_re);
                example_dts.push_str(EXAMPLE_START);
                let ex_joined = splitlines_keepends(ex).join("        ");
                let int_props = if int_cells > 0 {
                    interrupt_template(idx, int_cells)
                } else {
                    String::new()
                };
                example_dts.push_str(&example_template(idx, &int_props, &ex_joined));
            }
        }
    } else {
        example_dts.push_str(EXAMPLE_START);
        example_dts.push_str("\n};");
    }

    // Preserve the legacy trailing newline.
    println!("{example_dts}");
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args = Args::parse();
    run(&args)
}
