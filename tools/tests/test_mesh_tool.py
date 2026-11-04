"""Guards in tools/mesh.sh. Opens no port. Reads no key: the container test asks
for a file's type, never its contents."""

from __future__ import annotations

import os
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OTA_KEY = ROOT / "firmware" / "mesh" / "ota.key"
MISSING = "/nonexistent/secrets.yaml"
# Rows whose paths read as the same number to awk.
TABLE = (
    "# name\tmac\tchip\tusb\tnote\n"
    "node-0\taa\tesp32\t1.1\tnote\n"
    "toby-cam-0\tbb\tesp32s3\t2.1\tnote\n"
)


def shell(body: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    script = f"set -euo pipefail\nsource tools/mesh.sh\n{body}\n"
    return subprocess.run(
        ["bash", "-c", script], cwd=ROOT, check=False, capture_output=True, text=True, env=env
    )


class SessionGroup(unittest.TestCase):
    def test_a_member_missing_from_this_login_is_the_group_to_join(self) -> None:
        result = shell(
            "group_missing_from_session dialout btgrant 'btgrant wheel' 'other,btgrant'"
        )
        self.assertEqual(result.returncode, 0, result.stderr[-400:])
        self.assertEqual(result.stdout.strip(), "dialout")

    def test_a_group_already_in_this_login_is_not_joined_again(self) -> None:
        result = shell(
            "group_missing_from_session dialout btgrant 'btgrant dialout wheel' btgrant"
        )
        self.assertEqual(result.returncode, 0, result.stderr[-400:])
        self.assertEqual(result.stdout.strip(), "")

    def test_someone_who_is_not_a_member_is_not_named(self) -> None:
        result = shell("group_missing_from_session dialout btgrant 'btgrant wheel' other")
        self.assertEqual(result.returncode, 0, result.stderr[-400:])
        self.assertEqual(result.stdout.strip(), "")

    def test_an_unwritable_port_is_refused_before_a_tool_starts(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            port = Path(tmp) / "port"
            port.write_text("")
            port.chmod(0)
            result = shell(f'take_port_group "{port}"')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("is not writable by this login", result.stderr)

    def test_a_missing_group_reexecs_once_under_sg(self) -> None:
        # A stub sg stands in. The port is /dev/null, which is never opened.
        with tempfile.TemporaryDirectory() as tmp:
            stub = Path(tmp) / "sg"
            stub.write_text("#!/bin/sh\nprintf '%s\\n' \"$1\" \"$3\" \"${MESH_PORT_GROUP:-}\"\nexit 42\n")
            stub.chmod(0o755)
            env = {**os.environ, "PATH": f"{tmp}{os.pathsep}{os.environ['PATH']}"}
            result = shell(
                'port_session_group() { printf "%s\\n" dialout; }\n'
                "SCRIPT_ARGS=(flash /dev/ttyUSB0)\n"
                "take_port_group /dev/null\n",
                env=env,
            )
            self.assertEqual(result.returncode, 42, result.stderr[-400:])
            lines = result.stdout.splitlines()
            self.assertEqual(lines[0], "dialout")
            self.assertIn("flash", lines[1])
            self.assertIn("/dev/ttyUSB0", lines[1])
            self.assertEqual(lines[2], "1")
            self.assertIn("Joining group dialout", result.stderr)


FLASH_ID = (
    "esptool v5.3.1\n"
    "Detecting chip type... {chip}\n"
    "MAC: {mac}\n"
    "Manufacturer: 5e\n"
)


def fake_tree(base: Path, chip: str, mac: str) -> tuple[Path, Path, dict[str, str]]:
    """A stand-in podman that prints esptool flash-id text, a udevadm that
    reports a CP2102 on path 12, and empty catalog files. No container runs."""
    fake = base / "bin"
    fake.mkdir()
    (fake / "podman").write_text(
        "#!/usr/bin/env bash\ncat <<'EOF'\n" + FLASH_ID.format(chip=chip, mac=mac) + "EOF\n"
    )
    (fake / "udevadm").write_text(
        "#!/usr/bin/env bash\n"
        "printf 'ID_PATH=pci-0000:00:14.0-usb-0:12:1.0\\nID_VENDOR_ID=10c4\\n"
        "ID_MODEL_ID=ea60\\nID_MODEL=CP2102\\nID_SERIAL_SHORT=0001\\n'\n"
    )
    for tool in ("podman", "udevadm"):
        (fake / tool).chmod(0o755)
    catalog = base / "devices.jsonl"
    boards = base / "boards.tsv"
    catalog.write_text(
        '{"type":"esp32-mesh","name":"node-0","chip":"esp32","usb_path":"12",'
        '"wifi_mac":"70:4b:ca:04:21:64","flash_ok":true,"push_ok":false}\n'
    )
    boards.write_text("# name\twifi_mac\tchip\tusb_path\tnotes\nnode-0\t70:4b:ca:04:21:64\tesp32\t12\tnote\n")
    env = {**os.environ, "PATH": f"{fake}{os.pathsep}{os.environ['PATH']}"}
    return catalog, boards, env


class BoardMac(unittest.TestCase):
    def test_a_new_chip_on_a_known_socket_is_learned_not_refused(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            catalog, boards, env = fake_tree(Path(tmp), "ESP32", "20:9B:A9:97:9F:28")
            result = shell(
                f'CATALOG="{catalog}"\nBOARDS="{boards}"\nconfirm_port_mac /dev/null esp32', env=env
            )
            self.assertEqual(result.returncode, 0, result.stderr[-400:])
            self.assertEqual(result.stdout.strip(), "20:9b:a9:97:9f:28")
            self.assertIn("node-1", result.stderr)
            self.assertNotIn(", not ", result.stderr)
            self.assertIn("node-1\t20:9b:a9:97:9f:28\tesp32\t12", boards.read_text())
            self.assertIn('"usb_path":""', catalog.read_text().splitlines()[0])

    def test_a_cam_on_a_node_command_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            catalog, boards, env = fake_tree(Path(tmp), "ESP32-S3", "44:bd:8d:7a:50:cc")
            result = shell(
                f'CATALOG="{catalog}"\nBOARDS="{boards}"\nconfirm_port_mac /dev/null esp32', env=env
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("cam-flash", result.stderr)
            self.assertNotIn("44:bd:8d:7a:50:cc", boards.read_text())

    def test_each_new_cam_is_its_own_toby_cam(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            catalog, boards, env = fake_tree(Path(tmp), "ESP32-S3", "44:bd:8d:7a:50:cc")
            result = shell(
                f'CATALOG="{catalog}"\nBOARDS="{boards}"\nconfirm_port_mac /dev/null esp32s3', env=env
            )
            self.assertEqual(result.returncode, 0, result.stderr[-400:])
            self.assertIn("toby-cam-0", result.stderr)

    def test_the_bluetooth_address_is_the_base_plus_two(self) -> None:
        result = shell("bt_mac 70:4b:ca:04:21:64")
        self.assertEqual(result.returncode, 0, result.stderr[-400:])
        self.assertEqual(result.stdout.strip(), "70:4b:ca:04:21:66")
        result = shell("bt_mac aa:bb:cc:dd:ee:ff")
        self.assertEqual(result.returncode, 0, result.stderr[-400:])
        self.assertEqual(result.stdout.strip(), "aa:bb:cc:dd:ef:01")


class PortGuards(unittest.TestCase):
    def test_a_usb_path_is_compared_as_text_not_as_a_number(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            boards = Path(tmp) / "boards.tsv"
            boards.write_text(TABLE)
            result = shell(f'BOARDS="{boards}"\nboard_field 1.10 3\necho end')
            self.assertEqual(result.returncode, 0, result.stderr[-400:])
            self.assertEqual(result.stdout.split(), ["end"])
            result = shell(f'BOARDS="{boards}"\nboard_field 1.1 3')
            self.assertEqual(result.stdout.split(), ["esp32"])

    def test_the_usb_ids_name_the_chip_class(self) -> None:
        result = shell("port_class 10c4:ea60; port_class 303A:1001; port_class 1a86:7523")
        self.assertEqual(result.stdout.split(), ["classic", "s3", "unknown"])

    def test_a_cp2102_is_never_a_cam_port(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            _catalog, _boards, env = fake_tree(Path(tmp), "ESP32", "aa:bb:cc:dd:ee:ff")
            empty = Path(tmp) / "empty.tsv"
            empty.write_text("# name\twifi_mac\tchip\tusb_path\tnotes\n")
            cam = shell(f'BOARDS="{empty}"\nneed_cam_port /dev/null', env=env)
            self.assertNotEqual(cam.returncode, 0)
            self.assertIn("CP2102", cam.stderr)

    def test_push_goes_only_to_a_reserved_cam_address(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            catalog = Path(tmp) / "devices.jsonl"
            reservations = Path(tmp) / "reservations"
            catalog.write_text(
                '{"type":"esp32s3-cam","name":"toby-cam-1","wifi_mac":"44:bd:8d:7a:50:cc"}\n'
                '{"type":"esp32-mesh","name":"node-0","wifi_mac":"70:4b:ca:04:21:64"}\n'
            )
            reservations.write_text("44:BD:8D:7A:50:CC,10.87.0.23\n70:4b:ca:04:21:64,10.87.0.22\n")
            base = f'CATALOG="{catalog}"\nRESERVATIONS="{reservations}"\n'
            self.assertEqual(shell(base + "cam_allows_address 10.87.0.23").returncode, 0)
            self.assertNotEqual(shell(base + "cam_allows_address 10.87.0.22").returncode, 0)
            self.assertNotEqual(shell(base + "cam_allows_address 10.87.0.21").returncode, 0)
            self.assertNotEqual(shell(base + "cam_allows_address ''").returncode, 0)

    def test_a_different_usb_mac_is_not_the_path_s_board(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            boards = Path(tmp) / "boards.tsv"
            boards.write_text(
                "toby-cam-0\t44:bd:8d:7a:51:60\tesp32s3\t2\tnote\n"
                "node-0\t70:4b:ca:04:21:64\tesp32\t7\tnote\n"
            )
            foreign = shell(f'BOARDS="{boards}"\nboard_name 2 "44:BD:8D:7C:20:C4"')
            self.assertEqual(foreign.returncode, 0, foreign.stderr[-400:])
            self.assertEqual(foreign.stdout.splitlines(), ["\t44:bd:8d:7c:20:c4\t"])
            same = shell(f'BOARDS="{boards}"\nboard_name 9 "44:bd:8d:7a:51:60"')
            self.assertEqual(same.stdout.splitlines(), ["toby-cam-0\t44:bd:8d:7a:51:60\tverified"])
            shared = shell(f'BOARDS="{boards}"\nboard_name 7 0001')
            self.assertEqual(shared.stdout.splitlines(), ["node-0\t\tunverified"])

    def test_a_port_with_no_usb_path_matches_no_row(self) -> None:
        # A device whose USB path is unknown must not take a row that has no path.
        with tempfile.TemporaryDirectory() as tmp:
            boards = Path(tmp) / "boards.tsv"
            boards.write_text(TABLE + "toby-cam-0\tcc\tesp32s3\t\tno path yet\n")
            result = shell(f'BOARDS="{boards}"\nboard_field "" 3\necho end')
            self.assertEqual(result.stdout.split(), ["end"])


class PanelPause(unittest.TestCase):
    def test_a_port_command_asks_the_panel_to_pause_and_gives_the_port_back_on_exit(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            pause = Path(tmp) / "data" / "mesh" / "pause"
            result = shell(f'ROOT="{tmp}"\nPAUSE_FILE="{pause}"\nhold_port /nonexistent/port\ntest -e "$PAUSE_FILE" && echo held')
            self.assertEqual(result.returncode, 0, result.stderr[-400:])
            self.assertEqual(result.stdout.split(), ["held"])
            self.assertFalse(pause.exists(), "the exit trap removes the pause file")

    def test_the_pause_file_is_removed_when_the_command_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            pause = Path(tmp) / "data" / "mesh" / "pause"
            result = shell(f'ROOT="{tmp}"\nPAUSE_FILE="{pause}"\nhold_port /nonexistent/port\ndie "stop here"')
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(pause.exists())


class Containers(unittest.TestCase):
    @unittest.skipUnless(OTA_KEY.exists(), "no signing key on this machine")
    def test_the_signing_key_is_masked_in_every_build_container(self) -> None:
        result = shell("run_image /mesh stat -c %F /mesh/ota.key /repo/firmware/mesh/ota.key")
        self.assertEqual(result.returncode, 0, result.stderr[-400:])
        self.assertEqual(result.stdout.splitlines(), ["character special file"] * 2)

    def test_a_masked_key_is_not_created_when_there_is_none(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            mesh = Path(tmp)
            (mesh / ".cache").mkdir()
            result = shell(f'MESH="{mesh}"\nOTA_KEY="{mesh}/ota.key"\nKEY="{mesh}/mesh.key"\nOTA_PUB="{mesh}/ota.pub"\nrun_image /mesh true')
            self.assertEqual(result.returncode, 0, result.stderr[-400:])
            self.assertFalse((mesh / "ota.key").exists())

    def test_cam_formatting_needs_no_wifi_secrets(self) -> None:
        result = shell(f'CAM_SECRETS="{MISSING}"\nin_cam_plain true')
        self.assertEqual(result.returncode, 0, result.stderr[-400:])

    def test_a_cam_formatting_container_cannot_see_the_secrets(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            secrets = Path(tmp) / "secrets.yaml"
            secrets.write_text("wifi_ssid: stub\n")
            result = shell(f'CAM_SECRETS="{secrets}"\nin_cam_plain "test ! -e /run/secrets.yaml"')
            self.assertEqual(result.returncode, 0, result.stderr[-400:])

    def test_a_cam_build_still_stops_without_the_secrets_file(self) -> None:
        result = shell(f'CAM_SECRETS="{MISSING}"\nrun_cam_image /cam true')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("firmware/cam/secrets.yaml", result.stderr)
        self.assertIn("wifi_password", result.stderr)


class Backup(unittest.TestCase):
    def test_a_failed_mac_read_says_so_and_keeps_nothing(self) -> None:
        # ROOT is the stand-in tree, and a fake podman stands in for esptool: no container runs.
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            fake = base / "bin"
            fake.mkdir()
            (fake / "podman").write_text("#!/usr/bin/env bash\necho 'Serial port busy' >&2\nexit 1\n")
            (fake / "podman").chmod(0o755)
            port = base / "ttyfake"
            port.touch()
            root = base / "tree"
            env = {**os.environ, "PATH": f"{fake}{os.pathsep}{os.environ['PATH']}"}
            result = shell(f'ROOT="{root}"\nrun_backup "{port}"', env=env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Serial port busy", result.stderr)
            self.assertIn("Could not read a MAC", result.stderr)
            self.assertNotIn(", not ", result.stderr)
            self.assertEqual(list((root / "data" / "firmware-backups").glob("*")), [])
            compared = shell(f'ROOT="{root}"\nconfirm_port_mac "{port}"', env=env)
            self.assertNotEqual(compared.returncode, 0)
            self.assertIn("Serial port busy", compared.stderr)
            self.assertIn("Could not read a MAC", compared.stderr)
            self.assertNotIn(", not ", compared.stderr)


class CamLog(unittest.TestCase):
    def test_an_interrupted_cam_build_removes_its_log(self) -> None:
        # in_cam is replaced by a sleep, so the log is open when the script is terminated.
        with tempfile.TemporaryDirectory() as tmp:
            script = "set -euo pipefail\nsource tools/mesh.sh\nin_cam() { sleep 30; }\ncam_logged build --release\n"
            env = {**os.environ, "TMPDIR": tmp}
            proc = subprocess.Popen(
                ["bash", "-c", script], cwd=ROOT, env=env,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
            try:
                deadline = time.monotonic() + 10
                while not any(Path(tmp).iterdir()) and time.monotonic() < deadline:
                    time.sleep(0.05)
                self.assertTrue(any(Path(tmp).iterdir()), "cam_logged made no log")
            finally:
                proc.terminate()
                proc.wait(timeout=10)
            self.assertEqual(list(Path(tmp).iterdir()), [])


class ReservationAndTraps(unittest.TestCase):
    def test_a_refused_reservation_stops_cam_flash_before_writing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            reservations = Path(tmp) / "reservations"
            reservations.write_text("")
            result = shell(
                f"RESERVATIONS='{reservations}'\n"
                "sudo() { return 1; }\n"
                'address="$(reserve_cam aa:bb:cc:dd:ee:01)" || echo STOPPED\n'
                'printf %s "$address"\n'
            )
            self.assertIn("STOPPED", result.stdout)
            self.assertNotIn("10.87.0.", result.stdout)

    def test_a_new_reservation_that_sudo_accepts_is_printed(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            reservations = Path(tmp) / "reservations"
            reservations.write_text("")
            result = shell(
                f"RESERVATIONS='{reservations}'\n"
                "sudo() { return 0; }\n"
                'address="$(reserve_cam aa:bb:cc:dd:ee:01)"\n'
                'printf %s "$address"\n'
            )
            self.assertEqual(result.returncode, 0, result.stderr[-400:])
            self.assertIn("10.87.0.21", result.stdout)

    def test_a_reservation_already_on_file_does_not_call_sudo(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            reservations = Path(tmp) / "reservations"
            reservations.write_text("aa:bb:cc:dd:ee:01,10.87.0.23\n")
            result = shell(
                f"RESERVATIONS='{reservations}'\n"
                "sudo() { echo SUDO; return 1; }\n"
                'address="$(reserve_cam aa:bb:cc:dd:ee:01)"\n'
                'printf %s "$address"\n'
            )
            self.assertEqual(result.returncode, 0, result.stderr[-400:])
            self.assertIn("10.87.0.23", result.stdout)
            self.assertNotIn("SUDO", result.stdout + result.stderr)

    def test_holding_a_port_keeps_an_earlier_exit_trap(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            pause = Path(tmp) / "pause"
            result = shell(
                "trap 'echo FIRST' EXIT\n"
                "fuser() { return 1; }\n"
                f"PAUSE_FILE='{pause}'\n"
                "hold_port /dev/null\n"
                "exit\n"
            )
            self.assertIn("FIRST", result.stdout)
            self.assertFalse(pause.exists(), result.stderr[-400:])


if __name__ == "__main__":
    unittest.main()
