#!/usr/bin/env python3
"""The host end of the tallow mesh, on the serial port of the root node.

Sends the heartbeat that makes that board the root, decodes what the mesh
carries up, and keeps a map of which node hears which device. The map holds
addresses as heard: phones and watches rotate theirs. It sends nothing to any
device and connects to none.

  tools/mesh.sh host /dev/ttyUSB0 --seconds 120
  tools/mesh.sh host /dev/ttyUSB0 --trial
  tools/mesh.sh host /dev/ttyUSB0 --watch AA:BB:CC:DD:EE:FF

--watch replaces the list of Classic addresses every node pages, up to 16. With
no address it clears the list, so nothing is paged.
"""

from __future__ import annotations

import argparse
import json
import os
import string
import struct
import sys
import time

KIND_NAMES = {0: "ble-public", 1: "ble-random", 2: "classic"}
FRAME_REPORT, FRAME_NAME, FRAME_STATS, FRAME_PROBE, FRAME_ROUTE = 2, 3, 4, 5, 7
RECORD = 10
ROUTE_REASONS = {0: "beacon timeout", 1: "failed acks", 2: "better path"}
STAT_FIELDS = (
    "uptime_s heap_free heap_min heap_block adverts classic inquiries rx_frames rx_drops "
    "rx_bad rx_dup tx_ok tx_failed queue_drops parent_changes evicted parent settings "
    "inquiry_started inquiry_stopped inquiry_errors"
).split()

# window ms, interval ms, inquiry period s, inquiry length. Window 0 stops BLE.
DEFAULT_SCAN = (30, 100, 60, 4)
# The most Classic addresses one paging list holds. The firmware refuses more.
WATCH_MAX = 16
TRIAL = (
    ("radio alone", 0, 100, 0, 0),
    ("BLE 30/100", 30, 100, 0, 0),
    ("BLE 60/100", 60, 100, 0, 0),
    ("BLE 100/100", 100, 100, 0, 0),
    ("BLE 30/100, Classic", 30, 100, 12, 4),
)
# Probes run a little under their rate, because each waits for the loop. A node
# delivering less than this share of the nominal count has stopped partway.
PROBE_PACING = 0.9


def mac_text(raw: bytes) -> str:
    return ":".join(f"{byte:02X}" for byte in raw)


def watch_line(addresses: list[str]) -> bytes:
    """The serial line that replaces the paging list: `W <hex12> ...`, or `W` alone.

    Each address is six colon-separated hex pairs. Anything else is refused here,
    before the serial port opens, since opening it resets the root.
    """
    if len(addresses) > WATCH_MAX:
        raise ValueError(f"at most {WATCH_MAX} addresses, got {len(addresses)}")
    words = []
    for text in addresses:
        pairs = text.split(":")
        if len(pairs) != 6 or not all(len(pair) == 2 and all(c in string.hexdigits for c in pair) for pair in pairs):
            raise ValueError(f"not an address: {text!r}; use six hex pairs, such as AA:BB:CC:DD:EE:FF")
        words.append("".join(pairs).lower())
    line = " ".join(["W", *words])
    return line.encode("ascii") + b"\n"


def parse_report(payload: bytes) -> list[dict]:
    out = []
    for start in range(0, len(payload) - RECORD + 1, RECORD):
        record = payload[start : start + RECORD]
        out.append(
            {
                "addr": mac_text(record[:6]),
                "kind": KIND_NAMES.get(record[6], "unknown"),
                "rssi": struct.unpack("b", record[7:8])[0],
                "count": record[8],
                "age_s": record[9],
            }
        )
    return out


def parse_names(payload: bytes) -> list[dict]:
    out = []
    at = 0
    while at + 13 <= len(payload):
        length = payload[at + 12]
        end = at + 13 + length
        if end > len(payload):
            break
        company = int.from_bytes(payload[at + 7 : at + 9], "little")
        out.append(
            {
                "addr": mac_text(payload[at : at + 6]),
                "kind": KIND_NAMES.get(payload[at + 6], "unknown"),
                "company": None if company == 0xFFFF else company,
                "class": int.from_bytes(payload[at + 9 : at + 12], "little"),
                "name": payload[at + 13 : end].decode("utf-8", "replace"),
            }
        )
        at = end
    return out


def parse_route(payload: bytes) -> dict | None:
    """A parent change: uptime seconds, old parent, new parent, reason. Zero is no parent."""
    if len(payload) != 13:
        return None
    uptime, old, new = struct.unpack("<III", payload[:12])
    return {
        "uptime_s": uptime,
        "old": None if old == 0 else f"{old:08x}",
        "new": None if new == 0 else f"{new:08x}",
        "reason": ROUTE_REASONS.get(payload[12], "unknown"),
    }


def parse_stats(payload: bytes) -> dict | None:
    if len(payload) != 88:
        return None
    stats = dict(zip(STAT_FIELDS, struct.unpack("<21I", payload[:84])))
    stats["rank"] = payload[84]
    stats["neighbours"] = payload[85]
    stats["parent_rssi"] = struct.unpack("b", payload[86:87])[0]
    stats["parent_quality"] = payload[87]
    return stats


class Map:
    def __init__(self) -> None:
        self.devices: dict[str, dict] = {}
        self.nodes: dict[str, dict] = {}

    def device(self, addr: str, kind: str) -> dict:
        return self.devices.setdefault(f"{addr} {kind}", {"addr": addr, "kind": kind, "heard_by": {}})

    def sighting(self, node: str, entry: dict, now: float) -> None:
        heard = self.device(entry["addr"], entry["kind"])["heard_by"].setdefault(node, {"rssi": -127, "hits": 0})
        heard["rssi"] = entry["rssi"]
        heard["hits"] += entry["count"]
        heard["last"] = round(now - entry["age_s"])

    def name(self, entry: dict) -> None:
        device = self.device(entry["addr"], entry["kind"])
        device["name"] = entry["name"]
        if entry["company"] is not None:
            device["company"] = entry["company"]
        if entry["class"]:
            device["class"] = entry["class"]

    def save(self, path: str) -> None:
        for device in self.devices.values():
            heard = device["heard_by"]
            device["nearest"] = max(heard, key=lambda node: heard[node]["rssi"]) if heard else None
        temporary = f"{path}.tmp"
        with open(temporary, "w", encoding="utf-8") as stream:
            json.dump({"nodes": self.nodes, "devices": self.devices}, stream, indent=1, sort_keys=True)
        os.replace(temporary, path)


class Link:
    """The root's serial port. Opening it resets the board, see main()."""

    def __init__(self, port: str) -> None:
        import serial

        self.serial = serial.Serial()
        self.serial.port = port
        self.serial.baudrate = 115200
        self.serial.timeout = 0.05
        self.serial.dtr = False
        self.serial.rts = False
        self.serial.open()
        self.buffer = b""
        self.next_beat = 0.0
        self.command_id = int(time.time()) & 0x7FFF

    def send_command(self, window: int, interval: int, period: int, length: int, hz: int, size: int) -> int:
        self.command_id += 1
        self.serial.write(f"C {self.command_id} {window} {interval} {period} {length} {hz} {size}\n".encode())
        return self.command_id

    def send_watch(self, line: bytes) -> None:
        """Sends a line from `watch_line`. The root floods it to every node."""
        self.serial.write(line)

    def lines(self):
        now = time.time()
        if now >= self.next_beat:
            self.next_beat = now + 1
            self.serial.write(b"H\n")
        self.buffer += self.serial.read(4096)
        while b"\n" in self.buffer:
            line, self.buffer = self.buffer.split(b"\n", 1)
            yield line.decode("ascii", "replace").strip()


def decode_up(line: str) -> dict | None:
    words = line.split(" ")
    if len(words) < 7 or words[0] != "@U":
        return None
    try:
        return {
            "frame": int(words[1]),
            "origin": words[2],
            "boot": int(words[3]),
            "seq": int(words[4]),
            "hops": int(words[5]),
            "rssi": int(words[6]),
            "payload": bytes.fromhex(words[7]) if len(words) > 7 else b"",
        }
    except ValueError:
        return None


def handle(up: dict, world: Map, quiet: bool) -> None:
    node, payload, now = up["origin"], up["payload"], time.time()
    if up["frame"] == FRAME_REPORT:
        for entry in parse_report(payload):
            world.sighting(node, entry, now)
    elif up["frame"] == FRAME_NAME:
        for entry in parse_names(payload):
            world.name(entry)
            if not quiet:
                print(f"name {node} {entry['addr']} {entry['kind']} {entry['name']!r}", flush=True)
    elif up["frame"] == FRAME_ROUTE:
        route = parse_route(payload)
        if route and not quiet:
            print(
                f"route {node} up {route['uptime_s']} s from {route['old'] or 'none'} "
                f"to {route['new'] or 'none'} reason {route['reason']}",
                flush=True,
            )
    elif up["frame"] == FRAME_STATS:
        stats = parse_stats(payload)
        if stats:
            stats["hops"] = up["hops"]
            stats["link_rssi"] = up["rssi"]
            stats["seen"] = round(now)
            world.nodes[node] = stats
            if not quiet:
                print(
                    f"node {node} rank {stats['rank']} up {stats['uptime_s']} s heap {stats['heap_free']} "
                    f"low {stats['heap_min']} block {stats['heap_block']} adverts {stats['adverts']} "
                    f"classic {stats['classic']} inq {stats['inquiries']}/{stats['inquiry_started']}/{stats['inquiry_stopped']}/{stats['inquiry_errors']} tx {stats['tx_ok']}/{stats['tx_failed']} "
                    f"bad {stats['rx_bad']} drops {stats['queue_drops']}+{stats['rx_drops']}",
                    flush=True,
                )


def delivery(seqs: dict[str, set[int]], nodes: dict[str, dict], hold: float, hz: int) -> list[tuple[str, int, int, float]]:
    """(origin, delivered, expected, percent) for each node that should have sent.

    The sequence numbers alone only show holes between the first and last that
    arrived. So the expected count is at least what the hold should have produced,
    and a node that reported stats over the air but sent no probes counts as 0%.
    """
    least = int(hold * hz * PROBE_PACING)
    rows = []
    # The root's own probes never cross the air, and its stats have no link RSSI.
    for origin in sorted(set(seqs) | {node for node, stats in nodes.items() if stats.get("link_rssi")}):
        seen = seqs.get(origin, set())
        expected = max(max(seen) - min(seen) + 1 if seen else 0, least)
        rows.append((origin, len(seen), expected, 100 * len(seen) / expected if expected else 0.0))
    return rows


def run_map(link: Link, world: Map, seconds: float, out: str) -> int:
    end = time.time() + seconds
    next_save = 0.0
    while time.time() < end:
        for line in link.lines():
            up = decode_up(line)
            if up:
                handle(up, world, False)
            elif line.startswith("@"):
                print(line, flush=True)
        if time.time() >= next_save:
            next_save = time.time() + 5
            world.save(out)
    world.save(out)
    print(f"{len(world.devices)} devices, {len(world.nodes)} nodes, saved to {out}")
    return 0


def run_trial(link: Link, world: Map, out: str, hold: float, hz: int, size: int, pass_mark: float) -> int:
    rows = []
    for label, window, interval, period, length in TRIAL:
        command = link.send_command(window, interval, period, length, hz, size)
        # Let the setting flood out and the radio settle before counting.
        settle = time.time() + 6
        while time.time() < settle:
            for line in link.lines():
                up = decode_up(line)
                if up:
                    handle(up, world, True)
        seqs: dict[str, set[int]] = {}
        end = time.time() + hold
        lows: dict[str, int] = {}
        adverts_start: dict[str, int] = {}
        while time.time() < end:
            for line in link.lines():
                up = decode_up(line)
                if not up:
                    continue
                handle(up, world, True)
                # Frames the root made itself never crossed the air.
                if up["frame"] == FRAME_PROBE and up["rssi"] != 0:
                    seqs.setdefault(up["origin"], set()).add(struct.unpack("<I", up["payload"][:4])[0])
                if up["frame"] == FRAME_STATS:
                    stats = parse_stats(up["payload"])
                    if stats and stats["settings"] == command:
                        lows[up["origin"]] = stats["heap_free"]
                        adverts_start.setdefault(up["origin"], stats["adverts"])
        results = delivery(seqs, world.nodes, hold, hz)
        for origin, delivered, expected, percent in results:
            rows.append((label, origin, delivered, expected, percent, lows.get(origin)))
            print(f"{label:22} node {origin}  {delivered:5}/{expected:<5} {percent:6.2f}%  heap {lows.get(origin)}", flush=True)
        if not results:
            rows.append((label, "-", 0, 0, 0.0, None))
            print(f"{label:22} no probes arrived", flush=True)
    link.send_command(*DEFAULT_SCAN, 0, 8)
    world.save(out)
    return 0 if rows and all(row[4] >= pass_mark for row in rows) else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--port", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--seconds", type=float, default=60)
    parser.add_argument("--set", nargs=4, type=int, metavar=("WINDOW", "INTERVAL", "PERIOD", "LENGTH"), help="scan settings for every node")
    parser.add_argument("--trial", action="store_true", help="measure delivery under each scan setting")
    parser.add_argument("--hold", type=float, default=45, help="seconds counted per trial setting")
    parser.add_argument("--hz", type=int, default=10)
    parser.add_argument("--size", type=int, default=200)
    parser.add_argument("--pass-mark", type=float, default=95, help="percent every trial setting must deliver")
    parser.add_argument(
        "--watch",
        nargs="*",
        metavar="ADDR",
        help="Classic addresses to page, colon-separated, up to 16; with none, clears the list",
    )
    args = parser.parse_args()
    watch = None
    if args.watch is not None:
        try:
            watch = watch_line(args.watch)
        except ValueError as error:
            parser.error(str(error))
    link = Link(args.port)
    world = Map()
    if args.trial:
        # Give the leaf time to find this root before the first setting.
        run_map(link, world, 12, args.out)
        if watch is not None:
            link.send_watch(watch)
        return run_trial(link, world, args.out, args.hold, args.hz, args.size, args.pass_mark)
    # Opening the port resets the root through the USB chip's reset line, and
    # anything sent straight away is lost in its boot. Other nodes keep their last
    # settings across that reset, so every run sends them, or the mesh disagrees.
    run_map(link, world, 4, args.out)
    link.send_command(*(args.set or DEFAULT_SCAN), 0, 8)
    if watch is not None:
        link.send_watch(watch)
    return run_map(link, world, args.seconds, args.out)


if __name__ == "__main__":
    sys.exit(main())
