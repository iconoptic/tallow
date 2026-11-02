"""Identity records for tools/fleetd.sh. Opens no port and reads no key."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path


def bluetooth_mac(mac: str) -> str:
    """Espressif assigns Bluetooth as the base MAC plus 2."""
    parts = [int(part, 16) for part in mac.split(":")]
    carry = 2
    for index in range(5, -1, -1):
        total = parts[index] + carry
        parts[index] = total % 256
        carry = total // 256
    return ":".join(f"{part:02x}" for part in parts)


def mesh_id(mac: str) -> str:
    """Little-endian word of MAC bytes 2 through 5."""
    parts = mac.split(":")
    return "".join(reversed(parts[2:6]))


def chip_name(text: str) -> str:
    """Catalog chip from esptool's chip line. ESP32-S3 before plain ESP32."""
    match = re.search(r"Detecting chip type\.\.\.\s*(\S+)", text)
    if match is None:
        match = re.search(r"Chip is\s+([A-Za-z0-9-]+)", text)
    if match is None:
        return ""
    token = match.group(1)
    if token.startswith("ESP32-S3"):
        return "esp32s3"
    if token.startswith("ESP32-S2"):
        return "esp32s2"
    if token.startswith("ESP32-C3"):
        return "esp32c3"
    if token.startswith("ESP32-C6"):
        return "esp32c6"
    if token.startswith("ESP32"):
        return "esp32"
    return token.lower()


def parse_identity(text: str) -> tuple[str, str, str]:
    """chip, wifi MAC, and flash manufacturer (0xNN) from esptool flash-id text."""
    chip = chip_name(text)
    macs = re.findall(r"MAC:\s*([0-9A-Fa-f]{2}(?::[0-9A-Fa-f]{2}){5})", text)
    if not chip or not macs:
        raise ValueError("esptool text has no chip and MAC")
    manufacturer = re.search(r"Manufacturer:\s*([0-9A-Fa-f]+)", text)
    flash_mfr = f"0x{int(manufacturer.group(1), 16):02x}" if manufacturer else ""
    return chip, macs[0].lower(), flash_mfr


def serial_mac(serial: str) -> str:
    """MAC from a USB serial, or empty.

    Espressif's USB-JTAG serial is the chip's base MAC. The CP2102 boards on
    this host all report ``0001``, which is not a MAC, so the path is all
    they expose.
    """
    text = serial.strip().lower()
    if re.fullmatch(r"(?:[0-9a-f]{2}:){5}[0-9a-f]{2}", text):
        return text
    return ""


def attachment_row(rows: list[dict], usb_path: str, serial: str) -> dict | None:
    """The chip on this cable.

    A USB serial that is a MAC is the chip. The board last recorded on that
    path is a different chip, so it does not answer. A serial that is not a
    MAC leaves the path's row.
    """
    mac = serial_mac(serial)
    if mac:
        for row in rows:
            if str(row.get("wifi_mac", "")).lower() == mac:
                return row
        return None
    if not usb_path:
        return None
    for row in rows:
        if str(row.get("usb_path", "")) == usb_path:
            return row
    return None


def _board_rows(path: Path) -> list[dict]:
    rows = []
    if not path.exists():
        return rows
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) < 4:
            continue
        rows.append(
            {
                "name": parts[0],
                "wifi_mac": parts[1],
                "chip": parts[2],
                "usb_path": parts[3],
            }
        )
    return rows


def _load_catalog(path: Path) -> list[dict]:
    rows = []
    if not path.exists():
        return rows
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip():
            rows.append(json.loads(line))
    return rows


def _load_boards(path: Path) -> tuple[list[str], dict[str, str]]:
    header: list[str] = []
    notes: dict[str, str] = {}
    if not path.exists():
        return header, notes
    for line in path.read_text(encoding="utf-8").splitlines(keepends=True):
        if not line.strip() or line.startswith("#"):
            header.append(line if line.endswith("\n") else line + "\n")
            continue
        parts = line.rstrip("\n").split("\t")
        notes[parts[0]] = parts[4] if len(parts) > 4 else ""
    return header, notes


def _row_for(rows: list[dict], mac: str, chip: str) -> dict:
    for row in rows:
        recorded = str(row.get("wifi_mac", "")).lower()
        if recorded == mac:
            return row
    if chip == "esp32s3":
        unknown = [
            row
            for row in rows
            if row.get("type") == "esp32s3-cam"
            and str(row.get("wifi_mac", "")).lower() in ("", "unknown")
        ]
        if len(unknown) == 1:
            return unknown[0]
    names = {row["name"] for row in rows}
    if chip == "esp32":
        number = 0
        while f"node-{number}" in names:
            number += 1
        row = {
            "type": "esp32-mesh",
            "name": f"node-{number}",
            "push_ok": False,
            "flash_ok": True,
        }
    elif chip == "esp32s3":
        number = 1
        while f"cam-{number}" in names or f"toby-cam-{number}" in names:
            number += 1
        row = {
            "type": "esp32s3-cam",
            "name": f"cam-{number}",
            "push_ok": False,
            "flash_ok": False,
        }
    else:
        number = 0
        while f"board-{number}" in names:
            number += 1
        row = {
            "type": "other",
            "name": f"board-{number}",
            "push_ok": False,
            "flash_ok": False,
        }
    rows.append(row)
    return row


def learn(
    catalog_path: Path,
    boards_path: Path,
    usb_path: str,
    mac: str,
    chip: str,
    flash_mfr: str,
    model: str,
    vid_pid: str,
) -> None:
    """Write one seen chip into the catalog and boards.tsv. Opens nothing."""
    mac = mac.lower()
    rows = _load_catalog(catalog_path)
    header, notes = _load_boards(boards_path)
    row = _row_for(rows, mac, chip)
    for other in rows:
        if other is not row and other.get("usb_path") == usb_path:
            other["usb_path"] = ""
    if chip == "esp32":
        row["type"] = "esp32-mesh"
        row["mesh_id"] = mesh_id(mac)
        row["flash_ok"] = True
    elif chip == "esp32s3":
        row["type"] = "esp32s3-cam"
        row["flash_ok"] = row.get("name") == "toby-cam-0"
    else:
        row["type"] = "other"
        row["flash_ok"] = False
    row["chip"] = chip
    row["usb_path"] = usb_path
    row["wifi_mac"] = mac
    row["bt_mac"] = bluetooth_mac(mac)
    row["push_ok"] = False
    if model:
        row["usb_uart"] = model
    if vid_pid and vid_pid != ":":
        row["usb_vid_pid"] = vid_pid
    if flash_mfr:
        row["flash_mfr"] = flash_mfr
    if row["name"] not in notes:
        if chip == "esp32":
            notes[row["name"]] = (
                f"mesh id {row['mesh_id']}, flash manufacturer {row.get('flash_mfr', '')}, learned by fleetd"
            )
        else:
            notes[row["name"]] = "learned by fleetd"
    catalog_path.write_text(
        "".join(json.dumps(item, separators=(",", ":")) + "\n" for item in rows),
        encoding="utf-8",
    )
    lines = list(header)
    if not lines:
        lines.append("# name\twifi_mac\tchip\tusb_path\tnotes\n")
    for item in rows:
        lines.append(
            "\t".join(
                (
                    item["name"],
                    item.get("wifi_mac", ""),
                    item.get("chip", ""),
                    item.get("usb_path", ""),
                    notes.get(item["name"], ""),
                )
            )
            + "\n"
        )
    boards_path.write_text("".join(lines), encoding="utf-8")


def main(argv: list[str]) -> int:
    if len(argv) == 5 and argv[1] == "name":
        row = attachment_row(_board_rows(Path(argv[2])), argv[3], argv[4])
        mac = serial_mac(argv[4])
        print(f"{row['name'] if row else ''}\t{mac}")
        return 0
    if len(argv) == 5 and argv[1] == "lookup":
        row = attachment_row(_load_catalog(Path(argv[2])), argv[3], argv[4])
        if row:
            flag = "1" if row.get("flash_ok") else "0"
            print(f"{row.get('type', '')}\t{row['name']}\t{flag}")
        return 0
    if len(argv) >= 2 and argv[1] == "parse":
        try:
            chip, mac, flash_mfr = parse_identity(sys.stdin.read())
        except ValueError as error:
            print(error, file=sys.stderr)
            return 1
        print(f"{chip}\t{mac}\t{flash_mfr}")
        return 0
    if len(argv) == 10 and argv[1] == "learn":
        learn(
            Path(argv[2]),
            Path(argv[3]),
            argv[4],
            argv[5],
            argv[6],
            argv[7],
            argv[8],
            argv[9],
        )
        return 0
    print(
        "usage: fleet_catalog.py parse"
        " | name BOARDS PATH SERIAL | lookup CATALOG PATH SERIAL"
        " | learn CATALOG BOARDS PATH MAC CHIP MFR MODEL VID",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
