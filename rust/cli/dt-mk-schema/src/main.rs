// SPDX-License-Identifier: BSD-2-Clause
// Copyright 2026 dt-schema contributors
//! `dt-mk-schema`: build a processed schema from raw binding YAML directories.
//!
//! Reads directories or YAML files, meta-validates and fixes up each, attaches
//! the generated type / compatible caches, and emits the result as JSON (`-j`)
//! or YAML.

use std::io::Write;
use std::path::PathBuf;

use clap::Parser;
use dtschema::process::ProcessedSchemas;

#[derive(Parser)]
#[command(name = "dt-mk-schema", about = "Build a processed devicetree schema")]
struct Args {
    /// Filename of the processed schema (default: stdout).
    #[arg(short = 'o', long = "outfile")]
    outfile: Option<PathBuf>,

    /// Encode the processed schema in JSON.
    #[arg(short = 'j', long = "json")]
    json: bool,

    /// Only process user schemas (skip the bundled core schemas).
    #[arg(short = 'u', long = "useronly")]
    useronly: bool,

    /// Names of directories, or YAML encoded schema files.
    schemas: Vec<PathBuf>,

    /// Print version number.
    #[arg(short = 'V', long = "version")]
    version: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse_from(argfile::expand_args(
        argfile::parse_fromfile,
        argfile::PREFIX,
    )?);

    if args.version {
        println!("{}", dtschema::version());
        return Ok(());
    }

    let ps = ProcessedSchemas::build(&args.schemas, !args.useronly, &dtschema::version());
    if ps.schemas.len() <= 1 {
        // Only the `version` marker → nothing processed.
        std::process::exit(255);
    }

    let mut out: Box<dyn Write> = match &args.outfile {
        Some(p) => Box::new(std::fs::File::create(p)?),
        None => Box::new(std::io::stdout()),
    };

    if args.json {
        let text = serde_json::to_string_pretty(&ps.schemas)?;
        writeln!(out, "{text}")?;
    } else {
        let value = serde_json::to_value(&ps.schemas)?;
        let s = serde_yaml::to_string(&value)?;
        write!(out, "{s}")?;
    }

    Ok(())
}
