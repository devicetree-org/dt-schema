# SPDX-License-Identifier: BSD-2-Clause
# Copyright 2018 Linaro Ltd.
# Copyright 2018-2023 Arm Ltd.
# Python library for Devicetree schema validation
import sys
import os
import re
import copy

from functools import cache

import jsonschema

from referencing import Registry
from referencing.exceptions import NoSuchResource, Unresolvable
from referencing.jsonschema import DRAFT201909

import dtschema

schema_base_url = "http://devicetree.org/"
schema_basedir = os.path.dirname(os.path.abspath(__file__))


def path_to_obj(tree, path):
    for pc in path:
        tree = tree[pc]
    return tree


def get_line_col(tree, path, obj=None):
    import ruamel.yaml

    if isinstance(obj, ruamel.yaml.comments.CommentedBase):
        return obj.lc.line, obj.lc.col
    obj = path_to_obj(tree, path)
    if isinstance(obj, ruamel.yaml.comments.CommentedBase):
        return obj.lc.line, obj.lc.col
    if len(path) < 1:
        return -1, -1
    obj = path_to_obj(tree, list(path)[:-1])
    if isinstance(obj, ruamel.yaml.comments.CommentedBase):
        if path[-1] == '$nodename':
            return -1, -1
        return obj.lc.key(path[-1])
    return -1, -1

def _is_node_schema(schema):
    return isinstance(schema, dict) and \
           (('type' in schema and schema['type'] == 'object') or
            schema.keys() & {'properties', 'patternProperties', 'additionalProperties', 'unevaluatedProperties'})


def _schema_allows_no_undefined_props(schema):
    if not _is_node_schema(schema):
        return True

    additional_props = schema.get("additionalProperties", True)
    uneval_props = schema.get("unevaluatedProperties", True)

    return not additional_props or isinstance(additional_props, dict) or \
           not uneval_props or isinstance(uneval_props, dict)


@cache
def create_meta_schema_registry():
    import glob
    import ruamel.yaml

    yaml = ruamel.yaml.YAML(typ='safe')
    yaml.allow_duplicate_keys = False

    metaschemas = []
    for filename in glob.iglob(schema_basedir + "/meta-schemas/*.yaml"):
        with open(filename, 'r', encoding='utf-8') as f:
            metaschema = yaml.load(f.read())
            metaschemas += [(
                metaschema['$id'],
                DRAFT201909.create_resource(metaschema)
            )]
    return Registry().with_resources(metaschemas)


class DTSchema(dict):
    DtValidator = jsonschema.validators.extend(
        jsonschema.Draft201909Validator,
        format_checker=jsonschema.FormatChecker(),
        version='DT')

    def __init__(self, schema_file, line_numbers=False):
        self.paths = [(schema_base_url, schema_basedir + '/')]
        with open(schema_file, 'r', encoding='utf-8') as f:
            import ruamel.yaml

            if line_numbers:
                rtyaml = ruamel.yaml.YAML(typ='rt')
                rtyaml.allow_duplicate_keys = False
                rtyaml.preserve_quotes = True

                schema = rtyaml.load(f.read())
            else:
                yaml = ruamel.yaml.YAML(typ='safe')
                yaml.allow_duplicate_keys = False
                schema = yaml.load(f.read())

        self.filename = os.path.abspath(schema_file)

        self.DtValidator.META_SCHEMA['properties']['select'] = {'$recursiveRef': '#'}


        super().__init__(schema)

    def retrieve(self, uri):
        '''Custom handler for http://devicetree.org references'''
        uri = uri.rstrip('#')
        missing_files = ''
        for p in self.paths:
            filename = uri.replace(p[0], p[1])
            if not os.path.isfile(filename):
                missing_files += f"\t{filename}\n"
                continue
            with open(filename, 'r', encoding='utf-8') as f:
                import ruamel.yaml
                yaml = ruamel.yaml.YAML(typ='safe')
                yaml.allow_duplicate_keys = False

                return DRAFT201909.create_resource(yaml.load(f.read()))

        raise NoSuchResource(ref=uri)

    def annotate_error(self, error, uri, path):
        error.note = None

        for e in error.context:
            self.annotate_error(e, uri, path + e.schema_path)

        registry = create_meta_schema_registry()
        schema = registry[uri].contents
        error.schema_file = uri

        for p in path:
            while p not in schema and '$ref' in schema and isinstance(schema['$ref'], str):
                ref = schema['$ref']
                if ref.startswith('#'):
                    schema = registry.resolver(uri).lookup(ref).contents
                else:
                    for rsrc in registry:
                        if ref.split('#')[0] in rsrc:
                            uri = rsrc
                            break
                    schema = registry.resolver(uri).lookup(ref).contents
                    error.schema_file = uri

            schema = schema[p]

            if isinstance(schema, dict):
                if 'description' in schema and isinstance(schema['description'], str):
                    error.note = schema['description']

        if isinstance(error.schema, dict) and 'description' in error.schema:
            error.note = error.schema['description']

    def iter_errors(self, strict=True):
        if strict:
            registry = create_meta_schema_registry()

            meta_schema_id = self['$schema'].rstrip('#')
            self.validator = self.DtValidator(registry.contents(meta_schema_id), registry=registry)

            for error in self.validator.iter_errors(self):
                scherr = jsonschema.exceptions.SchemaError.create_from(error)
                self.annotate_error(scherr, meta_schema_id, scherr.schema_path)
                scherr.linecol = get_line_col(self, scherr.path)
                yield scherr

        for error in self.DtValidator(self.DtValidator.META_SCHEMA).iter_errors(self):
            scherr = jsonschema.exceptions.SchemaError.create_from(error)
            scherr.linecol = get_line_col(self, scherr.path)
            yield scherr

    def is_valid(self, strict=False):
        ''' Check if schema passes validation against json-schema.org schema '''
        for error in self.iter_errors(strict):
            raise error

    def fixup(self):
        processed_schema = copy.deepcopy(dict(self))
        dtschema.fixups.fixup_schema(processed_schema)
        return processed_schema

    def _check_schema_refs(self, resolver, schema, parent=None, is_common=False, has_constraint=False):
        if not parent:
            is_common = not _schema_allows_no_undefined_props(schema)
        if isinstance(schema, dict):
            if parent in {'if', 'select', 'definitions', '$defs', 'then',
                          'else', 'dependencies', 'dependentSchemas'}:
                return

            has_only_type = 'type' in schema and \
                not (schema.keys() & {'properties', 'patternProperties', 'additionalProperties', 'unevaluatedProperties'})

            if _is_node_schema(schema) and not parent in {'oneOf', 'allOf', 'anyOf'}:
                has_constraint = _schema_allows_no_undefined_props(schema)
                ref_has_constraint = False
            else:
                ref_has_constraint = True

            if '$ref' in schema:
                ref_sch = resolver.lookup(schema['$ref']).contents
                ref_has_constraint = _schema_allows_no_undefined_props(ref_sch)

            if not (is_common or ref_has_constraint or has_constraint or has_only_type or
               (schema.keys() & {'additionalProperties', 'unevaluatedProperties'})):
                print(f"{self.filename}: {parent}: Missing additionalProperties/unevaluatedProperties constraint\n",
                      file=sys.stderr)

            for k, v in schema.items():
                self._check_schema_refs(resolver, v, parent=k, is_common=is_common,
                                        has_constraint=has_constraint)
        elif isinstance(schema, (list, tuple)):
            for i in range(len(schema)):
                self._check_schema_refs(resolver, schema[i], parent=parent, is_common=is_common,
                                        has_constraint=has_constraint)

    def check_schema_refs(self):
        id = self['$id'].rstrip('#')
        base1 = re.search('schemas/(.+)$', id)[1]
        base_path = self.filename[:-len(base1)]
        base2 = self.filename.replace(base_path, '')
        if not base1 == base2:
            print(f"{self.filename}: $id: Cannot determine base path from $id, relative path/filename doesn't match actual path or filename\n",
                  f"\t $id: {id}\n",
                  f"\tfile: {self.filename}",
                  file=sys.stderr)
            return

        self.paths = [
            (schema_base_url + 'schemas/', base_path),
            (schema_base_url + 'schemas/', schema_basedir + '/schemas/'),
        ]

        rsrc = DRAFT201909.create_resource(self)
        registry = Registry(retrieve=self.retrieve).with_resource(uri=id, resource=rsrc)
        resolver = registry.resolver_with_root(rsrc)

        try:
            self._check_schema_refs(resolver, self)
        except Unresolvable as exc:
            print(f"{self.filename}: Unresolvable reference: {exc}", file=sys.stderr)
