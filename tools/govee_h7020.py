#!/usr/bin/env python3
"""Drive one Govee H7020 over BLE with the pinned ha-govee-led-ble codec.

Packets are built by references/protocols-ble/ha-govee-led-ble
(tag v7.14.0-rc.2.h7020). This file does not copy those frames.

Close Govee Home first. The control box accepts one BLE central.

Usage:
  .venv/bin/python tools/govee_h7020.py
  .venv/bin/python tools/govee_h7020.py identify
  .venv/bin/python tools/govee_h7020.py --strings 2 color 0 0 255
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
from dataclasses import replace
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
CODEC_ROOT = ROOT / "references" / "protocols-ble" / "ha-govee-led-ble"
DEFAULT_ADDRESS = "C5:38:30:33:3E:4A"
# The UB500 is the host seed. Its hci index moves; the combo radio stays blocked.
SEED_USB = ("2357", "0604")
MODEL = "H7020"
# ha-govee-led-ble device_profile keeps RGB only for these two pacts.
QUALIFIED_PACTS = {(1, 1), (2, 1)}

sys.path.insert(0, str(CODEC_ROOT))


def _load_codec() -> None:
    """Import packet builders without executing the Home Assistant package init."""
    import types

    pkg_dir = CODEC_ROOT / "custom_components" / "ha_govee_led_ble"
    if not (pkg_dir / "generated_protocol" / "__init__.py").is_file():
        print(
            "The H7020 codec has no generated_protocol/. Run ./tools/clone_references.sh",
            file=sys.stderr,
        )
        raise SystemExit(1)
    parent = types.ModuleType("custom_components")
    parent.__path__ = [str(CODEC_ROOT / "custom_components")]  # type: ignore[attr-defined]
    sys.modules["custom_components"] = parent
    pkg = types.ModuleType("custom_components.ha_govee_led_ble")
    pkg.__path__ = [str(pkg_dir)]  # type: ignore[attr-defined]
    pkg.__package__ = "custom_components.ha_govee_led_ble"
    sys.modules["custom_components.ha_govee_led_ble"] = pkg


try:
    _load_codec()
    from custom_components.ha_govee_led_ble.advertisement import (  # noqa: E402
        parse_govee_advertisement,
    )
    from custom_components.ha_govee_led_ble.const import device_profile  # noqa: E402
    from custom_components.ha_govee_led_ble.generated_protocol_adapter import (  # noqa: E402
        build_brightness,
        build_configured_strings_query,
        build_firmware_query,
        build_hardware_query,
        build_power,
        parse_status,
    )
    from custom_components.ha_govee_led_ble.light_commands import (  # noqa: E402
        build_color_rgb,
        build_segment_paint,
    )
    from custom_components.ha_govee_led_ble.native_scenes import build_native_scene_packets  # noqa: E402
    from custom_components.ha_govee_led_ble.scenes import (  # noqa: E402
        MODEL_SCENE_LABELS,
        OFFICIAL_MODEL_SCENES,
    )
    from custom_components.ha_govee_led_ble.transport import READ_UUID, WRITE_UUID  # noqa: E402
except ImportError as exc:
    print(
        "Cannot import the H7020 codec. Run ./tools/clone_references.sh and "
        ".venv/bin/pip install -r tools/requirements.txt",
        file=sys.stderr,
    )
    raise SystemExit(1) from exc


def seed_adapter() -> str:
    """Return the current hci name of the TP-Link UB500."""
    root = Path("/sys/class/bluetooth")
    vendor, product = SEED_USB
    if root.is_dir():
        for hci in sorted(root.glob("hci*")):
            vendor_path = hci / "device/../idVendor"
            product_path = hci / "device/../idProduct"
            try:
                if vendor_path.read_text().strip() == vendor and product_path.read_text().strip() == product:
                    return hci.name
            except OSError:
                continue
    return "hci0"


def profile_for(pact_type: int | None, pact_code: int | None):
    """Return the codec profile. Unqualified pacts keep power reads and drop RGB."""
    return device_profile(MODEL, pact_type, pact_code)


def sized_profile(profile, strings: int):
    """Match the color mask to one or two 15-bulb strings."""
    if strings not in (1, 2):
        raise ValueError("strings must be 1 or 2")
    bulbs = strings * profile.bulbs_per_string
    return replace(profile, segment_count=bulbs, whole_device_mask=(1 << bulbs) - 1)


def list_scenes() -> list[dict[str, Any]]:
    """Catalogue scenes the panel may apply. Parameter bytes stay in the codec."""
    scenes = OFFICIAL_MODEL_SCENES[MODEL]
    labels = MODEL_SCENE_LABELS[MODEL]
    listed: list[dict[str, Any]] = []
    for key, entry in scenes.items():
        speed = entry.speed
        listed.append(
            {
                "name": labels.get(key, entry.display_name),
                "category": entry.category,
                "speeds": speed.option_count if speed is not None else 0,
                "default_speed": speed.default_index if speed is not None else None,
            }
        )
    return listed


def scene_entry(name: str):
    """Return the catalogue label and entry for a display name."""
    if not isinstance(name, str) or not name.strip():
        raise ValueError("scene name is required")
    wanted = " ".join(name.split()).casefold()
    scenes = OFFICIAL_MODEL_SCENES[MODEL]
    labels = MODEL_SCENE_LABELS[MODEL]
    for key, entry in scenes.items():
        label = labels.get(key, entry.display_name)
        if key == wanted or " ".join(label.split()).casefold() == wanted:
            return label, entry
    raise ValueError(f"unknown scene {name}")


def scene_info(name: str) -> dict[str, Any]:
    """Name, category, and speed range. Raises ValueError for an unknown scene."""
    label, entry = scene_entry(name)
    speed = entry.speed
    return {
        "name": label,
        "category": entry.category,
        "speeds": speed.option_count if speed is not None else 0,
        "default_speed": speed.default_index if speed is not None else None,
    }


def build_scene_script(name: str, speed: int | None = None) -> tuple[dict[str, Any], list[bytes]]:
    """Ordered upload and activation packets for one catalogue scene."""
    if speed is not None and (isinstance(speed, bool) or not isinstance(speed, int)):
        raise ValueError("scene speed must be an integer")
    info = scene_info(name)
    _label, entry = scene_entry(info["name"])
    packets = build_native_scene_packets(MODEL, entry, speed_index=speed)
    if not packets:
        raise ValueError(f"scene {info['name']} produced no packets")
    resolved = info["default_speed"] if speed is None else speed
    return {"name": info["name"], "category": info["category"], "speed": resolved}, packets


def build_paint_script(groups: list[dict[str, Any]], profile) -> list[bytes]:
    """One packet per color group. Bulb numbers are 1-based and must not overlap."""
    if profile is None:
        raise ValueError("no colour profile")
    if not groups:
        raise ValueError("paint needs at least one group")
    painted: list[tuple[list[int], tuple[int, int, int]]] = []
    seen: set[int] = set()
    for group in groups:
        bulbs = list(group["bulbs"])
        rgb = group["rgb"]
        if len(rgb) != 3:
            raise ValueError("rgb needs three integers")
        color = (int(rgb[0]), int(rgb[1]), int(rgb[2]))
        for bulb in bulbs:
            if isinstance(bulb, bool) or not isinstance(bulb, int):
                raise ValueError("bulb numbers must be integers")
            if bulb in seen:
                raise ValueError(f"bulb {bulb} is in two groups")
            seen.add(bulb)
        painted.append((bulbs, color))
    return build_segment_paint(painted, MODEL, profile=profile)


def _status_fields(parsed: Any) -> dict[str, Any]:
    domain = getattr(parsed.domain, "name", str(parsed.domain))
    body = parsed.body
    fields: dict[str, Any] = {"domain": domain}
    for attr in ("text", "count", "is_on", "brightness_pct"):
        if hasattr(body, attr):
            value = getattr(body, attr)
            if isinstance(value, int) and attr == "is_on":
                value = bool(value)
            fields[attr] = value
    return fields


async def _scan(address: str, adapter: str, seconds: float) -> dict[str, Any]:
    from bleak import BleakScanner

    bluez = {"adapter": adapter} if adapter else {}
    print(f"Scanning {seconds:.0f}s on {adapter or 'default'} …", file=sys.stderr)
    devices = await BleakScanner.discover(timeout=seconds, return_adv=True, bluez=bluez)
    wanted = address.upper()
    for device, adv in devices.values():
        if device.address.upper() != wanted:
            continue
        return {
            "name": device.name or adv.local_name,
            "rssi": adv.rssi,
            "manufacturer_data": {
                int(company): bytes(payload) for company, payload in (adv.manufacturer_data or {}).items()
            },
        }
    return {}


async def _query(client, queue: asyncio.Queue[bytes], packet: bytes, profile, domain: str) -> dict[str, Any] | None:
    while not queue.empty():
        queue.get_nowait()
    await client.write_gatt_char(WRITE_UUID, packet, response=False)
    loop = asyncio.get_running_loop()
    deadline = loop.time() + 4.0
    while loop.time() < deadline:
        try:
            frame = await asyncio.wait_for(queue.get(), deadline - loop.time())
        except TimeoutError:
            return None
        parsed = parse_status(frame, MODEL, profile=profile)
        if parsed is None:
            continue
        fields = _status_fields(parsed)
        if fields["domain"] == domain:
            fields["raw"] = frame.hex()
            return fields
    return None


async def _write(client, packet: bytes) -> None:
    await client.write_gatt_char(WRITE_UUID, packet, response=False)
    await asyncio.sleep(0.2)


async def run(args: argparse.Namespace) -> int:
    seen = await _scan(args.address, args.adapter, args.scan_seconds)
    manufacturer = seen.get("manufacturer_data") or {}
    advert = parse_govee_advertisement(manufacturer)
    pact_type = advert.pact_type if advert else None
    pact_code = advert.pact_code if advert else None
    qualified = (pact_type, pact_code) in QUALIFIED_PACTS and not (advert and advert.supports_encryption)

    identity: dict[str, Any] = {
        "address": args.address,
        "name": seen.get("name"),
        "rssi": seen.get("rssi"),
        "manufacturer_data": {str(k): v.hex() for k, v in manufacturer.items()},
        "pact_type": pact_type,
        "pact_code": pact_code,
        "supports_encryption": bool(advert.supports_encryption) if advert else None,
        "qualified_for_color": qualified,
        "write_uuid": WRITE_UUID,
        "notify_uuid": READ_UUID,
    }
    if advert and advert.supports_encryption:
        identity["stopped"] = "advertisement says the link is encrypted"
        print(json.dumps(identity, indent=2))
        print(identity["stopped"], file=sys.stderr)
        return 2

    from bleak import BleakClient

    bluez = {"adapter": args.adapter} if args.adapter else {}
    queue: asyncio.Queue[bytes] = asyncio.Queue()

    def on_notify(_sender: Any, data: bytearray) -> None:
        queue.put_nowait(bytes(data))

    sent: list[dict[str, str]] = []
    try:
        async with BleakClient(args.address, timeout=args.timeout, bluez=bluez) as client:
            await client.start_notify(READ_UUID, on_notify)
            base = profile_for(pact_type, pact_code)
            queries = [
                ("firmware", build_firmware_query(MODEL, profile=base), "fw_version"),
                ("hardware", build_hardware_query(MODEL, profile=base), "hw_version"),
            ]
            if base.configured_string_counts:
                queries.append(("strings", build_configured_strings_query(base), "configured_strings"))
            for label, packet, domain in queries:
                identity[label] = await _query(client, queue, packet, base, domain)
            if not qualified or args.command == "identify":
                if not qualified:
                    identity["stopped"] = (
                        "advertisement did not include a pact"
                        if advert is None
                        else f"pact {pact_type}/{pact_code} is outside the codec's color profiles"
                    )
                await client.stop_notify(READ_UUID)
                print(json.dumps(identity, indent=2))
                if identity.get("stopped"):
                    print(identity["stopped"], file=sys.stderr)
                return 0 if qualified else 2

            reported = (identity.get("strings") or {}).get("count")
            strings = args.strings if args.strings is not None else reported or 1
            identity["strings_used"] = strings
            active = sized_profile(base, strings)

            async def send(name: str, packet: bytes) -> None:
                await _write(client, packet)
                sent.append({"name": name, "hex": packet.hex()})

            if args.command in {"on", "demo"}:
                await send("power_on", build_power(True, MODEL, profile=active))
            if args.command in {"brightness", "demo"}:
                await send("brightness", build_brightness(args.brightness, MODEL, profile=active))
            if args.command in {"color", "demo"}:
                red, green, blue = (255, 0, 0) if args.command == "demo" else tuple(args.rgb)
                await send("color", build_color_rgb(red, green, blue, MODEL, profile=active))
            if args.command == "demo":
                print(
                    f"Red for {args.hold_seconds:.0f}s on {strings} string(s), then off.",
                    file=sys.stderr,
                )
                await asyncio.sleep(args.hold_seconds)
            if args.command in {"off", "demo"}:
                await send("power_off", build_power(False, MODEL, profile=active))
            await client.stop_notify(READ_UUID)
    except Exception as exc:  # noqa: BLE001 — report the adapter or busy-link failure
        identity["connect_error"] = f"{type(exc).__name__}: {exc}"
        identity["sent"] = sent
        print(json.dumps(identity, indent=2))
        print(
            "Connect failed. Close Govee Home so this host can be the only central.",
            file=sys.stderr,
        )
        return 1

    identity["sent"] = sent
    print(json.dumps(identity, indent=2))
    return 0


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--address", default=DEFAULT_ADDRESS, help="BLE address")
    parser.add_argument("--adapter", help="BlueZ adapter. Default: the TP-Link UB500, whichever hci index it has")
    parser.add_argument("--scan-seconds", type=float, default=12.0)
    parser.add_argument("--timeout", type=float, default=20.0, help="GATT connect timeout")
    parser.add_argument("--strings", type=int, choices=(1, 2), help="15-bulb strings. Default: device report, else 1")
    parser.add_argument("--brightness", type=int, default=100, help="1..100, used by brightness and demo")
    parser.add_argument("--hold-seconds", type=float, default=3.0, help="How long demo stays red")
    parser.add_argument(
        "command",
        nargs="?",
        default="demo",
        choices=("demo", "identify", "on", "off", "brightness", "color"),
    )
    parser.add_argument("rgb", nargs="*", type=int, help="With color: R G B, each 0..255")
    args = parser.parse_args()
    if args.command == "color":
        if len(args.rgb) != 3 or any(not 0 <= part <= 255 for part in args.rgb):
            parser.error("color needs three integers R G B in 0..255")
    if not 1 <= args.brightness <= 100:
        parser.error("brightness must be 1..100")
    if args.adapter is None:
        args.adapter = seed_adapter()
    if not CODEC_ROOT.is_dir():
        print(f"Missing codec checkout: {CODEC_ROOT}", file=sys.stderr)
        raise SystemExit(1)
    raise SystemExit(asyncio.run(run(args)))


if __name__ == "__main__":
    main()
