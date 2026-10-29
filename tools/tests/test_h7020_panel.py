"""Host binding, password file, and write coalescing for the H7020 page."""

from __future__ import annotations

import asyncio
import stat
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "app"))

import h7020_panel as panel  # noqa: E402


class BindHostTest(unittest.TestCase):
    def test_refuses_unspecified_and_iot(self) -> None:
        self.assertIn("0.0.0.0", panel.reject_bind_host("0.0.0.0") or "")
        self.assertIn("10.87.0.0/24", panel.reject_bind_host("10.87.0.1") or "")
        self.assertIsNone(panel.reject_bind_host("192.168.1.20"))

    def test_default_route_outside_iot_wins(self) -> None:
        host = panel.choose_host(None, "192.168.1.20", ["10.87.0.1", "192.168.1.20", "10.0.0.8"])
        self.assertEqual(host, "192.168.1.20")

    def test_iot_route_falls_through_to_the_only_lan_address(self) -> None:
        host = panel.choose_host(None, "10.87.0.1", ["10.87.0.1", "127.0.0.1", "192.168.86.4"])
        self.assertEqual(host, "192.168.86.4")

    def test_several_lan_addresses_ask_for_host(self) -> None:
        with self.assertRaises(SystemExit) as raised:
            panel.choose_host(None, None, ["192.168.1.5", "192.168.1.6"])
        self.assertIn("192.168.1.5", str(raised.exception))
        self.assertIn("--host", str(raised.exception))

    def test_explicit_iot_address_is_refused(self) -> None:
        with self.assertRaises(SystemExit):
            panel.choose_host("0.0.0.0", "192.168.1.20", [])


class PasswordFileTest(unittest.TestCase):
    def test_requires_mode_0600_and_one_line(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "password"
            path.write_text("secret\n", encoding="utf-8")
            path.chmod(0o644)
            with self.assertRaises(SystemExit):
                panel.load_password(path)
            path.chmod(stat.S_IRUSR | stat.S_IWUSR)
            self.assertEqual(panel.load_password(path), "secret")
            self.assertEqual(len(panel.session_token("secret")), 64)

    def test_missing_file_explains_how_to_create_it(self) -> None:
        with self.assertRaises(SystemExit) as raised:
            panel.load_password(Path("/tmp/tallow-panel-missing-password"))
        self.assertIn("chmod 0600", str(raised.exception))


class CoalesceTest(unittest.TestCase):
    def test_slider_keeps_the_latest_color_while_a_write_is_in_flight(self) -> None:
        applied: list[dict] = []
        started = asyncio.Event()
        release = asyncio.Event()

        async def scenario() -> None:
            session = panel.LightSession()
            session.state["connected"] = True
            session.client = type("Client", (), {"is_connected": True})()

            async def fake_apply(batch: dict) -> None:
                applied.append(dict(batch))
                if len(applied) == 1:
                    started.set()
                    await release.wait()

            session._apply = fake_apply  # type: ignore[method-assign]
            session._writer = asyncio.create_task(session._writer_loop())
            await session.submit(color={"r": 1, "g": 0, "b": 0})
            await started.wait()
            await session.submit(color={"r": 2, "g": 0, "b": 0})
            await session.submit(color={"r": 9, "g": 0, "b": 0})
            release.set()
            await asyncio.sleep(0.05)
            session._writer.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await session._writer

        asyncio.run(scenario())
        self.assertEqual(applied[0]["color"]["r"], 1)
        self.assertEqual(applied[-1]["color"]["r"], 9)
        self.assertNotIn(2, [batch["color"]["r"] for batch in applied])

    def test_scene_script_stays_ordered_then_the_slider_color_follows(self) -> None:
        applied: list[tuple] = []
        started = asyncio.Event()
        release = asyncio.Event()

        async def scenario() -> None:
            session = panel.LightSession()
            session.state["connected"] = True
            session.client = type("Client", (), {"is_connected": True})()

            async def fake_script(packets: list[bytes]) -> None:
                applied.append(("script", list(packets)))
                if len(applied) == 1:
                    started.set()
                    await release.wait()

            async def fake_apply(batch: dict) -> None:
                applied.append(("batch", batch["color"]["r"]))

            session._apply_script = fake_script  # type: ignore[method-assign]
            session._apply = fake_apply  # type: ignore[method-assign]
            session._writer = asyncio.create_task(session._writer_loop())
            await session.submit_script([b"a", b"b", b"c"], mode="scene")
            await started.wait()
            await session.submit(color={"r": 9, "g": 0, "b": 0})
            release.set()
            await asyncio.sleep(0.05)
            session._writer.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await session._writer

        asyncio.run(scenario())
        self.assertEqual(applied[0], ("script", [b"a", b"b", b"c"]))
        self.assertEqual(applied[1], ("batch", 9))

    def test_preview_close_skips_a_missing_client(self) -> None:
        async def scenario() -> None:
            session = panel.LightSession()
            session.preview = True
            await session.close()

        asyncio.run(scenario())

    def test_snapshot_follows_the_live_link(self) -> None:
        session = panel.LightSession()
        session.state["connected"] = False
        session.client = type("Client", (), {"is_connected": True})()
        self.assertTrue(session.snapshot()["connected"])
        session.client.is_connected = False
        self.assertFalse(session.snapshot()["connected"])

    def test_page_states_the_wifi_limit(self) -> None:
        self.assertIn("phones home", panel.CONTROLS_HTML)
        self.assertIn("tallow-iot", panel.CONTROLS_HTML)

    def test_status_queries_are_codec_packets(self) -> None:
        profile = panel.light.sized_profile(panel.light.profile_for(1, 1), 1)
        power = panel._status_query("power", profile)
        brightness = panel._status_query("brightness", profile)
        self.assertEqual(power[0], 0xAA)
        self.assertEqual(brightness[0], 0xAA)
        self.assertNotEqual(power, brightness)


if __name__ == "__main__":
    unittest.main()
