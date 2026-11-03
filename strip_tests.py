#!/usr/bin/env python3
"""Retire les modules de tests inline (`#[cfg(test)] mod … { … }`) des fichiers Rust.

Usage : strip_tests.py <dossier>   (modifie les .rs en place : à lancer sur une COPIE)
Le code de production n'est pas touché ; chaque bloc retiré laisse un marqueur d'une ligne.
"""
import re
import sys
from pathlib import Path

START = re.compile(
    r'^[ \t]*#\[cfg\(test\)\][ \t]*\n(?:[ \t]*#\[[^\n]*\][ \t]*\n)*[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?mod[ \t]+\w+[ \t]*(\{|;)',
    re.M,
)


def match_brace(s: str, i: int) -> int:
    """Index suivant l'accolade fermante qui correspond à `{` en s[i], ou -1."""
    depth, n = 0, len(s)
    while i < n:
        c = s[i]
        if s.startswith('//', i):
            j = s.find('\n', i)
            i = n if j < 0 else j
        elif s.startswith('/*', i):
            d, i = 1, i + 2
            while i < n and d:
                if s.startswith('/*', i):
                    d, i = d + 1, i + 2
                elif s.startswith('*/', i):
                    d, i = d - 1, i + 2
                else:
                    i += 1
        elif c == 'r' and re.match(r'r#*"', s[i:i + 40]) and (i == 0 or not (s[i - 1].isalnum() or s[i - 1] == '_')):
            m = re.match(r'r(#*)"', s[i:])
            end = s.find('"' + m.group(1), i + m.end())
            i = n if end < 0 else end + 1 + len(m.group(1))
        elif c == '"':
            i += 1
            while i < n and s[i] != '"':
                i += 2 if s[i] == '\\' else 1
            i += 1
        elif c == "'":
            if s[i + 1:i + 2] == '\\':
                end = s.find("'", i + 3)
                i = n if end < 0 else end + 1
            elif s[i + 2:i + 3] == "'":
                i += 3
            else:  # durée de vie
                i += 1
        elif c == '{':
            depth, i = depth + 1, i + 1
        elif c == '}':
            depth, i = depth - 1, i + 1
            if depth == 0:
                return i
        else:
            i += 1
    return -1


def strip(s: str) -> str:
    out, pos = [], 0
    while True:
        m = START.search(s, pos)
        if not m:
            out.append(s[pos:])
            return ''.join(out)
        end = m.end() if m.group(1) == ';' else match_brace(s, m.end() - 1)
        if end < 0:  # structure inattendue : on ne touche à rien
            out.append(s[pos:])
            return ''.join(out)
        out.append(s[pos:m.start()])
        out.append('// [module de tests inline retiré]\n')
        pos = end
        if s[pos:pos + 1] == '\n':
            pos += 1


def main() -> None:
    root = Path(sys.argv[1])
    for f in root.rglob('*.rs'):
        s = f.read_text(encoding='utf-8')
        t = strip(s)
        if t != s:
            f.write_text(t, encoding='utf-8')


if __name__ == '__main__':
    main()
