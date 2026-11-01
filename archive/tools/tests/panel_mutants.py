#!/usr/bin/env python3
"""Mutation tests for the LAN panel contract.

Copies the panel into a temp directory, applies one mutation at a time, and
runs tools/tests/panel_contract.py there. The unmutated copy runs first and has
to pass. Exits non-zero if a mutation is missed, if its source text is no longer
in the tree, or if a run ends in anything but a contract FAIL line. A crash in
the contract proves nothing about the panel, so it is an error, not a catch.

    .venv/bin/python tools/tests/mutants/panel_mutants.py
    .venv/bin/python tools/tests/mutants/panel_mutants.py \\
        --panel-command 'host/target/release/tallowd panel --preview'

Every mutant here edits the Python panel, so none of them can reach tallowd.
With --panel-command this script applies no mutants: it runs the contract once
against that command from the repo root and reports that result alone. The Rust
panel has no mutants of its own.
"""

from __future__ import annotations

import argparse
import shlex
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import NamedTuple

REPO = Path(__file__).resolve().parents[3]


class Mutant(NamedTuple):
    name: str
    path: str
    old: str
    new: str


MUTANTS = [
    Mutant(
        "cookie presence only",
        "app/agents.py",
        "if cookie and hmac.compare_digest(cookie.encode(), browser_token.encode()):",
        "if cookie:",
    ),
    Mutant(
        "cookie prefix",
        "app/agents.py",
        "if cookie and hmac.compare_digest(cookie.encode(), browser_token.encode()):",
        "if cookie and browser_token.startswith(cookie):",
    ),
    Mutant(
        "any bearer",
        "app/agents.py",
        "    agent = match_agent(agents, secret)\n    if agent is None:\n        return None\n",
        "    agent = agents[-1]\n",
    ),
    Mutant(
        "no scope check",
        "app/h7020_panel.py",
        '        if scope not in scopes:\n            raise HTTPException(status_code=403, detail="missing scope")\n',
        "",
    ),
    Mutant(
        "no audit",
        "app/h7020_panel.py",
        "            catalog.audit(actor, request.method, request.url.path)\n",
        "            pass\n",
    ),
    Mutant(
        "no rate limit",
        "app/h7020_panel.py",
        '        if not limiter.allow():\n            raise HTTPException(status_code=429, detail="slow down")\n',
        "",
    ),
    Mutant(
        "socket left open",
        "app/h7020_panel.py",
        "                sock_path.chmod(0o600)\n",
        "                pass\n",
    ),
    Mutant(
        "no cam address guard",
        "app/cams.py",
        '        if address not in IOT_NET or address in (IOT_NET.network_address, IOT_NET.broadcast_address, AP_ADDR):\n'
        '            raise ValueError(f"{where}: {cam_id} must be a node on {IOT_NET}")\n',
        "",
    ),
    Mutant(
        "no title escape",
        "app/h7020_panel.py",
        'source.replace("__CAMS_TITLE__", html.escape(book.title()))',
        'source.replace("__CAMS_TITLE__", book.title())',
    ),
    Mutant(
        "no net fill",
        "app/h7020_panel.py",
        '.replace(\n            "__IOT_NET__", html.escape(str(camlib.IOT_NET))\n        )',
        "",
    ),
    Mutant(
        "login prefix",
        "app/h7020_panel.py",
        "if not hmac.compare_digest(password_form.encode(), password.encode()):",
        "if not password.startswith(password_form):",
    ),
    Mutant(
        "sequence never plays",
        "app/show.py",
        "        self._task = asyncio.create_task(self._run_sequence(pattern, generation))\n",
        "",
    ),
    Mutant(
        "scheduler off",
        "app/h7020_panel.py",
        "        stage.start()\n",
        "",
    ),
    Mutant(
        "cam name renamed",
        "app/h7020_panel.py",
        '{"id": cam.id, "name": cam.name, "address": cam.address}',
        '{"id": cam.id, "label": cam.name, "address": cam.address}',
    ),
    Mutant(
        "path back in the error",
        "app/cams.py",
        "    title, cams = parse_inventory(raw)\n",
        "    title, cams = parse_inventory(raw, path)\n",
    ),
    Mutant(
        "utf-8 500",
        "app/h7020_panel.py",
        "except (json.JSONDecodeError, UnicodeDecodeError) as exc:",
        "except json.JSONDecodeError as exc:",
    ),
    Mutant(
        "non-ascii cookie 500",
        "app/agents.py",
        "hmac.compare_digest(cookie.encode(), browser_token.encode())",
        "hmac.compare_digest(cookie, browser_token)",
    ),
    Mutant(
        "bearer on LAN",
        "app/agents.py",
        "        if authorization:\n            return None\n        if cookie and",
        '        if authorization:\n            return ("x", frozenset(SCOPES))\n        if cookie and',
    ),
    Mutant(
        "cookie on socket",
        "app/agents.py",
        "    if cookie:\n        return None\n    if authorization is None:",
        "    if authorization is None:",
    ),
    Mutant(
        "brightness bound",
        "app/h7020_panel.py",
        "not 1 <= level <= 100",
        "not 0 <= level <= 100",
    ),
]


def preflight() -> None:
    """Fail before a panel starts when a needle is missing or no longer unique."""
    for mutant in MUTANTS:
        count = (REPO / mutant.path).read_text(encoding="utf-8").count(mutant.old)
        if count != 1:
            print(f"STALE {mutant.name}", flush=True)
            sys.exit(1)


def stage(work: Path) -> None:
    for name in ("app", "tools"):
        path = work / name
        if path.exists():
            shutil.rmtree(path)
    ignore = shutil.ignore_patterns("__pycache__")
    shutil.copytree(REPO / "app", work / "app", ignore=ignore)
    (work / "tools").mkdir(parents=True)
    shutil.copytree(REPO / "tools" / "tests", work / "tools" / "tests", ignore=ignore)
    for src in (REPO / "tools").glob("*.py"):
        shutil.copy2(src, work / "tools" / src.name)
    for name in ("references", ".venv"):
        link = work / name
        if not link.exists():
            link.symlink_to(REPO / name)


def panel_command(work: Path) -> str:
    python = REPO / ".venv" / "bin" / "python"
    panel = work / "app" / "h7020_panel.py"
    return shlex.join([str(python), str(panel), "--preview"])


def apply(work: Path, mutant: Mutant) -> None:
    path = work / mutant.path
    text = path.read_text(encoding="utf-8")
    if text.count(mutant.old) != 1:
        print(f"STALE {mutant.name}", flush=True)
        sys.exit(1)
    path.write_text(text.replace(mutant.old, mutant.new, 1), encoding="utf-8")


def first_failure(output: str) -> str:
    for line in output.splitlines():
        if line.startswith("FAIL"):
            return line[:70]
    return ""


def run_contract(work: Path, command: str) -> subprocess.CompletedProcess[str]:
    contract = work / "tools" / "tests" / "panel_contract.py"
    python = REPO / ".venv" / "bin" / "python"
    return subprocess.run(
        [str(python), str(contract), "--panel-command", command],
        cwd=work,
        capture_output=True,
        text=True,
        timeout=120,
    )


def baseline(work: Path, command: str) -> None:
    """The copy has to pass, or every mutant below would read as caught."""
    stage(work)
    try:
        result = run_contract(work, command)
    except subprocess.TimeoutExpired:
        print("ERROR the unmutated panel timed out", flush=True)
        sys.exit(1)
    if result.returncode != 0:
        print("ERROR the unmutated panel does not pass the contract", flush=True)
        sys.stderr.write((result.stdout + result.stderr)[-2000:] + "\n")
        sys.exit(1)
    print("BASE    panel passes unmutated", flush=True)


def run_one(work: Path, mutant: Mutant, command: str) -> str:
    try:
        result = run_contract(work, command)
    except subprocess.TimeoutExpired:
        print(f"ERROR {mutant.name} timed out", flush=True)
        sys.exit(1)
    output = result.stdout + result.stderr
    failure = first_failure(output)
    if result.returncode == 0:
        tag = "MISSED"
    elif failure:
        tag = "CAUGHT"
    else:
        print(f"ERROR {mutant.name} ended without a FAIL line", flush=True)
        sys.stderr.write(output[-2000:] + "\n")
        sys.exit(1)
    print(f"{tag:7} {mutant.name:26} {failure}", flush=True)
    return tag


def contract_only(command: str) -> None:
    """Runs the repo's own contract against one command, from the repo root."""
    python = REPO / ".venv" / "bin" / "python"
    result = subprocess.run(
        [str(python), str(REPO / "tools" / "tests" / "panel_contract.py"), "--panel-command", command],
        cwd=REPO,
        capture_output=True,
        text=True,
        timeout=120,
    )
    if result.returncode != 0:
        print(f"ERROR the contract failed against: {command}", flush=True)
        sys.stderr.write((result.stdout + result.stderr)[-2000:] + "\n")
        sys.exit(1)
    print(f"BASE    contract passes against: {command}", flush=True)
    print("no mutants run: they edit the Python panel, which this command does not execute", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument(
        "--panel-command",
        default=None,
        help="Run the contract once against this command, from the repo root, and apply no mutants.",
    )
    args = parser.parse_args()
    if args.panel_command:
        contract_only(args.panel_command)
        return
    preflight()
    missed: list[str] = []
    with tempfile.TemporaryDirectory(prefix="tallow-panel-mutants-") as held:
        work = Path(held)
        baseline(work, panel_command(work))
        for mutant in MUTANTS:
            stage(work)
            apply(work, mutant)
            if run_one(work, mutant, panel_command(work)) == "MISSED":
                missed.append(mutant.name)
    print("missed:", missed, flush=True)
    sys.exit(1 if missed else 0)


if __name__ == "__main__":
    main()
