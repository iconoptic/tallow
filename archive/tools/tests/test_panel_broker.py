"""Broker allowlist, header refusal, and the secret file that stays off stdout."""

from __future__ import annotations

import contextlib
import io
import json
import socket
import stat
import sys
import tempfile
import threading
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "app"))
sys.path.insert(0, str(ROOT / "tools"))

import agents  # noqa: E402
import panel_broker  # noqa: E402
import panel_token  # noqa: E402


def _request(method: str, path: str, headers: list[tuple[str, str]], body: bytes = b"") -> bytes:
    lines = [f"{method} {path} HTTP/1.1", *[f"{name}: {value}" for name, value in headers]]
    return ("\r\n".join(lines) + "\r\n\r\n").encode() + body


class AllowlistTest(unittest.TestCase):
    def test_read_and_control_pass_and_library_writes_do_not(self) -> None:
        headers = [("Content-Type", "application/json"), ("Content-Length", "2")]
        self.assertIsNone(panel_broker.classify("GET", "/api/manifest", [], b""))
        self.assertIsNone(panel_broker.classify("GET", "/api/patterns/evening-walk", [], b""))
        self.assertIsNone(panel_broker.classify("GET", "/api/schedules/evening", [], b""))
        self.assertIsNone(panel_broker.classify("POST", "/api/power", headers, b"{}"))
        self.assertIsNone(panel_broker.classify("POST", "/api/patterns/evening-walk/apply", headers, b"{}"))
        self.assertEqual(panel_broker.classify("POST", "/api/patterns", headers, b"{}"), 403)
        self.assertEqual(panel_broker.classify("POST", "/api/schedules", headers, b"{}"), 403)
        self.assertEqual(panel_broker.classify("PUT", "/api/patterns/evening-walk", headers, b"{}"), 403)
        self.assertEqual(panel_broker.classify("DELETE", "/api/schedules/evening", [], b""), 403)
        self.assertEqual(panel_broker.classify("GET", "/api/patterns/Not-A-Slug", [], b""), 403)
        self.assertEqual(panel_broker.classify("POST", "/login", headers, b"{}"), 403)

    def test_client_credentials_query_and_chunking_are_refused(self) -> None:
        self.assertEqual(panel_broker.classify("GET", "/api/state", [("Cookie", "a")], b""), 400)
        self.assertEqual(panel_broker.classify("GET", "/api/state", [("Authorization", "Bearer x")], b""), 400)
        self.assertEqual(panel_broker.classify("GET", "/api/state?x=1", [], b""), 400)
        raw = _request(
            "POST",
            "/api/power",
            [("Transfer-Encoding", "chunked"), ("Content-Type", "application/json")],
            b"{}",
        )
        with self.assertRaises(panel_broker.BrokerError) as caught:
            panel_broker.parse_request(raw)
        self.assertEqual(caught.exception.status, 400)
        doubled = _request(
            "POST",
            "/api/power",
            [
                ("Content-Type", "application/json"),
                ("Content-Length", "2"),
                ("Content-Length", "2"),
            ],
            b"{}",
        )
        with self.assertRaises(panel_broker.BrokerError) as caught:
            panel_broker.parse_request(doubled)
        self.assertEqual(caught.exception.status, 400)


class ForwardTest(unittest.TestCase):
    def test_bearer_is_added_and_the_cookie_is_not_passed_back(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            sock_path = Path(tmp) / "agent.sock"
            seen: list[bytes] = []
            ready = threading.Event()

            def once() -> None:
                server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                server.bind(str(sock_path))
                server.listen(1)
                ready.set()
                conn, _addr = server.accept()
                try:
                    blob = panel_broker.read_message(conn, panel_broker.MAX_REQUEST, 2)
                    seen.append(blob)
                    body = b'{"device":"h7020"}'
                    conn.sendall(
                        b"HTTP/1.1 200 OK\r\n"
                        b"Content-Type: application/json\r\n"
                        b"Content-Length: " + str(len(body)).encode() + b"\r\n"
                        b"Set-Cookie: tallow=browser\r\n"
                        b"\r\n" + body
                    )
                finally:
                    conn.close()
                    server.close()

            thread = threading.Thread(target=once)
            thread.start()
            self.assertTrue(ready.wait(2))
            client, server_end = socket.socketpair()
            try:
                request = _request("GET", "/api/state", [])
                client.sendall(request)
                panel_broker.handle_connection(server_end, "unit-token", sock_path, 3, 3)
                client.settimeout(2)
                response = client.recv(4096)
            finally:
                client.close()
                server_end.close()
                thread.join(2)
            self.assertEqual(len(seen), 1)
            self.assertIn(b"Authorization: Bearer unit-token\r\n", seen[0])
            self.assertNotIn(b"Cookie", seen[0])
            self.assertNotIn(b"unit-token", response)
            self.assertNotIn(b"Set-Cookie", response)
            self.assertIn(b'"device":"h7020"', response)

    def test_missing_socket_is_503_and_a_foreign_cid_gets_nothing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            missing = Path(tmp) / "agent.sock"
            client, server_end = socket.socketpair()
            try:
                client.sendall(_request("GET", "/api/state", []))
                panel_broker.handle_connection(server_end, "unit-token", missing, 3, 3)
                client.settimeout(2)
                response = client.recv(4096)
                self.assertTrue(response.startswith(b"HTTP/1.1 503 "))
                foreign, foreign_end = socket.socketpair()
                try:
                    foreign.sendall(_request("GET", "/api/state", []))
                    panel_broker.handle_connection(foreign_end, "unit-token", missing, 3, 9)
                    foreign_end.close()
                    foreign.settimeout(2)
                    # Unread bytes plus a close is a reset on Linux. Either way the peer gets no HTTP response.
                    try:
                        self.assertEqual(foreign.recv(64), b"")
                    except ConnectionResetError:
                        pass
                finally:
                    foreign.close()
                    foreign_end.close()
            finally:
                client.close()
                server_end.close()


class SecretFileTest(unittest.TestCase):
    def test_secret_file_is_mode_0600_and_stdout_stays_empty(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            agents_path = root / "secrets" / "panel" / "agents.json"
            secret = root / "secrets" / "panel" / "hermes.token"
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                panel_token.mint(agents_path, "hermes", ["read", "control"], secret)
            self.assertEqual(stdout.getvalue(), "")
            self.assertEqual(stat.S_IMODE(secret.stat().st_mode), 0o600)
            self.assertEqual(stat.S_IMODE(agents_path.stat().st_mode), 0o600)
            loaded = panel_broker.load_token(secret)
            self.assertIsNotNone(agents.match_agent(agents.load_agents(agents_path), loaded))
            config_path = root / "secrets" / "panel" / "broker.json"
            sock = root / "data" / "panel" / "agent.sock"
            config_path.write_text(
                json.dumps(
                    {
                        "cid": 3,
                        "port": panel_broker.PORT,
                        "socket": str(sock),
                        "token_file": str(secret),
                    }
                ),
                encoding="utf-8",
            )
            config_path.chmod(0o600)
            config = panel_broker.load_config(config_path)
            self.assertEqual(config["cid"], 3)
            self.assertEqual(config["socket"], sock)
            config_path.write_text(
                json.dumps(
                    {
                        "cid": 3,
                        "port": panel_broker.PORT,
                        "socket": str(root / "data" / "panel-preview" / "agent.sock"),
                        "token_file": str(secret),
                    }
                ),
                encoding="utf-8",
            )
            config_path.chmod(0o600)
            with self.assertRaises(ValueError):
                panel_broker.load_config(config_path)


if __name__ == "__main__":
    unittest.main()
