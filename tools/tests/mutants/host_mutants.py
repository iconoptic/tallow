#!/usr/bin/env python3
"""Mutation tests for tallowd's GATT and OTA senders, and the agent broker.

Copies host/ and the core into a temp directory, applies one mutation at a time,
and runs that package's tests in the pinned image. The unmutated tree runs first
and has to pass. Exits non-zero if a mutation is missed, if its source text is no
longer in the tree, or if a mutant does not compile: one that never ran proved
nothing. The runner is the one in core_mutants.py.

    .venv/bin/python tools/tests/mutants/host_mutants.py
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

from core_mutants import GIT_CACHE, REGISTRY, REPO, Mutant, apply, copy_host, outcome, run_host

GATT = "host/tallowd/src/gatt.rs"
OTA = "host/tallowd/src/ota.rs"
LINK = "host/tallowd/src/link.rs"
CAMS = "host/tallowd/src/cams.rs"
LIBRARY = "host/tallowd/src/library.rs"
BROKER = "host/tallow-broker/src/serve.rs"

# `suite` is the cargo package.
MUTANTS = [
    Mutant(
        "gatt skips the ready wait",
        GATT,
        "if !wait_for_target(port, job.target, READY_S)? {",
        "if false && !wait_for_target(port, job.target, READY_S)? {",
        "tallowd",
    ),
    Mutant(
        "ota skips the ready wait",
        OTA,
        "if !gatt::wait_for_target(port, target, READY_S)? {",
        "if false && !gatt::wait_for_target(port, target, READY_S)? {",
        "tallowd",
    ),
    Mutant(
        "any frame counts as ready",
        GATT,
        "if parse_line(&line).is_some_and(|parsed| parsed.is_from(target)) {",
        "if parse_line(&line).is_some() {",
        "tallowd",
    ),
    Mutant(
        "any origin is the target",
        LINK,
        "u32::from_str_radix(&self.origin, 16) == Ok(target)",
        "self.origin.len() < 99 || target == 0",
        "tallowd",
    ),
    Mutant(
        "gatt id is constant",
        GATT,
        "let id = port.next_id();",
        "let id = 1 + 0 * port.next_id();",
        "tallowd",
    ),
    Mutant(
        "host waits no longer than the node",
        GATT,
        "f64::from(ms) / 1000.0 + GRACE_S",
        "f64::from(ms) / 1000.0 + 0.0 * GRACE_S",
        "tallowd",
    ),
    Mutant(
        "a failed Result is dropped between chunks",
        OTA,
        "if code != Code::Ok && !self.held {",
        "if code != Code::Ok && self.held && false {",
        "tallowd",
    ),
    Mutant(
        "Refused after a Query is trusted",
        OTA,
        "}) if asked => return Err(Fail::Unconfirmed.into()),",
        "}) if asked && false => return Err(Fail::Unconfirmed.into()),",
        "tallowd",
    ),
    Mutant(
        "Begin is not sent again after Refused",
        OTA,
        '                    self.send(begin, "begin")?;\n'
        "                    last_begin = self.port.clock();\n"
        "                }\n"
        "                Some(Up::Result { code: Code::Ok }) => {}",
        "                    last_begin = self.port.clock();\n"
        "                }\n"
        "                Some(Up::Result { code: Code::Ok }) => {}",
        "tallowd",
    ),
    Mutant(
        "Commit is not sent again",
        OTA,
        '                    asked = false;\n                    self.send(&Down::Commit, "commit")?;',
        "                    asked = false;",
        "tallowd",
    ),
    Mutant(
        "a stale Status resends Commit",
        OTA,
        "Some(Up::Status { .. }) if asked => {",
        "Some(Up::Status { .. }) if asked || true => {",
        "tallowd",
    ),
    Mutant(
        "an old 64-byte signature is accepted",
        OTA,
        "let bytes: &[u8; SIG_FILE] = bytes.try_into().ok()?;",
        "let mut pad = [0u8; SIG_FILE]; if bytes.len() < 60 { return None; } "
        "pad[..bytes.len().min(SIG_FILE)].copy_from_slice(&bytes[..bytes.len().min(SIG_FILE)]); "
        "let bytes = &pad;",
        "tallowd",
    ),
    Mutant(
        "stopping survives a cancelled join",
        CAMS,
        "                state.stopping = false;\n                state.ended = false;\n",
        "                state.ended = false;\n",
        "tallowd",
    ),
    Mutant(
        "a late viewer is not told the stream ended",
        CAMS,
        "        if state.ended {\n            queue.offer(None);\n        }\n",
        "",
        "tallowd",
    ),
    Mutant(
        "an old pump ends the new stream",
        CAMS,
        "        if state.generation != generation {\n            return;\n        }\n",
        "",
        "tallowd",
    ),
    Mutant(
        "cam saves share one temp file",
        CAMS,
        'temp_name.push(format!(".{}.{seq}.tmp", std::process::id()));',
        'let _ = seq;\n    temp_name.push(".tmp");',
        "tallowd",
    ),
    Mutant(
        "a save takes no write lock",
        LIBRARY,
        "        let _turn = self.turn();\n        let mut items: Vec<T> = self.read()?;\n",
        "        let mut items: Vec<T> = self.read()?;\n",
        "tallowd",
    ),
    Mutant(
        "library saves share one temp file",
        LIBRARY,
        '.join(format!("{}.{}.{seq}.tmp", T::FILE, std::process::id()));',
        '.join(format!("{}.tmp{}", T::FILE, seq * 0));',
        "tallowd",
    ),
    Mutant(
        "a guest write has no deadline",
        BROKER,
        "    match timeout(limit, conn.write_all(bytes)).await {\n"
        "        Ok(written) => written,\n"
        "        Err(_) => Err(io::ErrorKind::TimedOut.into()),\n"
        "    }\n",
        "    let _ = limit;\n    let _ = timeout::<std::future::Ready<()>>;\n    conn.write_all(bytes).await\n",
        "tallow-broker",
    ),
]


def preflight() -> None:
    for mutant in MUTANTS:
        count = (REPO / mutant.path).read_text(encoding="utf-8").count(mutant.old)
        if count != 1:
            print(f"STALE {mutant.name}", flush=True)
            sys.exit(1)


def main() -> None:
    preflight()
    REGISTRY.mkdir(parents=True, exist_ok=True)
    GIT_CACHE.mkdir(parents=True, exist_ok=True)
    missed: list[str] = []
    with tempfile.TemporaryDirectory(prefix="tallow-host-mutants-") as held:
        work = Path(held)
        target = work / "target"
        target.mkdir()
        tree = work / "tree"
        for package in sorted({mutant.suite for mutant in MUTANTS}):
            copy_host(tree)
            result = run_host(tree, target, 900, package)
            if result.returncode != 0 or "test result:" not in result.stdout + result.stderr:
                print(f"ERROR the unmutated {package} tests do not pass", flush=True)
                sys.stderr.write((result.stdout + result.stderr)[-2000:] + "\n")
                sys.exit(1)
            print(f"BASE    {package} passes unmutated", flush=True)
        for mutant in MUTANTS:
            copy_host(tree)
            apply(tree, mutant)
            try:
                result = run_host(tree, target, 600, mutant.suite)
            except subprocess.TimeoutExpired:
                print(f"ERROR {mutant.name} timed out", flush=True)
                sys.exit(1)
            tag, detail = outcome(result)
            if tag == "COMPILE":
                tag = "ERROR"
                detail = (result.stdout + result.stderr)[-2000:]
            print(f"{tag:7} {mutant.name}", flush=True)
            if tag == "ERROR":
                sys.stderr.write(detail if detail.endswith("\n") else detail + "\n")
                sys.exit(1)
            if tag == "MISSED":
                missed.append(mutant.name)
    print("missed:", missed, flush=True)
    sys.exit(1 if missed else 0)


if __name__ == "__main__":
    main()
