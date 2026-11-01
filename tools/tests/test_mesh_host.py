"""The host end of the mesh: what it decodes and what it keeps."""

from __future__ import annotations

import contextlib
import io
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


class Routes(unittest.TestCase):
    def test_a_parent_change_decodes(self) -> None:
        payload = struct.pack("<III", 1234, 0x11223344, 0) + bytes([0])
        self.assertEqual(
            host.parse_route(payload),
            {"uptime_s": 1234, "old": "11223344", "new": None, "reason": "beacon timeout"},
        )

    def test_a_route_of_the_wrong_size_is_refused(self) -> None:
        self.assertIsNone(host.parse_route(bytes(12)))
        self.assertIsNone(host.parse_route(bytes(14)))

    def test_an_unknown_reason_is_named_as_such(self) -> None:
        self.assertEqual(host.parse_route(bytes(12) + bytes([9]))["reason"], "unknown")

    def test_a_route_frame_is_printed_with_its_reason(self) -> None:
        payload = struct.pack("<III", 77, 0x0A0B0C0D, 0x01020304) + bytes([1])
        up = {"frame": host.FRAME_ROUTE, "origin": "00000005", "payload": payload, "hops": 2, "rssi": -60}
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            host.handle(up, host.Map(), False)
        self.assertEqual(
            out.getvalue(),
            "route 00000005 up 77 s from 0a0b0c0d to 01020304 reason failed acks\n",
        )

    def test_a_route_frame_is_silent_during_a_trial(self) -> None:
        up = {"frame": host.FRAME_ROUTE, "origin": "00000005", "payload": bytes(13), "hops": 1, "rssi": -60}
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            host.handle(up, host.Map(), True)
        self.assertEqual(out.getvalue(), "")


class Delivery(unittest.TestCase):
    NODES = {"root": {"link_rssi": 0}, "leaf": {"link_rssi": -50}}

    def test_a_clean_run_is_measured_by_what_it_sent(self) -> None:
        rows = host.delivery({"leaf": set(range(1, 441))}, self.NODES, 45, 10)
        self.assertEqual(rows, [("leaf", 440, 440, 100.0)])

    def test_a_hole_in_the_middle_counts(self) -> None:
        rows = host.delivery({"leaf": set(range(1, 441)) - {200, 201}}, self.NODES, 45, 10)
        self.assertEqual(rows[0][1:3], (438, 440))

    def test_a_node_that_stops_partway_fails(self) -> None:
        rows = host.delivery({"leaf": set(range(1, 101))}, self.NODES, 45, 10)
        self.assertEqual(rows[0][2], 405)
        self.assertLess(rows[0][3], 95)

    def test_a_node_that_sends_nothing_fails(self) -> None:
        self.assertEqual(host.delivery({}, self.NODES, 45, 10), [("leaf", 0, 405, 0.0)])

    def test_the_root_is_not_expected_to_cross_the_air(self) -> None:
        self.assertEqual(host.delivery({}, {"root": {"link_rssi": 0}}, 45, 10), [])


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


class Watch(unittest.TestCase):
    """The paging list line the host sends, and the addresses it refuses."""

    def test_addresses_become_one_line_of_hex_pairs(self) -> None:
        line = host.watch_line(["AA:BB:CC:DD:EE:01", "10:20:30:40:50:60"])
        self.assertEqual(line, b"W aabbccddee01 102030405060\n")

    def test_case_does_not_change_the_line(self) -> None:
        self.assertEqual(host.watch_line(["aa:bb:cc:dd:ee:01"]), host.watch_line(["AA:BB:CC:DD:EE:01"]))

    def test_no_address_is_a_line_that_clears_the_list(self) -> None:
        self.assertEqual(host.watch_line([]), b"W\n")

    def test_sixteen_addresses_fit_the_firmware_line(self) -> None:
        addresses = [f"00:00:00:00:00:{index:02X}" for index in range(16)]
        line = host.watch_line(addresses).rstrip(b"\n")
        # The firmware reads at most 220 bytes of a line into its buffer.
        self.assertEqual(len(line), 2 + 16 * 13 - 1)
        self.assertLessEqual(len(line), 220)

    def test_seventeen_addresses_are_refused(self) -> None:
        with self.assertRaisesRegex(ValueError, "at most 16"):
            host.watch_line([f"00:00:00:00:00:{index:02X}" for index in range(17)])

    def test_bad_addresses_are_refused(self) -> None:
        bad = [
            "AA:BB:CC:DD:EE",            # five pairs
            "AA:BB:CC:DD:EE:FF:00",      # seven pairs
            "AA:BB:CC:DD:EE:GG",         # not hex
            "AA:BB:CC:DD:EE:F",          # a single digit
            "AA:BB:CC:DD:EE:FFF",        # three digits
            "AABBCCDDEEFF",              # no separators
            "AA-BB-CC-DD-EE-FF",         # dashes
            " AA:BB:CC:DD:EE:FF",        # leading space
            "AA:BB:CC:DD:EE:FF\nW",      # a newline inside the address
            "",
        ]
        for text in bad:
            with self.subTest(text=text), self.assertRaises(ValueError):
                host.watch_line([text])

    def test_one_bad_address_refuses_the_whole_list(self) -> None:
        with self.assertRaises(ValueError):
            host.watch_line(["AA:BB:CC:DD:EE:FF", "AA:BB:CC:DD:EE"])


if __name__ == "__main__":
    unittest.main()
