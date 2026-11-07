# Phase 4: cam firmware

Replace ESPHome on the S3 cams with Rust. This is the riskiest phase for the least new function, so it runs last.

**First:** T4.0. `toby-cam-1` stays out of this phase until T4.5. `cam-flash` takes only the port `boards.tsv` names `toby-cam-0`. `cam-push` allows only `10.87.0.21` ([device note](../../devices/keyestudio-mb0200-s3-cam.md#rust-image)).

**Superseded on 2026-11-02:** the one-cam limits in this phase are gone. `cam-flash` takes any ESP32-S3 and names it by MAC (`toby-cam-N`), the image takes its address by DHCP from a per-MAC reservation that `cam-flash` writes, and `cam-push` allows any address reserved for a catalog cam. The identity rules are in [docs/01](../../01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh). The T4.x text below is the history.

**Lessons, all from this board:**

- Internal RAM ran out at 444 bytes while 8.28 MB of PSRAM sat unused. Set `CONFIG_SPIRAM_TRY_ALLOCATE_WIFI_LWIP=y` and keep frame buffers in PSRAM.
- Wi-Fi power save `none` stops the board joining while Bluetooth is on. A cam does not need Bluetooth at all in this firmware, so leave BT out.
- Rollback at 60 s missed a build that crashed after that mark. Mark an image valid only after 5 minutes of health.
- The cam's web server serves one connection and wedges under bursts. Do not load-test it.
- A weak USB supply looks like a firmware fault. Check power first.

## T4.0 Known good on `toby-cam-0`

- **Tier:** You. **Needs:** nothing.
- **Why:** The board still runs earlier TinyUSB firmware, its MAC is unknown, and it is on no list. Without an ESPHome baseline there is nothing to compare or return to.
- **Do:** Hold BOOT, tap RST. Run `ESPHOME_DEVICE=/dev/ttyACM0 tools/esphome.sh mac`, then `sudo ./tools/setup_iot_ap.sh allow <mac>`, then `ESPHOME_DEVICE=/dev/ttyACM0 tools/esphome.sh run toby-cam-0.yaml --device /dev/ttyACM0`. Record the MAC in `boards.tsv`. The live page is the catalog sync in [the device note](../../devices/keyestudio-mb0200-s3-cam.md#catalog-sync), which on 2026-11-02 left the existing `10.87.0.21` row as `toby-cam-0`. Port 6053 did not accept, and no picture is showing.
- **Done when:** the panel shows its picture, and it has run a week. Note any wedge: T4.5 compares against this.

## T4.1 S3 build

**Done (2026-10-31).** It builds, and `cam-size` is under the OTA slot. The image is not flashed. Size and layout: [device note](../../devices/keyestudio-mb0200-s3-cam.md#rust-image).

- **Tier:** Sonnet. **Needs:** nothing.
- **Files:** new `firmware/cam/` (crate `tallow-cam`), `tools/mesh.sh`.
- **Do:** A separate crate with target `xtensa-esp32s3-espidf`, ESP-IDF v5.5.5, image `espressif/idf-rust:esp32s3_1.95.0.0`. Add `tools/mesh.sh cam-build` and `cam-size`. Octal PSRAM at 80 MHz, 16 MB flash, and an OTA layout. No BT. `boot` prints heap and PSRAM free.
- **Done when:** it builds and `cam-size` prints a size under the OTA slot.
- **Stop if:** the image needs a change to the mesh crates.

`tools/mesh.sh` points `IDF_PATH` at the mesh node's checkout, so the cam's IDF follows that directory, and `esp-idf-sys` ignores `ESP_IDF_VERSION`. Both are v5.5.5 today. If the mesh moves, the cam moves with it. `tools/mesh.sh check` builds the cam, so a break shows there.

## T4.2 Camera driver

**Done (2026-10-31).** `tools/mesh.sh cam-build` passes, and `cam-size` is 464,400 bytes (5.71% of the OTA slot). The image is not flashed.

- **Tier:** Sonnet. **Needs:** T4.1.
- **Files:** `firmware/cam/Cargo.toml`, new `src/camera.rs`, new `src/bindings.h`, `src/main.rs`, `.gitignore`.
- **Do:** Add `espressif/esp32-camera` version **2.1.7** as a remote component through `esp-idf-sys`'s `extra_components`: that is the version the known-good ESPHome build uses. `references/vendor/esp32-camera` holds v2.1.8 and is for reading only. See `references/rust/esp-rs-esp-idf/esp-idf-sys/BUILD-OPTIONS.md`. No Rust bindings appear without `bindings_header` pointing at a `.h` file, so add `src/bindings.h`. The component also writes `components_esp32s3.lock` into the crate: gitignore it. Pins, XCLK 20 MHz, 800x600, and JPEG quality 12 come from [the device note](../../devices/keyestudio-mb0200-s3-cam.md#pins). Do not use `BOARD_ESP32S3_WROOM`: its PWDN is the SD card's CMD here. Wrap capture so each frame buffer is returned exactly once, including on error. Call it from `main.rs`.
- **Done when:** `tools/mesh.sh cam-build` passes. The first flash is T4.3, because this board's log cannot be read over USB: opening that port drops it into download mode.
- **Stop if:** a frame buffer needs copying into internal RAM.

## T4.3 Network and stream

**Code done (2026-11-01), not run on hardware.** The image joins `tallow-iot` at `10.87.0.21` ([device note](../../devices/keyestudio-mb0200-s3-cam.md#rust-image)). The station and the listeners on `8080` and `6053` come up before the sensor starts. It serves the panel's multipart stream on `8080` and listens on `6053`. `cam-flash` and `cam-backup` refuse every port except `toby-cam-0` and pass this build's bootloader. `tools/mesh.sh check` passed. The source no longer uses modem sleep. On 2026-11-02 a modem-sleep image was flashed. The later leaf flash is in the [device note](../../devices/keyestudio-mb0200-s3-cam.md#esp-now-leaf). **You:** `cam-backup` has not been run. `archive/tools/tests/cam_stream_check.py` still allows only `10.87.0.23`.

- **Tier:** Sonnet. **Needs:** T4.0, T4.2.
- **Files:** new `firmware/cam/src/net.rs`, `src/stream.rs`, `src/main.rs`, `firmware/cam/build.rs`, `firmware/cam/sdkconfig.defaults`, `tools/mesh.sh`, new `tools/tests/cam_stream_check.py`.
- **Do:**
  - Join `tallow-iot` with WPA2 required, static `10.87.0.21`, mask `/24`, gateway and DNS `10.87.0.1`, no fallback AP, and modem sleep on. Only `toby-cam-0` is built in this phase.
  - Credentials are the `wifi_ssid` and `wifi_password` keys in gitignored `firmware/esphome/secrets.yaml`. `tools/mesh.sh` mounts that one file read-only into the cam build, and `build.rs` reads it. A missing file or an empty value fails the build, and is never printed. An open or default join is the same bug as an open mesh.
  - Serve MJPEG on `8080` with the same multipart output the panel parses: `Content-Length` per part and no chunking. Keep a plain TCP listener on `6053`, because the panel's status check uses it.
  - In `sdkconfig.defaults`, set `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y`, because the bootloader only changes over USB and this is the first flash. Set the CPU to 240 MHz, as the ESPHome build runs. T4.1 used 160.
  - Add `tools/mesh.sh cam-flash <port>` and `cam-backup <port>`. Each refuses any port `boards.tsv` does not name `toby-cam-0`, says to hold BOOT and tap RST first, and neither takes an address.
  - Add `tools/tests/cam_stream_check.py --address <ip>`: it uses `camlib.dial` and `frames()` from `app/cams.py`, counts frames for 10 s, and refuses `10.87.0.21`. Run it once: it takes the cam's one connection.
- **Done when:** `tools/mesh.sh cam-build` passes. **You:** run `cam-backup`, then `cam-flash`. The image is at `10.87.0.21`. `archive/tools/tests/cam_stream_check.py` still allows only `10.87.0.23`. The panel shows the picture.
- **Stop if:** the panel needs a code change to read the stream.

## T4.4 OTA with signing

**Code done (2026-11-01), not run on hardware.** Port `3232` takes one signed image and refuses it before any flash write when the signature is bad. `tools/mesh.sh check` passed. `tools/mesh.sh cam-size` printed `App/part. size: 1,071,264/8,126,464 bytes, 13.18%`. No push. **You:** on `toby-cam-0`, update it, then push a build that crashes after 90 s and confirm it rolls back.

- **Tier:** Sonnet. **Needs:** T4.3, T1.9a.
- **Files:** new `firmware/cam/src/ota.rs`, `src/main.rs`, `firmware/cam/Cargo.toml`, `firmware/cam/build.rs`, `tools/mesh.sh`.
- **Do:** Reuse T1.9a's signature check through a path dependency on `../mesh/core`. In `run_cam_image`, mount only `firmware/mesh/core` at `/mesh/core:ro` and `firmware/mesh/ota.pub` at `/run/ota.pub:ro`, never all of `firmware/mesh/`: `mesh.key` lives there, and the cam build must not see it. `build.rs` bakes in `/run/ota.pub` and fails without it. The host pushes an image over TCP to a listener on `3232`, through `tools/mesh.sh cam-push <address> <image>`, which requires an explicit address and allows only `10.87.0.21`. The image marks itself valid after 5 minutes with Wi-Fi up and a frame captured. Superseded for membership (2026-11-04): `run_cam_image` also mounts `firmware/mesh/mesh.key` read-only at `/run/mesh-key`, and `build.rs` bakes it. `fmt` does not. The rest of `firmware/mesh/` stays unmounted. Video stays on TCP ([docs/01](../../01-bluetooth-server-hardware-and-proxies.md#camera-leaves)). The address limit was already superseded: `cam-push` allows any address reserved for a catalog cam.
- **Done when:** **You:** on `toby-cam-0`, update it, then push a build that crashes after 90 s and confirm it rolls back. A crash after the 60 s mark is what took out `toby-cam-1`.
- **Stop if:** it does not roll back. Nothing moves to `toby-cam-1` until it does.

## T4.5 Soak, then the wall cam

- **Tier:** You. **Needs:** T4.4, and `toby-cam-1` recovered and running a known-good ESPHome image.
- **Do:** Run `toby-cam-0` on Rust for a week. Then, with USB access to `toby-cam-1` ready, back it up and move it over. Its row in `boards.tsv`, and the change letting `cam-flash` and `cam-push` take it, happen here, at your word and not before.
- **Done when:** both cams run a week with no wedge the ESPHome build in T4.0 did not also have.
