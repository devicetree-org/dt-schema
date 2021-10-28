# Tooling for devicetree validation using YAML and jsonschema

This repository contains test code for devicetree schema validation using the
[json-schema](http://json-schema.org/documentation.html) vocabulary. Schema
files are written in YAML (a superset of JSON), and operate on the YAML
encoding of Devicetree data. Devicetree data must be transcoded from DTS to
YAML before being used by this tool.

## Data Model

To understand how validation works, it is important to understand how schema data is organized and used.
If you're reading this, I assume you're already familiar with Devicetree and the .dts file format.

In this repository you will find three kinds of files; *YAML Devicetrees*, *Schemas* and *Meta-Schemas*.

### *YAML Devicetrees*

Found under `./test`

*YAML Devicetrees* files are regular .dts files transcoded into a YAML
representation.
There is no special information in these files.
They are used as test cases against the validation tooling.

### *Devicetree Schemas*

Found under `./schemas`

*Devicetree Schemas* describe the format of devicetree data.
The raw Devicetree file format is very open ended and doesn't restrict how
data is encoded.
Hence, it is easy to make mistakes when writing a Devicetree.
Schema files impose constraints on what data can be put into a devicetree.
As the foundation, a single core schema describes all the common property types
that every devicetree node must match.
e.g. In every node the 'compatible' property must be an array of strings.
However, most devicetree data is heterogeneous as each device binding requires
a different set of data, therefore multiple schema files are used to capture the
data format of an entire devicetree.

When validating, the tool will load all the schema files it can find and then
iterate over all the nodes of the devicetree.
For each node, the tool will determine which schema(s) are applicable and make sure
the node data matches the schema constraints.
Nodes failing a schema test will emit an error.
Nodes that don't match any schema will emit a warning.

As a developer, you would write a devicetree schema file for each new
device binding that you create and add it to the `./schemas` directory.

Schema files also have the dual purpose of documenting a binding.
When you define a new binding, you only have to create one file that contains
both the machine-verifiable data format and the documentation.
Documentation generation tools are being written to extract documentation
from a schema file and emit a format that can be included in the devicetree
specification documents.

Devicetree Schema files are normal YAML files using the jsonschema vocabulary.

The Devicetree Schema files are simplified to make them more compact.

The default for arrays in json-schema is they are variable sized. This can be
restricted by defining 'minItems', 'maxItems', and 'additionalItems'. For
DeviceTree Schemas, a fixed size is desired in most cases, so these properties
are added based on the size of 'items' list.

The YAML DeviceTree format also makes all string values an array and scalar
values a matrix (in order to define groupings) even when only a single value
is present. Single entries in schemas are fixed up to match this encoding.

### *Devicetree Meta-Schemas*

Found in `./meta-schemas`

*Devicetree Meta-Schemas* describe the data format of Devicetree Schema files.
The Meta-schemas make sure all the binding schemas are in the correct format
and the tool will emit an error if the format is incorrect.

As a developer you normally will not need to write metaschema files.

Devicetree Meta-Schema files are normal YAML files using the jsonschema vocabulary.

## Usage
There are several tools available in the *tools/* directory.

`tools/dt-doc-validate`
This tool takes a schema file(s) or directory of schema files and validates
them against the DT meta-schema.

`tools/dt-mk-schema`
This tool takes user-provided schema file(s) plus the core schema files in this
repo, removes everything not needed for validation, applies fix-ups to the
schemas, and outputs a single file with the processed schema. This step is
done separately to speed up subsequent validation of YAML Devicetrees.

`tools/dt-validate`
This tool takes user-provided YAML Devicetree(s) and either a schema directory
or pre-processed schema file and validates the YAML Devicetree against the
schema.


## Installing
The project and its dependencies can be installed with pip directly from git:

```
pip3 install git+https://github.com/devicetree-org/dt-schema.git@master
```

All executables will be installed. Ensure ~/.local/bin is in the PATH.


For development, clone the git repository manually and run pip on local tree::

```
git clone https://github.com/devicetree-org/dt-schema.git
cd dt-schema
pip3 install -e .
```

## Dependencies
Note: The above installation instructions handle all of the dependencies
automatically.

This code depends on Python 3 with the ruamel.yaml, rfc3987, and jsonschema
libraries.

On Debian/Ubuntu, the dependencies can be installed with apt and/or pip. The
rfc3987 module is not packaged, so pip must be used:

```
apt-get install python3 python3-ruamel.yaml
pip3 install rfc3987
```


### jsonschema
This code depends on at least version 3.0.0 of the
[Python jsonschema](https://github.com/Julian/jsonschema/tree/master)
library for Draft 6 support.

The module can be installed directly from github with pip:

```
pip3 install git+https://github.com/Julian/jsonschema.git
```
