"""Parse an Xcode project.pbxproj the way Xcode does, and fail loudly if it cannot.

`xcodebuild` reports exactly one thing when the file is malformed -- "The
project 'X' is damaged and cannot be opened due to a parse error" -- with no
line, no offset and no offending token, and it only says it on a Mac. Editing
the file from Windows therefore means a CI round trip per guess.

pbxproj is an OpenStep (old-style ASCII) plist, which `plistlib` does not read,
so the tokenizer below is the whole point: it knows which characters may appear
in an UNQUOTED string. That is the trap this script exists for -- a path
containing '+', '-' or a space parses as a name, then an operator, and the
dictionary never closes. `DimmyCore+License.swift` is quoted in this project for
that reason; `DimmyCore+Confluence.swift` was added without quotes on
2026-09-11 and took down the whole macOS build.

Usage: python scripts/dev/check-pbxproj.py platforms/macos/Dimmy.xcodeproj/project.pbxproj
"""

import sys, re

UNQUOTED = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_$./")

def tokenize(s):
    i, n, out = 0, len(s), []
    while i < n:
        c = s[i]
        if c in " \t\r\n":
            i += 1; continue
        if s.startswith("//", i):
            i = s.find("\n", i);  i = n if i < 0 else i;  continue
        if s.startswith("/*", i):
            j = s.find("*/", i + 2)
            if j < 0: raise SyntaxError("comment never closed at %d" % i)
            i = j + 2; continue
        if c in "{}()=;,":
            out.append((c, i)); i += 1; continue
        if c == '"':
            j = i + 1
            while j < n:
                if s[j] == "\\": j += 2; continue
                if s[j] == '"': break
                j += 1
            if j >= n: raise SyntaxError("string never closed at %d" % i)
            out.append(("S", i)); i = j + 1; continue
        j = i
        while j < n and s[j] in UNQUOTED: j += 1
        if j == i:
            line = s.count("\n", 0, i) + 1
            raise SyntaxError("char %r is not legal unquoted (line %d): %r"
                              % (c, line, s[max(0,i-70):i+40]))
        out.append(("S", i)); i = j; continue
    return out

def parse(tok, k, s):
    """value -> new index"""
    t, off = tok[k]
    if t == "S": return k + 1
    if t == "{":
        k += 1
        while tok[k][0] != "}":
            if tok[k][0] != "S": raise SyntaxError("key expected at offset %d" % tok[k][1])
            k += 1
            if tok[k][0] != "=": raise SyntaxError("'=' expected at offset %d" % tok[k][1])
            k = parse(tok, k + 1, s)
            if tok[k][0] != ";": raise SyntaxError("';' expected at offset %d" % tok[k][1])
            k += 1
        return k + 1
    if t == "(":
        k += 1
        while tok[k][0] != ")":
            k = parse(tok, k, s)
            if tok[k][0] == ",": k += 1
        return k + 1
    raise SyntaxError("unexpected token %r at offset %d" % (t, off))

raw = open(sys.argv[1], "rb").read().decode("utf-8")
if raw.startswith("// !$*UTF8*$!"): raw = raw.split("\n", 1)[1]
tok = tokenize(raw)
end = parse(tok, 0, raw)
if end != len(tok): raise SyntaxError("trailing tokens after the root dictionary")
print("pbxproj OK -- %d token, root dictionary closed" % len(tok))
