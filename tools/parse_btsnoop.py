#!/usr/bin/env python3
"""Extract BLE ATT Write Request/Command payloads from a BTSnoop HCI log.

Supported datalink types:
  1002  HCI UART (H4) — Android ``btsnoop_hci.log``
  1001  Un-encapsulated HCI (H1)
  2001  Linux monitor — ``btmon -w capture.cfa``

ACL fragments are reassembled per connection handle, so writes longer than the
controller's ACL buffer come out whole. Writes that cannot be completed (log
ends mid-frame, snaplen-truncated records) are flagged ``truncated``.

Not supported: the ``btsnooz`` section of a bugreport (written when the full
snoop log was not enabled). Prefer Wireshark / ``rea inspect-packet-capture``
for full dissection; this tool lists write payloads for quick protocol RE.

Usage:
  .venv/bin/python tools/parse_btsnoop.py btsnoop_hci.log --hex
  .venv/bin/python tools/parse_btsnoop.py btsnoop_hci.log --json -o writes.json
"""

from __future__ import annotations

import argparse
import json
import struct
import sys
from dataclasses import asdict, dataclass
from pathlib import Path

# ATT opcodes of interest
ATT_WRITE_REQ = 0x12
ATT_WRITE_CMD = 0x52
ATT_PREP_WRITE_REQ = 0x16
ATT_EXEC_WRITE_REQ = 0x18

OPCODE_NAMES = {
    ATT_WRITE_REQ: "Write Request",
    ATT_WRITE_CMD: "Write Command",
    ATT_PREP_WRITE_REQ: "Prepare Write Request",
    ATT_EXEC_WRITE_REQ: "Execute Write Request",
}

BTSNOOP_MAGIC = b"btsnoop\x00"

DATALINK_H1 = 1001
DATALINK_H4 = 1002
DATALINK_MONITOR = 2001

HCI_ACL_DATA = 0x02  # H4 packet type
MONITOR_ACL_TX = 4
MONITOR_ACL_RX = 5

ATT_CID = 0x0004
PB_CONTINUATION = 0b01

# BTSnoop timestamps are microseconds since 0000-01-01; this is the Unix epoch offset.
BTSNOOP_EPOCH_DELTA_US = 0x00DCDDB30F2F8000


@dataclass
class AttWrite:
    index: int
    opcode: int
    opcode_name: str
    handle: int | None
    payload_hex: str
    payload_len: int
    direction: str
    conn_handle: int
    timestamp: float
    truncated: bool


@dataclass
class _Pending:
    data: bytearray
    expected: int
    timestamp: float
    truncated: bool


def _u16(data: bytes, off: int) -> int:
    return struct.unpack_from("<H", data, off)[0]


def _acl_from_record(datalink: int, flags: int, packet: bytes) -> tuple[bytes, str] | None:
    """Return (ACL packet without any type prefix, direction) or None if not ACL."""
    if datalink == DATALINK_H4:
        if not packet or packet[0] != HCI_ACL_DATA:
            return None
        return packet[1:], "received" if flags & 0x01 else "sent"
    if datalink == DATALINK_H1:
        if flags & 0x02:  # command / event
            return None
        return packet, "received" if flags & 0x01 else "sent"
    if datalink == DATALINK_MONITOR:
        opcode = flags & 0xFFFF
        if opcode == MONITOR_ACL_TX:
            return packet, "sent"
        if opcode == MONITOR_ACL_RX:
            return packet, "received"
        return None
    raise AssertionError(datalink)


def _decode_att(att: bytes) -> tuple[int, int | None, bytes] | None:
    if not att or att[0] not in OPCODE_NAMES:
        return None
    opcode = att[0]
    if opcode in (ATT_WRITE_REQ, ATT_WRITE_CMD) and len(att) >= 3:
        return opcode, _u16(att, 1), att[3:]
    if opcode == ATT_PREP_WRITE_REQ and len(att) >= 5:
        return opcode, _u16(att, 1), att[5:]  # skip value offset u16
    if opcode == ATT_EXEC_WRITE_REQ:
        return opcode, None, att[1:]
    return None


def parse_btsnoop(path: Path) -> list[AttWrite]:
    raw = path.read_bytes()
    if len(raw) < 16:
        raise ValueError("File too small to be BTSnoop")

    start = 0
    if raw[0:8] != BTSNOOP_MAGIC:
        start = raw.find(BTSNOOP_MAGIC)
        if start < 0:
            raise ValueError(
                "Not a BTSnoop file (missing btsnoop magic). If this came from a "
                "bugreport's btsnooz section, enable the full Bluetooth HCI snoop "
                "log, toggle Bluetooth, reproduce, and capture again."
            )
        print(f"warning: btsnoop magic found at offset {start}, not 0", file=sys.stderr)

    datalink = struct.unpack_from(">I", raw, start + 12)[0]
    if datalink not in (DATALINK_H1, DATALINK_H4, DATALINK_MONITOR):
        raise ValueError(
            f"Unsupported BTSnoop datalink type {datalink} (supported: 1001, 1002, "
            "2001). Open it in Wireshark or `rea inspect-packet-capture` instead."
        )

    writes: list[AttWrite] = []
    pending: dict[tuple[str, int], _Pending] = {}
    orphans = 0

    def emit(direction: str, conn: int, frame: _Pending) -> None:
        if len(frame.data) < 4 or _u16(frame.data, 2) != ATT_CID:
            return
        att = bytes(frame.data[4 : frame.expected])
        decoded = _decode_att(att)
        if decoded is None:
            return
        opcode, handle, payload = decoded
        writes.append(
            AttWrite(
                index=len(writes),
                opcode=opcode,
                opcode_name=OPCODE_NAMES[opcode],
                handle=handle,
                payload_hex=payload.hex(),
                payload_len=len(payload),
                direction=direction,
                conn_handle=conn,
                timestamp=frame.timestamp,
                truncated=frame.truncated or len(frame.data) < frame.expected,
            )
        )

    offset = start + 16
    while offset + 24 <= len(raw):
        orig_len, inc_len, flags, _drops, ts = struct.unpack_from(">IIIIq", raw, offset)
        offset += 24
        if offset + inc_len > len(raw):
            break
        packet = raw[offset : offset + inc_len]
        offset += inc_len

        acl_dir = _acl_from_record(datalink, flags, packet)
        if acl_dir is None:
            continue
        acl, direction = acl_dir
        if len(acl) < 4:
            continue

        handle_word = _u16(acl, 0)
        conn = handle_word & 0x0FFF
        pb = (handle_word >> 12) & 0b11
        data = acl[4 : 4 + _u16(acl, 2)]
        snapped = inc_len < orig_len
        key = (direction, conn)
        timestamp = (ts - BTSNOOP_EPOCH_DELTA_US) / 1e6

        if pb == PB_CONTINUATION:
            frame = pending.get(key)
            if frame is None:
                orphans += 1
                continue
            frame.data += data
            frame.truncated |= snapped
        else:
            if key in pending:
                emit(direction, conn, pending.pop(key))
            if len(data) < 4:
                continue
            frame = _Pending(bytearray(data), _u16(data, 0) + 4, timestamp, snapped)
            pending[key] = frame

        if len(frame.data) >= frame.expected or frame.truncated:
            emit(direction, conn, pending.pop(key))

    for (direction, conn), frame in pending.items():
        emit(direction, conn, frame)

    if orphans:
        print(f"warning: skipped {orphans} ACL continuation fragment(s) with no start", file=sys.stderr)
    return writes


def main() -> None:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("logfile", type=Path, help="Path to btsnoop_hci.log / btmon capture")
    parser.add_argument(
        "--hex",
        action="store_true",
        help="Pretty-print spaced hex payloads",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit JSON (also the default when -o ends in .json)",
    )
    parser.add_argument("-o", "--output", type=Path, help="Write output to file")
    parser.add_argument(
        "--min-len",
        type=int,
        default=1,
        help="Skip payloads shorter than this (default 1)",
    )
    parser.add_argument(
        "--direction",
        choices=("sent", "received", "any"),
        default="sent",
        help="Writes sent by the capturing host (default), received by it, or both",
    )
    args = parser.parse_args()

    try:
        writes = parse_btsnoop(args.logfile)
    except ValueError as exc:
        print(exc, file=sys.stderr)
        raise SystemExit(1) from exc

    writes = [
        w
        for w in writes
        if w.payload_len >= args.min_len and args.direction in ("any", w.direction)
    ]

    if args.json or (args.output and args.output.suffix == ".json"):
        text = json.dumps([asdict(w) for w in writes], indent=2) + "\n"
    else:
        lines: list[str] = []
        for w in writes:
            payload = w.payload_hex
            if args.hex:
                payload = " ".join(payload[i : i + 2] for i in range(0, len(payload), 2))
            handle = f"0x{w.handle:04x}" if w.handle is not None else "-"
            arrow = "→" if w.direction == "sent" else "←"
            flag = "  [TRUNCATED]" if w.truncated else ""
            lines.append(
                f"{w.index:4d}  {arrow} conn=0x{w.conn_handle:03x}  {w.opcode_name:22s}  "
                f"handle={handle}  len={w.payload_len:3d}  {payload}{flag}"
            )
        text = "\n".join(lines) + ("\n" if lines else "")

    if args.output:
        args.output.write_text(text, encoding="utf-8")
        print(f"Wrote {len(writes)} writes → {args.output}", file=sys.stderr)
    else:
        sys.stdout.write(text)


if __name__ == "__main__":
    main()
