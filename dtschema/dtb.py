# SPDX-License-Identifier: BSD-2-Clause
# Copyright 2021-2022 Arm Ltd.
# Python library for Devicetree schema validation

import sys
import struct

import libfdt
from libfdt import QUIET_NOTFOUND

import dtschema

u8 = struct.Struct('B')
s8 = struct.Struct('b')
u16 = struct.Struct('>H')
s16 = struct.Struct('>h')
u32 = struct.Struct('>L')
s32 = struct.Struct('>l')
u64 = struct.Struct('>Q')
s64 = struct.Struct('>q')

type_format = {
    'int8': s8,
    'uint8': u8,
    'int16': s16,
    'uint16': u16,
    'int32': s32,
    'uint32': u32,
    'int64': s64,
    'uint64': u64,
    'phandle': u32
}


def bytes_to_string(b):
    try:
        strings = b.decode(encoding='ascii').split('\0')

        count = len(strings) - 1
        if count > 0 and not len(strings[-1]):
            for string in strings[:-1]:
                if not string:
                    # While empty strings are valid, assume empty strings means
                    # not a string as we only get here when there are multiple
                    # types.
                    break
                if not string.isprintable():
                    break
            else:
                return strings[:-1]
    except:
        return None


def get_stride(len, dim):
    match = 0
    if min(dim) == 0:
        return 0
    for d in range(dim[0], dim[1] + 1):
        if not len % d:
            match += 1
            stride = d
    if match == 1:
        return stride

    return 0


def prop_value(validator, nodename, p):
    # First, check for a boolean type
    if not len(p):
        return True

    dim = None
    data = bytes(p)
    fmt = None

    if nodename in {'__fixups__', 'aliases'}:
        return data[:-1].decode(encoding='ascii').split('\0')

    prop_types = set(validator.property_get_type(p.name))
    prop_types -= {'node'}

    # Filter out types impossible for the size of the property
    if len(prop_types) > 1:
        if len(p) > 4:
            prop_types -= {'int32', 'uint32'}
        else:
            prop_types -= {'int64', 'uint64', 'int64-array', 'uint64-array'}
        if len(p) > 2:
            prop_types -= {'int16', 'uint16'}
        else:
            prop_types -= {'int32', 'uint32', 'int32-array', 'uint32-array'}
        if len(p) > 1:
            prop_types -= {'int8', 'uint8'}
        else:
            prop_types -= {'int16', 'uint16', 'int16-array', 'uint16-array'}
        if len(p) > 0:
            prop_types -= {'flag'}

    if len(prop_types) > 1:
        if {'string', 'string-array'} & prop_types:
            str = bytes_to_string(data)
            if str:
                return str
            # Assuming only one other type
            try:
                fmt = prop_types.difference({'string', 'string-array'}).pop()
            except:
                return data
        else:
            #print(p.name + ': multiple types found', file=sys.stderr)
            fmt = None
    elif len(prop_types) == 1:
        fmt = prop_types.pop()

    if not fmt:
        # Primarily for aliases properties
        try:
            s = data.decode(encoding='ascii')
            if s.endswith('\0'):
                s = s[:-1]
                if s.isprintable():
                    return [s]
        except:
            pass
        if not len(data) % 4:
            fmt = 'uint32-array'
        else:
            #print(p.name + ': no type found', file=sys.stderr)
            return data

    if fmt.startswith('string'):
        return data[:-1].decode(encoding='ascii').split('\0')

    if {'flag'} & prop_types:
        if len(data):
            if fmt == 'flag':
                print('{prop}: boolean property with value {val}'.format(prop=p.name, val=data),
                      file=sys.stderr)
                return data
        else:
            return True

    val_int = list()
    #print(p.name, fmt,  bytes(p))
    try:
        type_struct = type_format[fmt.split('-', 1)[0]]
        for i in type_struct.iter_unpack(data):
            val_int += [dtschema.sized_int(i[0], size=(type_struct.size * 8))]
    except:
        print('{prop}: size ({len}) error for type {fmt}'.format(prop=p.name, len=len(p), fmt=fmt), file=sys.stderr)
        if len(p) == 4:
            type_struct = type_format['uint32']
        elif len(p) == 2:
            type_struct = type_format['uint16']
        elif len(p) == 1:
            type_struct = type_format['uint8']
        else:
            return data
        for i in type_struct.iter_unpack(data):
            val_int += [dtschema.sized_int(i[0], size=(type_struct.size * 8))]

    if 'matrix' in fmt or 'phandle-array' in fmt:
        dim = validator.property_get_type_dim(p.name)
    if dim:
        if max(dim[1]) and dim[1][0] == dim[1][1]:
            stride = dim[1][1]
        elif max(dim[0]) and dim[0][0] == dim[0][1]:
            stride, rem = divmod(len(val_int), dim[0][1])
            if rem:
                stride = len(val_int)
        else:
            # If multiple dimensions, check if one and only one dimension fits
            stride = get_stride(len(val_int), dim[1])
            #if stride == 0:
            #    stride = get_stride(len(val_int), dim[0])
            if stride == 0:
                stride = len(val_int)

        #print(p.name, dim, stride)
        return [val_int[i:i+stride] for i in range(0, len(val_int), stride)]

    return [val_int]


def node_props(validator, fdt, nodename, offset):
    props_dict = {}
    poffset = fdt.first_property_offset(offset, QUIET_NOTFOUND)
    while poffset >= 0:
        p = fdt.get_property_by_offset(poffset)
        props_dict[p.name] = prop_value(validator, nodename, p)

        poffset = fdt.next_property_offset(poffset, QUIET_NOTFOUND)

    return props_dict


phandles = {}
phandle_loc = []


def process_fixups(validator, fdt, nodename, offset):
    if nodename != '__fixups__':
        return
    props = node_props(validator, fdt, nodename, offset)
    global phandle_loc
    phandle_loc += [s for l in props.values() for s in l]


def process_local_fixups(fdt, nodename, path, offset):
    global phandle_loc

    if nodename:
        path += '/' + nodename

    poffset = fdt.first_property_offset(offset, QUIET_NOTFOUND)
    while poffset >= 0:
        p = fdt.get_property_by_offset(poffset)

        for i in type_format['uint32'].iter_unpack(bytes(p)):
            phandle_loc += [path + ':' + p.name + ':' + str(i[0])]

        poffset = fdt.next_property_offset(poffset, QUIET_NOTFOUND)

    offset = fdt.first_subnode(offset, QUIET_NOTFOUND)
    while offset >= 0:
        nodename = fdt.get_name(offset)
        process_local_fixups(fdt, nodename, path, offset)

        offset = fdt.next_subnode(offset, QUIET_NOTFOUND)


def fdt_scan_node(validator, fdt, nodename, offset):
    if nodename == '__fixups__':
        process_fixups(validator, fdt, nodename, offset)
        return
    if nodename == '__local_fixups__':
        process_local_fixups(fdt, '', '', offset)
        return
    if nodename.startswith('__'):
        return

    node_dict = node_props(validator, fdt, nodename, offset)
    if 'phandle' in node_dict:
        #print('phandle', node_dict['phandle'])
        phandles[node_dict['phandle'][0][0]] = node_dict

    offset = fdt.first_subnode(offset, QUIET_NOTFOUND)
    while offset >= 0:
        nodename = fdt.get_name(offset)
        node = fdt_scan_node(validator, fdt, nodename, offset)
        if node is not None:
            node_dict[nodename] = node

        offset = fdt.next_subnode(offset, QUIET_NOTFOUND)

    return node_dict


phandle_args = {
    # phandle+args properties with fixed arg size or which don't match standard
    # 'foos' and '#foo-cells' pattern
    'assigned-clocks': '#clock-cells',
    'assigned-clock-parents': '#clock-cells',
    'cooling-device': '#cooling-cells',
    'interrupts-extended': '#interrupt-cells',
    'interconnects': '#interconnect-cells',
    'mboxes': '#mbox-cells',
    'sound-dai': '#sound-dai-cells',
    'msi-parent': '#msi-cells',
    'msi-ranges': '#interrupt-cells',
    'gpio-ranges': 3,

    'memory-region': None,
}


def _get_cells_size(node, cellname):
    if isinstance(cellname, int):
        return cellname
    elif cellname in node:
        return node[cellname][0][0]
    else:
        return 0


def _check_is_phandle(prop_path, cell):
    path = prop_path + ':' + str(cell * 4)
    return path in phandle_loc


def _get_phandle_arg_size(prop_path, idx, cells, cellname):
    if not cells:
        return 0
    phandle = cells[0]
    if phandle == 0 or not cellname:
        return 1
    if phandle == 0xffffffff:
        # Use fixups data if available (examples)
        # Mixing unresolved and resolved phandles doesn't work
        if _check_is_phandle(prop_path, idx):
            cell_count = 1
            while (cell_count < len(cells)) and not _check_is_phandle(prop_path, idx + cell_count):
                cell_count += 1

            return cell_count
        else:
            return 0

    if phandle not in phandles:
        return 0

    node = phandles[phandle]

    return _get_cells_size(node, cellname) + 1


def fixup_phandles(validator, dt, path=''):
    for k, v in dt.items():
        if isinstance(v, dict):
            fixup_phandles(validator, v, path=path + '/' + k)
            continue
        elif not {'phandle-array'} & set(validator.property_get_type(k)):
            continue
        elif validator.property_has_fixed_dimensions(k):
            continue
        elif not isinstance(v, list) or (len(v) > 1 or not isinstance(v[0], list)):
            # Not a matrix or already split, nothing to do
            continue
        elif k in phandle_args:
            cellname = phandle_args[k]
        elif k.endswith('s') and 'gpio' not in k:
            cellname = '#' + k[:-1] + '-cells'
            #print(k, v)
            i = _get_phandle_arg_size(path + ':' + k, 0, v[0], cellname)
            if i == 0:
                continue
        else:
            continue

        i = 0
        dt[k] = []
        val = v[0]
        #print(k, v)
        while i < len(val):
            #print(k, v, file=sys.stderr)
            cells = _get_phandle_arg_size(path + ':' + k, i, val[i:], cellname)
            if cells == 0:
                #print(k, v)
                break

            # Special case for msi-ranges which has an additional span cell
            if k == 'msi-ranges':
                cells += 1

            # Special case for interconnects which is pairs of phandles+args
            if k == 'interconnects':
                cells += _get_phandle_arg_size(path + ':' + k, i + cells, val[i + cells:], cellname)

            dt[k] += [val[i:i + cells]]
            #print(k, dt[k], file=sys.stderr)

            i += cells


def fixup_gpios(dt):
    if 'gpio-hog' in dt:
        return
    for k, v in dt.items():
        if isinstance(v, dict):
            fixup_gpios(v)
        elif (k.endswith('-gpios') or k.endswith('-gpio') or k in {'gpio', 'gpios'}) and \
             not k.endswith(',nr-gpios') and \
             isinstance(v, list):
            i = 0
            dt[k] = []
            val = v[0]
            while i < len(val):
                phandle = val[i]
                if phandle == 0:
                    cells = 0
                elif phandle == 0xffffffff:
                    #print(val, file=sys.stderr)
                    try:
                        cells = val.index(0xffffffff, i + 1, -1)
                    except:
                        cells = len(val)
                    #print(cells, file=sys.stderr)
                    cells -= (i + 1)
                else:
                    #print(k, v, file=sys.stderr)
                    node = phandles[phandle]
                    cells = _get_cells_size(node, '#gpio-cells')

                dt[k] += [val[i:i + cells + 1]]

                i += cells + 1


def fixup_interrupts(dt, icells):
    if 'interrupt-parent' in dt and isinstance(dt['interrupt-parent'], list):
        phandle = dt['interrupt-parent'][0][0]
        if phandle == 0xffffffff:
            del dt['interrupt-parent']
        else:
            icells = _get_cells_size(phandles[phandle], '#interrupt-cells')

    for k, v in dt.items():
        if isinstance(v, dict):
            if '#interrupt-cells' in dt:
                icells = _get_cells_size(dt, '#interrupt-cells')
            fixup_interrupts(v, icells)
        elif k == 'interrupts' and not isinstance(v, bytes):
            i = 0
            dt[k] = []
            val = v[0]
            while i < len(val):
                dt[k] += [val[i:i + icells]]
                i += icells
        elif k == 'interrupt-map' and not isinstance(v, bytes):
            icells = _get_cells_size(dt, '#interrupt-cells')
            ac = _get_cells_size(dt, '#address-cells')
            i = 0
            dt[k] = []
            val = v[0]
            phandle = val[ac + icells]
            if phandle == 0xffffffff:
                # Assume uniform sizes (same interrupt provider)
                try:
                    cells = val.index(0xffffffff, ac + icells + 1) - (ac + icells)
                    while i < len(val):
                        dt[k] += [val[i:i + cells]]
                        i += cells
                except:
                    pass    # Only 1 entry, nothing to do
            else:
                while i < len(val):
                    p_icells = _get_cells_size(phandles[phandle], '#interrupt-cells')
                    if '#address-cells' in phandles[phandle]:
                        p_ac = _get_cells_size(phandles[phandle], '#address-cells')
                    else:
                        p_ac = 0

                    cells = ac + icells + 1 + p_ac + p_icells
                    dt[k] += [val[i:i + cells]]
                    i += cells


def fixup_addresses(dt, ac, sc):
    for k, v in dt.items():
        if isinstance(v, dict):
            if '#address-cells' in dt:
                ac = _get_cells_size(dt,'#address-cells')
            if '#size-cells' in dt:
                sc = _get_cells_size(dt, '#size-cells')
            fixup_addresses(v, ac, sc)
        elif k == 'reg':
            i = 0
            dt[k] = []
            val = v[0]
            while i < len(val):
                dt[k] += [val[i:i + ac + sc]]
                i += ac + sc
        elif k in {'ranges', 'dma-ranges'} and not isinstance(v, bool):
            child_cells = _get_cells_size(dt, '#address-cells')
            child_cells += _get_cells_size(dt, '#size-cells')
            i = 0
            dt[k] = []
            val = v[0]
            while i < len(val):
                dt[k] += [val[i:i + ac + child_cells]]
                i += ac + child_cells


def fdt_unflatten(validator, dtb):
    fdt = libfdt.Fdt(dtb)

    offset = fdt.first_subnode(-1, QUIET_NOTFOUND)
    dt = fdt_scan_node(validator, fdt, '/', offset)

    #print(phandle_loc)
    fixup_gpios(dt)
    fixup_interrupts(dt, 1)
    fixup_addresses(dt, 2, 1)
    fixup_phandles(validator, dt)

#    pprint.pprint(dt, compact=True)
    return dt
