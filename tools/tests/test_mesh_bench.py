"""The bench tool's pure helpers. No port is opened here."""

from __future__ import annotations

import argparse
import subprocess
import sys
import unittest
from pathlib import Path

TOOLS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TOOLS))

import mesh_bench as bench  # noqa: E402


class Ids(unittest.TestCase):
    def test_a_hex_id_parses(self) -> None:
        self.assertEqual(bench.parse_id("1f"), 0x1F)
        self.assertEqual(bench.parse_id("ffffffff"), 0xFFFFFFFF)

    def test_zero_and_out_of_range_ids_are_refused(self) -> None:
        for text in ("0", "00000000", "100000000", "-1"):
            with self.subTest(text=text), self.assertRaises(argparse.ArgumentTypeError):
                bench.parse_id(text)

    def test_text_that_is_not_hex_is_refused(self) -> None:
        for text in ("zz", "", "1 2"):
            with self.subTest(text=text), self.assertRaises(argparse.ArgumentTypeError):
                bench.parse_id(text)


class Lines(unittest.TestCase):
    def test_each_id_becomes_one_deny_line(self) -> None:
        self.assertEqual(bench.deny_lines([0xABC, 1]), [b"X 00000abc\n", b"X 00000001\n"])

    def test_no_ids_send_nothing(self) -> None:
        self.assertEqual(bench.deny_lines([]), [])

    def test_a_partial_line_waits_for_its_newline(self) -> None:
        lines, rest = bench.take_lines(b"@B 0000abcd 7\n@U 3 0000")
        self.assertEqual(lines, ["@B 0000abcd 7"])
        self.assertEqual(rest, b"@U 3 0000")

    def test_a_line_split_across_reads_is_joined(self) -> None:
        lines, rest = bench.take_lines(b"@P 0000")
        self.assertEqual(lines, [])
        lines, rest = bench.take_lines(rest + b"0001\n")
        self.assertEqual(lines, ["@P 00000001"])
        self.assertEqual(rest, b"")

    def test_carriage_returns_are_stripped(self) -> None:
        lines, _ = bench.take_lines(b"@P 00000000\r\n")
        self.assertEqual(lines, ["@P 00000000"])

    def test_only_at_lines_are_picked(self) -> None:
        out = bench.at_lines(["I (123) boot: ready", "@B 0000abcd 7", "", "@P 00000000", "W (9) wifi: x"])
        self.assertEqual(out, ["@B 0000abcd 7", "@P 00000000"])


class Import(unittest.TestCase):
    def test_importing_the_tool_does_not_load_serial(self) -> None:
        code = f"import sys; sys.path.insert(0, {str(TOOLS)!r}); import mesh_bench; print('serial' in sys.modules)"
        done = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, check=True)
        self.assertEqual(done.stdout.strip(), "False")


if __name__ == "__main__":
    unittest.main()
