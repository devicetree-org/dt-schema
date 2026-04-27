#!/usr/bin/env python3
# SPDX-License-Identifier: BSD-2-Clause
# Copyright 2018 Linaro Ltd.
# Copyright 2018 Arm Ltd.

import sys
import os
import argparse
import glob
import hashlib
import json
import tempfile

import dtschema

verbose = False
show_unmatched = False
match_schema_file = None
compatible_match = False
CACHE_VERSION = 1
CACHE_DTB_FILENAME = '$dtb'


def _sha256_file(filename):
    h = hashlib.sha256()
    with open(filename, 'rb') as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def _display_path(filename):
    abspath = os.path.abspath(filename)
    relpath = os.path.relpath(abspath)

    if relpath == os.pardir or relpath.startswith(os.pardir + os.path.sep):
        return abspath
    return relpath


def _replace_filename_prefix(text, old, new):
    if text.startswith(old + ':'):
        return new + text[len(old):]
    return text


def _format_error(filename, error, nodename=None, compatible=None, verbose=False):
    text = dtschema.format_error(filename, error, nodename=nodename,
                                 compatible=compatible, verbose=verbose)
    return _replace_filename_prefix(text, os.path.abspath(filename),
                                    _display_path(filename))


def _error_path(path):
    return [str(p) if not isinstance(p, int) else p for p in path]


def _error_line_column(error):
    linecol = getattr(error, 'linecol', (-1, -1))
    if linecol[0] < 0:
        return None, None
    return linecol[0] + 1, linecol[1] + 1


def _error_context(error):
    context = {
        'message': error.message,
        'property_path': _error_path(error.absolute_path),
        'schema_path': _error_path(error.absolute_schema_path),
        'schema': getattr(error, 'schema_file', None),
    }

    note = getattr(error, 'note', None)
    if note:
        context['note'] = note

    return context


def _error_diagnostic(filename, error, nodename=None, fullname=None, compatible=None,
                      formatted=None):
    line, column = _error_line_column(error)
    diagnostic = {
        'type': 'validation',
        'level': 'error',
        'file': _display_path(filename),
        'line': line,
        'column': column,
        'node': fullname,
        'nodename': nodename,
        'compatible': compatible,
        'property_path': _error_path(error.absolute_path),
        'schema_path': _error_path(error.absolute_schema_path),
        'schema': getattr(error, 'schema_file', None),
        'message': error.message,
    }

    if formatted is not None:
        diagnostic['formatted'] = formatted

    note = getattr(error, 'note', None)
    if note:
        diagnostic['note'] = note

    if error.context:
        diagnostic['context'] = [_error_context(suberror)
                                 for suberror in sorted(error.context, key=lambda e: e.path)]

    return diagnostic


def _unmatched_diagnostic(filename, fullname, node):
    message = f"failed to match any schema with compatible: {node['compatible']}"
    return {
        'type': 'unmatched',
        'level': 'warning',
        'file': _display_path(filename),
        'line': None,
        'column': None,
        'node': fullname,
        'nodename': os.path.basename(fullname) or fullname,
        'compatible': node['compatible'],
        'message': message,
    }


def _diagnostic_text(diagnostic):
    if 'formatted' in diagnostic:
        return diagnostic['formatted']
    if diagnostic['type'] == 'unmatched':
        return (f"{diagnostic['file']}: {diagnostic['node']}: "
                f"{diagnostic['message']}")
    if diagnostic['type'] == 'recursion-error':
        return os.path.basename(sys.argv[0]) + ": " + diagnostic['message']
    return diagnostic['message']


def _emit_diagnostics(diagnostics, output_format):
    if output_format == 'json':
        return
    for diagnostic in diagnostics:
        print(_diagnostic_text(diagnostic), file=sys.stderr)


def _map_diagnostic_filename(value, old, new):
    if isinstance(value, list):
        return [_map_diagnostic_filename(v, old, new) for v in value]
    if not isinstance(value, dict):
        return value

    mapped = {}
    for key, val in value.items():
        if key == 'file' and val == old:
            mapped[key] = new
        elif key == 'formatted' and isinstance(val, str):
            mapped[key] = _replace_filename_prefix(val, old, new)
        else:
            mapped[key] = _map_diagnostic_filename(val, old, new)
    return mapped


class validation_cache():
    def __init__(self, cache_dir, schema_file, options):
        self.cache_dir = cache_dir
        self.schema_hash = _sha256_file(schema_file) if schema_file else None
        self.options = options

    def _cache_key(self, filename):
        key = {
            'cache_version': CACHE_VERSION,
            'dtschema_version': dtschema.__version__,
            'dtb_hash': _sha256_file(filename),
            'schema_hash': self.schema_hash,
            'options': self.options,
        }
        data = json.dumps(key, sort_keys=True, separators=(',', ':'))
        return hashlib.sha256(data.encode('utf-8')).hexdigest()

    def _cache_file(self, key):
        return os.path.join(self.cache_dir, key + '.json')

    def load(self, filename):
        cache_file = self._cache_file(self._cache_key(filename))
        try:
            with open(cache_file, 'r', encoding='utf-8') as f:
                diagnostics = json.load(f)['diagnostics']
                return _map_diagnostic_filename(diagnostics, CACHE_DTB_FILENAME,
                                                _display_path(filename))
        except (FileNotFoundError, KeyError, json.JSONDecodeError, OSError):
            return None

    def store(self, filename, diagnostics):
        os.makedirs(self.cache_dir, exist_ok=True)
        cache_file = self._cache_file(self._cache_key(filename))
        data = {
            'cache_version': CACHE_VERSION,
            'diagnostics': _map_diagnostic_filename(diagnostics,
                                                    _display_path(filename),
                                                    CACHE_DTB_FILENAME),
        }
        fd, tmp = tempfile.mkstemp(prefix='.dt-validate-', suffix='.json',
                                  dir=self.cache_dir, text=True)
        try:
            with os.fdopen(fd, 'w', encoding='utf-8') as f:
                json.dump(data, f, indent=2)
                f.write('\n')
            os.replace(tmp, cache_file)
        except Exception:
            try:
                os.unlink(tmp)
            except OSError:
                pass


class schema_group():
    def __init__(self, schema_file="", output_format="text"):
        if schema_file != "" and not os.path.exists(schema_file):
            exit(-1)

        self.validator = dtschema.DTValidator([schema_file])
        self.output_format = output_format
        self.diagnostics = []

    def emit_diagnostic(self, diagnostic, text=None):
        if text is not None and 'formatted' not in diagnostic:
            diagnostic['formatted'] = text
        self.diagnostics.append(diagnostic)
        if self.output_format != 'json':
            print(text if text is not None else diagnostic['message'], file=sys.stderr)

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
                    text = (f"{_display_path(filename)}: {fullname}: failed to match "
                            f"any schema with compatible: {node['compatible']}")
                    self.emit_diagnostic(
                        _unmatched_diagnostic(filename, fullname, node),
                        text)
                    continue

                if 'compatible' in node:
                    compat = node['compatible'][0]
                else:
                    compat = None
                text = _format_error(filename, error, nodename=nodename,
                                     compatible=compat, verbose=verbose)
                self.emit_diagnostic(
                    _error_diagnostic(filename, error, nodename=nodename, fullname=fullname,
                                      compatible=compat, formatted=text),
                    text)
        except RecursionError as e:
            self.emit_diagnostic({
                'type': 'recursion-error',
                'level': 'error',
                'file': _display_path(filename),
                'line': None,
                'column': None,
                'node': fullname,
                'nodename': nodename,
                'message': 'recursion error: Check for prior errors in a referenced schema',
            }, os.path.basename(sys.argv[0]) +
                ": recursion error: Check for prior errors in a referenced schema")

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
        self.diagnostics = []
        with open(filename, 'rb') as f:
            dt = self.validator.decode_dtb(f.read())
        for subtree in dt:
            self.check_subtree(dt, subtree, False, "/", "/", filename)
        return self.diagnostics


def _dtb_filenames(dtbs):
    for d in dtbs:
        if not os.path.isdir(d):
            continue
        for filename in glob.iglob(d + "/**/*.dtb", recursive=True):
            yield filename

    for filename in dtbs:
        if os.path.isfile(filename):
            yield filename


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
    ap.add_argument('--format', choices=['text', 'json'], default='text',
                    help="diagnostic output format")
    ap.add_argument('--cache-dir',
                    help="cache validation diagnostics in CACHE_DIR")
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

    schema_file = None
    if args.preparse:
        schema_file = args.preparse
    elif args.schema:
        schema_file = args.schema

    cache = None
    if args.cache_dir:
        if schema_file is None or not os.path.isfile(schema_file):
            print("--cache-dir requires a schema file", file=sys.stderr)
            exit(-1)
        cache_options = {
            'compatible_match': compatible_match,
            'limit': match_schema_file,
            'show_unmatched': show_unmatched,
            'verbose': verbose,
        }
        cache = validation_cache(args.cache_dir, schema_file, cache_options)

    sg = None
    if args.preparse and not cache:
        sg = schema_group(args.preparse, args.format)
    elif args.schema and not cache:
        sg = schema_group(args.schema, args.format)
    elif not cache:
        sg = schema_group(output_format=args.format)

    verbose_file = sys.stderr if args.format == 'json' else sys.stdout
    diagnostics = []

    for filename in _dtb_filenames(args.dtbs):
        if verbose:
            print("Check:  " + filename, file=verbose_file)

        cached = cache.load(filename) if cache else None
        if cached is not None:
            diagnostics += cached
            _emit_diagnostics(cached, args.format)
            continue

        if sg is None:
            if args.preparse:
                sg = schema_group(args.preparse, args.format)
            elif args.schema:
                sg = schema_group(args.schema, args.format)
            else:
                sg = schema_group(output_format=args.format)

        dtb_diagnostics = sg.check_dtb(filename)
        diagnostics += dtb_diagnostics
        if cache:
            cache.store(filename, dtb_diagnostics)

    if args.format == 'json':
        print(json.dumps(diagnostics, indent=2))
