# SPDX-License-Identifier: BSD-2-Clause
# Copyright 2018 Linaro Ltd.
# Copyright 2018 Arm Ltd.
# Python library for Devicetree schema validation
import sys
import os
import glob
import ruamel.yaml
import re
import pprint
import copy

from ruamel.yaml.comments import CommentedMap

import jsonschema
import pkgutil

schema_base_url = "http://devicetree.org/"
schema_basedir = os.path.dirname(os.path.abspath(__file__))

# We use a lot of regex's in schema and exceeding the cache size has noticeable
# peformance impact.
re._MAXCACHE = 2048

class tagged_list(list):

    tags = {u'!u8': 8, u'!u16': 16, u'!u32': 32, u'!u64': 64}

    def __init__(self, int_list, tag, tags=tags):
        super().__init__(int_list)
        self.type_size = tags[tag]

    @staticmethod
    def constructor(loader, node):
        return tagged_list(loader.construct_sequence(node), node.tag)

class phandle_int(int):

    def __new__(cls, value):
        return int.__new__(cls, value)

    @staticmethod
    def constructor(loader, node):
        return phandle_int(loader.construct_yaml_int(node))

rtyaml = ruamel.yaml.YAML(typ='rt')
rtyaml.allow_duplicate_keys = True
rtyaml.Constructor.add_constructor(u'!u8', tagged_list.constructor)
rtyaml.Constructor.add_constructor(u'!u16', tagged_list.constructor)
rtyaml.Constructor.add_constructor(u'!u32', tagged_list.constructor)
rtyaml.Constructor.add_constructor(u'!u64', tagged_list.constructor)
rtyaml.Constructor.add_constructor(u'!phandle', phandle_int.constructor)

yaml = ruamel.yaml.YAML(typ='safe')
yaml.allow_duplicate_keys = True
yaml.Constructor.add_constructor(u'!u8', tagged_list.constructor)
yaml.Constructor.add_constructor(u'!u16', tagged_list.constructor)
yaml.Constructor.add_constructor(u'!u32', tagged_list.constructor)
yaml.Constructor.add_constructor(u'!u64', tagged_list.constructor)
yaml.Constructor.add_constructor(u'!phandle', phandle_int.constructor)

def path_to_obj(tree, path):
    for pc in path:
        tree = tree[pc]
    return tree

def get_line_col(tree, path, obj=None):
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

schema_user_paths = []

def add_schema_path(path):
    if os.path.isdir(path):
        schema_user_paths.append(path)

def load_schema(schema):
    yaml.allow_duplicate_keys = False
    for path in schema_user_paths:
        if schema.startswith('schemas/'):
            schema_file = schema.partition('/')[2]
        else:
            schema_file = schema
        schema_file = os.path.join(path, schema_file)
        if not os.path.isfile(schema_file):
            continue
        with open(schema_file, 'r', encoding='utf-8') as f:
            return yaml.load(f.read())

    with open(os.path.join(schema_basedir, schema), 'r', encoding='utf-8') as f:
        return yaml.load(f.read())

def _value_is_type(subschema, key, type):
    if not ( isinstance(subschema, dict) and key in subschema.keys() ):
        return False

    if isinstance(subschema[key], list):
        val = subschema[key][0]
    else:
        val = subschema[key]

    return isinstance(val, type)


def _fixup_string_to_array(subschema):
    tmpsch = {}

    # nothing to do if we already have an array
    if 'items' in subschema.keys():
        return

    for match in ['const', 'enum', 'pattern']:
        if not _value_is_type(subschema, match, str):
            continue

        tmpsch[match] = subschema[match]
        subschema.pop(match, None)

    if tmpsch != {}:
        subschema['items'] = tmpsch

def _fixup_int_array_to_matrix(subschema):
    is_int = False

    if not 'items' in subschema.keys():
        return

    if (not isinstance(subschema['items'],dict)) or 'items' in subschema['items'].keys():
        return

    for match in ['const', 'enum', 'minimum', 'maximum']:
        if not _value_is_type(subschema['items'], match, int):
            continue

        is_int = True

    if not is_int:
        return

    subschema['items'] = copy.deepcopy(subschema)
    for k in list(subschema.keys()):
        if k == 'items':
            continue
        subschema.pop(k)


def _fixup_scalar_to_array(subschema):
    tmpsch = {}

    if 'items' in subschema.keys():
        return

    for match in ['const', 'enum', 'minimum', 'maximum']:
        if not _value_is_type(subschema, match, int):
            continue

        tmpsch[match] = subschema[match]
        subschema.pop(match, None)

    if tmpsch != {}:
        subschema['items'] = { 'items': tmpsch }

def _fixup_int_array(subschema):

    if not 'items' in subschema.keys():
        return

    # A string list or already a matrix?
    for l in subschema['items']:
        if isinstance(l, dict) and 'items' in l.keys():
            return
        for match in ['const', 'enum', 'minimum', 'maximum']:
            if _value_is_type(l, match, int):
                break
        else:
            return

    subschema['items'] = [ {'items': subschema['items']} ]

def _fixup_items_size(schema):
    # Make items list fixed size-spec
    if isinstance(schema, list):
        for l in schema:
            _fixup_items_size(l)
    elif isinstance(schema, dict):
        if 'items' in schema.keys():
            schema['type'] = 'array'

            if isinstance(schema['items'], list):
                c = len(schema['items'])
                if not 'minItems' in schema.keys():
                    schema['minItems'] = c
                if not 'maxItems' in schema.keys():
                    schema['maxItems'] = c

                if not 'additionalItems' in schema.keys():
                    schema['additionalItems'] = False
        elif 'maxItems' in schema.keys() and not 'minItems' in schema.keys():
            schema['minItems'] = schema['maxItems']
        elif 'minItems' in schema.keys() and not 'maxItems' in schema.keys():
            schema['maxItems'] = schema['minItems']

        for prop,val in schema.items():
            _fixup_items_size(val)

def fixup_vals(schema):
    # Now we should be a the schema level to do actual fixups
#    print(schema)
    _fixup_int_array_to_matrix(schema)
    _fixup_int_array(schema)
    _fixup_string_to_array(schema)
    _fixup_scalar_to_array(schema)
    _fixup_items_size(schema)
#    print(schema)

def walk_conditionals(schema):
    # Recurse until we don't hit a conditional
    # Note we expect to encounter conditionals first.
    # For example, a conditional below 'items' is not supported
    for cond in ['allOf', 'oneOf', 'anyOf']:
        if cond in schema.keys():
            for l in schema[cond]:
                walk_conditionals(l)
    else:
        fixup_vals(schema)

def walk_properties(props):
    for prop in props:
        if not isinstance(props[prop], dict):
            continue

        walk_conditionals(props[prop])

def fixup_schema(schema):
    if not isinstance(schema, dict):
        return

    for k,v in schema.items():
        # select is a subschema that we want to fixup
        if k in ['select']:
            fixup_schema(v)

        # If, then and else contain subschemas that we'll want to
        # fixup as well. Let's recurse into those subschemas.
        if k in ['if', 'then', 'else']:
            fixup_schema(v)

        # allOf can contain a list of if, then and else statements,
        # that in turn will contain subschemas that we'll want to
        # fixup. Let's recurse into each of those subschemas.
        if k in ['allOf', 'anyOf', 'oneOf']:
            for subschema in v:
                fixup_schema(subschema)

        # properties within dependencies can be a schema
        if k in ['dependencies']:
            for prop in v:
                fixup_schema(v[prop])

        if not k in ['properties', 'patternProperties']:
            continue

        walk_properties(v)
        for prop in v:
            # Recurse to check for {properties,patternProperties} in each prop
            fixup_schema(v[prop])

def item_generator(json_input, lookup_key):
    if isinstance(json_input, dict):
        for k, v in json_input.items():
            if k == lookup_key:
                if isinstance(v, str):
                    yield [v]
                else:
                    yield v
            else:
                for child_val in item_generator(v, lookup_key):
                    yield child_val
    elif isinstance(json_input, list):
        for item in json_input:
            for item_val in item_generator(item, lookup_key):
                yield item_val

# Convert to standard types from ruamel's CommentedMap/Seq
def convert_to_dict(schema):
    if isinstance(schema, dict):
        result = {}
        for k, v in schema.items():
            result[k] = convert_to_dict(v)
    elif isinstance(schema, list):
        result = []
        for item in schema:
            result.append(convert_to_dict(item))
    else:
        result = schema

    return result

def add_select_schema(schema):
    '''Get a schema to be used in select tests.

    If the provided schema has a 'select' property, then use that as the select schema.
    If it has a compatible property, then create a select schema from that.
    If it has a $nodename property, then create a select schema from that.
    If it has none of those, then return a match-nothing schema
    '''
    if "select" in schema.keys():
        return

    if 'compatible' in schema['properties'].keys():
        sch = schema['properties']['compatible']
        compatible_list = [ ]
        for l in item_generator(sch, 'enum'):
            compatible_list.extend(l)

        for l in item_generator(sch, 'const'):
            compatible_list.extend(l)

        if 'contains' in sch.keys():
            for l in item_generator(sch['contains'], 'enum'):
                compatible_list.extend(l)

            for l in item_generator(sch['contains'], 'const'):
                compatible_list.extend(l)

        compatible_list = list(set(compatible_list))

        if len(compatible_list) != 0:
            schema['select'] = {
                'required': ['compatible'],
                'properties': {'compatible': {'contains': {'enum': compatible_list}}}}

            return

    if '$nodename' in schema['properties'].keys():
        schema['select'] = {
            'required': ['$nodename'],
            'properties': {'$nodename': convert_to_dict(schema['properties']['$nodename']) }}

        return

    schema['select'] = False

def remove_description(schema):
    if isinstance(schema, list):
        for s in schema:
            remove_description(s)
    if isinstance(schema, dict):
        schema.pop('description', None)
        for k,v in schema.items():
            remove_description(v)

def fixup_interrupts(schema):
    # Supporting 'interrupts' implies 'interrupts-extended' is also supported.
    if not 'interrupts' in schema['properties'].keys():
        return

    # Any node with 'interrupts' can have 'interrupt-parent'
    schema['properties']['interrupt-parent'] = True

    schema['properties']['interrupts-extended'] = { "$ref": "#/properties/interrupts" };

    if not ('required' in schema.keys() and 'interrupts' in schema['required']):
        return

    # Currently no better way to express either 'interrupts' or 'interrupts-extended'
    # is required. If this fails validation, the error reporting is the whole
    # schema file fails validation
    schema['oneOf'] = [ {'required': ['interrupts']}, {'required': ['interrupts-extended']} ]
    schema['required'].remove('interrupts')

def fixup_node_props(schema):
    if not ('properties' in schema.keys() or 'patternProperties' in schema.keys()):
        return

    if 'properties' in schema.keys():
        for k,v in schema['properties'].items():
            if isinstance(v, dict) and 'type' in v.keys() and v['type'] == 'object':
                fixup_node_props(v)

    if 'patternProperties' in schema.keys():
        for k,v in schema['patternProperties'].items():
            if isinstance(v, dict) and 'type' in v.keys() and v['type'] == 'object':
                fixup_node_props(v)

    if not 'properties' in schema.keys():
        schema['properties'] = {}

    schema['properties']['phandle'] = True
    schema['properties']['status'] = True

    keys = list()
    if 'properties' in schema:
        keys.extend(schema['properties'])

    if 'patternProperties' in schema:
        keys.extend(schema['patternProperties'])

    for key in keys:
        if "pinctrl" in key:
            break

    else:
        schema['properties']['pinctrl-names'] = True
        schema.setdefault('patternProperties', dict())
        schema['patternProperties']['pinctrl-[0-9]+'] = True

def process_schema(filename):
    try:
        schema = load_schema(filename)
    except ruamel.yaml.error.YAMLError as exc:
        print(filename + ": ignoring, error parsing file")
        return

    # Check that the validation schema is valid
    try:
        DTValidator.check_schema(schema)
    except jsonschema.SchemaError as exc:
        print(filename + ": ignoring, error in schema '%s'" % exc.path[-1])
        #print(exc.message)
        return

    # Remove parts not necessary for validation
    schema.pop('examples', None)
    schema.pop('maintainers', None)
    schema.pop('historical', None)

    remove_description(schema)

    if not ('properties' in schema.keys() or 'patternProperties' in schema.keys()):
        return schema

    if not 'properties' in schema.keys():
        schema['properties'] = {}

    # Add any implicit properties
    fixup_node_props(schema)

    fixup_interrupts(schema)

    add_select_schema(schema)
    if not 'select' in schema.keys():
        return

    schema["$filename"] = filename
    return schema

def process_schemas(schema_paths, core_schema=True):
    ids = []
    schemas = []

    for filename in schema_paths:
        if not os.path.isfile(filename):
            continue
        sch = process_schema(os.path.abspath(filename))
        if sch:
            schemas.append(sch)
            if ids.count(sch['$id']):
                print(os.path.abspath(filename) + ": duplicate '$id' value '" + sch['$id'] + "'", file=sys.stderr)
            ids.append(sch['$id'])
        else:
            print("warning: no schema found in file: %s" % filename, file=sys.stderr)

    if core_schema:
        schema_paths.append(os.path.join(schema_basedir, 'schemas/'))

    for path in schema_paths:
        count = 0
        if not os.path.isdir(path):
            continue

        for filename in glob.iglob(os.path.join(os.path.abspath(path), "**/*.yaml"), recursive=True):
            sch = process_schema(os.path.abspath(filename))
            if sch:
                count += 1
                schemas.append(sch)
                if ids.count(sch['$id']):
                    print(os.path.abspath(filename) + ": duplicate '$id' value '" + sch['$id'] + "'", file=sys.stderr)
                ids.append(sch['$id'])

        if count == 0:
            print("warning: no schema found in path: %s" % path, file=sys.stderr)

    return schemas

def load(filename, line_number=False, duplicate_keys=True):
    with open(filename, 'r', encoding='utf-8') as f:
        if line_number:
            rtyaml.allow_duplicate_keys = duplicate_keys
            return rtyaml.load(f.read())
        else:
            yaml.allow_duplicate_keys = duplicate_keys
            return yaml.load(f.read())

schema_cache = []

def set_schema(schemas):
    global schema_cache
    schema_cache = schemas

def http_handler(uri):
    global schema_cache
    '''Custom handler for http://devicetre.org YAML references'''
    if schema_base_url in uri:
        for sch in schema_cache:
            if uri in sch['$id']:
                return sch
        if 'meta-schemas' in uri:
            return load_schema(uri.replace(schema_base_url, ''))
        return process_schema(uri.replace(schema_base_url, ''))

    return yaml.load(jsonschema.compat.urlopen(uri).read().decode('utf-8'))

handlers = {"http": http_handler}

def typeSize(validator, typeSize, instance, schema):
    if (isinstance(instance[0], tagged_list)):
        if typeSize != instance[0].type_size:
            yield jsonschema.ValidationError("size is %r, expected %r" % (instance[0].type_size, typeSize))
    else:
        yield jsonschema.ValidationError("missing size tag in %r" % instance)

def phandle(validator, phandle, instance, schema):
    if not isinstance(instance, phandle_int):
        yield jsonschema.ValidationError("missing phandle tag in %r" % instance)

DTVal = jsonschema.validators.extend(jsonschema.Draft7Validator, {'typeSize': typeSize, 'phandle': phandle})

class DTValidator(DTVal):
    '''Custom Validator for Devicetree Schemas

    Overrides the Draft7 metaschema with the devicetree metaschema. This
    validator is used in exactly the same way as the Draft7Validator. Schema
    files can be validated with the .check_schema() method, and .validate()
    will check the data in a devicetree file.
    '''
    resolver = jsonschema.RefResolver('', None, handlers=handlers)
    format_checker = jsonschema.FormatChecker()

    def __init__(self, schema, types=()):
        jsonschema.Draft7Validator.__init__(self, schema, types, resolver=self.resolver,
                                            format_checker=self.format_checker)

    @classmethod
    def iter_schema_errors(cls, schema):
        meta_schema = cls.resolver.resolve_from_url(schema['$schema'])
        for error in cls(meta_schema).iter_errors(schema):
            error.linecol = get_line_col(schema, error.path)
            yield error

    def iter_errors(self, instance, _schema=None):
        for error in jsonschema.Draft7Validator.iter_errors(self, instance, _schema):
            error.linecol = get_line_col(instance, error.path)
            yield error

    @classmethod
    def check_schema(cls, schema):
        meta_schema = cls.resolver.resolve_from_url(schema['$schema'])
        for error in cls(meta_schema).iter_errors(schema):
            raise jsonschema.SchemaError.create_from(error)
        fixup_schema(schema)

    @classmethod
    def _check_schema_refs(self, schema):
        if isinstance(schema, dict) and '$ref' in schema:
            self.resolver.resolve(schema['$ref'])
        elif isinstance(schema, dict):
            for k, v in schema.items():
                self._check_schema_refs(v)
        elif isinstance(schema, (list, tuple)):
            for i in range(len(schema)):
                self._check_schema_refs(schema[i])

    @classmethod
    def check_schema_refs(self, err_msg, schema):
        scope = self.ID_OF(schema)
        if scope:
            self.resolver.push_scope(scope)

        try:
            self._check_schema_refs(schema)
        except jsonschema.RefResolutionError as exc:
            print(err_msg, exc, file=sys.stderr)


def format_error(filename, error, nodename=None, verbose=False):
    src = os.path.abspath(filename) + ':'

    if error.linecol[0] >= 0 :
        src = src + '%i:%i: '%(error.linecol[0]+1, error.linecol[1]+1)
    else:
        src += ' '

    if nodename is not None:
        src += nodename + ': '

    if error.path:
        for path in error.path:
            src += str(path) + ":"
        src += ' '

    if verbose:
        msg = str(error)
    else:
        msg = error.message
        # Failures under 'oneOf', 'allOf', or 'anyOf' schema don't give useful
        # error messages, so dump the schema in those cases.
        if not error.path and error.validator in ['oneOf', 'allOf', 'anyOf']:
            msg += '\n' + pprint.pformat(error.schema, width=72)

    return src + msg
