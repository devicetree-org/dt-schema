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
import json

import jsonschema

from jsonschema.exceptions import RefResolutionError

import dtschema.dtb

schema_base_url = "http://devicetree.org/"
schema_basedir = os.path.dirname(os.path.abspath(__file__))

# We use a lot of regex's in schema and exceeding the cache size has noticeable
# peformance impact.
re._MAXCACHE = 2048


class tagged_list(list):

    tags = {u'!u8': 8, u'!u16': 16, u'!u32': 32, u'!u64': 64}

    def __init__(self, int_list, tag, tags=tags):
        int_list = [sized_int(i, size=tags[tag]) for i in int_list]
        super().__init__(int_list)

    @staticmethod
    def constructor(loader, node):
        return tagged_list(loader.construct_sequence(node), node.tag)


class sized_int(int):
    def __new__(cls, value, *args, **kwargs):
        return int.__new__(cls, value)

    def __init__(self, value, size=32, phandle=False):
        self.size = size
        self.phandle = phandle

    @staticmethod
    def constructor(loader, node):
        return sized_int(loader.construct_yaml_int(node), phandle=True)


rtyaml = ruamel.yaml.YAML(typ='rt')
rtyaml.allow_duplicate_keys = False
rtyaml.preserve_quotes = True
rtyaml.Constructor.add_constructor(u'!u8', tagged_list.constructor)
rtyaml.Constructor.add_constructor(u'!u16', tagged_list.constructor)
rtyaml.Constructor.add_constructor(u'!u32', tagged_list.constructor)
rtyaml.Constructor.add_constructor(u'!u64', tagged_list.constructor)
rtyaml.Constructor.add_constructor(u'!phandle', sized_int.constructor)

yaml = ruamel.yaml.YAML(typ='safe')
yaml.allow_duplicate_keys = False
yaml.Constructor.add_constructor(u'!u8', tagged_list.constructor)
yaml.Constructor.add_constructor(u'!u16', tagged_list.constructor)
yaml.Constructor.add_constructor(u'!u32', tagged_list.constructor)
yaml.Constructor.add_constructor(u'!u64', tagged_list.constructor)
yaml.Constructor.add_constructor(u'!phandle', sized_int.constructor)


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
        schema_user_paths.append(os.path.abspath(path))


def check_id_path(filename, id):
    id = id.replace('http://devicetree.org/schemas/', '')
    id = id.replace('#', '')

    base = os.path.abspath(filename)

    for p in schema_user_paths:
        base = base.replace(p + '/', '')

    base = base.replace(os.path.join(schema_basedir, 'schemas/'), '')
    base = base.replace(os.path.abspath('schemas/') + '/', '')

    if not id == base:
        print(filename +
              ": $id: relative path/filename doesn't match actual path or filename\n\texpected: http://devicetree.org/schemas/" +
              base + '#',
              file=sys.stderr)


def do_load(filename):
    with open(filename, 'r', encoding='utf-8') as f:
        if filename.endswith('.json'):
            return json.load(f)

        return yaml.load(f.read())


def load_schema(schema):
    for path in schema_user_paths:
        if schema.startswith('schemas/'):
            schema_file = schema.partition('/')[2]
        else:
            schema_file = schema
        schema_file = os.path.join(path, schema_file)
        if not os.path.isfile(schema_file):
            continue

        return do_load(schema_file)

    return do_load(os.path.join(schema_basedir, schema))


def _value_is_type(subschema, key, type):
    if key not in subschema:
        return False

    if isinstance(subschema[key], list):
        val = subschema[key][0]
    else:
        val = subschema[key]

    return isinstance(val, type)


def _is_int_schema(subschema):
    for match in ['const', 'enum', 'minimum', 'maximum']:
        if _value_is_type(subschema, match, int):
            return True

    return False


def _is_string_schema(subschema):
    for match in ['const', 'enum', 'pattern']:
        if _value_is_type(subschema, match, str):
            return True

    return False


def _extract_single_schemas(subschema):
    return {k: subschema.pop(k) for k in ('const', 'enum', 'pattern', 'minimum', 'maximum') if k in subschema}


def _fixup_string_to_array(propname, subschema):
    # nothing to do if we don't have a set of string schema
    if not _is_string_schema(subschema):
        return

    subschema['items'] = [_extract_single_schemas(subschema)]


def _fixup_reg_schema(propname, subschema):
    # nothing to do if we don't have a set of string schema
    if propname != 'reg':
        return

    if 'items' in subschema:
        if isinstance(subschema['items'], list):
            item_schema = subschema['items'][0]
        else:
            item_schema = subschema['items']
        if not _is_int_schema(item_schema):
            return
    elif _is_int_schema(subschema):
        item_schema = subschema
    else:
        return

    subschema['items'] = [ {'items': [ _extract_single_schemas(item_schema) ] } ]


def _is_matrix_schema(subschema):
    if 'items' not in subschema:
        return False

    if isinstance(subschema['items'], list):
        for l in subschema['items']:
            if l.keys() & {'items', 'maxItems', 'minItems'}:
                return True
    elif subschema['items'].keys() & {'items', 'maxItems', 'minItems'}:
        return True

    return False

# If we have a matrix with variable inner and outer dimensions, then drop the dimensions
# because we have no way to reconstruct them.
def _fixup_int_matrix(subschema):
    if not _is_matrix_schema(subschema):
        return

    outer_dim = _get_array_range(subschema)
    inner_dim = _get_array_range(subschema.get('items', {}))

    if outer_dim[0] != outer_dim[1] and inner_dim[0] != inner_dim[1]:
        subschema.pop('items', None)
        subschema.pop('maxItems', None)
        subschema.pop('minItems', None)
        subschema['type'] = 'array'


int_array_re = re.compile('int(8|16|32|64)-array')
unit_types_re = re.compile('-(bits|percent|bp|mhz|hz|sec|ms|us|ns|ps|mm|microamp|microamp-hours|ohms|micro-ohms|microwatt-hours|microvolt|picofarads|celsius|millicelsius|kpascal)$')

# Remove this once we remove array to matrix fixups
known_array_props = {'assigned-clock-rates', 'linux,keycodes'}

def is_int_array_schema(propname, subschema):
    if 'allOf' in subschema:
        # Find 'items'. It may be under the 'allOf' or at the same level
        for item in subschema['allOf']:
            if 'items' in item:
                subschema = item
                continue
            if '$ref' in item:
                return int_array_re.search(item['$ref'])
    if '$ref' in subschema:
        return int_array_re.search(subschema['$ref'])
    elif unit_types_re.search(propname) or propname in known_array_props:
        return True

    return 'items' in subschema and \
        ((isinstance(subschema['items'], list) and _is_int_schema(subschema['items'][0])) or
         (isinstance(subschema['items'], dict) and _is_int_schema(subschema['items'])))


# Fixup an int array that only defines the number of items.
# In this case, we allow either form [[ 0, 1, 2]] or [[0], [1], [2]]
def _fixup_int_array_min_max_to_matrix(propname, subschema):
    if not is_int_array_schema(propname, subschema):
        return

    if 'allOf' in subschema:
        # Find 'min/maxItems'. It may be under the 'allOf' or at the same level
        for item in subschema['allOf']:
            if item.keys() & {'minItems', 'maxItems'}:
                subschema = item
                break

    if 'items' in subschema and isinstance(subschema['items'], list):
        return

    if _is_matrix_schema(subschema):
        return

    if subschema.get('maxItems') == 1:
        return

    tmpsch = {}
    if 'minItems' in subschema:
        tmpsch['minItems'] = subschema.pop('minItems')
    if 'maxItems' in subschema:
        tmpsch['maxItems'] = subschema.pop('maxItems')

    if tmpsch:
        subschema['oneOf'] = [copy.deepcopy(tmpsch), {'items': [copy.deepcopy(tmpsch)]}]
        subschema['oneOf'][0].update({'items': {'maxItems': 1}})

        # if minItems can be 1, then both oneOf clauses can be true so increment
        # minItems in one clause to prevent that.
        if subschema['oneOf'][0].get('minItems') == 1:
            subschema['oneOf'][0]['minItems'] += 1

        # Since we added an 'oneOf' the tree walking code won't find it and we need to do fixups
        _fixup_items_size(subschema['oneOf'])


def _fixup_remove_empty_items(subschema):
    if 'items' not in subschema:
        return
    elif isinstance(subschema['items'], dict):
        _fixup_remove_empty_items(subschema['items'])
        return

    for item in subschema['items']:
        item.pop('description', None)
        _fixup_remove_empty_items(item)
        if item != {}:
            break
    else:
        subschema.setdefault('type', 'array')
        subschema.setdefault('maxItems', len(subschema['items']))
        subschema.setdefault('minItems', len(subschema['items']))
        del subschema['items']


def _fixup_int_array_items_to_matrix(propname, subschema):
    itemkeys = ('items', 'minItems', 'maxItems', 'uniqueItems', 'default')
    if not is_int_array_schema(propname, subschema):
        return

    if 'allOf' in subschema:
        # Find 'items'. It may be under the 'allOf' or at the same level
        for item in subschema['allOf']:
            if 'items' in item:
                subschema = item
                break

    if 'items' not in subschema or _is_matrix_schema(subschema):
        return

    if isinstance(subschema['items'], dict):
        subschema['items'] = {k: subschema.pop(k) for k in itemkeys if k in subschema}

    if isinstance(subschema['items'], list):
        subschema['items'] = [{k: subschema.pop(k) for k in itemkeys if k in subschema}]


def _fixup_scalar_to_array(propname, subschema):
    if not _is_int_schema(subschema):
        return

    subschema['items'] = [{'items': [_extract_single_schemas(subschema)]}]


def _fixup_items_size(schema):
    # Make items list fixed size-spec
    if isinstance(schema, list):
        for l in schema:
            _fixup_items_size(l)
    elif isinstance(schema, dict):
        schema.pop('description', None)
        if 'items' in schema:
            schema['type'] = 'array'

            if isinstance(schema['items'], list):
                c = len(schema['items'])
                if 'minItems' not in schema:
                    schema['minItems'] = c
                if 'maxItems' not in schema:
                    schema['maxItems'] = c

            _fixup_items_size(schema['items'])

        elif 'maxItems' in schema and 'minItems' not in schema:
            schema['minItems'] = schema['maxItems']
        elif 'minItems' in schema and 'maxItems' not in schema:
            schema['maxItems'] = schema['minItems']


def fixup_schema_to_201909(schema):
    if not isinstance(schema, dict):
        return

    # dependencies is now split into dependentRequired and dependentSchema
    try:
        val = schema.pop('dependencies')
        for k, v in val.items():
            if isinstance(v, list):
                schema.setdefault('dependentRequired', {})
                schema['dependentRequired'][k] = v
            else:
                schema.setdefault('dependentSchemas', {})
                schema['dependentSchemas'][k] = v
    except:
        pass


def fixup_schema_to_202012(schema):
    if not isinstance(schema, dict):
        return

    fixup_schema_to_201909(schema)

    try:
        if isinstance(schema['items'], list):
            schema['prefixItems'] = schema.pop('items')
            for i in schema['prefixItems']:
                fixup_schema_to_202012(i)
        if isinstance(schema['items'], dict):
            fixup_schema_to_202012(schema['items'])
    except:
        pass

    try:
        val = schema.pop('additionalItems')
        schema['unevaluatedItems'] = val
    except:
        pass


def fixup_vals(propname, schema):
    # Now we should be a the schema level to do actual fixups
    #print(schema)

    schema.pop('description', None)

    _fixup_reg_schema(propname, schema)
    _fixup_remove_empty_items(schema)
    _fixup_int_matrix(schema)
    _fixup_int_array_min_max_to_matrix(propname, schema)
    _fixup_int_array_items_to_matrix(propname, schema)
    _fixup_string_to_array(propname, schema)
    _fixup_scalar_to_array(propname, schema)
    _fixup_items_size(schema)

    fixup_schema_to_201909(schema)


def walk_properties(propname, schema):
    if not isinstance(schema, dict):
        return
    # Recurse until we don't hit a conditional
    # Note we expect to encounter conditionals first.
    # For example, a conditional below 'items' is not supported
    for cond in ['allOf', 'oneOf', 'anyOf']:
        if cond in schema.keys():
            for l in schema[cond]:
                walk_properties(propname, l)

    if 'then' in schema.keys():
        walk_properties(propname, schema['then'])

    fixup_vals(propname, schema)


def fixup_schema(schema):
    # Remove parts not necessary for validation
    schema.pop('examples', None)
    schema.pop('maintainers', None)
    schema.pop('historical', None)

    add_select_schema(schema)
    fixup_sub_schema(schema, True)


def fixup_sub_schema(schema, is_prop):
    if not isinstance(schema, dict):
        return

    schema.pop('description', None)
    fixup_interrupts(schema)
    if is_prop:
        fixup_node_props(schema)

    for k, v in schema.items():
        if k in ['select', 'if', 'then', 'else', 'additionalProperties', 'not']:
            fixup_sub_schema(v, False)

        if k in ['allOf', 'anyOf', 'oneOf']:
            for subschema in v:
                fixup_sub_schema(subschema, True)

        if k not in ['dependentRequired', 'dependentSchemas', 'dependencies', 'properties', 'patternProperties', '$defs']:
            continue

        for prop in v:
            walk_properties(prop, v[prop])
            # Recurse to check for {properties,patternProperties} in each prop
            fixup_sub_schema(v[prop], True)

    fixup_schema_to_201909(schema)


def fixup_node_props(schema):
    if not {'properties', 'patternProperties'} & schema.keys():
        return
    if ('additionalProperties' in schema and schema['additionalProperties'] is True) or \
       ('unevaluatedProperties' in schema and schema['unevaluatedProperties'] is True):
        return

    schema.setdefault('properties', dict())
    schema['properties'].setdefault('phandle', True)
    schema['properties'].setdefault('status', True)
    schema['properties'].setdefault('secure-status', True)
    schema['properties'].setdefault('$nodename', True)

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

    if "clocks" in keys and "assigned-clocks" not in keys:
        schema['properties']['assigned-clocks'] = True
        schema['properties']['assigned-clock-rates'] = True
        schema['properties']['assigned-clock-parents'] = True


def extract_node_compatibles(schema):
    if not isinstance(schema, dict):
        return set()

    compatible_list = set()

    for l in item_generator(schema, 'enum'):
        if isinstance(l[0], str):
            compatible_list.update(l)

    for l in item_generator(schema, 'const'):
        compatible_list.update([str(l)])

    for l in item_generator(schema, 'pattern'):
        compatible_list.update([l])

    return compatible_list


def extract_compatibles(schema):
    if not isinstance(schema, dict):
        return set()

    compatible_list = set()
    for sch in item_generator(schema, 'compatible'):
        compatible_list.update(extract_node_compatibles(sch))

    return compatible_list


def item_generator(json_input, lookup_key):
    if isinstance(json_input, dict):
        for k, v in json_input.items():
            if k == lookup_key:
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
    if "select" in schema:
        return

    if 'properties' not in schema:
        schema['select'] = False
        return

    if 'compatible' in schema['properties']:
        compatible_list = extract_node_compatibles(schema['properties']['compatible'])

        if len(compatible_list):
            try:
                compatible_list.remove('syscon')
            except:
                pass
            try:
                compatible_list.remove('simple-mfd')
            except:
                pass

            if len(compatible_list) != 0:
                schema['select'] = {
                    'required': ['compatible'],
                    'properties': {'compatible': {'contains': {'enum': sorted(compatible_list)}}}}

                return

    if '$nodename' in schema['properties'] and schema['properties']['$nodename'] is not True:
        schema['select'] = {
            'required': ['$nodename'],
            'properties': {'$nodename': convert_to_dict(schema['properties']['$nodename'])}}

        return

    schema['select'] = False


def fixup_interrupts(schema):
    # Supporting 'interrupts' implies 'interrupts-extended' is also supported.
    if 'properties' not in schema:
        return

    # Any node with 'interrupts' can have 'interrupt-parent'
    if schema['properties'].keys() & {'interrupts', 'interrupt-controller'} and \
       'interrupt-parent' not in schema['properties']:
        schema['properties']['interrupt-parent'] = True

    if 'interrupts' not in schema['properties'] or 'interrupts-extended' in schema['properties']:
        return

    schema['properties']['interrupts-extended'] = copy.deepcopy(schema['properties']['interrupts'])

    if not ('required' in schema and 'interrupts' in schema['required']):
        return

    # Currently no better way to express either 'interrupts' or 'interrupts-extended'
    # is required. If this fails validation, the error reporting is the whole
    # schema file fails validation
    reqlist = [{'required': ['interrupts']}, {'required': ['interrupts-extended']}]
    if 'oneOf' in schema:
        if 'allOf' not in schema:
            schema['allOf'] = []
        schema['allOf'].append({'oneOf': reqlist})
    else:
        schema['oneOf'] = reqlist
    schema['required'].remove('interrupts')


def make_compatible_schema(schemas):
    compat_sch = [{'enum': []}]
    compatible_list = set()
    for sch in schemas:
        compatible_list |= extract_compatibles(sch)

    # Allow 'foo' values for examples
    compat_sch += [{'pattern': '^foo'}]

    prog = re.compile('.*[\^\[{\(\$].*')
    for c in compatible_list:
        if prog.match(c):
            # Exclude the generic pattern
            if c != '^[a-zA-Z][a-zA-Z0-9,+\-._]+$':
                compat_sch += [{'pattern': c}]
        else:
            compat_sch[0]['enum'].append(c)

    compat_sch[0]['enum'].sort()
    schemas += [{
        '$id': 'generated-compatibles',
        '$filename': 'Generated schema of documented compatible strings',
        'select': True,
        'properties': {
            'compatible': {
                'items': {
                    'anyOf': compat_sch
                }
            }
        }
    }]


def process_schema(filename):
    try:
        schema = load_schema(filename)
    except:
        print(filename + ": ignoring, error parsing file", file=sys.stderr)
        return

    # Check that the validation schema is valid
    try:
        DTValidator.check_schema(schema)
    except jsonschema.SchemaError as exc:
        print(filename + ": ignoring, error in schema: " + ': '.join(str(x) for x in exc.path),
              file=sys.stderr)
        #print(exc.message)
        return
    except:
        print(filename + ": ignoring, unknown error in schema (not a schema?)",
              file=sys.stderr)
        return

    if 'select' not in schema:
        print(filename + ": warning: no 'select' found in schema found", file=sys.stderr)
        return

    schema["type"] = "object"
    schema["$filename"] = filename
    return schema


def process_schemas(schema_paths, core_schema=True):
    ids = []
    schemas = []

    for filename in schema_paths:
        if not os.path.isfile(filename):
            continue
        sch = process_schema(os.path.abspath(filename))
        if not sch:
            continue
        schemas.append(sch)
        if ids.count(sch['$id']):
            print(os.path.abspath(filename) + ": duplicate '$id' value '" + sch['$id'] + "'", file=sys.stderr)
        ids.append(sch['$id'])

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

    make_compatible_schema(schemas)

    return schemas


def _get_array_range(subschema):
    if isinstance(subschema, list):
        if len(subschema) != 1:
            return (0, 0)
        subschema = subschema[0]
    if 'items' in subschema and isinstance(subschema['items'], list):
        max = len(subschema['items'])
        min = subschema.get('minItems', max)
    else:
        min = subschema.get('minItems', 1)
        max = subschema.get('maxItems', subschema.get('minItems', 0))

    return (min, max)


def _merge_dim(dim1, dim2):
    d = []
    for i in range(0, 2):
        if dim1[i] == (0, 0):
            d.insert(i, dim2[i])
        elif dim2[i] == (0, 0):
            d.insert(i, dim1[i])
        else:
            d.insert(i, (min(dim1[i] + dim2[i]), max(dim1[i] + dim2[i])))

    return tuple(d)


type_re = re.compile('(flag|u?int(8|16|32|64)(-(array|matrix))?|string(-array)?|phandle(-array)?)')


def _extract_prop_type(props, schema, propname, subschema):
    if not isinstance(subschema, dict):
        return

    if subschema.keys() & {'properties', 'patternProperties', 'additionalProperties'}:
        _extract_subschema_types(props, schema, subschema)

    # We only support local refs
    if '$ref' in subschema and subschema['$ref'].startswith('#/'):
        sch_path = subschema['$ref'].split('/')[1:]
        subschema = schema
        for p in sch_path:
            subschema = subschema[p]
        #print(propname, sch_path, subschema, file=sys.stderr)
        _extract_prop_type(props, schema, propname, subschema)

    try:
        prop_type = type_re.search(subschema['$ref']).group(0)
    except:
        if 'type' in subschema and subschema['type'] == 'boolean':
            prop_type = 'flag'
        elif 'items' in subschema:
            items = subschema['items']
            if (isinstance(items, list) and _is_string_schema(items[0])) or \
               (isinstance(items, dict) and _is_string_schema(items)):
                # implicit string type
                prop_type = 'string-array'
            elif not (isinstance(items, list) and len(items) == 1 and \
                 'items' in items and isinstance(items['items'], list) and len(items['items']) == 1) and \
                 unit_types_re.search(propname):
                prop_type = 'uint32-matrix'
            else:
                prop_type = None
        else:
            prop_type = None

    if prop_type:
        props.setdefault(propname, {})

        if prop_type == 'phandle-array' or prop_type.endswith('-matrix'):
            dim = (_get_array_range(subschema), _get_array_range(subschema.get('items', {})))
            if dim != ((0, 0), (0, 0)):
                if not 'dim' in props[propname]:
                    props[propname]['dim'] = dim
                elif props[propname]['dim'] != dim:
                    # Conflicting dimensions
                    props[propname]['dim'] = _merge_dim(props[propname]['dim'], dim)

        if 'type' in props[propname]:
            if prop_type in props[propname]['type']:
                # Already have the same type
                return
            for t in props[propname]['type']:
                if prop_type in t:
                    # Already have the looser type
                    return
                if t in prop_type:
                    # Replace scalar type with array type
                    i = props[propname]['type'].index(t)
                    props[propname]['type'][i] = prop_type
                    props[propname]['$id'][i] = schema['$id']
                    return
        else:
            props[propname]['type'] = []
            props[propname]['$id'] = []

        props[propname]['type'] += [ prop_type ]
        props[propname]['$id'] += [ schema['$id'] ]
        return

    for k in subschema.keys() & {'allOf', 'oneOf', 'anyOf'}:
        for v in subschema[k]:
            _extract_prop_type(props, schema, propname, v)


def _extract_subschema_types(props, schema, subschema):
    if not isinstance(subschema, dict):
        return

    if 'additionalProperties' in subschema:
        _extract_subschema_types(props, schema, subschema['additionalProperties'])

    for k in subschema.keys() & {'properties', 'patternProperties'}:
        if isinstance(subschema[k], dict):
            for p,v in subschema[k].items():
                _extract_prop_type(props, schema, p, v)


def extract_types(schemas):
    if not isinstance(schemas, list):
        return

    props = {}
    for sch in schemas:
        _extract_subschema_types(props, sch, sch)

    return props


def get_prop_types():
    global schema_cache
    pat_props = {}

    props = dtschema.extract_types(schema_cache)

    # hack to remove aliases pattern
    del props['^[a-z][a-z0-9\-]*$']

    # Remove all properties without a type
    for key in [key for key in props if 'type' not in props[key] ]: del props[key]

    # Split out pattern properties
    for key in [key for key in props if re.fullmatch('(^\^.*|.*\$$|.*[*[].*)', key) ]:
        #print(key, props[key])
        pat_props[key] = props[key]
        pat_props[key]['regex'] = re.compile(key)
        del props[key]

    return [ props, pat_props ]


def load(filename, line_number=False):
    if filename.endswith('.dtb'):
        with open(filename, 'rb') as f:
            return [ dtschema.dtb.fdt_unflatten(f.read()) ]
    with open(filename, 'r', encoding='utf-8') as f:
        if line_number:
            return rtyaml.load(f.read())
        else:
            return yaml.load(f.read())


schema_cache = []


def set_schema(schemas):
    global schema_cache
    schema_cache = schemas


def http_handler(uri):
    global schema_cache
    '''Custom handler for http://devicetre.org YAML references'''
    try:
        if schema_base_url in uri:
            for sch in schema_cache:
                if uri in sch['$id']:
                    return sch
            # If we have a schema_cache, then the schema should have been there unless the schema had errors
            if len(schema_cache):
                return False
            if 'meta-schemas' in uri:
                return load_schema(uri.replace(schema_base_url, ''))

            try:
                schema = load_schema(uri.replace(schema_base_url, ''))
            except Exception as exc:
                raise RefResolutionError('Unable to find schema file matching $id: ' + uri)

            try:
                DTValidator.check_schema(schema)
            except Exception as exc:
                raise RefResolutionError('Error in referenced schema matching $id: ' + uri)

            return schema

        from urllib.request import urlopen

        return yaml.load(urlopen(uri).read().decode('utf-8'))
    except FileNotFoundError as e:
        print('Unknown file referenced:', e, file=sys.stderr)
        exit(-1)


handlers = {"http": http_handler}


def typeSize(validator, typeSize, instance, schema):
    if not isinstance(instance[0], list):
        return
    if isinstance(instance[0][0], sized_int):
        size = instance[0][0].size
    else:
        size = 32

    if typeSize != size:
        yield jsonschema.ValidationError("size is %r, expected %r" % (size, typeSize))


DTVal = jsonschema.validators.extend(jsonschema.Draft201909Validator, {'typeSize': typeSize})


class DTValidator(DTVal):
    '''Custom Validator for Devicetree Schemas

    Overrides the Draft7 metaschema with the devicetree metaschema. This
    validator is used in exactly the same way as the Draft7Validator. Schema
    files can be validated with the .check_schema() method, and .validate()
    will check the data in a devicetree file.
    '''
    resolver = jsonschema.RefResolver('', None, handlers=handlers)
    format_checker = jsonschema.FormatChecker()

    def __init__(self, *args, **kwargs):
        kwargs.setdefault('resolver', self.resolver)
        kwargs.setdefault('format_checker', self.format_checker)
        super().__init__(*args, **kwargs)

    @classmethod
    def annotate_error(self, error, schema, path):
        error.note = None
        error.schema_file = None

        for e in error.context:
            self.annotate_error(e, schema, path + e.schema_path)

        scope = self.ID_OF(schema)
        self.resolver.push_scope(scope)
        ref_depth = 1

        lastp = ''
        for p in path:
            # json-schema 3.2.0 includes 'if' in schema path
            if lastp != 'properties' and p == 'if':
                continue
            lastp = p

            while '$ref' in schema and isinstance(schema['$ref'], str):
                ref = self.resolver.resolve(schema['$ref'])
                schema = ref[1]
                self.resolver.push_scope(ref[0])
                ref_depth += 1

            if '$id' in schema and isinstance(schema['$id'], str):
                error.schema_file = schema['$id']

            schema = schema[p]

            if isinstance(schema, dict):
                if 'description' in schema and isinstance(schema['description'], str):
                    error.note = schema['description']

        while ref_depth > 0:
            self.resolver.pop_scope()
            ref_depth -= 1

        if isinstance(error.schema, dict) and 'description' in error.schema:
            error.note = error.schema['description']

    @classmethod
    def iter_schema_errors(cls, schema):
        meta_schema = cls.resolver.resolve_from_url(schema['$schema'])
        for error in cls(meta_schema).iter_errors(schema):
            cls(meta_schema).annotate_error(error, meta_schema, error.schema_path)
            error.linecol = get_line_col(schema, error.path)
            yield error

    def iter_errors(self, instance, _schema=None):
        for error in super().iter_errors(instance, _schema):
            error.linecol = get_line_col(instance, error.path)
            error.note = None
            error.schema_file = None
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
    def check_schema_refs(self, filename, schema):
        scope = self.ID_OF(schema)
        if scope:
            self.resolver.push_scope(scope)

        try:
            self._check_schema_refs(schema)
        except jsonschema.RefResolutionError as exc:
            print(filename + ':', exc, file=sys.stderr)

        check_id_path(filename, schema['$id'])

    @classmethod
    def _check_str(self, err_msg, schema, key, v):
        if not (isinstance(v, ruamel.yaml.scalarstring.SingleQuotedScalarString) or
                isinstance(v, ruamel.yaml.scalarstring.DoubleQuotedScalarString)):
            return

        # Only checking const and list values
        if key and key != 'const':
            return

        if v[0] in '#/"':
            return

        # Flow style with ',' needs quoting
        if schema.fa.flow_style() and ',' in v:
            return

        if isinstance(schema, ruamel.yaml.comments.CommentedBase):
            if isinstance(schema, dict):
                line = schema.lc.key(key)[0]
            else:
                line = schema.lc.line
            line += 1
        else:
            line = None

        print(err_msg + str(line) + ": quotes are not necessary: " + v, file=sys.stderr)

    @classmethod
    def check_quotes(self, err_msg, schema):
        if isinstance(schema, dict):
            for k, v in schema.items():
                self._check_str(err_msg, schema, k, v)
                self.check_quotes(err_msg, v)

        if isinstance(schema, list):
            for s in schema:
                self._check_str(err_msg, schema, None, s)
                self.check_quotes(err_msg, s)


def format_error(filename, error, prefix="", nodename=None, verbose=False):
    src = prefix + os.path.abspath(filename) + ':'

    if error.linecol[0] >= 0:
        src = src + '%i:%i: ' % (error.linecol[0]+1, error.linecol[1]+1)
    else:
        src += ' '

    if nodename is not None:
        src += nodename + ': '

    if error.absolute_path:
        for path in error.absolute_path:
            src += str(path) + ":"
        src += ' '

    #print(error.__dict__)
    if verbose:
        msg = str(error)
    elif error.context:
        # An error on a conditional will have context with sub-errors
        msg = "'" + error.schema_path[-1] + "' conditional failed, one must be fixed:"

        for suberror in sorted(error.context, key=lambda e: e.path):
            if suberror.context:
                msg += '\n' + format_error(filename, suberror, prefix=prefix+"\t", nodename=nodename, verbose=verbose)
            elif suberror.message not in msg:
                msg += '\n' + prefix + '\t' + suberror.message
                if suberror.note and suberror.note != error.note:
                    msg += '\n\t\t' + prefix + 'hint: ' + suberror.note

    elif error.schema_path[-1] == 'oneOf':
        msg = 'More than one condition true in oneOf schema:\n\t' + \
            '\t'.join(pprint.pformat(error.schema, width=72).splitlines(True))

    else:
        msg = error.message

    if error.note:
        msg += '\n\t' + prefix + 'hint: ' + error.note

    if error.schema_file:
        msg += '\n\t' + prefix + 'from schema $id: ' + error.schema_file

    return src + msg
