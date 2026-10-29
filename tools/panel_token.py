#!/usr/bin/env python3
"""Mint or replace a bearer token for the H7020 agent socket.

The secret is printed once, unless --secret-file is set. That flag writes it
to the given path at mode 0600 and leaves stdout empty. The sha256 is stored
in secrets/panel/agents.json (mode 0600). The guest VM cannot present it: the
panel accepts bearers only on the host unix socket. The broker reads
secrets/panel/hermes.token.

  .venv/bin/python tools/panel_token.py create hermes
  .venv/bin/python tools/panel_token.py create hermes --scopes read,control
  .venv/bin/python tools/panel_token.py create hermes --scopes read,control --secret-file secrets/panel/hermes.token
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "app"))

import agents  # noqa: E402

AGENTS_PATH = ROOT / "secrets" / "panel" / "agents.json"


def write_secret(path: Path, token: str) -> None:
    """Store the bearer on disk. The caller does not print it."""
    if not token or any(ch.isspace() for ch in token):
        raise ValueError("token is empty or unsafe to store")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.parent.chmod(0o700)
    temp = path.with_suffix(path.suffix + ".tmp")
    temp.write_text(token + "\n", encoding="utf-8")
    temp.chmod(0o600)
    temp.replace(path)
    path.chmod(0o600)


def mint(agents_path: Path, name: str, scopes: list[str], secret_file: Path | None) -> None:
    token, replaced = agents.create_agent(agents_path, name, scopes)
    if secret_file is not None:
        write_secret(secret_file, token)
    else:
        sys.stdout.write(token + "\n")
    if replaced:
        print(f"Replaced the previous token for {name}.", file=sys.stderr)
    if secret_file is not None:
        print(f"Wrote the secret to {secret_file} (mode 0600). It was not printed.", file=sys.stderr)
    print(
        f"Wrote {agents_path}. The panel accepts it on data/panel/agent.sock, not on the house LAN.",
        file=sys.stderr,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    create = sub.add_parser("create", help="Write a token hash and print the secret once")
    create.add_argument("name", help="Short lowercase id, for example hermes")
    create.add_argument(
        "--scopes",
        default=",".join(agents.SCOPES),
        help="Comma-separated: read, control, library, schedule",
    )
    create.add_argument(
        "--secret-file",
        type=Path,
        help="Write the secret here at mode 0600 and do not print it",
    )
    args = parser.parse_args()
    scopes = [part.strip() for part in args.scopes.split(",") if part.strip()]
    try:
        mint(AGENTS_PATH, args.name, scopes, args.secret_file)
    except (OSError, ValueError, PermissionError) as exc:
        print(exc, file=sys.stderr)
        raise SystemExit(1) from exc


if __name__ == "__main__":
    main()
