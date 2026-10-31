# Phase 4: cam firmware

Replace ESPHome on the S3 cams with Rust. This is the riskiest phase for the least new function, so it runs last.

**First:** `toby-cam-0` must run the ESPHome build (`toby-cam-0.yaml`) and answer, so there is a known good to compare and return to. `toby-cam-1` stays out of this phase until T4.5.

**Lessons, all from this board:**

- Internal RAM ran out at 444 bytes while 8.28 MB of PSRAM sat unused. Set `CONFIG_SPIRAM_TRY_ALLOCATE_WIFI_LWIP=y` and keep frame buffers in PSRAM.
- Wi-Fi power save `none` stops the board joining while Bluetooth is on. A cam does not need Bluetooth at all in this firmware, so leave BT out.
- Rollback at 60 s missed a build that crashed later. Mark an image valid only after 5 minutes of health.
- The cam's web server serves one connection and wedges under bursts. Do not load-test it.
- A weak USB supply looks like a firmware fault. Check power first.

## T4.1 S3 build

- **Tier:** Sonnet. **Needs:** nothing.
- **Files:** new `firmware/cam/` (crate `tallow-cam`), `tools/mesh.sh`.
- **Do:** A separate crate with target `xtensa-esp32s3-espidf`, ESP-IDF v5.5.5, image `espressif/idf-rust:esp32s3_1.95.0.0`. Add `tools/mesh.sh cam-build` and `cam-size`. Octal PSRAM at 80 MHz, 16 MB flash, and an OTA layout. No BT. `boot` prints heap and PSRAM free.
- **Done when:** it builds and `cam-size` prints a size under the OTA slot.
- **Stop if:** the image needs a change to the mesh crates.

## T4.2 Camera driver

- **Tier:** Sonnet. **Needs:** T4.1.
- **Files:** `firmware/cam/Cargo.toml`, `src/camera.rs`.
- **Do:** Add `espressif/esp32-camera` as a remote component through `esp-idf-sys`'s `extra_components`. See `references/rust/esp-rs-esp-idf`, and `references/vendor/esp32-camera` for v2.1.8. Pins, XCLK 20 MHz, 800x600, and JPEG quality 12 come from [the device note](../../devices/keyestudio-mb0200-s3-cam.md#pins). Do not use `BOARD_ESP32S3_WROOM`: its PWDN is the SD card's CMD here. Wrap capture so each frame buffer is returned exactly once, including on error.
- **Done when:** it builds. **You:** flash `toby-cam-0` and see the frame size logged.
- **Stop if:** a frame buffer needs copying into internal RAM.

## T4.3 Network and stream

- **Tier:** Sonnet. **Needs:** T4.2.
- **Files:** `firmware/cam/src/net.rs`, `src/stream.rs`.
- **Do:** Join `tallow-iot` with a static address, no fallback AP, and modem sleep on. Serve MJPEG on `8080` with the same multipart output the panel parses: `Content-Length` per part and no chunking. Keep a plain TCP listener on `6053`, because the panel's status check uses it. Credentials come from gitignored `firmware/esphome/secrets.yaml` at build time, and are never printed.
- **Done when:** `tools/tests/test_cams.py`'s fake-cam parsing accepts a capture of this stream. **You:** the panel shows the picture.
- **Stop if:** the panel needs a code change to read the stream.

## T4.4 OTA with signing

- **Tier:** Sonnet. **Needs:** T4.3, and T1.9's signing.
- **Files:** `firmware/cam/src/ota.rs`, `tools/mesh.sh`.
- **Do:** Reuse T1.9's signature check. The host pushes an image over TCP to a listener on `3232`. Rollback is on, and the image marks itself valid after 5 minutes with Wi-Fi up and a frame captured.
- **Done when:** **You:** on `toby-cam-0`, update it, then push a build that crashes after 90 s and confirm it rolls back. The 90 s crash is the case that killed `toby-cam-1`.
- **Stop if:** it does not roll back. Nothing moves to `toby-cam-1` until it does.

## T4.5 Soak, then the wall cam

- **Tier:** You. **Needs:** T4.4.
- **Do:** Run `toby-cam-0` on Rust for a week. Then, with USB access to `toby-cam-1` ready, move it over.
- **Done when:** both cams run a week with no wedge the ESPHome build did not also have.
