#!/usr/bin/env python3
"""HTTP contract for the LAN panel. The standard library only.

    .venv/bin/python tools/tests/panel_contract.py

It starts its own preview on a free 127.0.0.1 port with a throwaway password,
throwaway agent tokens, and a temporary data directory holding one seeded cam, so
it does not read secrets/, data/panel/, or data/panel-preview/. The seeded cam is
10.87.0.254, outside DHCP and the node addresses, so no device holds it, and the
script stops if the panel lists any other cam. It runs two panels: the second
spends the rate limit. It stops both and removes the directory.

The default panel is host/target/release/tallowd panel --preview (build it with
tools/mesh.sh daemon build). Another build takes the same flags:

    .venv/bin/python tools/tests/panel_contract.py --panel-command 'path/to/tallowd panel --preview'

It also checks /devices and GET /api/mesh/map, with the map's stale states, read
from a mesh-map.json the script seeds.

Skipped, because preview cannot reach them:

    needs-cam    GET /cams/{id}/stream live body, its headers, 429 and 502
    needs-light  503 disconnected on the light routes, and an idle reconnect
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import html
import http.client
import json
import os
import shlex
import socket
import stat
import subprocess
import sys
import tempfile
import time
import urllib.parse
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_PANEL = f"{ROOT / 'host' / 'target' / 'release' / 'tallowd'} panel --preview"

PHONE_HOME = (
    "This box still phones home if it remembers the house Wi-Fi. "
    "Forget that SSID and leave it off tallow-iot. BLE works with no Wi-Fi."
)
PREVIEW_STATE = {
    "device": "h7020",
    "name": "H7020 preview",
    "on": True,
    "brightness": 100,
    "color": {"r": 255, "g": 0, "b": 0},
    "firmware": "preview",
    "hardware": "preview",
    "strings": 1,
    "bulbs": 15,
    "connected": True,
    "error": None,
    "note": PHONE_HOME,
    "mode": None,
    "scene": None,
    "paint": None,
    "pattern_id": None,
    "playing": False,
    "preview": True,
}
CAPABILITIES = {
    "device": "h7020",
    "bulbs": 15,
    "diy": False,
    "music": False,
    "kelvin": False,
    "raw_frames": False,
}
SCENE_CATEGORIES = ["Emotion", "Festival", "Life", "Natural"]
PATTERN_ID = "contract-scene"
SCHEDULE_ID = "contract-slot"
SCENE = {"name": "Sunset Glow", "speed": 2}
SEQUENCE_ID = "contract-walk"
PAINT_ID = "contract-paint"
PASSWORD = "contract-password"
READ_TOKEN = "contract-read-token"
CONTROL_TOKEN = "contract-control-token"
CAMS_TITLE = 'Cams & <co> "here"'
CAM = {"id": "bench-cam", "name": "Bench cam", "address": "10.87.0.254"}
IOT_NET = "10.87.0.0/24"
RATE_LIMIT = 30
AUTH_ROUTES = (
    ("GET", "/api/cams"),
    ("GET", "/api/cams/status"),
    ("GET", "/cams/not-a-cam/stream"),
    ("GET", "/api/settings/cameras"),
    ("PUT", "/api/settings/cameras"),
    ("GET", "/api/state"),
    ("GET", "/api/scenes"),
    ("GET", "/api/capabilities"),
    ("POST", "/api/power"),
    ("POST", "/api/brightness"),
    ("POST", "/api/color"),
    ("POST", "/api/scene"),
    ("POST", "/api/paint"),
    ("GET", "/api/patterns"),
    ("POST", "/api/patterns"),
    ("GET", "/api/patterns/not-saved"),
    ("PUT", "/api/patterns/not-saved"),
    ("DELETE", "/api/patterns/not-saved"),
    ("POST", "/api/patterns/not-saved/apply"),
    ("GET", "/api/schedules"),
    ("POST", "/api/schedules"),
    ("GET", "/api/schedules/not-saved"),
    ("PUT", "/api/schedules/not-saved"),
    ("DELETE", "/api/schedules/not-saved"),
)


def fail(label: str, detail: object) -> None:
    raise SystemExit(f"FAIL {label}\n  {detail}")


def check(label: str, ok: bool, detail: object = "") -> None:
    if not ok:
        fail(label, detail)


def check_eq(label: str, actual: object, expected: object) -> None:
    if actual != expected:
        fail(label, f"actual {actual!r}\n  expected {expected!r}")


def session_token(password: str) -> str:
    return hmac.new(password.encode(), b"tallow-h7020-panel", hashlib.sha256).hexdigest()


def cookie_parts(header: str) -> tuple[str, str, dict[str, str]]:
    pieces = [part.strip() for part in header.split(";")]
    name, _, value = pieces[0].partition("=")
    attrs: dict[str, str] = {}
    for part in pieces[1:]:
        key, _, attr = part.partition("=")
        attrs[key.lower()] = attr
    return name, value, attrs


class Client:
    """One HTTP connection per call. Redirects are not followed."""

    def __init__(self, base_url: str) -> None:
        parsed = urllib.parse.urlparse(base_url)
        if parsed.scheme != "http" or parsed.hostname != "127.0.0.1" or parsed.path not in ("", "/"):
            raise SystemExit("The contract only runs against http://127.0.0.1:port")
        self.host = parsed.hostname
        self.port = parsed.port or 80
        self.cookie: str | None = None

    def call(
        self,
        method: str,
        path: str,
        body: bytes | None = None,
        content_type: str | None = None,
        *,
        auth: bool = True,
        extra: dict[str, str] | None = None,
    ) -> tuple[int, bytes, dict[str, str]]:
        headers: dict[str, str] = {}
        if content_type is not None:
            headers["Content-Type"] = content_type
        if auth and self.cookie is not None:
            headers["Cookie"] = self.cookie
        if extra:
            headers.update(extra)
        conn = http.client.HTTPConnection(self.host, self.port, timeout=10)
        try:
            conn.request(method, path, body=body, headers=headers)
            resp = conn.getresponse()
            payload = resp.read()
            found: dict[str, str] = {}
            for key, value in resp.getheaders():
                key = key.lower()
                if key in found:
                    fail(f"{method} {path}", f"repeated header {key}")
                found[key] = value
            return resp.status, payload, found
        except OSError as exc:
            fail(f"{method} {path}", exc)
        finally:
            conn.close()
        raise AssertionError("unreachable")


def json_of(label: str, status: int, payload: bytes, headers: dict[str, str], expected: int) -> object:
    check_eq(f"{label} status", status, expected)
    check_eq(f"{label} content-type", headers.get("content-type"), "application/json")
    try:
        return json.loads(payload)
    except json.JSONDecodeError as exc:
        fail(label, exc)


def detail(label: str, status: int, payload: bytes, headers: dict[str, str], expected: int) -> str:
    body = json_of(label, status, payload, headers, expected)
    check(label, isinstance(body, dict), body)
    text = body.get("detail") if isinstance(body, dict) else None
    check(label, isinstance(text, str), body)
    return str(text)


def redirect(label: str, status: int, payload: bytes, headers: dict[str, str], location: str) -> None:
    check_eq(f"{label} status", status, 303)
    check_eq(f"{label} location", headers.get("location"), location)
    check_eq(f"{label} body", payload, b"")


def text_has(label: str, payload: bytes, marker: str) -> None:
    check(label, marker in payload.decode("utf-8"), f"missing {marker!r}")


def text_lacks(label: str, payload: bytes, marker: str) -> None:
    check(label, marker not in payload.decode("utf-8"), f"unexpected {marker!r}")


def unix_call(
    sock_path: str,
    method: str,
    path: str,
    body: bytes = b"",
    headers: dict[str, str] | None = None,
) -> tuple[int, bytes]:
    status, payload, _ = unix_full(sock_path, method, path, body, headers)
    return status, payload


def unix_full(
    sock_path: str,
    method: str,
    path: str,
    body: bytes = b"",
    headers: dict[str, str] | None = None,
) -> tuple[int, bytes, dict[str, str]]:
    raw_headers = {"Host": "localhost", "Connection": "close"}
    if headers:
        raw_headers.update(headers)
    if body:
        raw_headers["Content-Length"] = str(len(body))
    request = f"{method} {path} HTTP/1.1\r\n".encode()
    request += "".join(f"{key}: {value}\r\n" for key, value in raw_headers.items()).encode()
    request += b"\r\n" + body
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(5)
    try:
        sock.connect(sock_path)
        sock.sendall(request)
        data = b""
        while True:
            chunk = sock.recv(65536)
            if not chunk:
                break
            data += chunk
    except OSError as exc:
        fail(f"socket {method} {path}", exc)
    finally:
        sock.close()
    head, _, payload = data.partition(b"\r\n\r\n")
    lines = head.decode("iso-8859-1").split("\r\n")
    check(f"socket {method} {path}", len(lines) >= 1 and len(lines[0].split(" ")) >= 2, head[:120])
    status = int(lines[0].split(" ", 2)[1])
    header_map: dict[str, str] = {}
    for line in lines[1:]:
        if ":" in line:
            key, _, value = line.partition(":")
            header_map[key.lower()] = value.strip()
    if "content-length" in header_map:
        payload = payload[: int(header_map["content-length"])]
    return status, payload, header_map


def expect_login_cookie(header: str, password: str) -> str:
    name, value, attrs = cookie_parts(header)
    check_eq("login cookie name", name, "tallow_panel")
    check_eq("login cookie value", value, session_token(password))
    check_eq("login cookie httponly", "httponly" in attrs, True)
    check_eq("login cookie max-age", attrs.get("max-age"), "2592000")
    check_eq("login cookie path", attrs.get("path"), "/")
    check_eq("login cookie samesite", attrs.get("samesite", "").lower(), "strict")
    return f"{name}={value}"


def expect_cleared_cookie(header: str) -> None:
    name, value, attrs = cookie_parts(header)
    check_eq("logout cookie name", name, "tallow_panel")
    # Starlette quotes an empty cookie value, so the header reads tallow_panel="".
    check_eq("logout cookie value", value, '""')
    check_eq("logout cookie max-age", attrs.get("max-age"), "0")
    check_eq("logout cookie path", attrs.get("path"), "/")
    check_eq("logout cookie samesite", attrs.get("samesite", "").lower(), "lax")


def unauthenticated(client: Client) -> None:
    status, payload, headers = client.call("GET", "/", auth=False)
    check_eq("GET / status", status, 200)
    check_eq("GET / content-type", headers.get("content-type"), "text/html; charset=utf-8")
    text_has("GET /", payload, 'action="/login"')
    text_lacks("GET /", payload, "Everything here stays in the house.")

    for path in ("/lights", "/cams", "/settings"):
        status, payload, headers = client.call("GET", path, auth=False)
        redirect(f"GET {path}", status, payload, headers, "/login")

    status, payload, headers = client.call("GET", "/login", auth=False)
    check_eq("GET /login status", status, 200)
    check_eq("GET /login content-type", headers.get("content-type"), "text/html; charset=utf-8")
    text_has("GET /login", payload, 'name="password"')
    text_lacks("GET /login", payload, "Wrong password.")

    status, payload, headers = client.call("GET", "/panel.css", auth=False)
    check_eq("GET /panel.css status", status, 200)
    check_eq("GET /panel.css content-type", headers.get("content-type"), "text/css; charset=utf-8")
    check_eq("GET /panel.css cache-control", headers.get("cache-control"), "no-cache")
    text_has("GET /panel.css", payload, "--bg: #14120f;")

    for method, path in AUTH_ROUTES:
        status, payload, headers = client.call(method, path, auth=False)
        check_eq(f"{method} {path} without cookie", detail(f"{method} {path}", status, payload, headers, 401), "login")

    # The manifest hides on the house LAN. It is 404 before the login check.
    status, payload, headers = client.call("GET", "/api/manifest", auth=False)
    check_eq("GET /api/manifest without cookie", detail("GET /api/manifest", status, payload, headers, 404), "not found")


def forged_cookies(client: Client, token: str) -> None:
    flipped = token[:-1] + ("0" if token[-1] != "0" else "1")
    forged = {
        "one character flipped": flipped,
        "cut short": token[:-1],
        "with a suffix": token + "0",
        "empty": "",
        "not ASCII": "\u00e9" * len(token),
    }
    for label, value in forged.items():
        cookie = {"Cookie": f"tallow_panel={value}"}
        for method, path in AUTH_ROUTES:
            status, payload, headers = client.call(method, path, auth=False, extra=cookie)
            check_eq(f"{method} {path} cookie {label}", detail(f"{method} {path}", status, payload, headers, 401), "login")
        for path in ("/lights", "/cams", "/settings"):
            status, payload, headers = client.call("GET", path, auth=False, extra=cookie)
            redirect(f"GET {path} cookie {label}", status, payload, headers, "/login")


def login(client: Client, password: str) -> None:
    for wrong_password in (
        "not-the-password",
        password[:-1],
        password + "x",
        password[:-1] + ("x" if password[-1] != "x" else "y"),
        "\u00e9" * len(password),
    ):
        wrong = urllib.parse.urlencode({"password": wrong_password}).encode()
        status, payload, headers = client.call(
            "POST",
            "/login",
            wrong,
            "application/x-www-form-urlencoded",
            auth=False,
        )
        redirect(f"POST /login wrong {wrong_password!r}", status, payload, headers, "/login?error=1")
        check("POST /login wrong sets no cookie", "set-cookie" not in headers, headers.get("set-cookie"))

    status, payload, headers = client.call("GET", "/login?error=1", auth=False)
    check_eq("GET /login?error=1 status", status, 200)
    text_has("GET /login?error=1", payload, "Wrong password.")

    status, payload, headers = client.call("POST", "/login", b"", "application/x-www-form-urlencoded", auth=False)
    body = json_of("POST /login missing", status, payload, headers, 422)
    check("POST /login missing detail", isinstance(body, dict) and isinstance(body.get("detail"), list), body)
    locations = []
    if isinstance(body, dict) and isinstance(body.get("detail"), list):
        for item in body["detail"]:
            if isinstance(item, dict):
                locations.extend(str(part) for part in item.get("loc", []))
    check_eq("POST /login missing field", "password" in locations, True)

    form = urllib.parse.urlencode({"password": password}).encode()
    status, payload, headers = client.call(
        "POST",
        "/login",
        form,
        "application/x-www-form-urlencoded",
        auth=False,
    )
    redirect("POST /login", status, payload, headers, "/")
    client.cookie = expect_login_cookie(headers.get("set-cookie", ""), password)

    status, payload, headers = client.call("GET", "/api/state", extra={"Authorization": "Bearer nope"})
    check_eq("cookie plus bearer", detail("GET /api/state bearer", status, payload, headers, 401), "login")
    bearer = {"Authorization": f"Bearer {CONTROL_TOKEN}"}
    status, payload, headers = client.call("POST", "/api/power", b'{"on":true}', "application/json", auth=False, extra=bearer)
    check_eq("a bearer alone on the LAN", detail("POST /api/power bearer", status, payload, headers, 401), "login")


def pages(client: Client, title: str) -> None:
    shown = html.escape(title)
    status, payload, headers = client.call("GET", "/")
    check_eq("GET / authed status", status, 200)
    check_eq("GET / authed content-type", headers.get("content-type"), "text/html; charset=utf-8")
    text_has("GET / authed", payload, "Everything here stays in the house.")
    text_has("GET / authed", payload, f"<strong>{shown}</strong>")
    text_lacks("GET / authed", payload, 'name="password"')

    status, payload, headers = client.call("GET", "/lights")
    check_eq("GET /lights status", status, 200)
    check_eq("GET /lights content-type", headers.get("content-type"), "text/html; charset=utf-8")
    text_has("GET /lights", payload, "<title>H7020</title>")
    text_has("GET /lights", payload, "tallow-iot")

    status, payload, headers = client.call("GET", "/cams")
    check_eq("GET /cams status", status, 200)
    check_eq("GET /cams content-type", headers.get("content-type"), "text/html; charset=utf-8")
    text_has("GET /cams", payload, f"<title>{html.escape(title)}</title>")

    status, payload, headers = client.call("GET", "/settings")
    check_eq("GET /settings status", status, 200)
    check_eq("GET /settings content-type", headers.get("content-type"), "text/html; charset=utf-8")
    text_has("GET /settings", payload, "<title>Settings</title>")
    text_has("GET /settings", payload, IOT_NET)
    for path in ("/", "/lights", "/cams", "/settings"):
        status, payload, headers = client.call("GET", path)
        check_eq(f"GET {path} status", status, 200)
        for marker in ("__CAMS_TITLE__", "__IOT_NET__", "__PHONE_HOME__"):
            text_lacks(f"GET {path}", payload, marker)
        text_lacks(f"GET {path}", payload, CAMS_TITLE)

    status, payload, headers = client.call("GET", "/login")
    redirect("GET /login authed", status, payload, headers, "/")


def light_and_library(client: Client) -> None:
    status, payload, headers = client.call("GET", "/api/state")
    check_eq("GET /api/state", json_of("GET /api/state", status, payload, headers, 200), PREVIEW_STATE)

    status, payload, headers = client.call("GET", "/api/scenes")
    scenes_body = json_of("GET /api/scenes", status, payload, headers, 200)
    check("GET /api/scenes shape", isinstance(scenes_body, dict) and isinstance(scenes_body.get("scenes"), list), scenes_body)
    scenes = scenes_body["scenes"] if isinstance(scenes_body, dict) else []
    check_eq("GET /api/scenes count", len(scenes), 51)
    check_eq(
        "GET /api/scenes categories",
        sorted({item.get("category") for item in scenes if isinstance(item, dict)}),
        SCENE_CATEGORIES,
    )
    for item in scenes:
        check(
            "GET /api/scenes item",
            isinstance(item, dict) and set(item) == {"name", "category", "speeds", "default_speed"},
            item,
        )
    sunset = next((item for item in scenes if isinstance(item, dict) and item.get("name") == "Sunset Glow"), None)
    check_eq("GET /api/scenes Sunset Glow", sunset, {"name": "Sunset Glow", "category": "Natural", "speeds": 3, "default_speed": 2})

    status, payload, headers = client.call("GET", "/api/capabilities")
    check_eq("GET /api/capabilities", json_of("GET /api/capabilities", status, payload, headers, 200), CAPABILITIES)

    status, payload, headers = client.call("GET", "/api/manifest")
    check_eq("GET /api/manifest on the LAN", detail("GET /api/manifest", status, payload, headers, 404), "not found")

    status, payload, headers = client.call("GET", "/api/cams")
    check_eq(
        "GET /api/cams",
        json_of("GET /api/cams", status, payload, headers, 200),
        {"cams": [{"id": CAM["id"], "name": CAM["name"]}], "error": None},
    )

    status, payload, headers = client.call("GET", "/api/cams/status")
    check_eq(
        "GET /api/cams/status",
        json_of("GET /api/cams/status", status, payload, headers, 200),
        {CAM["id"]: {"online": False, "viewers": 0}},
    )

    status, payload, headers = client.call("GET", "/cams/not-a-cam/stream")
    check_eq("GET /cams/not-a-cam/stream", detail("GET stream", status, payload, headers, 404), "no such cam")

    status, payload, headers = client.call("GET", "/api/settings/cameras")
    kept = {"title": CAMS_TITLE, "cams": [CAM]}
    check_eq("GET /api/settings/cameras", json_of("GET settings", status, payload, headers, 200), {**kept, "error": None})
    pages(client, CAMS_TITLE)
    status, payload, headers = client.call(
        "PUT",
        "/api/settings/cameras",
        json.dumps(kept).encode(),
        "application/json",
    )
    check_eq("PUT /api/settings/cameras", json_of("PUT settings", status, payload, headers, 200), {**kept, "error": None})

    status, payload, headers = client.call(
        "PUT",
        "/api/settings/cameras",
        json.dumps({"title": "", "cams": []}).encode(),
        "application/json",
    )
    check_eq(
        "PUT settings empty title",
        detail("PUT settings empty title", status, payload, headers, 400),
        "cameras: title needs 1 to 40 characters",
    )
    cam_address_guard(client, kept)

    remove_contract_rows(client)

    status, payload, headers = client.call("GET", "/api/patterns")
    patterns_body = json_of("GET /api/patterns", status, payload, headers, 200)
    check("GET /api/patterns", isinstance(patterns_body, dict) and isinstance(patterns_body.get("patterns"), list), patterns_body)
    pattern_ids = [item.get("id") for item in patterns_body["patterns"]] if isinstance(patterns_body, dict) else []

    status, payload, headers = client.call("GET", "/api/schedules")
    schedules_body = json_of("GET /api/schedules", status, payload, headers, 200)
    check("GET /api/schedules", isinstance(schedules_body, dict) and isinstance(schedules_body.get("schedules"), list), schedules_body)
    schedule_ids = [item.get("id") for item in schedules_body["schedules"]] if isinstance(schedules_body, dict) else []
    try:
        bad_and_good_controls(client)
        library_rows(client)
        playback(client)
        scheduler(client)
    finally:
        remove_contract_rows(client)

    status, payload, headers = client.call("GET", "/api/patterns")
    restored = json_of("GET /api/patterns restored", status, payload, headers, 200)
    check_eq(
        "patterns restored",
        [item.get("id") for item in restored["patterns"]] if isinstance(restored, dict) else restored,
        pattern_ids,
    )
    status, payload, headers = client.call("GET", "/api/schedules")
    restored = json_of("GET /api/schedules restored", status, payload, headers, 200)
    check_eq(
        "schedules restored",
        [item.get("id") for item in restored["schedules"]] if isinstance(restored, dict) else restored,
        schedule_ids,
    )


def refuse_real_cams(client: Client) -> None:
    """Stops before any route dials a cam, if this is not the seeded inventory."""
    status, payload, headers = client.call("GET", "/api/settings/cameras")
    body = json_of("GET /api/settings/cameras", status, payload, headers, 200)
    listed = body.get("cams") if isinstance(body, dict) else None
    if listed != [CAM]:
        fail("GET /api/settings/cameras is the seeded inventory, or this is a real panel: stopping", listed)


def cam_address_guard(client: Client, kept: dict[str, object]) -> None:
    """The panel dials what this saves, so it must only ever be a node on tallow-iot."""
    for address in ("8.8.8.8", "127.0.0.1", "192.168.1.1", "10.87.0.0", "10.87.0.1", "10.87.0.255"):
        doc = {"title": "x", "cams": [{"id": "bad", "name": "Bad", "address": address}]}
        status, payload, headers = client.call("PUT", "/api/settings/cameras", json.dumps(doc).encode(), "application/json")
        check_eq(
            f"PUT settings address {address}",
            detail(f"PUT settings {address}", status, payload, headers, 400),
            f"cameras: bad must be a node on {IOT_NET}",
        )
    doc = {"title": "x", "cams": [{"id": "bad", "name": "Bad", "address": "not-an-address"}]}
    status, payload, headers = client.call("PUT", "/api/settings/cameras", json.dumps(doc).encode(), "application/json")
    check_eq("PUT settings bad address", detail("PUT settings", status, payload, headers, 400), "cameras: bad needs an IPv4 address")
    status, payload, headers = client.call("GET", "/api/settings/cameras")
    check_eq("a refused save changes nothing", json_of("GET settings", status, payload, headers, 200), {**kept, "error": None})


def stray_routes(client: Client) -> None:
    status, payload, headers = client.call("GET", "/no-such-page")
    check_eq("unknown path", detail("GET /no-such-page", status, payload, headers, 404), "Not Found")
    status, payload, headers = client.call("DELETE", "/api/state")
    check_eq("wrong method", detail("DELETE /api/state", status, payload, headers, 405), "Method Not Allowed")
    status, payload, headers = client.call("HEAD", "/panel.css")
    check_eq("HEAD /panel.css", status, 405)
    status, payload, headers = client.call("GET", "/lights/")
    check_eq("GET /lights/ status", status, 307)
    check("GET /lights/ location", headers.get("location", "").endswith("/lights"), headers.get("location"))


def remove_contract_rows(client: Client) -> None:
    for method, path in (
        ("DELETE", f"/api/schedules/{SCHEDULE_ID}"),
        ("DELETE", f"/api/patterns/{PATTERN_ID}"),
        ("DELETE", f"/api/patterns/{SEQUENCE_ID}"),
        ("DELETE", f"/api/patterns/{PAINT_ID}"),
    ):
        status, payload, headers = client.call(method, path)
        if status == 404:
            check_eq(f"{method} {path}", detail(method + " " + path, status, payload, headers, 404), "not found")
        elif status == 200:
            check_eq(f"{method} {path}", json_of(method + " " + path, status, payload, headers, 200), {"ok": True})
        else:
            fail(f"{method} {path}", payload[:200])


def post_json(client: Client, path: str, body: object, expected: int) -> object:
    status, payload, headers = client.call("POST", path, json.dumps(body).encode(), "application/json")
    return json_of(f"POST {path}", status, payload, headers, expected)


def bad_and_good_controls(client: Client) -> None:
    cases = [
        ("/api/power", b"not-json", None, 400, "body must be JSON"),
        ("/api/power", b"[]", "application/json", 400, "body must be an object"),
        ("/api/power", b'{"on":"yes"}', "application/json", 400, "on must be a boolean"),
        ("/api/power", b'{"on":true,"extra":1}', "application/json", 400, "unknown fields: extra"),
        ("/api/brightness", b'{"level":0}', "application/json", 400, "level must be an integer 1..100"),
        ("/api/color", b'{"r":1}', "application/json", 400, "r, g, b required"),
        ("/api/color", b'{"r":1,"g":2,"b":300}', "application/json", 400, "r, g, b must be integers 0..255"),
        ("/api/scene", b'{"name":"No Such Scene"}', "application/json", 400, "unknown scene No Such Scene"),
        ("/api/scene", b'{"name":"Sunset Glow","speed":99}', "application/json", 400, "scene speed index 99 outside 0..2"),
        ("/api/paint", b'{"groups":[]}', "application/json", 400, "paint needs at least one group"),
    ]
    for path, raw, content_type, code, message in cases:
        status, payload, headers = client.call("POST", path, raw, content_type)
        check_eq(f"POST {path} bad", detail(f"POST {path}", status, payload, headers, code), message)

    # The body is read as JSON whatever the Content-Type says.
    for content_type in ("text/plain", None):
        status, payload, headers = client.call("POST", "/api/power", b'{"on":true}', content_type)
        check_eq(f"POST /api/power as {content_type}", status, 200)

    status, payload, headers = client.call("POST", "/api/power", b'{"on":"\xff"}', "application/json")
    check_eq("POST /api/power bad UTF-8", detail("POST /api/power", status, payload, headers, 400), "body must be JSON")

    state = post_json(client, "/api/power", {"on": False}, 200)
    check("POST /api/power shape", isinstance(state, dict) and isinstance(state.get("state"), dict), state)
    snap = state["state"]
    check_eq("POST /api/power on", snap.get("on"), False)
    check_eq("POST /api/power mode", snap.get("mode"), "off")
    check_eq("POST /api/power device", snap.get("device"), "h7020")

    state = post_json(client, "/api/brightness", {"level": 40}, 200)
    check_eq("POST /api/brightness", state["state"]["brightness"] if isinstance(state, dict) else state, 40)

    state = post_json(client, "/api/color", {"r": 0, "g": 0, "b": 255}, 200)
    snap = state["state"] if isinstance(state, dict) else {}
    check_eq("POST /api/color", snap.get("color"), {"r": 0, "g": 0, "b": 255})
    check_eq("POST /api/color mode", snap.get("mode"), "color")

    state = post_json(client, "/api/scene", {"name": "Sunset Glow"}, 200)
    snap = state["state"] if isinstance(state, dict) else {}
    check_eq("POST /api/scene", snap.get("scene"), SCENE)
    check_eq("POST /api/scene mode", snap.get("mode"), "scene")
    check_eq("POST /api/scene on", snap.get("on"), True)

    state = post_json(client, "/api/paint", {"groups": [{"bulbs": [2, 1], "rgb": [0, 255, 0]}]}, 200)
    snap = state["state"] if isinstance(state, dict) else {}
    check_eq("POST /api/paint", snap.get("paint"), [{"bulbs": [1, 2], "rgb": [0, 255, 0]}])
    check_eq("POST /api/paint mode", snap.get("mode"), "paint")


def library_rows(client: Client) -> None:
    status, payload, headers = client.call(
        "POST",
        "/api/patterns",
        b'{"id":"Bad","name":"X","kind":"scene"}',
        "application/json",
    )
    check_eq("POST /api/patterns bad id", detail("POST pattern", status, payload, headers, 400), "id must be a short lowercase id")

    created = {
        "id": PATTERN_ID,
        "name": "Contract scene",
        "kind": "scene",
        "scene": {"name": "Sunset Glow", "speed": 2},
    }
    status, payload, headers = client.call("POST", "/api/patterns", json.dumps(created).encode(), "application/json")
    check_eq("POST /api/patterns", json_of("POST pattern", status, payload, headers, 200), created)

    status, payload, headers = client.call("POST", "/api/patterns", json.dumps(created).encode(), "application/json")
    check_eq("POST /api/patterns duplicate", detail("POST pattern duplicate", status, payload, headers, 409), "already saved")

    status, payload, headers = client.call("GET", f"/api/patterns/{PATTERN_ID}")
    check_eq("GET /api/patterns/{id}", json_of("GET pattern", status, payload, headers, 200), created)

    status, payload, headers = client.call("GET", "/api/patterns/not-saved")
    check_eq("GET missing pattern", detail("GET pattern missing", status, payload, headers, 404), "not found")

    renamed = dict(created)
    renamed["name"] = "Contract scene two"
    status, payload, headers = client.call(
        "PUT",
        f"/api/patterns/{PATTERN_ID}",
        json.dumps({"name": "Contract scene two", "kind": "scene", "scene": {"name": "Sunset Glow", "speed": 2}}).encode(),
        "application/json",
    )
    check_eq("PUT /api/patterns/{id}", json_of("PUT pattern", status, payload, headers, 200), renamed)

    status, payload, headers = client.call(
        "PUT",
        "/api/patterns/not-saved",
        json.dumps({"name": "Nope", "kind": "scene", "scene": {"name": "Sunset Glow"}}).encode(),
        "application/json",
    )
    check_eq("PUT missing pattern", detail("PUT pattern missing", status, payload, headers, 404), "not found")

    status, payload, headers = client.call("POST", "/api/patterns/not-saved/apply")
    check_eq("POST missing apply", detail("POST apply missing", status, payload, headers, 404), "not found")

    state = post_json(client, f"/api/patterns/{PATTERN_ID}/apply", {}, 200)
    snap = state["state"] if isinstance(state, dict) else {}
    check_eq("POST apply mode", snap.get("mode"), "scene")
    check_eq("POST apply pattern", snap.get("pattern_id"), PATTERN_ID)
    check_eq("POST apply scene", snap.get("scene"), SCENE)

    status, payload, headers = client.call(
        "POST",
        "/api/schedules",
        b'{"id":"contract-slot","days":["mon"],"time":"25:00","action":{"power":false}}',
        "application/json",
    )
    check_eq("POST /api/schedules bad time", detail("POST schedule", status, payload, headers, 400), "time must be HH:MM")

    schedule = {
        "id": SCHEDULE_ID,
        "enabled": True,
        "days": ["mon", "sun"],
        "time": "06:05",
        "action": {"pattern": PATTERN_ID},
    }
    submitted = {
        "id": SCHEDULE_ID,
        "days": ["sun", "mon"],
        "time": "06:05",
        "action": {"pattern": PATTERN_ID},
    }
    status, payload, headers = client.call("POST", "/api/schedules", json.dumps(submitted).encode(), "application/json")
    check_eq("POST /api/schedules", json_of("POST schedule", status, payload, headers, 200), schedule)

    status, payload, headers = client.call("GET", f"/api/schedules/{SCHEDULE_ID}")
    check_eq("GET /api/schedules/{id}", json_of("GET schedule", status, payload, headers, 200), schedule)

    status, payload, headers = client.call("GET", "/api/schedules/not-saved")
    check_eq("GET missing schedule", detail("GET schedule missing", status, payload, headers, 404), "not found")

    disabled = dict(schedule)
    disabled["enabled"] = False
    status, payload, headers = client.call(
        "PUT",
        f"/api/schedules/{SCHEDULE_ID}",
        json.dumps(disabled).encode(),
        "application/json",
    )
    check_eq("PUT /api/schedules/{id}", json_of("PUT schedule", status, payload, headers, 200), disabled)

    status, payload, headers = client.call(
        "PUT",
        "/api/schedules/not-saved",
        json.dumps({"enabled": True, "days": ["mon"], "time": "06:05", "action": {"power": False}}).encode(),
        "application/json",
    )
    check_eq("PUT missing schedule", detail("PUT schedule missing", status, payload, headers, 404), "not found")

    status, payload, headers = client.call("DELETE", "/api/schedules/not-saved")
    check_eq("DELETE missing schedule", detail("DELETE schedule", status, payload, headers, 404), "not found")
    status, payload, headers = client.call("DELETE", "/api/patterns/not-saved")
    check_eq("DELETE missing pattern", detail("DELETE pattern", status, payload, headers, 404), "not found")


def state_of(client: Client) -> dict[str, object]:
    status, payload, headers = client.call("GET", "/api/state")
    body = json_of("GET /api/state", status, payload, headers, 200)
    check("GET /api/state shape", isinstance(body, dict), body)
    return body if isinstance(body, dict) else {}


def wait_for(client: Client, label: str, seconds: float, ready: Callable[[dict[str, object]], bool]) -> None:
    deadline = time.monotonic() + seconds
    while True:
        snap = state_of(client)
        if ready(snap):
            return
        if time.monotonic() > deadline:
            fail(label, snap)
        time.sleep(0.1)


def playback(client: Client) -> None:
    first = [{"bulbs": [1, 2], "rgb": [255, 0, 0]}]
    second = [{"bulbs": [3, 4], "rgb": [0, 0, 255]}]
    sequence = {
        "id": SEQUENCE_ID,
        "name": "Contract walk",
        "kind": "sequence",
        "repeat": True,
        "steps": [{"hold": 1, "paint": first}, {"hold": 1, "paint": second}],
    }
    check_eq("POST sequence", post_json(client, "/api/patterns", sequence, 200), sequence)
    state = post_json(client, f"/api/patterns/{SEQUENCE_ID}/apply", {}, 200)
    snap = state["state"] if isinstance(state, dict) else {}
    check_eq("apply sequence mode", snap.get("mode"), "sequence")
    check_eq("apply sequence pattern", snap.get("pattern_id"), SEQUENCE_ID)
    check_eq("apply sequence playing", snap.get("playing"), True)
    wait_for(client, "the sequence paints its first step", 3, lambda now: now.get("paint") == first)
    wait_for(client, "the sequence moves to its second step", 3, lambda now: now.get("paint") == second)
    wait_for(client, "a repeating sequence comes back around", 3, lambda now: now.get("paint") == first)

    state = post_json(client, "/api/power", {"on": False}, 200)
    snap = state["state"] if isinstance(state, dict) else {}
    check_eq("power off stops a sequence", snap.get("playing"), False)

    paint = {"id": PAINT_ID, "name": "Contract paint", "kind": "paint", "paint": second}
    check_eq("POST paint pattern", post_json(client, "/api/patterns", paint, 200), paint)
    state = post_json(client, f"/api/patterns/{PAINT_ID}/apply", {}, 200)
    snap = state["state"] if isinstance(state, dict) else {}
    check_eq("apply paint mode", snap.get("mode"), "paint")
    check_eq("apply paint pattern", snap.get("pattern_id"), PAINT_ID)
    check_eq("apply paint", snap.get("paint"), second)


def scheduler(client: Client) -> None:
    """A slot that is due now fires by itself. Its check runs every 5 seconds."""
    now = datetime.now().astimezone()
    if (now.hour, now.minute) == (23, 59):
        time.sleep(61 - now.second)
        now = datetime.now().astimezone()
    post_json(client, "/api/power", {"on": True}, 200)
    check_eq("the light is on before the slot", state_of(client).get("on"), True)
    day = ("mon", "tue", "wed", "thu", "fri", "sat", "sun")[now.weekday()]
    slot = {"id": SCHEDULE_ID, "days": [day], "time": now.strftime("%H:%M"), "action": {"power": False}}
    client.call("DELETE", f"/api/schedules/{SCHEDULE_ID}")
    post_json(client, "/api/schedules", slot, 200)
    wait_for(client, "a due schedule fires", 8, lambda state: state.get("on") is False)
    check_eq("a fired schedule leaves the error alone", state_of(client).get("error"), None)


def logout(client: Client) -> None:
    status, payload, headers = client.call("POST", "/logout", auth=False)
    redirect("POST /logout without cookie", status, payload, headers, "/login")
    expect_cleared_cookie(headers.get("set-cookie", ""))

    status, payload, headers = client.call("POST", "/logout")
    redirect("POST /logout", status, payload, headers, "/login")
    expect_cleared_cookie(headers.get("set-cookie", ""))
    client.cookie = None

    status, payload, headers = client.call("GET", "/api/state", auth=False)
    check_eq("GET /api/state after logout", detail("GET /api/state logged out", status, payload, headers, 401), "login")
    status, payload, headers = client.call("GET", "/lights", auth=False)
    redirect("GET /lights after logout", status, payload, headers, "/login")


def socket_is_private(sock_path: str) -> None:
    path = Path(sock_path)
    deadline = time.monotonic() + 3
    while stat.S_IMODE(os.stat(path).st_mode) != 0o600:
        if time.monotonic() > deadline:
            fail("agent socket mode", oct(stat.S_IMODE(os.stat(path).st_mode)))
        time.sleep(0.05)
    check_eq("agent socket directory mode", stat.S_IMODE(os.stat(path.parent).st_mode), 0o700)


def bearer(token: str) -> dict[str, str]:
    return {"Authorization": f"Bearer {token}"}


def socket_json(sock_path: str, label: str, call: tuple[int, bytes, dict[str, str]], expected: int) -> object:
    status, payload, headers = call
    return json_of(label, status, payload, headers, expected)


def agent_tokens(sock_path: str, cookie: str, data_dir: Path) -> None:
    read = bearer(READ_TOKEN)
    control = {**bearer(CONTROL_TOKEN), "Content-Type": "application/json"}

    call = unix_full(sock_path, "GET", "/api/state", headers=read)
    body = socket_json(sock_path, "socket GET /api/state", call, 200)
    check_eq("socket state device", body.get("device") if isinstance(body, dict) else body, "h7020")
    check_eq("socket state on", body.get("on") if isinstance(body, dict) else body, False)

    manifest = socket_json(sock_path, "socket GET /api/manifest", unix_full(sock_path, "GET", "/api/manifest", headers=read), 200)
    check("socket manifest shape", isinstance(manifest, dict), manifest)
    if isinstance(manifest, dict):
        check_eq("manifest device", manifest.get("device"), "h7020")
        check_eq("manifest bulbs", manifest.get("bulbs"), 15)
        check_eq("manifest absences", manifest.get("absences"), ["diy", "music", "kelvin", "raw_frames"])
        check_eq("manifest scopes", manifest.get("scopes"), ["read", "control", "library", "schedule"])
        scenes = manifest.get("scenes")
        check_eq("manifest scenes", len(scenes) if isinstance(scenes, list) else scenes, 51)
        routes = manifest.get("routes")
        check("manifest routes", isinstance(routes, list) and "POST /api/power" in routes, routes)
        check("manifest hides the LAN", isinstance(routes, list) and not any("/cams" in str(route) for route in routes), routes)

    power = json.dumps({"on": True}).encode()
    status, payload = unix_call(sock_path, "POST", "/api/power", power, {**read, "Content-Type": "application/json"})
    check_eq("read token cannot control", (status, payload), (403, b'{"detail":"missing scope"}'))
    status, payload = unix_call(sock_path, "POST", "/api/power", b"not-json", {**read, "Content-Type": "application/json"})
    check_eq("scope is checked before the body", (status, payload), (403, b'{"detail":"missing scope"}'))

    call = unix_full(sock_path, "POST", "/api/power", power, control)
    answer = socket_json(sock_path, "socket POST /api/power", call, 200)
    check_eq("control token controls", answer["state"]["on"] if isinstance(answer, dict) else answer, True)

    for label, header in (
        ("made up", "Bearer totally-made-up"),
        ("wrong scheme", f"Basic {READ_TOKEN}"),
        ("no secret", "Bearer"),
        ("cut short", f"Bearer {READ_TOKEN[:-1]}"),
        ("with a suffix", f"Bearer {READ_TOKEN}x"),
        ("two words", f"Bearer {READ_TOKEN} x"),
    ):
        status, payload = unix_call(sock_path, "GET", "/api/state", headers={"Authorization": header})
        check_eq(f"socket bearer {label}", (status, payload), (401, b'{"detail":"login"}'))
    status, payload = unix_call(sock_path, "GET", "/api/state", headers={"Authorization": f"bearer {READ_TOKEN}"})
    check_eq("the scheme ignores case", status, 200)
    status, payload = unix_call(sock_path, "GET", "/api/state", headers={**read, "Cookie": cookie})
    check_eq("bearer plus cookie", (status, payload), (401, b'{"detail":"login"}'))

    audit_path = data_dir / "audit.jsonl"
    check("the audit file exists", audit_path.exists(), audit_path)
    audit = audit_path.read_text(encoding="utf-8")
    lines = [json.loads(line) for line in audit.splitlines()]
    check(
        "the audit names who and what",
        any(line.get("token") == "contract-control" and line.get("method") == "POST" and line.get("route") == "/api/power" for line in lines),
        audit,
    )
    for token in (READ_TOKEN, CONTROL_TOKEN):
        check("the audit holds no token", token not in audit, audit)


def rate_limit(client: Client, sock_path: str) -> None:
    """The one radio takes about thirty control calls a minute."""
    for call in range(RATE_LIMIT):
        status, payload, headers = client.call("POST", "/api/brightness", b'{"level":50}', "application/json")
        check_eq(f"control call {call + 1}", status, 200)
    status, payload, headers = client.call("POST", "/api/brightness", b'{"level":50}', "application/json")
    check_eq("past the limit", detail("POST /api/brightness", status, payload, headers, 429), "slow down")
    status, payload, headers = client.call("POST", "/api/brightness", b"not-json", "application/json")
    check_eq("the limit is checked before the body", detail("POST /api/brightness", status, payload, headers, 429), "slow down")
    status, payload, headers = client.call("GET", "/api/state")
    check_eq("reads are not limited", status, 200)
    status, payload = unix_call(
        sock_path, "POST", "/api/power", b'{"on":true}', {**bearer(READ_TOKEN), "Content-Type": "application/json"}
    )
    check_eq("scope is checked before the limit", (status, payload), (403, b'{"detail":"missing scope"}'))
    status, payload = unix_call(
        sock_path, "POST", "/api/power", b'{"on":true}', {**bearer(CONTROL_TOKEN), "Content-Type": "application/json"}
    )
    check_eq("the socket shares the limit", (status, payload), (429, b'{"detail":"slow down"}'))


def agent_socket(sock_path: str, cookie: str) -> None:
    missing = [
        ("GET", "/lights"),
        ("GET", "/cams"),
        ("GET", "/settings"),
        ("GET", "/panel.css"),
        ("GET", "/api/cams"),
        ("GET", "/api/cams/status"),
        ("GET", "/cams/not-a-cam/stream"),
        ("GET", "/api/settings/cameras"),
        ("PUT", "/api/settings/cameras"),
        ("POST", "/logout"),
    ]
    for method, path in missing:
        status, payload = unix_call(sock_path, method, path)
        check_eq(f"socket {method} {path}", (status, payload), (404, b'{"detail":"not found"}'))

    form = urllib.parse.urlencode({"password": "x"}).encode()
    status, payload = unix_call(
        sock_path,
        "POST",
        "/login",
        form,
        {"Content-Type": "application/x-www-form-urlencoded"},
    )
    check_eq("socket POST /login", (status, payload), (404, b'{"detail":"not found"}'))

    status, payload = unix_call(sock_path, "GET", "/")
    check_eq("socket GET / status", status, 200)
    text_has("socket GET /", payload, 'action="/login"')
    status, payload = unix_call(sock_path, "GET", "/login")
    check_eq("socket GET /login status", status, 200)
    text_has("socket GET /login", payload, 'name="password"')

    status, payload = unix_call(sock_path, "GET", "/api/state")
    check_eq("socket GET /api/state", (status, payload), (401, b'{"detail":"login"}'))
    status, payload = unix_call(sock_path, "GET", "/api/state", headers={"Cookie": cookie})
    check_eq("socket cookie is not a login", (status, payload), (401, b'{"detail":"login"}'))
    status, payload = unix_call(sock_path, "GET", "/api/manifest")
    check_eq("socket GET /api/manifest without a bearer", (status, payload), (401, b'{"detail":"login"}'))


MAP_ROUTE = "/api/mesh/map"
DEVICES_ROUTE = "/devices"
MAP_FILE = "mesh-map.json"
MAP_MAX = 4 * 1024 * 1024


def fresh_map(now: int) -> dict[str, object]:
    return {
        "nodes": {
            "0a0b0c0d": {"rank": 0, "heap_free": 90112, "heap_min": 40960, "hops": 0, "parent": 0, "seen": now, "link_rssi": 0},
            "0a0b0c0e": {"rank": 2, "heap_free": 81920, "heap_min": 30720, "hops": 2, "parent": 0x0A0B0C0D, "seen": now, "link_rssi": -60},
        },
        "devices": {
            "41:22:13:00:00:01 ble-random": {
                "addr": "41:22:13:00:00:01",
                "kind": "ble-random",
                "heard_by": {"0a0b0c0e": {"rssi": -70, "hits": 4, "last": now}},
                "nearest": "0a0b0c0e",
            },
        },
        "identities": {},
        "suggestions": [],
    }


def write_map(path: Path, text: bytes, mtime: float) -> None:
    path.write_bytes(text)
    os.utime(path, (mtime, mtime))


def map_document(client: Client) -> dict[str, object]:
    status, payload, headers = client.call("GET", MAP_ROUTE)
    body = json_of(f"GET {MAP_ROUTE}", status, payload, headers, 200)
    if not isinstance(body, dict):
        fail(f"GET {MAP_ROUTE}", body)
    return body


def stale_as(body: dict[str, object], reason: str, label: str) -> None:
    check_eq(f"{label} stale", body.get("stale"), True)
    check_eq(f"{label} reason", body.get("reason"), reason)
    check(f"{label} now", isinstance(body.get("now"), int), body)
    check_eq(f"{label} carries no nodes", "nodes" in body, False)


def devices_routes(client: Client, sock_path: str, data_dir: Path) -> None:
    """The map on the panel: its pages, the 401s and 303s, the socket's 404s, read only, and the stale states."""
    map_file = data_dir / MAP_FILE
    check("no map file is seeded before the map cases", not map_file.exists(), map_file)

    status, payload, headers = client.call("GET", DEVICES_ROUTE)
    check_eq("GET /devices status", status, 200)
    check_eq("GET /devices content-type", headers.get("content-type"), "text/html; charset=utf-8")
    text_has("GET /devices", payload, "<title>Devices</title>")
    text_has("GET /devices", payload, MAP_ROUTE)
    text_has("GET /devices", payload, "The map is not running.")
    text_lacks("GET /devices", payload, "innerHTML")

    status, payload, headers = client.call("GET", "/")
    text_has("GET / devices tile", payload, 'href="/devices"')
    text_lacks("GET / devices tile", payload, "<!-- devices tile -->")

    token = session_token(PASSWORD)
    flipped = token[:-1] + ("0" if token[-1] != "0" else "1")
    for label, value in (("no cookie", None), ("one character flipped", flipped), ("cut short", token[:-1]), ("empty", "")):
        extra = None if value is None else {"Cookie": f"tallow_panel={value}"}
        status, payload, headers = client.call("GET", DEVICES_ROUTE, auth=False, extra=extra)
        redirect(f"GET /devices {label}", status, payload, headers, "/login")
        status, payload, headers = client.call("GET", MAP_ROUTE, auth=False, extra=extra)
        check_eq(f"GET {MAP_ROUTE} {label}", detail(f"GET {MAP_ROUTE} {label}", status, payload, headers, 401), "login")

    status, payload, headers = client.call("GET", MAP_ROUTE, extra={"Authorization": f"Bearer {CONTROL_TOKEN}"})
    check_eq("cookie plus bearer on the map", detail("GET map bearer", status, payload, headers, 401), "login")

    status, payload, headers = client.call("POST", MAP_ROUTE, b"{}", "application/json")
    check_eq("POST /api/mesh/map", detail("POST map", status, payload, headers, 405), "Method Not Allowed")

    for method, path, extra in (
        ("GET", DEVICES_ROUTE, None),
        ("GET", MAP_ROUTE, None),
        ("GET", MAP_ROUTE, bearer(CONTROL_TOKEN)),
        ("GET", MAP_ROUTE, {"Cookie": client.cookie or ""}),
    ):
        status, payload = unix_call(sock_path, method, path, headers=extra)
        check_eq(f"socket {method} {path}", (status, payload), (404, b'{"detail":"not found"}'))

    body = map_document(client)
    stale_as(body, "missing", "a panel with no map file")

    now = time.time()
    fresh = fresh_map(int(now))
    write_map(map_file, json.dumps(fresh).encode(), now)
    body = map_document(client)
    check_eq("a fresh map is live", body.get("stale"), False)
    check_eq("a fresh map's nodes", body.get("nodes"), fresh["nodes"])
    check_eq("a fresh map's devices", body.get("devices"), fresh["devices"])
    check("a fresh map's age is small", isinstance(body.get("age_s"), int) and 0 <= body["age_s"] <= 5, body)

    write_map(map_file, json.dumps(fresh).encode(), time.time() - 120)
    body = map_document(client)
    stale_as(body, "old", "a map two minutes old")
    check("an old map says its age", isinstance(body.get("age_s"), int) and body["age_s"] >= 60, body)

    for label, text in (("not JSON", b"{"), ("a list", b"[]"), ("with no devices", b'{"nodes": {}}')):
        write_map(map_file, text, time.time())
        stale_as(map_document(client), "malformed", f"a map that is {label}")

    write_map(map_file, b" " * (MAP_MAX + 1), time.time())
    stale_as(map_document(client), "too large", "a map over 4 MiB")

    map_file.unlink()
    map_file.mkdir()
    try:
        stale_as(map_document(client), "unreadable", "a directory where the map should be")
    finally:
        map_file.rmdir()

    write_map(map_file, json.dumps(fresh).encode(), time.time())
    check_eq("the map is live again once the file is good", map_document(client).get("stale"), False)
    map_file.unlink()
    stale_as(map_document(client), "missing", "a map file removed")


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


def write_private(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")
    path.chmod(0o600)


def token_record(name: str, token: str, scopes: list[str]) -> dict[str, object]:
    return {"name": name, "token_sha256": hashlib.sha256(token.encode()).hexdigest(), "scopes": scopes}


@contextmanager
def running_panel(command: str, work: Path, name: str) -> Iterator[tuple[Client, str, Path]]:
    """A fresh panel on a free port, with this run's seeded files. It stops on exit."""
    data_dir = work / name
    data_dir.mkdir(mode=0o700)
    (data_dir / "cams.json").write_text(json.dumps({"title": CAMS_TITLE, "cams": [CAM]}), encoding="utf-8")
    password_file = work / f"{name}.password"
    write_private(password_file, PASSWORD + "\n")
    agents_file = work / f"{name}.agents.json"
    write_private(
        agents_file,
        json.dumps(
            {
                "agents": [
                    token_record("contract-read", READ_TOKEN, ["read"]),
                    token_record("contract-control", CONTROL_TOKEN, ["read", "control", "library", "schedule"]),
                ]
            }
        ),
    )
    port = free_port()
    argv = [
        *shlex.split(command),
        "--port",
        str(port),
        "--password-file",
        str(password_file),
        "--data-dir",
        str(data_dir),
        "--agents-file",
        str(agents_file),
    ]
    log_path = work / f"{name}.log"
    with log_path.open("wb") as log:
        process = subprocess.Popen(argv, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT)
    sock_path = str(data_dir / "agent.sock")
    try:
        deadline = time.monotonic() + 20
        while True:
            if process.poll() is not None:
                raise SystemExit(f"The panel exited with {process.returncode}.\n{log_path.read_text(errors='replace')}")
            try:
                socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
                if os.path.exists(sock_path):
                    break
            except OSError:
                pass
            if time.monotonic() > deadline:
                raise SystemExit(f"The panel did not start.\n{log_path.read_text(errors='replace')}")
            time.sleep(0.05)
        try:
            yield Client(f"http://127.0.0.1:{port}"), sock_path, data_dir
        except SystemExit as stop:
            if stop.code not in (None, 0):
                print(log_path.read_text(errors="replace")[-2000:], file=sys.stderr)
            raise
    finally:
        process.terminate()
        try:
            process.wait(5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--panel-command", default=DEFAULT_PANEL, help="The panel in preview mode, before its flags")
    args = parser.parse_args()
    # A short path: a unix socket's name is limited to about 100 bytes.
    with tempfile.TemporaryDirectory(prefix="tallow-", dir="/tmp") as held:
        work = Path(held)
        with running_panel(args.panel_command, work, "main") as (client, sock_path, data_dir):
            unauthenticated(client)
            forged_cookies(client, session_token(PASSWORD))
            login(client, PASSWORD)
            cookie = client.cookie or ""
            refuse_real_cams(client)
            socket_is_private(sock_path)
            stray_routes(client)
            try:
                light_and_library(client)
            finally:
                if client.cookie:
                    remove_contract_rows(client)
            devices_routes(client, sock_path, data_dir)
            logout(client)
            agent_socket(sock_path, cookie)
            agent_tokens(sock_path, cookie, data_dir)
        with running_panel(args.panel_command, work, "limit") as (client, sock_path, _):
            login(client, PASSWORD)
            rate_limit(client, sock_path)
    print("ok")
    print("skip needs-cam: GET /cams/{id}/stream live body, its headers, 429 and 502")
    print("skip needs-light: 503 disconnected, idle reconnect")


if __name__ == "__main__":
    main()
