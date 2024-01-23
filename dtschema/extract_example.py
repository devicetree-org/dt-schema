#!/usr/bin/env python3
# SPDX-License-Identifier: BSD-2-Clause
# Copyright 2018 Linaro Ltd.
# Copyright 2019-2022 Arm Ltd.

import re
import sys
import argparse
import signal

import ruamel.yaml


def sigint_handler(signum, frame):
    sys.exit(-2)


signal.signal(signal.SIGINT, sigint_handler)


interrupt_template = """
        interrupt-parent = <&fake_intc{index}>;
        fake_intc{index}: fake-interrupt-controller {{
            interrupt-controller;
            #interrupt-cells = < {int_cells} >;
        }};
"""

example_template = """
    example-{example_num} {{
        #address-cells = <1>;
        #size-cells = <1>;

        {interrupt}

        {example}
    }};
}};
"""

example_header = """
/dts-v1/;
/plugin/; // silence any missing phandle references
"""
example_start = """
/{
    compatible = "foo";
    model = "foo";
    #address-cells = <1>;
    #size-cells = <1>;

"""

yaml = ruamel.yaml.YAML(typ='safe')


def main():
    ex = '// empty'
    ap = argparse.ArgumentParser()
    ap.add_argument("yamlfile", type=str,
                    help="Filename of YAML encoded schema input file")
    args = ap.parse_args()

    try:
        binding = yaml.load(open(args.yamlfile, encoding='utf-8').read())
        if not isinstance(binding, dict):
            exit(0)
    except ruamel.yaml.YAMLError as exc:
        print(args.yamlfile + ":" + str(exc.problem_mark.line + 1) + ":" +
              str(exc.problem_mark.column + 1) + ":", exc.problem, file=sys.stderr)
        exit(1)

    example_dts = example_header

    if 'examples' in binding.keys():
        for idx, ex in enumerate(binding['examples']):
            # Check if example contains a root node "/{"
            root_node = re.search(r'/\s*{', ex)

            if not root_node:
                try:
                    int_val = re.search(r'\sinterrupts\s*=\s*<([0-9a-zA-Z |()_]+)>', ex).group(1)
                    int_val = re.sub(r'\(.+|\)', r'0', int_val)
                    int_cells = len(int_val.strip().split())
                except:
                    int_cells = 0
                example_dts += example_start
                ex = '        '.join(ex.splitlines(True))
                if int_cells > 0:
                    int_props = interrupt_template.format(int_cells=int_cells, index=idx)
                else:
                    int_props = ""
                example_dts += example_template.format(example=ex, example_num=idx, interrupt=int_props)
            else:
                example_dts += ex
    else:
        example_dts += example_start
        example_dts += "\n};"

    print(example_dts)
