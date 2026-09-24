#!/usr/bin/env python3
"""Catch the one mistake a scripted edit keeps making to Swift files.

A `"..."` literal in Swift cannot contain a real newline — only `\\n`. A
Python script that writes `"\\n"` without escaping produces a literal
newline inside the quotes, which:

  * still balances the quote count, so counting quotes finds nothing;
  * still balances the braces, so a brace check finds nothing;
  * fails only in the Swift compiler, which on this project means a CI
    run on a macOS runner, twenty minutes later.

That happened three times in one session. This finds it in a second.

    python scripts/dev/check-swift-strings.py [path ...]

Defaults to platforms/macos. Exit code 1 if anything is wrong.
"""
from __future__ import annotations

import sys
from pathlib import Path

QUOTE = '"'
BACKSLASH = chr(92)
DEFAULT_ROOT = Path(__file__).resolve().parents[2] / "platforms" / "macos"


def offences(src: str) -> list[tuple[int, str]]:
    """(line number, reason) for every unterminated single-line string."""
    out: list[tuple[int, str]] = []
    i = 0
    line = 1
    in_line_comment = False
    in_block_comment = False
    in_multiline = False       # \"\"\" ... \"\"\", where newlines ARE legal
    in_string = False
    escaped = False
    string_started_on = 0

    while i < len(src):
        c = src[i]
        three = src[i : i + 3]
        two = src[i : i + 2]

        if c == "\n":
            line += 1
            if in_line_comment:
                in_line_comment = False
            elif in_string and not in_multiline:
                out.append((string_started_on, "newline inside a \"...\" literal"))
                in_string = False
            i += 1
            continue

        if in_line_comment:
            i += 1
        elif in_block_comment:
            if two == "*/":
                in_block_comment = False
                i += 2
            else:
                i += 1
        elif in_multiline:
            if three == QUOTE * 3:
                in_multiline = False
                in_string = False
                i += 3
            else:
                i += 1
        elif in_string:
            if escaped:
                escaped = False
                i += 1
            elif c == BACKSLASH:
                escaped = True
                i += 1
            elif c == QUOTE:
                in_string = False
                i += 1
            else:
                i += 1
        else:
            if two == "//":
                in_line_comment = True
                i += 2
            elif two == "/*":
                in_block_comment = True
                i += 2
            elif three == QUOTE * 3:
                in_multiline = True
                in_string = True
                string_started_on = line
                i += 3
            elif c == QUOTE:
                in_string = True
                string_started_on = line
                i += 1
            else:
                i += 1

    if in_string:
        out.append((string_started_on, "string never closed before end of file"))
    return out


def main(argv: list[str]) -> int:
    roots = [Path(a) for a in argv[1:]] or [DEFAULT_ROOT]
    files: list[Path] = []
    for r in roots:
        files.extend([r] if r.is_file() else sorted(r.rglob("*.swift")))

    bad = 0
    for f in files:
        try:
            src = f.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            print(f"{f}: not UTF-8")
            bad += 1
            continue
        for line, why in offences(src):
            print(f"{f}:{line}: {why}")
            bad += 1

    print(f"checked {len(files)} file(s), {bad} problem(s)")
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
