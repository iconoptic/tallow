#!/usr/bin/env python3
"""Print a board's serial output for a while, with seconds since opening.

Opening the port resets a CP2102 board. Runs through `tools/mesh.sh monitor`.
"""

from __future__ import annotations

import sys
import time

import serial


def main() -> int:
    port, seconds = sys.argv[1], float(sys.argv[2])
    link = serial.Serial()
    link.port = port
    link.baudrate = 115200
    link.timeout = 0.1
    link.dtr = False
    link.rts = False
    link.open()
    started = time.time()
    buffer = b""
    while time.time() - started < seconds:
        buffer += link.read(4096)
        while b"\n" in buffer:
            line, buffer = buffer.split(b"\n", 1)
            text = line.decode("ascii", "replace").strip()
            if text:
                print(f"{time.time() - started:7.1f} {text}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
