"""The menu, Toby's cams, and the agent socket that never sees either."""

from __future__ import annotations

import asyncio
import json
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "app"))
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import agents  # noqa: E402
import cams as camlib  # noqa: E402
import govee_h7020 as light  # noqa: E402
import h7020_panel as panel  # noqa: E402
import library  # noqa: E402
import show as playback  # noqa: E402
from test_h7020_library import _invoke  # noqa: E402

def part(number: int) -> bytes:
    jpeg = b"\xff\xd8" + b"%04d" % number + b"\xff\xd9"
    return b"Content-Type: image/jpeg\r\nContent-Length: %d\r\n\r\n" % len(jpeg) + jpeg + b"\r\n--123456789\r\n"


class FakeCam:
    """What the ESPHome camera web server sends: no chunking, a Content-Length on every part."""

    def __init__(
        self,
        content_type: bytes = b"multipart/x-mixed-replace;boundary=123456789",
        cut_after: int | None = None,
        silent: bool = False,
    ) -> None:
        self.content_type = content_type
        self.cut_after = cut_after
        self.silent = silent
        self.connections = 0
        self.open = 0
        self.hung_up = asyncio.Event()
        self.server: asyncio.Server | None = None
        self.writers: list[asyncio.StreamWriter] = []

    async def start(self) -> int:
        self.server = await asyncio.start_server(self._serve, "127.0.0.1", 0)
        return self.server.sockets[0].getsockname()[1]

    async def _serve(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        self.connections += 1
        self.open += 1
        self.writers.append(writer)
        self.hung_up.clear()
        try:
            await reader.readuntil(b"\r\n\r\n")
            writer.write(b"HTTP/1.1 200 OK\r\nContent-Type: " + self.content_type + b"\r\n\r\n--123456789\r\n")
            number = 0
            while True:
                number += 1
                if self.cut_after is not None and number > self.cut_after:
                    if self.silent:
                        await reader.read()  # say nothing until the panel hangs up
                    return
                writer.write(part(number))
                await writer.drain()
                await asyncio.sleep(0.01)
        except (ConnectionError, OSError, asyncio.IncompleteReadError):
            pass
        finally:
            self.open -= 1
            self.hung_up.set()
            writer.close()

    async def stop(self) -> None:
        for writer in self.writers:
            writer.close()
        if self.server is not None:
            self.server.close()
            await self.server.wait_closed()


class Viewer:
    """A browser holding a stream open until told to leave."""

    def __init__(self, app, path: str, cookie: str, spec_version: str) -> None:
        self.app = app
        self.path = path
        self.cookie = cookie
        self.spec_version = spec_version
        self.status: int | None = None
        self.content_type = b""
        self.body = b""
        self.got_frame = asyncio.Event()
        self.answered = asyncio.Event()
        self.leave = asyncio.Event()
        self.task: asyncio.Task | None = None

    def start(self) -> None:
        self.task = asyncio.create_task(self._run())

    async def _run(self) -> None:
        sent_request = False

        async def receive() -> dict:
            nonlocal sent_request
            if not sent_request:
                sent_request = True
                return {"type": "http.request", "body": b"", "more_body": False}
            await self.leave.wait()
            return {"type": "http.disconnect"}

        async def send(message: dict) -> None:
            if message["type"] == "http.response.start":
                self.status = message["status"]
                self.content_type = dict(message["headers"]).get(b"content-type", b"")
                self.answered.set()
            elif message["type"] == "http.response.body":
                if self.leave.is_set():
                    raise OSError("viewer left")
                self.body += message.get("body", b"")
                if b"\xff\xd9" in self.body:
                    self.got_frame.set()

        scope = {
            "type": "http",
            "asgi": {"version": "3.0", "spec_version": self.spec_version},
            "http_version": "1.1",
            "method": "GET",
            "scheme": "http",
            "path": self.path,
            "raw_path": self.path.encode(),
            "query_string": b"",
            "headers": [(b"cookie", f"{panel.COOKIE}={self.cookie}".encode())],
            "client": ("127.0.0.1", 9),
            "server": ("127.0.0.1", 80),
            "tallow_channel": "lan",
        }
        try:
            await self.app(scope, receive, send)
        except Exception:  # noqa: BLE001 — a viewer leaving mid-send is what is being tested
            pass
        finally:
            self.answered.set()


def make_app(root: Path, cams: list[camlib.Cam]):
    session = panel.LightSession()
    session.preview = True
    session.profile = light.sized_profile(light.profile_for(1, 1), 1)
    catalog = library.Library(root / "data")
    stage = playback.Show(session, catalog)
    return panel.create_app(
        "secret", session, None, catalog, stage, library.WriteLimiter(), root / "agents.json", cams=cams
    )


class PagesTest(unittest.TestCase):
    def test_the_menu_leads_to_the_lights_the_cams_and_settings(self) -> None:
        async def scenario() -> None:
            with tempfile.TemporaryDirectory() as tmp:
                app = make_app(Path(tmp), [])
                browser = panel.session_token("secret")
                status, body = await _invoke(app, "GET", "/", channel="lan", cookie=browser)
                self.assertEqual(status, 200)
                self.assertIn(b'href="/lights"', body)
                self.assertIn(b'href="/cams"', body)
                self.assertIn(b'href="/settings"', body)
                self.assertIn(b"<strong>Cameras</strong>", body)
                status, body = await _invoke(app, "GET", "/lights", channel="lan", cookie=browser)
                self.assertEqual(status, 200)
                self.assertIn(b"phones home", body)
                status, body = await _invoke(app, "GET", "/cams", channel="lan", cookie=browser)
                self.assertEqual(status, 200)
                self.assertIn(b"Nothing is saved", body)
                self.assertIn(b"<h1>Cameras</h1>", body)
                self.assertIn(b'href="/settings"', body)
                status, body = await _invoke(app, "GET", "/settings", channel="lan", cookie=browser)
                self.assertEqual(status, 200)
                self.assertIn(b"Log out", body)
                self.assertIn(b"10.87.0.0/24", body)
                self.assertNotIn(b"__IOT_NET__", body)
                self.assertNotIn(b"__CAMS_TITLE__", body)

        asyncio.run(scenario())

    def test_without_the_cookie_pages_go_to_login(self) -> None:
        async def scenario() -> None:
            with tempfile.TemporaryDirectory() as tmp:
                app = make_app(Path(tmp), [camlib.Cam("den", "Den", "10.87.0.21")])
                status, body = await _invoke(app, "GET", "/", channel="lan")
                self.assertEqual(status, 200)
                self.assertIn(b"Unlock", body)
                for path in ("/lights", "/cams", "/settings"):
                    status, _body = await _invoke(app, "GET", path, channel="lan")
                    self.assertEqual(status, 303, path)
                for path in ("/api/cams", "/cams/den/stream", "/api/settings/cameras"):
                    status, _body = await _invoke(app, "GET", path, channel="lan")
                    self.assertEqual(status, 401, path)

        asyncio.run(scenario())

    def test_the_agent_socket_sees_no_menu_and_no_cams(self) -> None:
        async def scenario() -> None:
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                app = make_app(root, [camlib.Cam("den", "Den", "10.87.0.21")])
                token, _replaced = agents.create_agent(root / "agents.json", "hermes", list(agents.SCOPES))
                bearer = f"Bearer {token}"
                for path in (
                    "/lights",
                    "/cams",
                    "/settings",
                    "/panel.css",
                    "/api/cams",
                    "/cams/den/stream",
                    "/api/settings/cameras",
                ):
                    status, _body = await _invoke(app, "GET", path, channel="agent", authorization=bearer)
                    self.assertEqual(status, 404, path)
                status, _body = await _invoke(
                    app, "PUT", "/api/settings/cameras", channel="agent", authorization=bearer, body=b"{}"
                )
                self.assertEqual(status, 404)
                status, _body = await _invoke(app, "POST", "/logout", channel="agent", authorization=bearer)
                self.assertEqual(status, 404)
                status, body = await _invoke(app, "GET", "/api/manifest", channel="agent", authorization=bearer)
                self.assertEqual(status, 200)
                routes = json.loads(body)["routes"]
                self.assertFalse(
                    [route for route in routes if "cam" in route or "lights" in route or "settings" in route]
                )

        asyncio.run(scenario())

    def test_the_cam_list_keeps_addresses_on_the_host(self) -> None:
        async def scenario() -> None:
            with tempfile.TemporaryDirectory() as tmp:
                app = make_app(Path(tmp), [camlib.Cam("den", "Den", "10.87.0.21")])
                status, body = await _invoke(
                    app, "GET", "/api/cams", channel="lan", cookie=panel.session_token("secret")
                )
                self.assertEqual(status, 200)
                self.assertEqual(json.loads(body), {"cams": [{"id": "den", "name": "Den"}], "error": None})
                self.assertNotIn(b"10.87", body)

        asyncio.run(scenario())

    def test_a_cam_added_to_the_file_shows_up_without_a_restart(self) -> None:
        async def scenario() -> None:
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                path = root / "cams.json"
                app = make_app(root, camlib.Book(path))
                browser = panel.session_token("secret")
                _status, body = await _invoke(app, "GET", "/api/cams", channel="lan", cookie=browser)
                self.assertEqual(json.loads(body)["cams"], [])
                path.write_text(json.dumps({"cams": [{"id": "den", "name": "Den", "address": "10.87.0.21"}]}))
                _status, body = await _invoke(app, "GET", "/api/cams", channel="lan", cookie=browser)
                self.assertEqual([cam["id"] for cam in json.loads(body)["cams"]], ["den"])

        asyncio.run(scenario())

    def test_settings_saves_cameras_and_hides_addresses_everywhere_else(self) -> None:
        async def scenario() -> None:
            with tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / "cams.json"
                path.write_text(json.dumps({"cams": [{"id": "den", "name": "Den", "address": "10.87.0.21"}]}))
                app = make_app(Path(tmp), camlib.Book(path))
                browser = panel.session_token("secret")
                status, body = await _invoke(app, "GET", "/api/settings/cameras", channel="lan", cookie=browser)
                self.assertEqual(status, 200)
                self.assertEqual(
                    json.loads(body),
                    {
                        "title": "Cameras",
                        "cams": [{"id": "den", "name": "Den", "address": "10.87.0.21"}],
                        "error": None,
                    },
                )
                status, body = await _invoke(app, "GET", "/", channel="lan", cookie=browser)
                self.assertIn(b"<strong>Cameras</strong>", body)
                bad = json.dumps(
                    {"title": "Yard", "cams": [{"id": "den", "name": "Den", "address": "192.168.1.9"}]}
                ).encode()
                status, body = await _invoke(
                    app, "PUT", "/api/settings/cameras", channel="lan", cookie=browser, body=bad
                )
                self.assertEqual(status, 400)
                self.assertIn(b"must be a node on", body)
                self.assertEqual(json.loads(path.read_text(encoding="utf-8"))["cams"][0]["address"], "10.87.0.21")
                status, body = await _invoke(
                    app,
                    "PUT",
                    "/api/settings/cameras",
                    channel="lan",
                    cookie=browser,
                    body=json.dumps(
                        {
                            "title": "A & B",
                            "extra": True,
                            "cams": [{"id": "porch", "name": "Porch", "address": "10.87.0.22"}],
                        }
                    ).encode(),
                )
                self.assertEqual(status, 400)
                self.assertIn(b"unknown fields", body)
                status, body = await _invoke(
                    app,
                    "PUT",
                    "/api/settings/cameras",
                    channel="lan",
                    cookie=browser,
                    body=json.dumps(
                        {"title": "A & B", "cams": [{"id": "porch", "name": "Porch", "address": "10.87.0.22"}]}
                    ).encode(),
                )
                self.assertEqual(status, 200, body)
                saved = json.loads(body)
                self.assertEqual(saved["title"], "A & B")
                self.assertEqual(saved["cams"], [{"id": "porch", "name": "Porch", "address": "10.87.0.22"}])
                status, body = await _invoke(app, "GET", "/api/cams", channel="lan", cookie=browser)
                self.assertEqual(json.loads(body)["cams"], [{"id": "porch", "name": "Porch"}])
                self.assertNotIn(b"10.87", body)
                status, body = await _invoke(app, "GET", "/", channel="lan", cookie=browser)
                self.assertIn(b"<strong>A &amp; B</strong>", body)
                self.assertNotIn(b"<strong>A & B</strong>", body)
                status, body = await _invoke(app, "GET", "/cams", channel="lan", cookie=browser)
                self.assertIn(b"<h1>A &amp; B</h1>", body)

        asyncio.run(scenario())

    def test_logout_clears_the_cookie(self) -> None:
        async def scenario() -> None:
            with tempfile.TemporaryDirectory() as tmp:
                app = make_app(Path(tmp), [])
                browser = panel.session_token("secret")
                messages: list[dict] = []

                async def receive() -> dict:
                    return {"type": "http.request", "body": b"", "more_body": False}

                async def send(message: dict) -> None:
                    messages.append(message)

                scope = {
                    "type": "http",
                    "asgi": {"version": "3.0", "spec_version": "2.3"},
                    "http_version": "1.1",
                    "method": "POST",
                    "scheme": "http",
                    "path": "/logout",
                    "raw_path": b"/logout",
                    "query_string": b"",
                    "headers": [(b"cookie", f"{panel.COOKIE}={browser}".encode())],
                    "client": ("127.0.0.1", 9),
                    "server": ("127.0.0.1", 80),
                    "tallow_channel": "lan",
                }
                await app(scope, receive, send)
                start = next(message for message in messages if message["type"] == "http.response.start")
                headers = dict(start["headers"])
                self.assertEqual(start["status"], 303)
                self.assertEqual(headers[b"location"], b"/login")
                cleared = headers[b"set-cookie"].decode()
                self.assertIn(f"{panel.COOKIE}=", cleared)
                self.assertIn("Max-Age=0", cleared)

        asyncio.run(scenario())

    def test_status_asks_the_node_and_never_the_camera_port_of_a_streaming_cam(self) -> None:
        async def scenario() -> None:
            fake = FakeCam()
            port = await fake.start()
            dead = await asyncio.start_server(lambda r, w: None, "127.0.0.1", 0)
            dead_port = dead.sockets[0].getsockname()[1]
            dead.close()
            await dead.wait_closed()
            try:
                with tempfile.TemporaryDirectory() as tmp:
                    cams = [
                        camlib.Cam("up", "Up", "127.0.0.1", port, node_port=port),
                        camlib.Cam("down", "Down", "127.0.0.1", dead_port, node_port=dead_port),
                    ]
                    app = make_app(Path(tmp), cams)
                    browser = panel.session_token("secret")
                    _status, body = await _invoke(app, "GET", "/api/cams/status", channel="lan", cookie=browser)
                    self.assertEqual(
                        json.loads(body),
                        {"up": {"online": True, "viewers": 0}, "down": {"online": False, "viewers": 0}},
                    )
                    await asyncio.sleep(0.05)
                    self.assertEqual(fake.open, 0)
                    viewer = Viewer(app, "/cams/up/stream", browser, "2.3")
                    viewer.start()
                    await asyncio.wait_for(viewer.got_frame.wait(), 3)
                    before = fake.connections
                    _status, body = await _invoke(app, "GET", "/api/cams/status", channel="lan", cookie=browser)
                    self.assertEqual(json.loads(body)["up"], {"online": True, "viewers": 1})
                    self.assertEqual(fake.connections, before)
                    viewer.leave.set()
                    await asyncio.wait_for(viewer.task, 3)
            finally:
                await fake.stop()

        asyncio.run(scenario())


class BookTest(unittest.TestCase):
    def test_a_bad_edit_keeps_the_last_good_list_and_says_why(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "cams.json"
            path.write_text(json.dumps({"cams": [{"id": "den", "address": "10.87.0.21"}]}))
            book = camlib.Book(path)
            self.assertEqual([cam.id for cam in book.all()], ["den"])
            self.assertIsNone(book.error)
            time.sleep(0.01)
            path.write_text('{"cams": [{"id": "den", "address": "192.168.1.9"}]}')
            self.assertEqual([cam.id for cam in book.all()], ["den"])
            self.assertIn("must be a node on", book.error or "")
            path.write_text("{")
            self.assertEqual([cam.id for cam in book.all()], ["den"])
            self.assertIn("not JSON", book.error or "")
            path.write_text(json.dumps({"cams": []}))
            self.assertEqual(book.all(), [])
            self.assertEqual(book.title(), camlib.DEFAULT_TITLE)
            self.assertIsNone(book.error)

    def test_a_title_is_optional_and_a_bad_edit_keeps_the_last_good_one(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "cams.json"
            path.write_text(json.dumps({"cams": [{"id": "den", "address": "10.87.0.21"}]}))
            book = camlib.Book(path)
            self.assertEqual(book.title(), "Cameras")
            time.sleep(0.01)
            path.write_text(json.dumps({"title": "Yard", "cams": [{"id": "den", "address": "10.87.0.21"}]}))
            self.assertEqual(book.title(), "Yard")
            time.sleep(0.01)
            path.write_text(json.dumps({"title": "Yard", "cams": [], "note": "no"}))
            self.assertEqual(book.title(), "Yard")
            self.assertIn("unknown fields", book.error or "")
            with self.assertRaises(ValueError):
                camlib.parse_inventory({"title": "", "cams": []})
            with self.assertRaises(ValueError):
                camlib.parse_inventory({"title": "x" * 41, "cams": []})

    def test_save_then_load_round_trips_the_inventory(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "cams.json"
            title, cams = camlib.save_inventory(
                path,
                {"title": " Toby ", "cams": [{"id": "den", "name": "Den", "address": "10.87.0.21"}]},
            )
            self.assertEqual(title, "Toby")
            self.assertEqual([(cam.id, cam.name, cam.address) for cam in cams], [("den", "Den", "10.87.0.21")])
            self.assertEqual(camlib.load_inventory(path), (title, cams))
            raw = json.loads(path.read_text(encoding="utf-8"))
            self.assertEqual(list(raw), ["title", "cams"])
            self.assertNotIn("port", raw["cams"][0])

    def test_a_slow_viewer_skips_frames_instead_of_holding_the_rest_back(self) -> None:
        queue: asyncio.Queue[bytes | None] = asyncio.Queue(maxsize=2)
        for frame in (b"1", b"2", b"3", b"4"):
            camlib._offer(queue, frame)
        self.assertEqual([queue.get_nowait(), queue.get_nowait()], [b"3", b"4"])


class StreamTest(unittest.TestCase):
    def setUp(self) -> None:
        patch = mock.patch.object(camlib, "LINGER_S", 0.0)
        patch.start()
        self.addCleanup(patch.stop)

    def run_viewers(self, spec_version: str) -> None:
        async def scenario() -> None:
            fake = FakeCam()
            port = await fake.start()
            try:
                with tempfile.TemporaryDirectory() as tmp:
                    app = make_app(Path(tmp), [camlib.Cam("den", "Den", "127.0.0.1", port)])
                    browser = panel.session_token("secret")
                    first = Viewer(app, "/cams/den/stream", browser, spec_version)
                    second = Viewer(app, "/cams/den/stream", browser, spec_version)
                    first.start()
                    second.start()
                    await asyncio.wait_for(first.got_frame.wait(), 3)
                    await asyncio.wait_for(second.got_frame.wait(), 3)
                    self.assertEqual(first.status, 200)
                    self.assertTrue(first.content_type.startswith(b"multipart/x-mixed-replace"))
                    self.assertIn(b"boundary=tallow", first.content_type)
                    # The cam holds one connection at a time. Two screens share it.
                    self.assertEqual(fake.connections, 1)

                    first.leave.set()
                    await asyncio.wait_for(first.task, 3)
                    self.assertEqual(fake.open, 1)
                    seen = len(second.body)
                    await asyncio.sleep(0.1)
                    self.assertGreater(len(second.body), seen)

                    second.leave.set()
                    await asyncio.wait_for(second.task, 3)
                    await asyncio.wait_for(fake.hung_up.wait(), 3)
                    self.assertEqual(app.state.hubs.watching(camlib.Cam("den", "Den", "127.0.0.1", port)), 0)

                    # Watching again wakes the cam again, after the old connection is gone.
                    again = Viewer(app, "/cams/den/stream", browser, spec_version)
                    again.start()
                    await asyncio.wait_for(again.got_frame.wait(), 3)
                    self.assertEqual(fake.connections, 2)
                    self.assertEqual(fake.open, 1)
                    again.leave.set()
                    await asyncio.wait_for(again.task, 3)
            finally:
                await fake.stop()

        asyncio.run(scenario())

    def test_screens_share_one_cam_connection_when_the_server_reports_disconnect(self) -> None:
        self.run_viewers("2.3")

    def test_screens_share_one_cam_connection_when_the_send_fails(self) -> None:
        self.run_viewers("2.4")

    def test_a_cam_serves_only_so_many_screens(self) -> None:
        async def scenario() -> None:
            fake = FakeCam()
            port = await fake.start()
            try:
                with tempfile.TemporaryDirectory() as tmp, mock.patch.object(camlib, "VIEWERS_MAX", 2):
                    app = make_app(Path(tmp), [camlib.Cam("den", "Den", "127.0.0.1", port)])
                    browser = panel.session_token("secret")
                    held = [Viewer(app, "/cams/den/stream", browser, "2.3") for _ in range(2)]
                    for viewer in held:
                        viewer.start()
                        await asyncio.wait_for(viewer.got_frame.wait(), 3)
                    extra = Viewer(app, "/cams/den/stream", browser, "2.3")
                    extra.start()
                    await asyncio.wait_for(extra.answered.wait(), 3)
                    self.assertEqual(extra.status, 429)
                    for viewer in held:
                        viewer.leave.set()
                    await asyncio.wait_for(asyncio.gather(*(v.task for v in held)), 3)
            finally:
                await fake.stop()

        asyncio.run(scenario())

    def test_a_dead_cam_is_a_502_and_leaves_nobody_counted(self) -> None:
        async def scenario() -> None:
            probe = await asyncio.start_server(lambda r, w: None, "127.0.0.1", 0)
            port = probe.sockets[0].getsockname()[1]
            probe.close()
            await probe.wait_closed()
            with tempfile.TemporaryDirectory() as tmp:
                cam = camlib.Cam("den", "Den", "127.0.0.1", port)
                app = make_app(Path(tmp), [cam])
                status, body = await _invoke(
                    app, "GET", "/cams/den/stream", channel="lan", cookie=panel.session_token("secret")
                )
                self.assertEqual(status, 502)
                self.assertIn(b"not answering", body)
                self.assertEqual(app.state.hubs.watching(cam), 0)

        asyncio.run(scenario())

    def test_a_cam_that_is_not_a_stream_is_a_502(self) -> None:
        async def scenario() -> None:
            fake = FakeCam(content_type=b"text/html")
            port = await fake.start()
            try:
                with tempfile.TemporaryDirectory() as tmp:
                    app = make_app(Path(tmp), [camlib.Cam("den", "Den", "127.0.0.1", port)])
                    status, _body = await _invoke(
                        app, "GET", "/cams/den/stream", channel="lan", cookie=panel.session_token("secret")
                    )
                    self.assertEqual(status, 502)
                    status, _body = await _invoke(
                        app, "GET", "/cams/nope/stream", channel="lan", cookie=panel.session_token("secret")
                    )
                    self.assertEqual(status, 404)
            finally:
                await fake.stop()

        asyncio.run(scenario())


class RecoveryTest(unittest.TestCase):
    """A cam that drops, goes quiet, or comes back must not need a panel restart."""

    def setUp(self) -> None:
        patch = mock.patch.object(camlib, "LINGER_S", 0.0)
        patch.start()
        self.addCleanup(patch.stop)
        slow = mock.patch.object(camlib, "COOLDOWN_S", (0.0,))
        slow.start()
        self.addCleanup(slow.stop)

    def run_drop(self, silent: bool) -> None:
        async def scenario() -> None:
            fake = FakeCam(cut_after=5, silent=silent)
            port = await fake.start()
            try:
                with tempfile.TemporaryDirectory() as tmp, mock.patch.object(camlib, "FRAME_TIMEOUT", 0.3):
                    cam = camlib.Cam("den", "Den", "127.0.0.1", port)
                    app = make_app(Path(tmp), [cam])
                    browser = panel.session_token("secret")
                    first = Viewer(app, "/cams/den/stream", browser, "2.4")
                    first.start()
                    await asyncio.wait_for(first.got_frame.wait(), 3)
                    # The cam goes. The viewer's response ends instead of hanging on a dead feed.
                    await asyncio.wait_for(first.task, 3)
                    self.assertEqual(app.state.hubs.watching(cam), 0)
                    # The cam comes back, and a new viewer gets it with no restart.
                    fake.cut_after = None
                    again = Viewer(app, "/cams/den/stream", browser, "2.4")
                    again.start()
                    await asyncio.wait_for(again.got_frame.wait(), 3)
                    self.assertEqual(again.status, 200)
                    again.leave.set()
                    await asyncio.wait_for(again.task, 3)
            finally:
                await fake.stop()

        asyncio.run(scenario())

    def test_a_cam_that_hangs_up_mid_stream_comes_back_without_a_restart(self) -> None:
        self.run_drop(silent=False)

    def test_a_cam_that_goes_quiet_comes_back_without_a_restart(self) -> None:
        self.run_drop(silent=True)


class GentleTest(unittest.TestCase):
    """The cam's web server holds one connection and jams under churn. Do not churn it."""

    def test_a_quick_stop_and_watch_keeps_the_same_connection(self) -> None:
        async def scenario() -> None:
            fake = FakeCam()
            port = await fake.start()
            try:
                with tempfile.TemporaryDirectory() as tmp, mock.patch.object(camlib, "LINGER_S", 0.3):
                    cam = camlib.Cam("den", "Den", "127.0.0.1", port)
                    app = make_app(Path(tmp), [cam])
                    browser = panel.session_token("secret")
                    first = Viewer(app, "/cams/den/stream", browser, "2.4")
                    first.start()
                    await asyncio.wait_for(first.got_frame.wait(), 3)
                    first.leave.set()
                    await asyncio.wait_for(first.task, 3)
                    # A tab switch, a reload, Stop then Watch: back inside the linger.
                    again = Viewer(app, "/cams/den/stream", browser, "2.4")
                    again.start()
                    await asyncio.wait_for(again.got_frame.wait(), 3)
                    self.assertEqual(fake.connections, 1)
                    again.leave.set()
                    await asyncio.wait_for(again.task, 3)
                    # Nobody comes back, so the panel hangs up and the cam stops sending.
                    await asyncio.wait_for(fake.hung_up.wait(), 3)
                    self.assertTrue(app.state.hubs.hub(cam).idle)
            finally:
                await fake.stop()

        asyncio.run(scenario())

    def test_a_hub_that_is_still_lingering_is_not_pruned(self) -> None:
        async def scenario() -> None:
            fake = FakeCam()
            port = await fake.start()
            try:
                cam = camlib.Cam("den", "Den", "127.0.0.1", port)
                hubs = camlib.Hubs()
                with mock.patch.object(camlib, "LINGER_S", 5.0):
                    hub = hubs.hub(cam)
                    queue = await hub.join()
                    hub.leave(queue)
                    hubs.prune([])
                    self.assertIs(hubs.hub(cam), hub)
                with mock.patch.object(camlib, "LINGER_S", 0.0):
                    hub._hang_up()
                    await asyncio.wait({hub._pump})
                    hubs.prune([])
                    self.assertIsNot(hubs.hub(cam), hub)
            finally:
                await fake.stop()

        asyncio.run(scenario())

    def test_after_a_failed_dial_the_cam_is_left_alone_for_a_while(self) -> None:
        async def scenario() -> None:
            probe = await asyncio.start_server(lambda r, w: None, "127.0.0.1", 0)
            port = probe.sockets[0].getsockname()[1]
            probe.close()
            await probe.wait_closed()
            now = [100.0]
            tries: list[float] = []
            real = camlib.dial

            async def counting(cam: camlib.Cam):
                tries.append(now[0])
                return await real(cam)

            hub = camlib.Hub(camlib.Cam("den", "Den", "127.0.0.1", port), counting, clock=lambda: now[0])
            for expected_wait in (3.0, 6.0, 12.0, 24.0, 30.0, 30.0):
                with self.assertRaises(camlib.CamDown):
                    await hub.join()
                made = len(tries)
                with self.assertRaises(camlib.CamDown) as cooling:
                    await hub.join()
                self.assertIn("cooling off", str(cooling.exception))
                self.assertEqual(len(tries), made, "the cam was dialed during the cooling-off")
                now[0] += expected_wait + 0.1
            self.assertEqual(len(tries), 6)

        asyncio.run(scenario())

    def test_a_dial_that_is_cancelled_does_not_leave_a_request_in_the_cams_queue(self) -> None:
        async def scenario() -> None:
            closed = asyncio.Event()

            async def silent(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
                await reader.readuntil(b"\r\n\r\n")  # takes the request and never answers
                await reader.read()
                closed.set()
                writer.close()

            server = await asyncio.start_server(silent, "127.0.0.1", 0)
            port = server.sockets[0].getsockname()[1]
            try:
                task = asyncio.create_task(camlib.dial(camlib.Cam("den", "Den", "127.0.0.1", port)))
                await asyncio.sleep(0.2)
                task.cancel()
                with self.assertRaises(asyncio.CancelledError):
                    await task
                await asyncio.wait_for(closed.wait(), 2)
            finally:
                server.close()
                await server.wait_closed()

        asyncio.run(scenario())


class LightAwayTest(unittest.TestCase):
    def test_the_panel_stays_up_when_the_light_does_not_answer(self) -> None:
        async def scenario() -> None:
            session = panel.LightSession()
            tries: list[int] = []

            async def refuse(identify: bool) -> None:
                tries.append(1)
                raise SystemExit("Advertisement did not include a color pact.")

            session._bind = refuse  # type: ignore[method-assign]
            connect = {"address": "AA:BB:CC:DD:EE:FF", "adapter": None, "scan_seconds": 0, "timeout": 1, "strings": None}
            app = panel.build_runtime("secret", connect, session=session)
            async with app.router.lifespan_context(app):
                self.assertFalse(session.linked())
                self.assertIn("color pact", session.state["error"])
                self.assertIsNotNone(session._writer)
                await asyncio.sleep(0.1)
            self.assertGreaterEqual(len(tries), 2)

        asyncio.run(scenario())


if __name__ == "__main__":
    unittest.main()
