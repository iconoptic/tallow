"""The one reservations parser. Opens no port and no socket."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))

import fleet_catalog  # noqa: E402


class Reservations(unittest.TestCase):
    def test_a_reservation_outside_twenty_to_forty_nine_is_dropped(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "reservations"
            path.write_text(
                "aa:bb:cc:dd:ee:01,10.87.0.60\n"
                "aa:bb:cc:dd:ee:02,10.87.0.19\n"
                "aa:bb:cc:dd:ee:03,10.87.0.20\n"
                "aa:bb:cc:dd:ee:04,10.87.0.49\n"
            )
            found = fleet_catalog.load_reservations(path)
            self.assertEqual(
                found,
                {
                    "aa:bb:cc:dd:ee:03": "10.87.0.20",
                    "aa:bb:cc:dd:ee:04": "10.87.0.49",
                },
            )

    def test_an_uppercase_mac_is_kept_lowercased(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "reservations"
            path.write_text("AA:BB:CC:DD:EE:05,10.87.0.21\n")
            self.assertEqual(
                fleet_catalog.load_reservations(path),
                {"aa:bb:cc:dd:ee:05": "10.87.0.21"},
            )

    def test_comments_blank_lines_and_three_field_lines_are_dropped(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "reservations"
            path.write_text(
                "\n# a comment\naa:bb:cc:dd:ee:06,10.87.0.22,extra\n"
                "aa:bb:cc:dd:ee:07,10.87.0.22\n"
            )
            self.assertEqual(
                fleet_catalog.load_reservations(path),
                {"aa:bb:cc:dd:ee:07": "10.87.0.22"},
            )

    def test_an_unreadable_file_is_empty(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            self.assertEqual(fleet_catalog.load_reservations(Path(tmp)), {})

    def test_cam_push_refuses_an_address_dnsmasq_would_ignore(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            catalog = Path(tmp) / "devices.jsonl"
            reservations = Path(tmp) / "reservations"
            catalog.write_text(
                json.dumps(
                    {
                        "type": "esp32s3-cam",
                        "name": "toby-cam-9",
                        "wifi_mac": "aa:bb:cc:dd:ee:09",
                    }
                )
                + "\n"
            )
            reservations.write_text("aa:bb:cc:dd:ee:09,10.87.0.60\n")
            allowed = fleet_catalog.cam_reserved_addresses(catalog, reservations)
            self.assertNotIn("10.87.0.60", allowed)


if __name__ == "__main__":
    unittest.main()
