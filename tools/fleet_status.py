"""One row per catalog board: flashed with the current build, and heard.

`tools/mesh.sh fleet` runs this with the current node and cam image ids. It
reads firmware/mesh/devices.jsonl, data/fleet/applied.jsonl, data/mesh/map.json,
the cam reservations, and the dnsmasq leases. With --probe (the default) it
opens a TCP connection to port 6053 on each cam address, the panel's status
check, and nothing else. Opens no serial port and reads no key.
"""

from __future__ import annotations

import argparse
import json
import re
import socket
import sys
import time
from pathlib import Path
from typing import Callable

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
from fleet_catalog import load_reservations  # noqa: E402

STATUS_PORT = 6053
NUMBER = re.compile(r"(\d+)$")


UNREADABLE: list[tuple[str, int]] = []


def _json_lines(path: Path) -> list[dict]:
    rows = []
    skipped = 0
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return rows
    for line in text.splitlines():
        if not line.strip():
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            skipped += 1
            continue
        if isinstance(row, dict):
            rows.append(row)
        else:
            skipped += 1
    if skipped:
        UNREADABLE.append((str(path), skipped))
    return rows


def load_catalog(path: Path) -> list[dict]:
    return _json_lines(path)


def load_journal(path: Path) -> list[dict]:
    return _json_lines(path)


def load_map(path: Path) -> tuple[dict | None, float | None, str]:
    """The map, its mtime, and a note when it could not be read."""
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
        return document, path.stat().st_mtime, ""
    except FileNotFoundError:
        return None, None, f"no map at {path}"
    except PermissionError:
        return None, None, f"cannot read {path} (owned by another user?)"
    except (OSError, ValueError) as error:
        return None, None, f"cannot read {path}: {error}"


def load_leases(path: Path) -> tuple[list[tuple[str, str, str]], str]:
    """(mac, address, hostname) from a dnsmasq lease file, and a note on failure."""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        return [], f"cannot read leases {path}: {error.strerror or error}"
    leases = []
    for line in text.splitlines():
        parts = line.split()
        if len(parts) >= 4:
            leases.append((parts[1].lower(), parts[2], parts[3]))
    return leases, ""


def is_cam(row: dict) -> bool:
    return row.get("type") == "esp32s3-cam"


def last_flash(row: dict, journal: list[dict]) -> dict | None:
    """The latest journal row for this board. A row with a wifi_mac is that
    chip; an older row without one is matched by name."""
    mac = str(row.get("wifi_mac", "")).lower()
    found = None
    for entry in journal:
        recorded = str(entry.get("wifi_mac", "")).lower()
        if recorded:
            if mac and recorded == mac:
                found = entry
        elif entry.get("name") == row.get("name"):
            found = entry
    return found


def flash_state(row: dict, journal: list[dict], node_image: str, cam_image: str) -> str:
    entry = last_flash(row, journal)
    if entry is None:
        return "never"
    current = cam_image if is_cam(row) else node_image
    if not current:
        return "?"
    return "current" if entry.get("image_id") == current else "stale"


def _duration(seconds: float) -> str:
    seconds = int(max(seconds, 0))
    hours, rest = divmod(seconds, 3600)
    minutes = rest // 60
    if hours:
        return f"{hours}h{minutes:02d}m"
    return f"{minutes}m{seconds % 60:02d}s"


def heard_node(row: dict, map_nodes: dict) -> str:
    node = map_nodes.get(str(row.get("mesh_id", "")))
    if node is None:
        return "no"
    text = f"hops {node.get('hops', '?')}"
    if isinstance(node.get("uptime_s"), (int, float)):
        text += f", up {_duration(node['uptime_s'])}"
    return text


def cam_address(row: dict, reservations: dict[str, str], leases: list[tuple[str, str, str]]) -> str:
    mac = str(row.get("wifi_mac", "")).lower()
    if mac in reservations:
        return reservations[mac]
    for lease_mac, address, _host in leases:
        if lease_mac == mac:
            return f"{address} (lease)"
    return ""


def _sort_key(row: dict) -> tuple:
    match = NUMBER.search(str(row.get("name", "")))
    return (is_cam(row), str(row.get("name", "")).rstrip("0123456789"), int(match.group(1)) if match else -1)


def rows_for(
    catalog: list[dict],
    journal: list[dict],
    map_nodes: dict,
    reservations: dict[str, str],
    leases: list[tuple[str, str, str]],
    node_image: str,
    cam_image: str,
    probe: Callable[[str], bool] | None,
) -> list[tuple[str, ...]]:
    table = []
    for row in sorted(catalog, key=_sort_key):
        cam = is_cam(row)
        address = cam_address(row, reservations, leases) if cam else ""
        if cam:
            if not address:
                heard = "no address"
            elif probe is None:
                heard = "not probed"
            else:
                heard = "6053 up" if probe(address.split()[0]) else "6053 down"
        else:
            heard = heard_node(row, map_nodes)
        table.append(
            (
                str(row.get("name", "")),
                str(row.get("chip", "")),
                str(row.get("wifi_mac", "")).lower(),
                str(row.get("mesh_id", "")) or "-",
                flash_state(row, journal, node_image, cam_image),
                heard,
                address or "-",
            )
        )
    return table


HEADER = ("NAME", "CHIP", "MAC", "MESH-ID", "FLASH", "HEARD", "ADDRESS")


def render(
    table: list[tuple[str, ...]],
    catalog: list[dict],
    map_nodes: dict | None,
    map_age: float | None,
    leases: list[tuple[str, str, str]],
) -> str:
    widths = [max(len(HEADER[i]), *(len(r[i]) for r in table)) if table else len(HEADER[i]) for i in range(len(HEADER))]
    lines = ["  ".join(cell.ljust(widths[i]) for i, cell in enumerate(HEADER)).rstrip()]
    for row in table:
        lines.append("  ".join(cell.ljust(widths[i]) for i, cell in enumerate(row)).rstrip())
    lines.append("")
    if map_nodes is None:
        lines.append("no map")
    else:
        lines.append(f"map written {_duration(map_age or 0)} ago, {len(map_nodes)} node(s)")
        known = {str(row.get("mesh_id", "")) for row in catalog}
        unknown = sorted(set(map_nodes) - known)
        if unknown:
            lines.append("unknown mesh ids: " + ", ".join(unknown))
    macs = {str(row.get("wifi_mac", "")).lower() for row in catalog}
    stray = [lease for lease in leases if lease[0] not in macs]
    if stray:
        lines.append("unknown leases: " + "; ".join(" ".join(lease) for lease in stray))
    return "\n".join(lines) + "\n"


def node_up(address: str, port: int = STATUS_PORT, timeout: float = 0.4) -> bool:
    try:
        with socket.create_connection((address, port), timeout=timeout):
            return True
    except OSError:
        return False


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--catalog", type=Path, default=ROOT / "firmware" / "mesh" / "devices.jsonl")
    parser.add_argument("--journal", type=Path, default=ROOT / "data" / "fleet" / "applied.jsonl")
    parser.add_argument("--map", type=Path, default=ROOT / "data" / "mesh" / "map.json")
    parser.add_argument("--reservations", type=Path, default=ROOT / "secrets" / "iot-ap" / "reservations")
    parser.add_argument("--leases", type=Path, default=Path("/var/lib/tallow/dnsmasq.leases"))
    parser.add_argument("--node-image", default="")
    parser.add_argument("--cam-image", default="")
    parser.add_argument("--probe", action=argparse.BooleanOptionalAction, default=True)
    parser.add_argument("--now", type=float, default=None, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)

    UNREADABLE.clear()
    catalog = load_catalog(args.catalog)
    journal = load_journal(args.journal)
    document, mtime, note = load_map(args.map)
    if note:
        print(note, file=sys.stderr)
    map_nodes = document.get("nodes", {}) if isinstance(document, dict) else None
    leases, lease_note = load_leases(args.leases)
    if lease_note:
        print(lease_note, file=sys.stderr)
    reservations = load_reservations(args.reservations)
    for path, count in UNREADABLE:
        print(f"skipped {count} unreadable lines in {path}", file=sys.stderr)
    now = time.time() if args.now is None else args.now
    table = rows_for(
        catalog,
        journal,
        map_nodes or {},
        reservations,
        leases,
        args.node_image,
        args.cam_image,
        node_up if args.probe else None,
    )
    age = None if mtime is None else now - mtime
    sys.stdout.write(render(table, catalog, map_nodes, age, leases))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
