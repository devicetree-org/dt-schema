// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! `dt-doc-validate`: meta-validate binding schema YAML files.
//!
//! Accepts positional `yamldt...`, `-v/--verbose`, `-n/--line-number`
//! (obsolete here; DTBs/loads carry no positions), `-u/--url-path`, and
//! `-V/--version`. Exits non-zero if any file has errors.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use dtschema::schema::DTSchema;

#[derive(Parser)]
#[command(disable_version_flag = true)]
struct Args {
    /// Directory or filename of YAML encoded devicetree schema file(s).
    yamldt: Vec<PathBuf>,
    /// Verbose mode.
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,
    /// Print line and column numbers (obsolete, accepted for compatibility).
    #[arg(short = 'n', long = "line-number")]
    line_number: bool,
    /// Additional search path for references.
    #[arg(short = 'u', long = "url-path")]
    url_path: Option<String>,
    /// Print version number.
    #[arg(short = 'V', long = "version")]
    version: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();

    if args.version {
        println!("{}", dtschema::version());
        return ExitCode::SUCCESS;
    }
    let _ = (args.line_number, &args.url_path, args.verbose);

    let mut ret = 0u8;
    for f in &args.yamldt {
        if f.is_dir() {
            let mut files = Vec::new();
            collect_yaml(f, &mut files);
            for filename in files {
                ret |= check_doc(&filename);
            }
        } else {
            ret |= check_doc(f);
        }
    }

    ExitCode::from(ret)
}

/// Meta-validate one file, print each error, then run the reference/constraint
/// check. Returns 1 if the file had validation errors.
fn check_doc(filename: &Path) -> u8 {
    let dtsch = match DTSchema::load(filename) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}: {e}", filename.display());
            return 1;
        }
    };

    let mut ret = 0;
    match dtsch.format_errors() {
        Ok(errors) => {
            for e in errors {
                eprintln!("{e}");
                ret = 1;
            }
        }
        Err(e) => {
            eprintln!("{}: error checking schema file: {e}", filename.display());
            return 1;
        }
    }

    dtsch.check_schema_refs();
    ret
}

/// Recursively collect `*.yaml` files under `dir`, sorted for stable output.
fn collect_yaml(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect_yaml(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("yaml") {
            out.push(p);
        }
    }
}
