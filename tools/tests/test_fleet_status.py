"""tools/fleet_status.py. Opens no port and no socket: the probe is a stub."""

from __future__ import annotations

import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))

import cam_push_send  # noqa: E402
import fleet_status  # noqa: E402

NODE = {"type": "esp32-mesh", "name": "node-4", "chip": "esp32", "wifi_mac": "20:9b:a9:97:9f:28", "mesh_id": "289f97a9"}
CAM = {"type": "esp32s3-cam", "name": "toby-cam-1", "chip": "esp32s3", "wifi_mac": "44:bd:8d:7a:50:cc"}


class Flash(unittest.TestCase):
    def test_current_stale_and_never(self) -> None:
        journal = [{"name": "node-4", "wifi_mac": "20:9b:a9:97:9f:28", "image_id": "new"}]
        self.assertEqual(fleet_status.flash_state(NODE, journal, "new", "cam"), "current")
        self.assertEqual(fleet_status.flash_state(NODE, journal, "newer", "cam"), "stale")
        self.assertEqual(fleet_status.flash_state(NODE, [], "new", "cam"), "never")
        self.assertEqual(fleet_status.flash_state(NODE, journal, "", "cam"), "?")

    def test_a_row_with_another_mac_is_not_this_board(self) -> None:
        journal = [{"name": "node-4", "wifi_mac": "20:9b:a9:99:32:0c", "image_id": "new"}]
        self.assertEqual(fleet_status.flash_state(NODE, journal, "new", ""), "never")

    def test_an_old_row_without_a_mac_matches_by_name(self) -> None:
        journal = [{"name": "node-4", "image_id": "old"}]
        self.assertEqual(fleet_status.flash_state(NODE, journal, "old", ""), "current")

    def test_a_cam_compares_with_the_cam_image(self) -> None:
        journal = [{"name": "toby-cam-1", "wifi_mac": "44:bd:8d:7a:50:cc", "image_id": "c1"}]
        self.assertEqual(fleet_status.flash_state(CAM, journal, "n1", "c1"), "current")


class Heard(unittest.TestCase):
    def test_a_node_in_the_map_shows_hops_and_uptime(self) -> None:
        nodes = {"289f97a9": {"hops": 2, "uptime_s": 3725}}
        self.assertEqual(fleet_status.heard_node(NODE, nodes), "hops 2, up 1h02m")
        self.assertEqual(fleet_status.heard_node(NODE, {}), "no")

    def test_an_unknown_mesh_id_and_lease_are_listed(self) -> None:
        nodes = {"289f97a9": {"hops": 0}, "deadbeef": {"hops": 1}}
        leases = [("b8:1f:3f:ab:c5:60", "10.87.0.60", "tallow-cam-abc560")]
        table = fleet_status.rows_for([NODE], [], nodes, {}, leases, "", "", None)
        text = fleet_status.render(table, [NODE], nodes, 12.0, leases)
        self.assertIn("unknown mesh ids: deadbeef", text)
        self.assertIn("unknown leases: b8:1f:3f:ab:c5:60 10.87.0.60", text)
        self.assertNotIn("289f97a9,", text)


class CamAddress(unittest.TestCase):
    def test_a_reservation_beats_a_lease(self) -> None:
        reservations = {"44:bd:8d:7a:50:cc": "10.87.0.23"}
        leases = [("44:bd:8d:7a:50:cc", "10.87.0.77", "x")]
        self.assertEqual(fleet_status.cam_address(CAM, reservations, leases), "10.87.0.23")
        self.assertEqual(fleet_status.cam_address(CAM, {}, leases), "10.87.0.77 (lease)")
        self.assertEqual(fleet_status.cam_address(CAM, {}, []), "")

    def test_the_probe_result_is_shown(self) -> None:
        reservations = {"44:bd:8d:7a:50:cc": "10.87.0.23"}
        asked = []

        def probe(address: str) -> bool:
            asked.append(address)
            return True

        table = fleet_status.rows_for([CAM], [], {}, reservations, [], "", "", probe)
        self.assertEqual(table[0][5], "6053 up")
        self.assertEqual(asked, ["10.87.0.23"])
        table = fleet_status.rows_for([CAM], [], {}, reservations, [], "", "", lambda _a: False)
        self.assertEqual(table[0][5], "6053 down")


class Main(unittest.TestCase):
    def test_a_missing_map_and_leases_still_print_a_table(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            (base / "devices.jsonl").write_text(json.dumps(NODE) + "\n" + json.dumps(CAM) + "\n")
            out = io.StringIO()
            with redirect_stdout(out):
                code = fleet_status.main(
                    [
                        "--catalog", str(base / "devices.jsonl"),
                        "--journal", str(base / "none.jsonl"),
                        "--map", str(base / "none.json"),
                        "--reservations", str(base / "none"),
                        "--leases", str(base / "none.leases"),
                        "--no-probe",
                    ]
                )
            self.assertEqual(code, 0)
            text = out.getvalue()
            self.assertTrue(text.startswith("NAME"))
            self.assertIn("node-4", text)
            self.assertIn("no address", text)
            self.assertIn("no map", text)


class PushAllowList(unittest.TestCase):
    def test_only_a_catalog_cam_reservation_is_allowed(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            catalog = Path(tmp) / "devices.jsonl"
            reservations = Path(tmp) / "reservations"
            catalog.write_text(json.dumps(CAM) + "\n" + json.dumps(NODE) + "\n")
            reservations.write_text("44:bd:8d:7a:50:cc,10.87.0.23\n20:9b:a9:97:9f:28,10.87.0.24\n")
            allowed = cam_push_send.cam_addresses(catalog, reservations)
            self.assertEqual(allowed, {"10.87.0.23"})
            sig = bytes([2]) + (1).to_bytes(4, "little") + bytes(64)

            def never(*_args, **_kwargs):
                raise AssertionError("no socket may open")

            ok, text = cam_push_send.send("10.87.0.24", b"x", sig, connect=never, allowed=allowed)
            self.assertFalse(ok)
            self.assertIn("not reserved", text)


    def test_an_unreadable_journal_line_is_skipped(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            journal = root / "applied.jsonl"
            journal.write_text('{"name": "node-4"}\n{not json\n')
            catalog = root / "devices.jsonl"
            catalog.write_text("")
            err = io.StringIO()
            with redirect_stderr(err):
                code = fleet_status.main(
                    [
                        "--catalog",
                        str(catalog),
                        "--journal",
                        str(journal),
                        "--map",
                        str(root / "map.json"),
                        "--reservations",
                        str(root / "reservations"),
                        "--leases",
                        str(root / "leases"),
                        "--no-probe",
                    ]
                )
            self.assertEqual(code, 0, err.getvalue())
            self.assertIn(f"skipped 1 unreadable lines in {journal}", err.getvalue())


if __name__ == "__main__":
    unittest.main()
