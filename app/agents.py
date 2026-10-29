"""Bearer tokens for the host-only agent socket.

The house LAN never accepts these tokens. The socket never accepts the
browser cookie. A token cannot mint tokens or send raw frames.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import re
import secrets
import stat
from pathlib import Path
from typing import Any

SCOPES = ("read", "control", "library", "schedule")
SLUG = re.compile(r"^[a-z][a-z0-9-]{0,62}$")


def _digest(token: str) -> str:
    return hashlib.sha256(token.encode("utf-8")).hexdigest()


def _check_mode(path: Path) -> None:
    mode = stat.S_IMODE(path.stat().st_mode)
    if mode != 0o600:
        raise PermissionError(f"{path} is mode {mode:04o}. chmod 0600 {path}")


def load_agents(path: Path) -> list[dict[str, Any]]:
    """Return token records. A missing file means there are no agents yet."""
    if not path.exists():
        return []
    if not path.is_file():
        raise ValueError(f"{path} is not a file")
    _check_mode(path)
    data = json.loads(path.read_text(encoding="utf-8"))
    agents = data.get("agents") if isinstance(data, dict) else None
    if not isinstance(agents, list):
        raise ValueError(f"{path} is missing agents")
    cleaned: list[dict[str, Any]] = []
    for agent in agents:
        if not isinstance(agent, dict):
            raise ValueError(f"{path} has a bad agent record")
        name = agent.get("name")
        digest = agent.get("token_sha256")
        scopes = agent.get("scopes")
        if not isinstance(name, str) or SLUG.fullmatch(name) is None:
            raise ValueError(f"{path} has a bad agent name")
        if not isinstance(digest, str) or len(digest) != 64:
            raise ValueError(f"{path} has a bad token hash")
        if not isinstance(scopes, list) or not scopes or any(scope not in SCOPES for scope in scopes):
            raise ValueError(f"{path} has bad scopes")
        cleaned.append(
            {
                "name": name,
                "token_sha256": digest,
                "scopes": [scope for scope in SCOPES if scope in scopes],
            }
        )
    return cleaned


def create_agent(path: Path, name: str, scopes: list[str]) -> tuple[str, bool]:
    """Store a new sha256 and return the secret once, plus whether a name was replaced."""
    if not isinstance(name, str) or SLUG.fullmatch(name) is None:
        raise ValueError("name must be a short lowercase id")
    if not scopes or any(scope not in SCOPES for scope in scopes):
        raise ValueError("scopes must be read, control, library, schedule")
    ordered = [scope for scope in SCOPES if scope in scopes]
    token = secrets.token_urlsafe(32)
    agents = load_agents(path) if path.exists() else []
    record = {"name": name, "token_sha256": _digest(token), "scopes": ordered}
    replaced = False
    kept: list[dict[str, Any]] = []
    for agent in agents:
        if agent["name"] == name:
            replaced = True
            continue
        kept.append(agent)
    kept.append(record)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.parent.chmod(0o700)
    temp = path.with_suffix(path.suffix + ".tmp")
    temp.write_text(json.dumps({"agents": kept}, indent=2) + "\n", encoding="utf-8")
    temp.chmod(0o600)
    temp.replace(path)
    path.chmod(0o600)
    return token, replaced


def match_agent(agents: list[dict[str, Any]], token: str) -> dict[str, Any] | None:
    digest = _digest(token)
    for agent in agents:
        if hmac.compare_digest(agent["token_sha256"], digest):
            return agent
    return None


def authenticate(
    channel: str,
    authorization: str | None,
    cookie: str | None,
    browser_token: str,
    agents: list[dict[str, Any]],
) -> tuple[str, frozenset[str]] | None:
    """Return ``(actor, scopes)`` or None.

    The LAN channel accepts only the browser cookie. The agent channel accepts
    only a bearer token. Sending the other credential is a refusal.
    """
    if channel == "lan":
        if authorization:
            return None
        if cookie and hmac.compare_digest(cookie, browser_token):
            return ("browser", frozenset(SCOPES))
        return None
    if channel != "agent":
        return None
    if cookie:
        return None
    if authorization is None:
        return None
    scheme, _, secret = authorization.partition(" ")
    if scheme.lower() != "bearer" or not secret or secret != secret.strip() or " " in secret:
        return None
    agent = match_agent(agents, secret)
    if agent is None:
        return None
    return (agent["name"], frozenset(agent["scopes"]))
