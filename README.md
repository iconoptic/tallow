# tallow

Cheap light, kept local.

The strings and bulbs already in the house, driven over BLE and Wi-Fi from one place on the LAN. Vendor apps stay outside.

A full device inventory and the universal controller are later. The LAN panel is the slice that runs now: a menu, the H7020 page ([docs/devices/govee-h7020.md](docs/devices/govee-h7020.md)), and cameras plus settings ([docs/devices/keyestudio-mb0200-s3-cam.md](docs/devices/keyestudio-mb0200-s3-cam.md)). A Rust mesh of ESP32 nodes hears Bluetooth around the house and reports it to this host; control over it is not built ([docs/01](docs/01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh)). Moving the firmware and the host runtime to Rust is planned in [docs/plans/2026-10-31-rust-conversion.md](docs/plans/2026-10-31-rust-conversion.md). This repo is the bench: reference clones, reverse-engineering playbooks, hardware guidance, and discovery scripts.

## Quick start

```bash
# Pull curated upstream references (gitignored; re-run on fresh clones).
# A fresh clone checks out the revision pinned in tools/references.lock.
./tools/clone_references.sh
./tools/clone_references.sh --latest   # newest revisions instead, for repos not yet cloned
./tools/clone_references.sh --check    # report clones that are missing or differ from the lock

# Tooling venv (PEP 668: do not pip install system-wide)
python -m venv .venv
.venv/bin/pip install -r tools/requirements.txt

# REA host toolchain (JDK 21, Ghidra, JADX JAR, unblob, Frida, …) — Fedora
./tools/install_re_toolchain_fedora.sh
# Then restart Cursor so .cursor/mcp.json (REA MCP) loads.

# BLE discovery (needs BlueZ + adapter permissions)
.venv/bin/python tools/ble_discover.py --scan-seconds 15 --lights-only
.venv/bin/python tools/ble_discover.py --connect-gatt --address AA:BB:CC:DD:EE:FF -o discovery.json

# Parse an Android HCI snoop for ATT writes
.venv/bin/python tools/parse_btsnoop.py path/to/btsnoop_hci.log --hex

# LAN panel: menu at /, H7020 at /lights, cams at /cams, settings at /settings.
# Bind, password, scenes, and the agent socket: docs/devices/govee-h7020.md
tools/mesh.sh daemon build   # then host/target/release/tallowd panel


# Rust builds, in a pinned container (rootless Podman). Header of tools/mesh.sh has the usage.
# Design and results: docs/01. Boards: docs/devices/esp32-devkit.md
tools/mesh.sh check                # every offline gate, with firmware clippy and the node and cam builds
tools/mesh.sh check --fast         # the same, skipping firmware clippy and those two builds
tools/mesh.sh mutants              # mutation tests for the panel, the mesh core, the host, and the cam's push; slow, not part of check
tools/mesh.sh fmt                  # format all Rust in place
tools/mesh.sh test                 # mesh core tests, on this machine
tools/mesh.sh daemon check         # fmt, clippy, and tests for host/
tools/mesh.sh daemon build         # release build of tallowd
tools/mesh.sh daemon run /dev/ttyUSB0 --out data/mesh/map.json   # tallowd on the root's port; writes the map while it runs
tools/mesh.sh daemon gatt /dev/ttyUSB0 --target <id> <subcommand>   # tallowd gatt on the root's port; same container as daemon run; not run on a board
tools/mesh.sh key                  # write the gitignored firmware/mesh/mesh.key once
tools/mesh.sh ota-key              # write the gitignored ota.key (private) and tracked ota.pub, once each
tools/mesh.sh ota-image            # write the node app image to firmware/mesh/node/target/ota.bin
tools/mesh.sh ota-sign <image> <node|cam0> <version>
                                   # sign <image> for that product and version with ota.key; write the 69-byte <image>.sig
tools/mesh.sh daemon ota /dev/ttyUSB0 --target <id> --image <file> --sig <file>
                                   # send that signed image; the container gets the port as daemon gatt does,
                                   # and the named image and signature only; not run on a board
tools/mesh.sh build                # release image for a classic ESP32
tools/mesh.sh size                 # image size against the OTA slot
tools/mesh.sh cam-build            # release image for an ESP32-S3 cam
tools/mesh.sh cam-size             # cam image size against its OTA slot
tools/mesh.sh cam-image            # write the cam app image to firmware/cam/target/ota.bin
tools/mesh.sh cam-flash <port>     # write the cam image and this build's bootloader, and erase otadata,
                                   # on the port boards.tsv names toby-cam-0; not run
tools/mesh.sh cam-backup <port>    # save that cam's whole flash; not run
tools/mesh.sh cam-frame            # print one host-built multipart sample on stdout
tools/mesh.sh cam-push <address> <image>
                                   # send <image> and <image>.sig to 10.87.0.23:3232 and wait for the cam's result.
                                   # Refuses every other address. Does not read ota.key. Not run
tools/mesh.sh ports                # list attached boards; opens no port, resets nothing
# flash, monitor, backup, host, bench, daemon run, daemon gatt, and daemon ota open only a /dev/ttyUSB* port that firmware/mesh/boards.tsv lists as esp32
tools/mesh.sh flash /dev/ttyUSB0   # write the image and this build's bootloader to one classic ESP32
tools/mesh.sh monitor /dev/ttyUSB0 30   # print a board's serial output for 30 s
tools/mesh.sh backup /dev/ttyUSB0  # save a board's whole flash to data/firmware-backups/ (mode 0600; it holds the mesh key)
tools/mesh.sh bench /dev/ttyUSB0 --deny <id-hex> --seconds 900   # hold the port, refuse a root id, print @ lines
# Panel HTTP contract: starts its own previews against host/target/release/tallowd (tools/mesh.sh daemon build), reads nothing under secrets/, and always checks /devices and /api/mesh/map
.venv/bin/python tools/tests/panel_contract.py
# Another tallowd build
.venv/bin/python tools/tests/panel_contract.py --panel-command 'path/to/tallowd panel --preview'
```


## Repository map

| Path | Purpose |
| --- | --- |
| [docs/01-bluetooth-server-hardware-and-proxies.md](docs/01-bluetooth-server-hardware-and-proxies.md) | Host Bluetooth seed, USB adapters, ESP32 BLE proxies, and the Rust mesh: design and measured results |
| [docs/02-reverse-engineering-playbook.md](docs/02-reverse-engineering-playbook.md) | REA (JADX/Ghidra/ADB/Frida/tshark), btsnoop, btmon workflow |
| [docs/03-known-light-protocols-matrix.md](docs/03-known-light-protocols-matrix.md) | Fingerprints for ELK-BLEDOM, Tuya, MagicHome, Govee, … |
| [docs/04-firmware-liberation-and-network-isolation.md](docs/04-firmware-liberation-and-network-isolation.md) | Host IoT access point, isolation boundary, CloudCutter / OpenBeken |
| [docs/05-target-webapp-architecture.md](docs/05-target-webapp-architecture.md) | FastAPI blueprint. The H7020 LAN page is the slice that exists |
| [docs/devices/govee-h7020.md](docs/devices/govee-h7020.md) | Govee H7020 BLE frames, the probe, the LAN page, the host agent socket, and the Rust codec, panel, and broker |
| [docs/devices/keyestudio-mb0200-s3-cam.md](docs/devices/keyestudio-mb0200-s3-cam.md) | MB0200 S3 CAM node board: pins, first flash, the cams page, and the unflashed Rust image |
| [docs/devices/esp32-devkit.md](docs/devices/esp32-devkit.md) | Classic ESP32 devkits: the two mesh boards, flash, and the way back to ESPHome |
| [docs/plans/2026-10-31-rust-conversion.md](docs/plans/2026-10-31-rust-conversion.md) | Rust conversion plan: scope, settled decisions, and phases. Tasks are in `docs/plans/rust-conversion/` |
| [docs/plans/rust-conversion/agent-brief.md](docs/plans/rust-conversion/agent-brief.md) | What every model reads before a Rust conversion task |
| [app/](app/) | Page assets the Rust panel serves: `menu_page.html`, `h7020_page.html`, `cams_page.html`, `devices_page.html`, `settings_page.html`, `panel.css`. HTTP contract: `tools/tests/panel_contract.py` |
| [archive/](archive/) | Retired Python panel, broker, H7020 probe, mesh host, tests, and ESPHome firmware. Historical reference only; do not import from it |
| [firmware/cam/](firmware/cam/) | Rust cam image `tallow-cam` for the MB0200 (ESP32-S3), with the camera driver in `src/camera.rs`. `tools/mesh.sh cam-build`, `cam-size`, `cam-image`, `cam-flash`, `cam-backup`, `cam-frame`, and `cam-push`. Not flashed, and `cam-push` has not been run. `target/`, `.embuild/`, and `sdkconfig` are gitignored. Layout and size: [device note](docs/devices/keyestudio-mb0200-s3-cam.md#rust-image) |
| [firmware/mesh/](firmware/mesh/) | Rust mesh: `core/` (`no_std` logic and tests; frame bytes in [`core/README.md`](firmware/mesh/core/README.md)), `node/` (ESP-IDF firmware), `boards.tsv` (boards by USB path, read by `tools/mesh.sh ports`), `ota.pub` (OTA public key). `mesh.key`, `ota.key`, `target/`, `.embuild/`, `.cache/`, and `sdkconfig` are gitignored |
| [host/](host/) | Cargo workspace. `tallowd` shares `tallow-mesh-core` with the firmware. Its `run` is the mesh host end and its `panel` is the Rust LAN panel, neither cut over; the serial link is `src/link.rs`. `tallowd ota` (`src/ota.rs`) sends one signed image and has not been run. `tallow-govee` is the H7020 codec, and `tallow-broker` is the agent broker, not deployed. `host/target/` is gitignored |
| [AGENTS.md](AGENTS.md) | Instructions for future AI coding agents |
| [.cursor/skills/improve-repo-docs/SKILL.md](.cursor/skills/improve-repo-docs/SKILL.md) | Reconcile docs with the tree. Ledger: `.cursor/docs-ledger.json`. Stop hook: `.cursor/hooks/docs-before-stop.py` |
| [.cursor/skills/](.cursor/skills/) | The one agent-config tree (skills, plugins, rules, hooks, `mcp.json`). Other harnesses read it through symlinks: `.agents/skills`, `.agents/plugins`, `.claude/skills`, `CLAUDE.md` (to `AGENTS.md`), and `.mcp.json` (to `.cursor/mcp.json`). Copied agent skills. Caveman: 22 from [JuliusBrussee/caveman](https://github.com/JuliusBrussee/caveman) (voice, commit, review, compress, learn, and workflow helpers), plus `megacave` (Classical Chinese voice) and `ultracave` (maximum compression), which are untracked and not yet in the lock file. Refresh: `npx skills add JuliusBrussee/caveman -a cursor --skill '*' --copy -y`. Rust security skills live in the same directory: `zeroize-audit`, `constant-time-analysis`, `differential-review`, `rust-review`, and `rust-best-practices`. Lock file: [skills-lock.json](skills-lock.json), which pins the Trail of Bits commit and records a hash for each other skill. Inventory and the Rust refresh: [AGENTS.md](AGENTS.md) |
| [.cursor/plugins/](.cursor/plugins/) | Unmodified Trail of Bits plugins those skills call: `rust-review`, `constant-time-analysis`, `differential-review`, `insecure-defaults` (no `SKILL.md` upstream), and `LICENSE`. Cursor entries: [.cursor/skills/rust-review-cursor/SKILL.md](.cursor/skills/rust-review-cursor/SKILL.md) and [.cursor/skills/insecure-defaults/SKILL.md](.cursor/skills/insecure-defaults/SKILL.md). Inventory: [AGENTS.md](AGENTS.md). Review output `.rust-review-results/` is gitignored. No audit has been run |
| [.cursor/mcp.json](.cursor/mcp.json) | Project-scoped REA MCP registration for Cursor |
| [tools/](tools/) | Clone script, Fedora RE toolchain, IoT access point, Bluetooth seed, mesh wrapper, the bench (`mesh_bench.py`), mutation tests (`tools/mesh.sh mutants`), the Rust host setup (`setup_mesh_host.sh`, `tallowd.service`, `rust-host.Dockerfile`), serial monitor (`mesh_monitor.py`), discovery / snoop parsers |
| `references/` | Shallow clones (not committed; see catalog below). `references/vendor/` is copied by hand |

## Hardware stance (summary)

For “open a web app from any device on my network”:

1. **ESP32 mesh nodes** hear the house today. Control is not built. **ESP32 Ethernet/PoE Bluetooth proxies** near the lights remain the known route to GATT at a distance, and they are not wired.
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
| [teh-hippo/ha-govee-led-ble](https://github.com/teh-hippo/ha-govee-led-ble) tag `v7.14.0-rc.2.h7020` | Govee H7020 BLE codec. The clone script unpacks `generated_protocol/` from the release zip. `host/tallow-govee` is checked against it |

### Wi-Fi light protocols (`references/protocols-wifi/`)

| Repo | Role |
| --- | --- |
| [jasonacox/tinytuya](https://github.com/jasonacox/tinytuya) | Tuya local LAN (+ BLE helpers) |
| [python-kasa/python-kasa](https://github.com/python-kasa/python-kasa) | TP-Link Kasa / Tapo |
| [Danielhiversen/flux_led](https://github.com/Danielhiversen/flux_led) | MagicHome / Flux LED (TCP 5577) |
| [wez/govee2mqtt](https://github.com/wez/govee2mqtt) | Govee LAN and cloud for SKUs on Govee's LAN list. No BLE driver |

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

Host deps: `./tools/install_re_toolchain_fedora.sh`. Cursor loads REA via [`.cursor/mcp.json`](.cursor/mcp.json).

### Rust libraries (`references/rust/`)

The libraries the [Rust conversion](docs/plans/2026-10-31-rust-conversion.md) builds on.

| Repo | Role |
| --- | --- |
| [esp-rs/esp-idf](https://github.com/esp-rs/esp-idf) | `esp-idf-svc`, `esp-idf-hal`, and `esp-idf-sys`, moved here from their archived repos. Cloned as `esp-rs-esp-idf` |
| [bluez/bluer](https://github.com/bluez/bluer) | BlueZ from Rust, for the host's own radio |

### Presence (`references/presence/`)

Prior art for rooms and rotating addresses.

| Repo | Role |
| --- | --- |
| [ESPresense/ESPresense](https://github.com/ESPresense/ESPresense) | ESP32 room presence nodes |
| [agittins/bermuda](https://github.com/agittins/bermuda) | BLE trilateration in Home Assistant |
| [DerekSeaman/irk-capture](https://github.com/DerekSeaman/irk-capture) | ESPHome package that captures Apple and Android IRKs |

## Next research round

1. Inventory every owned light (name, MAC/IP, app, chip if known).
2. Match against [docs/03](docs/03-known-light-protocols-matrix.md) or run the RE playbook.
3. Isolate or flash Wi-Fi units ([docs/04](docs/04-firmware-liberation-and-network-isolation.md)).
4. Inventory, groups, control, and proxies still follow [docs/05](docs/05-target-webapp-architecture.md). The H7020 page already has catalogue scenes, bulb paints, sequences, host schedules, and a host-only agent socket.

## Legal / ethics

Reverse-engineer and flash **hardware you own**. Do not attack others’ devices or redistribute proprietary firmware/APKs in this repo (captures and APKs are gitignored).
