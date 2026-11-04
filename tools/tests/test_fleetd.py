"""Decisions in tools/fleetd.sh. Opens no port. Reads no key."""

from __future__ import annotations

import json
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CATALOG = ROOT / "firmware" / "mesh" / "devices.jsonl"
BOARDS = ROOT / "firmware" / "mesh" / "boards.tsv"


def shell(body: str) -> subprocess.CompletedProcess[str]:
    script = f"set -euo pipefail\nsource tools/fleetd.sh\n{body}\n"
    return subprocess.run(
        ["bash", "-c", script], cwd=ROOT, check=False, capture_output=True, text=True
    )


class Catalog(unittest.TestCase):
    def test_the_catalog_matches_boards_tsv(self) -> None:
        result = shell(f'catalog_agrees "{CATALOG}" "{BOARDS}"')
        self.assertEqual(result.returncode, 0, result.stderr[-400:])

    def test_push_stays_closed(self) -> None:
        result = shell(f'catalog_push_closed "{CATALOG}"')
        self.assertEqual(result.returncode, 0, result.stderr[-400:])

    def test_a_different_mac_is_a_disagreement(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            boards = Path(tmp) / "boards.tsv"
            catalog = Path(tmp) / "devices.jsonl"
            boards.write_text("node-0\t70:4b:ca:04:21:64\tesp32\t12\tnote\n")
            catalog.write_text(
                '{"type":"esp32-mesh","name":"node-0","chip":"esp32",'
                '"usb_path":"12","wifi_mac":"20:9b:a9:99:1e:54","flash_ok":true,"push_ok":false}\n'
            )
            result = shell(f'catalog_agrees "{catalog}" "{boards}"')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("disagrees", result.stderr)

    def test_a_board_missing_from_the_catalog_is_a_disagreement(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            boards = Path(tmp) / "boards.tsv"
            catalog = Path(tmp) / "devices.jsonl"
            boards.write_text(
                "node-0\t70:4b:ca:04:21:64\tesp32\t12\tnote\n"
                "node-1\t20:9b:a9:99:1e:54\tesp32\t7\tnote\n"
            )
            catalog.write_text(
                '{"type":"esp32-mesh","name":"node-0","chip":"esp32",'
                '"usb_path":"12","wifi_mac":"70:4b:ca:04:21:64","flash_ok":true,"push_ok":false}\n'
            )
            result = shell(f'catalog_agrees "{catalog}" "{boards}"')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("node-1", result.stderr)


class Decide(unittest.TestCase):
    def test_an_unknown_path_is_refused(self) -> None:
        result = shell("fleet_decide '' /dev/ttyACM1 0 1 0 0")
        self.assertEqual(result.returncode, 0, result.stderr[-400:])
        self.assertEqual(result.stdout.strip(), "refuse")

    def test_a_mesh_node_on_ttyacm_waits(self) -> None:
        result = shell("fleet_decide esp32-mesh /dev/ttyACM0 0 0 1 0")
        self.assertEqual(result.stdout.strip(), "wait")

    def test_a_mesh_node_waits_until_every_node_is_on_ttyusb(self) -> None:
        result = shell("fleet_decide esp32-mesh /dev/ttyUSB0 0 0 1 0")
        self.assertEqual(result.stdout.strip(), "wait")

    def test_a_ready_mesh_node_is_a_flash(self) -> None:
        result = shell("fleet_decide esp32-mesh /dev/ttyUSB0 0 1 1 0")
        self.assertEqual(result.stdout.strip(), "flash-node")

    def test_a_cam_with_flash_ok_is_a_flash(self) -> None:
        result = shell("fleet_decide esp32s3-cam /dev/ttyUSB1 0 0 1 0")
        self.assertEqual(result.stdout.strip(), "flash-cam")

    def test_a_cam_without_flash_ok_is_refused(self) -> None:
        result = shell("fleet_decide esp32s3-cam /dev/ttyUSB1 0 1 0 0")
        self.assertEqual(result.stdout.strip(), "refuse")

    def test_an_applied_image_is_left_closed(self) -> None:
        result = shell("fleet_decide esp32-mesh /dev/ttyUSB0 1 1 1 0")
        self.assertEqual(result.stdout.strip(), "skip")

    def test_an_attempt_already_made_does_not_flash_again(self) -> None:
        result = shell("fleet_decide esp32-mesh /dev/ttyUSB0 0 1 1 1")
        self.assertEqual(result.stdout.strip(), "wait")
        result = shell("fleet_decide esp32s3-cam /dev/ttyUSB1 0 0 1 1")
        self.assertEqual(result.stdout.strip(), "wait")


class Plan(unittest.TestCase):
    def test_node_1_waits_while_node_0_is_still_boot_cdc(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            catalog = Path(tmp) / "devices.jsonl"
            catalog.write_text(
                '{"type":"esp32-mesh","name":"node-1","chip":"esp32","usb_path":"7",'
                '"wifi_mac":"20:9b:a9:99:1e:54","flash_ok":true,"push_ok":false}\n'
                '{"type":"esp32-mesh","name":"node-0","chip":"esp32","usb_path":"12",'
                '"wifi_mac":"70:4b:ca:04:21:64","flash_ok":true,"push_ok":false}\n'
                '{"type":"esp32s3-cam","name":"toby-cam-0","chip":"esp32s3","usb_path":"11",'
                '"wifi_mac":"unknown","flash_ok":true,"push_ok":false}\n',
                encoding="utf-8",
            )
            snapshot = (
                "/dev/ttyUSB0\t7\tCP2102_USB_to_UART_Bridge_Controller\n"
                "/dev/ttyUSB1\t11\tCP2102_USB_to_UART_Bridge_Controller\n"
                "/dev/ttyACM0\t12\tDFRobot_Boot_CDC\n"
                "/dev/ttyACM1\t2\tUSB_JTAG_serial_debug_unit\n"
            )
            result = shell(
                f"printf '%s' '{snapshot}' | fleet_plan \"{catalog}\" /nonexistent/journal aabbcc ''"
            )
        self.assertEqual(result.returncode, 0, result.stderr[-400:])
        lines = result.stdout.splitlines()
        self.assertEqual(lines[0], "mesh_ready\t0")
        by_path = {line.split("|")[1]: line.split("|") for line in lines[1:]}
        self.assertEqual(by_path["7"][4], "wait")
        self.assertEqual(by_path["7"][2], "node-1")
        self.assertEqual(by_path["12"][4], "wait")
        self.assertEqual(by_path["12"][2], "node-0")
        self.assertEqual(by_path["11"][4], "flash-cam")
        self.assertEqual(by_path["11"][2], "toby-cam-0")
        self.assertEqual(by_path["2"][4], "refuse")
        self.assertEqual(by_path["2"][2], "")

    def test_both_nodes_on_ttyusb_are_flashed_and_a_known_image_is_skipped(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            catalog = Path(tmp) / "devices.jsonl"
            catalog.write_text(
                '{"type":"esp32-mesh","name":"node-0","chip":"esp32","usb_path":"7",'
                '"wifi_mac":"70:4b:ca:04:21:64","flash_ok":true,"push_ok":false}\n'
                '{"type":"esp32-mesh","name":"node-4","chip":"esp32","usb_path":"12",'
                '"wifi_mac":"20:9b:a9:97:9f:28","flash_ok":true,"push_ok":false}\n',
                encoding="utf-8",
            )
            journal = Path(tmp) / "applied.jsonl"
            journal.write_text(
                '{"usb_path":"7","name":"node-0","image_id":"abc","command":"flash"}\n'
            )
            snapshot = "/dev/ttyUSB0\t7\tCP2102\n/dev/ttyUSB1\t12\tCP2102\n"
            result = shell(
                f"printf '%s' '{snapshot}' | fleet_plan \"{catalog}\" \"{journal}\" abc ''"
            )
            self.assertEqual(result.returncode, 0, result.stderr[-400:])
            by_path = {
                line.split("|")[1]: line.split("|")[4]
                for line in result.stdout.splitlines()
                if not line.startswith("mesh_ready")
            }
            self.assertTrue(result.stdout.startswith("mesh_ready\t1"), result.stdout)
            self.assertEqual(by_path["7"], "skip")
            self.assertEqual(by_path["12"], "flash-node")

    def test_a_new_mac_on_a_known_path_is_not_that_board(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            catalog = Path(tmp) / "devices.jsonl"
            catalog.write_text(
                '{"type":"esp32s3-cam","name":"toby-cam-0","chip":"esp32s3","usb_path":"2",'
                '"wifi_mac":"44:bd:8d:7a:51:60","flash_ok":true,"push_ok":false}\n'
                '{"type":"esp32-mesh","name":"node-0","chip":"esp32","usb_path":"7",'
                '"wifi_mac":"70:4b:ca:04:21:64","flash_ok":true,"push_ok":false}\n',
                encoding="utf-8",
            )
            journal = Path(tmp) / "applied.jsonl"
            journal.write_text(
                '{"usb_path":"2","name":"toby-cam-0","image_id":"abc","command":"cam-flash"}\n'
                '{"usb_path":"7","name":"node-0","image_id":"abc","command":"flash"}\n'
            )
            snapshot = (
                "/dev/ttyACM0\t2\tUSB_JTAG_serial_debug_unit\t44:BD:8D:7C:20:C4\n"
                "/dev/ttyUSB0\t7\tCP2102\t0001\n"
                "/dev/ttyACM1\t9\tUSB_JTAG_serial_debug_unit\t44:bd:8d:7a:51:60\n"
            )
            result = shell(
                f"printf '%s' '{snapshot}' | fleet_plan \"{catalog}\" \"{journal}\" abc abc"
            )
            self.assertEqual(result.returncode, 0, result.stderr[-400:])
            by_path = {
                line.split("|")[1]: line.split("|")
                for line in result.stdout.splitlines()[1:]
            }
            self.assertEqual(by_path["2"][2], "")
            self.assertEqual(by_path["2"][4], "refuse")
            self.assertEqual(by_path["7"][2], "node-0")
            self.assertEqual(by_path["7"][4], "skip")
            self.assertEqual(by_path["9"][2], "toby-cam-0")
            self.assertEqual(by_path["9"][4], "flash-cam")


class Journal(unittest.TestCase):
    def test_node_4_on_node_3_s_old_path_is_flashed(self) -> None:
        # 2026-11-02: node-4 was learned on path 12 after node-3 was written there,
        # and fleetd logged skip until it stopped. The path is not the board.
        with tempfile.TemporaryDirectory() as tmp:
            catalog = Path(tmp) / "devices.jsonl"
            catalog.write_text(
                '{"type":"esp32-mesh","name":"node-3","chip":"esp32","usb_path":"",'
                '"wifi_mac":"20:9b:a9:99:32:0c","flash_ok":true,"push_ok":false}\n'
                '{"type":"esp32-mesh","name":"node-4","chip":"esp32","usb_path":"12",'
                '"wifi_mac":"20:9b:a9:97:9f:28","flash_ok":true,"push_ok":false}\n',
                encoding="utf-8",
            )
            journal = Path(tmp) / "applied.jsonl"
            journal.write_text(
                '{"usb_path":"12","name":"node-3","image_id":"abc","command":"flash"}\n'
                '{"usb_path":"12","name":"node-3","wifi_mac":"20:9b:a9:99:32:0c",'
                '"image_id":"abc","command":"flash"}\n'
            )
            snapshot = "/dev/ttyUSB1\t12\tCP2102\t0001\n"
            result = shell(
                f"printf '%s' '{snapshot}' | fleet_plan \"{catalog}\" \"{journal}\" abc ''"
            )
            self.assertEqual(result.returncode, 0, result.stderr[-400:])
            fields = result.stdout.splitlines()[1].split("|")
            self.assertEqual(fields[2], "node-4")
            self.assertEqual(fields[4], "flash-node")
            self.assertEqual(fields[6], "20:9b:a9:97:9f:28")

    def test_a_row_with_a_mac_is_that_chip_wherever_it_was_plugged_in(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            journal = Path(tmp) / "applied.jsonl"
            journal.write_text(
                '{"usb_path":"7","name":"node-4","wifi_mac":"20:9b:a9:97:9f:28",'
                '"image_id":"abc","command":"flash"}\n'
            )
            moved = shell(f'journal_has "{journal}" 11 abc node-4 20:9B:A9:97:9F:28')
            self.assertEqual(moved.returncode, 0, moved.stderr[-400:])
            other = shell(f'journal_has "{journal}" 7 abc node-4 20:9b:a9:99:1e:54')
            self.assertNotEqual(other.returncode, 0)
            newer = shell(f'journal_has "{journal}" 7 def node-4 20:9b:a9:97:9f:28')
            self.assertNotEqual(newer.returncode, 0)

    def test_an_added_row_carries_the_mac(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            journal = Path(tmp) / "applied.jsonl"
            result = shell(
                f'journal_add "{journal}" 12 node-4 abc "mesh.sh flash" 20:9B:A9:97:9F:28'
            )
            self.assertEqual(result.returncode, 0, result.stderr[-400:])
            row = json.loads(journal.read_text())
            self.assertEqual(row["wifi_mac"], "20:9b:a9:97:9f:28")
            self.assertEqual(row["name"], "node-4")


class Learn(unittest.TestCase):
    def test_a_moved_board_is_recorded_and_the_old_path_is_released(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            catalog = Path(tmp) / "devices.jsonl"
            boards = Path(tmp) / "boards.tsv"
            catalog.write_text(
                '{"type":"esp32-mesh","name":"node-0","chip":"esp32","usb_path":"12",'
                '"wifi_mac":"70:4b:ca:04:21:64","flash_ok":true,"push_ok":false}\n'
                '{"type":"esp32-mesh","name":"node-1","chip":"esp32","usb_path":"7",'
                '"wifi_mac":"20:9b:a9:99:1e:54","flash_ok":true,"push_ok":false}\n'
                '{"type":"esp32s3-cam","name":"toby-cam-0","chip":"esp32s3","usb_path":"11",'
                '"wifi_mac":"unknown","flash_ok":false,"push_ok":false}\n',
                encoding="utf-8",
            )
            boards.write_text(
                "# name\twifi_mac\tchip\tusb_path\tnotes\n"
                "node-0\t70:4b:ca:04:21:64\tesp32\t12\tnote\n"
                "node-1\t20:9b:a9:99:1e:54\tesp32\t7\tnote\n"
                "toby-cam-0\tunknown\tesp32s3\t11\tnote\n",
                encoding="utf-8",
            )
            seen = (
                "catalog_learn '{c}' '{b}' 7 70:4b:ca:04:21:64 esp32 0x20 CP2102 10c4:ea60\n"
                "catalog_learn '{c}' '{b}' 12 20:9b:a9:99:32:0c esp32 0x5e CP2102 10c4:ea60\n"
                "catalog_learn '{c}' '{b}' 11 7c:9e:bd:ed:bd:98 esp32 0x20 CP2102 10c4:ea60\n"
                "catalog_agrees '{c}' '{b}'\n"
            ).format(c=catalog, b=boards)
            result = shell(seen)
            self.assertEqual(result.returncode, 0, result.stderr[-500:])
            rows = {
                json.loads(line)["name"]: json.loads(line)
                for line in catalog.read_text().splitlines()
            }
            self.assertEqual(rows["node-0"]["usb_path"], "7")
            self.assertEqual(rows["node-0"]["wifi_mac"], "70:4b:ca:04:21:64")
            self.assertEqual(rows["node-0"]["bt_mac"], "70:4b:ca:04:21:66")
            self.assertEqual(rows["node-0"]["mesh_id"], "642104ca")
            self.assertEqual(rows["node-1"]["usb_path"], "")
            self.assertEqual(rows["node-2"]["wifi_mac"], "20:9b:a9:99:32:0c")
            self.assertEqual(rows["node-2"]["usb_path"], "12")
            self.assertEqual(rows["node-2"]["bt_mac"], "20:9b:a9:99:32:0e")
            self.assertEqual(rows["node-2"]["mesh_id"], "0c3299a9")
            self.assertEqual(rows["node-2"]["flash_mfr"], "0x5e")
            self.assertTrue(rows["node-2"]["flash_ok"])
            self.assertFalse(rows["node-2"]["push_ok"])
            self.assertEqual(rows["node-3"]["usb_path"], "11")
            self.assertEqual(rows["node-3"]["wifi_mac"], "7c:9e:bd:ed:bd:98")
            self.assertEqual(rows["toby-cam-0"]["usb_path"], "")
            self.assertEqual(rows["toby-cam-0"]["wifi_mac"], "unknown")
            plan = shell(
                "printf '%s' '/dev/ttyUSB0\t7\tCP2102\n/dev/ttyUSB1\t11\tCP2102\n/dev/ttyUSB2\t12\tCP2102\n' "
                f"| fleet_plan '{catalog}' /nonexistent/journal abc ''"
            )
            self.assertEqual(plan.returncode, 0, plan.stderr[-500:])
            self.assertTrue(plan.stdout.startswith("mesh_ready\t1"), plan.stdout)
            actions = {
                line.split("|")[1]: line.split("|")[4]
                for line in plan.stdout.splitlines()
                if "|" in line
            }
            self.assertEqual(actions, {"7": "flash-node", "11": "flash-node", "12": "flash-node"})

    def test_the_first_s3_fills_toby_cam_0_and_a_second_s3_is_toby_cam_1(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            catalog = Path(tmp) / "devices.jsonl"
            boards = Path(tmp) / "boards.tsv"
            catalog.write_text(
                '{"type":"esp32s3-cam","name":"toby-cam-0","chip":"esp32s3","usb_path":"",'
                '"wifi_mac":"unknown","flash_ok":false,"push_ok":false}\n',
                encoding="utf-8",
            )
            boards.write_text("# name\twifi_mac\tchip\tusb_path\tnotes\n", encoding="utf-8")
            result = shell(
                f"catalog_learn '{catalog}' '{boards}' 2 aa:bb:cc:dd:ee:01 esp32s3 0x68 USB_JTAG 303a:1001\n"
                f"catalog_learn '{catalog}' '{boards}' 3 aa:bb:cc:dd:ee:02 esp32s3 0x68 USB_JTAG 303a:1001"
            )
            self.assertEqual(result.returncode, 0, result.stderr[-500:])
            rows = {
                json.loads(line)["name"]: json.loads(line)
                for line in catalog.read_text().splitlines()
            }
            self.assertEqual(rows["toby-cam-0"]["usb_path"], "2")
            self.assertEqual(rows["toby-cam-0"]["wifi_mac"], "aa:bb:cc:dd:ee:01")
            self.assertTrue(rows["toby-cam-0"]["flash_ok"])
            self.assertFalse(rows["toby-cam-0"]["push_ok"])
            self.assertEqual(rows["toby-cam-1"]["usb_path"], "3")
            self.assertEqual(rows["toby-cam-1"]["wifi_mac"], "aa:bb:cc:dd:ee:02")
            self.assertTrue(rows["toby-cam-1"]["flash_ok"])
            self.assertFalse(rows["toby-cam-1"]["push_ok"])
            self.assertNotIn("cam-1", rows)

    def test_a_later_s3_does_not_replace_toby_cam_0(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            catalog = Path(tmp) / "devices.jsonl"
            boards = Path(tmp) / "boards.tsv"
            catalog.write_text(
                '{"type":"esp32s3-cam","name":"toby-cam-0","chip":"esp32s3","usb_path":"2",'
                '"wifi_mac":"44:bd:8d:7a:51:60","flash_ok":true,"push_ok":false}\n',
                encoding="utf-8",
            )
            boards.write_text(
                "# name\twifi_mac\tchip\tusb_path\tnotes\n"
                "toby-cam-0\t44:bd:8d:7a:51:60\tesp32s3\t2\tnote\n",
                encoding="utf-8",
            )
            result = shell(
                f"catalog_learn '{catalog}' '{boards}' 2 44:bd:8d:7c:20:c4 esp32s3 0x68 USB_JTAG 303a:1001"
            )
            self.assertEqual(result.returncode, 0, result.stderr[-500:])
            rows = {
                json.loads(line)["name"]: json.loads(line)
                for line in catalog.read_text().splitlines()
            }
            self.assertEqual(rows["toby-cam-0"]["wifi_mac"], "44:bd:8d:7a:51:60")
            self.assertEqual(rows["toby-cam-0"]["usb_path"], "")
            self.assertEqual(rows["toby-cam-1"]["wifi_mac"], "44:bd:8d:7c:20:c4")
            self.assertEqual(rows["toby-cam-1"]["usb_path"], "2")
            self.assertTrue(rows["toby-cam-1"]["flash_ok"])


class Identity(unittest.TestCase):
    def test_esp32_s3_is_not_read_as_a_plain_esp32(self) -> None:
        result = shell(
            "identity_from_esptool <<'EOF'\n"
            "Detecting chip type... ESP32-S3\n"
            "Chip is ESP32-S3 (revision v0.2)\n"
            "MAC: AA:BB:CC:DD:EE:FF\n"
            "Manufacturer: 68\n"
            "EOF"
        )
        self.assertEqual(result.returncode, 0, result.stderr[-400:])
        self.assertEqual(result.stdout.strip(), "esp32s3\taa:bb:cc:dd:ee:ff\t0x68")

    def test_a_plain_esp32_chip_line_is_enough(self) -> None:
        result = shell(
            "identity_from_esptool <<'EOF'\n"
            "Chip is ESP32-D0WD-V3 (revision v3.1)\n"
            "MAC: 70:4b:ca:04:21:64\n"
            "Manufacturer: 20\n"
            "EOF"
        )
        self.assertEqual(result.returncode, 0, result.stderr[-400:])
        self.assertEqual(result.stdout.strip(), "esp32\t70:4b:ca:04:21:64\t0x20")

    def test_text_without_a_mac_is_refused(self) -> None:
        result = shell("identity_from_esptool <<'EOF'\nDetecting chip type... ESP32\nEOF")
        self.assertNotEqual(result.returncode, 0)


class ReadRetry(unittest.TestCase):
    def test_a_mac_serial_is_retried_and_a_cp2102_is_not(self) -> None:
        result = shell(
            "serial_is_mac '44:BD:8D:7C:20:C4'\n"
            "learn_failed one '44:BD:8D:7C:20:C4'\n"
            "learn_failed one '44:BD:8D:7C:20:C4'\n"
            '[[ -z "${LEARNED[one]:-}" ]]\n'
            "learn_failed one '44:BD:8D:7C:20:C4'\n"
            '[[ -n "${LEARNED[one]:-}" ]]\n'
            "learn_failed two 0001\n"
            '[[ -n "${LEARNED[two]:-}" ]]\n'
            "serial_is_mac 0001 && exit 1 || true\n"
        )
        self.assertEqual(result.returncode, 0, result.stderr[-400:])


class ImageId(unittest.TestCase):
    def test_the_id_changes_when_the_elf_changes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            boot = Path(tmp) / "bootloader.bin"
            elf = Path(tmp) / "app"
            boot.write_bytes(b"boot")
            elf.write_bytes(b"app")
            first = shell(f'image_id "{boot}" "{elf}"')
            self.assertEqual(first.returncode, 0, first.stderr[-400:])
            again = shell(f'image_id "{boot}" "{elf}"')
            self.assertEqual(first.stdout.strip(), again.stdout.strip())
            elf.write_bytes(b"next")
            changed = shell(f'image_id "{boot}" "{elf}"')
            self.assertNotEqual(first.stdout.strip(), changed.stdout.strip())


if __name__ == "__main__":
    unittest.main()
