"""Synthetic-capture tests for tools/parse_btsnoop.py (stdlib only)."""

from __future__ import annotations

import struct
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from parse_btsnoop import BTSNOOP_EPOCH_DELTA_US, parse_btsnoop  # noqa: E402

SMALL = bytes.fromhex("7e000401ef")


def header(datalink: int) -> bytes:
    return b"btsnoop\x00" + struct.pack(">II", 1, datalink)


def record(packet: bytes, flags: int = 0, orig_len: int | None = None, ts_s: float = 0.0) -> bytes:
    ts = BTSNOOP_EPOCH_DELTA_US + int(ts_s * 1e6)
    return struct.pack(">IIIIq", orig_len or len(packet), len(packet), flags, 0, ts) + packet


def acl(conn: int, pb: int, data: bytes) -> bytes:
    return struct.pack("<HH", conn | (pb << 12), len(data)) + data


def l2cap_att_write(handle: int, payload: bytes, opcode: int = 0x52) -> bytes:
    att = bytes([opcode]) + struct.pack("<H", handle) + payload
    return struct.pack("<HH", len(att), 0x0004) + att


class ParseBtsnoopTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)

    def parse(self, blob: bytes):
        path = Path(self._tmp.name) / "cap.log"
        path.write_bytes(blob)
        return parse_btsnoop(path)

    def test_h4_small_write(self) -> None:
        frame = acl(0x40, 0b10, l2cap_att_write(0x17, SMALL))
        (w,) = self.parse(header(1002) + record(b"\x02" + frame, ts_s=12.5))
        self.assertEqual((w.handle, w.payload_hex, w.direction), (0x17, SMALL.hex(), "sent"))
        self.assertEqual(w.conn_handle, 0x40)
        self.assertAlmostEqual(w.timestamp, 12.5)
        self.assertFalse(w.truncated)

    def test_h4_fragmented_write_is_reassembled(self) -> None:
        payload = bytes(range(40))
        l2 = l2cap_att_write(0x17, payload)
        frags = [l2[i : i + 27] for i in range(0, len(l2), 27)]
        blob = header(1002) + record(b"\x02" + acl(0x40, 0b00, frags[0]))
        blob += b"".join(record(b"\x02" + acl(0x40, 0b01, f)) for f in frags[1:])
        (w,) = self.parse(blob)
        self.assertEqual(w.payload_hex, payload.hex())
        self.assertFalse(w.truncated)

    def test_interleaved_connections_reassemble_independently(self) -> None:
        a, b = l2cap_att_write(0x10, bytes(30)), l2cap_att_write(0x20, bytes([1]) * 30)
        blob = header(1002)
        blob += record(b"\x02" + acl(0x40, 0b00, a[:20]))
        blob += record(b"\x02" + acl(0x41, 0b00, b[:20]))
        blob += record(b"\x02" + acl(0x40, 0b01, a[20:]))
        blob += record(b"\x02" + acl(0x41, 0b01, b[20:]))
        writes = self.parse(blob)
        self.assertEqual([(w.handle, w.payload_len) for w in writes], [(0x10, 30), (0x20, 30)])

    def test_h1_no_type_byte(self) -> None:
        frame = acl(0x40, 0b10, l2cap_att_write(0x17, SMALL))
        (w,) = self.parse(header(1001) + record(frame, flags=0b01))
        self.assertEqual((w.payload_hex, w.direction), (SMALL.hex(), "received"))

    def test_h1_skips_command_event(self) -> None:
        frame = acl(0x40, 0b10, l2cap_att_write(0x17, SMALL))
        self.assertEqual(self.parse(header(1001) + record(frame, flags=0b10)), [])

    def test_monitor_acl_tx(self) -> None:
        frame = acl(0x40, 0b00, l2cap_att_write(0x17, SMALL))
        (w,) = self.parse(header(2001) + record(frame, flags=(1 << 16) | 4))
        self.assertEqual((w.payload_hex, w.direction), (SMALL.hex(), "sent"))

    def test_snaplen_truncated_record_is_flagged(self) -> None:
        frame = acl(0x40, 0b10, l2cap_att_write(0x17, bytes(40)))
        cut = b"\x02" + frame[:20]
        (w,) = self.parse(header(1002) + record(cut, orig_len=1 + len(frame)))
        self.assertTrue(w.truncated)

    def test_capture_ending_mid_frame_is_flagged(self) -> None:
        l2 = l2cap_att_write(0x17, bytes(40))
        (w,) = self.parse(header(1002) + record(b"\x02" + acl(0x40, 0b00, l2[:27])))
        self.assertTrue(w.truncated)
        self.assertEqual(w.payload_len, 20)

    def test_orphan_continuation_is_not_a_write(self) -> None:
        bogus = l2cap_att_write(0x17, b"\xaa\xbb")  # looks like ATT if misparsed
        self.assertEqual(self.parse(header(1002) + record(b"\x02" + acl(0x40, 0b01, bogus))), [])

    def test_unknown_datalink_raises(self) -> None:
        with self.assertRaisesRegex(ValueError, "datalink type 1003"):
            self.parse(header(1003) + record(b"\x00" * 8))

    def test_missing_magic_raises(self) -> None:
        with self.assertRaisesRegex(ValueError, "btsnooz"):
            self.parse(b"\x00" * 32)


if __name__ == "__main__":
    unittest.main()
