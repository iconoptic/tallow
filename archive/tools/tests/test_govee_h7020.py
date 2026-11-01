"""Offline checks for the H7020 probe. The live string is not required."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import govee_h7020 as probe  # noqa: E402


class CodecGateTest(unittest.TestCase):
    def test_scan_advertisement_is_pact_1_code_1(self) -> None:
        # captures/scan.json manufacturer id 34818, payload ec00010101.
        advert = probe.parse_govee_advertisement({34818: bytes.fromhex("ec00010101")})
        self.assertIsNotNone(advert)
        assert advert is not None
        self.assertEqual((advert.pact_type, advert.pact_code), (1, 1))
        self.assertFalse(advert.supports_encryption)

    def test_qualified_pact_builds_a_red_frame(self) -> None:
        profile = probe.sized_profile(probe.profile_for(1, 1), 1)
        self.assertTrue(profile.supports_rgb)
        packet = probe.build_color_rgb(255, 0, 0, probe.MODEL, profile=profile)
        self.assertEqual(len(packet), 20)
        self.assertEqual(packet[0], 0x33)
        self.assertIn(bytes.fromhex("ff7f0000"), packet)

    def test_two_strings_use_a_wider_mask(self) -> None:
        one = probe.build_color_rgb(
            255, 0, 0, probe.MODEL, profile=probe.sized_profile(probe.profile_for(2, 1), 1)
        )
        two = probe.build_color_rgb(
            255, 0, 0, probe.MODEL, profile=probe.sized_profile(probe.profile_for(2, 1), 2)
        )
        self.assertIn(bytes.fromhex("ffffff3f"), two)
        self.assertNotEqual(one, two)

    def test_other_pacts_drop_color(self) -> None:
        profile = probe.profile_for(9, 9)
        self.assertFalse(profile.supports_rgb)
        self.assertNotIn((9, 9), probe.QUALIFIED_PACTS)


class SceneScriptTest(unittest.TestCase):
    def test_catalogue_lists_names_without_parameter_bytes(self) -> None:
        scenes = probe.list_scenes()
        names = [scene["name"] for scene in scenes]
        self.assertIn("Sunset Glow", names)
        self.assertIn("Valentine's Day", names)
        self.assertEqual(len(names), len(set(names)))
        self.assertGreaterEqual(len(scenes), 51)
        for scene in scenes:
            self.assertNotIn("param", scene)
            self.assertIn(scene["category"], {"Natural", "Life", "Emotion", "Festival"})

    def test_every_scene_builds_an_ordered_script(self) -> None:
        for scene in probe.list_scenes():
            info, packets = probe.build_scene_script(scene["name"])
            self.assertEqual(info["name"], scene["name"])
            self.assertGreaterEqual(len(packets), 1)
            self.assertTrue(all(isinstance(packet, bytes) and len(packet) == 20 for packet in packets))
            self.assertEqual(packets[-1][0], 0x33)
            if len(packets) > 1:
                self.assertTrue(all(packet[0] == 0xA3 for packet in packets[:-1]))

    def test_speed_uses_the_catalogue_range(self) -> None:
        info, _packets = probe.build_scene_script("Sunset Glow", 0)
        self.assertEqual(info["speed"], 0)
        with self.assertRaises(ValueError):
            probe.build_scene_script("Sunset Glow", 9)
        with self.assertRaises(ValueError):
            probe.build_scene_script("not a scene")

    def test_paint_groups_are_separate_packets(self) -> None:
        profile = probe.sized_profile(probe.profile_for(1, 1), 1)
        packets = probe.build_paint_script(
            [
                {"bulbs": [1, 2, 3], "rgb": [255, 80, 0]},
                {"bulbs": [4, 5], "rgb": [0, 0, 255]},
            ],
            profile,
        )
        self.assertEqual(len(packets), 2)
        self.assertTrue(all(len(packet) == 20 and packet[0] == 0x33 for packet in packets))
        self.assertNotEqual(packets[0], packets[1])
        with self.assertRaises(ValueError):
            probe.build_paint_script(
                [
                    {"bulbs": [1, 2], "rgb": [1, 0, 0]},
                    {"bulbs": [2, 3], "rgb": [0, 1, 0]},
                ],
                profile,
            )
        with self.assertRaises(ValueError):
            probe.build_paint_script([{"bulbs": [16], "rgb": [1, 0, 0]}], profile)


if __name__ == "__main__":
    unittest.main()
