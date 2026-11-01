# Keyestudio MB0200 S3 CAM

ESP32-S3 board with an OV3660 camera, an SD slot, and two USB-C ports. Here it is a node, and `toby-cam-1` is the node that watches the dog. A second board, [`toby-cam-0`](#toby-cam-0), has an ESPHome image compiled and not flashed (the ESPHome tree is archived in `archive/firmware/esphome/`, and the ESPHome commands in this note now run from `archive/tools/`). A Rust image builds and is not flashed ([Rust image](#rust-image)). Node design is in [docs/01](../01-bluetooth-server-hardware-and-proxies.md#tallow-nodes). Addresses and ports are in [docs/04](../04-firmware-liberation-and-network-isolation.md#esp32-nodes).

On 2026-10-28 this unit ran `relay-mb0200`, then `toby-cam-1`. The relay is proven. The cam streams and the panel proxy was checked against it, but its picture is dark and the page is not checked in a browser. After the board was moved, a weak USB supply made it unreachable, and a burst of connects later jammed its web server for minutes. On 2026-10-30 the board held `10.87.0.21` and did not answer ARP for it. The cams page returned 502, and an OTA to that address timed out. On 2026-10-31 it took an esphome-mesh build and crash-looped, and it is still unreachable and not recovered ([Where it is](#where-it-is-2026-10-31)). Results are in [On the board](#on-the-board-2026-10-28), [Power](#power), and [The one-slot jam](#the-one-slot-jam).

## This unit

| Field | Value |
| --- | --- |
| Board | Keyestudio MB0200 S3 CAM |
| Module | ESP32-S3-WROOM-1-N16R8: 16 MB flash, 8 MB octal PSRAM |
| Sensor | OV3660 |
| USB | CH340C UART on one Type-C. Native USB-OTG on the other |
| Chip | ESP32-S3 (QFN56) rev v0.2, 8 MB embedded PSRAM, 40 MHz crystal (esptool) |
| Attached | USB-OTG port. USB `303a:1001` (USB-Serial/JTAG) under ESPHome; `303a:4001` under the earlier project's TinyUSB firmware. `/dev/ttyACM0` at first flash; the number moves, so check `ls /dev/ttyACM*` |
| Wi-Fi MAC | `44:BD:8D:7A:51:60`, on the AP accept list |
| BLE MAC | `44:BD:8D:7A:51:62` |
| Nodes | `node-mb0200`, static `10.87.0.20` (2026-10-28 image name `relay-mb0200`); `toby-cam-1`, static `10.87.0.21`, unreachable on 2026-10-31 |
| ESPHome | 2026.9.1 (`archive/tools/esphome.sh`; archived) |

On 2026-10-28 both nodes passed `config` and `compile`. RAM and flash use: relay 41.3% / 14.3%, cam 44.9% / 15.4% (the cam figure predates the power-save fix below). On 2026-10-31 `toby-cam-1.yaml`, without esphome-mesh, compiled at RAM 45.0%, flash 15.4%. It is not flashed.

## Rust image

`firmware/cam` is crate `tallow-cam` for this board. Target `xtensa-esp32s3-espidf`, ESP-IDF v5.5.5, image `espressif/idf-rust:esp32s3_1.95.0.0`. Nothing is installed on the host.

```bash
tools/mesh.sh cam-build    # release image
tools/mesh.sh cam-size     # size against the OTA slot
tools/mesh.sh cam-image    # write the app image to firmware/cam/target/ota.bin
```

`tools/mesh.sh fmt` formats this crate. `tools/mesh.sh check` runs `cargo fmt --check` on it and, without `--fast`, clippy (`cargo clippy --release -- -D warnings`) and then the build, after the node firmware, so a break here shows there. `check --fast` skips that clippy and that build. The same command covers the mesh, the host daemon, the panel's tests, the cam stream test is archived (`archive/tools/tests/test_cam_stream.py`), and a host test of the push signature ([docs/01](../01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh)). After the cam firmware build, the full check runs `tools/cam_build_scan.py` on the image and the cam's stack budget (`tools/tests/stack_budget.py`, with the chains in `tools/tests/stack_chains_cam.json`). A single build is `cam-build`. The cam commands do not read `firmware/mesh/mesh.key`. `fmt`, `cam-build`, `cam-size`, and the cam half of `check` stop if `firmware/cam/secrets.yaml` or `firmware/mesh/ota.pub` is missing. `target/`, `.embuild/`, and `sdkconfig` under `firmware/cam/` are gitignored.

Octal PSRAM at 80 MHz, 16 MB DIO flash at 80 MHz, Bluetooth off. The partition table is the ESPHome OTA layout on this board: `app0` and `app1` are each `0x7C0000` bytes. The CPU is 240 MHz. Rollback is enabled in the bootloader, so `cam-flash` passes the bootloader this build produces and erases otadata, so the board boots `app0`, which it just wrote. The image is not flashed. `toby-cam-1` was not touched.

`boot` prints free internal heap and free PSRAM on the USB Serial/JTAG port (`boot heap … psram …`). It then starts the camera, which is esp32-camera 2.1.7 through the `camera.rs` driver. The first frame prints `frame WxH jpeg N bytes` once. If the sensor does not start, it prints `camera init failed` and the listeners still come up.

The station joins `tallow-iot` with WPA2, static `10.87.0.23/24`, gateway and DNS `10.87.0.1`. There is no DHCP client and no fallback access point. Modem sleep is `WIFI_PS_MIN_MODEM`. The SSID and password are the `wifi_ssid` and `wifi_password` keys in gitignored `firmware/cam/secrets.yaml` (moved there from `firmware/esphome/secrets.yaml`; the ESPHome file is archived). The build mounts that one file read-only and fails if it is missing or a value is empty. It does not print them. `firmware/cam/target` is mode `0700` after the build, because the password is in the image.

Port `8080` serves one MJPEG client. A connection is served only when its request line starts with `GET ` and its headers end within 8 KB and 3 s; any other request is dropped with no stream. The response is `multipart/x-mixed-replace` with a `Content-Length` on each part and no chunked transfer, which is what the panel's `dial` and `frames` already parse. Port `6053` accepts a TCP connection and closes it. That is the panel's status check. It does not speak the ESPHome API.

`tools/tests/cam_stream_check.py --address 10.87.0.23` counts frames for 10 s through the panel's `dial` and `frames`. It refuses `10.87.0.21` before any socket. It has not been run. `tools/mesh.sh cam-frame` prints one multipart sample built on the host from `firmware/cam/src/stream.rs`. It opens no socket.

Port `3232` takes one signed image. The header is 105 bytes: a big-endian size, a product byte (`2`, the cam), a little-endian version, the 32-byte SHA-256, and a 64-byte Ed25519 signature over the product, version, and digest. Each refusal happens before `esp_ota_begin` and before any image byte is written: a size of zero or past the `0x7C0000` slot, another product, a version below the floor (the highest version this board has confirmed; an equal version is a retry), and a bad signature. The body is hashed while it is written. The next boot partition changes only when that hash is the signed digest. The cam answers with one result byte, `0` for ok, then reboots into the stored image. The public key is `firmware/mesh/ota.pub`, baked at build time. The seed is not in the image, and the cam build does not mount `firmware/mesh/` or `mesh.key`.

The running image marks itself valid once, after 5 minutes of continuous Wi-Fi and a frame captured in the last 5 s, and only if the running slot is pending verify. Wi-Fi down clears that timer. The gap is a wrapping subtraction. A pending image still unmarked after 15 minutes rolls back, which also covers a hang.

```bash
tools/mesh.sh ota-sign <image> cam0 <version>
tools/mesh.sh cam-push 10.87.0.23 <image>
```

`cam-push` allows only `10.87.0.23`. It reads the sibling `.sig` from `ota-sign`, which must be 69 bytes, and does not read `ota.key`. `tools/cam_push_send.py` repeats the checks before it opens a socket: the address, the signed product, and an image that is neither empty nor past the `0x7C0000` slot. It waits for the cam's result byte and exits non-zero on anything but ok. It has not been run.

```bash
tools/mesh.sh cam-flash /dev/ttyACM0    # hold BOOT, tap RST first; only toby-cam-0
tools/mesh.sh cam-backup /dev/ttyACM0   # same guard, no address
```

`cam-backup` writes `data/firmware-backups/` at mode `0600` in a `0700` directory and refuses to overwrite, the same writer as `backup` ([board note](esp32-devkit.md#back-up)). The cam image holds the Wi-Fi password, so the file stays private. `cam-flash` and `cam-backup` have not been run.

On 2026-10-31 `tools/mesh.sh cam-size` printed `App/part. size: 464,400/8,126,464 bytes, 5.71%`, with the camera driver in and before this network. Before the driver the image was 367,392 bytes (4.52%). On 2026-11-01 `tools/mesh.sh cam-size` printed `App/part. size: 1,071,264/8,126,464 bytes, 13.18%`, with Wi-Fi, the stream, and signed OTA in. `strings` on that image printed `no default credential`. The image is not flashed. `toby-cam-1` was not touched.

## Pins

Source: the vendor page in `references/vendor/hardware/keyestudio-MB0200-S3-CAM.md`. It matches esp32-camera's `BOARD_ESP32S3_GOOUUU`. `BOARD_ESP32S3_WROOM` is the wrong pick: its PWDN on IO38 is the SD card's CMD on this board.

| Camera | GPIO |
| --- | --- |
| SIOD (SDA) | 4 |
| SIOC (SCL) | 5 |
| XCLK | 15, 20 MHz |
| Y2..Y9 | 11, 9, 8, 10, 12, 18, 17, 16 |
| VSYNC | 6 |
| HREF | 7 |
| PCLK | 13 |
| PWDN, RESET | none |

The SD card uses IO38 (CMD), IO39 (CLK), and IO40 (D0). The configs do not use it.

`packages/dogcam.yaml` serves 800x600 MJPEG, JPEG quality 12, at most 10 fps and 0.1 fps idle, on port `8080`. Wi-Fi power save stays at ESPHome's default, which compiled to `light` on 2026-10-31 ([docs/01](../01-bluetooth-server-hardware-and-proxies.md#tallow-nodes)).

## First flash

Bring up `tallow-iot` first ([docs/04](../04-firmware-liberation-and-network-isolation.md#host-access-point)). This is the ESPHome first flash of 2026-10-28, kept as the record. The tooling is archived, so run the commands from `archive/tools/` (`archive/tools/esphome.sh`, `archive/tools/node_check.py`); nothing wires them into the tree. Then:

```bash
archive/tools/esphome.sh secrets
ESPHOME_DEVICE=/dev/ttyACM0 archive/tools/esphome.sh mac
sudo ./tools/setup_iot_ap.sh allow <mac>
ESPHOME_DEVICE=/dev/ttyACM0 archive/tools/esphome.sh run node-mb0200.yaml --device /dev/ttyACM0
.venv/bin/python archive/tools/node_check.py --address 10.87.0.20
archive/tools/esphome.sh run toby-cam-1.yaml --device 10.87.0.20
curl -s -o /dev/null -m 5 -w '%{content_type}\n' http://10.87.0.21:8080/
```

`secrets` writes `archive/firmware/esphome/secrets.yaml` (mode `0600`) from `secrets/iot-ap/` and a fresh API key. If the file exists, the command stops. The API key is the OTA key for every node. `node_check.py` needs `aioesphomeapi`, which is no longer in `tools/requirements.txt`.

The bare node goes first, over USB, and proves the base. The same board then becomes `toby-cam-1` over the air. That upload has to name the bare node's address, because `toby-cam-1.yaml` points at `10.87.0.21` and the board is still at `10.87.0.20`. Later cam updates were `archive/tools/esphome.sh run toby-cam-1.yaml`, which dials `10.87.0.21`. On 2026-10-30 that dial timed out ([Where it is](#where-it-is-2026-10-30)). The curl line prints `multipart/x-mixed-replace;boundary=…`.

The 2026-10-28 serial flash was `relay-mb0200.yaml`, the earlier name of `node-mb0200.yaml`. It came up at `10.87.0.20`, and the cam OTA then came up at `10.87.0.21`.

The board may need download mode for the first flash: hold BOOT, tap RST. `ESPHOME_DEVICE` passes the port into the container. `/dev/ttyACM0` is `root:dialout 0660` and the user is not in `dialout`; the wrapper adds the port's group to the container, so it opens read-write there. The serial flash of `relay-mb0200.yaml` through the wrapper worked on 2026-10-28.

Rollback covers only a build that fails before the 60 s safe-mode mark. ESPHome 2026.9.1 has `enable_ota_rollback` on by default, so the bootloader then boots the previous image, at the previous image's address. The first cam build (Wi-Fi power save off) never joined Wi-Fi and came back as `relay-mb0200` at `10.87.0.20`. If a new build is not at its address, try the old one. A build that fails after the mark stays on the board. An esphome-mesh build did that ([Where it is](#where-it-is-2026-10-31)).

### Native USB resets into download mode

Under ESPHome the OTG port is USB-Serial/JTAG (`303a:1001`). An RTS hard reset leaves the S3 in ROM download mode. Opening the port from the host does the same: `esphome logs --device /dev/ttyACM0` and plain serial readers both do it. The boot log shows `rst:0x15 (USB_UART_CHIP_RESET),boot:0x2 (DOWNLOAD(USB/UART0))` and `waiting for download`, and the node drops off `tallow-iot`.

`archive/tools/esphome.sh mac` passes esptool `--after watchdog-reset` for this reason. Recovery that worked on 2026-10-28: esptool `--after watchdog-reset chip-id` from the wrapper's image. `ESPHOME_DEVICE=/dev/ttyACM0 archive/tools/esphome.sh mac` runs the same reset; it now runs `flash-id`, which also prints chip and flash size. A physical RST also works, with nothing holding the port open.

Read logs over the network, not this port:

```bash
archive/tools/esphome.sh logs toby-cam-1.yaml
```

## On the board (2026-10-28)

| Step | Result |
| --- | --- |
| Serial `run relay-mb0200.yaml` through the wrapper | Flashed. Joined at `10.87.0.20` |
| `relay_check.py --address 10.87.0.20` | `relay-mb0200`, ESPHome 2026.9.1. Proxy flags PASSIVE_SCAN, ACTIVE_CONNECTIONS, REMOTE_CACHING, PAIRING, CACHE_CLEARING, RAW_ADVERTISEMENTS, FEATURE_STATE_AND_MODE, CONNECTION_PARAMS_SETTING. 17 BLE addresses in 15 s. H7020 `C5:38:30:33:3E:4A` not heard from where the board sat |
| `run toby-cam-1.yaml --device 10.87.0.20` | Encrypted OTA, about 6–8 s. Up at `10.87.0.21` about 10 s later; stayed up |
| `relay_check.py --address 10.87.0.21` | `toby-cam-1`. Proxy flags PASSIVE_SCAN, RAW_ADVERTISEMENTS, FEATURE_STATE_AND_MODE: scan-only, as designed; GATT belongs to the dedicated nodes ([esp32-devkit.md](esp32-devkit.md)). 9 BLE addresses in 10 s. On 2026-10-30: 13 in 10 s, H7020 not heard, Wi-Fi signal not reported (this build predates the node package's `wifi_signal` sensor). The later OTA of that package timed out ([Where it is](#where-it-is-2026-10-30)) |
| `curl http://10.87.0.21:8080/` from the host | 200, `multipart/x-mixed-replace;boundary=123456789000000000000987654321`. About 7 frames/s of 800x600 JPEG, about 10.6 KB each (around 23:00; about 2.5 to 3 fps by 23:30, see [below](#checked-on-the-real-cam-2026-10-28-before-2333-mdt)) |

Proven: the standalone relay, encrypted OTA, and the cam's sensor init and pins (well-formed 800x600 frames).

Not proven:

- Picture content. Frames were black at about 23:10 local, still black 8 s into a stream. By 23:30 they were very dark with faint shapes, so the lens is not covered; the room is dark. Not tuned ([checked on the real cam](#checked-on-the-real-cam-2026-10-28-before-2333-mdt)).
- The `/cams` page in a real browser. The proxy and routes were checked against this cam; the page was not.
- The H7020 heard through any node.

## Cams page

The panel at `/` is a menu (`app/menu_page.html`): Lights at `/lights`, the cameras app at `/cams`, and Settings at `/settings`. The cameras tile, the `/cams` heading, and that page's title are the inventory `title`. The inventory is `data/panel/cams.json` (`data/panel-preview/cams.json` under `--preview`, or `cams.json` in the `--data-dir` of a preview). `data/` is gitignored local state, so the file is not part of the repo. On 2026-10-28 it held one cam and no title. The running inventory now has a title, and the preview inventory does too. A document looks like:

```json
{"title": "Toby", "cams": [{"id": "toby-cam-1", "name": "Cam 1", "address": "10.87.0.21"}]}
```

That address is the static one in `toby-cam-1.yaml`. The board is not answering there on 2026-10-31 ([Where it is](#where-it-is-2026-10-31)). `toby-cam-0` is not yet in `data/panel/cams.json`.

`title` is optional, 1 to 40 characters. Omit it, or leave the file missing, and the name is `Cameras` (`DEFAULT_TITLE` in the original `archive/app/cams.py`; the Rust panel is `host/tallowd/src/panel/`). `id` is a lowercase slug and must be unique. `name` is 1 to 40 characters. Unknown fields are refused. `address` must be a node in `10.87.0.0/24`, not `.0`, `.1`, or `.255`, so the panel cannot be pointed anywhere else. Edit the list on the settings page. A direct edit of the file is still picked up when the file changes.

### Inventory reload

The panel re-reads `cams.json` whenever its mtime changes. Adding, renaming, or removing a cam needs no restart. A bad edit (not JSON, bad id, bad name, address outside the net, unknown field, duplicate id, bad title) keeps the last good list and title, and the page shows `cams.json is not read: <why>`. At startup an invalid file still stops the panel with `Cams unreadable`; a missing file means no cams, named Cameras. Startup prints `Cams N from <path>, re-read when it changes`.

### Settings

`/settings` is `app/settings_page.html`, styled with `app/panel.css`. The cameras form sets the app name and one row per camera (id, name, address). Up and Down set the order on the cams page. Add a camera appends a row. Save sends `PUT /api/settings/cameras` with `{"title", "cams"}`.

`GET /api/settings/cameras` returns `{"title", "cams": [{id, name, address}], "error"}`. That is the browser call that includes addresses. `GET /api/cams` still returns id and name only.

PUT uses the same checks as load, then replaces the file in `save_inventory`: a temporary file next to `cams.json`, then a rename. A rejected document is 400 and the file stays as it was. The message starts with `cameras:` and names no path on the server, because it answers a browser. A missing title is stored as `Cameras`.

The same page has Log out. `POST /logout` clears the `tallow_panel` cookie and redirects to `/login`. It does not change the password. The cookie header, and that logout runs without a cookie, are in [govee-h7020.md](govee-h7020.md#lan-page). The password stays one line in `secrets/panel/password`.

### One connection per cam

ESPHome's `esp32_camera_web_server` is a single-slot server: `max_open_sockets = 1`, `backlog_conn = 2`, LRU purge on, and its stream handler blocks the one httpd task until a send fails. The cam serves one connection at a time. A second client waits in the TCP backlog until the first leaves. A panel where each screen dialed the cam, "two viewers per cam", was wrong for this hardware. A burst of connects and drops can also wedge it for minutes ([The one-slot jam](#the-one-slot-jam)).

So the panel dials once per cam and shares it, and is gentle about it (`Hub` and `Hubs` in the original `archive/app/cams.py`):

- The first viewer dials `http://<cam>:8080/`. The panel parses the cam's multipart stream by `Content-Length` and hands each frame to every viewer's own queue. A slow viewer skips frames; it does not hold the others back. There is no chunked-transfer support; the cam does not chunk.
- Linger: after the last viewer leaves, the panel keeps the cam connection `LINGER_S = 10` s, then closes it and the cam stops sending. A tab switch, reload, or Stop then Watch inside that time reuses the connection. Frames are discarded meanwhile; nothing is stored. A viewer who joins while the socket is closing waits for it to close, then dials again. A hub with a live or lingering connection is never pruned from the list.
- Cooling-off: after a failed dial the panel does not dial that cam again for 3, 6, 12, 24, then 30 s (`COOLDOWN_S`); a successful dial resets it. During it a viewer gets 502 at once (`cooling off for N s` in the panel log) and the cam is not touched.
- Dial patience: connect `CONNECT_TIMEOUT = 5` s, response headers `HEADER_TIMEOUT = 10` s. A dial cancelled mid-way closes its socket, so it leaves no request in the cam's queue.
- The panel re-emits its own stream: `multipart/x-mixed-replace; boundary=tallow`, each part `image/jpeg` with its `Content-Length`, `cache-control: no-store`.
- At most `VIEWERS_MAX = 8` screens per cam; the ninth gets 429 `cam busy`. A cam that fails to answer, or answers without MJPEG, is 502 `The cam is not answering.` An unknown id is 404.
- Nothing is recorded or written to disk.

### Routes

| Route | Returns |
| --- | --- |
| `GET /api/cams` | `{"cams": [{id, name}], "error": null or string}`. Addresses are omitted |
| `GET /api/cams/status` | `{id: {"online": bool, "viewers": n}}`. `online` is a TCP connect (1.5 s) to the node's API port `6053` (`NODE_PORT`), never the camera port `8080`; a cam with viewers is reported online without being asked. So `online` can be true while the stream is wedged |
| `GET /cams/{id}/stream` | The shared multipart stream |
| `GET /api/settings/cameras` | `{"title", "cams": [{id, name, address}], "error"}` |
| `PUT /api/settings/cameras` | The same document after the file is replaced. 400 leaves the file unchanged |
| `POST /logout` | Clears `tallow_panel` and redirects to `/login` |

`GET /api/cams`, `GET /api/cams/status`, `GET /cams/{id}/stream`, and `GET` and `PUT /api/settings/cameras` answer 401 `{"detail":"login"}` without the cookie. Page redirects, `GET /panel.css`, `GET /`, and the cookie header are in [govee-h7020.md](govee-h7020.md#lan-page). The agent socket answers 404 for `GET /lights`, `GET /cams`, `GET /settings`, `GET /panel.css`, the cam routes, `GET` and `PUT /api/settings/cameras`, and `POST /logout`. The 404 body, `POST /login`, socket `GET /`, and `GET /api/manifest` are in [govee-h7020.md](govee-h7020.md#agent-socket). None of those page and cam routes are on the H7020 manifest.

### The page

`app/cams_page.html` with `app/panel.css`: a grid, one column on a phone and up to 72rem wide, built for any number of cams. Each tile has the name, a status dot and text (Checking, Ready, Offline, Connecting, Live mm:ss), a Watch or Stop button (disabled while the cam is offline), and a 4:3 screen.

- Tap a live picture to fill the screen. Esc or another tap returns; Enter and Space work on the focused screen.
- The page does not give up. While a tile is watching, a dropped or refused stream is retried after 2, 4, 8, 15, then 30 s, repeating at 30 s. The tile says "The cam is not answering. Trying again in N s." The first frame clears that and resets the pauses. Stop ends it.
- A finished multipart response gives the browser no error event. So a tile that is live while the panel reports 0 viewers is treated as a stream that silently ended, and starts the same retry cycle.
- Watch all and Stop all appear with 2 or more cams. Watch all skips offline cams.
- While the tab is visible, the page re-reads the list and status every 5 s if any tile is watching, every 20 s otherwise, so file edits appear without a reload.
- Switching away from the tab stops every stream. Coming back resumes the ones that were on.
- With no cams it says to add one in Settings. The page says nothing is saved.

### Tests

`archive/tools/tests/test_cams.py` (archived with the Python panel; no longer run) covered the shared feed (two screens on one cam connection under both ASGI disconnect paths, leave and rejoin, the cap, the 502s, `Book` reload and bad edits, the drop-oldest queue), status asking the node port and never the camera port, recovery when a cam hangs up mid-stream or goes quiet, and gentleness: linger reuse and hang-up, a lingering hub not pruned, the cooling-off schedule with no dial during it, and a cancelled dial closing its socket. It also covers the optional title, `save_inventory`, settings auth, addresses hidden on `GET /api/cams` and returned by the settings API, and logout clearing the cookie. `tools/tests/test_cams_page.py` runs `tools/tests/cams_page_harness.js` in node against a fake DOM and is skipped when node is absent. Its 17 checks walk watch, live, enlarge, drop and endless retry with pauses capped at 30 s, a silently ended stream, the 5 s status poll, Stop during retry, a second cam, watch all, tab away and back, a removed cam and a bad edit, and the empty state. The harness failed on deliberately broken copies of the page.

### Checked on the real cam (2026-10-28, before 23:33 MDT)

A throwaway panel on `127.0.0.1` with its own test password and the real `cams.json`: `/api/cams` and `/api/cams/status` were correct. Two simultaneous screens both got 200, `multipart/x-mixed-replace; boundary=tallow`, and frames. Status showed `viewers: 2` while both watched and 0 after both left, so the cam was released.

Frame rate that hour was about 2.5 to 3 fps, through the panel and with raw curl straight to the cam. Earlier that night it was 7 to 9 fps. The Wi-Fi link is fine (-34 dBm, 72 Mbit/s). The latest frame was no longer pure black but very dark, with faint shapes. The likely cause is a dark room: auto-exposure lengthens exposure and the frame rate drops. Exposure and gain are not tuned (ESPHome's default `agc_gain_ceiling` is `2X`).

## Power

At the first move, about 23:33 on 2026-10-28, the cam joined Wi-Fi and was unreachable. The cause was a weak USB supply and cable. The AP saw the board's frames at -46 to -57 dBm with zero AP retries. The board sent its own gratuitous ARP but answered none of the host's broadcast ARP requests and only about 2 of 5 directed ones. Ping was 100% loss, "Host Unreachable". A better power brick and cable fixed it; the user reports it works.

Use a proper brick and cable. If the symptom comes back, check power before firmware. The same silence, IP from the reserved address and no ARP answer, is what `sudo ./tools/setup_iot_ap.sh` pins for a station that is associated ([docs/04](../04-firmware-liberation-and-network-isolation.md#host-access-point)). On 2026-10-30 the cam did that at `10.87.0.21` while associated at about -38 dBm ([Where it is](#where-it-is-2026-10-30)).

## The one-slot jam

Seen on the real cam at about 00:00 on 2026-10-29. After a burst of connects and drops, the cam's web server wedged. A test of 25 viewers dropping at random moments (50 ms to 2 s) left the cam answering ping and the API on `6053` but not `8080`: SYN unanswered, listen queue full. It recovered on its own after about 3 minutes; the first byte then took 3.3 s. Treat 3 minutes as one observation, not a figure.

What it looks like: ping fine, `6053` fine, `8080` silent. The page shows "Reconnecting" because `online` stays true. Do not hammer it. Every new connect or dropped request queues behind the one slot and makes the wait longer. Wait, and it drains.

Before this was fixed the panel fed the jam. Each open cams page connected to `8080` every 20 s for status. Every dial that timed out (5 s) left a request in the cam's queue that it later spent its one slot on. Every tab switch or Stop then Watch made a fresh connect and drop. The page gave up after 3 retries, so retries came in bursts. Restarting the panel appeared to fix the cam only because it stopped that traffic and let the cam drain. The panel process itself was not stuck: unit tests show it recovers when a cam hangs up mid-stream, goes quiet, or returns. The gentleness described under [One connection per cam](#one-connection-per-cam) and [The page](#the-page) is the fix.

Checked on the real cam through a throwaway panel on `127.0.0.1`, one viewer: the status call opened no connection to `8080`; the panel opened one connection when the viewer arrived, held it through the 10 s linger after the viewer left, then hung up (about 13 s after start for a 3 s viewer); the cam served a fresh viewer afterwards (first byte 0.05 s).

Not verified:

- The page in a real browser (only the node harness has run it).
- Recovery of the real cam across a real power loss with the new code.
- That the recovery time is always about 3 minutes.
- The user's own panel. It predates this code and needs one restart to get the shared proxy, the gentle status route, the linger, the cooling-off, the endless retry, and the reload.

## toby-cam-0

The second MB0200, same board as `toby-cam-1`. Counting starts at 0 from here on. As of 2026-10-31 it is not flashed.

| Field | Value |
| --- | --- |
| Config | `archive/firmware/esphome/toby-cam-0.yaml`, static `10.87.0.23` |
| Build | Compiled at RAM 45.0%, flash 15.4% |
| Attached | `/dev/ttyACM0`, USB `303a:4001`, serial `123456` |
| Running | Earlier TinyUSB firmware |
| Wi-Fi MAC | Unknown |

esptool could not connect, and a 1200-baud touch did not help. It needs BOOT+RST for the first read. The MAC must go on the AP accept list before the board can join. The board is not yet in `data/panel/cams.json`.

```bash
ESPHOME_DEVICE=/dev/ttyACM0 archive/tools/esphome.sh mac            # hold BOOT, tap RST first
sudo ./tools/setup_iot_ap.sh allow <mac>
ESPHOME_DEVICE=/dev/ttyACM0 archive/tools/esphome.sh run toby-cam-0.yaml --device /dev/ttyACM0
```

## Wi-Fi

`tallow-iot` only. The address is static in the YAML (`manual_ip`, [docs/04](../04-firmware-liberation-and-network-isolation.md#esp32-nodes)). A reservation is optional. There is no fallback AP, no captive portal, and no web server other than the cam stream. The node MAC goes on the accept list before it can associate.

## Where it is (2026-10-31)

`toby-cam-1` is taped to the wall, still unreachable at `10.87.0.21` on 2026-10-31 (ping fails, `node_check.py` gets no route to host), and not recovered.

As of 09:58 MDT that day it was running the build from the [2026-10-30 esphome-mesh trial](../01-bluetooth-server-hardware-and-proxies.md#esphome-mesh-removed). esphome-mesh is out of the repo, and the build is presumably still on this board. It has nothing to do with the [Rust mesh](../01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh), which runs on classic ESP32 boards only.

That build got past the 60 s safe-mode mark, so rollback did not apply. Then it hit a heap assert in `free` during Noise handshake teardown, `internal_min=444` bytes, and the cam stopped answering ARP.

`toby-cam-1.yaml` without esphome-mesh compiled (RAM 45.0%, flash 15.4%). It is not flashed.

Recovery, not yet done, is one of two paths:

- OTA with the neighbor pinned. `sudo ./tools/setup_iot_ap.sh` pins `10.87.0.21` only while this MAC is associated, and it needs the reservation line, which is still there. At 10:05 MDT on 2026-10-31 the host's entry was `INCOMPLETE`, so no pin was in place. Then `archive/tools/esphome.sh run toby-cam-1.yaml`.
- USB on the OTG port: `ESPHOME_DEVICE=/dev/ttyACM0 archive/tools/esphome.sh run toby-cam-1.yaml --device /dev/ttyACM0`. Download mode may be needed: hold BOOT, tap RST.

Not proven:

- Either recovery path on this board.
- `toby-cam-1` without esphome-mesh running on any board.

## Where it is (2026-10-30)

Superseded by [Where it is (2026-10-31)](#where-it-is-2026-10-31). Kept for the lease and ARP evidence.

`secrets/iot-ap/reservations` and `/etc/tallow/dnsmasq.hosts` reserve this board (`44:bd:8d:7a:51:60`) as `10.87.0.21`. At 2026-10-30 19:58:39 dnsmasq logged `cannot read /etc/tallow/dnsmasq.hosts: Permission denied` and offered `10.87.0.119`. `sudo ./tools/setup_iot_ap.sh` at 20:34 set `/etc/tallow` to mode `0711`. dnsmasq could read the hosts file and DHCPACK'd that MAC as `10.87.0.21`, and again at 21:48 after a group-key deauth. The station is associated at about -38 dBm. The board was not re-flashed. The 2026-10-29 image is still what is running.

The cam does not answer ARP for `10.87.0.21`. The AP's who-has goes unanswered. The cam sends IPv4 from `10.87.0.21` port `6053` to `10.87.0.1`. The host neighbor goes INCOMPLETE. Connects fail with "No route to host"; the panel logs `TimeoutError`, then `OSError`. The cams page returns 502. OTA to `10.87.0.21:3232` still failed.

`up` in the current `tools/setup_iot_ap.sh` pins a permanent neighbor for an associated reservation ([docs/04](../04-firmware-liberation-and-network-isolation.md#host-access-point)). That script has not been run. `10.87.0.21` is unpinned, and the panel still cannot open the stream.
