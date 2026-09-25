// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! `dt-check-compatible`: check whether compatible strings are documented by
//! the schema set.
//!
//! Accepts positional `compatible_str...`, `-q/--quiet`, `-v/--invert-match`,
//! `-s/--schema` (required), and `-V/--version`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use dtschema::validator::DTValidator;

#[derive(Parser)]
#[command(disable_version_flag = true)]
struct Args {
    /// 1 or more compatible strings to check for a match.
    #[arg(required = true)]
    compatible_str: Vec<String>,
    /// Suppress printing matches.
    #[arg(short = 'q', long = "quiet")]
    quiet: bool,
    /// Invert sense of matching, printing compatibles which don't match.
    #[arg(short = 'v', long = "invert-match")]
    invert_match: bool,
    /// Path to processed schema file or schema directory.
    #[arg(short = 's', long = "schema")]
    schema: String,
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

    if !args.schema.is_empty() && !Path::new(&args.schema).exists() {
        return failure();
    }

    let validator = match DTValidator::new(&[PathBuf::from(&args.schema)]) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return failure();
        }
    };

    let undoc = validator.get_undocumented_compatibles(&args.compatible_str);

    if args.invert_match {
        if !undoc.is_empty() {
            if !args.quiet {
                println!("{}", undoc.join("\n"));
            }
            return ExitCode::SUCCESS;
        }
    } else {
        // Matches = inputs that ARE documented. Preserve input order and drop
        // duplicates.
        let mut seen = std::collections::HashSet::new();
        let matches: Vec<&String> = args
            .compatible_str
            .iter()
            .filter(|c| !undoc.contains(c) && seen.insert((*c).clone()))
            .collect();
        if !matches.is_empty() {
            if !args.quiet {
                for m in matches {
                    println!("{m}");
                }
            }
            return ExitCode::SUCCESS;
        }
    }

    failure()
}

/// Legacy failure status.
fn failure() -> ExitCode {
    ExitCode::from(255)
}
