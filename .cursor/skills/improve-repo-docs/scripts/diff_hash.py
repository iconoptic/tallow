#!/usr/bin/env python3
"""Hash the working tree for the docs ledger.

Prints the SHA-256 of `git diff HEAD` plus the bytes of untracked,
non-ignored files. `.cursor/docs-ledger.json` is excluded so writing
the ledger does not change the hash.

Usage:
  python3 .cursor/skills/improve-repo-docs/scripts/diff_hash.py
  python3 .cursor/skills/improve-repo-docs/scripts/diff_hash.py --json
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

LEDGER = ".cursor/docs-ledger.json"


def project_root() -> Path:
    env = os.environ.get("CURSOR_PROJECT_DIR")
    if env:
        return Path(env)
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        check=True,
        capture_output=True,
        text=True,
    )
    return Path(out.stdout.strip())


def git_bytes(root: Path, *args: str) -> bytes:
    out = subprocess.run(
        ["git", "-C", str(root), "-c", "core.quotepath=false", *args],
        check=True,
        capture_output=True,
    )
    return out.stdout


def file_bytes(root: Path, rel: str) -> bytes:
    path = root / rel
    if path.is_symlink():
        return os.readlink(path).encode()
    return path.read_bytes()


def working_tree(root: Path) -> tuple[str, bool]:
    head = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "--verify", "HEAD"],
        capture_output=True,
    )
    diff = git_bytes(root, "diff", "--no-ext-diff", "HEAD") if head.returncode == 0 else b""
    listed = git_bytes(root, "ls-files", "--others", "--exclude-standard", "-z")
    paths = sorted(
        p.decode()
        for p in listed.split(b"\0")
        if p and p.decode() != LEDGER
    )
    digest = hashlib.sha256()
    digest.update(b"diff\0")
    digest.update(diff)
    for rel in paths:
        digest.update(b"file\0")
        digest.update(rel.encode())
        digest.update(b"\0")
        digest.update(file_bytes(root, rel))
        digest.update(b"\0")
    return digest.hexdigest(), diff == b"" and not paths


def main(argv: list[str]) -> int:
    if argv not in ([], ["--json"]):
        print(
            "usage: diff_hash.py [--json]",
            file=sys.stderr,
        )
        return 2
    diff_hash, clean = working_tree(project_root())
    if argv == ["--json"]:
        json.dump({"diff_hash": diff_hash, "clean": clean}, sys.stdout)
        sys.stdout.write("\n")
    else:
        print(diff_hash)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except subprocess.CalledProcessError as exc:
        sys.stderr.write(exc.stderr.decode() if exc.stderr else "git failed\n")
        raise SystemExit(exc.returncode or 1) from exc
