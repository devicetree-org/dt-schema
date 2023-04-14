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
import sys
import subprocess
import tempfile

basedir = os.path.dirname(__file__)
import jsonschema
import ruamel.yaml
import dtschema

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



if __name__ == '__main__':
    unittest.main()
