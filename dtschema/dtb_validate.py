#!/usr/bin/env python3
# SPDX-License-Identifier: BSD-2-Clause
# Copyright 2018 Linaro Ltd.
# Copyright 2018 Arm Ltd.

import sys
import os
import argparse
import glob

import dtschema

verbose = False
show_unmatched = False
match_schema_file = None
compatible_match = False


class schema_group():
    failure = False

    def __init__(self, schema_file=""):
        if schema_file != "" and not os.path.exists(schema_file):
            exit(-1)

        self.validator = dtschema.DTValidator([schema_file])

    def check_node(self, tree, node, disabled, nodename, fullname, filename):
        # Hack to save some time validating examples
        if 'example-0' in node or 'example-' in nodename:
            return

        node['$nodename'] = [nodename]

        try:
            for error in self.validator.iter_errors(node, filter=match_schema_file,
                                                    compatible_match=compatible_match):

                # Disabled nodes might not have all the required
                # properties filled in, such as a regulator or a
                # GPIO meant to be filled at the DTS level on
                # boards using that particular node. Thus, if the
                # node is marked as disabled, let's just ignore
                # any error message reporting a missing property.
                if disabled or (isinstance(error.instance, dict) and
                   'status' in error.instance and
                   'disabled' in error.instance['status']):

                    if {'required', 'unevaluatedProperties'} & set(error.schema_path):
                        continue
                    elif error.context:
                        found = False
                        for e in error.context:
                            if {'required', 'unevaluatedProperties'} & set(e.schema_path):
                                found = True
                                break
                        if found:
                            continue

                if error.schema_file == 'generated-compatibles':
                    if not show_unmatched:
                        continue
                    print(f"{filename}: {fullname}: failed to match any schema with compatible: {node['compatible']}",
                          file=sys.stderr)
                    self.failure = True
                    continue

                print(dtschema.format_error(filename, error, nodename=nodename, verbose=verbose),
                    file=sys.stderr)
                self.failure = True
        except RecursionError as e:
            print(os.path.basename(sys.argv[0]) + ": recursion error: Check for prior errors in a referenced schema", file=sys.stderr)
            self.failure = True

    def check_subtree(self, tree, subtree, disabled, nodename, fullname, filename):
        if nodename.startswith('__'):
            return

        try:
            disabled = ('disabled' in subtree['status'])
        except:
            pass

        self.check_node(tree, subtree, disabled, nodename, fullname, filename)
        if fullname != "/":
            fullname += "/"
        for name, value in subtree.items():
            if isinstance(value, dict):
                self.check_subtree(tree, value, disabled, name, fullname + name, filename)

    def check_dtb(self, filename):
        """Check the given DT against all schemas"""
        with open(filename, 'rb') as f:
            dt = self.validator.decode_dtb(f.read())
        for subtree in dt:
            self.check_subtree(dt, subtree, False, "/", "/", filename)


def main():
    global verbose
    global show_unmatched
    global match_schema_file
    global compatible_match

    ap = argparse.ArgumentParser(fromfile_prefix_chars='@',
        epilog='Arguments can also be passed in a file prefixed with a "@" character.')
    ap.add_argument("dtbs", nargs='*',
                    help="Filename or directory of devicetree DTB input file(s)")
    ap.add_argument('-s', '--schema', help="preparsed schema file or path to schema files")
    ap.add_argument('-p', '--preparse', help="preparsed schema file (deprecated, use '-s')")
    ap.add_argument('-l', '--limit', help="limit validation to schemas with $id matching LIMIT substring. " \
                    "Multiple substrings separated by ':' can be listed (e.g. foo:bar:baz).")
    ap.add_argument('-c', '--compatible-match', action="store_true",
                    help="limit validation to schema matching nodes' most specific compatible string")
    ap.add_argument('-m', '--show-unmatched',
        help="Print out node 'compatible' strings which don't match any schema.",
        action="store_true")
    ap.add_argument('-n', '--line-number', help="Obsolete", action="store_true")
    ap.add_argument('-v', '--verbose', help="verbose mode", action="store_true")
    ap.add_argument('-u', '--url-path', help="Additional search path for references (deprecated)")
    ap.add_argument('-V', '--version', help="Print version number",
                    action="version", version=dtschema.__version__)
    args = ap.parse_args()

    verbose = args.verbose
    show_unmatched = args.show_unmatched
    if args.limit:
        match_schema_file = args.limit.split(':')
    compatible_match = args.compatible_match

    # Maintain prior behaviour which accepted file paths by stripping the file path
    if args.url_path and args.limit:
        for i,match in enumerate(match_schema_file):
            for d in args.url_path.split(os.path.sep):
                if d and match.startswith(d):
                    match = match[(len(d) + 1):]
            match_schema_file[i] = match

    if args.preparse:
        sg = schema_group(args.preparse)
    elif args.schema:
        sg = schema_group(args.schema)
    else:
        sg = schema_group()

    for d in args.dtbs:
        if not os.path.isdir(d):
            continue
        for filename in glob.iglob(d + "/**/*.dtb", recursive=True):
            if verbose:
                print("Check:  " + filename)
            sg.check_dtb(filename)

    for filename in args.dtbs:
        if not os.path.isfile(filename):
            continue
        if verbose:
            print("Check:  " + filename)
        sg.check_dtb(filename)

    if sg.failure:
        sys.exit(1)
