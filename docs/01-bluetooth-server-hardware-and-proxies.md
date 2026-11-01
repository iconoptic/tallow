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

This is the target for control. `habluetooth` and `bleak-esphome` are not wired, and the [mesh](#multi-hop-mesh) uses neither: `tools/mesh_host.py` reads the root's serial port.

The [Rust conversion plan](plans/2026-10-31-rust-conversion.md) replaces this stack: `tallowd` on axum for FastAPI, `bluer` for the host's own radio, GATT over the mesh for proxies, and ESPHome retired in its phase 5. None of that is built yet.

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

### Docker / container passthrough

If the controller runs in Docker:

```yaml
cap_add:
  - NET_ADMIN
  - NET_RAW
volumes:
  - /run/dbus:/run/dbus:ro
```

Without those caps, BlueZ management APIs fail and adapters cannot be reset when they wedge.

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

- A **mesh node** is a classic ESP32 running the Rust firmware in `firmware/mesh/`. It joins no Wi-Fi, and it reaches the host through other nodes. Both classic boards run this as of 2026-10-31 ([Multi-hop mesh](#multi-hop-mesh)).
- An **ESPHome node** runs `bluetooth_proxy`, joins `tallow-iot`, and talks only to the host over Wi-Fi. The cams are built this way. A Rust image in `firmware/cam` builds and is not flashed ([device note](devices/keyestudio-mb0200-s3-cam.md#rust-image)). Picking a node per device with habluetooth and bleak-esphome is not wired.

The rest of this section is the ESPHome nodes. Their names are separate from the mesh's: `firmware/esphome/node-1.yaml` targets the first board, which the mesh calls `node-0`.

The configs are in `firmware/esphome/`. They are stock ESPHome with no external components, on ESPHome's default ESP-IDF (5.5.5 in the 2026-10-31 builds). `tools/esphome.sh` builds them in the pinned Docker image `ghcr.io/esphome/esphome:2026.9.1`; nothing is installed on the host. Its header has the usage lines. Addresses are in [docs/04](04-firmware-liberation-and-network-isolation.md#esp32-nodes).

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
| `secrets.example.yaml` | Shape of the gitignored `secrets.yaml` (`wifi_ssid`, `wifi_password`, `api_key`) |

`packages/node.yaml` joins `tallow-iot` with a static `manual_ip`: the node's address, with gateway and DNS at `10.87.0.1`. `use_address` is the same address, so the host dials it directly. A node needs no lease and no reservation. There is no fallback AP and no captive portal. mDNS stays at ESPHome's default; the host does not use it. The native API is noise-encrypted with `api_key`. OTA uses the same key. `reboot_timeout` is `0s` on the API, because nothing on the host holds the API open yet. Wi-Fi keeps ESPHome's default `reboot_timeout` of 15 minutes. Time comes from `10.87.0.1`. `power_save_mode` is ESPHome's default, which compiled to `light` on 2026-10-31. Scanning is active and `bluetooth_proxy` is `active: true`, with ESPHome's default 3 GATT slots. A `wifi_signal` sensor ("Wi-Fi signal") reports the signal to the AP every 60 s. That signal sets where a node can live.

A cam sets `bluetooth_proxy: active: false`. It still forwards advertisements and holds no GATT connections. Video and BLE share one 2.4 GHz radio, so a cam takes no GATT. No board runs a GATT node as of 2026-10-31.

`power_save_mode: none` is the wrong setting on a BLE node. ESP-IDF needs Wi-Fi modem sleep while Bluetooth is on. On 2026-10-28 a cam built that way never joined Wi-Fi, and OTA rollback put the relay image back ([device note](devices/keyestudio-mb0200-s3-cam.md#first-flash)).

`tools/node_check.py` asks one node for its name, ESPHome version, Wi-Fi and BLE MACs, and proxy flags, then counts advertisements for `--seconds` (default 15). It prints the Wi-Fi signal: "not reported by this build" when the sensor is absent, "not measured yet" before the first 60 s reading. It says whether the H7020 was heard (`--target` to look for another address). It sends nothing to any light. It exits 1 when the node does not answer, runs no BLE proxy, or heard nothing. The key is the `api_key` line of `firmware/esphome/secrets.yaml`, or `--key-file`. `--address` must be in `10.87.0.0/24`.

```bash
.venv/bin/python tools/node_check.py --address 10.87.0.22
.venv/bin/python tools/node_check.py --address 10.87.0.21 --seconds 30
```

On 2026-10-28 one MB0200 ran `relay-mb0200`, then `toby-cam-1`. The standalone relay is proven: it answered `relay_check.py` (the earlier name of this check) with full proxy flags and heard 17 BLE addresses in 15 s. It did not hear the H7020 from where it sat. The cam image is scan-only, as designed. Results: [device note](devices/keyestudio-mb0200-s3-cam.md#on-the-board-2026-10-28). The host does not use ESPHome nodes yet: bleak-esphome and habluetooth are not wired in. That host merge, picking a node per light, is still unbuilt.

On 2026-10-30 `relay_check.py --address 10.87.0.21` found `toby-cam-1` up (built 2026-10-29 05:11:41 UTC), scan-only flags, 13 BLE addresses in 10 s, H7020 not heard, and "Wi-Fi signal not reported by this build". That image predates the sensor. The same day the earlier `relay-1` image passed `config` and `compile`. A later OTA of `toby-cam-1` to `10.87.0.21:3232` timed out. The board held `10.87.0.21` and did not answer ARP, so the panel could not open the stream ([device note](devices/keyestudio-mb0200-s3-cam.md#where-it-is-2026-10-30)).

On 2026-10-31 no ESPHome node is up. `toby-cam-1` is unreachable at `10.87.0.21` and not recovered, after an [esphome-mesh](#esphome-mesh-removed) build crash-looped. Its build without esphome-mesh is compiled and not flashed. `toby-cam-0` is compiled and not flashed ([device note](devices/keyestudio-mb0200-s3-cam.md#where-it-is-2026-10-31)). Both classic ESP32 boards now run the Rust mesh ([device note](devices/esp32-devkit.md#where-it-is-2026-10-31)).

The MB0200 is a Wi-Fi ESP32, rank 2 above. Board, pins, and first flash: [devices/keyestudio-mb0200-s3-cam.md](devices/keyestudio-mb0200-s3-cam.md). The mesh boards are classic ESP32 devkits: [devices/esp32-devkit.md](devices/esp32-devkit.md).

### Multi-hop mesh

Classic ESP32 boards listen to Bluetooth near them and pass what they hear to this host through each other. The link is ESP-NOW on channel 1, the cleanest in the 2026-10-31 survey and separate from `tallow-iot` on 6. A mesh node joins no Wi-Fi and opens no socket. It listens and reports. Control is not built, and neither is OTA over the mesh. `bleak-esphome` and `habluetooth` remain unwired, and this mesh uses neither.

| Path | Role |
| --- | --- |
| `firmware/mesh/core` | `no_std` mesh logic: frames, routing, sightings, names, advert parsing. It has no radio and no OS, so all of it runs under host tests |
| `firmware/mesh/node` | ESP-IDF firmware `tallow-node`, on ESP-IDF v5.5.5 like the ESPHome nodes. `src/radio.rs` is ESP-NOW, `src/bluetooth.rs` is BLE scan and Classic inquiry, `src/main.rs` is the loop and the serial port. `partitions.csv` is the ESPHome nodes' OTA layout |
| `tools/mesh.sh` | Pinned image `espressif/idf-rust:esp32_1.95.0.0`; nothing is installed on the host. Its header has the usage lines |
| `tools/mesh_host.py` | The host end, on the root's serial port. Runs through `tools/mesh.sh host` |
| `tools/tests/test_mesh_host.py` | What the host decodes and keeps |
| `tools/mesh_monitor.py` | Prints a board's serial output. Runs through `tools/mesh.sh monitor` |
| `firmware/mesh/boards.tsv` | Known boards by USB path. `tools/mesh.sh ports` reads it |
| `host/` | Cargo workspace. `host/tallowd` prints a banner and shares `tallow-mesh-core`. `src/link.rs` yields `@` lines and parses `@U`. `Link::open` waits for T2.3 (`tallowd run`). `tools/mesh_host.py` is still the host end |

```bash
tools/mesh.sh check                # every offline gate
tools/mesh.sh check --fast         # the same, skipping the firmware build
tools/mesh.sh fmt                  # format all Rust in place
tools/mesh.sh test                 # mesh core tests, on this machine
tools/mesh.sh daemon check         # fmt, clippy, and tests for host/
tools/mesh.sh daemon build         # release build of tallowd
tools/mesh.sh key                  # write the gitignored firmware/mesh/mesh.key once
tools/mesh.sh build                # release image for a classic ESP32
tools/mesh.sh size                 # image size against the OTA slot
tools/mesh.sh ports                # list attached boards; opens no port, resets nothing
tools/mesh.sh flash /dev/ttyUSB0
tools/mesh.sh monitor /dev/ttyUSB0 30    # 20 s if no time is given
tools/mesh.sh backup /dev/ttyUSB0        # whole flash to data/firmware-backups/
tools/mesh.sh host /dev/ttyUSB0 --seconds 120
tools/mesh.sh host /dev/ttyUSB0 --trial
.venv/bin/python -m unittest tools/tests/test_mesh_host.py    # pytest is not installed
```

Every command exits non-zero when its step fails. `flash`, `monitor`, `backup`, and `host` refuse any port but `/dev/ttyUSB*`: the mesh image must not land on an S3 cam, and just opening a cam's port drops it into download mode. `flash` tries 3 times. Port numbers can swap, so run `ports` first. Opening a CP2102 port resets that board, so those four all reboot it. `backup` is described in the [board note](devices/esp32-devkit.md#back-up).

`tools/mesh.sh host` also takes `--set WINDOW INTERVAL PERIOD LENGTH`, which sets scan settings for every node. A run without it sends `DEFAULT_SCAN` from `tools/mesh_host.py`, so `--set` lasts until the next run. `--trial` steps through `TRIAL` in `tools/mesh_host.py`, counts delivery at each setting, and sends `DEFAULT_SCAN` when it ends. It exits non-zero if any setting delivers under `--pass-mark` percent (default 95).

How it works:

- **Root.** Every board runs the same image. The board that hears the host's heartbeat on serial is the root, so any board can be one. It stops being root 5 s after the last heartbeat.
- **Routes.** Each node sends a beacon every 2 s and picks its own parent from its neighbours' beacons. Cost counts hops, missed beacons, and a penalty below -85 dBm. A neighbour unheard for 11 s is gone. Four failed unicasts in a row drop a parent sooner.
- **Up.** Reports (every 5 s), names, stats (every 10 s), and delivery probes go hop by hop as ESP-NOW unicast, one frame on the air at a time.
- **Down.** Scan settings flood the mesh, and the root sends each one three times. Settings are all the downlink carries: BLE window and interval, inquiry period and length, probe rate and size.
- **Frames.** At most 250 bytes, 218 of payload. Header and payload layouts are in [`firmware/mesh/core/README.md`](../firmware/mesh/core/README.md). Sealed with ChaCha20-Poly1305 under one shared key baked in at build ([docs/04](04-firmware-liberation-and-network-isolation.md#mesh-nodes)). TTL and hops change at each forwarder, so they are not authenticated. Replay protection is per origin and per boot, with a window of 64 frames. A frame carrying a different boot id counts as new, so it does not stop a frame captured in an earlier boot.
- **Hearing.** BLE scan 30 ms in every 100 ms, and one 5 s Classic inquiry every 60 s. A node keeps 128 sightings and drops the one heard longest ago when full. Name, maker, and class of device go up once and again after 5 minutes. The node offers nothing to pair with or connect to. `D` on a board's own serial port makes it visible to inquiry for 60 s, not connectable, so a bench test has something to find.
- **Map.** `tools/mesh.sh host` writes `data/mesh/map.json` (gitignored) only while it runs: nodes with their stats, and devices with who heard them and the nearest node. It holds addresses as heard and sends nothing to any device.

#### Measured (2026-10-31)

Two boards on the air, `node-0` as root and `node-1` as leaf ([boards](devices/esp32-devkit.md#boards)).

Tests: 13 core unit, 13 golden, and 15 simulated-radio. The simulated-radio tests cover four hops, reroute around a dead board, a replay window, forged-frame rejection, and clock wrap. `tools/mesh.sh check --fast` on 2026-10-31 also ran 6 daemon tests (the banner and the serial link), and the host unittest passed. 11 Python tests in `tools/tests/test_mesh_host.py`.

| Size | Value |
| --- | --- |
| Mesh state | 7,320 bytes: `Mesh` 5,256 + `Sightings<128>` 2,064. esphome-mesh wanted 116 KB. This is `size_of` in the x86-64 host test, and the name table is not in it |
| Image | 1,435,552 of 1,835,008 bytes (78.23%) |
| Static DRAM | 59,028 bytes (`.dram0.data` 25,108 + `.bss` 33,920). ESPHome node: 69,204 |
| IRAM | Overflowed by 2,628 bytes until `CONFIG_ESP_WIFI_RX_IRAM_OPT=n` |
| Heap | About 114 KB free, lowest seen 106,480 bytes across soaks, against a 40 KB pass mark |

Delivery from leaf to root at 10 frames/s of 200 bytes, 45 s per setting. The pass mark was 95%.

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
- Parent stability: a clean 285 s default run had 1 parent change (the initial join at 2.6 s) and 1 drop. A 10-minute run straight after a root handover showed 5 changes and 36 drops. Those were not timestamped, so the cause is unconfirmed, though the handover is suspected. An earlier run with an 8 s inquiry period showed 9 changes.
- Classic: inquiry found `node-1` from `node-0` (5 hits) with `D` bench visibility, and found a real Klipsch Flexus Core 100 at classic address `23:00:00:11:A0:38`. Its BLE address is `DC:00:00:11:A0:38`.
- BLE: named devices included an LG webOS TV and Sony WF-1000XM5 earbuds. The earbuds appeared under three rotating addresses (`53:DA:B1:35:6A:12`, `62:E8:55:65:83:8E`, `46:EE:BB:F5:85:80`), which proves the rotating-address limit.
- Inquiry against BLE: Classic inquiry cuts BLE adverts heard, 39 adverts in 21 s with inquiry every 8 s against 168 in 21 s without. That is why the default is one 5 s inquiry every 60 s.

#### Found on hardware or in source

- `EspNow::take()` calls `esp_wifi_set_ps(0)`, which conflicts with Bluetooth. So `firmware/mesh/node/src/radio.rs` uses raw `esp_now_*` calls.
- `esp-idf-svc`'s Classic name readers run `from_utf8_unchecked` on over-the-air bytes. So EIR goes through the checked parser in `firmware/mesh/core/src/ad.rs`.
- `esp-idf-svc` fails to compile with GATTS/GATTC off.
- std sleep below a tick busy-waits and trips the task watchdog. Hence `CONFIG_FREERTOS_HZ=1000` and `FreeRtos::delay_ms`.
- Opening a CP2102 serial port resets the board. So `tools/mesh_host.py` waits 4 s and then always sends scan settings. Each run of the host tool therefore reboots the root. The other nodes keep their last settings across that reset.
- `espflash` writes no otadata. Boot logs `ota data partition invalid and no factory, will try all partitions`. This is harmless: it boots `app0`.

#### Not proven

- More than one physical hop. Only two boards were on the air, and 2+ hops are simulated only.
- Reroute around a dead board on real hardware.

#### Not built

- Control. The downlink sets scan settings and nothing else.
- OTA over the mesh. Nodes flash over USB.
- Paging known Classic addresses. A paired speaker that is not discoverable stays invisible on Classic.
- IRK resolution. A rotating address looks like a new device.
- The map on the panel. No service runs `tools/mesh.sh host`, so `data/mesh/map.json` updates only during a manual run.
- Rooms. The map has node ids, not rooms.

The [Rust conversion plan](plans/2026-10-31-rust-conversion.md) says what comes next. [Phase 1](plans/rust-conversion/p1-mesh.md) proves multi-hop on real radios, then adds Classic paging, the replay fix, OTA, and GATT control. [Phase 2](plans/rust-conversion/p2-host.md) moves the host end to `tallowd` and adds identities and rooms. [T3.9](plans/rust-conversion/p3-panel.md#t39-the-map-on-the-panel) puts the map on the panel.

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
