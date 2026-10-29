#!/usr/bin/env python3
"""LAN page for one Govee H7020. Holds a single BLE connection from this host.

Packets come from tools/govee_h7020.py. This file does not copy frames.

  .venv/bin/python app/h7020_panel.py
  .venv/bin/python app/h7020_panel.py --preview

Create secrets/panel/password (mode 0600) first. The process binds a house LAN
address, never 0.0.0.0 and never the IoT access point. Agent tokens are
accepted only on the host unix socket data/panel/agent.sock.
"""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import hmac
import ipaddress
import json
import signal
import socket
import stat
import subprocess
import sys
from contextlib import asynccontextmanager, contextmanager
from pathlib import Path
from typing import Any

from fastapi import FastAPI, Form, HTTPException, Request
from fastapi.responses import HTMLResponse, JSONResponse, RedirectResponse

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "app"))

import agents  # noqa: E402
import govee_h7020 as light  # noqa: E402
import library  # noqa: E402
import show as playback  # noqa: E402

PASSWORD_PATH = ROOT / "secrets" / "panel" / "password"
AGENTS_PATH = ROOT / "secrets" / "panel" / "agents.json"
IOT_NET = ipaddress.ip_network("10.87.0.0/24")
COOKIE = "tallow_panel"
PHONE_HOME = (
    "This box still phones home if it remembers the house Wi-Fi. "
    "Forget that SSID and leave it off tallow-iot. BLE works with no Wi-Fi."
)

LOGIN_HTML = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>H7020</title>
<style>
  body {{ font-family: system-ui, sans-serif; margin: 0; background: #14120f; color: #f4efe6; }}
  main {{ max-width: 28rem; margin: 0 auto; padding: 1.5rem; }}
  input, button {{ width: 100%; box-sizing: border-box; font-size: 1.05rem; }}
  input {{ margin: 0.6rem 0 1rem; padding: 0.7rem; }}
  button {{ padding: 0.8rem; border: 0; border-radius: 0.6rem; background: #e7d7b1; color: #1c160f; }}
  .err {{ color: #ffb4a8; }}
</style>
</head>
<body>
<main>
  <h1>H7020</h1>
  <p class="err">{error}</p>
  <form method="post" action="/login">
    <label for="password">Password</label>
    <input id="password" name="password" type="password" autocomplete="current-password" required>
    <button type="submit">Unlock</button>
  </form>
</main>
</body>
</html>
"""

CONTROLS_HTML = (
    Path(__file__).with_name("h7020_page.html").read_text(encoding="utf-8").replace("__PHONE_HOME__", PHONE_HOME)
)


def reject_bind_host(host: str) -> str | None:
    """Refuse an unspecified address and anything on the IoT access point."""
    text = host.strip()
    if text in {"0.0.0.0", "::", "::0"}:
        return "Refusing to bind 0.0.0.0."
    try:
        ip = ipaddress.ip_address(text)
    except ValueError:
        return f"{text} is not an IP address."
    if isinstance(ip, ipaddress.IPv6Address):
        return "Pass an IPv4 house LAN address."
    if ip.is_unspecified:
        return "Refusing to bind an unspecified address."
    if ip in IOT_NET:
        return f"Refusing {text}: that address is on the IoT access point (10.87.0.0/24)."
    return None


def _usable_lan(host: str) -> bool:
    if reject_bind_host(host):
        return False
    ip = ipaddress.ip_address(host)
    return not (ip.is_loopback or ip.is_link_local)


def choose_host(explicit: str | None, route_source: str | None, locals_: list[str]) -> str:
    """Pick one house LAN address. The default route wins when it is outside the IoT subnet."""
    if explicit is not None:
        reason = reject_bind_host(explicit)
        if reason:
            raise SystemExit(reason)
        return explicit
    if route_source and _usable_lan(route_source):
        return route_source
    options: list[str] = []
    for addr in locals_:
        if _usable_lan(addr) and addr not in options:
            options.append(addr)
    if len(options) == 1:
        return options[0]
    if not options:
        raise SystemExit("No house LAN address to bind. Pass --host.")
    raise SystemExit("Pass --host. Candidates: " + ", ".join(options))


def default_route_ipv4() -> str | None:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.connect(("192.0.2.1", 9))
        return sock.getsockname()[0]
    except OSError:
        return None
    finally:
        sock.close()


def local_ipv4() -> list[str]:
    try:
        raw = subprocess.check_output(["ip", "-4", "-j", "addr"], text=True)
    except (OSError, subprocess.CalledProcessError):
        return []
    found: list[str] = []
    for iface in json.loads(raw):
        for info in iface.get("addr_info") or []:
            if info.get("family") == "inet" and info.get("local"):
                found.append(info["local"])
    return found


def session_token(password: str) -> str:
    return hmac.new(password.encode(), b"tallow-h7020-panel", hashlib.sha256).hexdigest()


def load_password(path: Path) -> str:
    if not path.is_file():
        raise SystemExit(
            "Missing panel password. Create it, then run again:\n"
            "  install -d -m 0700 secrets/panel\n"
            "  umask 077 && printf '%s\\n' 'your-password' > secrets/panel/password\n"
            "  chmod 0600 secrets/panel/password"
        )
    mode = stat.S_IMODE(path.stat().st_mode)
    if mode != 0o600:
        raise SystemExit(f"{path} is mode {mode:04o}. chmod 0600 {path}")
    password = path.read_text(encoding="utf-8").strip("\n")
    if not password or "\n" in password:
        raise SystemExit(f"{path} must be one non-empty line.")
    return password


class LightSession:
    """One GATT client. Later slider events replace a color or brightness still waiting to be written."""

    def __init__(self) -> None:
        self.client: Any = None
        self.profile: Any = None
        self.preview = False
        self.state: dict[str, Any] = {
            "device": "h7020",
            "name": None,
            "on": None,
            "brightness": None,
            "color": None,
            "firmware": None,
            "hardware": None,
            "strings": None,
            "bulbs": 15,
            "connected": False,
            "error": None,
            "note": PHONE_HOME,
            "mode": None,
            "scene": None,
            "paint": None,
            "pattern_id": None,
            "playing": False,
            "preview": False,
        }
        self._pending: dict[str, Any] = {}
        self._script: list[bytes] | None = None
        self._gate = asyncio.Lock()
        self._wake = asyncio.Event()
        self._writer: asyncio.Task[None] | None = None
        self._queue: asyncio.Queue[bytes] = asyncio.Queue()
        self._params: dict[str, Any] = {}
        self._closing = False
        self._binding = False

    def linked(self) -> bool:
        if self.preview:
            return True
        client = self.client
        return bool(client is not None and client.is_connected)

    def snapshot(self) -> dict[str, Any]:
        data = dict(self.state)
        data["connected"] = self.linked()
        data["device"] = "h7020"
        return data

    def _on_link_down(self, client: Any) -> None:
        if self._binding or client is not self.client or self._closing:
            return
        self.state["connected"] = False
        print("BLE link down; reconnecting.", file=sys.stderr)
        self._wake.set()

    async def submit(self, replace_script: bool = False, **fields: Any) -> dict[str, Any]:
        async with self._gate:
            if replace_script:
                self._script = None
            self._pending.update(fields)
            self._remember(fields)
        self._wake.set()
        return self.snapshot()

    async def submit_script(self, packets: list[bytes], **fields: Any) -> dict[str, Any]:
        """Queue one ordered scene or paint. Packets are not collapsed into each other."""
        if not packets:
            raise ValueError("script is empty")
        async with self._gate:
            self._script = list(packets)
            self._pending.clear()
            self._remember(fields)
        self._wake.set()
        return self.snapshot()

    def _remember(self, fields: dict[str, Any]) -> None:
        for key, value in fields.items():
            self.state[key] = value
        self.state["error"] = None

    def _on_notify(self, _sender: Any, data: bytearray) -> None:
        frame = bytes(data)
        self._queue.put_nowait(frame)
        if self.profile is None:
            return
        parsed = light.parse_status(frame, light.MODEL, profile=self.profile)
        if parsed is None:
            return
        fields = light._status_fields(parsed)
        pending = self._pending
        if fields["domain"] == "power" and "on" not in pending and "is_on" in fields:
            self.state["on"] = fields["is_on"]
        if fields["domain"] == "brightness" and "brightness" not in pending and "brightness_pct" in fields:
            self.state["brightness"] = fields["brightness_pct"]

    async def _send(self, packet: bytes) -> None:
        if self.preview:
            return
        await light._write(self.client, packet)

    async def _apply(self, batch: dict[str, Any]) -> None:
        if "on" in batch:
            packet = light.build_power(bool(batch["on"]), light.MODEL, profile=self.profile)
            await self._send(packet)
        if "brightness" in batch:
            packet = light.build_brightness(int(batch["brightness"]), light.MODEL, profile=self.profile)
            await self._send(packet)
        if "color" in batch:
            color = batch["color"]
            packet = light.build_color_rgb(color["r"], color["g"], color["b"], light.MODEL, profile=self.profile)
            await self._send(packet)

    async def _apply_script(self, packets: list[bytes]) -> None:
        for packet in packets:
            await self._send(packet)

    async def _writer_loop(self) -> None:
        while not self._closing:
            try:
                await asyncio.wait_for(self._wake.wait(), 4)
            except TimeoutError:
                if (
                    not self.preview
                    and self.linked()
                    and self.profile is not None
                    and not self._pending
                    and self._script is None
                ):
                    try:
                        await light._query(
                            self.client,
                            self._queue,
                            _status_query("power", self.profile),
                            self.profile,
                            "power",
                        )
                    except Exception as exc:  # noqa: BLE001 — the next pass reconnects
                        self.state["error"] = f"{type(exc).__name__}: {exc}"
                        self.state["connected"] = self.linked()
                        if not self.linked():
                            self._wake.set()
                continue
            self._wake.clear()
            if not self.linked():
                try:
                    await self._bind(identify=self.profile is None)
                except asyncio.CancelledError:
                    raise
                except (Exception, SystemExit) as exc:
                    self.state["error"] = f"{type(exc).__name__}: {exc}"
                    self.state["connected"] = False
                    print(f"Reconnect failed ({self.state['error']}).", file=sys.stderr)
                    await asyncio.sleep(2)
                    if not self._closing:
                        self._wake.set()
                continue
            while True:
                async with self._gate:
                    script = self._script
                    self._script = None
                    if script is None and not self._pending:
                        break
                    batch = self._pending
                    self._pending = {}
                try:
                    if script is not None:
                        await self._apply_script(script)
                    if batch:
                        await self._apply(batch)
                    self.state["connected"] = self.linked()
                    self.state["error"] = None
                except asyncio.CancelledError:
                    raise
                except Exception as exc:  # noqa: BLE001 — keep the page up and show the failure
                    self.state["error"] = f"{type(exc).__name__}: {exc}"
                    self.state["connected"] = self.linked()
                    async with self._gate:
                        if script is not None and self._script is None:
                            self._script = script
                        merged = dict(batch)
                        merged.update(self._pending)
                        self._pending = merged
                    self._wake.set()
                    break

    async def open(
        self,
        address: str,
        adapter: str,
        scan_seconds: float,
        timeout: float,
        strings: int | None,
    ) -> None:
        self._params = {
            "address": address,
            "adapter": adapter,
            "scan_seconds": scan_seconds,
            "timeout": timeout,
            "strings": strings,
        }
        await self._bind(identify=True)
        self._writer = asyncio.create_task(self._writer_loop())

    async def open_preview(self) -> None:
        """Serve the page without taking the BLE central."""
        self.preview = True
        self.profile = light.sized_profile(light.profile_for(1, 1), 1)
        self.state.update(
            name="H7020 preview",
            on=True,
            brightness=100,
            color={"r": 255, "g": 0, "b": 0},
            firmware="preview",
            hardware="preview",
            strings=1,
            bulbs=self.profile.segment_count,
            connected=True,
            preview=True,
            error=None,
        )
        self._writer = asyncio.create_task(self._writer_loop())

    async def _bind(self, identify: bool) -> None:
        self._binding = True
        try:
            await self._bind_locked(identify)
        finally:
            self._binding = False
        if self.linked():
            self._wake.clear()

    async def _bind_locked(self, identify: bool) -> None:
        from bleak import BleakClient

        address = self._params["address"]
        adapter = self._params["adapter"]
        seen = await light._scan(address, adapter, self._params["scan_seconds"])
        manufacturer = seen.get("manufacturer_data") or {}
        advert = light.parse_govee_advertisement(manufacturer) if manufacturer else None
        if advert is None:
            raise SystemExit("Advertisement did not include a color pact.")
        if advert.supports_encryption:
            raise SystemExit("Advertisement says the link is encrypted. The page will not send plaintext frames.")
        pact = (advert.pact_type, advert.pact_code)
        if pact not in light.QUALIFIED_PACTS:
            raise SystemExit(f"Pact {pact[0]}/{pact[1]} is outside the codec's color profiles.")
        base = self.profile or light.profile_for(*pact)
        client = BleakClient(
            address,
            self._on_link_down,
            timeout=self._params["timeout"],
            bluez={"adapter": adapter} if adapter else {},
        )
        self.client = client
        await client.connect()
        await client.start_notify(light.READ_UUID, self._on_notify)
        if identify or self.profile is None:
            queries: list[tuple[str, bytes, str]] = [
                ("firmware", light.build_firmware_query(light.MODEL, profile=base), "fw_version"),
                ("hardware", light.build_hardware_query(light.MODEL, profile=base), "hw_version"),
                ("power", _status_query("power", base), "power"),
                ("brightness", _status_query("brightness", base), "brightness"),
            ]
            if base.configured_string_counts:
                queries.insert(2, ("strings", light.build_configured_strings_query(base), "configured_strings"))
            identity: dict[str, Any] = {}
            for label, packet, domain in queries:
                identity[label] = await light._query(client, self._queue, packet, base, domain)
            count = (identity.get("strings") or {}).get("count")
            used = self._params["strings"] if self._params["strings"] is not None else count or 1
            self.profile = light.sized_profile(base, used)
            self.state.update(
                name=seen.get("name") or "H7020",
                firmware=(identity.get("firmware") or {}).get("text"),
                hardware=(identity.get("hardware") or {}).get("text"),
                strings=used,
                bulbs=self.profile.segment_count,
                on=(identity.get("power") or {}).get("is_on"),
                brightness=(identity.get("brightness") or {}).get("brightness_pct"),
            )
        self.state["connected"] = True
        self.state["error"] = None
        print(
            f"Connected {self.state['name']} pact {pact[0]}/{pact[1]} "
            f"fw {self.state['firmware']} hw {self.state['hardware']} strings {self.state['strings']}",
            file=sys.stderr,
        )

    async def close(self) -> None:
        self._closing = True
        self._wake.set()
        if self._writer is not None:
            self._writer.cancel()
            try:
                await self._writer
            except asyncio.CancelledError:
                pass
        client = self.client
        if client is not None and client.is_connected:
            await client.stop_notify(light.READ_UUID)
            await client.disconnect()
        self.state["connected"] = False


def _status_query(domain: str, profile: Any) -> bytes:
    """Build a power or brightness read using the codec's query helpers."""
    if domain == "power":
        from custom_components.ha_govee_led_ble.generated_protocol_adapter import build_power_query

        return build_power_query(light.MODEL, profile=profile)
    if domain == "brightness":
        from custom_components.ha_govee_led_ble.generated_protocol_adapter import build_brightness_query

        return build_brightness_query(light.MODEL, profile=profile)
    raise ValueError(domain)


class Channel:
    """Mark each request with the listener it arrived on."""

    def __init__(self, app: Any, channel: str) -> None:
        self.app = app
        self.channel = channel

    async def __call__(self, scope: dict[str, Any], receive: Any, send: Any) -> None:
        if scope["type"] == "http":
            scope = dict(scope)
            scope["tallow_channel"] = self.channel
        await self.app(scope, receive, send)


def create_app(
    password: str,
    session: Any,
    lifespan: Any = None,
    catalog: library.Library | None = None,
    stage: playback.Show | None = None,
    limiter: library.WriteLimiter | None = None,
    agents_path: Path = AGENTS_PATH,
) -> FastAPI:
    app = FastAPI(title="H7020", docs_url=None, redoc_url=None, lifespan=lifespan)
    catalog = catalog or library.Library(ROOT / "data" / "panel")
    stage = stage or playback.Show(session, catalog)
    limiter = limiter or library.WriteLimiter()
    app.state.password = password
    app.state.session = session
    app.state.catalog = catalog
    app.state.limiter = limiter
    browser = session_token(password)
    agents_note: dict[str, str | None] = {"text": None}

    def current_agents() -> list[dict[str, Any]]:
        try:
            return agents.load_agents(agents_path)
        except (OSError, ValueError, PermissionError, json.JSONDecodeError) as exc:
            note = f"{type(exc).__name__}: {exc}"
            if note != agents_note["text"]:
                print(f"Agent tokens unreadable ({note}).", file=sys.stderr)
                agents_note["text"] = note
            return []

    def lan_page(request: Request) -> bool:
        """The HTML page is the house-LAN cookie. A bearer or the agent socket stays out."""
        return (
            agents.authenticate(
                request.scope.get("tallow_channel", "lan"),
                request.headers.get("authorization"),
                request.cookies.get(COOKIE),
                browser,
                [],
            )
            is not None
        )

    def require(request: Request, scope: str) -> str:
        result = agents.authenticate(
            request.scope.get("tallow_channel", "lan"),
            request.headers.get("authorization"),
            request.cookies.get(COOKIE),
            browser,
            current_agents(),
        )
        if result is None:
            raise HTTPException(status_code=401, detail="login")
        actor, scopes = result
        if scope not in scopes:
            raise HTTPException(status_code=403, detail="missing scope")
        if request.scope.get("tallow_channel") == "agent":
            catalog.audit(actor, request.method, request.url.path)
        return actor

    def require_link() -> None:
        if not session.linked():
            raise HTTPException(status_code=503, detail="disconnected")

    def bulb_count() -> int:
        profile = session.profile
        if profile is not None and profile.segment_count:
            return int(profile.segment_count)
        return int(session.state.get("bulbs") or 15)

    async def read_body(request: Request) -> dict[str, Any]:
        try:
            body = await request.json()
        except json.JSONDecodeError as exc:
            raise HTTPException(status_code=400, detail="body must be JSON") from exc
        if not isinstance(body, dict):
            raise HTTPException(status_code=400, detail="body must be an object")
        return body

    def only_fields(body: dict[str, Any], allowed: set[str]) -> None:
        extra = set(body) - allowed
        if extra:
            raise HTTPException(status_code=400, detail="unknown fields: " + ", ".join(sorted(extra)))

    def control(request: Request) -> str:
        actor = require(request, "control")
        if not limiter.allow():
            raise HTTPException(status_code=429, detail="slow down")
        require_link()
        return actor

    async def acted(coro: Any) -> JSONResponse:
        try:
            snap = await coro
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return JSONResponse({"state": snap})

    def manifest_body() -> dict[str, Any]:
        return {
            "device": "h7020",
            "bulbs": bulb_count(),
            "absences": ["diy", "music", "kelvin", "raw_frames"],
            "scopes": list(agents.SCOPES),
            "scenes": light.list_scenes(),
            "pattern": {
                "kinds": ["scene", "paint", "sequence"],
                "hold_min_seconds": 1,
                "steps_max": library.STEPS_MAX,
                "example_sequence": {
                    "id": "evening-walk",
                    "name": "Evening walk",
                    "kind": "sequence",
                    "repeat": True,
                    "steps": [{"hold": 4, "paint": [{"bulbs": [1, 2, 3], "rgb": [255, 80, 0]}]}],
                },
                "example_scene": {
                    "id": "sunset",
                    "name": "Sunset",
                    "kind": "scene",
                    "scene": {"name": "Sunset Glow", "speed": 1},
                },
            },
            "schedule": {
                "days": list(library.WEEKDAYS),
                "time": "HH:MM",
                "actions": ["pattern", "scene", "power_off"],
            },
            "routes": [
                "GET /api/state",
                "GET /api/scenes",
                "GET /api/capabilities",
                "GET /api/manifest",
                "POST /api/power",
                "POST /api/brightness",
                "POST /api/color",
                "POST /api/scene",
                "POST /api/paint",
                "GET /api/patterns",
                "POST /api/patterns",
                "GET /api/patterns/{id}",
                "PUT /api/patterns/{id}",
                "DELETE /api/patterns/{id}",
                "POST /api/patterns/{id}/apply",
                "GET /api/schedules",
                "POST /api/schedules",
                "GET /api/schedules/{id}",
                "PUT /api/schedules/{id}",
                "DELETE /api/schedules/{id}",
            ],
        }

    @app.get("/", response_class=HTMLResponse)
    async def index(request: Request) -> HTMLResponse:
        if not lan_page(request):
            return HTMLResponse(LOGIN_HTML.format(error=""), status_code=200)
        return HTMLResponse(CONTROLS_HTML)

    @app.get("/login", response_class=HTMLResponse)
    async def login_form(request: Request) -> HTMLResponse:
        if lan_page(request):
            return RedirectResponse("/", status_code=303)
        error = "Wrong password." if request.query_params.get("error") else ""
        return HTMLResponse(LOGIN_HTML.format(error=error))

    @app.post("/login")
    async def login(request: Request, password_form: str = Form(..., alias="password")) -> RedirectResponse:
        if request.scope.get("tallow_channel") == "agent":
            raise HTTPException(status_code=404, detail="not found")
        if len(password_form) != len(password) or not hmac.compare_digest(password_form, password):
            return RedirectResponse("/login?error=1", status_code=303)
        response = RedirectResponse("/", status_code=303)
        response.set_cookie(
            COOKIE,
            browser,
            httponly=True,
            samesite="strict",
            path="/",
            max_age=60 * 60 * 24 * 30,
        )
        return response

    @app.get("/api/state")
    async def state(request: Request) -> JSONResponse:
        require(request, "read")
        return JSONResponse(session.snapshot())

    @app.get("/api/scenes")
    async def scenes(request: Request) -> JSONResponse:
        require(request, "read")
        return JSONResponse({"scenes": light.list_scenes()})

    @app.get("/api/capabilities")
    async def capabilities(request: Request) -> JSONResponse:
        require(request, "read")
        return JSONResponse(
            {
                "device": "h7020",
                "bulbs": bulb_count(),
                "diy": False,
                "music": False,
                "kelvin": False,
                "raw_frames": False,
            }
        )

    @app.get("/api/manifest")
    async def manifest(request: Request) -> JSONResponse:
        if request.scope.get("tallow_channel") != "agent":
            raise HTTPException(status_code=404, detail="not found")
        require(request, "read")
        return JSONResponse(manifest_body())

    @app.post("/api/power")
    async def power(request: Request) -> JSONResponse:
        control(request)
        body = await read_body(request)
        only_fields(body, {"on"})
        if not isinstance(body.get("on"), bool):
            raise HTTPException(status_code=400, detail="on must be a boolean")
        return await acted(stage.apply_power(body["on"]))

    @app.post("/api/brightness")
    async def brightness(request: Request) -> JSONResponse:
        control(request)
        body = await read_body(request)
        only_fields(body, {"level"})
        level = body.get("level")
        if isinstance(level, bool) or not isinstance(level, int) or not 1 <= level <= 100:
            raise HTTPException(status_code=400, detail="level must be an integer 1..100")
        return await acted(stage.apply_brightness(level))

    @app.post("/api/color")
    async def color(request: Request) -> JSONResponse:
        control(request)
        body = await read_body(request)
        only_fields(body, {"r", "g", "b"})
        try:
            rgb = {channel: body[channel] for channel in ("r", "g", "b")}
        except KeyError as exc:
            raise HTTPException(status_code=400, detail="r, g, b required") from exc
        if any(isinstance(part, bool) or not isinstance(part, int) or not 0 <= part <= 255 for part in rgb.values()):
            raise HTTPException(status_code=400, detail="r, g, b must be integers 0..255")
        return await acted(stage.apply_color(rgb))

    @app.post("/api/scene")
    async def scene(request: Request) -> JSONResponse:
        control(request)
        body = await read_body(request)
        only_fields(body, {"name", "speed"})
        return await acted(stage.apply_scene(body.get("name"), body.get("speed")))

    @app.post("/api/paint")
    async def paint(request: Request) -> JSONResponse:
        control(request)
        body = await read_body(request)
        only_fields(body, {"groups"})
        try:
            groups = library.validate_paint(body.get("groups"), bulb_count())
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return await acted(stage.apply_paint(groups))

    @app.get("/api/patterns")
    async def patterns(request: Request) -> JSONResponse:
        require(request, "read")
        return JSONResponse({"patterns": catalog.list_patterns()})

    @app.post("/api/patterns")
    async def create_pattern(request: Request) -> JSONResponse:
        require(request, "library")
        return JSONResponse(save_pattern(await read_body(request), create=True))

    @app.get("/api/patterns/{pattern_id}")
    async def pattern_one(request: Request, pattern_id: str) -> JSONResponse:
        require(request, "read")
        found = catalog.get_pattern(pattern_id)
        if found is None:
            raise HTTPException(status_code=404, detail="not found")
        return JSONResponse(found)

    @app.put("/api/patterns/{pattern_id}")
    async def update_pattern(request: Request, pattern_id: str) -> JSONResponse:
        require(request, "library")
        body = await read_body(request)
        body["id"] = pattern_id
        return JSONResponse(save_pattern(body, create=False))

    @app.delete("/api/patterns/{pattern_id}")
    async def delete_pattern(request: Request, pattern_id: str) -> JSONResponse:
        require(request, "library")
        try:
            catalog.delete_pattern(pattern_id)
        except FileNotFoundError as exc:
            raise HTTPException(status_code=404, detail="not found") from exc
        return JSONResponse({"ok": True})

    @app.post("/api/patterns/{pattern_id}/apply")
    async def apply_saved(request: Request, pattern_id: str) -> JSONResponse:
        control(request)
        found = catalog.get_pattern(pattern_id)
        if found is None:
            raise HTTPException(status_code=404, detail="not found")
        return await acted(stage.apply_pattern(found))

    @app.get("/api/schedules")
    async def schedules(request: Request) -> JSONResponse:
        require(request, "read")
        return JSONResponse({"schedules": catalog.list_schedules()})

    @app.post("/api/schedules")
    async def create_schedule(request: Request) -> JSONResponse:
        require(request, "schedule")
        return JSONResponse(save_schedule(await read_body(request), create=True))

    @app.get("/api/schedules/{schedule_id}")
    async def schedule_one(request: Request, schedule_id: str) -> JSONResponse:
        require(request, "read")
        found = catalog.get_schedule(schedule_id)
        if found is None:
            raise HTTPException(status_code=404, detail="not found")
        return JSONResponse(found)

    @app.put("/api/schedules/{schedule_id}")
    async def update_schedule(request: Request, schedule_id: str) -> JSONResponse:
        require(request, "schedule")
        body = await read_body(request)
        body["id"] = schedule_id
        return JSONResponse(save_schedule(body, create=False))

    @app.delete("/api/schedules/{schedule_id}")
    async def delete_schedule(request: Request, schedule_id: str) -> JSONResponse:
        require(request, "schedule")
        try:
            catalog.delete_schedule(schedule_id)
        except FileNotFoundError as exc:
            raise HTTPException(status_code=404, detail="not found") from exc
        return JSONResponse({"ok": True})

    def save_pattern(body: dict[str, Any], *, create: bool) -> dict[str, Any]:
        try:
            cleaned = library.validate_pattern(body, bulb_count(), light.scene_info)
            return catalog.save_pattern(cleaned, create=create)
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        except FileExistsError as exc:
            raise HTTPException(status_code=409, detail="already saved") from exc
        except FileNotFoundError as exc:
            raise HTTPException(status_code=404, detail="not found") from exc

    def save_schedule(body: dict[str, Any], *, create: bool) -> dict[str, Any]:
        try:
            cleaned = library.validate_schedule(
                body,
                {item["id"]: item for item in catalog.list_patterns()},
                light.scene_info,
            )
            return catalog.save_schedule(cleaned, create=create)
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        except FileExistsError as exc:
            raise HTTPException(status_code=409, detail="already saved") from exc
        except FileNotFoundError as exc:
            raise HTTPException(status_code=404, detail="not found") from exc

    return app


def _connect_args(args: argparse.Namespace) -> dict[str, Any]:
    adapter = args.adapter or light.seed_adapter()
    return {
        "address": args.address,
        "adapter": adapter,
        "scan_seconds": args.scan_seconds,
        "timeout": args.timeout,
        "strings": args.strings,
    }


def build_runtime(password: str, connect: dict[str, Any], *, preview: bool = False) -> FastAPI:
    session = LightSession()
    root = ROOT / "data" / ("panel-preview" if preview else "panel")
    catalog = library.Library(root)
    stage = playback.Show(session, catalog)

    @asynccontextmanager
    async def lifespan(_app: FastAPI):
        try:
            if preview:
                await session.open_preview()
            else:
                await session.open(**connect)
        except SystemExit:
            raise
        except Exception as exc:
            print(
                f"Connect failed ({type(exc).__name__}: {exc}). Close Govee Home so this host can be the only central.",
                file=sys.stderr,
            )
            raise SystemExit(1) from exc
        stage.start()
        try:
            yield
        finally:
            await stage.stop()
            await session.close()

    return create_app(password, session, lifespan, catalog, stage)


def serve(app: FastAPI, host: str, port: int, sock_path: Path) -> None:
    """House-LAN HTTP plus a mode-0600 unix socket. Bearers are socket-only."""
    import uvicorn

    sock_path.parent.mkdir(parents=True, exist_ok=True)
    sock_path.parent.chmod(0o700)
    if sock_path.exists():
        sock_path.unlink()
    lan = uvicorn.Server(uvicorn.Config(Channel(app, "lan"), host=host, port=port, log_level="info"))
    agent = uvicorn.Server(
        uvicorn.Config(Channel(app, "agent"), uds=str(sock_path), lifespan="off", log_level="info")
    )

    def handle_exit(sig: int, frame: Any) -> None:
        for server in (lan, agent):
            if server.should_exit and sig == signal.SIGINT:
                server.force_exit = True
            else:
                server.should_exit = True

    lan.handle_exit = handle_exit  # type: ignore[method-assign]

    @contextmanager
    def no_signals():
        yield

    agent.capture_signals = no_signals  # type: ignore[method-assign]

    async def own_socket() -> None:
        # Uvicorn chmods a new unix socket to 0666 immediately after bind.
        for _ in range(100):
            if sock_path.exists():
                break
            await asyncio.sleep(0.05)
        else:
            print(f"Agent socket {sock_path} did not appear.", file=sys.stderr)
            return
        for _ in range(20):
            await asyncio.sleep(0.05)
            if sock_path.exists():
                sock_path.chmod(0o600)

    async def run() -> None:
        await asyncio.gather(lan.serve(), agent.serve(), own_socket())

    asyncio.run(run())


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", help="House LAN IPv4 address. Default: default-route source outside 10.87.0.0/24")
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--address", default=light.DEFAULT_ADDRESS, help="BLE address")
    parser.add_argument("--adapter", help="BlueZ adapter. Default: the TP-Link UB500")
    parser.add_argument("--scan-seconds", type=float, default=12.0)
    parser.add_argument("--timeout", type=float, default=20.0)
    parser.add_argument("--strings", type=int, choices=(1, 2), help="15-bulb strings. Default: device report, else 1")
    parser.add_argument("--preview", action="store_true", help="Bind 127.0.0.1 and skip BLE")
    args = parser.parse_args()
    password = load_password(PASSWORD_PATH)
    if args.preview:
        if args.host not in (None, "127.0.0.1"):
            raise SystemExit("Preview binds only 127.0.0.1.")
        host = "127.0.0.1"
    else:
        host = choose_host(args.host, default_route_ipv4(), local_ipv4())
    data_name = "panel-preview" if args.preview else "panel"
    sock_path = ROOT / "data" / data_name / "agent.sock"
    print(f"Panel http://{host}:{args.port}/", file=sys.stderr)
    print(f"Agent socket {sock_path}", file=sys.stderr)
    if args.preview:
        print("Preview only. BLE is not connected.", file=sys.stderr)
    serve(build_runtime(password, _connect_args(args), preview=args.preview), host, args.port, sock_path)


if __name__ == "__main__":
    main()
