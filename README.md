# tallow

Cheap light, kept local.

The strings and bulbs already in the house, driven over BLE and Wi-Fi from one place on the LAN. Vendor apps stay outside.

Device-specific inventory and the production web app are **later phases**. This repo is the bench: reference clones, reverse-engineering playbooks, hardware guidance, and discovery scripts.

## Quick start

```bash
# Pull curated upstream references (gitignored; re-run on fresh clones)
./tools/clone_references.sh

# Tooling venv (Arch/PEP 668: do not pip install system-wide)
python -m venv .venv
.venv/bin/pip install -r tools/requirements.txt

# REA host toolchain (JDK 21, Ghidra, JADX JAR, unblob, Frida, …) — Arch
./tools/install_re_toolchain.sh
# Then restart Cursor so .cursor/mcp.json (REA MCP) loads.

# BLE discovery (needs BlueZ + adapter permissions)
.venv/bin/python tools/ble_discover.py --scan-seconds 15 --lights-only
.venv/bin/python tools/ble_discover.py --connect-gatt --address AA:BB:CC:DD:EE:FF -o discovery.json

# Parse an Android HCI snoop for ATT writes
.venv/bin/python tools/parse_btsnoop.py path/to/btsnoop_hci.log --hex
```


## Repository map

| Path | Purpose |
| --- | --- |
| [docs/01-bluetooth-server-hardware-and-proxies.md](docs/01-bluetooth-server-hardware-and-proxies.md) | USB adapters vs ESP32 BLE proxies for a LAN web app |
| [docs/02-reverse-engineering-playbook.md](docs/02-reverse-engineering-playbook.md) | REA (JADX/Ghidra/ADB/Frida/tshark), btsnoop, btmon workflow |
| [docs/03-known-light-protocols-matrix.md](docs/03-known-light-protocols-matrix.md) | Fingerprints for ELK-BLEDOM, Tuya, MagicHome, Govee, … |
| [docs/04-firmware-liberation-and-network-isolation.md](docs/04-firmware-liberation-and-network-isolation.md) | IoT VLAN + CloudCutter / OpenBeken |
| [docs/05-target-webapp-architecture.md](docs/05-target-webapp-architecture.md) | FastAPI + habluetooth + driver plugin blueprint |
| [AGENTS.md](AGENTS.md) | Instructions for future AI coding agents |
| [.cursor/mcp.json](.cursor/mcp.json) | Project-scoped REA MCP registration for Cursor |
| [tools/](tools/) | Clone / RE toolchain scripts + discovery / snoop parsers |
| `references/` | Shallow clones (not committed; see catalog below) |

## Hardware stance (summary)

For “open a web app from any device on my network”:

1. Prefer **ESP32 Ethernet/PoE Bluetooth proxies** near the lights.
2. Keep one **CSR8510 Class 1 USB adapter** (Feasycom FSC-BP119 / Sena UD100) on the server for nearby devices and RE work.
3. Put Wi-Fi lights on a **no-WAN IoT VLAN**; only the controller may talk to them.

Details: [docs/01](docs/01-bluetooth-server-hardware-and-proxies.md).

## Reference catalog

Shallow-cloned by `./tools/clone_references.sh` into `references/`:

### BLE stack (`references/ble-stack/`)

| Repo | Role |
| --- | --- |
| [hbldh/bleak](https://github.com/hbldh/bleak) | Async Python BLE client (BlueZ) |
| [google/bumble](https://github.com/google/bumble) | Full Python BT stack; raw ATT/HCI |
| [Bluetooth-Devices/habluetooth](https://github.com/Bluetooth-Devices/habluetooth) | Multi-adapter / proxy HA Bluetooth manager |
| [Bluetooth-Devices/bleak-esphome](https://github.com/Bluetooth-Devices/bleak-esphome) | bleak over ESPHome Bluetooth proxy |

### BLE light protocols (`references/protocols-ble/`)

| Repo | Role |
| --- | --- |
| [Bluetooth-Devices/led-ble](https://github.com/Bluetooth-Devices/led-ble) | Triones / LEDBLE / HappyLighting family |
| [dave-code-ruiz/elkbledom](https://github.com/dave-code-ruiz/elkbledom) | ELK-BLEDOM / Lotus Lantern / MELK |
| [8none1/lednetwf_ble](https://github.com/8none1/lednetwf_ble) | Zengge LEDnetWF |
| [rabidpaperclip/magichome2-ble](https://github.com/rabidpaperclip/magichome2-ble) | MagicHome2 / Surplife BLE |

### Wi-Fi light protocols (`references/protocols-wifi/`)

| Repo | Role |
| --- | --- |
| [jasonacox/tinytuya](https://github.com/jasonacox/tinytuya) | Tuya local LAN (+ BLE helpers) |
| [python-kasa/python-kasa](https://github.com/python-kasa/python-kasa) | TP-Link Kasa / Tapo |
| [Danielhiversen/flux_led](https://github.com/Danielhiversen/flux_led) | MagicHome / Flux LED (TCP 5577) |
| [wez/govee2mqtt](https://github.com/wez/govee2mqtt) | Govee LAN / BLE / IoT reference |

### Firmware liberation (`references/firmware-liberation/`)

| Repo | Role |
| --- | --- |
| [tuya-cloudcutter/tuya-cloudcutter](https://github.com/tuya-cloudcutter/tuya-cloudcutter) | OTA cloud-cut + flash BK7231 |
| [openshwprojects/OpenBK7231T_App](https://github.com/openshwprojects/OpenBK7231T_App) | OpenBeken replacement firmware |

### AI reverse-engineering bridges (`references/re-tooling/`)

| Repo | Role |
| --- | --- |
| [morluto/rea](https://github.com/morluto/rea) | **Primary** — REA MCP/CLI (Android/JADX, Ghidra, ADB, Frida, tshark, firmware) |
| [LaurieWired/GhidraMCP](https://github.com/LaurieWired/GhidraMCP) | Fallback Ghidra ↔ MCP |
| [zinja-coder/jadx-ai-mcp](https://github.com/zinja-coder/jadx-ai-mcp) | Fallback JADX ↔ MCP for APK analysis |

Host deps: `./tools/install_re_toolchain.sh`. Cursor loads REA via [`.cursor/mcp.json`](.cursor/mcp.json).

## Next research round

1. Inventory every owned light (name, MAC/IP, app, chip if known).
2. Match against [docs/03](docs/03-known-light-protocols-matrix.md) or run the RE playbook.
3. Isolate or flash Wi-Fi units ([docs/04](docs/04-firmware-liberation-and-network-isolation.md)).
4. Implement drivers + FastAPI UI per [docs/05](docs/05-target-webapp-architecture.md).

## Legal / ethics

Reverse-engineer and flash **hardware you own**. Do not attack others’ devices or redistribute proprietary firmware/APKs in this repo (captures and APKs are gitignored).
