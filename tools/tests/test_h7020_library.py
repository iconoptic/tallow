"""Patterns, schedules, tokens, and which door accepts which credential."""

from __future__ import annotations

import asyncio
import json
import stat
import sys
import tempfile
import time
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "app"))
sys.path.insert(0, str(ROOT / "tools"))

import agents  # noqa: E402
import govee_h7020 as light  # noqa: E402
import h7020_panel as panel  # noqa: E402
import library  # noqa: E402
import show as playback  # noqa: E402

ZONE = timezone(timedelta(hours=-6))


def scene_info(name: str) -> dict:
    return light.scene_info(name)


class PatternScheduleTest(unittest.TestCase):
    def test_scene_pattern_stores_the_catalogue_speed(self) -> None:
        saved = library.validate_pattern(
            {"id": "sunset", "name": "Sunset", "kind": "scene", "scene": {"name": "Sunset Glow"}},
            15,
            scene_info,
        )
        self.assertEqual(saved["scene"]["name"], "Sunset Glow")
        self.assertIn("speed", saved["scene"])
        with self.assertRaises(ValueError):
            library.validate_pattern(
                {"id": "sunset", "name": "Sunset", "kind": "scene", "scene": {"name": "Sunset Glow", "speed": 9}},
                15,
                scene_info,
            )

    def test_sequence_rejects_a_zero_hold_and_overlapping_bulbs(self) -> None:
        with self.assertRaises(ValueError):
            library.validate_pattern(
                {
                    "id": "walk",
                    "name": "Walk",
                    "kind": "sequence",
                    "steps": [{"hold": 0, "scene": {"name": "Night"}}],
                },
                15,
                scene_info,
            )
        with self.assertRaises(ValueError):
            library.validate_paint(
                [
                    {"bulbs": [1, 2], "rgb": [1, 0, 0]},
                    {"bulbs": [2, 3], "rgb": [0, 0, 1]},
                ],
                15,
            )

    def test_due_schedule_is_the_latest_slot_inside_twelve_hours(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            catalog = library.Library(Path(tmp))
            now = datetime(2026, 10, 28, 20, 0, tzinfo=ZONE)
            for schedule_id, clock in (("morning", "06:00"), ("afternoon", "18:00"), ("evening", "19:30")):
                catalog.save_schedule(
                    library.validate_schedule(
                        {
                            "id": schedule_id,
                            "days": ["wed"],
                            "time": clock,
                            "action": {"power": False},
                        },
                        {},
                        scene_info,
                    ),
                    create=True,
                )
            self.assertEqual(catalog.due_schedule(now)["id"], "evening")
            self.assertEqual(catalog.due_schedule(now.replace(hour=19, minute=29))["id"], "afternoon")
            self.assertEqual(catalog.due_schedule(now.replace(hour=17, minute=59))["id"], "morning")
            self.assertIsNone(catalog.due_schedule(now + timedelta(days=1)))
            later = library.Library(Path(tmp) / "later")
            later.save_schedule(
                library.validate_schedule(
                    {"id": "morning", "days": ["wed"], "time": "06:00", "action": {"power": False}},
                    {},
                    scene_info,
                ),
                create=True,
            )
            self.assertIsNone(later.due_schedule(now))
            catalog.save_schedule(
                library.validate_schedule(
                    {
                        "id": "noon",
                        "days": ["wed"],
                        "time": "08:00",
                        "enabled": False,
                        "action": {"power": False},
                    },
                    {},
                    scene_info,
                ),
                create=True,
            )
            self.assertEqual(catalog.due_schedule(datetime(2026, 10, 28, 20, 0, tzinfo=ZONE))["id"], "evening")


class PlaybackTest(unittest.TestCase):
    def test_sequence_holds_and_a_color_cancels_it(self) -> None:
        slept: list[int] = []

        async def scenario() -> None:
            session = _FakeSession()
            stage = playback.Show(session, library.Library(Path(tempfile.mkdtemp())), sleeper=_record(slept))
            pattern = library.validate_pattern(
                {
                    "id": "walk",
                    "name": "Walk",
                    "kind": "sequence",
                    "repeat": False,
                    "steps": [
                        {"hold": 1, "scene": {"name": "Sunset Glow"}},
                        {"hold": 2, "scene": {"name": "Night"}},
                    ],
                },
                15,
                scene_info,
            )
            await stage.apply_pattern(pattern)
            assert stage._task is not None
            await stage._task
            self.assertEqual(slept, [1, 2])
            self.assertEqual(len(session.scripts), 2)
            self.assertFalse(session.state["playing"])
            self.assertEqual(session.scripts[0][0][0], 0xA3)

            gate = asyncio.Event()

            async def block(_seconds: int) -> None:
                await gate.wait()

            stage._sleep = block
            await stage.apply_pattern(pattern)
            await asyncio.sleep(0)
            await stage.apply_color({"r": 1, "g": 2, "b": 3})
            self.assertEqual(session.state["mode"], "color")
            self.assertFalse(session.state["playing"])

        asyncio.run(scenario())

    def test_startup_applies_only_the_latest_due_schedule(self) -> None:
        async def scenario() -> None:
            with tempfile.TemporaryDirectory() as tmp:
                catalog = library.Library(Path(tmp))
                session = _FakeSession()
                stage = playback.Show(session, catalog)
                for schedule_id, clock in (("afternoon", "18:00"), ("evening", "19:30")):
                    catalog.save_schedule(
                        library.validate_schedule(
                            {"id": schedule_id, "days": ["wed"], "time": clock, "action": {"power": False}},
                            {},
                            scene_info,
                        ),
                        create=True,
                    )
                now = datetime(2026, 10, 28, 20, 0, tzinfo=ZONE)
                chosen = await stage.fire_due(now)
                assert chosen is not None
                self.assertEqual(chosen["id"], "evening")
                self.assertIsNone(await stage.fire_due(now))
                self.assertEqual(session.power, [False])

        asyncio.run(scenario())


class TokenDoorTest(unittest.TestCase):
    def test_token_file_is_mode_0600_and_lanes_do_not_cross(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "agents.json"
            token, replaced = agents.create_agent(path, "hermes", ["read", "control", "library", "schedule"])
            self.assertFalse(replaced)
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            stored = agents.load_agents(path)
            browser = panel.session_token("secret")
            self.assertIsNone(agents.authenticate("lan", f"Bearer {token}", browser, browser, stored))
            self.assertIsNone(agents.authenticate("agent", None, browser, browser, stored))
            actor = agents.authenticate("agent", f"Bearer {token}", None, browser, stored)
            assert actor is not None
            self.assertEqual(actor[0], "hermes")
            self.assertEqual(actor[1], frozenset(agents.SCOPES))
            path.chmod(0o644)
            with self.assertRaises(PermissionError):
                agents.load_agents(path)

    def test_http_rejects_a_bearer_on_the_lan_and_a_cookie_on_the_socket(self) -> None:
        async def scenario() -> None:
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                agents_path = root / "agents.json"
                full_token, _replaced = agents.create_agent(agents_path, "hermes", list(agents.SCOPES))
                session = panel.LightSession()
                session.preview = True
                session.profile = light.sized_profile(light.profile_for(1, 1), 1)
                session.state["on"] = True
                catalog = library.Library(root / "data")
                stage = playback.Show(session, catalog)
                limiter = library.WriteLimiter()
                app = panel.create_app("secret", session, None, catalog, stage, limiter, agents_path)
                browser = panel.session_token("secret")
                lan_bearer = await _invoke(
                    app, "GET", "/api/state", channel="lan", authorization=f"Bearer {full_token}"
                )
                self.assertEqual(lan_bearer[0], 401)
                socket_cookie = await _invoke(app, "GET", "/api/state", channel="agent", cookie=browser)
                self.assertEqual(socket_cookie[0], 401)
                allowed = await _invoke(
                    app, "GET", "/api/state", channel="agent", authorization=f"Bearer {full_token}"
                )
                self.assertEqual(allowed[0], 200)
                self.assertIn(b'"device":"h7020"', allowed[1])
                hidden = await _invoke(app, "GET", "/api/manifest", channel="lan", cookie=browser)
                self.assertEqual(hidden[0], 404)
                shown = await _invoke(
                    app, "GET", "/api/manifest", channel="agent", authorization=f"Bearer {full_token}"
                )
                self.assertEqual(shown[0], 200)
                self.assertIn(b"Sunset Glow", shown[1])
                self.assertIn(b"raw_frames", shown[1])
                self.assertNotIn(full_token.encode(), shown[1])
                audit = (catalog.audit_path).read_text(encoding="utf-8")
                self.assertIn("hermes", audit)
                self.assertNotIn(full_token, audit)
                saved = await _invoke(
                    app,
                    "POST",
                    "/api/patterns",
                    channel="agent",
                    authorization=f"Bearer {full_token}",
                    body=json.dumps(
                        {"id": "walk", "name": "Walk", "kind": "scene", "scene": {"name": "Sunset Glow"}}
                    ).encode(),
                )
                self.assertEqual(saved[0], 200, saved[1])
                scheduled = await _invoke(
                    app,
                    "POST",
                    "/api/schedules",
                    channel="agent",
                    authorization=f"Bearer {full_token}",
                    body=json.dumps(
                        {
                            "id": "evening",
                            "days": ["wed"],
                            "time": "19:30",
                            "action": {"pattern": "walk"},
                        }
                    ).encode(),
                )
                self.assertEqual(scheduled[0], 200, scheduled[1])
                read_only, _replaced = agents.create_agent(agents_path, "hermes", ["read"])
                denied = await _invoke(
                    app, "POST", "/api/scene", channel="agent", authorization=f"Bearer {read_only}", body=b"{}"
                )
                self.assertEqual(denied[0], 403)
                app.state.limiter.hits = [time.monotonic()] * 30
                slowed = await _invoke(
                    app,
                    "POST",
                    "/api/power",
                    channel="lan",
                    cookie=browser,
                    body=b'{"on":false}',
                )
                self.assertEqual(slowed[0], 429)

        asyncio.run(scenario())


class _FakeSession:
    def __init__(self) -> None:
        self.profile = light.sized_profile(light.profile_for(1, 1), 1)
        self.state: dict = {"on": True, "playing": False}
        self.scripts: list[list[bytes]] = []
        self.power: list[bool] = []

    def snapshot(self) -> dict:
        return dict(self.state)

    async def submit(self, replace_script: bool = False, **fields: object) -> dict:
        if "on" in fields:
            self.power.append(bool(fields["on"]))
        self.state.update(fields)
        return self.snapshot()

    async def submit_script(self, packets: list[bytes], **fields: object) -> dict:
        self.scripts.append(list(packets))
        self.state.update(fields)
        return self.snapshot()


def _record(slept: list[int]):
    async def sleeper(seconds: int) -> None:
        slept.append(seconds)

    return sleeper


async def _invoke(
    app,
    method: str,
    path: str,
    *,
    channel: str,
    authorization: str | None = None,
    cookie: str | None = None,
    body: bytes = b"",
) -> tuple[int, bytes]:
    messages: list[dict] = []

    async def receive() -> dict:
        return {"type": "http.request", "body": body, "more_body": False}

    async def send(message: dict) -> None:
        messages.append(message)

    headers: list[tuple[bytes, bytes]] = []
    if authorization is not None:
        headers.append((b"authorization", authorization.encode()))
    if cookie is not None:
        headers.append((b"cookie", f"{panel.COOKIE}={cookie}".encode()))
    if body:
        headers.append((b"content-type", b"application/json"))
    scope = {
        "type": "http",
        "asgi": {"version": "3.0", "spec_version": "2.3"},
        "http_version": "1.1",
        "method": method,
        "scheme": "http",
        "path": path,
        "raw_path": path.encode(),
        "query_string": b"",
        "headers": headers,
        "client": ("127.0.0.1", 9),
        "server": ("127.0.0.1", 80),
        "tallow_channel": channel,
    }
    await app(scope, receive, send)
    status = next(message["status"] for message in messages if message["type"] == "http.response.start")
    raw = b"".join(message.get("body", b"") for message in messages if message["type"] == "http.response.body")
    return status, raw


if __name__ == "__main__":
    unittest.main()
