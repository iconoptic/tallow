# Classic ESP32 devkit

Plain ESP32 devkits with a CP2102 USB-serial chip. They are the mesh boards: no camera, no Wi-Fi join, and the same `tallow-node` image on each. Mesh design and results are in [docs/01](../01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh). The shared key is in [docs/04](../04-firmware-liberation-and-network-isolation.md#mesh-nodes).

Numbering starts at 0 from here on. The mesh's `node-0` is the first board, so `archive/firmware/esphome/node-1.yaml` targets `node-0`, not the mesh's `node-1`.

## Boards

| Field | `node-0` | `node-1` |
| --- | --- | --- |
| Wi-Fi MAC | `70:4b:ca:04:21:64` | `20:9b:a9:99:1e:54` |
| Chip | ESP32-D0WD-V3 rev v3.1. Wi-Fi and BT, dual core, 240 MHz, 40 MHz crystal (esptool, 2026-10-30) | ESP32-D0WD-V3 |
| Flash | 4 MB (manufacturer `0x20`, device `0x4016`) | 4 MB (manufacturer `0x5e`) |
| PSRAM | None | Not recorded |
| USB chip | CP2102 (`10c4:ea60`) | CP2102 |
| USB path | `7` in `boards.tsv` (an earlier reading was `pci-0000:00:14.0-usb-0:12`) | `11` in `boards.tsv` (an earlier reading was `usb-0:7`) |
| Mesh id | `642104ca` | `541e99a9` |
| BT address | `70:4B:CA:04:21:66` (2026-10-31) | `20:9B:A9:99:1E:56` |
| Role in tests | Root | Leaf |
| Shipped with | Not recorded | Espressif AT firmware (IDF v3.0.3) |

Both CP2102s report serial `0001`, which is not a MAC. `tools/mesh.sh ports` names each of them from the `boards.tsv` row whose `usb_path` is that port, prints USB-MAC `-`, and opens no port. That name is the last chip read on the path, not a live chip read. A colon MAC (Espressif USB-JTAG) is the row with that Wi-Fi MAC, and a MAC with no row is `unknown` ([docs/01](../01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh)). A port is `/dev/ttyUSB0` when one is attached (`root:dialout` mode `0660`). How this login joins that group is in [Flash](#flash). Numbers can swap.

Full 4 MB flash backups are in gitignored `data/firmware-backups/`: `209ba9991e54-before-rust.bin` and `704bca042164-before-rust.bin`, one per board, and `209ba9991e54-2026-10-31.bin`, a later one of `node-1`. They are mode `0600` in a `0700` directory ([Back up](#back-up)).

`70:4b:ca:04:21:64` is still on the AP accept list, and an optional reservation for `10.87.0.22` is in `secrets/iot-ap/reservations`. A mesh node needs neither.

## Other boards

On 2026-10-30 this note counted a pack of five and three older ESP32 boards with about the same feature set. `tools/fleetd.sh` has since recorded `node-2`, `node-3`, and `node-4`. The rows are in [docs/01](../01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh). On 2026-11-02 a later `tools/mesh.sh ports` still named `/dev/ttyUSB0` as `node-0` on path `7`, USB-MAC `-`. The ACM port from that run is in the [cam note](keyestudio-mb0200-s3-cam.md#toby-cam-0). Read a board before reusing `partitions.csv` or `board-esp32.yaml`, and check chip and flash size in the output:

```bash
ESPHOME_DEVICE=/dev/ttyUSB0 archive/tools/esphome.sh mac
```

`tools/mesh.sh ports` lists attached boards without opening a port. `mac` (archived wrapper) runs esptool `flash-id`: MAC, chip, and flash size.

## Flash

`tools/mesh.sh`'s header has the usage lines. `tools/mesh.sh key` writes the key once, and every node is built with it. The boards were written over USB. Signed OTA is in the source and has not been flashed or run ([docs/01](../01-bluetooth-server-hardware-and-proxies.md#source-ahead-of-the-boards-frame-version-0xa8)). `flash` tries 3 times. It, `monitor`, `backup`, `bench`, `daemon run`, `daemon gatt`, and `daemon ota` open only a `/dev/ttyUSB*` port whose USB path `firmware/mesh/boards.tsv` lists with chip `esp32`. A board that is not listed is refused until its row is added. `tools/fleetd.sh` writes the row from the chip before it flashes. By hand: read its MAC and chip with `archive/tools/esphome.sh mac` ([Other boards](#other-boards)), then add a row (name, Wi-Fi MAC, chip, USB path, notes). The name `ttyUSB` alone is not enough, because the MB0200 cam's CH340 port is also a `ttyUSB`.

Before it writes, `flash` reads the chip MAC with esptool. When that read finds no MAC, `port_mac` prints esptool's last 3 lines, then `Could not read a MAC from PORT.`, and exits. `confirm_port_mac` (`flash` and `cam-flash`) and `run_backup` (`backup`) follow the substitution with `|| exit 1`, because `exit` inside `$(port_mac)` does not stop the parent. A failed read is not compared to `boards.tsv`. When a MAC is read, the port is refused if the `wifi_mac` cell for that USB path is a different address: `PORT is SEEN, not RECORDED. USB path PATH is NAME.` The compare ignores case. An empty cell or `unknown` accepts the chip. After that check it computes the Bluetooth address as the base MAC plus 2, Espressif's assignment (`70:4b:ca:04:21:64` becomes `70:4b:ca:04:21:66`), and runs `sudo tools/setup_iot_ap.sh allow` with that address before it writes the image. `setup_iot_ap.sh allow` owns the `tallow-iot` accept list. `cam-flash` and `cam-backup` first admit the port by the USB-serial lookup in [docs/01](../01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh): a matching colon MAC is that board even on another path, and a different USB MAC is not `toby-cam-0`. `cam-flash` then uses this esptool check against the `wifi_mac` cell for that USB path, and allows the station MAC and the Bluetooth address ([MB0200 note](keyestudio-mb0200-s3-cam.md#rust-image)).

```bash
tools/mesh.sh ports
tools/mesh.sh build
tools/mesh.sh flash /dev/ttyUSB0
tools/fleetd.sh
```

`tools/fleetd.sh` reads a newly seen port with `tools/mesh.sh identify` and rewrites `firmware/mesh/devices.jsonl` and `boards.tsv` from that chip when it runs. A failed CP2102 read is not retried until the port disappears, and `fleetd` will not flash that path as the last recorded name until this presence has read the chip. A failed read of a MAC-serial port is tried again, up to three times. A USB serial MAC that is not the catalog row is refused: it is not flashed as that row, and the journal does not skip a different name because the USB path and image id match. A known Wi-Fi MAC keeps its name and takes the path where it answered. The row that had that path loses it. It then flashes the ESP32s that are on `/dev/ttyUSB*`. A node that is still enumerating as `/dev/ttyACM*` holds the mesh flashes. A row with no board attached does not. The files name `node-0` on path `7` and `node-1` on path `11`. On 2026-11-02 a later `tools/mesh.sh ports` named `/dev/ttyUSB0` `node-0` path `7` USB-MAC `-`. The learned rows, the journal, and the failure logs are in [docs/01](../01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh). Deleting that board's line in `data/fleet/applied.jsonl` makes it eligible again.

`tools/mesh.sh backup` is the step before a first flash ([Back up](#back-up)).

`/dev/ttyUSB` devices are `root:dialout` mode `0660`. `flash`, `identify`, and every other command that calls `need_classic_port` or `need_cam0_port` (`monitor`, `backup`, `bench`, `daemon run`, `daemon gatt`, `daemon ota`, `cam-flash`, `cam-backup`) join the port's group with `sg` when the account lists the user and this login's process token does not. `identify` calls that join on its own, and it is not limited to a row in `boards.tsv`. It prints one line, `Joining group NAME for PORT. This login started before that membership.`, then re-execs once with `MESH_PORT_GROUP=1`. If the port is still not writable, it exits before any tool starts: `PORT is not writable by this login (MODE USER:GROUP)`. A login started before the user was added to `dialout` cannot open the port (`EACCES`). Holding BOOT does not help. `sudo` is not the workaround.

If every flash try fails with `Failed to open serial port` and none started erasing or writing, the final message is `Flash failed on PORT: the serial port would not open. Nothing was written.` The partial-image warning remains only when a try started erasing or writing.

The boards still boot with invalid otadata. Their last flash logged `ota data partition invalid and no factory, will try all partitions`, and they boot `app0`. They have not been flashed since. The source's `flash` passes this build's bootloader and erases otadata; that command has not been run ([docs/01](../01-bluetooth-server-hardware-and-proxies.md#source-ahead-of-the-boards-frame-version-0xa8)).

Opening the port resets the board, because the CP2102's reset line is wired to it. So each `tools/mesh.sh daemon run` reboots the root ([docs/01](../01-bluetooth-server-hardware-and-proxies.md#found-on-hardware-or-in-source)).

A classic ESP32 has no watchdog reset. esptool falls back to an RTS reset, which works over the CP2102. The S3's native USB problem ([MB0200 note](keyestudio-mb0200-s3-cam.md#native-usb-resets-into-download-mode)) does not apply here.

## Back up

```bash
tools/mesh.sh backup /dev/ttyUSB0
```

This saves the board's whole flash to gitignored `data/firmware-backups/<mac>-<date>.bin`. It tries 460800 baud, then 115200, and refuses to overwrite a file. Opening the port reboots the board. It opens only a listed classic board's port, as `flash` does ([Flash](#flash)). A real run took 1m55s.

A mesh board's flash holds the mesh key, so a backup is as private as `firmware/mesh/mesh.key`. The command makes `data/firmware-backups/` mode `0700` and writes each file at `0600`. The three backups already there were chmodded to match on 2026-10-31. Do not copy one out of the tree ([docs/04](../04-firmware-liberation-and-network-isolation.md#mesh-nodes)). `tools/mesh.sh cam-backup` uses this writer after the USB-serial lookup that admits only `toby-cam-0` ([device note](keyestudio-mb0200-s3-cam.md#rust-image)). It has not been run.

## Back to ESPHome

`archive/firmware/esphome/node-1.yaml` is on no board. It remains as a way back to ESPHome on the first board; the ESPHome tree and wrapper are archived (`archive/`), so the commands below run from `archive/tools/`. The board package is `archive/firmware/esphome/packages/board-esp32.yaml`: board `esp32dev`, variant `esp32`, 4 MB flash, ESPHome's default ESP-IDF (5.5.5 in the 2026-10-31 build). On 2026-10-31 `node-1.yaml` compiled at RAM 55.5% and flash 63.8%. ESPHome is 2026.9.1 (`archive/tools/esphome.sh`), and the node is static `10.87.0.22`.

Bring up `tallow-iot` first ([docs/04](../04-firmware-liberation-and-network-isolation.md#host-access-point)) and write `archive/firmware/esphome/secrets.yaml` with `archive/tools/esphome.sh secrets` (it stops if the file already exists). Then:

```bash
ESPHOME_DEVICE=/dev/ttyUSB0 archive/tools/esphome.sh mac
sudo ./tools/setup_iot_ap.sh allow 70:4b:ca:04:21:64
ESPHOME_DEVICE=/dev/ttyUSB0 archive/tools/esphome.sh run node-1.yaml --device /dev/ttyUSB0
.venv/bin/python archive/tools/node_check.py --address 10.87.0.22
```

`node-1.yaml` sets the static address, so no reservation is needed ([docs/04](../04-firmware-liberation-and-network-isolation.md#esp32-nodes)). The MAC is already allowed, so `allow` only confirms it.

## Where it is (2026-10-31)

Both boards run the Rust mesh. `node-0` was the root and `node-1` the leaf during tests. The root moved between them with no configuration by moving the host to the other port. Neither board joins `tallow-iot`, and no ESPHome image is on either.

Both boards were flashed with the cross-check build on 2026-10-31. That build fixes Classic RSSI and class of device, stops inquiry before the name requests that paged nameless Classic devices, checks the source of each beacon, and counts parent changes once per change ([list](../01-bluetooth-server-hardware-and-proxies.md#flashed-2026-10-31-the-cross-check-build)). Nobody has read the boards back since, so those fixes are confirmed by source and tests only.

- Classic RSSI, class of device, and `nearest` for a Classic device from before that flash are wrong. Hit counts and names are fine.

**The boards and the source are on different frame versions.** The boards run frame version `0xA1`. The source is at `0xA8`, with the work from T1.2 through T1.10, including signed OTA (T1.9), in [docs/01](../01-bluetooth-server-hardware-and-proxies.md#source-ahead-of-the-boards-frame-version-0xa8). No board was flashed with `0xA8`. Frames between the two versions are refused, so a board on the new image and one on the old cannot talk. **Flash every board in one pass**, with `tools/mesh.sh build` and `tools/mesh.sh flash <port>` on each. That includes the third classic board that T1.8 needs. It is not attached.

Proven:

- The mesh image on both boards, in 10-minute soaks with 0 resets ([results](../01-bluetooth-server-hardware-and-proxies.md#measured-2026-10-31)).
- Delivery from leaf to root over one hop, and commands from root to leaf.
- Classic inquiry finding `node-1` from `node-0` with `D` bench visibility. The hit is proven. Its RSSI was wrong before the flash, and has not been checked since.

Not proven:

- More than one physical hop. Only these two boards were on the air.
- Reroute around a dead board on real hardware.
- The other boards in the pack.
- GATT. The GATT client is built and not flashed. No GATT was tested through these boards, or through an ESPHome build.

### History

The first board's earlier state. None of this is current.

- 2026-10-30: esptool read the chip over `/dev/ttyUSB0`, and the earlier `relay-1` image passed `config` and `compile`. That image used RAM 55.5% (69204 / 124580 bytes) and flash 63.8% (1171495 / 1835008 bytes, the OTA app slot of the 4 MB part).
- 2026-10-31 01:44 UTC: the board ran a proxy-only build named `relay-1`. It joined `tallow-iot` at `10.87.0.22` at -19 to -28 dBm, with the proxy active and 3 connections. Its MAC was already allowed, as the log line said.
- 2026-10-31 09:57 MDT: the board was offline: no `/dev/ttyUSB0`, not associated. `node-1` was compiled and not flashed.
- 2026-10-31, before replacement: the ESPHome baseline was `relay-1` hearing 9 BLE addresses in 30 s at -24 dBm.
