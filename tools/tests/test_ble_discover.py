"""Fingerprint tests for tools/ble_discover.py (runs without bleak installed)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from ble_discover import family_from_gatt, fingerprint_name  # noqa: E402


def gatt(*writes: str) -> dict:
    chars = [
        {"uuid": f"0000{u}-0000-1000-8000-00805f9b34fb", "properties": ["write-without-response"]}
        for u in writes
    ]
    return {"services": [{"characteristics": chars}]}


class FingerprintNameTest(unittest.TestCase):
    # Names taken from upstream: elkbledom README/manifest, led-ble model_db.py,
    # lednetwf_ble README/manifest, docs/03.
    CASES = {
        "ELK-BLEDOM": ["elk-bledom"],
        "ELK-BULB2": ["elk-bledom"],
        "MELK-OA10": ["elk-bledom"],
        "XROCKER": ["elk-bledom"],
        "DMRRBA-007": ["elk-bledom"],
        "LEDBLE": ["elk-bledom", "led-ble"],
        "LEDBLE-DE1254F9": ["elk-bledom", "led-ble"],
        "Triones:C10511000166": ["led-ble"],
        "Dream~MAC": ["led-ble"],
        "LEDBlue-C2AF4BD5": ["led-ble"],
        "QHM-095F": ["led-ble"],
        "AP-1234": ["led-ble"],
        "LEDnet1234": ["led-ble"],
        "LEDnetWF010097DAB37A": ["lednetwf"],
        "IOTBT65C": ["lednetwf"],
        "IOTWF12": ["lednetwf"],
        "LEDDMX-00-1234": ["leddmx"],
        "GATT-DEMO": ["mrstar"],
        "GVH5075_1234": ["govee"],
        "Apple Watch": [],
        "Happy Lighting": [],
        "MyLEDBLE": [],
    }

    def test_names(self) -> None:
        for name, expected in self.CASES.items():
            with self.subTest(name=name):
                self.assertEqual(fingerprint_name(name), expected)

    def test_empty(self) -> None:
        self.assertEqual(fingerprint_name(None), [])


class FamilyFromGattTest(unittest.TestCase):
    def test_elk_write_char_resolves_ledble_ambiguity(self) -> None:
        self.assertEqual(family_from_gatt(gatt("fff3"), ["elk-bledom", "led-ble"]), ["elk-bledom"])

    def test_led_ble_write_char_resolves_ledble_ambiguity(self) -> None:
        self.assertEqual(family_from_gatt(gatt("ffd9"), ["elk-bledom", "led-ble"]), ["led-ble"])

    def test_no_name_returns_all_gatt_hints(self) -> None:
        self.assertEqual(family_from_gatt(gatt("ffe1"), []), ["elk-bledom", "leddmx"])

    def test_read_only_chars_ignored(self) -> None:
        g = gatt("fff3")
        g["services"][0]["characteristics"][0]["properties"] = ["read", "notify"]
        self.assertEqual(family_from_gatt(g, []), [])


if __name__ == "__main__":
    unittest.main()
