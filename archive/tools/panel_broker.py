#!/usr/bin/env python3
"""Vsock broker for the H7020 agent socket.

Listens on AF_VSOCK port 17620 and accepts one guest CID from
secrets/panel/broker.json. Each connection is one HTTP/1.1 request. The
bearer stays in secrets/panel/hermes.token and is added on the way to
data/panel/agent.sock. Read and control routes are forwarded. Library and
schedule writes are refused here.
"""

from __future__ import annotations

import json
import re
import socket
import stat
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
CONFIG_PATH = ROOT / "secrets" / "panel" / "broker.json"
PORT = 17620
MAX_REQUEST = 16 * 1024
MAX_RESPONSE = 256 * 1024
REQUEST_TIMEOUT = 5.0
UPSTREAM_TIMEOUT = 15.0
SLUG = re.compile(r"^[a-z][a-z0-9-]{0,62}$")

GET_EXACT = frozenset(
    {
        "/api/state",
        "/api/scenes",
        "/api/capabilities",
        "/api/manifest",
        "/api/patterns",
        "/api/schedules",
    }
)
POST_EXACT = frozenset(
    {
        "/api/power",
        "/api/brightness",
        "/api/color",
        "/api/scene",
        "/api/paint",
    }
)
REFUSED_HEADERS = frozenset(
    {"authorization", "cookie", "transfer-encoding", "proxy-authorization"}
)

PHRASES = {
    400: "Bad Request",
    403: "Forbidden",
    502: "Bad Gateway",
    503: "Service Unavailable",
}


class BrokerError(Exception):
    def __init__(self, status: int, detail: str) -> None:
        super().__init__(detail)
        self.status = status
        self.detail = detail


def check_mode(path: Path) -> None:
    mode = stat.S_IMODE(path.stat().st_mode)
    if mode != 0o600:
        raise PermissionError(f"{path} is mode {mode:04o}. chmod 0600 {path}")


def load_token(path: Path) -> str:
    """Return the bearer. The file is one line, mode 0600."""
    if path.name != "hermes.token" or path.parent.name != "panel" or path.parent.parent.name != "secrets":
        raise ValueError(f"{path} is not the hermes token file")
    check_mode(path)
    text = path.read_text(encoding="utf-8")
    if text.endswith("\n"):
        text = text[:-1]
    if not text or any(ch.isspace() for ch in text):
        raise ValueError(f"{path} has a bad token")
    return text


def load_config(path: Path) -> dict[str, Any]:
    check_mode(path)
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ValueError(f"{path} is not an object")
    cid = data.get("cid")
    port = data.get("port")
    sock = data.get("socket")
    token_file = data.get("token_file")
    if isinstance(cid, bool) or not isinstance(cid, int) or not 3 <= cid <= 0xFFFFFFFE:
        raise ValueError(f"{path} has a bad cid")
    if port != PORT:
        raise ValueError(f"{path} must use port {PORT}")
    if not isinstance(sock, str) or not _panel_socket(Path(sock)):
        raise ValueError(f"{path} must name data/panel/agent.sock")
    if not isinstance(token_file, str) or not _token_file(Path(token_file)):
        raise ValueError(f"{path} must name secrets/panel/hermes.token")
    return {"cid": cid, "port": PORT, "socket": Path(sock), "token_file": Path(token_file)}


def _panel_socket(path: Path) -> bool:
    return path.is_absolute() and path.parts[-3:] == ("data", "panel", "agent.sock")


def _token_file(path: Path) -> bool:
    return (
        path.is_absolute()
        and path.name == "hermes.token"
        and path.parent.name == "panel"
        and path.parent.parent.name == "secrets"
    )


def respond(status: int, detail: str) -> bytes:
    body = json.dumps({"detail": detail}).encode("utf-8")
    head = (
        f"HTTP/1.1 {status} {PHRASES[status]}\r\n"
        "Content-Type: application/json\r\n"
        f"Content-Length: {len(body)}\r\n"
        "Connection: close\r\n"
        "\r\n"
    )
    return head.encode("ascii") + body


def parse_request(blob: bytes) -> tuple[str, str, list[tuple[str, str]], bytes]:
    if len(blob) > MAX_REQUEST or b"\r\n\r\n" not in blob:
        raise BrokerError(400, "bad request")
    head, _, rest = blob.partition(b"\r\n\r\n")
    try:
        text = head.decode("ascii")
    except UnicodeDecodeError as exc:
        raise BrokerError(400, "bad request") from exc
    lines = text.split("\r\n")
    parts = lines[0].split(" ")
    if len(parts) != 3 or parts[2] != "HTTP/1.1" or not parts[0].isalpha() or not parts[0].isupper():
        raise BrokerError(400, "bad request")
    method, target = parts[0], parts[1]
    headers: list[tuple[str, str]] = []
    for line in lines[1:]:
        if not line or line[0] in " \t" or ":" not in line:
            raise BrokerError(400, "bad request")
        name, _, value = line.partition(":")
        if not name or any(ord(ch) < 33 or ord(ch) > 126 for ch in name):
            raise BrokerError(400, "bad request")
        headers.append((name, value.strip()))
    if _target_rejected(target):
        raise BrokerError(400, "bad request")
    lengths = [value for name, value in headers if name.lower() == "content-length"]
    if len(lengths) > 1:
        raise BrokerError(400, "bad request")
    if any(name.lower() in REFUSED_HEADERS for name, _value in headers):
        raise BrokerError(400, "bad request")
    if lengths:
        length = _length(lengths[0])
        if length != len(rest) or len(head) + 4 + length > MAX_REQUEST:
            raise BrokerError(400, "bad request")
    elif rest:
        raise BrokerError(400, "bad request")
    return method, target, headers, rest


def _target_rejected(target: str) -> bool:
    if not target.startswith("/") or target != target.strip():
        return True
    if any(mark in target for mark in ("?", "#", "\\", "%", "//")):
        return True
    return ".." in target.split("/")


def _length(value: str) -> int:
    if not value.isdigit() or (len(value) > 1 and value.startswith("0")):
        raise BrokerError(400, "bad request")
    return int(value)


def classify(method: str, path: str, headers: list[tuple[str, str]], body: bytes) -> int | None:
    """Return a refusal status, or None when the request may be forwarded."""
    if _target_rejected(path):
        return 400
    names = [name.lower() for name, _value in headers]
    if any(name in REFUSED_HEADERS for name in names):
        return 400
    if names.count("content-length") > 1 or names.count("content-type") > 1:
        return 400
    lengths = [value for name, value in headers if name.lower() == "content-length"]
    if lengths:
        try:
            length = _length(lengths[0])
        except BrokerError:
            return 400
        if length != len(body):
            return 400
    elif body:
        return 400
    if method == "GET":
        if body or any(name == "content-type" for name in names):
            return 400
        if path in GET_EXACT or _read_slug(path):
            return None
        return 403
    if method == "POST":
        types = [value for name, value in headers if name.lower() == "content-type"]
        if types != ["application/json"]:
            return 400
        try:
            parsed = json.loads(body)
        except json.JSONDecodeError:
            return 400
        if not isinstance(parsed, dict):
            return 400
        if path in POST_EXACT or _apply_slug(path):
            return None
        return 403
    return 403


def _read_slug(path: str) -> bool:
    parts = path.split("/")
    return (
        len(parts) == 4
        and parts[1] == "api"
        and parts[2] in {"patterns", "schedules"}
        and SLUG.fullmatch(parts[3]) is not None
    )


def _apply_slug(path: str) -> bool:
    parts = path.split("/")
    return (
        len(parts) == 5
        and parts[1] == "api"
        and parts[2] == "patterns"
        and parts[4] == "apply"
        and SLUG.fullmatch(parts[3]) is not None
    )


def build_upstream(method: str, path: str, body: bytes, token: str) -> bytes:
    if not token or any(ch.isspace() for ch in token):
        raise BrokerError(503, "token unreadable")
    lines = [
        f"{method} {path} HTTP/1.1",
        "Host: localhost",
        f"Authorization: Bearer {token}",
        "Connection: close",
        f"Content-Length: {len(body)}",
    ]
    if method == "POST":
        lines.insert(3, "Content-Type: application/json")
    return ("\r\n".join(lines) + "\r\n\r\n").encode("ascii") + body


def forward(sock_path: Path, upstream: bytes, timeout: float = UPSTREAM_TIMEOUT) -> bytes:
    if not sock_path.is_socket():
        return respond(503, "panel socket unavailable")
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(timeout)
    try:
        try:
            client.connect(str(sock_path))
        except OSError:
            return respond(503, "panel socket unavailable")
        try:
            client.sendall(upstream)
            return _read_upstream(client, timeout)
        except BrokerError:
            raise
        except OSError as exc:
            raise BrokerError(502, "panel unreachable") from exc
    finally:
        client.close()


def _read_upstream(client: socket.socket, timeout: float) -> bytes:
    try:
        blob = read_message(client, MAX_RESPONSE, timeout)
    except BrokerError as exc:
        raise BrokerError(502, "panel unreachable") from exc
    except OSError as exc:
        raise BrokerError(502, "panel unreachable") from exc
    head, _, body = blob.partition(b"\r\n\r\n")
    try:
        text = head.decode("ascii")
    except UnicodeDecodeError as exc:
        raise BrokerError(502, "panel unreachable") from exc
    lines = text.split("\r\n")
    status_parts = lines[0].split(" ")
    if len(status_parts) < 2 or not status_parts[1].isdigit():
        raise BrokerError(502, "panel unreachable")
    status = int(status_parts[1])
    reason = " ".join(status_parts[2:]) or PHRASES.get(status, "OK")
    if any(ch in reason for ch in "\r\n"):
        raise BrokerError(502, "panel unreachable")
    kept: list[str] = []
    for line in lines[1:]:
        name, _, value = line.partition(":")
        lowered = name.lower()
        if lowered in {"content-type", "content-length"}:
            kept.append(f"{name}: {value.strip()}")
    if not any(item.lower().startswith("content-length:") for item in kept):
        kept.append(f"Content-Length: {len(body)}")
    rebuilt = f"HTTP/1.1 {status} {reason}\r\n" + "\r\n".join(kept) + "\r\nConnection: close\r\n\r\n"
    return rebuilt.encode("ascii") + body


def read_message(conn: socket.socket, limit: int, timeout: float) -> bytes:
    conn.settimeout(timeout)
    data = bytearray()
    while b"\r\n\r\n" not in data:
        chunk = conn.recv(min(4096, limit + 1 - len(data)))
        if not chunk:
            raise BrokerError(400, "bad request")
        data += chunk
        if len(data) > limit:
            raise BrokerError(400, "bad request")
    head, _, rest = bytes(data).partition(b"\r\n\r\n")
    if any(name.lower() == "transfer-encoding" for name, _value in _header_pairs(head)):
        raise BrokerError(400, "bad request")
    lengths = [value for name, value in _header_pairs(head) if name.lower() == "content-length"]
    if len(lengths) > 1:
        raise BrokerError(400, "bad request")
    if not lengths:
        if rest:
            raise BrokerError(400, "bad request")
        return bytes(data)
    total = len(head) + 4 + _length(lengths[0])
    if total > limit:
        raise BrokerError(400, "bad request")
    while len(data) < total:
        chunk = conn.recv(total - len(data))
        if not chunk:
            raise BrokerError(400, "bad request")
        data += chunk
    if len(data) != total:
        raise BrokerError(400, "bad request")
    return bytes(data)


def _header_pairs(head: bytes) -> list[tuple[str, str]]:
    try:
        lines = head.decode("ascii").split("\r\n")[1:]
    except UnicodeDecodeError as exc:
        raise BrokerError(400, "bad request") from exc
    pairs: list[tuple[str, str]] = []
    for line in lines:
        if ":" not in line:
            continue
        name, _, value = line.partition(":")
        pairs.append((name, value.strip()))
    return pairs


def handle_connection(
    conn: socket.socket,
    token: str,
    sock_path: Path,
    allowed_cid: int,
    peer_cid: int,
) -> None:
    if peer_cid != allowed_cid:
        print(f"broker refused peer {peer_cid}", file=sys.stderr)
        return
    method = "-"
    path = "-"
    status = 500
    try:
        blob = read_message(conn, MAX_REQUEST, REQUEST_TIMEOUT)
        method, path, headers, body = parse_request(blob)
        refusal = classify(method, path, headers, body)
        if refusal is not None:
            status = refusal
            detail = "bad request" if refusal == 400 else "missing scope"
            conn.sendall(respond(status, detail))
            return
        response = forward(sock_path, build_upstream(method, path, body, token))
        status = _status_of(response)
        conn.sendall(response)
    except BrokerError as exc:
        status = exc.status
        try:
            conn.sendall(respond(exc.status, exc.detail))
        except OSError:
            pass
    except OSError:
        status = 502
    finally:
        print(f"broker {peer_cid} {method} {path} {status}", file=sys.stderr)


def _status_of(response: bytes) -> int:
    line = response.split(b"\r\n", 1)[0]
    parts = line.split(b" ")
    if len(parts) > 1 and parts[1].isdigit():
        return int(parts[1])
    return 502


def serve(config_path: Path) -> None:
    config = load_config(config_path)
    load_token(config["token_file"])
    listener = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind((socket.VMADDR_CID_ANY, PORT))
    listener.listen(16)
    print(f"broker vsock {PORT} cid {config['cid']}", file=sys.stderr)
    while True:
        conn, addr = listener.accept()
        peer = int(addr[0]) if isinstance(addr, tuple) and addr else -1
        try:
            if peer != config["cid"]:
                print(f"broker refused peer {peer}", file=sys.stderr)
                continue
            token = load_token(config["token_file"])
            handle_connection(conn, token, config["socket"], config["cid"], peer)
        except (OSError, ValueError, PermissionError) as exc:
            print(f"broker {type(exc).__name__}: {exc}", file=sys.stderr)
            try:
                conn.sendall(respond(503, "token unreadable"))
            except OSError:
                pass
        finally:
            conn.close()


def main() -> None:
    config_path = CONFIG_PATH
    if len(sys.argv) == 3 and sys.argv[1] == "--config":
        config_path = Path(sys.argv[2])
    elif len(sys.argv) != 1:
        print("usage: panel_broker.py [--config secrets/panel/broker.json]", file=sys.stderr)
        raise SystemExit(2)
    try:
        serve(config_path)
    except (OSError, ValueError, PermissionError, json.JSONDecodeError) as exc:
        print(f"{type(exc).__name__}: {exc}", file=sys.stderr)
        raise SystemExit(1) from exc


if __name__ == "__main__":
    main()
