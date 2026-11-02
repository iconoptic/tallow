"""Put catalog cams on the panel page when they answer.

`tools/mesh.sh cams` and `tools/fleetd.sh` run this. A cam is an
`esp32s3-cam` row with a real Wi-Fi MAC and a reservation. The script opens
that address on port 6053, the port the panel uses for status. A cam that
accepts is added. A row already on that address takes the catalog id, so the
page stops naming a different board. A cam that does not answer is not added.
Mesh nodes are not cameras. The title and every other row stay.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import socket
import sys
from pathlib import Path

NODE_PORT = 6053
MAC = re.compile(r"^(?:[0-9a-f]{2}:){5}[0-9a-f]{2}$")
SLUG = re.compile(r"^[a-z][a-z0-9-]{0,62}$")


def on_iot_net(address: str) -> bool:
    """The panel's rule: 10.87.0.0/24, not the network, the AP, or broadcast."""
    parts = address.split(".")
    if parts[:3] != ["10", "87", "0"] or len(parts) != 4:
        return False
    try:
        last = int(parts[3])
    except ValueError:
        return False
    return 2 <= last <= 254 and str(last) == parts[3]


def load_catalog(path: Path) -> list[dict]:
    rows = []
    if not path.exists():
        return rows
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip():
            rows.append(json.loads(line))
    return rows


def load_reservations(path: Path) -> dict[str, str]:
    found: dict[str, str] = {}
    if not path.exists():
        return found
    for line in path.read_text(encoding="utf-8").splitlines():
        text = line.strip().lower()
        if not text or text.startswith("#") or "," not in text:
            continue
        mac, address = text.split(",", 1)
        mac = mac.strip()
        address = address.strip()
        if MAC.fullmatch(mac) and on_iot_net(address):
            found[mac] = address
    return found


def load_inventory(path: Path) -> dict:
    if not path.exists():
        return {"cams": []}
    raw = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(raw, dict) or not isinstance(raw.get("cams"), list):
        raise ValueError(f"{path}: expected {{\"cams\": [...]}}")
    return raw


def catalog_cams(rows: list[dict], reservations: dict[str, str]) -> list[tuple[str, str]]:
    """(name, address) for cams the page can name. Mesh rows are skipped."""
    cams = []
    for row in rows:
        if row.get("type") != "esp32s3-cam":
            continue
        name = str(row.get("name", ""))
        mac = str(row.get("wifi_mac", "")).lower()
        if not SLUG.fullmatch(name) or not MAC.fullmatch(mac):
            continue
        address = reservations.get(mac)
        if address:
            cams.append((name, address))
    return cams


def sync_inventory(
    inventory: dict,
    cams: list[tuple[str, str]],
    reached: set[str],
) -> tuple[dict, list[str]]:
    """Return the inventory and the lines that describe a change.

    `reached` holds addresses whose node port accepted a connection.
    """
    rows = [dict(item) for item in inventory.get("cams", []) if isinstance(item, dict)]
    notes: list[str] = []
    for name, address in cams:
        by_id = next((row for row in rows if row.get("id") == name), None)
        by_address = next((row for row in rows if row.get("address") == address), None)
        up = address in reached
        if by_id is None and by_address is None:
            if not up:
                continue
            rows.append({"id": name, "name": name, "address": address})
            notes.append(f"added {name} at {address}")
            continue
        if by_id is not None and by_address is not None and by_id is not by_address:
            label = str(by_address.get("name") or by_id.get("name") or name)
            old = str(by_address.get("id", ""))
            by_address["id"] = name
            by_address["name"] = label
            rows.remove(by_id)
            notes.append(f"{old} at {address} is {name}")
            continue
        row = by_address if by_address is not None else by_id
        assert row is not None
        if by_address is not None and row.get("id") != name:
            old = str(row.get("id", ""))
            row["id"] = name
            notes.append(f"{old} at {address} is {name}")
        elif by_id is not None and row.get("address") != address:
            row["address"] = address
            notes.append(f"{name} address is {address}")
    document = {"cams": rows}
    if "title" in inventory:
        document = {"title": inventory["title"], "cams": rows}
    return document, notes


def node_up(address: str, port: int = NODE_PORT, timeout: float = 0.4) -> bool:
    """True when the cam's status port accepts. Opens nothing else."""
    try:
        with socket.create_connection((address, port), timeout):
            return True
    except OSError:
        return False


def signature(document: dict) -> tuple:
    title = document["title"] if "title" in document else None
    cams = tuple(
        (row.get("id"), row.get("name"), row.get("address"))
        for row in document.get("cams", [])
        if isinstance(row, dict)
    )
    return title, cams


def write_inventory(path: Path, document: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    text = json.dumps(document, indent=2) + "\n"
    temp = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    temp.write_text(text, encoding="utf-8")
    temp.replace(path)


def run(catalog_path: Path, reservations_path: Path, inventory_path: Path, verbose: bool) -> int:
    try:
        inventory = load_inventory(inventory_path)
    except (OSError, json.JSONDecodeError, ValueError) as error:
        print(error, file=sys.stderr)
        return 1
    cams = catalog_cams(load_catalog(catalog_path), load_reservations(reservations_path))
    reached = {address for _name, address in cams if node_up(address)}
    if verbose:
        for name, address in cams:
            state = "up" if address in reached else "no answer"
            print(f"contact {name} {address}: {state}", file=sys.stderr)
    updated, notes = sync_inventory(inventory, cams, reached)
    for line in notes:
        print(line)
    if signature(updated) != signature(inventory):
        write_inventory(inventory_path, updated)
    return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="Sync catalog cams onto the panel page.")
    parser.add_argument("--catalog", type=Path, required=True)
    parser.add_argument("--reservations", type=Path, required=True)
    parser.add_argument("--inventory", type=Path, required=True)
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args(argv[1:])
    return run(args.catalog, args.reservations, args.inventory, args.verbose)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
