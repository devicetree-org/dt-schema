// SPDX-License-Identifier: BSD-2-Clause
use dtschema::schema::DTSchema;
fn main() {
    for f in [
        "../test/schemas/good-example.yaml",
        "../test/schemas/bad-example.yaml",
    ] {
        let s = DTSchema::load(std::path::Path::new(f)).unwrap();
        println!(
            "{f}: id={:?} schema={:?}",
            s.id(),
            s.value.get("$schema").and_then(|v| v.as_str())
        );
    }
}
