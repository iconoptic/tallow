"""The cam stream bytes, the port guard, and the build-log scan."""

from __future__ import annotations

import asyncio
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "app"))
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import cam_build_scan  # noqa: E402
import cam_push_send  # noqa: E402
import cam_stream_check  # noqa: E402
import cams as camlib  # noqa: E402

JPEG_A = b"\xff\xd8one\xff\xd9"
JPEG_B = b"\xff\xd8two\xff\xd9"
STUB = (
    "node-0\taa\tesp32\t12\tnote\n"
    "toby-cam-0\tunknown\tesp32s3\t11\tnote\n"
    "toby-cam-1\tbb\tesp32s3\t99\tnote\n"
)


def guard(name: str, *args: str) -> int:
    """Run one mesh.sh function against the arguments. Opens no port."""
    script = f"""
set +e
source tools/mesh.sh
set +e
{name} "$@"
echo $?
"""
    result = subprocess.run(
        ["bash", "-c", script, "guard", *args],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    line = result.stdout.strip().splitlines()[-1]
    return int(line)


class CamStream(unittest.TestCase):
    def test_the_panel_reads_this_firmware_s_parts(self) -> None:
        raw = subprocess.check_output(["tools/mesh.sh", "cam-frame"], cwd=ROOT)
        head, sep, body = raw.partition(b"\r\n\r\n")
        self.assertEqual(sep, b"\r\n\r\n")
        self.assertNotIn(b"Transfer-Encoding", head)
        lines = head.decode("latin-1").split("\r\n")
        status = lines[0].split(" ", 2)
        self.assertGreaterEqual(len(status), 2)
        self.assertEqual(status[1], "200")
        headers: dict[str, str] = {}
        for line in lines[1:]:
            name, found, value = line.partition(":")
            if found:
                headers[name.strip().lower()] = value.strip()
        content_type = headers.get("content-type", "")
        self.assertTrue(content_type.lower().startswith("multipart/x-mixed-replace"))

        async def read() -> list[bytes]:
            reader = asyncio.StreamReader()
            reader.feed_data(body)
            reader.feed_eof()
            upstream = camlib.Upstream(reader, None)
            return [frame async for frame in upstream.frames()]

        self.assertEqual(asyncio.run(read()), [JPEG_A, JPEG_B])

    def test_stream_check_allows_only_toby_cam_0_without_a_socket(self) -> None:
        called: list[object] = []

        async def dial(cam):
            called.append(cam)
            raise AssertionError("dialled")

        for address in (
            "10.87.0.21",
            "10.87.21",
            "0xa.87.0.21",
            "173473813",
            "010.87.0.21",
            "10.87.0.021",
            "toby-cam-1.local",
            "10.87.0.23 ",
            "",
        ):
            with self.subTest(address=address):
                self.assertEqual(cam_stream_check.main(["--address", address], dial=dial), 1)
        self.assertEqual(called, [])
        self.assertFalse(cam_stream_check.refuse("10.87.0.23"))

    def test_cam_ports_follow_the_stub_table(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            boards = Path(tmp) / "boards.tsv"
            boards.write_text(STUB)
            self.assertEqual(guard("cam_port_ok", str(boards), "11"), 0)
            self.assertNotEqual(guard("cam_port_ok", str(boards), "12"), 0)
            self.assertNotEqual(guard("cam_port_ok", str(boards), "99"), 0)
            self.assertNotEqual(guard("cam_port_ok", str(boards), "3"), 0)
        self.assertEqual(guard("cam_takes_port", "/dev/ttyACM0"), 0)
        self.assertNotEqual(guard("cam_takes_port", "10.87.0.21"), 0)
        self.assertNotEqual(guard("cam_takes_port", "10.87.0.23"), 0)

    def test_cam_push_allows_only_toby_cam_0(self) -> None:
        self.assertEqual(guard("cam_allows_address", "10.87.0.23"), 0)
        self.assertNotEqual(guard("cam_allows_address", "10.87.0.21"), 0)
        self.assertNotEqual(guard("cam_allows_address", "10.87.0.1"), 0)
        self.assertNotEqual(guard("cam_allows_address"), 0)

    def test_the_cam_build_does_not_mount_the_mesh_key(self) -> None:
        script = """
set -euo pipefail
source tools/mesh.sh
run_cam_image /cam cat /proc/self/mountinfo
"""
        result = subprocess.run(
            ["bash", "-c", script],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr[-400:])
        text = result.stdout
        self.assertNotIn("mesh.key", text)
        saw_core = False
        saw_pub = False
        for line in text.splitlines():
            fields = line.split()
            if len(fields) < 5:
                continue
            source, dest = fields[3], fields[4]
            # Field 4 is the path inside its filesystem, which can start with a
            # subvolume, so the tail is the part that means anything.
            self.assertFalse(source.endswith("/firmware/mesh"), source)
            self.assertNotEqual(dest, "/mesh")
            saw_core = saw_core or dest == "/mesh/core"
            saw_pub = saw_pub or dest == "/run/ota.pub"
        self.assertTrue(saw_core)
        self.assertTrue(saw_pub)
        self.assertNotIn("mesh.key", result.stderr)

    def test_no_key_file_is_visible_inside_the_cam_build(self) -> None:
        # Names only. This does not read any file it finds. `ota.pub` is the
        # control: if the search stopped working, it would not appear either.
        script = """
set -euo pipefail
source tools/mesh.sh
run_cam_image /cam bash -c 'find / \\( -path /proc -o -path /sys \\) -prune -o \\( -name mesh.key -o -name ota.key -o -name ota.pub \\) -print 2>/dev/null || true'
"""
        result = subprocess.run(
            ["bash", "-c", script],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr[-400:])
        found = result.stdout.split()
        self.assertIn("/run/ota.pub", found)
        self.assertEqual([path for path in found if path.endswith(".key")], [])

    def test_cam_push_refuses_a_bad_target_before_any_socket(self) -> None:
        image = b"image"

        def connect(*_args, **_kwargs):
            raise AssertionError("dialled")

        good = bytes([cam_push_send.PRODUCT_CAM0]) + (7).to_bytes(4, "little") + bytes(64)
        node = bytes([1]) + (7).to_bytes(4, "little") + bytes(64)
        ok, text = cam_push_send.send("10.87.0.21", image, good, connect=connect)
        self.assertFalse(ok)
        self.assertIn("10.87.0.21", text)
        ok, text = cam_push_send.send("10.87.0.23", image, node, connect=connect)
        self.assertFalse(ok)
        self.assertIn("product 1", text)
        with self.assertRaises(ValueError):
            cam_push_send.send("10.87.0.23", image, good[:-1], connect=connect)
        for size in (0, cam_push_send.SLOT + 1):
            ok, text = cam_push_send.send("10.87.0.23", bytes(size), good, connect=connect)
            self.assertFalse(ok, size)
            self.assertIn("slot", text)

    def test_cam_push_header_matches_the_firmware_layout(self) -> None:
        signature = bytes(range(64))
        header = cam_push_send.build_header(b"abc", 2, 0x01020304, signature)
        self.assertEqual(len(header), 105)
        self.assertEqual(header[:4], (3).to_bytes(4, "big"))
        self.assertEqual(header[4], 2)
        self.assertEqual(header[5:9], (0x01020304).to_bytes(4, "little"))
        self.assertEqual(header[41:], signature)

    def test_cam_push_only_calls_a_zero_result_ok(self) -> None:
        self.assertTrue(cam_push_send.meaning(b"\x00")[0])
        for answer in (b"", b"\x01", b"\x02", b"\x05", b"\x06", b"\xff"):
            self.assertFalse(cam_push_send.meaning(answer)[0], answer)

    def test_the_build_scan_hides_a_leaked_credential(self) -> None:
        secret = "super-secret-value"
        text = f'wifi_ssid: "tallow-iot"\nwifi_password: "{secret}"\n'
        self.assertTrue(cam_build_scan.log_leaks(f"note {secret} end", text))
        self.assertFalse(cam_build_scan.log_leaks("Finished release", text))
        self.assertTrue(cam_build_scan.default_hit("from secrets/iot-ap/passphrase"))
        self.assertTrue(cam_build_scan.default_hit("12345678"))
        self.assertFalse(cam_build_scan.default_hit("123456789abcdef"))
        self.assertFalse(cam_build_scan.default_hit("Finished release"))
        with tempfile.TemporaryDirectory() as tmp:
            log = Path(tmp) / "log"
            secrets = Path(tmp) / "secrets.yaml"
            log.write_text(f"cargo: {secret}\n")
            secrets.write_text(text)
            result = subprocess.run(
                [sys.executable, str(ROOT / "tools/cam_build_scan.py"), "log", str(log), str(secrets)],
                check=False,
                capture_output=True,
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(cam_build_scan.LEAKED.encode(), result.stderr)
        self.assertNotIn(secret.encode(), result.stdout)
        self.assertNotIn(secret.encode(), result.stderr)


if __name__ == "__main__":
    unittest.main()
