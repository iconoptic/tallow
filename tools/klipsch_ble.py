#!/usr/bin/env python3
"""BLE control and discovery for the Klipsch Flexus Core 100 (docs/devices/klipsch-flexus-core-100.md).

Volume uses the standard LE Audio Volume Control Service, so it needs no reverse engineering.
Power, input, dialog, night, and EQ live in the vendor ``da6d0f*`` services. ``snapshot``,
``diff``, and ``watch`` find which characteristic carries each, by pressing the bar's own remote
while the tool listens.

  klipsch_ble.py volume                    read setting and mute
  klipsch_ble.py volume set 120            absolute 0-255
  klipsch_ble.py volume up|down|mute|unmute
  klipsch_ble.py snapshot -o off.json      read every readable characteristic
  klipsch_ble.py diff off.json on.json     what changed between two snapshots
  klipsch_ble.py watch --seconds 90        print every notification (press remote buttons now)
  klipsch_ble.py write <uuid> <hex>        raw write to one characteristic (vendor probing)

The serial string and the Coordinated Set SIRK are never read or printed.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
import time

from bleak import BleakClient, BleakScanner

DEFAULT_ADDR = "DC:00:00:11:A0:38"
DEFAULT_ADAPTER = "hci0"

VOLUME_STATE = "00002b7d-0000-1000-8000-00805f9b34fb"
VOLUME_CP = "00002b7e-0000-1000-8000-00805f9b34fb"
VOLUME_FLAGS = "00002b7f-0000-1000-8000-00805f9b34fb"

# Never read: serial number string, Coordinated Set SIRK.
SECRET = {
    "00002a25-0000-1000-8000-00805f9b34fb",
    "00002b84-0000-1000-8000-00805f9b34fb",
}

# VCS control point opcodes (Volume Control Service 1.0, 3.3).
OP = {"down": 0x00, "up": 0x01, "unmute_down": 0x02, "unmute_up": 0x03, "set": 0x04, "unmute": 0x05, "mute": 0x06}


def parse_volume(b: bytes) -> dict:
    return {"setting": b[0], "mute": bool(b[1]), "counter": b[2]}


async def connect(addr: str, adapter: str, timeout: float) -> BleakClient:
    dev = await BleakScanner.find_device_by_address(addr, timeout=timeout, bluez={"adapter": adapter})
    if dev is None:
        sys.exit(f"{addr} not heard on {adapter} in {timeout:.0f}s")
    client = BleakClient(dev, bluez={"adapter": adapter})
    await client.connect()
    return client


async def cmd_volume(client: BleakClient, args) -> None:
    state = parse_volume(await client.read_gatt_char(VOLUME_STATE))
    if args.action is None:
        print(json.dumps(state))
        return
    if args.action == "set":
        if args.value is None or not 0 <= args.value <= 255:
            sys.exit("set needs a value 0-255")
        payload = bytes([OP["set"], state["counter"], args.value])
    else:
        payload = bytes([OP[args.action], state["counter"]])
    await client.write_gatt_char(VOLUME_CP, payload, response=True)
    await asyncio.sleep(0.3)
    print(json.dumps({"before": state, "after": parse_volume(await client.read_gatt_char(VOLUME_STATE))}))


async def cmd_snapshot(client: BleakClient, args) -> None:
    out = {"address": args.address, "taken": time.strftime("%Y-%m-%dT%H:%M:%S"), "chars": {}}
    for svc in client.services:
        for ch in svc.characteristics:
            if "read" not in ch.properties or ch.uuid in SECRET:
                continue
            try:
                val = (await client.read_gatt_char(ch)).hex(" ")
            except Exception as e:  # noqa: BLE001 - record and continue; some chars refuse reads
                val = f"error: {e}"
            out["chars"][f"{ch.uuid}@{ch.handle}"] = {"service": svc.uuid, "props": ch.properties, "value": val}
    text = json.dumps(out, indent=2)
    if args.out:
        with open(args.out, "w") as f:
            f.write(text + "\n")
        print(f"{len(out['chars'])} characteristics -> {args.out}")
    else:
        print(text)


def cmd_diff(args) -> None:
    a = json.load(open(args.a))["chars"]
    b = json.load(open(args.b))["chars"]
    same = 0
    for key in sorted(set(a) | set(b)):
        va, vb = a.get(key, {}).get("value"), b.get(key, {}).get("value")
        if va == vb:
            same += 1
            continue
        print(f"{key}\n  - {va}\n  + {vb}")
    print(f"({same} unchanged)")


async def cmd_watch(client: BleakClient, args) -> None:
    t0 = time.monotonic()
    last: dict[str, str] = {}

    def handler(ch, data: bytearray) -> None:
        key = f"{ch.uuid}@{ch.handle}"
        val = bytes(data).hex(" ")
        tag = "" if last.get(key) != val else " (repeat)"
        last[key] = val
        extra = f"  volume={parse_volume(bytes(data))}" if ch.uuid == VOLUME_STATE else ""
        print(f"{time.monotonic() - t0:7.2f}s {key}  {val}{tag}{extra}", flush=True)

    n = 0
    for svc in client.services:
        for ch in svc.characteristics:
            if ("notify" in ch.properties or "indicate" in ch.properties) and ch.uuid not in SECRET:
                try:
                    await client.start_notify(ch, handler)
                    n += 1
                except Exception as e:  # noqa: BLE001
                    print(f"skip {ch.uuid}@{ch.handle}: {e}", file=sys.stderr)
    print(f"listening on {n} characteristics for {args.seconds}s; press remote buttons now", flush=True)
    await asyncio.sleep(args.seconds)


async def cmd_write(client: BleakClient, args) -> None:
    data = bytes.fromhex(args.hex.replace(" ", ""))
    await client.write_gatt_char(args.uuid, data, response=not args.no_response)
    print(f"wrote {data.hex(' ')} to {args.uuid}")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--address", default=DEFAULT_ADDR)
    p.add_argument("--adapter", default=DEFAULT_ADAPTER)
    p.add_argument("--scan-timeout", type=float, default=15.0)
    sub = p.add_subparsers(dest="cmd", required=True)
    v = sub.add_parser("volume")
    v.add_argument("action", nargs="?", choices=["set", "up", "down", "mute", "unmute"])
    v.add_argument("value", nargs="?", type=int)
    s = sub.add_parser("snapshot")
    s.add_argument("-o", "--out")
    d = sub.add_parser("diff")
    d.add_argument("a")
    d.add_argument("b")
    w = sub.add_parser("watch")
    w.add_argument("--seconds", type=float, default=60.0)
    wr = sub.add_parser("write")
    wr.add_argument("uuid")
    wr.add_argument("hex")
    wr.add_argument("--no-response", action="store_true")
    args = p.parse_args()

    if args.cmd == "diff":
        cmd_diff(args)
        return

    async def run() -> None:
        client = await connect(args.address, args.adapter, args.scan_timeout)
        try:
            await {"volume": cmd_volume, "snapshot": cmd_snapshot, "watch": cmd_watch, "write": cmd_write}[args.cmd](
                client, args
            )
        finally:
            await client.disconnect()

    asyncio.run(run())


if __name__ == "__main__":
    main()
