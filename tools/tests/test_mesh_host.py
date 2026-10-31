"""The host end of the mesh: what it decodes and what it keeps."""

from __future__ import annotations

import json
import struct
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import mesh_host as host  # noqa: E402


def record(addr: bytes, kind: int, rssi: int, count: int, age: int) -> bytes:
    return addr + bytes([kind]) + struct.pack("b", rssi) + bytes([count, age])


def named(addr: bytes, kind: int, company: int, name: bytes, cod: int = 0) -> bytes:
    return addr + bytes([kind]) + company.to_bytes(2, "little") + cod.to_bytes(3, "little") + bytes([len(name)]) + name


class Decode(unittest.TestCase):
    def test_report_records(self) -> None:
        payload = record(b"\xaa" * 6, 0, -60, 3, 2) + record(b"\xbb" * 6, 2, -90, 1, 0)
        out = host.parse_report(payload)
        self.assertEqual(out[0], {"addr": "AA:AA:AA:AA:AA:AA", "kind": "ble-public", "rssi": -60, "count": 3, "age_s": 2})
        self.assertEqual(out[1]["kind"], "classic")

    def test_a_partial_report_record_is_ignored(self) -> None:
        self.assertEqual(len(host.parse_report(record(b"\xaa" * 6, 0, -60, 3, 2) + b"\x01\x02")), 1)

    def test_names(self) -> None:
        out = host.parse_names(named(b"\x01" * 6, 1, 0x004C, b"Flip 5") + named(b"\x02" * 6, 2, 0xFFFF, b"x", 0x240404))
        self.assertEqual(out[0]["name"], "Flip 5")
        self.assertEqual(out[0]["company"], 0x004C)
        self.assertIsNone(out[1]["company"])
        self.assertEqual(out[1]["class"], 0x240404)

    def test_a_name_past_the_end_stops_the_parse(self) -> None:
        self.assertEqual(host.parse_names(named(b"\x01" * 6, 1, 1, b"abc")[:-1]), [])

    def test_a_name_that_is_not_utf8_survives(self) -> None:
        self.assertEqual(len(host.parse_names(named(b"\x01" * 6, 1, 1, b"\xff\xfe"))), 1)

    def test_stats(self) -> None:
        fields = list(range(1, 22))
        payload = struct.pack("<21I", *fields) + bytes([2, 3]) + struct.pack("b", -55) + bytes([16])
        out = host.parse_stats(payload)
        self.assertEqual(out["uptime_s"], 1)
        self.assertEqual(out["inquiry_errors"], 21)
        self.assertEqual((out["rank"], out["neighbours"], out["parent_rssi"], out["parent_quality"]), (2, 3, -55, 16))

    def test_stats_of_the_wrong_size_are_refused(self) -> None:
        self.assertIsNone(host.parse_stats(bytes(76)))

    def test_up_lines(self) -> None:
        up = host.decode_up("@U 2 541e99a9 123 7 1 -60 0a0b")
        self.assertEqual((up["frame"], up["origin"], up["hops"], up["rssi"], up["payload"]), (2, "541e99a9", 1, -60, b"\x0a\x0b"))

    def test_serial_noise_is_not_an_up_line(self) -> None:
        for line in ("", "W (735) wifi:Password", "@U 2", "@U x y z 1 2 3 zz", "@B 642104ca 3594854591"):
            self.assertIsNone(host.decode_up(line))


class WorldMap(unittest.TestCase):
    def test_the_nearest_node_is_the_strongest(self) -> None:
        world = host.Map()
        for node, rssi in (("a", -80), ("b", -50)):
            world.sighting(node, {"addr": "AA", "kind": "classic", "rssi": rssi, "count": 2, "age_s": 0}, 100.0)
        world.name({"addr": "AA", "kind": "classic", "name": "Flip 5", "company": None, "class": 0x240404})
        out = Path(self.id().replace(".", "_") + ".json")
        try:
            world.save(str(out))
            saved = json.loads(out.read_text())
        finally:
            out.unlink(missing_ok=True)
        device = saved["devices"]["AA classic"]
        self.assertEqual(device["nearest"], "b")
        self.assertEqual(device["name"], "Flip 5")
        self.assertEqual(device["heard_by"]["a"]["hits"], 2)

    def test_one_address_on_two_transports_stays_two_devices(self) -> None:
        world = host.Map()
        world.sighting("a", {"addr": "AA", "kind": "classic", "rssi": -50, "count": 1, "age_s": 0}, 1.0)
        world.sighting("a", {"addr": "AA", "kind": "ble-public", "rssi": -50, "count": 1, "age_s": 0}, 1.0)
        self.assertEqual(len(world.devices), 2)


if __name__ == "__main__":
    unittest.main()
