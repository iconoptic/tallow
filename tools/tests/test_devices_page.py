"""The devices page script, run in node against a fake DOM. Skipped when node is not installed."""

from __future__ import annotations

import shutil
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HARNESS = Path(__file__).with_name("devices_page_harness.js")


@unittest.skipUnless(shutil.which("node"), "node is not installed")
class DevicesPageTest(unittest.TestCase):
    def test_the_page_shows_the_empty_state_the_rotating_address_and_a_hostile_name_as_text(self) -> None:
        run = subprocess.run(["node", str(HARNESS)], capture_output=True, text=True, timeout=30, check=False)
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
        self.assertGreaterEqual(run.stdout.count("ok "), 19)

    def test_the_page_never_writes_markup_from_the_map(self) -> None:
        source = (ROOT / "app" / "devices_page.html").read_text(encoding="utf-8")
        self.assertNotIn("innerHTML", source)
        self.assertNotIn("insertAdjacentHTML", source)
        self.assertNotIn("outerHTML", source)


if __name__ == "__main__":
    unittest.main()
