#!/usr/bin/env python3
"""Ask for a docs pass when the agent stops on an unreconciled tree.

Cursor calls this on the stop event. Stdin is JSON with status and
loop_count. Stdout is JSON: a followup_message, or {}.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

FOLLOWUP = (
    "The working tree differs from .cursor/docs-ledger.json. "
    "Before any other work, launch one generalPurpose subagent and do not edit documentation yourself. "
    "Tell the subagent to read and follow .cursor/skills/improve-repo-docs/SKILL.md in working-tree mode. "
    "The subagent runs python3 .cursor/skills/improve-repo-docs/scripts/diff_hash.py and writes .cursor/docs-ledger.json last. "
    "If this session cannot edit files, stop now and do not write the ledger. "
    "After the subagent finishes, summarize what it changed and stop."
)


def emit(payload: dict) -> None:
    json.dump(payload, sys.stdout)
    sys.stdout.write("\n")


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


def tree_state(root: Path) -> dict:
    script = root / ".cursor/skills/improve-repo-docs/scripts/diff_hash.py"
    out = subprocess.run(
        [sys.executable, str(script), "--json"],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(out.stdout)


def ledger_hash(root: Path) -> str | None:
    path = root / ".cursor/docs-ledger.json"
    if not path.is_file():
        return None
    try:
        data = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return None
    value = data.get("diff_hash")
    return value if isinstance(value, str) else None


def main() -> int:
    raw = sys.stdin.read()
    try:
        payload = json.loads(raw) if raw.strip() else {}
    except json.JSONDecodeError:
        emit({})
        return 0
    if payload.get("status") != "completed":
        emit({})
        return 0
    root = project_root()
    state = tree_state(root)
    if state.get("clean") is True or state.get("diff_hash") == ledger_hash(root):
        emit({})
        return 0
    emit({"followup_message": FOLLOWUP})
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        sys.stderr.write(f"docs-before-stop: {exc}\n")
        emit({})
        raise SystemExit(0) from exc
