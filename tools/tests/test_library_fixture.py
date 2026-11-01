"""Cross-language check for the saved patterns and schedules.

`host/tallowd/tests/fixtures/library/inputs.json` holds raw documents. `app/library.py`
validates and saves them into `patterns.json` and `schedules.json`, which are
committed. `host/tallowd` (library.rs) must write the same bytes, and this file
reads back the library that the Rust test writes to `host/target/library-interop`.

Run `.venv/bin/python tools/tests/test_library_fixture.py --write` to regenerate
the fixtures. Run `tools/mesh.sh daemon check` before this test, because it is
the Rust test that writes the interop library.
"""

from __future__ import annotations

import json
import shutil
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "app"))
sys.path.insert(0, str(ROOT / "tools"))

import govee_h7020 as light  # noqa: E402
import library  # noqa: E402

FIXTURES = ROOT / "host/tallowd/tests/fixtures/library"
INTEROP = ROOT / "host/target/library-interop"
FILES = ("patterns.json", "schedules.json")
ZONE = timezone(timedelta(hours=-6))


def build(root: Path) -> library.Library:
    """Save the raw inputs the way the panel does, and return the library."""
    catalog = library.Library(root)
    raw = json.loads((FIXTURES / "inputs.json").read_text(encoding="utf-8"))
    for doc in raw["patterns"]:
        catalog.save_pattern(library.validate_pattern(doc, 15, light.scene_info), create=True)
    patterns = {item["id"]: item for item in catalog.list_patterns()}
    for doc in raw["schedules"]:
        catalog.save_schedule(
            library.validate_schedule(doc, patterns, light.scene_info), create=True
        )
    return catalog


def write_fixtures() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        build(Path(tmp))
        for name in FILES:
            shutil.copyfile(Path(tmp) / name, FIXTURES / name)
            print(f"wrote {FIXTURES / name}")


class LibraryFixtureTest(unittest.TestCase):
    def test_committed_fixtures_are_what_app_library_writes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            build(Path(tmp))
            for name in FILES:
                self.assertEqual(
                    (Path(tmp) / name).read_bytes(),
                    (FIXTURES / name).read_bytes(),
                    f"{name} is stale: run this file with --write",
                )

    def test_python_reads_back_the_library_rust_wrote(self) -> None:
        for name in FILES:
            if not (INTEROP / name).is_file():
                self.fail(
                    f"{INTEROP / name} is missing. Run tools/mesh.sh daemon check first, "
                    "because its cargo test writes the interop library."
                )
            self.assertEqual(
                (INTEROP / name).read_bytes(),
                (FIXTURES / name).read_bytes(),
                f"Rust wrote {name} with different bytes",
            )
        rust = library.Library(INTEROP)
        python = library.Library(FIXTURES)
        self.assertEqual(rust.list_patterns(), python.list_patterns())
        self.assertEqual(rust.list_schedules(), python.list_schedules())
        due = rust.due_schedule(datetime(2026, 10, 28, 19, 45, tzinfo=ZONE))
        self.assertIsNotNone(due)
        assert due is not None
        self.assertEqual(due["id"], "evening")


if __name__ == "__main__":
    if "--write" in sys.argv:
        write_fixtures()
        sys.exit(0)
    unittest.main()
