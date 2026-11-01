#!/usr/bin/env python3
"""Ask one node what it is and what it hears.

Connects to the node's ESPHome API on tallow-iot with the noise key, prints
its identity and bluetooth-proxy flags, then counts BLE advertisements for a
while. Prints the node's Wi-Fi signal to the AP when its build reports one.
Says whether the H7020 was among them. Sends nothing to any light.

  .venv/bin/python tools/node_check.py --address 10.87.0.22
  .venv/bin/python tools/node_check.py --address 10.87.0.21 --seconds 30

The key is the api_key line of firmware/esphome/secrets.yaml unless --key-file
names a file that holds only the key.
"""

from __future__ import annotations

import argparse
import asyncio
import ipaddress
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))

import govee_h7020 as light  # noqa: E402

SECRETS_PATH = ROOT / "firmware" / "esphome" / "secrets.yaml"
IOT_NET = ipaddress.ip_network("10.87.0.0/24")
API_KEY = re.compile(r"""^api_key:\s*["']?([A-Za-z0-9+/=]+)["']?\s*$""", re.MULTILINE)


def read_key(path: Path, whole_file: bool) -> str:
    text = path.read_text(encoding="utf-8")
    if whole_file:
        return text.strip()
    found = API_KEY.search(text)
    if found is None:
        raise SystemExit(f"No api_key line in {path}. Run: tools/esphome.sh secrets")
    return found.group(1)


def mac_text(value: int) -> str:
    return ":".join(f"{(value >> shift) & 0xFF:02X}" for shift in range(40, -8, -8))


async def check(address: str, port: int, key: str, seconds: float, target: str) -> int:
    from aioesphomeapi import APIClient, APIConnectionError, BluetoothProxyFeature, SensorInfo, SensorState

    client = APIClient(address, port, None, noise_psk=key, client_info="tallow node_check")
    try:
        await asyncio.wait_for(client.connect(login=True), 15)
    except (APIConnectionError, OSError, TimeoutError) as exc:
        print(f"No answer from {address}:{port} ({type(exc).__name__}: {exc}).", file=sys.stderr)
        return 1
    try:
        info = await client.device_info()
        flags = info.bluetooth_proxy_feature_flags_compat(client.api_version)
        named = [flag.name for flag in BluetoothProxyFeature if flags & flag] or ["none"]
        print(f"Node {info.name} ESPHome {info.esphome_version} built {info.compilation_time}")
        print(f"Wi-Fi {info.mac_address} BLE {info.bluetooth_mac_address or '-'}")
        print("Proxy " + ", ".join(named))
        if not flags:
            print("This node runs no BLE proxy.", file=sys.stderr)
            return 1

        # The node package's wifi_signal sensor. Older builds have none.
        entities, _ = await client.list_entities_services()
        signal_keys = {
            entity.key
            for entity in entities
            if isinstance(entity, SensorInfo)
            and entity.device_class == "signal_strength"
            and entity.unit_of_measurement == "dBm"
        }
        signal: list[float] = []

        def on_state(state) -> None:
            if isinstance(state, SensorState) and state.key in signal_keys and not state.missing_state:
                signal.append(state.state)

        heard: dict[str, int] = {}

        def on_adverts(batch) -> None:
            for advert in batch.advertisements:
                heard[mac_text(advert.address)] = advert.rssi

        if signal_keys:
            client.subscribe_states(on_state)
        stop = client.subscribe_bluetooth_le_raw_advertisements(on_adverts)
        await asyncio.sleep(seconds)
        stop()
        if not signal_keys:
            print("Wi-Fi signal not reported by this build.")
        elif signal:
            print(f"Wi-Fi signal {signal[-1]:.0f} dBm to the AP.")
        else:
            print("Wi-Fi signal not measured yet (once a minute after boot).")
        print(f"Heard {len(heard)} BLE addresses in {seconds:.0f} s.")
        if target in heard:
            print(f"H7020 {target} at {heard[target]} dBm.")
        else:
            print(f"H7020 {target} not heard.")
        return 0 if heard else 1
    finally:
        await client.disconnect()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--address", required=True, help="Node address on tallow-iot, e.g. 10.87.0.20")
    parser.add_argument("--port", type=int, default=6053)
    parser.add_argument("--seconds", type=float, default=15.0)
    parser.add_argument("--key-file", type=Path, help="File holding only the API key")
    parser.add_argument("--target", default=light.DEFAULT_ADDRESS, help="BLE address to look for")
    args = parser.parse_args()
    if ipaddress.ip_address(args.address) not in IOT_NET:
        raise SystemExit(f"Nodes live on {IOT_NET}.")
    key = read_key(args.key_file or SECRETS_PATH, whole_file=args.key_file is not None)
    sys.exit(asyncio.run(check(args.address, args.port, key, args.seconds, args.target.upper())))


if __name__ == "__main__":
    main()
