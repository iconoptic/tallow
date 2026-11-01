"""Count frames from one cam for 10 seconds.

Allows only 10.87.0.23, checked as text before any socket. A list of refused
addresses would miss 10.87.21, 173473813, and any hostname, which all reach the
wall cam at 10.87.0.21. Do not point this at an address from a test.
"""

from __future__ import annotations

import argparse
import asyncio
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT.parent / "app"))

import cams as camlib  # noqa: E402

ALLOWED = "10.87.0.23"
SECONDS = 10.0


def refuse(address: str) -> bool:
    """True for everything but toby-cam-0. This does not open a socket."""
    return address != ALLOWED


async def count_frames(address: str, dial, seconds: float = SECONDS) -> int:
    """How many JPEGs `dial` yields before `seconds` elapse."""
    cam = camlib.Cam("check", "check", address)
    upstream = await dial(cam)
    try:
        frames = 0
        loop = asyncio.get_running_loop()
        end = loop.time() + seconds
        async for _frame in upstream.frames():
            frames += 1
            if loop.time() >= end:
                break
        return frames
    finally:
        upstream.close()


def main(argv: list[str] | None = None, dial=camlib.dial) -> int:
    parser = argparse.ArgumentParser(description="Count frames from one cam.")
    parser.add_argument("--address", required=True)
    args = parser.parse_args(argv)
    if refuse(args.address):
        print(f"refusing {args.address}: only {ALLOWED} is allowed", file=sys.stderr)
        return 1
    print(asyncio.run(count_frames(args.address, dial)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
