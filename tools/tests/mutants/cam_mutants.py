#!/usr/bin/env python3
"""Mutation tests for the cam's push and stream logic.

Copies tools/tests/cam_push with firmware/cam/src/push.rs, serve.rs, and stream.rs,
applies one mutation at a time, and runs those tests on this machine's architecture
in the pinned image, as `cam_push_test` in tools/mesh.sh does. The unmutated copy
runs first and has to pass. Exits non-zero if a mutation is missed, if its source
text is no longer in the tree, or if a mutant does not compile. Wi-Fi credentials
and the mesh key are not mounted, and the ESP-IDF code in the cam crate is not built.

A mutant with `hangs` removes a deadline, so its test never returns. Only those may
end by the time limit. A time limit on any other mutant is an error.

    .venv/bin/python tools/tests/mutants/cam_mutants.py
"""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import NamedTuple

from core_mutants import CORE_IMAGE, GIT_CACHE, REGISTRY, REPO, cargo, ignore_build, outcome

PUSH = "firmware/cam/src/push.rs"
SERVE = "firmware/cam/src/serve.rs"
CORE = REPO / "firmware" / "mesh" / "core"
HANG_LIMIT = 90


class Mutant(NamedTuple):
    name: str
    path: str
    old: str
    new: str
    hangs: bool = False


MUTANTS = [
    Mutant("a zero size is admitted", PUSH, "if size == 0 || size > SLOT {", "if size > SLOT {"),
    Mutant("a size past the slot is admitted", PUSH, "if size == 0 || size > SLOT {", "if size == 0 {"),
    Mutant(
        "another product is admitted",
        PUSH,
        "        if product != PRODUCT_CAM0 {\n            return Code::WrongProduct;\n        }\n",
        "",
    ),
    Mutant(
        "a version below the floor is admitted",
        PUSH,
        "        if version < floor {\n            return Code::Downgrade;\n        }\n",
        "",
    ),
    Mutant("the floor itself is refused", PUSH, "if version < floor {", "if version <= floor {"),
    Mutant(
        "the signature is not checked",
        PUSH,
        "        if !verify(public_key, product, version, &digest, &signature) {\n"
        "            return Code::BadSignature;\n"
        "        }\n",
        "        let _ = (verify, &signature);\n",
    ),
    Mutant(
        "any body activates",
        PUSH,
        "self.hash.clone().finish() == self.digest",
        "true",
    ),
    Mutant(
        "more than the size is taken",
        PUSH,
        "        if bytes.len() as u32 > room {\n            return false;\n        }\n",
        "        let _ = room;\n",
    ),
    Mutant("a lost floor reads as zero", PUSH, "Err(_) => u32::MAX,", "Err(_) => 0,"),
    Mutant(
        "a lower pending version lowers the floor",
        PUSH,
        "pending.filter(|version| *version > floor)",
        "pending.filter(|version| *version != floor)",
    ),
    Mutant(
        "an idle job never expires at the limit",
        PUSH,
        "now_ms.wrapping_sub(self.last) >= IDLE_MS",
        "now_ms.wrapping_sub(self.last) > IDLE_MS",
    ),
    Mutant(
        "the idle clock does not wrap",
        PUSH,
        "now_ms.wrapping_sub(self.last) >= IDLE_MS",
        "now_ms >= self.last + IDLE_MS",
    ),
    Mutant(
        "one old frame is enough for the mark",
        PUSH,
        "now_ms.wrapping_sub(frame) <= FRAME_FRESH_MS && ",
        "",
    ),
    Mutant(
        "Wi-Fi down keeps the timer",
        PUSH,
        "        if !up {\n            self.since = None;\n            return;\n        }\n",
        "        if !up {\n            return;\n        }\n",
    ),
    Mutant(
        "the request has no deadline",
        SERVE,
        "        if Instant::now() >= deadline {\n            return false;\n        }\n",
        "",
        hangs=True,
    ),
    Mutant("a spent deadline still writes", SERVE, "left.is_zero() ||", ""),
    Mutant(
        "a GET anywhere in the head passes",
        SERVE,
        "return buf.starts_with(b\"GET \");",
        "return buf.windows(4).any(|window| window == b\"GET \");",
    ),
    Mutant(
        "a write has no deadline",
        SERVE,
        "sock.set_write_timeout(Some(left.max(SHORTEST))).is_err()",
        "false",
        hangs=True,
    ),
]


def preflight() -> None:
    for mutant in MUTANTS:
        count = (REPO / mutant.path).read_text(encoding="utf-8").count(mutant.old)
        if count != 1:
            print(f"STALE {mutant.name}", flush=True)
            sys.exit(1)


def stage(tree: Path) -> Path:
    """Lays the tree out as cam_push_test does. Returns the crate's directory."""
    if tree.exists():
        shutil.rmtree(tree)
    crate = tree / "tools" / "tests" / "cam_push"
    shutil.copytree(REPO / "tools" / "tests" / "cam_push", crate, ignore=ignore_build)
    for name in ("push.rs", "serve.rs", "stream.rs"):
        shutil.copy2(REPO / "firmware" / "cam" / "src" / name, crate / "src" / name)
    (tree / "firmware" / "mesh" / "core").mkdir(parents=True)
    return crate


def apply(crate: Path, mutant: Mutant) -> None:
    path = crate / "src" / Path(mutant.path).name
    text = path.read_text(encoding="utf-8")
    if text.count(mutant.old) != 1:
        print(f"STALE {mutant.name}", flush=True)
        sys.exit(1)
    path.write_text(text.replace(mutant.old, mutant.new, 1), encoding="utf-8")


def run(tree: Path, target: Path, limit: int) -> subprocess.CompletedProcess[str]:
    return cargo(
        CORE_IMAGE,
        "/work/tools/tests/cam_push",
        [
            (tree, "/work"),
            (CORE, "/work/firmware/mesh/core:ro"),
            (target, "/cargo-target"),
            (REGISTRY, "/home/esp/.cargo/registry"),
            (GIT_CACHE, "/home/esp/.cargo/git"),
        ],
        ["cargo", "test", "--offline", "--target", "x86_64-unknown-linux-gnu"],
        limit,
    )


def main() -> None:
    preflight()
    REGISTRY.mkdir(parents=True, exist_ok=True)
    GIT_CACHE.mkdir(parents=True, exist_ok=True)
    missed: list[str] = []
    with tempfile.TemporaryDirectory(prefix="tallow-cam-mutants-") as held:
        work = Path(held)
        tree = work / "tree"
        target = work / "target"
        target.mkdir()
        stage(tree)
        result = run(tree, target, 600)
        if result.returncode != 0 or "test result:" not in result.stdout + result.stderr:
            print("ERROR the unmutated cam tests do not pass", flush=True)
            sys.stderr.write((result.stdout + result.stderr)[-2000:] + "\n")
            sys.exit(1)
        print("BASE    cam passes unmutated", flush=True)
        for mutant in MUTANTS:
            crate = stage(tree)
            apply(crate, mutant)
            try:
                result = run(tree, target, HANG_LIMIT if mutant.hangs else 300)
            except subprocess.TimeoutExpired:
                if not mutant.hangs:
                    print(f"ERROR {mutant.name} timed out", flush=True)
                    sys.exit(1)
                print(f"CAUGHT  {mutant.name} (the test hung past {HANG_LIMIT} s)", flush=True)
                continue
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
