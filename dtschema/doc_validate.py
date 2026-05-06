#!/usr/bin/env python3
# SPDX-License-Identifier: BSD-2-Clause
# Copyright 2018 Linaro Ltd.
# Copyright 2018 Arm Ltd.


import os
import sys
import argparse
import glob

import ruamel.yaml
import dtschema

line_number = False
verbose = False


def check_doc(filename):
    ret = 0
    try:
        dtsch = dtschema.DTSchema(filename, line_numbers=line_number)
    except ruamel.yaml.YAMLError as exc:
        print(filename + ":" + str(exc.problem_mark.line + 1) + ":" +
              str(exc.problem_mark.column + 1) + ":", exc.problem, file=sys.stderr)
        return 1

    try:
        for error in sorted(dtsch.iter_errors(), key=lambda e: e.linecol):
            sys.stderr.write(dtschema.format_error(filename, error, verbose=verbose) + "\n")
            ret = 1
    except:
        raise
        print(filename + ': error checking schema file', file=sys.stderr)
        return 1

    dtsch.check_schema_refs()

    return ret


def main():
    global verbose
    global line_number

    ap = argparse.ArgumentParser(fromfile_prefix_chars='@',
        epilog='Arguments can also be passed in a file prefixed with a "@" character.')
    ap.add_argument("yamldt", nargs='*', type=str,
                    help="Directory or filename of YAML encoded devicetree schema file")
    ap.add_argument('-v', '--verbose', help="verbose mode", action="store_true")
    ap.add_argument('-n', '--line-number', help="Print line and column numbers (slower)", action="store_true")
    ap.add_argument('-u', '--url-path', help="Additional search path for references")
    ap.add_argument('-V', '--version', help="Print version number",
                    action="version", version=dtschema.__version__)
    args = ap.parse_args()

    line_number = args.line_number
    verbose = args.verbose

    ret = 0
    for f in args.yamldt:
        if os.path.isdir(f):
            for filename in glob.iglob(f + "/**/*.yaml", recursive=True):
                ret |= check_doc(filename)
        else:
            ret |= check_doc(f)

    exit(ret)
