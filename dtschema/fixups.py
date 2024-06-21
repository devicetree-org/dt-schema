# SPDX-License-Identifier: BSD-2-Clause
# Copyright 2018 Linaro Ltd.
# Copyright 2018-2023 Arm Ltd.
# Python library for Devicetree schema validation
import re
import copy

import dtschema
from dtschema.lib import _get_array_range
from dtschema.lib import _is_int_schema
from dtschema.lib import _is_string_schema


def _extract_single_schemas(subschema):
    scalar_keywords = ('const', 'enum', 'pattern', 'minimum', 'maximum', 'multipleOf')
    return {k: subschema.pop(k) for k in scalar_keywords if k in subschema}


def _fixup_string_to_array(subschema):
    # nothing to do if we don't have a set of string schema
    if not _is_string_schema(subschema):
        return

    subschema['items'] = [_extract_single_schemas(subschema)]


def _fixup_reg_schema(subschema, path=[]):
    # nothing to do if we don't have a set of string schema
    if 'reg' not in path:
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

    subschema['items'] = [{'items': [_extract_single_schemas(item_schema)]}]


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


int_array_re = re.compile('int(8|16|32|64)-array')
unit_types_re = re.compile('-(bps|kBps|bits|percent|bp|m?hz|sec|ms|us|ns|ps|mm|nanoamp|(micro-)?ohms|micro(amp|watt)(-hours)?|milliwatt|microvolt|picofarads|(milli)?celsius|kelvin|k?pascal)$')

# Remove this once we remove array to matrix fixups
known_array_props = {
    'assigned-clock-rates',
    'linux,keycodes',
    'max8997,pmic-buck1-dvs-voltage',
    'max8997,pmic-buck2-dvs-voltage',
    'max8997,pmic-buck5-dvs-voltage',
}


def is_int_array_schema(subschema, path=[]):
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
    else:
        if [p for p in path if unit_types_re.search(p)] or set(path) & known_array_props:
            return True

    return 'items' in subschema and \
        ((isinstance(subschema['items'], list) and _is_int_schema(subschema['items'][0])) or
         (isinstance(subschema['items'], dict) and _is_int_schema(subschema['items'])))


# Fixup an int array that only defines the number of items.
def _fixup_int_array_min_max_to_matrix(subschema, path=[]):
    if not is_int_array_schema(subschema, path=path):
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

    tmpsch = {}
    if 'minItems' in subschema:
        tmpsch['minItems'] = subschema.pop('minItems')
    if 'maxItems' in subschema:
        tmpsch['maxItems'] = subschema.pop('maxItems')
    if 'items' in subschema:
        tmpsch['items'] = subschema.pop('items')

    if tmpsch:
        subschema['items'] = tmpsch
        subschema['maxItems'] = 1

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


def _fixup_int_array_items_to_matrix(subschema, path=[]):
    itemkeys = ('items', 'minItems', 'maxItems', 'uniqueItems', 'default')
    if not is_int_array_schema(subschema, path=path):
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


def _fixup_scalar_to_array(subschema):
    if not _is_int_schema(subschema):
        return

    subschema['items'] = [{'items': [_extract_single_schemas(subschema)]}]


def _fixup_items_size(schema, path=[]):
    # Make items list fixed size-spec
    if isinstance(schema, list):
        for l in schema:
            _fixup_items_size(l, path=path)
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

            _fixup_items_size(schema['items'], path=path + ['items'])

        elif not {'then', 'else'} & set(path):
            if 'maxItems' in schema and 'minItems' not in schema:
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


def fixup_vals(schema, path=[]):
    # Now we should be a the schema level to do actual fixups
    #print(schema)

    schema.pop('description', None)

    _fixup_reg_schema(schema, path=path)
    _fixup_remove_empty_items(schema)
    _fixup_int_array_min_max_to_matrix(schema, path=path)
    _fixup_int_array_items_to_matrix(schema, path=path)
    _fixup_string_to_array(schema)
    _fixup_scalar_to_array(schema)
    _fixup_items_size(schema, path=path)

    fixup_schema_to_201909(schema)


def _fixup_oneOf_to_enum(schema, path=[]):
    # Convert oneOf/anyOf lists with just 'const' entries into an enum.
    # This pattern is used to add descriptions on each entry which is not
    # possible with 'enum', but the error reporting is much worse with
    # oneOf/anyOf schemas.

    if 'anyOf' in schema:
        sch_list = schema['anyOf']
    elif 'oneOf' in schema:
        sch_list = schema['oneOf']
    elif 'items' in schema and isinstance(schema['items'], dict):
        # Sometimes 'items' appears first which isn't really handled by the
        # fixups, but we can handle it here.
        _fixup_oneOf_to_enum(schema['items'], path=path + ['items'])
        return
    else:
        return

    const_list = []
    for l in sch_list:
        if 'const' not in l or set(l) > {'const', 'description'}:
            return
        const_list += [l['const']]

    schema.pop('anyOf', None)
    schema.pop('oneOf', None)
    schema['enum'] = const_list


def walk_properties(schema, path=[]):
    if not isinstance(schema, dict):
        return

    _fixup_oneOf_to_enum(schema, path=path)

    # Recurse until we don't hit a conditional
    # Note we expect to encounter conditionals first.
    # For example, a conditional below 'items' is not supported
    for cond in ['allOf', 'oneOf', 'anyOf']:
        if cond in schema.keys():
            for l in schema[cond]:
                walk_properties(l, path=path + [cond])

    if 'then' in schema.keys():
        walk_properties(schema['then'], path=path + ['then'])

    fixup_vals(schema, path=path)


def fixup_interrupts(schema, path):
    # Supporting 'interrupts' implies 'interrupts-extended' is also supported.
    if 'properties' in schema:
        # Any node with 'interrupts' can have 'interrupt-parent'
        if schema['properties'].keys() & {'interrupts', 'interrupt-controller'} and \
        'interrupt-parent' not in schema['properties']:
            schema['properties']['interrupt-parent'] = True

        if 'interrupts' not in schema['properties'] or 'interrupts-extended' in schema['properties']:
            return

        schema['properties']['interrupts-extended'] = copy.deepcopy(schema['properties']['interrupts'])

    if 'required' in schema and 'interrupts' in schema['required'] and \
       (len(path) == 0 or path[-1] != 'oneOf'):
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
        if len(schema['required']) == 0:
            schema.pop('required')


known_variable_matrix_props = {
    'fsl,pins',
    'qcom,board-id'
}


def fixup_sub_schema(schema, path=[]):
    if not isinstance(schema, dict):
        return

    schema.pop('description', None)
    fixup_interrupts(schema, path)
    fixup_node_props(schema)

    # 'additionalProperties: true' doesn't work with 'unevaluatedProperties', so
    # remove it. It's in the schemas for common (incomplete) schemas.
    if 'additionalProperties' in schema and schema['additionalProperties'] == True:
        schema.pop('additionalProperties', None)

    for k, v in schema.items():
        if k in ['select', 'if', 'then', 'else', 'not', 'additionalProperties']:
            fixup_sub_schema(v, path=path + [k])

        if k in ['allOf', 'anyOf', 'oneOf']:
            for subschema in v:
                fixup_sub_schema(subschema, path=path + [k])

        if k not in ['dependentRequired', 'dependentSchemas', 'dependencies', 'properties', 'patternProperties', '$defs']:
            continue

        for prop in v:
            if prop in known_variable_matrix_props and isinstance(v[prop], dict):
                ref = v[prop].pop('$ref', None)
                schema[k][prop] = {}
                if ref:
                    schema[k][prop]['$ref'] = ref
                continue

            walk_properties(v[prop], path=path + [k, prop])
            # Recurse to check for {properties,patternProperties} in each prop
            fixup_sub_schema(v[prop], path=path + [k, prop])

    fixup_schema_to_201909(schema)


def fixup_node_props(schema):
    # If no restrictions on undefined properties, then no need to add any implicit properties
    if (not {'unevaluatedProperties', 'additionalProperties'} & schema.keys()) or \
       ('additionalProperties' in schema and schema['additionalProperties'] is True) or \
       ('unevaluatedProperties' in schema and schema['unevaluatedProperties'] is True):
        return

    schema.setdefault('properties', dict())
    schema['properties'].setdefault('phandle', True)
    schema['properties'].setdefault('status', True)
    schema['properties'].setdefault('secure-status', True)
    schema['properties'].setdefault('$nodename', True)
    schema['properties'].setdefault('bootph-pre-sram', True)
    schema['properties'].setdefault('bootph-verify', True)
    schema['properties'].setdefault('bootph-pre-ram', True)
    schema['properties'].setdefault('bootph-some-ram', True)
    schema['properties'].setdefault('bootph-all', True)

    # 'dma-ranges' allowed when 'ranges' is present
    if 'ranges' in schema['properties']:
        schema['properties'].setdefault('dma-ranges', True)

    keys = list(schema['properties'].keys())
    if 'patternProperties' in schema:
        keys.extend(schema['patternProperties'])

    for key in keys:
        if re.match(r'^pinctrl-[0-9]', key):
            break
    else:
        schema['properties'].setdefault('pinctrl-names', True)
        schema.setdefault('patternProperties', dict())
        schema['patternProperties']['pinctrl-[0-9]+'] = True

    if "clocks" in keys and "assigned-clocks" not in keys:
        schema['properties']['assigned-clocks'] = True
        schema['properties']['assigned-clock-rates-u64'] = True
        schema['properties']['assigned-clock-rates'] = True
        schema['properties']['assigned-clock-parents'] = True


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
    If it has a $nodename property, then create a select schema from that.
    '''
    if "select" in schema:
        return

    if 'properties' not in schema or 'compatible' in schema['properties']:
        return

    if '$nodename' in schema['properties'] and schema['properties']['$nodename'] is not True:
        schema['select'] = {
            'required': ['$nodename'],
            'properties': {'$nodename': convert_to_dict(schema['properties']['$nodename'])}}

        return


def fixup_schema(schema):
    # Remove parts not necessary for validation
    schema.pop('examples', None)
    schema.pop('maintainers', None)
    schema.pop('historical', None)

    add_select_schema(schema)
    fixup_sub_schema(schema)
