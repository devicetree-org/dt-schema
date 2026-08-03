#!/usr/bin/env python3
#
# Testcases for the Devicetree schema files and validation library
#
# Copyright 2018 Arm Ltd.
#
# SPDX-License-Identifier: BSD-2-Clause
#
# Testcases are executed by running 'make test' from the top level directory of this repo.

import unittest
import os
import copy
import glob
import json
import shutil
import sys
import subprocess
import tempfile
from collections import deque

basedir = os.path.dirname(__file__)
import jsonschema
import ruamel.yaml
import dtschema
import dtschema.dtb_validate

dtschema_dir = os.path.dirname(dtschema.__file__)

yaml = ruamel.yaml.YAML(typ='safe')

def load(filename):
    with open(filename, 'r', encoding='utf-8') as f:
        return yaml.load(f.read())

class TestDTMetaSchema(unittest.TestCase):
    def setUp(self):
        self.schema = dtschema.DTSchema(os.path.join(basedir, 'schemas/good-example.yaml'))
        self.bad_schema = dtschema.DTSchema(os.path.join(basedir, 'schemas/bad-example.yaml'))

    def test_all_metaschema_valid(self):
        '''The metaschema must all be a valid Draft2019-09 schema'''
        for filename in glob.iglob(os.path.join(dtschema_dir, 'meta-schemas/**/*.yaml'), recursive=True):
            with self.subTest(schema=filename):
                schema = load(filename)
                jsonschema.Draft201909Validator.check_schema(schema)

    def test_required_properties(self):
        self.schema.is_valid(strict=True)

    def test_required_property_missing(self):
        for key in self.schema.keys():
            if key in ['$schema', 'properties', 'required', 'description', 'examples', 'additionalProperties']:
                continue
            with self.subTest(k=key):
                schema_tmp = copy.deepcopy(self.schema)
                del schema_tmp[key]
                self.assertRaises(jsonschema.SchemaError, schema_tmp.is_valid, strict=True)

    def test_bad_schema(self):
        '''bad-example.yaml is all bad. There is no condition where it should pass validation'''
        self.assertRaises(jsonschema.SchemaError, self.bad_schema.is_valid, strict=True)

    def test_bad_properties(self):
        for key in self.bad_schema.keys():
            if key in ['$schema', 'properties']:
                continue

            with self.subTest(k=key):
                schema_tmp = copy.deepcopy(self.schema)
                schema_tmp[key] = self.bad_schema[key]
                self.assertRaises(jsonschema.SchemaError, schema_tmp.is_valid, strict=True)

        bad_props = self.bad_schema['properties']
        schema_tmp = copy.deepcopy(self.schema)
        for key in bad_props.keys():
            with self.subTest(k="properties/"+key):
                schema_tmp['properties'] = self.schema['properties'].copy()
                schema_tmp['properties'][key] = bad_props[key]
                self.assertRaises(jsonschema.SchemaError, schema_tmp.is_valid, strict=True)

class TestDTSchema(unittest.TestCase):
    def test_binding_schemas_valid(self):
        '''Test that all schema files under ./dtschema/schemas/ validate against the DT metaschema'''
        for filename in glob.iglob(os.path.join(dtschema_dir, 'schemas/**/*.yaml'), recursive=True):
            with self.subTest(schema=filename):
                dtschema.DTSchema(filename).is_valid(strict=True)

    def test_binding_schemas_refs(self):
        '''Test that all schema files under ./dtschema/schemas/ have valid references'''
        for filename in glob.iglob(os.path.join(dtschema_dir, 'schemas/**/*.yaml'), recursive=True):
            with self.subTest(schema=filename):
                dtschema.DTSchema(filename).check_schema_refs()

    def test_binding_schemas_id_is_unique(self):
        '''Test that all schema files under ./dtschema/schemas/ validate against the DT metaschema'''
        ids = []
        for filename in glob.iglob(os.path.join(dtschema_dir, 'schemas/**/*.yaml'), recursive=True):
            with self.subTest(schema=filename):
                schema = load(filename)
                self.assertEqual(ids.count(schema['$id']), 0)
                ids.append(schema['$id'])

    def test_binding_schemas_valid_draft201909(self):
        '''Test that all schema files under ./dtschema/schemas/ validate against the Draft7 metaschema
        The DT Metaschema is supposed to force all schemas to be valid against
        Draft7. This test makes absolutely sure that they are.
        '''
        for filename in glob.iglob(os.path.join(dtschema_dir, 'schemas/**/*.yaml'), recursive=True):
            with self.subTest(schema=filename):
                schema = load(filename)
                jsonschema.Draft201909Validator.check_schema(schema)


class TestDTValidate(unittest.TestCase):
    def setUp(self):
        self.validator = dtschema.DTValidator([ os.path.join(os.path.abspath(basedir), "schemas/")])

    def check_node(self, nodename, node):
        if nodename == "/" or nodename.startswith('__'):
            return

        node['$nodename'] = [ nodename ]
        self.validator.validate(node)

    def check_subtree(self, nodename, subtree):
        self.check_node(nodename, subtree)
        for name,value in subtree.items():
            if isinstance(value, dict):
                self.check_subtree(name, value)

    def test_dtb_validation(self):
        '''Test that all DT files under ./test/ validate against the DT schema (DTB)'''
        for filename in glob.iglob('test/*.dts'):
            with self.subTest(schema=filename):
                expect_fail = "-fail" in filename
                res = subprocess.run(['dtc', '-Odtb', filename], capture_output=True)
                testtree = self.validator.decode_dtb(res.stdout)
                self.assertEqual(res.returncode, 0, msg='dtc failed:\n' + res.stderr.decode())

                if expect_fail:
                    with self.assertRaises(jsonschema.ValidationError):
                        self.check_subtree('/', testtree[0])
                else:
                    self.assertIsNone(self.check_subtree('/', testtree[0]))

    def test_validator_cache(self):
        node = {
            '$nodename': ['test'],
            'compatible': ['vendor,soc1-ip'],
            'vendor,int-prop': [4],
        }
        errors = [error.message for error in self.validator.iter_errors(node)]
        schema_id = self.validator.compat_map['vendor,soc1-ip']
        schema_validator = self.validator._schema_validators[schema_id]
        select_validators = self.validator._select_validators.copy()

        self.assertTrue(errors)
        self.assertEqual(errors, [error.message for error in self.validator.iter_errors(node)])
        self.assertIs(self.validator._schema_validators[schema_id], schema_validator)
        self.assertEqual(self.validator._select_validators.keys(), select_validators.keys())
        for select_id, validator in select_validators.items():
            self.assertIs(self.validator._select_validators[select_id], validator)

    def test_json_error_diagnostic(self):
        error = jsonschema.ValidationError(
            "'foo' is a required property",
            path=deque(["soc", "device@0"]),
            schema_path=deque(["then", "required"]))
        error.linecol = (4, 8)
        error.schema_file = "http://devicetree.org/schemas/test.yaml#"
        error.note = "missing required property"

        diagnostic = dtschema.dtb_validate._error_diagnostic(
            "test.dtb", error, nodename="device@0", fullname="/soc/device@0",
            compatible="test,device")

        self.assertEqual(diagnostic["type"], "validation")
        self.assertEqual(diagnostic["level"], "error")
        self.assertEqual(diagnostic["file"], "test.dtb")
        self.assertEqual(diagnostic["line"], 5)
        self.assertEqual(diagnostic["column"], 9)
        self.assertEqual(diagnostic["node"], "/soc/device@0")
        self.assertEqual(diagnostic["nodename"], "device@0")
        self.assertEqual(diagnostic["compatible"], "test,device")
        self.assertEqual(diagnostic["property_path"], ["soc", "device@0"])
        self.assertEqual(diagnostic["schema_path"], ["then", "required"])
        self.assertEqual(diagnostic["schema"], "http://devicetree.org/schemas/test.yaml#")
        self.assertEqual(diagnostic["message"], "'foo' is a required property")
        self.assertEqual(diagnostic["note"], "missing required property")

    def test_json_unmatched_diagnostic(self):
        diagnostic = dtschema.dtb_validate._unmatched_diagnostic(
            "test.dtb", "/soc/device@0", {"compatible": ["test,device"]})

        self.assertEqual(diagnostic["type"], "unmatched")
        self.assertEqual(diagnostic["level"], "warning")
        self.assertEqual(diagnostic["file"], "test.dtb")
        self.assertEqual(diagnostic["node"], "/soc/device@0")
        self.assertEqual(diagnostic["compatible"], ["test,device"])
        self.assertIn("failed to match any schema", diagnostic["message"])

        diagnostic = dtschema.dtb_validate._unmatched_diagnostic(
            "test.dtb", "/", {"compatible": ["test,board"]})
        self.assertEqual(diagnostic["nodename"], "/")

    def test_format_error_rewrites_indented_paths(self):
        filename = "test.dtb"
        abs_filename = os.path.abspath(filename)

        leaf = jsonschema.ValidationError(
            "leaf problem",
            path=deque(["leaf"]),
            schema_path=deque(["type"]))
        leaf.linecol = (2, 0)
        leaf.schema_file = "http://devicetree.org/schemas/test.yaml#"

        inner = jsonschema.ValidationError(
            "inner problem",
            path=deque(["inner"]),
            schema_path=deque(["anyOf"]))
        inner.linecol = (1, 0)
        inner.schema_file = "http://devicetree.org/schemas/test.yaml#"
        inner.context = [leaf]

        other = jsonschema.ValidationError(
            "other problem",
            path=deque(["other"]),
            schema_path=deque(["type"]))
        other.linecol = (3, 0)
        other.schema_file = "http://devicetree.org/schemas/test.yaml#"

        error = jsonschema.ValidationError(
            "outer problem",
            path=deque(["root"]),
            schema_path=deque(["then"]))
        error.linecol = (0, 0)
        error.schema_file = "http://devicetree.org/schemas/test.yaml#"
        error.context = [inner, other]

        text = dtschema.dtb_validate._format_error(filename, error, nodename="node")

        self.assertNotIn(abs_filename, text)
        self.assertIn("test.dtb:1:1", text)
        self.assertIn("\ttest.dtb:2:1", text)

    def test_json_cli_output_file(self):
        dtc = shutil.which('dtc')
        if not dtc:
            self.skipTest("dtc not found")

        with tempfile.NamedTemporaryFile(suffix=".dtb") as dtb, \
             tempfile.NamedTemporaryFile(suffix=".json") as json_output:
            res = subprocess.run([dtc, '-Odtb', '-o', dtb.name, 'test/device-fail.dts'],
                                 capture_output=True)
            self.assertEqual(res.returncode, 0, msg='dtc failed:\n' + res.stderr.decode())

            res = subprocess.run([
                sys.executable, '-c',
                'import dtschema.dtb_validate as d; d.main()',
                '--json-output', json_output.name,
                '-s', os.path.abspath('test/schemas'), dtb.name],
                capture_output=True, text=True)

            self.assertEqual(res.returncode, 0, msg=res.stderr)
            self.assertEqual(res.stdout, "")
            self.assertIn("from schema $id:", res.stderr)
            json_output.seek(0)
            diagnostics = json.load(json_output)

        self.assertGreater(len(diagnostics), 0)
        validation = next(d for d in diagnostics if d["type"] == "validation")
        self.assertEqual(validation["level"], "error")
        self.assertIn("message", validation)
        self.assertIn("formatted", validation)
        self.assertIn("schema", validation)

    def test_cli_cache_output(self):
        dtc = shutil.which('dtc')
        if not dtc:
            self.skipTest("dtc not found")

        with tempfile.NamedTemporaryFile(suffix=".dtb") as f, \
             tempfile.NamedTemporaryFile(suffix=".dtb") as f2, \
             tempfile.NamedTemporaryFile(suffix=".json") as schema, \
             tempfile.NamedTemporaryFile(suffix=".json") as json_output, \
             tempfile.TemporaryDirectory() as cache_dir:
            res = subprocess.run([dtc, '-Odtb', '-o', f.name, 'test/device-fail.dts'],
                                 capture_output=True)
            self.assertEqual(res.returncode, 0, msg='dtc failed:\n' + res.stderr.decode())
            shutil.copyfile(f.name, f2.name)

            res = subprocess.run([
                sys.executable, '-c',
                'import dtschema.mk_schema as m; m.main()',
                '-j', '-o', schema.name, os.path.abspath('test/schemas')],
                capture_output=True, text=True)
            self.assertEqual(res.returncode, 0, msg=res.stderr)

            cmd = [
                sys.executable, '-c',
                'import dtschema.dtb_validate as d; d.main()',
                '--json-output', json_output.name, '--cache-dir', cache_dir,
                '-s', schema.name, f.name]
            res = subprocess.run(cmd, capture_output=True, text=True)
            self.assertEqual(res.returncode, 0, msg=res.stderr)
            self.assertEqual(res.stdout, "")
            self.assertIn("from schema $id:", res.stderr)
            self.assertIn("vendor,bool-prop: size (5) error for type flag", res.stderr)
            json_output.seek(0)
            first = json.load(json_output)
            decode = next(d for d in first if d["type"] == "decode" and
                          d["message"] == "vendor,bool-prop: size (5) error for type flag")
            self.assertEqual(decode["file"], f.name)

            res = subprocess.run(cmd, capture_output=True, text=True)
            self.assertEqual(res.returncode, 0, msg=res.stderr)
            self.assertEqual(res.stdout, "")
            self.assertIn("from schema $id:", res.stderr)
            self.assertIn("vendor,bool-prop: size (5) error for type flag", res.stderr)
            json_output.seek(0)
            self.assertEqual(json.load(json_output), first)
            self.assertEqual(len(os.listdir(cache_dir)), 1)

            cmd[-1] = f2.name
            res = subprocess.run(cmd, capture_output=True, text=True)
            self.assertEqual(res.returncode, 0, msg=res.stderr)
            self.assertEqual(res.stdout, "")
            self.assertIn("from schema $id:", res.stderr)
            json_output.seek(0)
            second = json.load(json_output)
            validation = next(d for d in second if d["type"] == "validation")
            self.assertEqual(validation["file"], f2.name)
            self.assertTrue(validation["formatted"].startswith(f2.name + ":"))
            self.assertEqual(len(os.listdir(cache_dir)), 1)

if __name__ == '__main__':
    unittest.main()
