# AGENTS.md

Guidance for AI agents (and humans) working in the **dt-schema** repository.

## What this repo is

This is **devicetree-org/dt-schema** — the `dtschema` Python package: tools and schema
data for validating Devicetree files and Devicetree *binding* documents using the
[json-schema](https://json-schema.org) vocabulary. Schema files are written in a
JSON-compatible subset of YAML so they are both human- and machine-readable.

There are **two kinds of data files**:

- **Schemas** (`dtschema/schemas/`) — constrain actual Devicetree *data*. This repo
  holds only the *core* schemas: properties from the DT Specification plus common
  bindings (GPIO, clock, PHY, interrupts, PCI, …). **Device-specific bindings do NOT
  live here** — they are maintained in the Linux kernel tree alongside the `.dts` files.
- **Meta-schemas** (`dtschema/meta-schemas/`) — constrain the *schema files themselves*.
  Plain json-schema silently ignores unknown keywords; the meta-schemas restrict what a
  binding may contain and catch common authoring mistakes.

License: BSD-2-Clause. Author: Rob Herring <robh@kernel.org>.

## Repo layout

| Path | Purpose |
|------|---------|
| `dtschema/` | The Python package: library modules, CLI `main()`s, and bundled `schemas/` + `meta-schemas/` data |
| `dtschema/schemas/` | Core/common Devicetree binding schemas (constrain DT data) |
| `dtschema/meta-schemas/` | Meta-schemas (constrain the binding schema files) |
| `test/` | Test suite `test-dt-validate.py`, `.dts` fixtures, example schemas under `test/schemas/` |
| `tools/` | Standalone helper scripts: `dt-prop-populate`, `yaml-format`, `yaml2json` |
| `.github/workflows/` | `ci.yml` (lint + test matrix) and `publish.yml` (PyPI on tags) |
| `pyproject.toml` | Package metadata, deps, and `[project.scripts]` entry points |
| `example-schema.yaml` | Annotated reference template for authoring a new binding |
| `.yamllint` | YAML lint config (enforced in CI) |
| `README.md` | User-facing docs (see note below) |

> **Note:** `README.md` refers to `tools/dt-validate`, `tools/dt-mk-schema`, etc. That
> wording is dated — those are installed as **console entry points** (see below), not
> scripts in `tools/`. The `tools/` directory only contains the three helper scripts.
> There is no `Makefile`, `setup.py`, `setup.cfg`, `tox.ini`, or `CONTRIBUTING`.

## CLI tools

Console entry points are declared in `pyproject.toml` under `[project.scripts]`; each
maps to a `main()` in a `dtschema/*.py` module. All argparse tools accept `@file` to
read arguments from a file (`fromfile_prefix_chars='@'`).

| Command | Module | Purpose |
|---------|--------|---------|
| `dt-validate` | `dtb_validate.py` | Validate DTB(s) (or a dir of `**/*.dtb`) against schemas |
| `dt-doc-validate` | `doc_validate.py` | Validate binding YAML file(s) against the meta-schema |
| `dt-mk-schema` | `mk_schema.py` | Preprocess schemas into a single processed schema file |
| `dt-check-compatible` | `check_compatible.py` | Test whether compatible string(s) are documented |
| `dt-extract-example` | `extract_example.py` | Emit the DTS example(s) from a binding (pipe to `dtc`) |
| `dt-extract-props` | `extract_props.py` | Dump the property→type(s) map derived from schemas |
| `dt-cmp-schema` | `cmp_schema.py` | Compare two schema sets for possible ABI regressions |
| `dtb2py` | `dtb2py.py` | Decode a DTB into a Python dict dump |

Also present but **not** console entry points: `dtschema/extract_compatibles.py` (a
runnable module that prints `enum` compatibles from a single binding) and the `tools/`
helpers (`yaml2json`, `yaml-format`, `dt-prop-populate`).

## Core library & how validation works

Module map (all under `dtschema/`):

- **`lib.py`** — low-level helpers: `sized_int` (an `int` carrying a bit `.size`),
  `_is_int_schema`/`_is_string_schema`, `extract_compatibles`, `_get_array_range`, and
  `format_error` (the central human-readable error formatter).
- **`schema.py`** — `DTSchema` represents **one** binding file, loads its YAML,
  meta-validates it (`iter_errors`/`is_valid`) against the meta-schema named by the
  binding's `$schema`, applies `fixup()`, and runs `check_schema_refs()` (verifies `$id`
  matches the file path, refs resolve, and node schemas carry an
  additional/unevaluatedProperties constraint).
- **`fixups.py`** — `fixup_schema` expands the compact DT authoring syntax into strict
  json-schema before validation: string→array, `items` list→fixed-size array (adds
  `minItems`/`maxItems`), unit-suffix typing (`-hz`, `-microvolt`, `-ohms`, …),
  `interrupts`/`interrupts-extended` handling, and implicit node props (`phandle`,
  `status`, `pinctrl-*`, `bootph-*`, …).
- **`validator.py`** — `DTValidator` loads/preprocesses **all** schemas, builds a
  `compat_map` (compatible → schema `$id`) and `always_schemas` (schemas with a
  `select`), exposes `iter_errors`, the custom `typeSize` keyword, the property-type
  cache, and the synthetic `generated-compatibles` schema.
- **`dtb.py`** — decodes a flattened DTB via `pylibfdt` into a typed nested Python tree,
  using the property-type cache to turn raw bytes into ints / strings / matrices /
  phandle tuples.

Two validation flows:

- **Flow A — binding vs meta-schema** (`dt-doc-validate`): `DTSchema.iter_errors()`
  validates the binding against the meta-schema referenced by its `$schema`, then
  `check_schema_refs()` checks `$id`/references.
- **Flow B — DTB vs schemas** (`dt-validate`): `decode_dtb` unflattens the DTB, then
  each node is validated by (1) the schema matched from its first known `compatible` in
  `compat_map`, and (2) every `select`-bearing `always_schemas` entry applied as
  `{if: select, then: schema}`. Disabled nodes suppress `required`/`unevaluatedProperties`
  errors. Nodes matching no schema are surfaced only with `-m/--show-unmatched`.

Preprocessing & caching:

- **`dt-mk-schema`** serializes the fully processed `.schemas` dict (including
  `generated-types`, `generated-pattern-types`, `generated-compatibles`, and a `version`
  stamp). `DTValidator` can reload that file directly to skip all fixup/type work;
  a `version` mismatch raises *"Processed schema out of date, delete and retry"*.
- **`dt-validate --cache-dir`** is a *separate* per-DTB diagnostics cache: one
  `<sha256>.json` per DTB, keyed on cache/dtschema versions + DTB hash + schema hash +
  options, with file paths normalized to the `$dtb` sentinel so entries are
  path-independent.

`dt-validate` also supports structured/CI-friendly output: `--json-output` (writes
diagnostics as JSON), relative in-tree DTB paths (`_display_path`), and the `--cache-dir`
diagnostics cache above — all implemented in `dtschema/dtb_validate.py`.

## Dev setup & build

Editable install against this tree (pulls all deps from `pyproject.toml`):

```
pip3 install -e .
```

> **Modern Linux note:** most current distros mark the system Python as
> *externally managed* (PEP 668), so a bare `pip3 install` fails unless you're inside a
> virtualenv. Either work in a venv (`python3 -m venv .venv && . .venv/bin/activate`,
> then `pip install -e .`), or use **pipx** to install the CLIs into an isolated
> environment:
> ```
> pipx install dtschema                      # end users
> pipx install --editable .                  # this tree, for development
> pipx install git+https://github.com/devicetree-org/dt-schema.git@main
> ```
> pipx puts the `dt-*` executables on `PATH` (`~/.local/bin`) while keeping their deps
> off the system Python. For editable dev work where you `import dtschema` in your own
> scripts/tests, prefer a venv.

Runtime deps: `ruamel.yaml>0.15.69`, `jsonschema>=4.18`, `rfc3987`, `pylibfdt`
(building pylibfdt needs `swig`). The `dtc` device-tree-compiler is needed to produce
DTBs for tests. Version is dynamic via `setuptools_scm` (writes `dtschema/version.py`).

> **Sanity check your install:** if `dt-*` binaries are already on `PATH` from a distro
> package or an old `pip`/`pipx` install, they can shadow this tree — confirm with
> `dt-validate --version` and `python3 -c "import dtschema; print(dtschema.__file__)"`.
> A base `python3` that can't `import dtschema` (e.g. missing `referencing`, a
> `jsonschema` dep) just means the deps aren't on that interpreter; install into a
> venv/pipx as above.

## Testing & CI

Tests are a single self-executing `unittest` script (no pytest/tox config). It needs
`dtc` on `PATH`.

```
test/test-dt-validate.py          # or: python -m unittest test/test-dt-validate.py
```

It covers: all meta-schemas are valid Draft2019-09; the good/bad example schemas
pass/fail as expected; **every** bundled `dtschema/schemas/**/*.yaml` validates against
the meta-schema, has a unique `$id`, and resolves its refs; and DTB fixtures — files
named `*-fail.dts` must raise `ValidationError`, others must pass.

CI (`.github/workflows/ci.yml`) runs on push/PR across Python 3.9–3.14:

```
flake8 . --select=E9,F63,F7,F82 --show-source --statistics   # fatal
flake8 . --exit-zero --max-complexity=10 --max-line-length=127 --statistics   # advisory
yamllint --strict $(git ls-files '*.yaml')
test/test-dt-validate.py
```

`publish.yml` builds with `python -m build` and publishes to PyPI on `v*` tags
(excluding `v*-pre`).

## Conventions for editing schemas

- Use **`example-schema.yaml`** (repo root) as the authoring template — it is heavily
  annotated with the meaning of each keyword. For a minimal binding that is *guaranteed
  valid* (the test suite asserts it passes strict meta-schema validation), copy
  `test/schemas/good-example.yaml`; `test/schemas/bad-example.yaml` is the deliberately
  invalid counterpart. Reformat with `tools/yaml-format`.
- YAML style is enforced by `.yamllint`: 2-space indentation (sequences indented),
  single quotes only-when-needed, files must start with `---` (`document-start`),
  no empty values, line-length warning at 110. Property names starting with `#` (e.g.
  `'#interrupt-cells'`) must be quoted.
- Every binding needs `$id` (a `http://devicetree.org/schemas/...` URL matching its
  path), `$schema` (usually `.../meta-schemas/core.yaml#`), `title`, `maintainers`, and
  an `additionalProperties`/`unevaluatedProperties` constraint on node schemas.
- After editing a binding, run `dt-doc-validate <file>` and the full test suite.

## Quick command reference

```
# editable dev install (deps from pyproject.toml)
# on PEP 668 distros, do this in a venv, or use: pipx install --editable .
pip3 install -e .

# run the test suite (needs dtc)
test/test-dt-validate.py

# lint exactly as CI does
flake8 . --count --select=E9,F63,F7,F82 --show-source --statistics
yamllint --strict $(git ls-files '*.yaml')

# validate a binding against the meta-schema
dt-doc-validate test/schemas/good-example.yaml

# build a processed schema, then validate a DTB against it
dt-mk-schema -j test/schemas/ > processed-schema.json
dtc -O dtb -o device.dtb test/device.dts
dt-validate -s processed-schema.json device.dtb

# check whether a compatible is documented
dt-check-compatible -s processed-schema.json vendor,a-compatible
```

## Linux kernel integration

dt-schema is the engine behind the kernel's `make dt_binding_check` and
`make dtbs_check` targets — but those Makefile targets live in the Linux source tree
(`Documentation/devicetree/bindings/Makefile`), **not** here. The integration contract
is the installed CLIs (`dt-mk-schema`, `dt-doc-validate`, `dt-validate`,
`dt-extract-example`), which the kernel runs against its own in-tree bindings.

The core schemas bundled here are **always** merged in during processing:
`process_schemas()` unconditionally appends this package's `schemas/` directory
(`core_schema=True`), so the kernel's bindings are validated alongside them. Note
`dt-mk-schema`'s `-u/--useronly` flag is currently a **no-op** (declared in argparse but
never read), and the `-u` on `dt-validate`/`dt-doc-validate` is the unrelated,
deprecated `--url-path` — don't confuse the two.

## AI-assisted contribution rules

dt-schema feeds directly into the Linux kernel workflow, so contributions here follow
the kernel's expectations for AI-assisted work — see
<https://docs.kernel.org/process/coding-assistants.html>. In short:

- **A human is accountable.** The human contributor must review all AI-generated code,
  ensure it is correct and licensing-compliant, and take full responsibility for it. An
  AI assistant is a tool, not a submitter.
- **AI agents MUST NOT add `Signed-off-by` tags.** Only a human can certify the
  [Developer Certificate of Origin](https://developercertificate.org/); the human
  submitter adds their own `Signed-off-by`.
- **Disclose assistance with an `Assisted-by` trailer** in the commit message, using the
  format `Assisted-by: AGENT_NAME:MODEL_VERSION [TOOL1] [TOOL2]` — e.g.
  `Assisted-by: Claude:claude-opus-4 coccinelle sparse`. List specialized analysis tools
  (coccinelle, sparse, smatch, …) but **not** basic tooling (git, make, editors).
- **Licensing:** keep the `# SPDX-License-Identifier: BSD-2-Clause` header on new
  Python/YAML files and match the existing copyright style. (Note: dt-schema itself is
  BSD-2-Clause; the kernel tree it serves is GPL-2.0-only.)
- Otherwise follow the normal process in this file: pass the test suite, `flake8`
  (fatal set) and `yamllint --strict`, and keep changes minimal and in-style.

## Keeping this file current

Treat `AGENTS.md` as living documentation. When a change would make anything here
inaccurate, update it in the **same** commit — for example: adding/renaming/removing a
CLI entry point in `pyproject.toml`, changing a tool's flags or behavior, adding or
reorganizing library modules or `schemas/`/`meta-schemas/` directories, bumping the
supported Python range or dependencies, or altering the CI/lint/test commands. If you
notice this file has already drifted from the code, fix it as part of your change rather
than leaving it stale. `CLAUDE.md` is a symlink to this file, so there is only one place
to edit.
