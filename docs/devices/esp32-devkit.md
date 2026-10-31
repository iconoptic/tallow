# Classic ESP32 devkit

Plain ESP32 devkits with a CP2102 USB-serial chip. They are the mesh boards: no camera, no Wi-Fi join, and the same `tallow-node` image on each. Mesh design and results are in [docs/01](../01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh). The shared key is in [docs/04](../04-firmware-liberation-and-network-isolation.md#mesh-nodes).

Numbering starts at 0 from here on. The mesh's `node-0` is the first board, so `firmware/esphome/node-1.yaml` targets `node-0`, not the mesh's `node-1`.

## Boards

| Field | `node-0` | `node-1` |
| --- | --- | --- |
| Wi-Fi MAC | `70:4b:ca:04:21:64` | `20:9b:a9:99:1e:54` |
| Chip | ESP32-D0WD-V3 rev v3.1. Wi-Fi and BT, dual core, 240 MHz, 40 MHz crystal (esptool, 2026-10-30) | ESP32-D0WD-V3 |
| Flash | 4 MB (manufacturer `0x20`, device `0x4016`) | 4 MB (manufacturer `0x5e`) |
| PSRAM | None | Not recorded |
| USB chip | CP2102 (`10c4:ea60`) | CP2102 |
| USB path | `pci-0000:00:14.0-usb-0:12` | `usb-0:7` |
| Mesh id | `642104ca` | `541e99a9` |
| BT address | `70:4B:CA:04:21:66` (2026-10-31) | `20:9B:A9:99:1E:56` |
| Role in tests | Root | Leaf |
| Shipped with | Not recorded | Espressif AT firmware (IDF v3.0.3) |

Both CP2102s report serial `0001`. Identify them by USB path, not serial. A port is `/dev/ttyUSB0` when one is attached (`root:dialout 0660`; the wrappers add the group). Numbers can swap: `tools/mesh.sh ports` names each attached board from `firmware/mesh/boards.tsv`, and it opens no port.

Full 4 MB flash backups of both boards are in gitignored `data/firmware-backups/`: `209ba9991e54-before-rust.bin` and `704bca042164-before-rust.bin`.

`70:4b:ca:04:21:64` is still on the AP accept list, and an optional reservation for `10.87.0.22` is in `secrets/iot-ap/reservations`. A mesh node needs neither.

## Other boards

On 2026-10-30 this note counted a pack of five and three older ESP32 boards with about the same feature set. The other boards are not attached yet. Read each one before reusing `partitions.csv` or `board-esp32.yaml`, and check chip and flash size in the output:

```bash
ESPHOME_DEVICE=/dev/ttyUSB0 tools/esphome.sh mac
```

`mac` runs esptool `flash-id`: MAC, chip, and flash size.

## Flash

`tools/mesh.sh`'s header has the usage lines. `tools/mesh.sh key` writes the key once, and every node is built with it. The image goes on over USB. OTA over the mesh is not built. `flash` tries 3 times and refuses any port but `/dev/ttyUSB*`.

```bash
tools/mesh.sh ports
tools/mesh.sh build
tools/mesh.sh flash /dev/ttyUSB0
```

`tools/mesh.sh backup` is the step before a first flash ([Back up](#back-up)).

`espflash` writes no otadata, so boot logs `ota data partition invalid and no factory, will try all partitions`. This is harmless: it boots `app0`.

Opening the port resets the board, because the CP2102's reset line is wired to it. So each `tools/mesh.sh host` run reboots the root ([docs/01](../01-bluetooth-server-hardware-and-proxies.md#found-on-hardware-or-in-source)).

A classic ESP32 has no watchdog reset. esptool falls back to an RTS reset, which works over the CP2102. The S3's native USB problem ([MB0200 note](keyestudio-mb0200-s3-cam.md#native-usb-resets-into-download-mode)) does not apply here.

## Back up

```bash
tools/mesh.sh backup /dev/ttyUSB0
```

This saves the board's whole flash to gitignored `data/firmware-backups/<mac>-<date>.bin`. It tries 460800 baud, then 115200, and refuses to overwrite a file. Opening the port reboots the board, and it refuses any port but `/dev/ttyUSB*`. A real run took 1m55s.

## Back to ESPHome

`firmware/esphome/node-1.yaml` is on no board. It remains as a way back to ESPHome on the first board. The board package is `firmware/esphome/packages/board-esp32.yaml`: board `esp32dev`, variant `esp32`, 4 MB flash, ESPHome's default ESP-IDF (5.5.5 in the 2026-10-31 build). On 2026-10-31 `node-1.yaml` compiled at RAM 55.5% and flash 63.8%. ESPHome is 2026.9.1 (`tools/esphome.sh`), and the node is static `10.87.0.22`.

Bring up `tallow-iot` first ([docs/04](../04-firmware-liberation-and-network-isolation.md#host-access-point)) and write `firmware/esphome/secrets.yaml` with `tools/esphome.sh secrets` (it stops if the file already exists). Then:

```bash
ESPHOME_DEVICE=/dev/ttyUSB0 tools/esphome.sh mac
sudo ./tools/setup_iot_ap.sh allow 70:4b:ca:04:21:64
ESPHOME_DEVICE=/dev/ttyUSB0 tools/esphome.sh run node-1.yaml --device /dev/ttyUSB0
.venv/bin/python tools/node_check.py --address 10.87.0.22
```

`node-1.yaml` sets the static address, so no reservation is needed ([docs/04](../04-firmware-liberation-and-network-isolation.md#esp32-nodes)). The MAC is already allowed, so `allow` only confirms it.

## Where it is (2026-10-31)

Both boards run the Rust mesh. `node-0` was the root and `node-1` the leaf during tests. The root moved between them with no configuration by moving the host to the other port. Neither board joins `tallow-iot`, and no ESPHome image is on either.

Proven:

- The mesh image on both boards, in 10-minute soaks with 0 resets ([results](../01-bluetooth-server-hardware-and-proxies.md#measured-2026-10-31)).
- Delivery from leaf to root over one hop, and commands from root to leaf.
- Classic inquiry finding `node-1` from `node-0` with `D` bench visibility.

Not proven:

- More than one physical hop. Only these two boards were on the air.
- Reroute around a dead board on real hardware.
- The other boards in the pack.
- GATT. The mesh firmware has none, and no GATT was tested through an ESPHome build on these boards.

### History

The first board's earlier state. None of this is current.

- 2026-10-30: esptool read the chip over `/dev/ttyUSB0`, and the earlier `relay-1` image passed `config` and `compile`. That image used RAM 55.5% (69204 / 124580 bytes) and flash 63.8% (1171495 / 1835008 bytes, the OTA app slot of the 4 MB part).
- 2026-10-31 01:44 UTC: the board ran a proxy-only build named `relay-1`. It joined `tallow-iot` at `10.87.0.22` at -19 to -28 dBm, with the proxy active and 3 connections. Its MAC was already allowed, as the log line said.
- 2026-10-31 09:57 MDT: the board was offline: no `/dev/ttyUSB0`, not associated. `node-1` was compiled and not flashed.
- 2026-10-31, before replacement: the ESPHome baseline was `relay-1` hearing 9 BLE addresses in 30 s at -24 dBm.
