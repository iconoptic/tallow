#!/usr/bin/env python3
"""Hold one board's serial port, refuse some roots, and print what it writes.

Opening the port resets a CP2102 board, so this waits 4 s, sends one `X <id>`
line per --deny, then prints every `@` line for --seconds. It sends no
heartbeat: a heartbeat would make this board the root. Runs through
`tools/mesh.sh bench`.
"""

from __future__ import annotations

import argparse
import sys
import time

BOOT_WAIT_S = 4.0
MAX_DENY = 4


def parse_id(text: str) -> int:
    """A board id in hex. Zero is the board's empty slot, so it is refused."""
    try:
        value = int(text, 16)
    except ValueError:
        raise argparse.ArgumentTypeError(f"{text!r} is not a hex id")
    if not 0 < value <= 0xFFFFFFFF:
        raise argparse.ArgumentTypeError(f"{text!r} is not a board id")
    return value


def deny_lines(ids: list[int]) -> list[bytes]:
    """The serial lines that deny each id. An empty list sends nothing."""
    return [f"X {value:08x}\n".encode("ascii") for value in ids]


def take_lines(buffer: bytes) -> tuple[list[str], bytes]:
    """The complete lines in the buffer, and the bytes still waiting for a newline."""
    *complete, rest = buffer.split(b"\n")
    return [line.decode("ascii", "replace").strip() for line in complete], rest


def at_lines(lines: list[str]) -> list[str]:
    """The lines the board prints for the host: `@B`, `@U`, `@P`, and the rest."""
    return [line for line in lines if line.startswith("@")]


def main() -> int:
    parser = argparse.ArgumentParser(description="Deny roots on one board and print its @ lines.")
    parser.add_argument("port", help="the board's serial port")
    parser.add_argument(
        "--deny",
        action="append",
        type=parse_id,
        default=[],
        metavar="ID",
        help=f"board id in hex to refuse as a neighbour, up to {MAX_DENY} times",
    )
    parser.add_argument("--seconds", type=float, default=60.0, help="how long to hold the port after the wait")
    args = parser.parse_args()
    if len(args.deny) > MAX_DENY:
        parser.error(f"at most {MAX_DENY} --deny ids")
    if args.seconds <= 0:
        parser.error("--seconds must be positive")

    import serial

    link = serial.Serial()
    link.port = args.port
    link.baudrate = 115200
    link.timeout = 0.1
    link.dtr = False
    link.rts = False
    link.open()
    started = time.time()
    sent = False
    buffer = b""
    while time.time() - started < BOOT_WAIT_S + args.seconds:
        if not sent and time.time() - started >= BOOT_WAIT_S:
            for line in deny_lines(args.deny):
                link.write(line)
                print(f"sent {line.decode('ascii').strip()}", file=sys.stderr, flush=True)
            sent = True
        buffer += link.read(4096)
        lines, buffer = take_lines(buffer)
        for line in at_lines(lines):
            print(f"{time.time() - started:7.1f} {line}", flush=True)
    link.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
