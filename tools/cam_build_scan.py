"""Fail a cam build that leaked a credential, without printing the secret.

`log` reads a build log and `firmware/cam/secrets.yaml`. A default
credential, or a secrets value of 8 characters or more, is a leak. The
message is fixed. The log line is not printed.

`image` reads `strings` output on stdin and rejects a baked-in default.
The real Wi-Fi password is supposed to be in the image, so this pass does
not look for it.
"""

from __future__ import annotations

import sys
from pathlib import Path

DENY = (
    "from secrets/iot-ap/passphrase",
    "12345678",
    "password123",
    "YOUR_PASSWORD",
)
LEAKED = "credential appeared in the cam build output"
CLEAN = "no default credential"
DEFAULT = "default credential"


def unquote(raw: str) -> str:
    """Quoted values keep a `#` inside the quotes. An unquoted value stops at `#`."""
    raw = raw.strip()
    if len(raw) >= 2 and raw[0] in {'"', "'"}:
        end = raw.find(raw[0], 1)
        if end > 0:
            return raw[1:end]
    return raw.split("#", 1)[0].strip()


def secret_values(text: str) -> list[str]:
    """wifi_ssid and wifi_password values long enough to search for."""
    found: list[str] = []
    for line in text.splitlines():
        body = line.strip()
        if not body or body.startswith("#") or ":" not in body:
            continue
        name, raw = body.split(":", 1)
        if name.strip() not in {"wifi_ssid", "wifi_password"}:
            continue
        value = unquote(raw)
        if len(value) >= 8:
            found.append(value)
    return found


def default_hit(blob: str) -> bool:
    """A default credential as its own token.

    `12345678` is also a prefix of the `123456789abcdef` test vector that
    ships inside the image. A longer run of digits is not a baked password.
    """
    for item in DENY:
        start = 0
        while True:
            found = blob.find(item, start)
            if found < 0:
                break
            prev = blob[found - 1] if found else ""
            after = found + len(item)
            nxt = blob[after] if after < len(blob) else ""
            if item == "12345678" and (prev.isdigit() or nxt.isdigit()):
                start = found + 1
                continue
            return True
    return False


def log_leaks(log: str, secrets: str) -> bool:
    if default_hit(log):
        return True
    return any(value in log for value in secret_values(secrets))


def main(argv: list[str]) -> int:
    if argv[:1] == ["image"]:
        blob = sys.stdin.read()
        if default_hit(blob):
            print(DEFAULT)
            return 1
        print(CLEAN)
        return 0
    if len(argv) == 3 and argv[0] == "log":
        log = Path(argv[1]).read_text(errors="replace")
        secrets = Path(argv[2]).read_text(errors="replace")
        if log_leaks(log, secrets):
            print(LEAKED, file=sys.stderr)
            return 1
        return 0
    print("usage: cam_build_scan.py log <log> <secrets> | image", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
