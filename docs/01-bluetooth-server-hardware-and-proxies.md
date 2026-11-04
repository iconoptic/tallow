# Bluetooth Server Hardware and Proxies

Goal: control cheap BLE smart lights from a self-hosted web app reachable from any device on the LAN. The browser never speaks Bluetooth; the **server** owns the radios.

## Recommendation (TL;DR)

| Scale | Approach |
| --- | --- |
| Few lights near the server | One high-quality USB BLE adapter on a **USB 2.0** extension, away from the machine |
| Hearing the whole house | The ESP32 [mesh](#multi-hop-mesh) in `firmware/mesh/`. It reports BLE and Classic devices. Control is not built |
| Whole-house / multi-floor GATT | Distributed **ESP32 Bluetooth proxies** (Ethernet/PoE preferred) + optional local USB adapter. Not wired on this host |
| Many concurrent GATT connections | Proxies + multi-adapter HA stack (`habluetooth`); do not rely on one dongle’s 5–7 connection slots |

A “top tier” single dongle helps, but **proxies usually beat a single long-range radio** for reliability in a house.

## This host

The seed radio is the TP-Link UB500 (USB `2357:0604`) on USB 2.0 port 3-6. `tools/setup_iot_ble.sh` powers that controller and leaves it not discoverable and not pairable. No pairing agent is registered, so inbound pairing stays closed. `up` refuses unless the Kootech access point from [docs/04](04-firmware-liberation-and-network-isolation.md) is already up. It reinstalls that access-point script's combo-off oneshot, so bluetoothd AutoEnable cannot turn the Realtek combo's Bluetooth (USB `0bda:d723`) back on. That controller shares the survey antenna. House listening is the ESP32 [mesh](#multi-hop-mesh), which joins no SSID. This script only prepares the host radio.

Port 3-6 is the USB 2.0 port beside the Kootech antenna. A short USB 2.0 extension that moves the UB500 away from that antenna is worth doing before trusting RSSI from this stick. Leave the Kootech where it is.

`secrets/iot-ble/accept_macs` starts empty (directory `0700`, file `0600`, gitignored with `secrets/`). It is the outbound allow list for the future controller. `allow` and `revoke` only edit that file; they do not change BlueZ policy. Re-running `up` leaves an existing allow file in place. Leave the kernel LE whitelist unset so `tools/ble_discover.py` still sees advertisers.

```bash
sudo ./tools/setup_iot_ble.sh
sudo ./tools/setup_iot_ble.sh status
sudo ./tools/setup_iot_ble.sh allow aa:bb:cc:dd:ee:ff
sudo ./tools/setup_iot_ble.sh revoke aa:bb:cc:dd:ee:ff
```

## Transport stack for control (target)

This is the target for control. `habluetooth` and `bleak-esphome` are not wired, and the [mesh](#multi-hop-mesh) uses neither: `tallowd run` reads the root's serial port. It also listens on the host's own adapter through BlueZ (`--bluez`), as one more node ([Host adapter as a node](#host-adapter-as-a-node)).

The [Rust conversion plan](plans/2026-10-31-rust-conversion.md) replaces this stack: `tallowd` on axum for FastAPI, `bluer` for the host's own radio, GATT over the mesh for proxies, and ESPHome retired in its phase 5. Code exists for the first two. `tallowd panel` serves the panel routes, and its H7020 link runs on `bluer`. Neither has run on the box. The Python panel and the ESPHome configs are archived (`archive/`), so `tallowd panel` is the panel. The GATT client over the mesh is in the node source and is not flashed. No board runs an ESPHome image that this tree can rebuild.

```
Web UI  →  FastAPI backend  →  habluetooth / bleak
                                      ├─ local BlueZ adapter(s)  →  BLE lights
                                      └─ bleak-esphome          →  ESP32 proxies  →  BLE lights
```

Reference clones:

- `references/ble-stack/bleak`
- `references/ble-stack/habluetooth`
- `references/ble-stack/bleak-esphome`
- `references/ble-stack/bumble` (raw HCI / ATT when BlueZ discovery is broken)

## Local USB adapters (Linux / BlueZ)

### Requirements

- BlueZ **≥ 5.63** strongly recommended (passive scanning / experimental features).
- Prefer **dbus-broker** and kernel **≥ 5.15.62**.
- Adapter must be usable from D-Bus (`bluetoothd`).

### High-performance class (Home Assistant Bluetooth docs)

**CSR8510A10**-based Class 1 sticks with external antenna are the most consistently recommended for active GATT:

- Feasycom **FSC-BP119** (CSR8510A10, external antenna) — HA Bluetooth maintainers’ primary test dongle
- Sena **UD100-G03** (CSR8510A10, external antenna)
- Many ORICO / UGREEN / Digitus CSR 4.0 sticks (same chip family)

Typical connection budget: about **5** simultaneous connections.

**Avoid:**

- Unbranded CSR clones with USB ID `0a12:0001` (kernel workarounds; often unrecoverable failures)
- Many Realtek RTL8761BU sticks as *primary* active-connection adapters (no reset pin; may need physical re-plug when they hang). Fine as secondary / long-range experiment.
- ATS2851 dongles still listed as unsupported in HA (missing Linux drivers for some USB IDs)

### Long-range / Coded PHY notes

BLE Long Range (LE Coded PHY) needs adapter + BlueZ + app support. Cheap LED controllers almost always use **1M PHY**, so Coded PHY rarely helps *light* control. Buy Class 1 + good antenna placement instead of chasing “BT 5.4 Long Range” marketing.

Verify adapter capabilities:

```bash
hciconfig -a
btmgmt -i hci0 info
btmgmt -i hci0 phy   # look for LECODEDTX / LECODEDRX if experimenting
```

### Placement and interference (critical)

2.4 GHz BLE is fragile next to USB 3.0 and Wi-Fi APs:

1. Plug the dongle into a **USB 2.0** port (or a powered USB 2.0 hub if the host only has USB 3.x).
2. Use a **shielded USB extension** and put the antenna near the middle of the coverage area, away from the server chassis and Wi-Fi APs.
3. Prefer vertical antenna orientation; try a few positions and measure RSSI with `tools/ble_discover.py`.
4. Keep multiple USB adapters physically separated if using more than one.

### Container passthrough (Podman)

If the controller runs in a container (Compose syntax; Podman reads it too):

```yaml
cap_add:
  - NET_ADMIN
  - NET_RAW
volumes:
  - /run/dbus:/run/dbus:ro
```

Without those caps, BlueZ management APIs fail and adapters cannot be reset when they wedge. Rootless Podman cannot grant `NET_ADMIN` on the host network, so a controller that needs adapter resets runs rootful, or resets the adapter from the host.

## Distributed Bluetooth proxies

An ESP32 (or Pico W) running ESPHome `bluetooth_proxy` scans (and optionally opens GATT connections) near the lights, then relays over Wi-Fi or Ethernet to the server. The Python side would use `bleak-esphome` so existing `bleak`-style drivers keep working. This is the upstream pattern, and it remains the known route to GATT at a distance. On this host the cams run it, nothing consumes it, and house listening runs on the [mesh](#multi-hop-mesh) instead.

### Proxy hardware preference (best → good)

1. **Ethernet / PoE ESP32** — e.g. Olimex `ESP32-POE-ISO-EA` (+ matching enclosure). Avoids Wi-Fi/BLE radio contention on the proxy itself.
2. **Wi-Fi ESP32** with external antenna — place one per zone; expect more interference than PoE.
3. Shelly Gen2+ / some SMLIGHT sticks — advertisement listening only (no active GATT on many models).

### ESPHome sketch outline

```yaml
esp32_ble_tracker:

bluetooth_proxy:
  active: true          # required for writing to lights
  # connection_slots: 3 # raise carefully; RAM limited (~5 max recommended on ESP32)
```

Performance tips from HA docs:

- Ethernet proxies with **passive** scanning outperform Wi-Fi proxies and local USB in many setups.
- Advertisement bundling (ESPHome ≥ 2023.6) reduces Wi-Fi chatter.
- Separate proxies spatially; overlapping radios fight each other.

### tallow nodes

The goal is central control of the house's Bluetooth devices. There are two kinds of node:

- A **mesh node** is a classic ESP32 running the Rust firmware in `firmware/mesh/`. It joins no Wi-Fi, and it reaches the host through other nodes. Both classic boards run this. They were flashed on 2026-10-31 with the cross-check build, and the source has moved on since ([Multi-hop mesh](#multi-hop-mesh), [Flashed 2026-10-31](#flashed-2026-10-31-the-cross-check-build), [Source ahead of the boards](#source-ahead-of-the-boards-frame-version-0xa8)).
- An **ESPHome node** runs `bluetooth_proxy`, joins `tallow-iot`, and talks only to the host over Wi-Fi. The cams are built this way. A Rust image in `firmware/cam` builds. On 2026-11-02 `cam-flash` wrote a modem-sleep image. USB `cam-flash` of the later source has not been run. An evening `cam-push` stored a built image on two other cams ([device note](devices/keyestudio-mb0200-s3-cam.md#signed-push)). Picking a node per device with habluetooth and bleak-esphome is not wired.

The rest of this section is the ESPHome nodes. They are retired: the configs, `esphome.sh`, and `node_check.py` moved to `archive/` (same relative paths; [archive/README.md](../archive/README.md)), and nothing runs them. Paths below such as `firmware/esphome/` and `tools/esphome.sh` now live under `archive/`. Their names are separate from the mesh's: `archive/firmware/esphome/node-1.yaml` targets the first board, which the mesh calls `node-0`.

The configs were in `firmware/esphome/`. They are stock ESPHome with no external components, on ESPHome's default ESP-IDF (5.5.5 in the 2026-10-31 builds). `tools/esphome.sh` built them in the pinned container image (rootless Podman) `ghcr.io/esphome/esphome:2026.9.1`; nothing was installed on the host. Its header has the usage lines. Addresses are in [docs/04](04-firmware-liberation-and-network-isolation.md#esp32-nodes).

| File | Role |
| --- | --- |
| `packages/node.yaml` | The Bluetooth proxy node. Every node pulls it in |
| `packages/board-mb0200.yaml` | Keyestudio MB0200 S3 CAM board |
| `packages/board-esp32.yaml` | Classic ESP32 devkit, 4 MB flash, default ESP-IDF |
| `packages/dogcam.yaml` | The cam skew |
| `node-mb0200.yaml` | The node alone, `10.87.0.20` |
| `toby-cam-1.yaml` | Node plus cam, `10.87.0.21` |
| `toby-cam-0.yaml` | Second cam, node plus cam, `10.87.0.23`. Compiled, not flashed |
| `node-1.yaml` | Proxy on a classic ESP32, `10.87.0.22`. On no board. It remains as a way back to ESPHome on the first board |
| `secrets.example.yaml` | Shape of the gitignored `secrets.yaml` (`wifi_ssid`, `wifi_password`, `api_key`). The Rust cam now reads its Wi-Fi from gitignored `firmware/cam/secrets.yaml` instead |

`packages/node.yaml` joins `tallow-iot` with a static `manual_ip`: the node's address, with gateway and DNS at `10.87.0.1`. `use_address` is the same address, so the host dials it directly. A node needs no lease and no reservation. There is no fallback AP and no captive portal. mDNS stays at ESPHome's default; the host does not use it. The native API is noise-encrypted with `api_key`. OTA uses the same key. `reboot_timeout` is `0s` on the API, because nothing on the host holds the API open yet. Wi-Fi keeps ESPHome's default `reboot_timeout` of 15 minutes. Time comes from `10.87.0.1`. `power_save_mode` is ESPHome's default, which compiled to `light` on 2026-10-31. Scanning is active and `bluetooth_proxy` is `active: true`, with ESPHome's default 3 GATT slots. A `wifi_signal` sensor ("Wi-Fi signal") reports the signal to the AP every 60 s. That signal sets where a node can live.

A cam sets `bluetooth_proxy: active: false`. It still forwards advertisements and holds no GATT connections. Video and BLE share one 2.4 GHz radio, so a cam takes no GATT. No board runs a GATT node as of 2026-10-31.

`power_save_mode: none` is the wrong setting on a BLE node. ESP-IDF needs Wi-Fi modem sleep while Bluetooth is on. On 2026-10-28 a cam built that way never joined Wi-Fi, and OTA rollback put the relay image back ([device note](devices/keyestudio-mb0200-s3-cam.md#first-flash)).

`tools/node_check.py` (now `archive/tools/node_check.py`) asks one node for its name, ESPHome version, Wi-Fi and BLE MACs, and proxy flags, then counts advertisements for `--seconds` (default 15). It prints the Wi-Fi signal: "not reported by this build" when the sensor is absent, "not measured yet" before the first 60 s reading. It says whether the H7020 was heard (`--target` to look for another address). It sends nothing to any light. It exits 1 when the node does not answer, runs no BLE proxy, or heard nothing. The key is the `api_key` line of `firmware/esphome/secrets.yaml`, or `--key-file`. Run it as `archive/tools/node_check.py`; the archived path is not wired to anything. `--address` must be in `10.87.0.0/24`.

```bash
.venv/bin/python archive/tools/node_check.py --address 10.87.0.22
.venv/bin/python archive/tools/node_check.py --address 10.87.0.21 --seconds 30
```

On 2026-10-28 one MB0200 ran `relay-mb0200`, then `toby-cam-1`. The standalone relay is proven: it answered `relay_check.py` (the earlier name of this check) with full proxy flags and heard 17 BLE addresses in 15 s. It did not hear the H7020 from where it sat. The cam image is scan-only, as designed. Results: [device note](devices/keyestudio-mb0200-s3-cam.md#on-the-board-2026-10-28). The host does not use ESPHome nodes yet: bleak-esphome and habluetooth are not wired in. That host merge, picking a node per light, is still unbuilt.

On 2026-10-30 `relay_check.py --address 10.87.0.21` found `toby-cam-1` up (built 2026-10-29 05:11:41 UTC), scan-only flags, 13 BLE addresses in 10 s, H7020 not heard, and "Wi-Fi signal not reported by this build". That image predates the sensor. The same day the earlier `relay-1` image passed `config` and `compile`. A later OTA of `toby-cam-1` to `10.87.0.21:3232` timed out. The board held `10.87.0.21` and did not answer ARP, so the panel could not open the stream ([device note](devices/keyestudio-mb0200-s3-cam.md#where-it-is-2026-10-30)).

On 2026-10-31 no ESPHome node is up. `toby-cam-1` is unreachable at `10.87.0.21` and not recovered, after an [esphome-mesh](#esphome-mesh-removed) build crash-looped. Its build without esphome-mesh is compiled and not flashed. `toby-cam-0` is compiled and not flashed ([device note](devices/keyestudio-mb0200-s3-cam.md#where-it-is-2026-10-31)). Both classic ESP32 boards run the Rust mesh ([device note](devices/esp32-devkit.md#where-it-is-2026-10-31)).

The MB0200 is a Wi-Fi ESP32, rank 2 above. Board, pins, and first flash: [devices/keyestudio-mb0200-s3-cam.md](devices/keyestudio-mb0200-s3-cam.md). The mesh boards are classic ESP32 devkits: [devices/esp32-devkit.md](devices/esp32-devkit.md).

### Multi-hop mesh

Classic ESP32 boards listen to Bluetooth near them and pass what they hear to this host through each other. The link is ESP-NOW on channel 1, the cleanest in the 2026-10-31 survey and separate from `tallow-iot` on 6. A mesh node joins no Wi-Fi and opens no socket. It listens and reports. The source also pages known Classic addresses, makes GATT requests to whitelisted devices, and runs a signed OTA receiver on the node. The host can send one image. All of that is built and not flashed ([Source ahead of the boards](#source-ahead-of-the-boards-frame-version-0xa8)). Control of a light over the mesh is not built. `bleak-esphome` and `habluetooth` remain unwired, and this mesh uses neither.

| Path | Role |
| --- | --- |
| `firmware/mesh/core` | `no_std` mesh logic: frames, routing, sightings, names, advert parsing, and `src/ota.rs` (the signed-image receiver). It has no radio and no OS, so all of it runs under host tests |
| `firmware/mesh/node` | ESP-IDF firmware `tallow-node`, on ESP-IDF v5.5.5 like the ESPHome nodes. `src/radio.rs` is ESP-NOW, `src/bluetooth.rs` is BLE scan and Classic inquiry, `src/gatt.rs` is the GATT client, `src/ota.rs` writes one OTA image from the main loop, and `src/main.rs` is the loop and the serial port. `partitions.csv` is the ESPHome nodes' OTA layout |
| `tools/mesh.sh` | Pinned image `espressif/idf-rust:esp32_1.95.0.0`; nothing is installed on the host. Its header has the usage lines |
| `archive/tools/mesh_host.py` | The retired Python host end. `tools/mesh.sh host` is gone; `tallowd run` replaced it |
| `tools/mesh_monitor.py` | Prints a board's serial output. Runs through `tools/mesh.sh monitor` |
| `firmware/mesh/boards.tsv` | Known boards. `usb_path` is where that chip was last read. A USB serial that is a colon MAC names the chip; the CP2102 serial `0001` does not, so those rows are named by path, and `tools/mesh.sh ports` marks them `(unverified)`. The header comment states that. `tools/mesh.sh ports` reads the file and opens no port; `ports --read` and `identify` read each chip and rewrite it. `tools/fleetd.sh` rewrites it from the chip when it runs |
| `firmware/mesh/devices.jsonl` | Identity catalog for `tools/fleetd.sh`. `fleetd` rewrites it, and `boards.tsv`, from the chip on each port when it runs. The learned rows are in the paragraph below. Name, chip, USB path, and Wi-Fi MAC match. `push_ok` is false. `tools/sync_panel_cams.py` reads the cam rows ([cam note](devices/keyestudio-mb0200-s3-cam.md#catalog-sync)) |
| `host/` | Cargo workspace. `host/tallowd` shares `tallow-mesh-core`. `tallowd run` is the host end: the job the archived `mesh_host.py` did, with `--trial`, `--watch`, and `--set`. `src/traffic.rs` is the private sighting log beside the map ([Traffic log](#traffic-log)). `tallowd gatt` sends one GATT request (`src/gatt.rs`). `tallowd ota` sends one signed image (`src/ota.rs`) and has not been run. `src/link.rs` is its serial link. It caps its line buffer at 2 KB and refuses any port but `/dev/ttyUSB*`. `tallowd panel` is the Rust LAN panel ([govee-h7020](devices/govee-h7020.md#rust-panel-tallowd-panel)). `host/tallow-govee` is the H7020 codec, and `host/tallow-broker` is the agent broker. Its user unit is installed. The panel has to run as that same user, because a root panel locks the `0600` socket. |

```bash
tools/mesh.sh check                # every offline gate, with firmware clippy and the node and cam builds
tools/mesh.sh check --fast         # the same, skipping firmware clippy and those two builds
tools/mesh.sh mutants              # mutation tests for the panel, the mesh core, the host, and the cam's push; slow, not part of check
tools/mesh.sh fmt                  # format all Rust in place
tools/mesh.sh test                 # mesh core tests, on this machine
tools/mesh.sh daemon check         # fmt, clippy, and tests for host/
tools/mesh.sh daemon build         # release build of tallowd
tools/mesh.sh key                  # write the gitignored firmware/mesh/mesh.key once
tools/mesh.sh build                # release image for a classic ESP32
tools/mesh.sh size                 # image size against the OTA slot
tools/mesh.sh cam-build            # release image for an ESP32-S3 cam
tools/mesh.sh cam-size             # cam image size against its OTA slot
tools/mesh.sh cam-image            # write the cam app image to firmware/cam/target/ota.bin
tools/mesh.sh cam-flash <port>     # any ESP32-S3, named by its MAC (next toby-cam-N); reserve its address (a reservation already on file skips sudo; a failed reserve stops before the write); this build's bootloader, and erase otadata; then wait up to 2 minutes for the cam on 6053 and on the panel page (else `run: tools/mesh.sh cams`). A modem-sleep image was written on 2026-11-02; USB flash of this source has not been run ([device note](devices/keyestudio-mb0200-s3-cam.md#signed-push))
tools/mesh.sh cam-backup <port>    # that cam's whole flash; not run
tools/mesh.sh cam-frame            # one host-built multipart sample on stdout
tools/mesh.sh cams                 # contact each catalog cam on port 6053 and
                                   # add the ones that answer to data/panel/cams.json.
                                   # A row already on that address takes the catalog id.
                                   # Opens no serial port. Ran on 2026-11-02 (cam note)
tools/mesh.sh cam-push <address> <image>   # port 3232 on an address reserved for a catalog cam; does not read ota.key; evening of 2026-11-02 ([device note](devices/keyestudio-mb0200-s3-cam.md#signed-push))
tools/mesh.sh fleet [--no-probe]   # each catalog board: current with this build, heard in the map, cam address and port 6053 (--no-probe skips 6053); opens no serial port
tools/mesh.sh ports                # list attached boards and a MAC column; opens no port, resets nothing; a CP2102 board is (unverified)
tools/mesh.sh ports --read         # identify each port whose USB serial is not a known MAC (a CP2102 resets) and name it by MAC
tools/mesh.sh identify /dev/ttyUSB0   # esptool flash-id, and record the chip by MAC; opens the port, so a CP2102 resets; any tty, not only a listed board
tools/fleetd.sh                    # hand-started; identify each new port and rewrite the catalog; each poll contacts catalog cams on port 6053; Ctrl-C stops it; ran on 2026-11-02
tools/mesh.sh flash /dev/ttyUSB0
tools/mesh.sh monitor /dev/ttyUSB0 30    # 20 s if no time is given
tools/mesh.sh backup /dev/ttyUSB0        # whole flash to data/firmware-backups/
tools/mesh.sh daemon run /dev/ttyUSB0 --out data/mesh/map.json   # tallowd run on the root's port
tools/mesh.sh daemon run /dev/ttyUSB0 --trial --out data/mesh/trial.json
tools/mesh.sh daemon run /dev/ttyUSB0 --watch <classic-address>...   # page these Classic addresses (up to 16)
tools/mesh.sh daemon gatt /dev/ttyUSB0 --target <id> <subcommand>   # tallowd gatt; same container and port as daemon run; not run on a board
tools/mesh.sh bench /dev/ttyUSB0 --deny <id-hex> --seconds 900   # hold the port, refuse a root, print @ lines
tools/mesh.sh ota-key              # write the gitignored ota.key (private) and tracked ota.pub, once each
tools/mesh.sh ota-image            # write the node app image to firmware/mesh/node/target/ota.bin
tools/mesh.sh ota-sign <image> <node|cam> <version>   # cam0 is the same product; sign for that product and version; write the 69-byte <image>.sig; tmpfs is /tmp:mode=1777 ([device note](devices/keyestudio-mb0200-s3-cam.md#signed-push))
tools/mesh.sh daemon ota /dev/ttyUSB0 --target <id> --image <file> --sig <file>   # tallowd ota; same port as daemon gatt; mounts only that image and signature; not run on a board
```

`tools/mesh.sh ota-key` has already been run. Where the two files live: [docs/04](04-firmware-liberation-and-network-isolation.md#mesh-nodes).

Every command exits non-zero when its step fails. `check` runs every offline gate, in this order:

1. Core: fmt, clippy, and tests.
2. Daemon: fmt, clippy, and tests for `host/`. The image is built locally from `tools/rust-host.Dockerfile` on first use by Podman, and that needs apt network access.
3. The host tool's unit tests: `test_mesh_bench`, `test_mesh_tool`, `test_fleetd`, `test_sync_panel_cams`, `test_fleet_status`, `test_fleet_catalog`, `test_parse_btsnoop`, and `test_ble_discover`.
4. Scripts: `bash -n` on `tools/*.sh` and `py_compile` on `tools/*.py`. The vector byte-compare is gone with `dump_h7020_vectors.py`; `host/tallow-govee/tests/vectors.json` is now fixed data.
5. The cam push signature test: a host build of `tools/tests/cam_push` that copies `firmware/cam/src/push.rs`, `serve.rs`, and `stream.rs`, and mounts only `firmware/mesh/core`.
6. The panel's page tests: `test_cams_page`, `test_devices_page`, and `test_traffic_page` (node harnesses). The HTTP contract runs last, in step 8.
7. `cargo fmt --check` on the node and the cam. The cam step stops without `firmware/cam/secrets.yaml` or `firmware/mesh/ota.pub` ([device note](devices/keyestudio-mb0200-s3-cam.md#rust-image)).
8. Without `--fast`: clippy on the node and the cam (`cargo clippy --release -- -D warnings`). `firmware/mesh/node` and `firmware/cam` both warn on `clippy::undocumented_unsafe_blocks`, so a missing `SAFETY` comment fails this step. Then the node firmware build, its stack budget (`tools/tests/stack_budget.py` on the node ELF), the cam firmware build, `tools/cam_build_scan.py` on the cam image (it must print `no default credential`), the cam's stack budget against the chains in `tools/tests/stack_chains_cam.json`, the `tallowd` release build, and the HTTP contract, `tools/tests/panel_contract.py`, against `tallowd panel --preview` (the default; it always checks `/devices`, `GET /api/mesh/map`, and `/traffic`). The node steps need `firmware/mesh/mesh.key`. The cam steps need the secrets file and `ota.pub`, and they do not mount `mesh.key`.

`tools/mesh.sh mutants` is not one of those steps. It runs the mesh core, host, and cam mutants (`tools/tests/mutants/`), applies one mutation at a time, and exits non-zero if a mutation is missed or its source text is no longer in the tree. The panel mutants went with the Python panel. `cut_inquiry` is not in that run: the window math is in `firmware/mesh/node/src/bluetooth.rs`, and that crate does not `cargo test` on the host.

`tools/fleetd.sh` is the hand-started flasher. It runs until Ctrl-C and is not a login service. On start it runs `tools/mesh.sh build` and `cam-build`, whatever is plugged in. A failed cam build writes a failure log and is not retried in that run: cams are then not flashed, and nodes still are. Every 5 seconds it reads the same udev paths as `ports`. For a port this presence has not finished reading, it runs `tools/mesh.sh identify` (`esptool flash-id`), which opens the port and resets a CP2102. A later poll does not open a tty whose read succeeded. A failed esptool read of a CP2102 is logged and not written, and that port is not opened again until it disappears. Until this presence has read that chip, `fleetd` will not flash the path as the last recorded name. A failed read of a MAC-serial port is tried again, up to three times. `tools/fleet_catalog.py` parses the chip, the MAC, and the flash manufacturer, and rewrites `firmware/mesh/devices.jsonl` and `firmware/mesh/boards.tsv` when `fleetd` runs. The files name `node-0` on path `7` (`70:4b:ca:04:21:64`), `node-1` on path `12` (`20:9b:a9:99:1e:54`), `node-6` on path `11` (`20:9b:a9:8a:22:bc`), `toby-cam-3` on path `2` (`44:bd:8d:7c:20:c4`), and, with an empty path, `node-2` (`7c:9e:bd:ed:bd:98`), `node-3` (`20:9b:a9:99:32:0c`), `node-4` (`20:9b:a9:97:9f:28`), `node-5` (`7c:9e:bd:ed:c3:8c`), `toby-cam-0` (`44:bd:8d:7a:51:60`), `toby-cam-1` (`44:bd:8d:7a:50:cc`), and `toby-cam-2` (`b8:1f:3f:ab:c5:60`). On 2026-11-02 an earlier `tools/mesh.sh ports` showed only `/dev/ttyUSB0`, `node-0`, path `7`. A later run the same day printed a USB-MAC column (now `MAC`) and did not run identify, `fleetd`, or any flash: `/dev/ttyUSB0` `node-0` path `7` USB-MAC `-`; `/dev/ttyACM0` `unknown` path `2` USB-MAC `44:bd:8d:7c:20:c4`. That ACM chip is now `toby-cam-3` on path `2`. `toby-cam-0` keeps `44:bd:8d:7a:51:60` with an empty path. `ports` still opens no port. `attachment_row` in `tools/fleet_catalog.py` is the name `ports` and `fleetd` use. When `ID_SERIAL_SHORT` is a colon MAC (Espressif USB-JTAG), the board is the `boards.tsv` row with that Wi-Fi MAC, including when the cable is on a different path. A MAC with no row is `unknown`. It is not the board last recorded on that USB path. `fleetd` refuses a USB serial MAC that is not the catalog row: it is not flashed as that row, and a journal line does not skip a different name just because the USB path and image id match. CP2102 boards on this host report serial `0001`, which is not a MAC. For those, `ports` shows the row whose `usb_path` is that port, marked `(unverified)`, and MAC is `-`. That name is the last chip read on the path, not a live chip read. `ports --read` reads those chips. A known Wi-Fi MAC keeps its name and takes the USB path where it answered. The row that had that path loses it. The catalog row for an ESP32 records the Bluetooth MAC (the base plus 2), the mesh id (little-endian MAC bytes 2–5), the USB product, vid:pid, and the flash manufacturer, with `flash_ok` true and `push_ok` false. A new ESP32 is the next `node-N`. `boards.tsv` keeps name, Wi-Fi MAC, chip, USB path, and notes; a new ESP32's note includes the mesh id and the flash manufacturer. An ESP32-S3 fills the one cam row whose Wi-Fi MAC is empty or `unknown`. Otherwise a new S3 is the lowest free `toby-cam-N` with `flash_ok` true, and the old path is released. That includes `toby-cam-1`: the ESPHome `toby-cam-1` on the wall has no catalog row, so attached while `fleetd` runs it would be learned and flashed like any S3. A mesh row that is not plugged in does not hold the others. A mesh row present as `/dev/ttyACM*` still holds every mesh flash, because a board on `0xA8` and a board on `0xA1` cannot talk. Attached ESP32s on `/dev/ttyUSB*` are then `flash`, once per image id, with `node-0` first when it is one of them. `cam-flash` reads the chip itself, so a cam is named by its MAC on any path. `fleetd` calls `cam-flash` for each S3 port, which still needs BOOT held and RST tapped. A failed attempt waits until that port disappears and returns. A journal line in `data/fleet/applied.jsonl` leaves the port closed when the image id and its `wifi_mac` match the catalog row's. A line with no `wifi_mac` (older than 2026-11-02) needs the USB path, the image id, and the name to match. Deleting that line makes the board eligible again. Plan lines separate fields with `|`, so an empty catalog name does not shift the debug log. Debug lines go to stderr and `fleetd.log`. A failed build, identify, or flash writes `fleetd-<utc>-<name>.log` in the repo root. `/*.log` ignores both. `push_ok` in the catalog is false. This command does not call `daemon ota` or `cam-push`. At the end of every poll it runs `tools/sync_panel_cams.py` without `--verbose`. It logs only when the page changes or the script fails. A sync failure does not stop a flash. That step opens no serial port. `tools/mesh.sh cams` is the same script with `--verbose`. What it writes is in [the cam note](devices/keyestudio-mb0200-s3-cam.md#catalog-sync). `fleetd` ran on 2026-11-02, and `cam-flash` of `toby-cam-0` exited success ([device note](devices/keyestudio-mb0200-s3-cam.md#rust-image)). `python3 tools/tests/test_fleetd.py` has 26 tests and opens no port. `PortGuards` and `BoardMac`, 10 of the 28 tests in `tools/tests/test_mesh_tool.py`, open no port. `tools/tests/test_fleet_status.py` has 11 tests and opens no socket. `tools/tests/test_fleet_catalog.py` has 5 tests. All four passed on 2026-11-02. The move test uses the chips from the 2026-11-02 run: path `7` is `node-0`'s MAC `70:4b:ca:04:21:64`, path `12` is `20:9b:a9:99:32:0c`, and path `11` is `7c:9e:bd:ed:bd:98`, an ESP32. The S3 test fills `toby-cam-0` and stores the next board as `toby-cam-1` with `flash_ok` true.

Later on 2026-11-02 the identity is the chip MAC everywhere, and the USB socket is only where a board was last seen. Before that change, `ports` printed `node-0` and `node-1` for whatever CP2102 boards sat in sockets `7` and `11`, `flash` refused a known board in a socket another board had used, and `cam-flash` admitted only `toby-cam-0`. That made the rotated boards look like the same two nodes. What changed:

- `tools/mesh.sh ports` marks a CP2102 board `(unverified)`. `ports --read` runs `identify` on those ports, and `identify` records the chip in `devices.jsonl` and `boards.tsv` by MAC.
- `flash` and `cam-flash` read the chip with `flash-id`. A known MAC keeps its name and takes the socket. A new ESP32 becomes the next `node-N` and a new ESP32-S3 the next `toby-cam-N`.
- `cam-flash` reserves the cam's own address (its existing reservation, or the lowest free one in `10.87.0.21`–`49`). A reservation already on file skips `sudo`. Otherwise it runs `sudo tools/setup_iot_ap.sh reserve`, and a failed reserve stops before the write. The Rust cam image takes that address by DHCP instead of a static `10.87.0.21`. After the write, `wait_cam_on_panel` runs `tools/sync_panel_cams.py` every 5 s for up to 2 minutes until that address answers on 6053 and is in `data/panel/cams.json`. Otherwise it prints `run: tools/mesh.sh cams`. `fleetd` waits on it too, since it calls `cam-flash` ([cam note](devices/keyestudio-mb0200-s3-cam.md#catalog-sync)).
- Every learned S3 has `flash_ok` true, so `fleetd` flashes each cam. The name `cam-N` is gone.
- Journal lines carry `wifi_mac`, and a line with one matches only that chip. The four lines before the change were given their MAC from the catalog.
- `fleetd` builds the cam image at start whatever is plugged in. The path-`11` check is gone.

`tools/mesh.sh fleet` (`tools/fleet_status.py`) prints one row per catalog board: whether its last journal image is the current build, whether its mesh id is in `data/mesh/map.json`, and each cam's address and port `6053`. It also lists mesh ids in the map that the catalog does not name. An unreadable line in `data/fleet/applied.jsonl` is skipped, and the count goes to stderr. On 2026-11-02 it showed `node-1` and `node-4` never written by `fleetd`, and `node-4` (`289f97a9`) absent from the map. The map held `642104ca`, `541e99a9`, `0c3299a9`, and `98bdedbd`. An earlier `fleetd` had logged `skip` for `node-4` on path `12` for its whole presence. `ports --read` that night read `/dev/ttyUSB0` as `node-0` and `/dev/ttyUSB1` as `node-1`.

`flash`, `monitor`, `backup`, `bench`, `daemon run`, `daemon gatt`, and `daemon ota` open only a `/dev/ttyUSB*` port that is a CP2102 (`10c4:ea60`) in any socket, or another bridge whose USB path last read as `esp32` in `firmware/mesh/boards.tsv`. An Espressif native-USB port is refused. `tools/mesh.sh ports` names it and opens nothing; `tools/mesh.sh identify` reads the chip and records it. The name alone proves nothing, because the MB0200 cam's CH340 port is also a `ttyUSB`, and opening an S3 cam's port can drop it into download mode. `flash` tries 3 times. Before it writes, it reads the chip and MAC, refuses anything but a classic ESP32, records the chip by MAC (a new one is the next `node-N`), then allows that board's Bluetooth address on `tallow-iot` ([board note](devices/esp32-devkit.md#flash)). `cam-flash` and `cam-backup` refuse a CP2102 port. `cam-flash` then reads the chip, refuses anything but an ESP32-S3, records it by MAC (a new one is the next `toby-cam-N`), reserves its address, and allows the station MAC and the Bluetooth address ([cam note](devices/keyestudio-mb0200-s3-cam.md#rust-image)). Port numbers can swap, so run `ports` first. Opening a CP2102 port resets that board, so all seven reboot it. `flash` passes this build's bootloader and erases otadata; that command has not been run since the bootloader flag was added ([Source ahead of the boards](#source-ahead-of-the-boards-frame-version-0xa8)). `backup` is described in the [board note](devices/esp32-devkit.md#back-up).

`tools/mesh.sh daemon run` (that is, `tallowd run`) also takes `--set WINDOW INTERVAL PERIOD LENGTH`, which sets scan settings for every node. A run without it sends the default scan, so `--set` lasts until the next run. `--trial` steps through the trial settings, counts delivery at each setting, and sends `DEFAULT_SCAN` when it ends. It exits non-zero if any setting delivers under `--pass-mark` percent (default 95).

Delivery is counted by `delivery` in `host/tallowd/src/trial.rs` (the same count as the archived `mesh_host.py`). The expected count for a node is the larger of the sequence span and 90% of the hold time times the probe rate. A node that reported stats over the air but sent no probes counts as 0%. So a node that stops partway, or sends nothing, fails `--trial`. The root's own probes never cross the air, so it is not counted. The percentages under [Measured](#measured-2026-10-31) used the span alone, before this change, and are left as they were. Under that count a node that stopped partway still scored 100%. `tallowd run` takes `--port` and `--bluez`, `--out`, `--seconds`, `--irks`, `--devices`, `--rooms`, `--set`, and `--watch`. With `--trial` it also takes `--hold`, `--hz`, `--size`, and `--pass-mark`. `tallowd run --trial` counts the same way (`host/tallowd/src/trial.rs`). Whether it matches the Python within 3 points is the T2.7 check, not yet run.

How it works:

- **Root.** Every board runs the same image. The board that hears the host's heartbeat on serial is the root, so any board can be one. It stops being root 5 s after the last heartbeat.
- **Routes.** Each node sends a beacon every 2 s and picks its own parent from its neighbours' beacons. Cost counts hops, missed beacons, and a penalty below -85 dBm. A neighbour unheard for 11 s is gone. Four failed unicasts in a row drop a parent sooner. In the source, each real change of parent is one `Event::Parent`, counted in `parent_changes` and printed on the node's serial port as `@P <id>`. A loss with no replacement is a change and prints `@P 00000000`, whether the parent went quiet or failed its acks. A neighbour that is not the parent expiring is not. The boards print this form since the 2026-10-31 flash ([Flashed 2026-10-31](#flashed-2026-10-31-the-cross-check-build)).
- **Up.** Reports (every 5 s), names, stats (every 10 s), and delivery probes go hop by hop as ESP-NOW unicast, one frame on the air at a time.
- **Down.** The root floods two kinds of list to every node, and sends each three times. One is scan settings (BLE window and interval, inquiry period and length, probe rate and size). The other is the Classic addresses to page (Watch, up to 16). Ping, GATT, and OTA frames go to one node and need a route. Routed downlink (T1.8b), the GATT client (T1.10), and OTA (T1.9) are built and not flashed, so the boards carry only the floods.
- **Frames.** At most 250 bytes, 218 of payload. Header and payload layouts are in [`firmware/mesh/core/README.md`](../firmware/mesh/core/README.md). Sealed with ChaCha20-Poly1305 under one shared key baked in at build ([docs/04](04-firmware-liberation-and-network-isolation.md#mesh-nodes)). TTL and hops change at each forwarder, so they are not authenticated. Replay protection is per origin, with a window of 64 frames within a boot. The replay fix across reboots ([T1.6](plans/rust-conversion/p1-mesh.md#t16-replay-protection-across-reboots)) is built and not flashed. Its limits are under [Source ahead of the boards](#source-ahead-of-the-boards-frame-version-0xa8). Beacons are never forwarded, so `receive` in `firmware/mesh/core/src/mesh.rs` drops a beacon unless its source address matches its origin. A recorded beacon sent from another radio can no longer repoint a parent. That check was flashed on 2026-10-31.
- **Hearing.** BLE scan 30 ms in every 100 ms, and one 5 s Classic inquiry every 60 s. A node keeps 128 sightings and drops the one heard longest ago when full. A name goes up once and again after 5 minutes, with the maker and the class of device that came with it. `names.rs` drops an offer with no name, so a device whose adverts carry only a maker never reports one, and a Classic class of device goes up only with a name. A Classic device with no name in its EIR reports no name until Watch paging ([T1.5](plans/rust-conversion/p1-mesh.md#t15-find-classic-devices-by-address)) is flashed. The node accepts no connection. Its GATT client, built and not flashed, opens connections only to whitelisted addresses. `D` on a board's own serial port makes it visible to inquiry for 60 s, not connectable, so a bench test has something to find.
- **Inquiry.** In the source, `cut_inquiry` in `firmware/mesh/node/src/bluetooth.rs` cancels discovery 200 ms before the window closes, so a node pages nobody. Left to finish, Bluedroid follows an inquiry with a Remote Name Request to every device that has no name in its EIR. That pages a stranger, and it breaks the rule that a node connects only to whitelisted devices. The boards have run the fixed image since the 2026-10-31 flash ([Flashed 2026-10-31](#flashed-2026-10-31-the-cross-check-build)).
- **Map.** `tools/mesh.sh daemon run` writes `data/mesh/map.json` (gitignored) only while it runs: nodes with their stats, and devices with who heard them and the nearest node. It holds addresses as heard and sends nothing to any device. The same run appends each Report to the log beside that file ([Traffic log](#traffic-log)). The file does not store a join. `GET /api/mesh/map` adds the label, and a cluster once that log has one ([Not built](#not-built)). `tallowd run` writes the map through `data/mesh/map.json.tmp` at mode `0600` and renames it into place, so the Rust host's map is private. Classic RSSI, and so `nearest` for a Classic device, were wrong in every map written before the 2026-10-31 flash ([Flashed 2026-10-31](#flashed-2026-10-31-the-cross-check-build)). No map written since has been checked on the boards.

#### Traffic log

`tallowd run` opens `traffic.db` beside `--out` before the serial port, unless `--trial`. With `--out data/mesh/map.json` the file is `data/mesh/traffic.db`. `data/` is gitignored. The database and its `-wal` and `-shm` files are mode `0600`, and SQLite runs in WAL mode (`host/tallowd/src/traffic.rs`). A preview keeps the file beside its `mesh-map.json`.

An empty log copies the map already on disk: one sighting for each `heard_by` entry, and each device name. It prints `traffic: seeded N sightings from the current map`. A missing or unreadable map copies nothing. A log that already has rows is left as it is.

Each Report appends the address, kind, node, best RSSI, hit count, and the host time minus the sighting's age. A Name frame upserts the name for that address and kind. The same address, kind, and node again, within 5 seconds and 3 dB of the last row this process kept, is skipped. A `--bluez` poll is appended the same way. The panel's mesh child does not pass `--bluez`, so its log is the mesh reports. About once a minute the writer deletes sighting rows older than 7 days, and deletes a name once that address has no sightings left. The repeat cache drops entries older than 5 seconds on that same pass. Joins are recomputed on their own thread, not on the serial read: the first pass is 30 seconds after the log opens, then every 5 minutes. A missing file or a locked database is logged and the thread keeps going (`host/tallowd/src/cluster.rs`). A log that will not open, or that fails ten writes in a row, is turned off and `tallowd run` keeps writing the map. `devices.toml` is not rewritten. The file stays on this host.

`clusters` stores `ble-random` addresses the sightings treat as one radio. A handoff is one chain: an address has at most one successor and one predecessor, and a handoff with two candidates joins neither. The later address has to start within 20 seconds before the earlier one ends and 2 minutes after. Each side needs two hearings in a 2-minute window from one node, both medians at or above -85 dBm, and every node that heard both sides has to agree within 6 dB. A node at -75 dBm or louder that heard only one side blocks the join. Walking the chain, an address whose envelope overlaps another member by more than 2 minutes starts a new group. A stored group spans at least 30 minutes. A new group takes its earliest address as the seed. A group that already had a seed keeps it, including after that address's sightings age out. A public or classic address is not joined. A database with no `clusters` table has no joins.

The panel's mesh child is that same `tallowd run`, so a running panel writes the log. On the evening of 2026-11-02 the panel was running as root from `host/target/release/tallowd`, and `data/mesh/map.json` was mode `0600` owned by root. The tree-review tasks did not restart it. `tools/mesh.sh daemon build` rewrites that file, and the panel respawns its map writer from its own path ([AGENTS.md](../AGENTS.md)). The firmware is unchanged. Report and Name frames already carry these fields. `VERSION` in source is still `0xA8`, the boards still run `0xA1`, and nothing was flashed.

The page is `/traffic` (`app/traffic_page.html`), the Traffic tile between Devices and Settings. Without the cookie the page is 303 to `/login` and `GET /api/traffic` is 401 `{"detail":"login"}`. `POST /api/traffic` is 405. The agent socket answers 404 `{"detail":"not found"}` for the page, `GET /api/traffic`, the layout and clear posts, and the timeline files ([cam routes](devices/keyestudio-mb0200-s3-cam.md#routes)). A GET of the layout or clear path is 405 `Method Not Allowed` on both doors, because those routes are POST only. `GET /api/traffic` defaults to `window=live`, the last 15 minutes. `hour` and `day` are the other windows. Anything else is 400 `window must be live, hour, or day`. A missing database is an empty picture.

Tracks are computed on that read. A gap longer than 45 seconds starts a new stay. A new node has to lead by 3 dB, and a tie keeps the current node. A reading older than 60 seconds cannot hold the place. Two or more nodes is a move. One node, every stay at most 20 seconds, and the last one silent for more than 45 seconds, is a pass-by. Otherwise it is a linger. Joined addresses are one track. An address with no identity, IRK, or name from the air is shown as a stable two-word name from its MAC, and a joined group uses the name of its seed. The same MAC always yields the same name. An identity name, then an IRK name, then a name from the air, replaces that generated name. The generated name does not count as named, so a one-ping pass-by still sits on the "Passed by" band until "Show one-ping addresses" is checked, and that band shows the generated name. A named pass-by keeps its own row. `devices.toml` and `irks.toml` beside the log supply the identity and the IRK name, a Name frame supplies the name from the air, and `rooms.toml` labels the nodes. A missing or unreadable file leaves that part to the generated name. More than 100,000 samples in the window sets `truncated`, and the page shows the latest part. Each track also carries `samples`, each `{t, node, rssi}`.

Dragging a node sends `POST /api/traffic/layout` with `{"nodes": {"<id>": {"x", "y"}}}` (each coordinate from 0 to 1, at most 64 nodes, and only that field) and stores `layout.json` beside the log at mode `0600`. A bad body is 400 and the file stays. Clear log is `POST /api/traffic/clear`, which deletes the sightings, the names, and the clusters with `secure_delete` and then truncates the WAL. A failed clear tells the page the log was not cleared and does not reload. Each node in the picture has `parent` and `parent_rssi` when `map.json` beside the log, or the preview's `mesh-map.json`, has both. A parent of 0 is left off. The map is a 1000 by 640 view. A dot is placed from the fresh samples. A reading older than 60 seconds, at or below -100 dBm, or a Classic -127, is ignored. One listener: the dot sits 36px beside that node. Two or more listeners use a path-loss exponent fitted from placed node-to-parent links whose RSSI is from -95 to -20 and whose layout distance is at least 0.02. Fewer than two such links, or a fit that is not finite, leaves the exponent at 2.7. A finite fit is clamped to 1.5–4.5. Distances use canvas proportion: stored `x`, and `y` multiplied by 640/1000, then converted back before the dot is scaled to pixels. Two listeners split the segment by `d1/d2 = 10^((rssi2-rssi1)/(10*n))`. Three or more run a short Gauss-Newton fit of position and transmit level, eight iterations, and fall back to the two loudest. The lanes are vis-timeline 7.7.3, Apache-2.0 or MIT, vendored as `app/vendor/vis-timeline-graph2d.min.js` and `.css` ([`app/vendor/VERSIONS.tsv`](../app/vendor/VERSIONS.tsv)) and served at `/vendor/vis-timeline.js` and `/vendor/vis-timeline.css`. Those files need the cookie. The live window reloads every 5 seconds and follows the current time until the user drags the marker more than 5 seconds behind the latest sample. A window button starts following again.

Tests are the unit tests in `host/tallowd/src/traffic.rs` and `host/tallowd/src/cluster.rs`, the panel route test in `host/tallowd/src/panel/tests.rs`, `tools/tests/test_traffic_page.py` (node harness `traffic_page_harness.js`, at least 16 `ok` lines, skipped when node is absent), and `traffic_routes` in `tools/tests/panel_contract.py`. That contract asks the socket for `POST /api/traffic/layout` and `POST /api/traffic/clear` (and GET of the page, `GET /api/traffic`, and `GET /vendor/vis-timeline.js`) and expects 404 `{"detail":"not found"}`. It sends no GET to the layout or clear paths. It has not been run against a live preview. `tools/mesh.sh check` runs the page test with the other panel page tests. On 2026-11-02 the tree-review tasks ran `tools/mesh.sh check --fast` after each change. `daemon build` and the HTTP contract were skipped while the live panel's map writer was `host/target/release/tallowd`.

#### Host adapter as a node

`tallowd run --bluez [hciN]` runs a scanner (`host/tallowd/src/local.rs`) on the host's Bluetooth adapter, the seed's by default (`tallow_govee::ble::seed_adapter`), through BlueZ. Its reports go into the same map as a node whose id is the last four bytes of the adapter's address, at rank 0 and no hops. Mesh boards reach the root over ESP-NOW, which an adapter cannot hear, so the adapter is a listener beside the mesh, not the root of it. It needs no board: `tallowd run --bluez --out <path>` alone makes a map, and `--trial`, `--set`, and `--watch` (which go to boards) are refused without `--port`. With both, one run reads the port and the adapter.

BlueZ keeps one record per device with the last RSSI. The scanner polls those records every 2 s, so a sighting's count is the polls that found one, not the adverts heard, and its age is zero. A device BlueZ has no RSSI for is skipped. The adapter's `adverts` and `classic` stats count those polls, and its heap and radio fields are zero. If bluetoothd goes away the scanner prints a line to stderr and retries every 10 s. Batches go through a channel of 256. A full channel drops the batch and the scanner keeps going. Every 100th drop, or once a minute, it offers one note (`host/tallowd/src/local.rs`). The panel's mesh child does not pass `--bluez`. The seed adapter stays with the H7020 session ([Not built](#not-built)). `tools/mesh.sh daemon check` (fmt, clippy `-D warnings`) and `daemon build` passed with it. That check reported 296 tests before `src/traffic.rs`. The count after the log is under [Traffic log](#traffic-log). On 2026-11-01 a 10 s run of the release build, `host/target/release/tallowd run --bluez`, on the dongle ended with 3 devices and 1 node (earlier, it heard `Govee_H7020_3E4A`). It has not run beside a board, or for long.

`tools/mesh.sh daemon run` runs in a container that has libdbus but no system bus socket, and always passes `--port`, so it is board-only. Run `--bluez` from the host build, `host/target/release/tallowd`, when the light is not using the dongle.

#### Flashed 2026-10-31: the cross-check build

A cross-check on 2026-10-31 found five faults in the node firmware and its core. Both boards were flashed with the fixed image that day, frame version `0xA1`. Nobody has read them back since, so the fixes are confirmed by source and tests only. None of the fixes changes bytes on the air, so this image talks to the one before it.

- **Classic RSSI and class of device were wrong.** `PropData::prop()` in `esp-idf-svc` reads them from the pointer `val` instead of through it. So every Classic RSSI and class of device recorded so far is the low bits of an address. BLE RSSI came from another path and was fine. `read_props` in `firmware/mesh/node/src/bluetooth.rs` reads through the pointer. Classic RSSI and class of device recorded before that flash are meaningless, and so is any `nearest` for a Classic device in a map written before it. Counts of Classic hits and names are unaffected. Readings since the flash have not been checked.
- **A Classic inquiry paged strangers.** After the window closed, Bluedroid sent a Remote Name Request to every device with no name in its EIR. That broke the rule that a node connects only to whitelisted devices. `cut_inquiry` now cancels discovery 200 ms before the window closes, so nodes page nobody. A Classic device with no name in its EIR reports no name until Watch paging ([T1.5](plans/rust-conversion/p1-mesh.md#t15-find-classic-devices-by-address)) is flashed.
- **A beacon from the wrong radio was accepted.** A neighbour's address comes from the frame's source, which the seal does not cover. `receive` now drops a beacon unless its source matches its origin, and counts it in `rx_bad`.
- **Parent events were wrong.** `Event::Parent` now fires once per real change, including a loss with no replacement, and `parent_changes` counts each. A non-parent neighbour expiring no longer prints a false `@P 00000000`. A parent lost to failed acks now prints it.
- **A late sighting read as 255 s old.** A sighting stamped after the main loop read its clock now reports age 0.

**You:** Read each board back before trusting any Classic RSSI. Run `tools/mesh.sh monitor <port>` and a `D` on one board, then check `data/mesh/map.json` for a Classic RSSI between -100 and -20, which is the T1.4 check. Nothing in the repo prints a board's frame version, so the flash itself is the only proof that it took.

#### Source ahead of the boards (frame version 0xA8)

`VERSION` in `firmware/mesh/core/src/wire.rs` is `0xA8`. The boards run `0xA1`. A board refuses any frame whose version byte is not its own, so the two versions cannot hear each other. **Flash every board in one pass.** Until then a board on the new image and one on the old cannot talk. `tallowd` built from this source prints `0xa8` in its banner. That is the source's version, not a board's.

Built and not flashed, all in the `0xA8` source:

- **Route frames (T1.2).** `FrameKind::Route` (7). Each parent change goes up with its time, the old and new parent, and its reason. A node with no parent keeps the last 4 changes and sends them when it regains one.
- **Bench deny list (T1.3).** `X <id>` on a node's serial port refuses a root id, up to 4 of them. `tools/mesh.sh bench` sends those lines and prints the `@` lines.
- **Raw GAP callback (T1.4).** A raw Classic callback (`on_gap` in `firmware/mesh/node/src/bluetooth.rs`) replaces `EspGap`. Its Classic RSSI check is the T1.4 hardware check.
- **Watch paging (T1.5).** The root floods up to 16 Classic addresses. Each node pages them in turn, no faster than one page every 5 s, and gives up on a page after 6 s.
- **Replay across reboots (T1.6).** A boot id is a 20-bit counter kept in NVS, plus 12 random bits. Limits: the origin table is RAM only. A board that has just rebooted accepts one recorded frame per origin until that origin's next real frame, about 2 s for a neighbour's beacon. An origin silent for 10 minutes can be replayed once. A board with erased NVS starts from a random counter, and neighbours that hold a higher counter refuse it for up to 10 minutes.
- **Stagger (T1.7).** Nodes given a Watch list each wait `id mod 5 s` before their first page, so they do not all page at once.
- **Downward routing (T1.8b).** Each node learns the next hop toward each origin from upward traffic. The table has 32 entries, each good for 60 s. A frame down to one node goes hop by hop as unicast.
- **GATT client (T1.10).** The root sends requests to one node, and the node runs them against a whitelist of up to 8 addresses: connect, write, read, subscribe, disconnect, and a whitelist replace. Answers, notifications, and a closed link come back up. `gatt::encode` writes the request layouts `parse` already accepts, and `Reply::decode` reads what `Reply::encode` writes. No new operation byte. `VERSION` is `0xA8`: OTA added kinds 9 and 13 at `0xA7`, and `0xA8` put the product and version into OTA Begin. The client is built and not flashed. `tallowd gatt --port --target` takes `whitelist`, `connect`, `write`, `read`, `subscribe`, or `disconnect`. It formats `G <8 hex id> <hex request>`, refuses a line whose text before the newline is longer than 400 characters, and a 170-byte write is 361 characters. `host/tallowd/src/decode.rs` calls `Reply::decode`. `map.rs` has an explicit `FrameKind::GattUp` arm that does not record a sighting. `tools/mesh.sh daemon gatt` is wired like `daemon run` and has not been run. T1.11 is still unmarked. Frame bytes are in [`firmware/mesh/core/README.md`](../firmware/mesh/core/README.md).
- **OTA (T1.9, G6–G8).** `FrameKind::Ota` (9) goes down to one node. `FrameKind::OtaUp` (13) comes back. `ota::Receiver` in `firmware/mesh/core/src/ota.rs` takes chunks in order, hashes the bytes it accepted, and yields a finish only when that hash and the signature both match. A chunk ahead of the cursor stays missing and a Status bitmap names it. The receiver is 1,752 bytes. The node (`firmware/mesh/node/src/ota.rs`) runs it on the main loop. `Finish` is the only path that calls `esp_ota_end` and `esp_ota_set_boot_partition`. A new image is marked valid only after 5 minutes with a parent or as root, with free heap and minimum free heap both at least 60 KB. A leaf also needs an acknowledged frame in that window; the root does not. `node/build.rs` bakes the mesh key and `firmware/mesh/ota.pub`, mounted read-only at `/run/ota.pub`. It does not bake `ota.key`. `sdkconfig.defaults` sets `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`. `ed25519-compact` is built with the `opt_size` feature so the node image links on Xtensa. The root accepts `O <id> <hex>` the way it accepts `G`, and prints `@O refused` when `send_down` returns false. `tallowd ota --port --target --image --sig` sends those `O` lines. `tools/mesh.sh daemon ota` is wired like `daemon gatt` and mounts only the named image and signature. `tools/mesh.sh ota-image` writes `firmware/mesh/node/target/ota.bin`. `tools/mesh.sh ota-sign <image> <node|cam> <version>` (`cam0` is the same product) signs the image's SHA-256 with `openssl pkeyutl -sign -rawin`, over the product and version as well, and writes the 69-byte `<image>.sig`. Podman 5.8.7 rejects `uid=` and `gid=` on its tmpfs. The evening cam signing is in the [cam note](devices/keyestudio-mb0200-s3-cam.md#signed-push). It has not signed a node image onto a mesh board. The image is private because it contains the mesh key ([docs/04](04-firmware-liberation-and-network-isolation.md#mesh-nodes)). None of this was run on a board, and no board was flashed. `tools/mesh.sh flash` passes `--bootloader target/xtensa-esp32-espidf/release/bootloader.bin` and `--erase-parts otadata`. espflash 4.4.0 has `--bootloader`. The built bootloader's `bootloader_utility_get_selected_boot_partition` grew from `0xde` to `0x126` bytes and writes the pending-verify and aborted states. That flash has not been run. Frame bytes are in [`firmware/mesh/core/README.md`](../firmware/mesh/core/README.md).

Size, x86-64 sim test on 2026-11-01: `Mesh`, the sightings table, the GATT client, and the OTA receiver come to 10,360 bytes, under a 12 KB budget. The budget was 8 KB before T1.8b. The receiver is 1,752 of those bytes. Before G8's `O` parser, `tools/mesh.sh size` reported 1,498,320 of 1,835,008 bytes (81.65%). After that parser, `tools/mesh.sh size` and `ota-image` report 1,498,400 of 1,835,008 bytes (81.66%). The 2026-10-31 image, 1,452,864 bytes (79.17%), is the figure in [Measured](#measured-2026-10-31).

#### Measured (2026-10-31)

Two boards on the air, `node-0` as root and `node-1` as leaf ([boards](devices/esp32-devkit.md#boards)). The sizes, delivery, and soak results below were measured on the image from before the fixes above.

Where the tests are: unit tests in `firmware/mesh/core/src/`; `core/tests/golden.rs` for frame bytes and decoding; `core/tests/routing.rs` for parent changes one node at a time; `core/tests/sim.rs` on a simulated radio (four hops, reroute around a dead board, a replay window, forged-frame rejection, clock wrap); the banner and the serial link in `host/tallowd`; and what the host decodes, keeps, and counts as delivered in `host/tallowd`. At the 2026-10-31 cross-check that was 15 core unit, 26 golden, 7 routing, 15 simulated-radio, 15 daemon, and 16 Python host tests (the Python host tests are archived). On 2026-11-01 `tools/mesh.sh check --fast` printed core 48 unit, 43 golden, 10 routing, and 80 simulated-radio; `tallowd` 240; `tallow-broker` 27; `tallow-govee` 5 unit and 2 vector. `tools/mesh.sh check` runs all of them.

| Size | Value |
| --- | --- |
| Mesh state | 7,320 bytes: `Mesh` 5,256 + `Sightings<128>` 2,064. esphome-mesh wanted 116 KB. This is `size_of` in the x86-64 host test, and the name table is not in it |
| Image | 1,435,552 of 1,835,008 bytes (78.23%) |
| Static DRAM | 59,028 bytes (`.dram0.data` 25,108 + `.bss` 33,920). ESPHome node: 69,204 |
| IRAM | Overflowed by 2,628 bytes until `CONFIG_ESP_WIFI_RX_IRAM_OPT=n` |
| Heap | About 114 KB free, lowest seen 106,480 bytes across soaks, against a 40 KB pass mark |

The table above is the image from before the 2026-10-31 fixes. Later that day `tools/mesh.sh size` reported 1,452,864 of 1,835,008 bytes (79.17%). `Mesh` is 6,064 bytes, `Sightings<128>` is 2,064, and the GATT `Client` is 480. Those three plus the OTA receiver are the 10,360 bytes, and the current image is 1,498,400 bytes, both in [Source ahead of the boards](#source-ahead-of-the-boards-frame-version-0xa8).

Delivery from leaf to root at 10 frames/s of 200 bytes, 45 s per setting. The pass mark was 95%. These counts used the sequence span as the expected count. `--trial` counts differently now (see the `delivery` paragraph above), so a new run is not comparable row for row. The last row includes the airtime of the name requests the old image sent after each inquiry.

| Setting | Delivered |
| --- | --- |
| Radio alone | 450/450 |
| BLE 30/100 ms | 448/448 |
| BLE 60/100 ms | 448/448 |
| BLE 100/100 ms | 429/440 (97.50%) |
| BLE 30/100 ms plus Classic inquiry | 439/449 (97.77%) |

- Soaks: 10-minute runs had 0 resets, 0 `rx_bad`, every inquiry completed, and up to 22 devices with 20 heard by both nodes.
- Root: it moved between boards with no configuration by moving the host to the other port.
- Downlink: commands are proven over the air. The leaf took an 8 s inquiry period.
- Parent stability: a clean 285 s default run had 1 parent change (the initial join at 2.6 s) and 1 drop. A 10-minute run straight after a root handover showed 5 changes and 36 drops. Those were not timestamped, so the cause is unconfirmed, though the handover is suspected. An earlier run with an 8 s inquiry period showed 9 changes. These counts were made under the old counting, before the 2026-10-31 cross-check changed it (see [Flashed 2026-10-31](#flashed-2026-10-31-the-cross-check-build)). The old code printed a false `@P 00000000` when a non-parent neighbour expired, and printed nothing for a parent lost to failed acks. The boards have counted the new way since the 2026-10-31 flash.
- Classic: inquiry found `node-1` from `node-0` (5 hits) with `D` bench visibility, and found a real Klipsch Flexus Core 100 at classic address `23:00:00:11:A0:38`. Its BLE address is `DC:00:00:11:A0:38`. On 2026-11-02 the host seed read that BLE address: GATT table, device-information strings, and the LE Audio volume state ([device note](devices/klipsch-flexus-core-100.md)). The hits and the names stand. Any Classic RSSI or class of device from these runs does not (2026-10-31: the old image read them from an address, see [Flashed 2026-10-31](#flashed-2026-10-31-the-cross-check-build)).
- BLE: named devices included an LG webOS TV and Sony WF-1000XM5 earbuds. The earbuds appeared under three rotating addresses (`53:DA:B1:35:6A:12`, `62:E8:55:65:83:8E`, `46:EE:BB:F5:85:80`). The firmware reported three addresses. A later host join of a `ble-random` trail is in [Traffic log](#traffic-log).
- Inquiry against BLE: Classic inquiry cuts BLE adverts heard, 39 adverts in 21 s with inquiry every 8 s against 168 in 21 s without. That is why the default is one 5 s inquiry every 60 s. The old image also paged every nameless Classic device after each inquiry, so those 39 adverts include that airtime. The number is as measured. The same run with `cut_inquiry` flashed has not been done.

#### Found on hardware or in source

- `EspNow::take()` calls `esp_wifi_set_ps(0)`, which conflicts with Bluetooth. So `firmware/mesh/node/src/radio.rs` uses raw `esp_now_*` calls.
- `esp-idf-svc`'s Classic name readers run `from_utf8_unchecked` on over-the-air bytes. So EIR goes through the checked parser in `firmware/mesh/core/src/ad.rs`.
- `esp-idf-svc`'s `PropData::prop()` reads Classic RSSI and class of device from the pointer `val` rather than through it. So `read_props` in `firmware/mesh/node/src/bluetooth.rs` reads the raw properties itself. Found 2026-10-31, after the first Classic readings had been taken ([Flashed 2026-10-31](#flashed-2026-10-31-the-cross-check-build)).
- Bluedroid follows a finished inquiry with a Remote Name Request to every device that has no name in its EIR. That is a page to a stranger. So `cut_inquiry` cancels discovery 200 ms early.
- `esp-idf-svc` fails to compile with GATTS/GATTC off.
- std sleep below a tick busy-waits and trips the task watchdog. Hence `CONFIG_FREERTOS_HZ=1000` and `FreeRtos::delay_ms`.
- Opening a CP2102 serial port resets the board. So `tallowd run` waits 4 s and then always sends scan settings. Each run of the host therefore reboots the root. The other nodes keep their last settings across that reset.
- The boards still boot with invalid otadata. Their last flash logged `ota data partition invalid and no factory, will try all partitions`, and they boot `app0`. They have not been flashed since. The source's `tools/mesh.sh flash` now passes this build's bootloader and erases otadata. That command has not been run ([Source ahead of the boards](#source-ahead-of-the-boards-frame-version-0xa8)).

#### Not proven

- More than one physical hop. Only two boards were on the air, and 2+ hops are simulated only.
- Reroute around a dead board on real hardware.

#### Not built

- Control. On the boards the downlink sets scan settings and nothing else. Routed GATT control is built and not flashed ([Source ahead](#source-ahead-of-the-boards-frame-version-0xa8)), and no H7020 has been driven over the mesh (T1.11). Signed OTA is in that same source and has not been flashed or run, so the boards still update over USB.
- Paging known Classic addresses (Watch) is built and not flashed. A paired speaker that is not discoverable stays invisible until then.
- IRK resolution in `tallowd` (T2.4) is built, but it resolves nothing until a real IRK exists. Getting one needs the user's go-ahead. A `ble-random` trail can still be joined in the log without an IRK ([Traffic log](#traffic-log)). That join does not write `devices.toml`.
- The map on the panel. The Rust panel serves `/devices` and `GET /api/mesh/map` in code, and it is not cut over (T3.10). The page is `app/devices_page.html`, which the Rust panel includes. It renders identities and suggestions as read-only text through `textContent`. The suggestions heading says they are suggestions, and nothing accepts or merges them. `GET /api/mesh/map` already returned identities and suggestions. It also adds `label` on each device, and `cluster` plus `members` once a join is stored. `members` lists the joined addresses. `label` is an identity name from the identities already on the map, then the IRK name (`resolved`), then the name from the air, then the generated two-word name. A joined device takes that generated name from the group's seed, and `cluster` is that seed. A missing `traffic.db` still sets a per-address `label` and adds no join. The page groups cards that share a `cluster`, titles the card with `label`, and says "These addresses kept the same signal." A lone `ble-random` address that is not resolved keeps "A changing address, not a stable identity. A random address changes. Any name shown came from the air and is not checked." The join rules are in [Traffic log](#traffic-log). That page takes no POST. The empty state says the panel starts the map itself and runs it on the root board when one is plugged into this machine by USB, and shows the `host` status below. Waiting is `Waiting for the root board. Plug a mesh board into this machine by USB. The other boards only need power.` Running is `The panel is running the map on the root board. The first write can take a few seconds.` By hand, with `--mesh off`, the command is `tools/mesh.sh daemon run <port> --out data/mesh/map.json`, where `<port>` is the root board's USB serial port. The other boards only need power. The harness is `tools/tests/devices_page_harness.js`, and `tools/tests/test_devices_page.py` requires at least 18 `ok` lines.
- The mesh host in the panel (`host/tallowd/src/panel/mesh_host.rs`). Unless `--mesh off` is given, the real panel runs `tallowd run --port <port> --seconds 315360000 --out data/mesh/map.json --parent-pipe` as a child when a listed classic ESP32 is plugged in, so the map stays fresh, and the child writes the traffic log beside it ([Traffic log](#traffic-log)), with nothing started by hand. The panel resolves its own executable once and respawns `tallowd run` from that file. `tools/mesh.sh daemon build` rewrites `host/target/release/tallowd`, so start the panel from `host/target/live/tallowd` ([AGENTS.md](../AGENTS.md)). The child does not pass `--bluez`. With no board it starts no child, and the state is `waiting`. The seed adapter stays with the H7020 session ([Host adapter as a node](#host-adapter-as-a-node)). A preview never does this, and `--mesh` or `--mesh-port` with `--preview` is refused. The board is `--mesh-port`, else `/dev/tallow-root`, else the lowest `/dev/ttyUSB*` whose USB path `firmware/mesh/boards.tsv` lists as a classic ESP32. A cam is never a candidate. Nothing opens a port to look, since opening one resets the board. The child gets a pipe on stdin and exits when it closes, so a killed panel leaves no process on the port. A failed child is tried again 30 s later, and after 5 failures in 10 minutes the panel gives up until it restarts, as the service does. A child that says `Permission denied` is its own state and is retried without a limit. If another process wrote the map in the last minute when the panel starts, the panel does not start a second writer, so `tallowd.service` and the panel do not fight over the port. `GET /api/mesh/map` carries a `host` object (`state`, `port`, `restarts`, `last_exit`) only when the panel keeps a host. The states are `starting`, `waiting`, `no-permission`, `held-elsewhere`, `running`, `backoff`, `gave-up`, and `paused`. `tools/mesh.sh` `flash`, `monitor`, `bench`, `backup`, `identify`, and `daemon run|gatt|ota` create `data/mesh/pause`, which makes the panel stop its child and start another once the file is gone. The exit trap removes it. The command waits up to 10 s for the port to clear. `cam-flash` and `cam-backup` do not create the file. A holder that is not the panel, such as `tallowd.service`, keeps the port. On the evening of 2026-11-02 the panel was running as root from `host/target/release/tallowd` and its child was writing the map. The pause file and the permission state have not been checked on hardware. The unit in `tools/tallowd.service` is the alternative for a host with no panel: it restarts only on failure, 30 s apart, and stops after 5 starts in 10 minutes, because each start resets the root board. It runs under `ProtectSystem=strict`, with `data/mesh` as its only writable path, and `PrivateTmp=true`.
- Rooms are in `tallowd` (T2.6). They appear on the map only when a `rooms.toml` is given.

The [Rust conversion plan](plans/2026-10-31-rust-conversion.md) says what comes next. [Phase 1](plans/rust-conversion/p1-mesh.md) proves multi-hop on real radios, then adds Classic paging, the replay fix, routing down to one node, a signed-image check, OTA, and GATT control. [Phase 2](plans/rust-conversion/p2-host.md) moves the host end to `tallowd` and adds identities and rooms. [T3.9](plans/rust-conversion/p3-panel.md#t39-the-map-on-the-panel) puts the map on the panel.

#### esphome-mesh (removed)

[rosenrot00/esphome-mesh](https://github.com/rosenrot00/esphome-mesh) `v0.1.0-alpha.1` was tried on 2026-10-30 and removed from the repo on 2026-10-31.

On a classic ESP32 it does not link beside the proxy. Static DRAM overflows:

| Config | DRAM overflow (bytes) | MeshComponent static storage (bytes) |
| --- | --- | --- |
| As configured | 61,344 | 116,360 |
| `ip_discovery: false`, mDNS off | 27,816 | 84,952 |
| `max_nodes: 8` | 12,504 | 69,640 |

On the MB0200 cam the first builds reset within seconds, and OTA rollback restored the old image. A later build with discovery off survived past the 60 s safe-mode mark, so rollback did not apply. It then crash-looped: a heap assert in `free` during Noise handshake teardown, `internal_min=444` bytes. The cam stopped answering ARP ([device note](devices/keyestudio-mb0200-s3-cam.md#where-it-is-2026-10-31)).

### When to still buy a “top tier” dongle

- Server room has several BLE devices in the same room as the host.
- You need a reliable adapter for **RE work** (`btmon`, `bumble`, pairing experiments) without depending on a proxy.
- You want a fallback path if a proxy is offline.

For whole-house convenience of a LAN web app: **house nodes first, dongle second**.

## Connection-slot reality

| Path | Approx. concurrent GATT connections |
| --- | --- |
| One CSR USB adapter | ~5 |
| One ESP32 proxy | ~3 default (configurable, RAM-limited) |
| N proxies + local adapters via `habluetooth` | Sum of healthy scanners; automatic failover |

Cheap LED controllers often allow **only one** central connection. Close the vendor phone app before the server connects.

## Quick hardware shopping shortlist

| Role | Example |
| --- | --- |
| Local RE / nearby lights | Feasycom FSC-BP119 or Sena UD100-G03 + USB 2.0 extension |
| Mesh nodes | Classic ESP32 devkits, 4 MB flash ([devices/esp32-devkit.md](devices/esp32-devkit.md)) |
| Wired GATT reach, if proxies are wired in | 2–4× ESP32-PoE (EA antenna) flashed as ESPHome Bluetooth proxy |
| Optional long-range USB experiment | EDUP EP-B3536 (RTL8761BUE, external antenna) — treat as secondary |

## Related docs

- [02 Reverse engineering playbook](02-reverse-engineering-playbook.md)
- [05 Target webapp architecture](05-target-webapp-architecture.md)
- Upstream: [Home Assistant Bluetooth](https://www.home-assistant.io/integrations/bluetooth/), [ESPHome bluetooth_proxy](https://esphome.io/components/bluetooth_proxy/)
