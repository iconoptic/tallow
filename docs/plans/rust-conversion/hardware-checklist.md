# Hardware checklist

The steps only you can do. Nothing here is for an agent, and each has its pass mark. Agents hand you commands, and you run them.

Boards on different frame versions refuse each other. G6 bumps the version again, so flash every board in one pass, from one commit.

## Before the Grok run

1. **Commit the tree and mark the start.** The review diffs against this tag:

   ```bash
   git status --short
   git add -A
   git commit -m "Finish the Rust conversion code that needs no hardware."
   git tag grok-run-start
   ```

   Read the status list before `git add`. `.gitignore` covers the keys, backups, and build trees, and `git status` should show none of them.

2. **Make the signing key.** G7 and G10 stop without it. It runs once and refuses to overwrite:

   ```bash
   tools/mesh.sh ota-key
   git add firmware/mesh/ota.pub
   ```

   Back up `firmware/mesh/ota.key` somewhere safe and offline. If it is lost, no board can be updated over the air.

## Worth doing now

These check code the Grok run builds on. Doing them first gives the review real evidence. Use the `grok-run-start` commit.

3. **Flash the mesh (T1.4).** Run `tools/mesh.sh build`, then `tools/mesh.sh flash <port>` on each board. Send `D` to one board, and run `tools/mesh.sh daemon run <port> --seconds 120 --out data/mesh/map.json` on the other.
   **Pass:** the first board appears with a Classic RSSI between -100 and -20.

4. **Find a speaker by address (T1.5).** With a paired speaker idle, then playing, run `tools/mesh.sh daemon run <port> --seconds 120 --out data/mesh/map.json --watch <addr>`.
   **Pass:** its address appears in `map.json`. Record both results in `docs/01`. Stop if `--trial` delivery falls below 95% with paging on.

5. **Compare the trial (T2.7).** Run `tools/mesh.sh daemon run <port> --trial --out data/mesh/trial.json`, then `tools/mesh.sh host <port> --trial`.
   **Pass:** rows agree within 3 points.

6. **Try the Rust panel on the light (T3.7).** Stop the Python panel, and close Govee Home. The box takes one connection.

   ```bash
   host/target/release/tallowd panel
   ```

   **Pass:** power, brightness, and solid red work on `/lights`, and 30 s pass with no `BLE link down` line. To roll back, press Ctrl-C and run `.venv/bin/python app/h7020_panel.py`. Scenes and paints have never been sent to this string, from either language, so leave them out.

7. **Set up the service (T2.7).** Run `tools/setup_mesh_host.sh`, replug the root board, and check `ls -l /dev/tallow-root`. Then run `systemctl --user enable --now tallowd.service`.
   **Pass:** `data/mesh/map.json` stays fresh, and `/devices` on the Rust panel shows it. It holds the root's port, so stop the service before `flash`, `monitor`, or `backup` on that board.

## Any time

8. **Put `toby-cam-0` on ESPHome (T4.0).** The steps are in [p4-cams.md](p4-cams.md). `/dev/ttyACM0` now reports as `USB_JTAG_serial_debug_unit`, not the earlier TinyUSB name, so check what it runs first. **Pass:** the panel shows its picture for a week.

9. **Attach a third classic board (T1.8).** **Pass:** at least 90% over two hops, end node at rank 2.

10. **Recover `toby-cam-1`.** T4.5 needs it on a known-good ESPHome image. It is out of every other step.

## After the Grok run and the review

11. **Drive the H7020 over the mesh (T1.11).** This needs G5, and a node that hears the light. **Pass:** connect under 10 s, one write under 500 ms after connect.

12. **OTA a leaf (T1.9).** This needs G7, G8, and a USB flash of every board, because the bootloader changes. Update a leaf one hop away, then push a build that crashes after 90 s.
    **Pass:** it updates, then rolls back. Do not roll out to any board that is not on a desk.

13. **Rust on `toby-cam-0` (T4.3, T4.4).** Run `cam-backup`, then `cam-flash`, then `cam_stream_check.py --address 10.87.0.23`. Then push a crashing build. **Pass:** the panel shows the picture, and the crash rolls back. Nothing moves to `toby-cam-1` until it does.

14. **Fleet, cut-over, wall cam (T1.12, T3.10, T4.5).** In that order, each when the one before has held for its full soak.

15. **Retire the old stack (T5.1 to T5.3).** Only at your word.
