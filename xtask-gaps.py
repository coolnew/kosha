#!/usr/bin/env python3
"""Verify that no string the program draws carries whitespace nobody wrote.

A Rust string literal broken across source lines keeps the newline and every
space of the next line's indentation, unless the break is escaped with a
trailing backslash. The source reads perfectly either way; the difference shows
only when the text is drawn, as a break or a gap in the middle of a sentence.

Six of them were shipping: two in the confirmation dialog, one in the notice
about filesystem compression, one in a command-palette description, and the
paragraph in Settings that explains what "Record who owns each file" costs, in
both of the versions it has. None had ever been noticed, because noticing one
means scrolling a settings panel to the bottom and reading what is down there.

Two shapes are deliberate and skipped:

* A literal that opens or closes with a line break, or opens with the escaped
  break that starts a help text. Those are blocks, laid out on purpose, and
  their columns are the whole point of them. A sentence split by accident opens
  and closes on a word.
* A run of spaces ending at a `{`. That is a `println!` layout, padding a label
  out to the column its value starts in.

Run with no arguments; exits non-zero and names the offending files.
"""
import pathlib
import re
import sys

GAP = re.compile(r"\S {3,}[^{ ]")

# A backslash and the newline it swallows: the escape that makes a break safe.
CONTINUATION = chr(92) + chr(10)


def literals(text):
    """Every ordinary string literal in `text`, as `(line, body)` pairs.

    A hand-written walk rather than a regular expression. An escaped quote ends
    the obvious pattern early and a `//` inside a string starts a comment that
    is not there, and this check exists precisely because the exact contents of
    a string are what matter.

    Raw strings are skipped: they keep their own newlines on purpose.
    """
    found = []
    i, n, line = 0, len(text), 1
    while i < n:
        char = text[i]
        if char == "\n":
            line += 1
            i += 1
        elif text.startswith("//", i):
            while i < n and text[i] != "\n":
                i += 1
        elif text.startswith("/*", i):
            end = text.find("*/", i + 2)
            end = n if end < 0 else end + 2
            line += text.count("\n", i, end)
            i = end
        elif char == "r" and i + 1 < n and text[i + 1] in '"#':
            j = i + 1
            hashes = 0
            while j < n and text[j] == "#":
                hashes += 1
                j += 1
            close = '"' + "#" * hashes
            end = text.find(close, j + 1)
            end = n if end < 0 else end + len(close)
            line += text.count("\n", i, end)
            i = end
        elif char == "'":
            # A char literal, which may be a quote: `'\"'` would otherwise open
            # a string that swallows the rest of the file. A lifetime is the
            # same first character and is not one, so look for the closing
            # quote where a char literal would have to put it.
            if text[i + 1 : i + 2] == "\\":
                end = text.find("'", i + 2)
                i = i + 2 if end < 0 or end > i + 6 else end + 1
            elif text[i + 2 : i + 3] == "'":
                i += 3
            else:
                i += 1
        elif char == '"':
            start, j, body = line, i + 1, []
            while j < n and text[j] != '"':
                if text[j] == "\\":
                    body.append(text[j : j + 2])
                    if text[j + 1 : j + 2] == "\n":
                        line += 1
                    j += 2
                    continue
                if text[j] == "\n":
                    line += 1
                body.append(text[j])
                j += 1
            found.append((start, "".join(body)))
            i = j + 1
        else:
            i += 1
    return found


def main() -> int:
    bad = []
    for path in sorted(pathlib.Path("crates").rglob("*.rs")):
        text = path.read_bytes().decode("utf-8")
        for line, body in literals(text):
            plain = body.replace(CONTINUATION, "")
            # A block of text, laid out on purpose: it opens with a break, or
            # ends with one, or opens with the escape that starts a help text.
            if (
                body.startswith(CONTINUATION)
                or plain.startswith(chr(10))
                or plain.endswith(chr(10))
            ):
                continue
            where = f"{path.as_posix()}:{line}"
            # Anywhere else an escaped newline is the fix and a bare one is the
            # bug: it takes the next line's indentation into the sentence.
            if chr(10) in plain:
                bad.append(f"{where}: a line break, and the next line's indent with it")
                continue
            for gap in GAP.finditer(body):
                run = len(gap.group(0)) - 2
                bad.append(
                    f"{where}: {gap.group(0)[0]} <{run} spaces> {gap.group(0)[-1]}"
                )

    if bad:
        print("These strings will be drawn with whitespace nobody wrote:\n")
        for one in bad:
            print(f"  {one}")
        print(
            "\nEnd the source line with a backslash to continue a string without "
            "keeping the break and the indentation after it."
        )
        return 1

    print("No string literal carries stray whitespace.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
