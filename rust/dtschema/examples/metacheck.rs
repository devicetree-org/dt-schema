// SPDX-License-Identifier: BSD-2-Clause
use dtschema::schema::DTSchema;
fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let summary = args.first().map(|s| s == "--summary").unwrap_or(false);
    if summary {
        args.remove(0);
    }
    let mut invalid = 0;
    let mut total = 0;
    for f in &args {
        total += 1;
        let s = match DTSchema::load(std::path::Path::new(f)) {
            Ok(s) => s,
            Err(e) => {
                println!("{f}: LOAD-ERR {e}");
                invalid += 1;
                continue;
            }
        };
        match s.meta_validate() {
            Ok(errs) if errs.is_empty() => {
                if !summary {
                    println!("{f}: VALID");
                }
            }
            Ok(errs) => {
                invalid += 1;
                println!("{f}: {} errors", errs.len());
                if !summary {
                    for e in errs.iter().take(4) {
                        println!("  {e}");
                    }
                }
            }
            Err(e) => {
                invalid += 1;
                println!("{f}: ERR {e}");
            }
        }
    }
    if summary {
        println!("TOTAL={total} INVALID={invalid}");
    }
}
