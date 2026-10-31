"""The cams page script, run in node against a fake DOM. Skipped when node is not installed."""

from __future__ import annotations

import shutil
import subprocess
import unittest
from pathlib import Path

HARNESS = Path(__file__).with_name("cams_page_harness.js")


@unittest.skipUnless(shutil.which("node"), "node is not installed")
class CamsPageTest(unittest.TestCase):
    def test_the_page_walks_from_watch_to_a_second_cam(self) -> None:
        run = subprocess.run(["node", str(HARNESS)], capture_output=True, text=True, timeout=30, check=False)
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
        self.assertGreaterEqual(run.stdout.count("ok "), 11)


if __name__ == "__main__":
    unittest.main()
