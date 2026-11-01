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

    def test_the_cam_guard_compares_a_path_as_text_too(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            boards = Path(tmp) / "boards.tsv"
            boards.write_text(TABLE)
            self.assertEqual(shell(f'cam_port_ok "{boards}" 2.1').returncode, 0)
            self.assertNotEqual(shell(f'cam_port_ok "{boards}" 2.10').returncode, 0)

    def test_a_port_with_no_usb_path_matches_no_row(self) -> None:
        # A device whose USB path is unknown must not take a row that has no path.
        with tempfile.TemporaryDirectory() as tmp:
            boards = Path(tmp) / "boards.tsv"
            boards.write_text(TABLE + "toby-cam-0\tcc\tesp32s3\t\tno path yet\n")
            result = shell(f'BOARDS="{boards}"\nboard_field "" 3\necho end')
            self.assertEqual(result.stdout.split(), ["end"])
            self.assertNotEqual(shell(f'cam_port_ok "{boards}" ""').returncode, 0)


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
            self.assertIn("Could not read a MAC", result.stderr)
            self.assertEqual(list((root / "data" / "firmware-backups").glob("*")), [])


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


if __name__ == "__main__":
    unittest.main()
