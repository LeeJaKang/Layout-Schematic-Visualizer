#!/usr/bin/env python3
"""Write THIRD_PARTY_LICENSES.txt for every crate linked into the Windows, Linux and macOS builds.

Run from the repository root: python3 scripts/third_party_licenses.py [output]
Crate sources must be in the cargo registry (cargo fetch).
"""
import json
import os
import re
import subprocess
import sys

TARGETS = ['x86_64-pc-windows-gnu', 'x86_64-unknown-linux-gnu', 'aarch64-apple-darwin']
LICENSE_FILE = re.compile(r'(?i)^(licen[cs]e|copying|notice|unlicense|copyright)')
MIT = '''Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
'''


def linked_crates():
    crates = {}
    for t in TARGETS:
        m = json.loads(subprocess.run(['cargo', 'metadata', '--format-version', '1', '--filter-platform', t],
                                      check=True, capture_output=True, text=True).stdout)
        pkgs = {p['id']: p for p in m['packages']}
        nodes = {n['id']: n for n in m['resolve']['nodes']}
        root = m['resolve']['root']
        stack, seen = [root], set()
        while stack:
            i = stack.pop()
            if i in seen:
                continue
            seen.add(i)
            for d in nodes[i]['deps']:
                if any(k['kind'] in (None, 'normal') for k in d['dep_kinds']):
                    stack.append(d['pkg'])
        for i in seen - {root}:
            p = pkgs[i]
            crates[(p['name'], p['version'])] = p
    return [crates[k] for k in sorted(crates)]


def license_files(p):
    d = os.path.dirname(p['manifest_path'])
    files = sorted(os.path.join(d, f) for f in os.listdir(d) if LICENSE_FILE.match(f))
    fonts = os.path.join(d, 'fonts')
    if os.path.isdir(fonts):  # epaint_default_fonts keeps the font licenses here
        files += sorted(os.path.join(fonts, f) for f in os.listdir(fonts) if f.endswith('.txt'))
    return files


def find_text(crates, name_re, file_re):
    for p in crates:
        if re.match(name_re, p['name']):
            for f in license_files(p):
                if re.search(file_re, os.path.basename(f)):
                    return open(f, encoding='utf-8', errors='replace').read()
    sys.exit('no %s text found in %s' % (file_re, name_re))


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else 'THIRD_PARTY_LICENSES.txt'
    crates = linked_crates()
    std_needed = set()
    parts = ['Third-party software in the Layout-Schematic-Visualizer programs\n',
             '%d Rust crates are linked into the Windows, Linux and macOS builds.\n' % len(crates),
             'Each entry gives the license the crate is published under and its license files.\n'
             'For a crate with no license file, MIT is used when it is one of the choices.\n']
    for p in crates:
        lic = p.get('license') or 'see license file'
        parts.append('\n' + '=' * 78 + '\n%s %s\nLicense: %s\n' % (p['name'], p['version'], lic))
        if p.get('repository'):
            parts.append('Source: %s\n' % p['repository'])
        files = license_files(p)
        if files:
            for f in files:
                parts.append('\n--- %s ---\n' % os.path.relpath(f, os.path.dirname(p['manifest_path'])))
                parts.append(open(f, encoding='utf-8', errors='replace').read().rstrip() + '\n')
            continue
        authors = ', '.join(p.get('authors') or []) or 'the %s authors' % p['name']
        ids = re.findall(r'[A-Za-z0-9.\-]+', lic)
        if 'MIT' in ids:
            parts.append('\nThe published crate has no license file. MIT license:\n\nCopyright (c) %s\n\n%s' % (authors, MIT))
        else:
            pick = ids[0]
            std_needed.add(pick)
            parts.append('\nThe published crate has no license file. Copyright (c) %s.\n'
                         'Licensed under %s, text under "Standard license texts" below.\n' % (authors, pick))
    if std_needed:
        parts.append('\n' + '=' * 78 + '\nStandard license texts\n')
        sources = {'Apache-2.0': (r'^bitflags$', r'(?i)apache'), 'BSL-1.0': (r'^error-code$', r'(?i)licen')}
        for s in sorted(std_needed):
            if s not in sources:
                sys.exit('no source for the %s text' % s)
            parts.append('\n--- %s ---\n%s\n' % (s, find_text(crates, *sources[s]).rstrip()))
    with open(out, 'w', encoding='utf-8') as fh:
        fh.write(''.join(parts))
    print('%s: %d crates, %d without a license file, standard texts: %s'
          % (out, len(crates), sum(1 for p in crates if not license_files(p)), ', '.join(sorted(std_needed)) or 'none'))


main()
