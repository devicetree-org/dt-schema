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

    def test_json_cli_output(self):
        dtc = shutil.which('dtc')
        if not dtc:
            self.skipTest("dtc not found")

        with tempfile.NamedTemporaryFile(suffix=".dtb") as f:
            res = subprocess.run([dtc, '-Odtb', '-o', f.name, 'test/device-fail.dts'],
                                 capture_output=True)
            self.assertEqual(res.returncode, 0, msg='dtc failed:\n' + res.stderr.decode())

            res = subprocess.run([
                sys.executable, '-c',
                'import dtschema.dtb_validate as d; d.main()',
                '--format', 'json', '-s', os.path.abspath('test/schemas'), f.name],
                capture_output=True, text=True)

        self.assertEqual(res.returncode, 0, msg=res.stderr)
        diagnostics = json.loads(res.stdout)
        self.assertGreater(len(diagnostics), 0)
        self.assertEqual(diagnostics[0]["type"], "validation")
        self.assertEqual(diagnostics[0]["level"], "error")
        self.assertIn("message", diagnostics[0])
        self.assertIn("formatted", diagnostics[0])
        self.assertIn("schema", diagnostics[0])

    def test_cli_cache_output(self):
        dtc = shutil.which('dtc')
        if not dtc:
            self.skipTest("dtc not found")

        with tempfile.NamedTemporaryFile(suffix=".dtb") as f, \
             tempfile.NamedTemporaryFile(suffix=".dtb") as f2, \
             tempfile.NamedTemporaryFile(suffix=".json") as schema, \
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
                '--format', 'json', '--cache-dir', cache_dir,
                '-s', schema.name, f.name]
            res = subprocess.run(cmd, capture_output=True, text=True)
            self.assertEqual(res.returncode, 0, msg=res.stderr)
            first = json.loads(res.stdout)
            self.assertEqual(first[0]["file"], f.name)

            res = subprocess.run(cmd, capture_output=True, text=True)
            self.assertEqual(res.returncode, 0, msg=res.stderr)
            self.assertEqual(json.loads(res.stdout), first)
            self.assertEqual(len(os.listdir(cache_dir)), 1)

            cmd[-1] = f2.name
            res = subprocess.run(cmd, capture_output=True, text=True)
            self.assertEqual(res.returncode, 0, msg=res.stderr)
            second = json.loads(res.stdout)
            self.assertEqual(second[0]["file"], f2.name)
            self.assertTrue(second[0]["formatted"].startswith(f2.name + ":"))
            self.assertEqual(len(os.listdir(cache_dir)), 1)

if __name__ == '__main__':
    unittest.main()
