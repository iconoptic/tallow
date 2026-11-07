# Hardware checklist

The steps only you can do. Nothing here is for an agent, and each has its pass mark. Agents hand you commands, and you run them.

Boards on different frame versions refuse each other. The source is `0xAB` (G6 set `0xA7`, the OTA Begin change set `0xA8`, the camera `LEAF` bit set `0xA9`, kind 14 Identity set `0xAA`, and kind 15 Neighbours set `0xAB`). The boards still run `0xA1`. No board was flashed with `0xA8`, `0xA9`, `0xAA`, or `0xAB`. Flash every board in one pass, from one commit ([docs/01](../../01-bluetooth-server-hardware-and-proxies.md#source-ahead-of-the-boards-frame-version-0xa9)).

## Before the Grok run

1. **Commit the tree and mark the start.** The review diffs against this tag:

   ```bash
   git status --short
   git add -A
   git commit -m "Finish the Rust conversion code that needs no hardware."
   git tag grok-run-start
   ```

   Read the status list before `git add`. `.gitignore` covers the keys, backups, and build trees, and `git status` should show none of them.

2. **Signing key.** Already written, so leave `tools/mesh.sh ota-key` alone. It refuses to overwrite. `firmware/mesh/ota.key` is mode `0600` and gitignored. `firmware/mesh/ota.pub` is in the tree. Back up `firmware/mesh/ota.key` somewhere safe and offline. If it is lost, no board can be updated over the air. Details: [docs/04](../../04-firmware-liberation-and-network-isolation.md#mesh-nodes).

## Worth doing now

These check code the Grok run builds on. Doing them first gives the review real evidence. Use the `grok-run-start` commit.

3. **Flash the mesh (T1.4).** Run `tools/mesh.sh build`, then `tools/mesh.sh flash <port>` on each board. Send `D` to one board, and run `tools/mesh.sh daemon run <port> --seconds 120 --out data/mesh/map.json` on the other.
   **Pass:** the first board appears with a Classic RSSI between -100 and -20.

4. **Find a speaker by address (T1.5).** With a paired speaker idle, then playing, run `tools/mesh.sh daemon run <port> --seconds 120 --out data/mesh/map.json --watch <addr>`.
   **Pass:** its address appears in `map.json`. Record both results in `docs/01`. Stop if `--trial` delivery falls below 95% with paging on.

5. **Compare the trial (T2.7).** Run `tools/mesh.sh daemon run <port> --trial --out data/mesh/trial.json`, then `.venv/bin/python archive/tools/mesh_host.py --port <port> --trial --out data/mesh/trial-py.json`.
   **Pass:** rows agree within 3 points. The Python host is that archived script ([docs/01](../../01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh)).

6. **Try the Rust panel on the light (T3.7).** Stop the Python panel, and close Govee Home. The box takes one connection.

   ```bash
   tools/deploy_panel.sh
   ```

   **Pass:** power, brightness, and solid red work on `/lights`, and 30 s pass with no `BLE link down` line. The live process is the user unit ([govee](../../devices/govee-h7020.md#rust-panel-tallowd-panel)). The Python panel is archived. Scenes and paints have never been sent to this string, so leave them out.

7. **Check the panel's mesh host (T2.7).** Skip `tools/setup_mesh_host.sh` unless you want a host with no panel: the real panel now finds the root board and keeps `data/mesh/map.json` by itself. Needs access to the port first: `sudo setfacl -m u:$USER:rw /dev/ttyUSB0` for now, or `tools/setup_mesh_host.sh` for the udev rule. Restart with `tools/deploy_panel.sh` ([govee](../../devices/govee-h7020.md#rust-panel-tallowd-panel)) and open `/devices`.
   **Pass:** the page shows the map within a minute, and `GET /api/mesh/map` says `host.state` is `running`. Then run `tools/mesh.sh monitor <port> 5`: the panel lets go of the port and takes it back. With no board plugged in, `host.state` is `waiting` and the panel starts no child. Plug a listed classic ESP32 in and it becomes `running`. Do not also enable `tallowd.service`. States: [docs/01](../../01-bluetooth-server-hardware-and-proxies.md#not-built).

## Any time

8. **Put `toby-cam-0` on ESPHome (T4.0).** The steps are in [p4-cams.md](p4-cams.md). `/dev/ttyACM0` now reports as `USB_JTAG_serial_debug_unit`, not the earlier TinyUSB name, so check what it runs first. **Pass:** the panel shows its picture for a week.

9. **Attach a third classic board (T1.8).** **Pass:** at least 90% over two hops, end node at rank 2.

10. **Recover `toby-cam-1`.** T4.5 needs it on a known-good ESPHome image. It is out of every other step.

## After the Grok run and the review

11. **Drive the H7020 over the mesh (T1.11).** This needs G5, and a node that hears the light. **Pass:** connect under 10 s, one write under 500 ms after connect.

12. **OTA a leaf (T1.9).** This needs G7, G8, and a USB flash of every board, because the bootloader changes. Update a leaf one hop away, then push a build that crashes after 90 s.
    **Pass:** it updates, then rolls back. Do not roll out to any board that is not on a desk.

13. **Rust on `toby-cam-0` (T4.3, T4.4).** On 2026-11-02 `cam-flash` of `/dev/ttyACM0` wrote the modem-sleep image and exited success. The station associated, then hostapd dropped it for inactivity, and it has not associated again. The `WIFI_PS_NONE` source has not been flashed onto this board ([device note](../../devices/keyestudio-mb0200-s3-cam.md#rust-image)). The 2026-11-06 leaf flash was `toby-cam-2` ([device note](../../devices/keyestudio-mb0200-s3-cam.md#esp-now-leaf)). `cam-backup` has not been run. The image takes its address by DHCP; `toby-cam-0`'s reservation is `10.87.0.21`. `archive/tools/tests/cam_stream_check.py` still allows only `10.87.0.23` and refuses `10.87.0.21` before any socket, so it will not count frames from this image. Then push a crashing build. **Pass:** the panel shows the picture, and the crash rolls back. Nothing moves to `toby-cam-1` until it does. The evening `cam-push` did not reach this board ([device note](../../devices/keyestudio-mb0200-s3-cam.md#signed-push)).

14. **Fleet, cut-over, wall cam (T1.12, T3.10, T4.5).** In that order, each when the one before has held for its full soak.

15. **Retire the old stack (T5.1 to T5.3).** Only at your word.
