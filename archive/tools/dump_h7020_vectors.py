#!/usr/bin/env python3
"""Dump every H7020 packet the LAN panel can send, as golden vectors for the Rust codec.

Packets come from tools/govee_h7020.py, which wraps the pinned codec in
references/protocols-ble/ha-govee-led-ble. This script opens no Bluetooth
connection, no socket, and no serial port. Its output has no clock and no
randomness: two runs write identical bytes.

Writes two files:
  host/tallow-govee/tests/vectors.json  inputs and expected packet or reply bytes
  host/tallow-govee/scenes.json         the scene table the builders read

Usage:
  .venv/bin/python tools/dump_h7020_vectors.py
  .venv/bin/python tools/dump_h7020_vectors.py --vectors OUT.json --scenes OUT2.json
"""

from __future__ import annotations

import argparse
import functools
import json
import operator
import subprocess
import sys
from pathlib import Path
from typing import Any, Callable

import govee_h7020 as light
from custom_components.ha_govee_led_ble.generated_protocol_adapter import (
    build_brightness_query,
    build_power_query,
)

ROOT = Path(__file__).resolve().parents[1]
CODEC_ROOT = light.CODEC_ROOT
DEFAULT_VECTORS = ROOT / "host" / "tallow-govee" / "tests" / "vectors.json"
DEFAULT_SCENES = ROOT / "host" / "tallow-govee" / "scenes.json"
STRING_COUNTS = (1, 2)
BASE = light.profile_for(1, 1)  # the qualified pact; status and query grammar only

COLOURS: list[tuple[int, int, int]] = [
    (0, 0, 0),
    (255, 255, 255),
    (255, 0, 0),
    (0, 255, 0),
    (0, 0, 255),
    (1, 2, 3),
    (10, 20, 30),
    (128, 64, 32),
    (255, 128, 0),
]


def _outcome(fn: Callable[[], Any]) -> dict[str, Any]:
    """Run a builder; a refusal is recorded as the error the Python raised."""
    try:
        return {"ok": fn()}
    except (ValueError, TypeError, KeyError) as exc:
        return {"error": f"{type(exc).__name__}: {exc}"}


def _hex(packets: list[bytes]) -> list[str]:
    return [packet.hex() for packet in packets]


def _sized(strings: int):
    return light.sized_profile(BASE, strings)


def _query_packets(profile) -> list[tuple[str, bytes]]:
    return [
        ("firmware", light.build_firmware_query(light.MODEL, profile=profile)),
        ("hardware", light.build_hardware_query(light.MODEL, profile=profile)),
        ("configured_strings", light.build_configured_strings_query(profile)),
        ("power", build_power_query(light.MODEL, profile=profile)),
        ("brightness", build_brightness_query(light.MODEL, profile=profile)),
    ]


def _query_vectors() -> list[dict[str, Any]]:
    """Pact 1/1, then the same five queries built from pact 2/1."""
    out = [
        {"id": f"query/{name}", "kind": "query", "input": {"query": name}, "strings": None,
         "result": {"packets": [packet.hex()]}}
        for name, packet in _query_packets(BASE)
    ]
    pact = light.profile_for(2, 1)
    out.extend(
        {"id": f"query/{name}/pact-2-1", "kind": "query",
         "input": {"query": name, "pact": [2, 1]}, "strings": None,
         "result": {"packets": [packet.hex()]}}
        for name, packet in _query_packets(pact)
    )
    return out


def _power_vectors() -> list[dict[str, Any]]:
    out = []
    for strings in STRING_COUNTS:
        for on in (True, False):
            packet = light.build_power(on, light.MODEL, profile=_sized(strings))
            out.append({"id": f"power/{'on' if on else 'off'}/{strings}s", "kind": "power",
                        "input": {"on": on}, "strings": strings,
                        "result": {"packets": [packet.hex()]}})
    return out


def _brightness_vectors() -> list[dict[str, Any]]:
    out = []
    for strings in STRING_COUNTS:
        for level in range(1, 101):
            packet = light.build_brightness(level, light.MODEL, profile=_sized(strings))
            out.append({"id": f"brightness/{level:03d}/{strings}s", "kind": "brightness",
                        "input": {"level": level}, "strings": strings,
                        "result": {"packets": [packet.hex()]}})
    return out


def _colour_vectors() -> list[dict[str, Any]]:
    out = []
    for strings in STRING_COUNTS:
        for red, green, blue in COLOURS:
            packet = light.build_color_rgb(red, green, blue, light.MODEL, profile=_sized(strings))
            out.append({"id": f"color/{red:03d}-{green:03d}-{blue:03d}/{strings}s", "kind": "color",
                        "input": {"r": red, "g": green, "b": blue}, "strings": strings,
                        "result": {"packets": [packet.hex()]}})
    return out


def _paint_cases(bulbs: int) -> list[tuple[str, list[dict[str, Any]]]]:
    half = bulbs // 2
    return [
        ("all-blue", [{"bulbs": list(range(1, bulbs + 1)), "rgb": [0, 0, 255]}]),
        ("split-red-blue", [
            {"bulbs": list(range(1, half + 1)), "rgb": [255, 0, 0]},
            {"bulbs": list(range(half + 1, bulbs + 1)), "rgb": [0, 0, 255]},
        ]),
        ("boundary-pair", [{"bulbs": [half, half + 1], "rgb": [10, 20, 30]}]),
        ("ends", [
            {"bulbs": [1], "rgb": [255, 255, 255]},
            {"bulbs": [bulbs], "rgb": [1, 2, 3]},
        ]),
        ("refused-overlap", [
            {"bulbs": [1, 2], "rgb": [255, 0, 0]},
            {"bulbs": [2, 3], "rgb": [0, 255, 0]},
        ]),
        ("refused-past-last-bulb", [{"bulbs": [bulbs + 1], "rgb": [255, 0, 0]}]),
        ("refused-bulb-zero", [{"bulbs": [0], "rgb": [255, 0, 0]}]),
        ("refused-empty", []),
    ]


def _paint_vectors() -> list[dict[str, Any]]:
    out = []
    for strings in STRING_COUNTS:
        profile = _sized(strings)
        for name, groups in _paint_cases(profile.segment_count):
            result = _outcome(lambda groups=groups: _hex(light.build_paint_script(groups, profile)))
            wrapped = {"packets": result["ok"]} if "ok" in result else result
            out.append({"id": f"paint/{name}/{strings}s", "kind": "paint",
                        "input": {"groups": groups}, "strings": strings, "result": wrapped})
    return out


def _scene_record(key: str, entry: Any, label: str) -> dict[str, Any]:
    speed = entry.speed
    record: dict[str, Any] = {
        "key": key,
        "name": label,
        "category": entry.category,
        "speeds": speed.option_count if speed is not None else 0,
        "default_speed": speed.default_index if speed is not None else None,
        "code": entry.code,
        "scene_type": entry.scene_type,
        "selector_only": entry.selector_only,
        "music_code": entry.music_code,
        "param": entry.param,
        "speed": None,
    }
    if speed is not None:
        record["speed"] = {
            "default_index": speed.default_index,
            "option_count": speed.option_count,
            "pages": [
                {
                    "page": page.page,
                    "move_in": list(page.move_in),
                    "move_all": list(page.move_all),
                    "colour_speed": list(page.colour_speed),
                    "brightness": [
                        {"block": block.block, "values": list(block.values)}
                        for block in page.brightness_speeds
                    ],
                }
                for page in speed.pages
            ],
        }
    return record


def _scene_table() -> list[dict[str, Any]]:
    catalogue = light.OFFICIAL_MODEL_SCENES[light.MODEL]
    labels = light.MODEL_SCENE_LABELS[light.MODEL]
    return [_scene_record(key, entry, labels.get(key, entry.display_name)) for key, entry in catalogue.items()]


def _scene_vectors() -> tuple[list[dict[str, Any]], dict[str, int]]:
    out: list[dict[str, Any]] = []
    counts = {"scenes": 0, "speed_scripts": 0, "plain_scripts": 0, "refused": 0}
    for info in light.list_scenes():
        name = info["name"]
        counts["scenes"] += 1
        if light.scene_entry(name)[0] != name:
            raise SystemExit(f"scene label does not round-trip: {name}")
        if info["speeds"]:
            options: list[int | None] = list(range(info["speeds"]))
            default = info["default_speed"]
            # speed=None must resolve to the default index, byte for byte.
            _, by_none = light.build_scene_script(name, None)
            _, by_default = light.build_scene_script(name, default)
            if by_none != by_default:
                raise SystemExit(f"scene {name}: speed None does not equal default index {default}")
        else:
            options = [None]
        for speed in options:
            if speed is None:
                counts["plain_scripts"] += 1
            else:
                counts["speed_scripts"] += 1
            info_out, packets = light.build_scene_script(name, speed)
            out.append({
                "id": f"scene/{name}/speed-{'none' if speed is None else speed}",
                "kind": "scene",
                "input": {"name": name, "speed": speed},
                "strings": None,
                "result": {
                    "name": info_out["name"],
                    "category": info_out["category"],
                    "speed": info_out["speed"],
                    "packets": _hex(packets),
                },
            })
    speedy = next(i for i in light.list_scenes() if i["speeds"])
    plain = next(i for i in light.list_scenes() if not i["speeds"])
    refused = {
        "refused-unknown-scene": ("No Such Scene", None),
        "refused-speed-past-last-option": (speedy["name"], speedy["speeds"]),
        "refused-speed-on-plain-scene": (plain["name"], 0),
    }
    for case_id, (name, speed) in refused.items():
        result = _outcome(lambda name=name, speed=speed: _hex(light.build_scene_script(name, speed)[1]))
        if "ok" in result:
            raise SystemExit(f"scene {case_id} was accepted; expected a refusal")
        counts["refused"] += 1
        out.append({
            "id": f"scene/{case_id}",
            "kind": "scene",
            "input": {"name": name, "speed": speed},
            "strings": None,
            "result": result,
        })
    # Sharp s casefolds to "ss", which `str.lower` does not. The catalogue key is "cherry blossoms".
    folded = "Cherry bloßoms"
    if light.scene_entry(folded)[0] != "Cherry blossoms":
        raise SystemExit(f"scene casefold did not resolve {folded!r}")
    info_out, packets = light.build_scene_script(folded, None)
    counts["plain_scripts"] += 1
    out.append({
        "id": "scene/casefold-sharp-s",
        "kind": "scene",
        "input": {"name": folded, "speed": None},
        "strings": None,
        "result": {
            "name": info_out["name"],
            "category": info_out["category"],
            "speed": info_out["speed"],
            "packets": _hex(packets),
        },
    })
    return out, counts


def _with_checksum(raw: bytes) -> bytes:
    """Append the XOR of every byte before it, as the codec's frames do."""
    return raw + bytes([functools.reduce(operator.xor, raw)])


def _frame(domain: int, body: bytes) -> bytes:
    """A 20-byte status reply: 0xaa, domain, 17 body bytes, XOR of the 19 before it."""
    return _with_checksum(bytes([0xAA, domain]) + body.ljust(17, b"\x00"))


def _colour_fields(parsed: Any) -> dict[str, Any]:
    """The colour-mode body `parse_status` read. `_status_fields` drops it."""
    mode_name = getattr(parsed.body.mode, "name", None)
    if mode_name not in {"static", "scene", "diy", "music"}:
        raise SystemExit(f"colour-mode parse has no named mode: {parsed.body.mode!r}")
    detail = parsed.body.mode_body
    fields: dict[str, Any] = {"domain": "colormode", "mode": mode_name}
    if mode_name == "static":
        fields["sub"] = int(detail.sub)
        fields["kelvin"] = int(detail.kelvin)
    elif mode_name == "scene":
        fields["scene_id"] = int(detail.scene_id)
    elif mode_name == "diy":
        fields["code"] = int(detail.code)
    else:
        fields["mode_id"] = int(detail.mode_id)
        fields["sensitivity"] = int(detail.sensitivity)
        if fields["mode_id"] == 4:
            fields["selector"] = int(detail.unknown_selector_parameter)
        fields["has_fixed_colour"] = int(detail.has_fixed_colour)
        if detail.has_fixed_colour:
            fields["rgb"] = [int(detail.rgb.red), int(detail.rgb.green), int(detail.rgb.blue)]
    return fields


def _status_vector(case_id: str, reply: bytes, source: str, fields: dict[str, Any] | None) -> dict[str, Any]:
    return {
        "id": f"status/{case_id}",
        "kind": "status",
        "input": {"reply": reply.hex(), "source": source},
        "strings": None,
        "result": {"fields": fields},
    }


def _colour_mode_vectors() -> list[dict[str, Any]]:
    """One vector per colour-mode layout the H7020 schema parses."""
    def body(mode: int, rest: bytes) -> bytes:
        return _frame(0x05, bytes([mode]) + rest)

    def rgb(mode_id: int, sensitivity: int, colour: tuple[int, int, int] | None, selector: int | None = None) -> bytes:
        raw = bytes([mode_id, sensitivity])
        if selector is not None:
            raw += bytes([selector])
        if colour is None:
            raw += bytes([0])
        else:
            raw += bytes([1, *colour])
        return raw

    # (id, frame, values the parser must read back)
    built: list[tuple[str, bytes, dict[str, Any]]] = [
        ("colormode-static-2700", body(0x15, bytes([0]) + (2700).to_bytes(2, "big")),
         {"mode": "static", "sub": 0, "kelvin": 2700}),
        ("colormode-static-0", body(0x15, bytes([1]) + (0).to_bytes(2, "big")),
         {"mode": "static", "sub": 1, "kelvin": 0}),
        ("colormode-scene-2092", body(0x04, (2092).to_bytes(2, "little")),
         {"mode": "scene", "scene_id": 2092}),
        ("colormode-diy", body(0x0A, (7).to_bytes(2, "little")),
         {"mode": "diy", "code": 7}),
        ("colormode-music-3-colour", body(0x13, rgb(3, 40, (9, 8, 7))),
         {"mode": "music", "mode_id": 3, "sensitivity": 40, "has_fixed_colour": 1, "rgb": [9, 8, 7]}),
        ("colormode-music-5-colour", body(0x13, rgb(5, 40, (9, 8, 7))),
         {"mode": "music", "mode_id": 5, "sensitivity": 40, "has_fixed_colour": 1, "rgb": [9, 8, 7]}),
        ("colormode-music-6-colour", body(0x13, rgb(6, 40, (9, 8, 7))),
         {"mode": "music", "mode_id": 6, "sensitivity": 40, "has_fixed_colour": 1, "rgb": [9, 8, 7]}),
        ("colormode-music-3", body(0x13, rgb(3, 10, None)),
         {"mode": "music", "mode_id": 3, "sensitivity": 10, "has_fixed_colour": 0}),
        ("colormode-music-4-colour", body(0x13, rgb(4, 40, (9, 8, 7), selector=0)),
         {"mode": "music", "mode_id": 4, "sensitivity": 40, "selector": 0,
          "has_fixed_colour": 1, "rgb": [9, 8, 7]}),
        ("colormode-music-4", body(0x13, rgb(4, 10, None, selector=0)),
         {"mode": "music", "mode_id": 4, "sensitivity": 10, "selector": 0, "has_fixed_colour": 0}),
    ]
    out = []
    for case_id, reply, expect in built:
        parsed = light.parse_status(reply, light.MODEL, profile=BASE)
        if parsed is None:
            raise SystemExit(f"status {case_id}: codec did not parse the colour-mode body")
        fields = _colour_fields(parsed)
        for key, value in expect.items():
            if fields.get(key) != value:
                raise SystemExit(f"status {case_id}: codec gave {fields!r}, expected {key}={value!r}")
        out.append(_status_vector(case_id, reply, "constructed per h7020_status_reply.ksy", fields))
    return out


def _unknown_domain_vector() -> dict[str, Any]:
    """A domain the schema does not name. The body is raw bytes, so this does not use `_status_fields`."""
    reply = _frame(0x99, b"")
    parsed = light.parse_status(reply, light.MODEL, profile=BASE)
    if parsed is None or getattr(parsed.domain, "name", None) is not None or int(parsed.domain) != 0x99:
        raise SystemExit(f"unknown domain did not parse as 153: {parsed!r}")
    fields = {"domain": str(int(parsed.domain))}
    return _status_vector("unknown-domain", reply, "constructed; domain 0x99 has no name", fields)


def _status_vectors() -> list[dict[str, Any]]:
    # Constructed replies follow tools/ble/kaitai/speculative/h7020_status_reply.ksy.
    # Expected fields are written here by hand and checked against the codec's parse.
    fixture_count = _with_checksum(bytes([0xAA, 0x0F, 0x02]) + bytes(range(16)))
    bad_checksum = bytearray(_frame(0x01, b"\x01"))
    bad_checksum[-1] ^= 0x01
    cases: list[tuple[str, bytes, str, dict[str, Any] | None]] = [
        ("power-on", _frame(0x01, b"\x01"), "constructed per ksy", {"domain": "power", "is_on": True}),
        ("power-off", _frame(0x01, b"\x00"), "constructed per ksy", {"domain": "power", "is_on": False}),
        ("brightness-1", _frame(0x04, b"\x01"), "constructed per ksy", {"domain": "brightness", "brightness_pct": 1}),
        ("brightness-100", _frame(0x04, b"\x64"), "constructed per ksy", {"domain": "brightness", "brightness_pct": 100}),
        ("firmware-text", _frame(0x06, b"1.02.03\x00"), "constructed per ksy", {"domain": "fw_version", "text": "1.02.03"}),
        ("hardware-text", _frame(0x07, b"\x03" + b"2.00\x00"), "constructed per ksy", {"domain": "hw_version", "text": "2.00"}),
        ("strings-1", _frame(0x0F, b"\x01"), "constructed per ksy", {"domain": "configured_strings", "count": 1}),
        ("strings-2-codec-fixture", fixture_count, "codec test_kaitai_protocol.py configured_strings", {"domain": "configured_strings", "count": 2}),
        ("segments-codec-fixture", _with_checksum(bytes.fromhex("aaa50a250102032604050627070809aabbccdd")),
         "codec test_kaitai_protocol.py segments", {"domain": "segments"}),
        ("power-off-memory-codec-fixture", bytes.fromhex("aa410100000000000000000000000000000000ea"),
         "codec test_kaitai_protocol.py power_off_memory", {"domain": "power_off_memory"}),
        ("refused-bad-checksum", bytes(bad_checksum), "constructed; checksum flipped", None),
        ("refused-command-ack", bytes.fromhex("3341000000000000000000000000000000000072"),
         "codec test_kaitai_protocol.py command ack", None),
    ]
    out = []
    for case_id, reply, source, expected in cases:
        fields = None
        parsed = light.parse_status(reply, light.MODEL, profile=BASE)
        if parsed is not None:
            fields = light._status_fields(parsed)
        if fields != expected:
            raise SystemExit(f"status {case_id}: codec gave {fields!r}, expected {expected!r}")
        out.append(_status_vector(case_id, reply, source, fields))
    out.extend(_colour_mode_vectors())
    out.append(_unknown_domain_vector())
    return out


def _codec_commit() -> str | None:
    try:
        done = subprocess.run(
            ["git", "-C", str(CODEC_ROOT), "rev-parse", "HEAD"],
            capture_output=True, text=True, check=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return None
    return done.stdout.strip()


def _dump(path: Path, data: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2, sort_keys=True, ensure_ascii=True) + "\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--vectors", type=Path, default=DEFAULT_VECTORS, help="where vectors.json goes")
    parser.add_argument("--scenes", type=Path, default=DEFAULT_SCENES, help="where scenes.json goes")
    args = parser.parse_args()

    scene_vectors, scene_counts = _scene_vectors()
    vectors = [
        *_query_vectors(),
        *_power_vectors(),
        *_brightness_vectors(),
        *_colour_vectors(),
        *_paint_vectors(),
        *scene_vectors,
        *_status_vectors(),
    ]
    kinds: dict[str, int] = {}
    for vector in vectors:
        kinds[vector["kind"]] = kinds.get(vector["kind"], 0) + 1
    commit = _codec_commit()
    vectors_doc = {
        "schema": 1,
        "model": light.MODEL,
        "codec": "references/protocols-ble/ha-govee-led-ble",
        "codec_commit": commit,
        "string_counts": list(STRING_COUNTS),
        "counts": {"total": len(vectors), **dict(sorted(kinds.items()))},
        "vectors": vectors,
    }
    scenes_doc = {
        "schema": 1,
        "model": light.MODEL,
        "codec_commit": commit,
        "scenes": _scene_table(),
    }
    _dump(args.vectors, vectors_doc)
    _dump(args.scenes, scenes_doc)
    print(
        f"vectors={len(vectors)} kinds={json.dumps(dict(sorted(kinds.items())))} "
        f"scenes={scene_counts['scenes']} speed_scripts={scene_counts['speed_scripts']} "
        f"plain_scripts={scene_counts['plain_scripts']} refused_scene_cases={scene_counts['refused']}",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
