#!/usr/bin/env python3
"""Checks deep call chains against a task's stack, from a built Xtensa ELF.

A size_of test cannot see this. Rust builds large values in the caller's frame,
so main's frame holds the mesh, and every call stacks on top of it. Each Xtensa
windowed function opens with `entry a1, N`, and N is its frame. This reads N for
the symbols in each chain, adds them, and adds a margin for what the chain does
not name, such as ESP-IDF's own frames.

Prints symbol names and numbers only. The node's ELF embeds the mesh key, so
this copies nothing and prints no other bytes.

    stack_budget.py <elf> [--chains file.json] [--stack N] [--list pattern]

The default chains are the node's. A chains file holds:

    {"config": "path/to/sdkconfig.defaults",
     "key": "CONFIG_ESP_MAIN_TASK_STACK_SIZE",
     "margin": 2048,
     "chains": {"name": ["symbol substring", ...]}}

Patterns match the raw symbol, which is Rust's v0 mangling: `Bluetooth5start`,
not `Bluetooth::start`. Use --list to find one. A pattern that matches nothing
fails, so a rename or an inlined function cannot pass unnoticed. Where several
match, the largest frame counts.
"""

from __future__ import annotations

import argparse
import json
import re
import struct
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

NODE_CHAINS = {
    "config": "firmware/mesh/node/sdkconfig.defaults",
    "key": "CONFIG_ESP_MAIN_TASK_STACK_SIZE",
    "margin": 2048,
    "chains": {
        "main + Mesh::new": ["tallow_node4main", "4Mesh3new"],
        "main + Bluetooth::start": ["tallow_node4main", "Bluetooth5start"],
        "main + ota verify": [
            "tallow_node4main",
            "tallow_node3ota5apply",
            "tallow_mesh_core3ota6verify",
            "decode_point_negated",
        ],
    },
}

SHT_SYMTAB = 2
SHT_NOBITS = 8
STT_FUNC = 2
ENTRY_OPCODE = 0x36
ENTRY_STACK_REGISTER = 1


class Elf:
    """Only the parts of an ELF32 little-endian file this tool needs."""

    def __init__(self, path: Path) -> None:
        self.data = path.read_bytes()
        if self.data[:6] != b"\x7fELF\x01\x01":
            raise SystemExit(f"{path}: not a 32-bit little-endian ELF")
        shoff = struct.unpack_from("<I", self.data, 32)[0]
        shentsize, shnum = struct.unpack_from("<HH", self.data, 46)
        self.sections = [
            struct.unpack_from("<10I", self.data, shoff + index * shentsize)
            for index in range(shnum)
        ]

    def functions(self) -> dict[str, list[int]]:
        """Each function's name and every address it has. C statics repeat names."""
        found: dict[str, list[int]] = {}
        for section in self.sections:
            if section[1] != SHT_SYMTAB:
                continue
            offset, size, link = section[4], section[5], section[6]
            strings = self.sections[link][4]
            for at in range(offset, offset + size, 16):
                name, value, _, info, _, _ = struct.unpack_from(
                    "<IIIBBH", self.data, at
                )
                if info & 0xF != STT_FUNC or value == 0:
                    continue
                end = self.data.index(b"\0", strings + name)
                text = self.data[strings + name : end].decode("ascii", "replace")
                found.setdefault(text, []).append(value)
        return found

    def frame(self, address: int) -> int | None:
        """N from `entry a1, N` at `address`, or None if no entry opens there."""
        for section in self.sections:
            addr, offset, size, kind = section[3], section[4], section[5], section[1]
            if kind == SHT_NOBITS or addr == 0 or not addr <= address < addr + size:
                continue
            start = offset + address - addr
            word = int.from_bytes(self.data[start : start + 3], "little")
            if word & 0xFF != ENTRY_OPCODE or (word >> 8) & 0xF != ENTRY_STACK_REGISTER:
                return None
            return (word >> 12) * 8
        return None


def configured(path: Path, key: str) -> int:
    match = re.search(rf"^{re.escape(key)}=(\d+)\s*$", path.read_text(), re.M)
    if not match:
        raise SystemExit(f"{path}: no {key}")
    return int(match.group(1))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("elf", type=Path)
    parser.add_argument("--chains", type=Path)
    parser.add_argument("--stack", type=int, help="use this instead of the config")
    parser.add_argument("--list", metavar="PATTERN", help="print matching frames")
    args = parser.parse_args()

    elf = Elf(args.elf)
    functions = elf.functions()
    frames: dict[str, int | None] = {}
    for name, addresses in functions.items():
        known = [f for f in map(elf.frame, addresses) if f is not None]
        frames[name] = max(known) if known else None

    if args.list:
        for name in sorted(frames):
            if args.list in name:
                print(f"{frames[name]!s:>6}  {name}")
        return 0

    spec = json.loads(args.chains.read_text()) if args.chains else NODE_CHAINS
    stack = args.stack or configured(REPO / spec["config"], spec["key"])
    margin = spec["margin"]
    failed = False
    for chain, patterns in spec["chains"].items():
        total = 0
        parts = []
        for pattern in patterns:
            matched = [
                frame
                for name, frame in frames.items()
                if pattern in name and frame is not None
            ]
            if not matched:
                print(f"FAIL {chain}: no function matches {pattern!r}")
                failed = True
                continue
            total += max(matched)
            parts.append(f"{pattern} {max(matched)}")
        ok = total + margin <= stack
        failed = failed or not ok
        verdict = "ok  " if ok else "FAIL"
        print(
            f"{verdict} {chain}: {total} + {margin} margin of {stack}"
            f" ({stack - total - margin} spare): {', '.join(parts)}"
        )
    print(f"stack budget {'failed' if failed else 'ok'}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
