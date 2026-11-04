"""Panel cam sync. Opens no socket and no port."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from tools.sync_panel_cams import catalog_cams, load_reservations, sync_inventory, write_inventory


CATALOG = [
    {
        "type": "esp32-mesh",
        "name": "node-0",
        "wifi_mac": "70:4b:ca:04:21:64",
    },
    {
        "type": "esp32s3-cam",
        "name": "toby-cam-0",
        "wifi_mac": "44:bd:8d:7a:51:60",
    },
    {
        "type": "esp32s3-cam",
        "name": "cam-1",
        "wifi_mac": "unknown",
    },
]
RESERVATIONS = {"44:bd:8d:7a:51:60": "10.87.0.21"}


class Sync(unittest.TestCase):
    def test_a_mesh_node_and_an_unknown_mac_are_not_cams(self) -> None:
        self.assertEqual(catalog_cams(CATALOG, RESERVATIONS), [("toby-cam-0", "10.87.0.21")])

    def test_a_cam_that_answers_is_added(self) -> None:
        page = {"title": "Toby", "cams": []}
        updated, notes = sync_inventory(page, [("toby-cam-0", "10.87.0.21")], {"10.87.0.21"})
        self.assertEqual(notes, ["added toby-cam-0 at 10.87.0.21"])
        self.assertEqual(updated["title"], "Toby")
        self.assertEqual(
            updated["cams"],
            [{"id": "toby-cam-0", "name": "toby-cam-0", "address": "10.87.0.21"}],
        )

    def test_a_cam_that_does_not_answer_is_not_added(self) -> None:
        page = {"title": "Toby", "cams": []}
        updated, notes = sync_inventory(page, [("toby-cam-0", "10.87.0.21")], set())
        self.assertEqual(notes, [])
        self.assertEqual(updated["cams"], [])

    def test_the_row_on_that_address_takes_the_catalog_id(self) -> None:
        page = {
            "title": "Toby",
            "cams": [{"id": "toby-cam-1", "name": "Cam 1", "address": "10.87.0.21"}],
        }
        updated, notes = sync_inventory(page, [("toby-cam-0", "10.87.0.21")], set())
        self.assertEqual(notes, ["toby-cam-1 at 10.87.0.21 is toby-cam-0"])
        self.assertEqual(
            updated["cams"],
            [{"id": "toby-cam-0", "name": "Cam 1", "address": "10.87.0.21"}],
        )

    def test_another_camera_stays(self) -> None:
        page = {
            "cams": [
                {"id": "yard", "name": "Yard", "address": "10.87.0.23"},
                {"id": "toby-cam-1", "name": "Cam 1", "address": "10.87.0.21"},
            ]
        }
        updated, _notes = sync_inventory(page, [("toby-cam-0", "10.87.0.21")], {"10.87.0.21"})
        self.assertEqual(updated["cams"][0]["id"], "yard")
        self.assertEqual(updated["cams"][1]["id"], "toby-cam-0")
        self.assertNotIn("title", updated)

    def test_reservations_keep_a_node_address(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "reservations"
            path.write_text(
                "44:BD:8D:7A:51:60,10.87.0.21\n"
                "70:4b:ca:04:21:64,10.87.0.1\n"
                "not-a-mac,10.87.0.22\n",
                encoding="utf-8",
            )
            self.assertEqual(load_reservations(path), {"44:bd:8d:7a:51:60": "10.87.0.21"})

    def test_a_change_is_written_and_reread(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "cams.json"
            page = {"title": "Toby", "cams": []}
            updated, _notes = sync_inventory(
                page, [("toby-cam-0", "10.87.0.21")], {"10.87.0.21"}
            )
            write_inventory(path, updated)
            self.assertEqual(json.loads(path.read_text(encoding="utf-8")), updated)
            self.assertFalse(path.with_name(path.name + ".tmp").exists())


    def test_a_merge_keeps_the_rotation_of_the_row_it_drops(self) -> None:
        page = {
            "cams": [
                {"id": "toby-cam-0", "name": "Old", "address": "10.87.0.22", "rotation": 90},
                {"id": "other", "name": "Cam", "address": "10.87.0.21"},
            ]
        }
        updated, _notes = sync_inventory(page, [("toby-cam-0", "10.87.0.21")], set())
        kept = next(row for row in updated["cams"] if row["address"] == "10.87.0.21")
        self.assertEqual(kept["id"], "toby-cam-0")
        self.assertEqual(kept["rotation"], 90)
        self.assertFalse(any(row["address"] == "10.87.0.22" for row in updated["cams"]))


if __name__ == "__main__":
    unittest.main()
