#!/usr/bin/env python3
"""Mutation tests for the mesh core and for delivery in tallowd.

Copies the crate under test into a temp directory, applies one mutation at a
time, and runs that crate's tests in the pinned image. Exits non-zero if a
mutation is missed or its source text is no longer in the tree. A missing
needle fails before any container starts.

Cargo uses the mesh registry cache and a target directory that outlives each
source copy, so later mutants recompile one file. The host cargo is not used.
mesh.key is not mounted.

The unmutated tree runs first and has to pass, so a copy that cannot build or
pass does not read as every mutant caught. A mutant that does not compile was
never tested, so it is an error, not a catch.

cut_inquiry is not here. Its window math lives in
firmware/mesh/node/src/bluetooth.rs. That crate does not cargo-test on the
host, so a mutant of it would survive. It is not listed.

    .venv/bin/python tools/tests/mutants/core_mutants.py [--core-only]

--core-only skips the suites that build tallowd.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import NamedTuple

REPO = Path(__file__).resolve().parents[3]
CORE_ROOT = Path("firmware/mesh/core")
# The same images tools/mesh.sh uses. Core tests are the mesh image. tallowd
# links bluer, so delivery runs in the host image.
CORE_IMAGE = "docker.io/espressif/idf-rust:esp32_1.95.0.0"
HOST_IMAGE = "localhost/tallow-rust-host:esp32_1.95.0.0"
REGISTRY = REPO / "firmware" / "mesh" / ".cache" / "registry"
GIT_CACHE = REPO / "firmware" / "mesh" / ".cache" / "git"


class Mutant(NamedTuple):
    name: str
    path: str
    old: str
    new: str
    suite: str


# The replace-time whitelist check. Each suite copy starts from the original,
# so these three share a needle and exclude one link state each.
_WHITELIST = "if !matches!(self.link, Link::Idle | Link::Closing) && !self.on_list(&self.peer) {"

MUTANTS = [
    Mutant(
        "open ignores version",
        "firmware/mesh/core/src/wire.rs",
        "|| data[0] != VERSION {",
        " {",
        "core",
    ),
    Mutant(
        "unknown kind becomes Beacon",
        "firmware/mesh/core/src/wire.rs",
        "let kind = FrameKind::from_u8(data[1])?;",
        "let kind = FrameKind::from_u8(data[1]).unwrap_or(FrameKind::Beacon);",
        "core",
    ),
    Mutant(
        "decode swaps window/interval",
        "firmware/mesh/core/src/wire.rs",
        "ble_window_ms: half(2),\n            ble_interval_ms: half(4),",
        "ble_window_ms: half(4),\n            ble_interval_ms: half(2),",
        "core",
    ),
    Mutant(
        "decode swaps probe fields",
        "firmware/mesh/core/src/wire.rs",
        "probe_hz: data[9],\n            probe_size: data[10],",
        "probe_hz: data[10],\n            probe_size: data[9],",
        "core",
    ),
    Mutant(
        "period big-endian",
        "firmware/mesh/core/src/wire.rs",
        "inquiry_period_s: half(6),",
        "inquiry_period_s: u16::from_be_bytes([data[6],data[7]]),",
        "core",
    ),
    Mutant(
        "long command accepted",
        "firmware/mesh/core/src/wire.rs",
        "if data.len() != Self::WIRE {",
        "if data.len() < Self::WIRE {",
        "core",
    ),
    Mutant(
        "218 refused",
        "firmware/mesh/core/src/wire.rs",
        "if payload.len() > MAX_PAYLOAD {",
        "if payload.len() >= MAX_PAYLOAD {",
        "core",
    ),
    Mutant(
        "frame 255",
        "firmware/mesh/core/src/wire.rs",
        "pub const MAX_FRAME: usize = 250;",
        "pub const MAX_FRAME: usize = 255;",
        "core",
    ),
    Mutant(
        "beacon parent BE",
        "firmware/mesh/core/src/mesh.rs",
        "payload[1..5].copy_from_slice(&parent.to_le_bytes());",
        "payload[1..5].copy_from_slice(&parent.to_be_bytes());",
        "core",
    ),
    Mutant(
        "beacon number moved",
        "firmware/mesh/core/src/mesh.rs",
        "payload[5..7].copy_from_slice(&self.beacon_no.to_le_bytes());",
        "payload[7..9].copy_from_slice(&self.beacon_no.to_le_bytes());",
        "core",
    ),
    Mutant(
        "beacon ttl 16",
        "firmware/mesh/core/src/mesh.rs",
        "let head = self.head(FrameKind::Beacon, 1);",
        "let head = self.head(FrameKind::Beacon, 16);",
        "core",
    ),
    Mutant(
        "company BE",
        "firmware/mesh/core/src/names.rs",
        "record[7..9].copy_from_slice(&entry.company.to_le_bytes());",
        "record[7..9].copy_from_slice(&entry.company.to_be_bytes());",
        "core",
    ),
    Mutant(
        "name kind zero",
        "firmware/mesh/core/src/names.rs",
        "record[6] = entry.kind;",
        "record[6] = 0;",
        "core",
    ),
    Mutant(
        "report kind zero",
        "firmware/mesh/core/src/sightings.rs",
        "record[6] = entry.kind;",
        "record[6] = 0;",
        "core",
    ),
    Mutant(
        "age cap 254",
        "firmware/mesh/core/src/sightings.rs",
        "record[9] = age.min(255) as u8;",
        "record[9] = age.min(254) as u8;",
        "core",
    ),
    Mutant(
        "negative age back",
        "firmware/mesh/core/src/sightings.rs",
        "(now_ms.wrapping_sub(entry.last_ms) as i32).max(0) as u32 / 1000",
        "now_ms.wrapping_sub(entry.last_ms) / 1000",
        "core",
    ),
    Mutant(
        "no beacon source check",
        "firmware/mesh/core/src/mesh.rs",
        "        if frame.kind == FrameKind::Beacon && id_of(&src) != frame.origin {\n"
        "            self.rx_bad = self.rx_bad.wrapping_add(1);\n"
        "            return;\n"
        "        }\n",
        "",
        "core",
    ),
    Mutant(
        "silent ack loss",
        "firmware/mesh/core/src/mesh.rs",
        "                    let before = self.parent().map(|parent| parent.id);\n"
        "                    if let Some(index) = self.parent {",
        "                    let before = None;\n                    if let Some(index) = self.parent {",
        "core",
    ),
    Mutant(
        "replay window accepts an old seq",
        "firmware/mesh/core/src/mesh.rs",
        "                        if behind >= REPLAY_WINDOW {\n"
        "                            return false;\n"
        "                        }\n",
        "                        if behind >= REPLAY_WINDOW {\n"
        "                            entry.last_ms = now_ms;\n"
        "                            return true;\n"
        "                        }\n",
        "core",
    ),
    Mutant(
        "denied beacon becomes a neighbour",
        "firmware/mesh/core/src/mesh.rs",
        "        if frame.kind == FrameKind::Beacon && self.denied(frame.origin) {\n"
        "            return;\n"
        "        }\n",
        "",
        "core",
    ),
    Mutant(
        "non-root sends down",
        "firmware/mesh/core/src/mesh.rs",
        "if !self.is_root(now_ms) || !is_down(kind) || target == self.id {",
        "if !is_down(kind) || target == self.id {",
        "core",
    ),
    Mutant(
        "connect ignores the whitelist",
        "firmware/mesh/core/src/gatt.rs",
        "                if !self.on_list(&address) {\n"
        "                    return Some(self.answer(id, Status::Refused));\n"
        "                }\n",
        "",
        "core",
    ),
    Mutant(
        "whitelist drop skips Opening",
        "firmware/mesh/core/src/gatt.rs",
        _WHITELIST,
        "if !matches!(self.link, Link::Idle | Link::Closing | Link::Opening) && !self.on_list(&self.peer) {",
        "core",
    ),
    Mutant(
        "whitelist drop skips Setup",
        "firmware/mesh/core/src/gatt.rs",
        _WHITELIST,
        "if !matches!(self.link, Link::Idle | Link::Closing | Link::Setup) && !self.on_list(&self.peer) {",
        "core",
    ),
    Mutant(
        "whitelist drop skips Open",
        "firmware/mesh/core/src/gatt.rs",
        _WHITELIST,
        "if !matches!(self.link, Link::Idle | Link::Closing | Link::Open) && !self.on_list(&self.peer) {",
        "core",
    ),
    Mutant(
        "replay bitmap is not read",
        "firmware/mesh/core/src/mesh.rs",
        "                        if entry.window & bit != 0 {\n"
        "                            return false;\n"
        "                        }\n",
        "",
        "core",
    ),
    Mutant(
        "replay bit is not set",
        "firmware/mesh/core/src/mesh.rs",
        "                        entry.window |= bit;\n",
        "",
        "core",
    ),
    Mutant(
        "replay bitmap does not move with the newest",
        "firmware/mesh/core/src/mesh.rs",
        "                            entry.window << ahead\n",
        "                            entry.window\n",
        "core",
    ),
    Mutant(
        "late open is not closed",
        "firmware/mesh/core/src/gatt.rs",
        "(Done::Opened(true), Link::Idle, _) => {",
        "(Done::Opened(true), Link::Idle, _) if false => {",
        "core",
    ),
    Mutant(
        "written bytes are not the chunk",
        "firmware/mesh/core/src/ota.rs",
        "        self.write_data[..len].copy_from_slice(data);\n",
        "",
        "core",
    ),
    Mutant(
        "write length is wrong",
        "firmware/mesh/core/src/ota.rs",
        "self.write_len = len as u16;",
        "self.write_len = CHUNK as u16;",
        "core",
    ),
    Mutant(
        "slot limit off by one",
        "firmware/mesh/core/src/ota.rs",
        "if size == 0 || size > SLOT {",
        "if size == 0 || size >= SLOT {",
        "core",
    ),
    Mutant(
        "zero size allowed",
        "firmware/mesh/core/src/ota.rs",
        "if size == 0 || size > SLOT {",
        "if size > SLOT {",
        "core",
    ),
    Mutant(
        "chunk past the end allowed",
        "firmware/mesh/core/src/ota.rs",
        "        if end > self.size {\n            return None;\n        }\n",
        "",
        "core",
    ),
    Mutant(
        "short chunk in the middle allowed",
        "firmware/mesh/core/src/ota.rs",
        "(!at_end && len != CHUNK) || ",
        "",
        "core",
    ),
    Mutant(
        "long begin decodes",
        "firmware/mesh/core/src/ota.rs",
        "if data.len() != BEGIN {",
        "if data.len() < BEGIN {",
        "core",
    ),
    Mutant(
        "long chunk is cut and decodes",
        "firmware/mesh/core/src/ota.rs",
        "            if body.is_empty() || body.len() > CHUNK {\n",
        "            let body = &body[..body.len().min(CHUNK)];\n            if body.is_empty() {\n",
        "core",
    ),
    Mutant(
        "commit stays active",
        "firmware/mesh/core/src/ota.rs",
        "        self.active = false;\n        self.size = 0;\n",
        "        self.size = 0;\n",
        "core",
    ),
    Mutant(
        "commit needs no active transfer",
        "firmware/mesh/core/src/ota.rs",
        "    fn on_commit(&mut self, public_key: &[u8; 32]) -> Option<Up> {\n        if !self.active {\n",
        "    fn on_commit(&mut self, public_key: &[u8; 32]) -> Option<Up> {\n        if false {\n",
        "core",
    ),
    Mutant(
        "begin does not check the signature",
        "firmware/mesh/core/src/ota.rs",
        "} else if !verify(public_key, product, version, &digest, &signature) {",
        "} else if false {",
        "core",
    ),
    Mutant(
        "commit does not check the signature",
        "firmware/mesh/core/src/ota.rs",
        "            } else if !verify(\n                public_key,\n",
        "            } else if false && !verify(\n                public_key,\n",
        "core",
    ),
    Mutant(
        "commit does not check the hash",
        "firmware/mesh/core/src/ota.rs",
        "            if got != self.digest {\n",
        "            if false {\n",
        "core",
    ),
    Mutant(
        "product not checked",
        "firmware/mesh/core/src/ota.rs",
        "} else if product != self.product {",
        "} else if false {",
        "core",
    ),
    Mutant(
        "floor not checked",
        "firmware/mesh/core/src/ota.rs",
        "} else if version < self.floor {",
        "} else if false {",
        "core",
    ),
    Mutant(
        "equal version refused",
        "firmware/mesh/core/src/ota.rs",
        "} else if version < self.floor {",
        "} else if version <= self.floor {",
        "core",
    ),
    Mutant(
        "signed message omits the version",
        "firmware/mesh/core/src/ota.rs",
        "    out[context + 1..context + 5].copy_from_slice(&version.to_le_bytes());\n",
        "",
        "core",
    ),
    Mutant(
        "signed message omits the product",
        "firmware/mesh/core/src/ota.rs",
        "    out[context] = product;\n",
        "",
        "core",
    ),
    Mutant(
        "health: a lost parent keeps the wait",
        "firmware/mesh/core/src/ota.rs",
        "        if !linked || !clean {\n            self.since = None;\n",
        "        if !linked || !clean {\n",
        "core",
    ),
    Mutant(
        "health: no route counts as linked",
        "firmware/mesh/core/src/ota.rs",
        "let linked = reading.rooted || reading.has_parent;",
        "let linked = true;",
        "core",
    ),
    Mutant(
        "health: a leaf needs no ack",
        "firmware/mesh/core/src/ota.rs",
        "&& (reading.rooted || reading.acked != self.acked_then)",
        "",
        "core",
    ),
    Mutant(
        "health: the root needs an ack",
        "firmware/mesh/core/src/ota.rs",
        "&& (reading.rooted || reading.acked != self.acked_then)",
        "&& reading.acked != self.acked_then",
        "core",
    ),
    Mutant(
        "health: free heap ignored",
        "firmware/mesh/core/src/ota.rs",
        "let clean = reading.heap_free >= HEAP_FLOOR && reading.heap_min >= HEAP_FLOOR;",
        "let clean = reading.heap_min >= HEAP_FLOOR;",
        "core",
    ),
    Mutant(
        "health: heap mark ignored",
        "firmware/mesh/core/src/ota.rs",
        "let clean = reading.heap_free >= HEAP_FLOOR && reading.heap_min >= HEAP_FLOOR;",
        "let clean = reading.heap_free >= HEAP_FLOOR;",
        "core",
    ),
    Mutant(
        "health: five minutes not waited",
        "firmware/mesh/core/src/ota.rs",
        "reading.now_ms.wrapping_sub(since) >= CONFIRM_AFTER_MS",
        "reading.now_ms.wrapping_sub(since) > 0",
        "core",
    ),
    Mutant(
        "silence never expires",
        "firmware/mesh/core/src/ota.rs",
        "if !self.active || now_ms.wrapping_sub(last_ms) < SILENCE_MS {",
        "if !self.active || now_ms.wrapping_sub(last_ms) < u32::MAX {",
        "core",
    ),
    Mutant(
        "silence expires at once",
        "firmware/mesh/core/src/ota.rs",
        "if !self.active || now_ms.wrapping_sub(last_ms) < SILENCE_MS {",
        "if !self.active {",
        "core",
    ),
    Mutant(
        "silence does not wrap",
        "firmware/mesh/core/src/ota.rs",
        "if !self.active || now_ms.wrapping_sub(last_ms) < SILENCE_MS {",
        "if !self.active || now_ms.saturating_sub(last_ms) < SILENCE_MS {",
        "core",
    ),
    Mutant(
        "abandon queues no abort",
        "firmware/mesh/core/src/ota.rs",
        "        self.clear_steps();\n        self.queue(Step::Abort);\n        self.reset();\n",
        "        self.clear_steps();\n        self.reset();\n",
        "core",
    ),
    Mutant(
        "abandon leaves the transfer active",
        "firmware/mesh/core/src/ota.rs",
        "        self.clear_steps();\n        self.queue(Step::Abort);\n        self.reset();\n",
        "        self.clear_steps();\n        self.queue(Step::Abort);\n",
        "core",
    ),
    Mutant(
        "abandon aborts with nothing running",
        "firmware/mesh/core/src/ota.rs",
        "    pub fn abandon(&mut self) {\n        if !self.active {\n            return;\n        }\n",
        "    pub fn abandon(&mut self) {\n",
        "core",
    ),
    Mutant(
        "floor can be lowered",
        "firmware/mesh/core/src/ota.rs",
        "self.floor = self.floor.max(version);",
        "self.floor = version;",
        "core",
    ),
    Mutant(
        "commit ignores a floor raised after begin",
        "firmware/mesh/core/src/ota.rs",
        "let code = if self.version < self.floor {",
        "let code = if false {",
        "core",
    ),
    Mutant(
        "floor never raised",
        "firmware/mesh/core/src/ota.rs",
        "self.floor = self.floor.max(version);",
        "let _ = version;",
        "core",
    ),
    Mutant(
        "push runs over actions not yet taken",
        "firmware/mesh/core/src/ota.rs",
        "        if self.step_len != 0 && !matches!(down, Down::Query { .. }) {\n"
        "            return None;\n"
        "        }\n",
        "",
        "core",
    ),
    Mutant(
        "delivery ignores the hold",
        "host/tallowd/src/trial.rs",
        "let expected = span.max(least);",
        "let expected = span;",
        "host",
    ),
]


def preflight() -> None:
    for mutant in MUTANTS:
        count = (REPO / mutant.path).read_text(encoding="utf-8").count(mutant.old)
        if count != 1:
            print(f"STALE {mutant.name}", flush=True)
            sys.exit(1)


def ignore_build(_path: str, names: list[str]) -> set[str]:
    return {"target"} if "target" in names else set()


def copy_core(dest: Path) -> None:
    if dest.exists():
        shutil.rmtree(dest)
    shutil.copytree(REPO / CORE_ROOT, dest, ignore=ignore_build)


def copy_host(dest: Path) -> None:
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    shutil.copytree(REPO / "host", dest / "host", ignore=ignore_build)
    shutil.copytree(REPO / CORE_ROOT, dest / CORE_ROOT, ignore=ignore_build)
    # tallowd's pages are include_str! of app/*.html. The path is four levels
    # up from host/tallowd/src/panel/, so the copy sits beside host/.
    shutil.copytree(REPO / "app", dest / "app", ignore=shutil.ignore_patterns("__pycache__"))


def apply(root: Path, mutant: Mutant) -> None:
    if mutant.suite == "core":
        path = root / Path(mutant.path).relative_to(CORE_ROOT)
    else:
        path = root / mutant.path
    text = path.read_text(encoding="utf-8")
    if text.count(mutant.old) != 1:
        print(f"STALE {mutant.name}", flush=True)
        sys.exit(1)
    path.write_text(text.replace(mutant.old, mutant.new, 1), encoding="utf-8")


def cargo(image: str, workdir: str, mounts: list[tuple[Path, str]], args: list[str], timeout: int) -> subprocess.CompletedProcess[str]:
    command = [
        "podman",
        "run",
        "--rm",
        "--userns=keep-id",
        "--user",
        f"{os.getuid()}:{os.getgid()}",
        "-w",
        workdir,
        "-e",
        "HOME=/home/esp",
        "-e",
        "CARGO_TARGET_DIR=/cargo-target",
        "-e",
        "CARGO_NET_OFFLINE=true",
    ]
    for src, dest in mounts:
        command.extend(["-v", f"{src}:{dest}"])
    command.append(image)
    command.extend(args)
    return subprocess.run(command, capture_output=True, text=True, timeout=timeout)


def outcome(result: subprocess.CompletedProcess[str]) -> tuple[str, str]:
    output = result.stdout + result.stderr
    if "could not compile" in output or "error[E" in output:
        tag = "COMPILE"
    elif "test result:" in output:
        tag = "MISSED" if result.returncode == 0 else "CAUGHT"
    else:
        tag = "ERROR"
    detail = ""
    for line in output.splitlines():
        if line.startswith("test result:") or line.startswith("error"):
            detail = line[:120]
            break
    return tag, detail if tag != "ERROR" else output[-2000:]


def run_core(src: Path, target: Path, timeout: int) -> subprocess.CompletedProcess[str]:
    return cargo(
        CORE_IMAGE,
        "/work",
        [
            (src, "/work"),
            (target, "/cargo-target"),
            (REGISTRY, "/home/esp/.cargo/registry"),
            (GIT_CACHE, "/home/esp/.cargo/git"),
        ],
        ["cargo", "test", "--offline", "--target", "x86_64-unknown-linux-gnu"],
        timeout,
    )


def run_host(tree: Path, target: Path, timeout: int, package: str = "tallowd") -> subprocess.CompletedProcess[str]:
    return cargo(
        HOST_IMAGE,
        "/work/host",
        [
            (tree, "/work"),
            (target, "/cargo-target"),
            (REGISTRY, "/home/esp/.cargo/registry"),
            (GIT_CACHE, "/home/esp/.cargo/git"),
        ],
        ["cargo", "test", "--offline", "-p", package],
        timeout,
    )


def main() -> None:
    core_only = "--core-only" in sys.argv[1:]
    suites = {"core"} if core_only else {"core", "host"}
    preflight()
    REGISTRY.mkdir(parents=True, exist_ok=True)
    GIT_CACHE.mkdir(parents=True, exist_ok=True)
    missed: list[str] = []
    with tempfile.TemporaryDirectory(prefix="tallow-core-mutants-") as held:
        work = Path(held)
        core_target = work / "core-target"
        host_target = work / "host-target"
        core_target.mkdir()
        host_target.mkdir()
        for suite in sorted(suites):
            if suite == "core":
                src = work / "core"
                copy_core(src)
                result = run_core(src, core_target, 300)
            else:
                tree = work / "host-tree"
                copy_host(tree)
                result = run_host(tree, host_target, 600)
            if result.returncode != 0 or "test result:" not in result.stdout + result.stderr:
                print(f"ERROR the unmutated {suite} suite does not pass", flush=True)
                sys.stderr.write((result.stdout + result.stderr)[-2000:] + "\n")
                sys.exit(1)
            print(f"BASE    {suite} passes unmutated", flush=True)
        for mutant in MUTANTS:
            if mutant.suite not in suites:
                continue
            if mutant.suite == "core":
                src = work / "core"
                copy_core(src)
                apply(src, mutant)
                try:
                    result = run_core(src, core_target, 300)
                except subprocess.TimeoutExpired:
                    print(f"ERROR {mutant.name} timed out", flush=True)
                    sys.exit(1)
            else:
                tree = work / "host-tree"
                copy_host(tree)
                apply(tree, mutant)
                try:
                    result = run_host(tree, host_target, 600)
                except subprocess.TimeoutExpired:
                    print(f"ERROR {mutant.name} timed out", flush=True)
                    sys.exit(1)
            tag, detail = outcome(result)
            # A mutant that does not compile was not run, so nothing was proved.
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
