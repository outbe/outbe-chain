#!/usr/bin/env python3
"""Enforce decomposition limits on added/modified Rust sources, including tests."""
import argparse
import os
from pathlib import Path
import re
import subprocess
import sys


def git(*args):
    return subprocess.check_output(["git", *args])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default=os.environ.get("CHANGE_BASE", "HEAD"))
    args = parser.parse_args()
    base = args.base
    if not base or re.fullmatch("0+", base):
        base = "HEAD^"
    root = Path(git("rev-parse", "--show-toplevel").decode().strip())
    os.chdir(root)
    paths = set(git("diff", "--name-only", "--diff-filter=ACMR", "-z", base).split(b"\0"))
    paths.update(git("ls-files", "--others", "--exclude-standard", "-z").split(b"\0"))
    failures = []
    for raw in sorted(paths):
        path = Path(os.fsdecode(raw))
        if path.suffix != ".rs" or not path.is_file():
            continue
        source = path.read_text(encoding="utf-8")
        lines, characters = len(source.splitlines()), len(source)
        if lines > 1000 or characters > 40_000:
            failures.append(f"{path}: {lines} lines, {characters} characters")
    if failures:
        print("Decompose by domain responsibility (max 1000 lines and 40000 characters):", file=sys.stderr)
        print("\n".join(failures), file=sys.stderr)
        return 1
    print("Changed Rust files satisfy the decomposition limits.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
