#!/usr/bin/env python3
"""HTTP contract for the LAN panel. The standard library only.

Start a fresh preview process, then point this script at it. The opening
``/api/state`` check is that process's preview defaults, so rerun it only
against a new process. The password file is the same one passed to
``--password-file``. This script does not read ``secrets/panel/``.

    umask 077 && printf '%s\\n' 'secret' > /tmp/tallow-panel-contract-password
    chmod 0600 /tmp/tallow-panel-contract-password
    .venv/bin/python app/h7020_panel.py --preview --port 18765 \\
        --password-file /tmp/tallow-panel-contract-password
    python3 tools/tests/panel_contract.py \\
        --base-url http://127.0.0.1:18765 \\
        --password-file /tmp/tallow-panel-contract-password \\
        --unix-socket data/panel-preview/agent.sock

A live ``GET /cams/{id}/stream`` body is skipped (``needs-cam``). Opening it
would take the cam's one connection. Preview serves the light routes, so none
are marked ``needs-light``.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import html
import http.client
import json
import socket
import stat
import urllib.parse
from pathlib import Path

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


def load_password(path: Path) -> str:
    if not path.is_file():
        raise SystemExit(f"Missing password file {path}")
    mode = stat.S_IMODE(path.stat().st_mode)
    if mode != 0o600:
        raise SystemExit(f"{path} is mode {mode:04o}. chmod 0600 {path}")
    password = path.read_text(encoding="utf-8").strip("\n")
    if not password or "\n" in password:
        raise SystemExit(f"{path} must be one non-empty line.")
    return password


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
        if parsed.scheme != "http" or not parsed.hostname or parsed.path not in ("", "/"):
            raise SystemExit("--base-url must be http://host:port")
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
    return status, payload


def expect_login_cookie(header: str, password: str) -> str:
    name, value, attrs = cookie_parts(header)
    check_eq("login cookie name", name, "tallow_panel")
    check_eq("login cookie value", value, session_token(password))
    check_eq("login cookie httponly", "httponly" in attrs, True)
    check_eq("login cookie max-age", attrs.get("max-age"), "2592000")
    check_eq("login cookie path", attrs.get("path"), "/")
    check_eq("login cookie samesite", attrs.get("samesite"), "strict")
    return f"{name}={value}"


def expect_cleared_cookie(header: str) -> None:
    name, value, attrs = cookie_parts(header)
    check_eq("logout cookie name", name, "tallow_panel")
    # Starlette quotes an empty cookie value, so the header reads tallow_panel="".
    check_eq("logout cookie value", value, '""')
    check_eq("logout cookie max-age", attrs.get("max-age"), "0")
    check_eq("logout cookie path", attrs.get("path"), "/")
    check_eq("logout cookie samesite", attrs.get("samesite"), "lax")


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

    api_401 = [
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
    ]
    for method, path in api_401:
        status, payload, headers = client.call(method, path, auth=False)
        check_eq(f"{method} {path} without cookie", detail(f"{method} {path}", status, payload, headers, 401), "login")

    # The manifest hides on the house LAN. It is 404 before the login check.
    status, payload, headers = client.call("GET", "/api/manifest", auth=False)
    check_eq("GET /api/manifest without cookie", detail("GET /api/manifest", status, payload, headers, 404), "not found")


def login(client: Client, password: str) -> None:
    wrong = urllib.parse.urlencode({"password": "not-the-password"}).encode()
    status, payload, headers = client.call(
        "POST",
        "/login",
        wrong,
        "application/x-www-form-urlencoded",
        auth=False,
    )
    redirect("POST /login wrong", status, payload, headers, "/login?error=1")
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
    cams_body = json_of("GET /api/cams", status, payload, headers, 200)
    check("GET /api/cams shape", isinstance(cams_body, dict) and isinstance(cams_body.get("cams"), list), cams_body)
    cams = cams_body["cams"] if isinstance(cams_body, dict) else []
    check_eq("GET /api/cams error", cams_body.get("error") if isinstance(cams_body, dict) else None, None)
    cam_ids: list[str] = []
    for cam in cams:
        check("GET /api/cams item", isinstance(cam, dict) and set(cam) == {"id", "name"}, cam)
        check("GET /api/cams id", isinstance(cam.get("id"), str) and isinstance(cam.get("name"), str), cam)
        cam_ids.append(cam["id"])

    status, payload, headers = client.call("GET", "/api/cams/status")
    status_body = json_of("GET /api/cams/status", status, payload, headers, 200)
    check_eq("GET /api/cams/status keys", sorted(status_body) if isinstance(status_body, dict) else status_body, sorted(cam_ids))
    if isinstance(status_body, dict):
        for cam_id, row in status_body.items():
            check(
                f"GET /api/cams/status {cam_id}",
                isinstance(row, dict) and set(row) == {"online", "viewers"} and isinstance(row["online"], bool) and row["viewers"] == 0,
                row,
            )

    status, payload, headers = client.call("GET", "/cams/not-a-cam/stream")
    check_eq("GET /cams/not-a-cam/stream", detail("GET stream", status, payload, headers, 404), "no such cam")

    status, payload, headers = client.call("GET", "/api/settings/cameras")
    settings = json_of("GET /api/settings/cameras", status, payload, headers, 200)
    check("GET settings shape", isinstance(settings, dict), settings)
    check_eq("GET settings error", settings.get("error") if isinstance(settings, dict) else None, None)
    title = settings.get("title") if isinstance(settings, dict) else None
    listed = settings.get("cams") if isinstance(settings, dict) else None
    check("GET settings title", isinstance(title, str) and title != "", title)
    check("GET settings cams", isinstance(listed, list), listed)
    if isinstance(listed, list):
        for cam in listed:
            check(
                "GET settings cam",
                isinstance(cam, dict) and set(cam) == {"id", "name", "address"} and all(isinstance(cam[key], str) and cam[key] for key in cam),
                cam,
            )
    pages(client, str(title))
    kept = {"title": title, "cams": listed}
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
    text = detail("PUT settings empty title", status, payload, headers, 400)
    check("PUT settings empty title", text.endswith(": title needs 1 to 40 characters"), text)

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


def remove_contract_rows(client: Client) -> None:
    for method, path in (
        ("DELETE", f"/api/schedules/{SCHEDULE_ID}"),
        ("DELETE", f"/api/patterns/{PATTERN_ID}"),
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

    status, payload = unix_call(sock_path, "GET", "/api/state")
    check_eq("socket GET /api/state", (status, payload), (401, b'{"detail":"login"}'))
    status, payload = unix_call(sock_path, "GET", "/api/state", headers={"Cookie": cookie})
    check_eq("socket cookie is not a login", (status, payload), (401, b'{"detail":"login"}'))
    status, payload = unix_call(sock_path, "GET", "/api/manifest")
    check_eq("socket GET /api/manifest without a bearer", (status, payload), (401, b'{"detail":"login"}'))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--password-file", type=Path, required=True)
    parser.add_argument("--unix-socket", required=True, help="Preview agent socket, for example data/panel-preview/agent.sock")
    args = parser.parse_args()
    password = load_password(args.password_file)
    client = Client(args.base_url)
    unauthenticated(client)
    login(client, password)
    cookie = client.cookie or ""
    try:
        light_and_library(client)
    finally:
        if client.cookie:
            remove_contract_rows(client)
    logout(client)
    agent_socket(args.unix_socket, cookie)
    print("ok")
    print("skip needs-cam: GET /cams/{id}/stream live body (would take the cam's one connection)")


if __name__ == "__main__":
    main()
