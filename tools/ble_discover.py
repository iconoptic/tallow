#!/usr/bin/env python3
"""Scan for BLE devices, fingerprint likely smart lights, optionally dump GATT.

Requires: .venv/bin/pip install -r tools/requirements.txt (bleak 3.x)
Linux: BlueZ running; user in group that can use the adapter (often 'bluetooth').

GATT connections are only made to --address, or to every row kept by
--lights-only; never to every device in range.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from bleak.backends.device import BLEDevice
    from bleak.backends.scanner import AdvertisementData

# (family label, regex on advertised name). Prefixes mirror upstream matchers:
# elkbledom + lednetwf_ble manifest.json "local_name", led-ble model_db.py.
# A name may match several families (LEDBLE* is claimed by elkbledom and led-ble).
NAME_FINGERPRINTS: list[tuple[str, re.Pattern[str]]] = [
    ("elk-bledom", re.compile(r"^(ELK|MELK|XROCKER|JACKYLED|DMRRBA|LED-|LEDBLE)", re.I)),
    ("lednetwf", re.compile(r"^(LEDnetWF|IOTWF|IOTB)", re.I)),
    ("led-ble", re.compile(r"^(LEDBLE|BLE-LED|Triones|LEDBlue|Dream|QHM|AP\b|LEDnet(?!WF))", re.I)),
    ("leddmx", re.compile(r"^LEDDMX", re.I)),
    ("mrstar", re.compile(r"^GATT-DEMO", re.I)),
    ("govee", re.compile(r"^(Govee|GVH)", re.I)),
    ("magichome2-ble", re.compile(r"Surplife|MagicHome", re.I)),
]

KNOWN_SERVICE_HINTS = {
    "0000fff0-0000-1000-8000-00805f9b34fb": "common LED service (fff0)",
    "0000ffe0-0000-1000-8000-00805f9b34fb": "common LED service (ffe0)",
    "0000ffff-0000-1000-8000-00805f9b34fb": "zengge / magichome2-style",
}

# Writable characteristic (16-bit short UUID) → families that write to it.
# Sources: elkbledom README "Write UUID", led_ble/const.py
# POSSIBLE_WRITE_CHARACTERISTIC_UUIDS, docs/03 (LEDDMX, LEDnetWF).
GATT_WRITE_HINTS: dict[str, list[str]] = {
    "fff3": ["elk-bledom", "mrstar"],
    "ffe1": ["elk-bledom", "leddmx"],
    "ffd9": ["led-ble"],
    "ffe9": ["led-ble"],
    "ffd5": ["led-ble"],
    "ffe5": ["led-ble"],
    "ff01": ["lednetwf", "magichome2-ble", "led-ble"],
}

BASE_UUID_SUFFIX = "-0000-1000-8000-00805f9b34fb"


def fingerprint_name(name: str | None) -> list[str]:
    if not name:
        return []
    return [label for label, pattern in NAME_FINGERPRINTS if pattern.search(name)]


def family_from_gatt(gatt: dict[str, Any], name_candidates: list[str]) -> list[str]:
    """Families suggested by writable characteristics, narrowed by name candidates."""
    found: list[str] = []
    for svc in gatt.get("services", []):
        for char in svc["characteristics"]:
            uuid = char["uuid"].lower()
            props = set(char["properties"])
            if not uuid.endswith(BASE_UUID_SUFFIX) or not props & {"write", "write-without-response"}:
                continue
            for family in GATT_WRITE_HINTS.get(uuid[4:8], []):
                if family not in found:
                    found.append(family)
    narrowed = [f for f in found if f in name_candidates]
    return narrowed or found


def _apply_candidates(row: dict[str, Any], candidates: list[str]) -> None:
    row["family_candidates"] = candidates
    row["family_guess"] = candidates[0] if len(candidates) == 1 else None
    row["ambiguous"] = len(candidates) > 1


def adv_to_dict(device: BLEDevice, adv: AdvertisementData) -> dict[str, Any]:
    name = device.name or adv.local_name
    row: dict[str, Any] = {
        "address": device.address,
        "name": name,
        "rssi": adv.rssi,
        "seen_in_scan": True,
        "manufacturer_data": {
            str(k): bytes(v).hex() for k, v in (adv.manufacturer_data or {}).items()
        },
        "service_uuids": list(adv.service_uuids or []),
        "service_data": {
            str(k): bytes(v).hex() for k, v in (adv.service_data or {}).items()
        },
        "tx_power": adv.tx_power,
    }
    _apply_candidates(row, fingerprint_name(name))
    return row


def unseen_row(address: str) -> dict[str, Any]:
    row: dict[str, Any] = {
        "address": address,
        "name": None,
        "rssi": None,
        "seen_in_scan": False,
        "manufacturer_data": {},
        "service_uuids": [],
        "service_data": {},
        "tx_power": None,
    }
    _apply_candidates(row, [])
    return row


def looks_like_light(row: dict[str, Any]) -> bool:
    if row["family_candidates"]:
        return True
    uuids = {u.lower() for u in row["service_uuids"]}
    return bool(uuids.intersection(KNOWN_SERVICE_HINTS))


async def dump_gatt(address: str, timeout: float, bluez: dict[str, str]) -> dict[str, Any]:
    from bleak import BleakClient

    result: dict[str, Any] = {"address": address, "services": []}
    async with BleakClient(address, timeout=timeout, bluez=bluez) as client:
        for service in client.services:
            svc: dict[str, Any] = {
                "uuid": service.uuid,
                "description": service.description,
                "hint": KNOWN_SERVICE_HINTS.get(service.uuid.lower()),
                "characteristics": [],
            }
            for char in service.characteristics:
                svc["characteristics"].append(
                    {
                        "uuid": char.uuid,
                        "description": char.description,
                        "properties": list(char.properties),
                        "handle": char.handle,
                    }
                )
            result["services"].append(svc)
    return result


async def run(args: argparse.Namespace) -> int:
    from bleak import BleakScanner

    bluez = {"adapter": args.adapter} if args.adapter else {}
    print(f"Scanning {args.scan_seconds}s …", file=sys.stderr)
    devices = await BleakScanner.discover(
        timeout=args.scan_seconds,
        return_adv=True,
        bluez=bluez,
    )

    scanned = [adv_to_dict(device, adv) for device, adv in devices.values()]

    target: dict[str, Any] | None = None
    if args.address:
        wanted = args.address.upper()
        target = next((r for r in scanned if r["address"].upper() == wanted), None)
        if target is None:
            print(f"{args.address} not seen in scan; connecting anyway", file=sys.stderr)
            target = unseen_row(args.address)

    rows = [r for r in scanned if looks_like_light(r)] if args.lights_only else scanned
    if target is not None and target not in rows:
        rows.append(target)

    rows.sort(key=lambda r: (r["rssi"] is None, -(r["rssi"] or -999), r["address"]))

    if args.connect_gatt:
        targets = [target] if target is not None else rows
        for row in targets:
            addr = row["address"]
            print(f"GATT connect {addr} …", file=sys.stderr)
            try:
                row["gatt"] = await dump_gatt(addr, timeout=args.gatt_timeout, bluez=bluez)
            except Exception as exc:  # noqa: BLE001 — discovery tool; report all failures
                row["gatt_error"] = f"{type(exc).__name__}: {exc}"
                continue
            row["family_from_gatt"] = family_from_gatt(row["gatt"], row["family_candidates"])

    payload = {
        "scanned_at": datetime.now(timezone.utc).isoformat(),
        "adapter_note": args.adapter or "bleak default adapter",
        "device_count": len(rows),
        "devices": rows,
    }

    text = json.dumps(payload, indent=2)
    if args.output:
        Path(args.output).write_text(text + "\n", encoding="utf-8")
        print(f"Wrote {args.output}", file=sys.stderr)
    else:
        print(text)
    return 0


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--scan-seconds",
        type=float,
        default=10.0,
        help="Passive/active scan duration (default 10)",
    )
    parser.add_argument(
        "--lights-only",
        action="store_true",
        help="Keep only name-fingerprint or LED service-UUID matches "
        "(fff0/ffe0 also appear on non-light gadgets)",
    )
    parser.add_argument(
        "--connect-gatt",
        action="store_true",
        help="Connect and dump GATT tables of --address, or of every --lights-only row "
        "(close vendor apps first)",
    )
    parser.add_argument(
        "--address",
        help="GATT-dump only this MAC (turns on --connect-gatt; works even if not seen in scan)",
    )
    parser.add_argument(
        "--adapter",
        help="BlueZ adapter to use, e.g. hci1 (default: bleak's choice)",
    )
    parser.add_argument(
        "--gatt-timeout",
        type=float,
        default=20.0,
        help="Per-device GATT connect timeout seconds",
    )
    parser.add_argument(
        "-o",
        "--output",
        help="Write JSON to file instead of stdout",
    )
    args = parser.parse_args()
    if args.address:
        args.connect_gatt = True
    if args.connect_gatt and not (args.address or args.lights_only):
        parser.error("--connect-gatt needs --address MAC or --lights-only (refusing to connect to every device in range)")

    try:
        import bleak  # noqa: F401
    except ImportError:
        print("Missing dependency: .venv/bin/pip install -r tools/requirements.txt", file=sys.stderr)
        raise SystemExit(1) from None
    raise SystemExit(asyncio.run(run(args)))


if __name__ == "__main__":
    main()
