"""The map, checked on both sides: mesh_host.py and tallowd read the same lines.

The lines are @U frames built here, so their hex stays valid. The fixture files
live beside tallowd, because the build container cannot see tools/.

  .venv/bin/python -m unittest tools/tests/test_map_fixture.py
  .venv/bin/python tools/tests/test_map_fixture.py --write   rebuild both fixture files
"""

from __future__ import annotations

import json
import struct
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))

import mesh_host as host  # noqa: E402

FIXTURE = ROOT / "host" / "tallowd" / "tests" / "fixtures" / "map"
LINES_FILE = FIXTURE / "map_lines.txt"
MAP_FILE = FIXTURE / "map.json"

# Half a second past a whole second, so round() has a half to resolve. tallowd's
# test uses the same number.
FIXED_NOW = 1798761600.5

ROOT_NODE = "541e99a9"
LEAF_NODE = "2c7f0b11"
EARBUDS = "C011223344AA"  # ble-random, rotates in the field
SOUNDBAR = "23000011A038"  # classic
TIE = "102030405060"  # ble-public, heard by both nodes at the same strength
ODD_KIND = "0A0B0C0D0E0F"  # a transport code this build does not know


def mac(hex_text: str) -> bytes:
    return bytes.fromhex(hex_text)


def record(addr: str, kind: int, rssi: int, count: int, age: int) -> bytes:
    """One report record: address, kind, best RSSI, count, age in seconds."""
    return mac(addr) + bytes([kind]) + struct.pack("b", rssi) + bytes([count, age])


def name_record(addr: str, kind: int, company: int, device_class: int, name: bytes) -> bytes:
    """One name record. 0xFFFF is no company, and class zero is unknown."""
    return (
        mac(addr)
        + bytes([kind])
        + company.to_bytes(2, "little")
        + device_class.to_bytes(3, "little")
        + bytes([len(name)])
        + name
    )


def stats(**fields: int) -> bytes:
    """An 88-byte Stats payload. Named fields are the 21 counters; the rest are the tail."""
    words = {name: 0 for name in host.STAT_FIELDS}
    words.update({key: value for key, value in fields.items() if key in words})
    tail = {key: fields.get(key, 0) for key in ("rank", "neighbours", "parent_rssi", "parent_quality")}
    return (
        struct.pack("<21I", *(words[name] for name in host.STAT_FIELDS))
        + bytes([tail["rank"], tail["neighbours"]])
        + struct.pack("b", tail["parent_rssi"])
        + bytes([tail["parent_quality"]])
    )


def route(uptime: int, old: int, new: int, reason: int) -> bytes:
    return struct.pack("<III", uptime, old, new) + bytes([reason])


def up(frame: int, origin: str, seq: int, hops: int, rssi: int, payload: bytes = b"", boot: int = 1000) -> str:
    words = ["@U", str(frame), origin, str(boot), str(seq), str(hops), str(rssi)]
    if payload:
        words.append(payload.hex())
    return " ".join(words)


def build_lines() -> list[str]:
    """The fixture, in the order a root would hear it. Comments say what each line tests."""
    lines = [
        # The leaf hears the earbuds at -55 and the tie device at -60, and an unknown kind.
        up(
            host.FRAME_REPORT,
            LEAF_NODE,
            1,
            1,
            -58,
            record(EARBUDS, 1, -55, 2, 1)
            + record(TIE, 0, -60, 2, 0)
            + record(ODD_KIND, 9, -80, 1, 3),
        ),
        # The root hears the soundbar, the earbuds further off, and the tie device
        # at the same -60, so the leaf (heard first) wins the tie. The trailing four
        # bytes are a partial record, which the parse ignores.
        up(
            host.FRAME_REPORT,
            ROOT_NODE,
            1,
            0,
            0,
            record(SOUNDBAR, 2, -48, 5, 2)
            + record(EARBUDS, 1, -70, 3, 0)
            + record(TIE, 0, -60, 1, 4)
            + b"\x01\x02\x03\x04",
        ),
        # A name frame with two records: a classic device with no company, and the earbuds.
        up(
            host.FRAME_NAME,
            ROOT_NODE,
            2,
            0,
            0,
            name_record(SOUNDBAR, 2, 0xFFFF, 0x240404, b"Klipsch Soundbar")
            + name_record(EARBUDS, 1, 0x004C, 0, b"WF-1000XM5"),
        ),
        # The root's own stats, then the leaf's, which carries its parent.
        up(
            host.FRAME_STATS,
            ROOT_NODE,
            3,
            0,
            0,
            stats(uptime_s=600, heap_free=151234, heap_min=120000, heap_block=65536, adverts=5000,
                  inquiries=3, tx_ok=900, tx_failed=4, rank=0, neighbours=1, parent=0),
        ),
        up(
            host.FRAME_STATS,
            LEAF_NODE,
            4,
            1,
            -58,
            stats(uptime_s=312, heap_free=140000, heap_min=118000, heap_block=60000, adverts=2600,
                  inquiries=2, tx_ok=400, tx_failed=1, rank=1, neighbours=2,
                  parent=int(ROOT_NODE, 16), parent_rssi=-52, parent_quality=200),
        ),
        # A parent change, which tallowd prints but the map does not keep.
        up(host.FRAME_ROUTE, LEAF_NODE, 5, 1, -58, route(77, 0x0A0B0C0D, 0x01020304, 1)),
        # A probe frame, which the map ignores.
        up(host.FRAME_PROBE, LEAF_NODE, 6, 1, -58, struct.pack("<I", 7)),
        # Noise: a board's own serial lines, a line that is not @U, and malformed @U lines.
        "@P 00000000",
        "@B 642104ca 3594854591",
        "W (735) wifi:Password",
        "@U 2 zz",
        f"@U 2 {LEAF_NODE} 1 9 1 -60 zz",
    ]
    return lines


def write_fixture() -> None:
    FIXTURE.mkdir(parents=True, exist_ok=True)
    LINES_FILE.write_text("\n".join(build_lines()) + "\n", encoding="utf-8")
    MAP_FILE.write_text(map_text(LINES_FILE.read_text(encoding="utf-8").splitlines()), encoding="utf-8")
    print(f"wrote {LINES_FILE}")
    print(f"wrote {MAP_FILE}")


def map_text(lines: list[str]) -> str:
    """What mesh_host.py writes for these lines, read from its own file."""
    world = host.Map()
    with mock.patch.object(host.time, "time", return_value=FIXED_NOW):
        for line in lines:
            up_frame = host.decode_up(line)
            if up_frame:
                host.handle(up_frame, world, True)
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "map.json"
        world.save(str(path))
        return path.read_text(encoding="utf-8")


class Fixture(unittest.TestCase):
    def test_the_lines_file_is_what_the_helpers_build(self) -> None:
        self.assertEqual(LINES_FILE.read_text(encoding="utf-8"), "\n".join(build_lines()) + "\n")

    def test_the_map_file_is_what_mesh_host_writes_from_the_lines(self) -> None:
        text = map_text(LINES_FILE.read_text(encoding="utf-8").splitlines())
        self.assertEqual(MAP_FILE.read_text(encoding="utf-8"), text)

    def test_the_tie_goes_to_the_first_node_heard(self) -> None:
        saved = map_text(LINES_FILE.read_text(encoding="utf-8").splitlines())
        tie = next(value for key, value in json.loads(saved)["devices"].items() if key.startswith("10:20:30:40:50:60 "))
        self.assertEqual(tie["nearest"], LEAF_NODE)


class PythonRounding(unittest.TestCase):
    def test_python_rounds_halves_to_even(self) -> None:
        # tallowd's py_round is checked against these same values.
        self.assertEqual([round(x) for x in (0.5, 1.5, 2.5, 3.5, -2.5, 2.4, 2.6)], [0, 2, 2, 4, -2, 2, 3])
        self.assertEqual(round(FIXED_NOW), 1798761600)
        self.assertEqual(round(FIXED_NOW - 4), 1798761596)


if __name__ == "__main__":
    if "--write" in sys.argv:
        write_fixture()
        sys.exit(0)
    unittest.main()
