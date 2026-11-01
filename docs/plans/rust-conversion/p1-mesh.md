# Phase 1: finish the mesh

The nodes hear the house over one hop. This phase proves many hops, finds Classic speakers by address, closes the replay gap, then adds OTA and control.

Files live in `firmware/mesh/`. Read `core/src/mesh.rs` and `core/tests/sim.rs` before any routing task.

**Flash state (2026-10-31).** Both boards run the cross-check build, frame version `0xA1`: Classic RSSI and class of device, inquiry cut before name requests, beacon source check, parent events. Classic readings from before that flash are meaningless. The source is ahead of them at `0xA6`, and boards on different versions refuse each other's frames, so flash every board in one pass. **You:** `tools/mesh.sh build`, then `tools/mesh.sh flash <port>` on each.

## T1.1 Golden frames

**Done (2026-10-31).**

- **Tier:** Grok. **Needs:** nothing.
- **Files:** new `firmware/mesh/core/tests/golden.rs`, `firmware/mesh/core/README.md`.
- **Do:** Seal one frame of each `FrameKind` with key `[7; 32]`, fixed origin, boot, and seq, and assert the exact hex bytes. Add one test per `Command` field position. In the README, list every frame kind and its payload bytes, using `wire.rs`, `sightings.rs`, and `names.rs` as the source. Stats layout comes from `node/src/main.rs`.
- **Done when:** `tools/mesh.sh check --fast` passes. Changing any byte in `wire.rs`'s `seal` makes `golden.rs` fail (try it, then revert).
- **Stop if:** a frame's bytes differ between runs. The nonce must be deterministic for fixed inputs.
- **Extended at the cross-check:** `golden.rs` also pins decoding, refused versions and kinds, the size limits, and payloads built by the real `Mesh`, `Names`, and `Sightings`.

## T1.2 Timestamped routing events

**Done (2026-10-31).**

- **Tier:** Grok. **Needs:** nothing.
- **Why:** A 10-minute run after a root handover showed 5 parent changes and 36 drops, with no way to date them.
- **Files:** `core/src/mesh.rs`, `core/src/wire.rs`, `core/tests/sim.rs`, `core/tests/routing.rs`, `core/tests/golden.rs`, `core/README.md`, `node/src/main.rs`, `tools/mesh_host.py`, `tools/tests/test_mesh_host.py`. The `wire.rs` change is an enum variant and a `VERSION` bump, not crypto, so this stays Grok tier.
- **Do:**
  - Add `FrameKind::Route = 7`. On each parent change, send `{uptime_s: u32, old_parent: u32, new_parent: u32, reason: u8}`. Reason is 0 beacon timeout, 1 failed acks, 2 better path. Zero means no parent. Bump `VERSION`.
  - `Event::Parent` fires once per change today, including a loss with no replacement, and carries only the new parent. Add the old parent and the reason to it. `reselect` already takes `before_id`. Its three callers know the reason: `tick` is 0, `tx_done` is 1, `on_beacon` is 2.
  - A node with no parent keeps the last 4 events in a fixed array and sends them after it regains one.
  - Every node now sends a Route frame when it joins. `tests/sim.rs` pushes every `Event::Up` into `arrivals`, so existing tests would count them. Add `kind` to `Arrival` and have those tests look at their own kind. That keeps each test's meaning, so it is not weakening one.
  - The bump changes all six golden hex strings. Regenerate them, add a Route golden, and add a Route table to `core/README.md`.
  - Print Route events from `mesh_host.py`, with a test.
- **Done when:** `tools/mesh.sh check` passes, and a new sim test shows a beacon timeout, then a report carrying reason 0.
- **Stop if:** the array would need the heap.

## T1.3 Force a chain on a desk

**Code done (2026-10-31), not used on hardware.**

- **Tier:** Sonnet. **Needs:** T1.2.
- **Why:** Boards on one desk all hear each other, so nothing forces a second hop.
- **Files:** `core/src/mesh.rs`, `core/tests/sim.rs`, `node/src/main.rs`, new `tools/mesh_bench.py`, `tools/mesh.sh`.
- **Do:**
  - Add a bench deny list to `Mesh`: `deny: [u32; 4]`, set by a serial line `X <id-hex>` on a board's own port and cleared by `X` alone. `receive` ignores beacons from denied origins before `on_beacon`, so they never become neighbours. It lives in RAM only, so a port open resets it.
  - Add `tools/mesh_bench.py`: it opens a board's port, waits 4 s, sends the deny lines, holds the port open for `--seconds`, and prints every `@` line the board writes.
  - Add `tools/mesh.sh bench <port> [arguments]`, built like `monitor`: same `need_classic_port`, same image. `.venv` has no `serial` module, and the user is not in `dialout`.
  - A chain only needs the end node to deny the root. The root's port belongs to `tools/mesh.sh host`, and reopening it would reset the board.
- **Done when:** `tools/mesh.sh check` passes. A sim test: three nodes all linked, with node 2 denying node 0, gives node 2 a rank of 2.
- **Stop if:** denying needs a frame on the air. It must stay serial only, so it cannot be used remotely.

## T1.4 Replace `EspGap` with a raw callback

**Code done (2026-10-31), not flashed.** The hardware check is yours.

- **Tier:** Sonnet. **Needs:** nothing.
- **Why:** `esp-idf-svc` runs `from_utf8_unchecked` on off-air bytes for `RemoteName` and `AuthenticationCompleted` while building the event, before any of our code sees it. Merely building that value is undefined behaviour, and T1.5 needs remote names.
- **Files:** `node/src/bluetooth.rs`.
- **Do:** Call `esp_bt_gap_register_callback` from `esp-idf-sys` with an `extern "C"` function. Handle `ESP_BT_GAP_DISC_RES_EVT`, `ESP_BT_GAP_DISC_STATE_CHANGED_EVT`, and `ESP_BT_GAP_READ_REMOTE_NAME_EVT`, reading raw bytes. Names go through `from_utf8_lossy` or stay bytes. Remove `EspGap`. Everything it did becomes a raw call: `esp_bt_gap_set_scan_mode`, `esp_bt_gap_start_discovery`, and `esp_bt_gap_cancel_discovery`. Keep what `read_props` does: each property's `val` is a pointer to the value, and `PropData::prop` gets that wrong. Keep `cut_inquiry`: without it, the stack pages strangers for their names.
- **Done when:** `tools/mesh.sh check` passes. Then **You:** flash two boards, run `D` on one, and confirm the other logs it, as in [docs/01](../../01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh), with a Classic RSSI between -100 and -20 in `map.json`.
- **Stop if:** the crate blocks raw registration while `BtDriver` is alive.

## T1.5 Find Classic devices by address

**Code done (2026-10-31), not flashed.** The hardware check is yours.

- **Tier:** Sonnet. **Needs:** T1.4.
- **Why:** Inquiry only finds discoverable devices. A paired speaker stays silent unless paged by address.
- **Files:** `core/src/mesh.rs`, `core/src/wire.rs`, `node/src/bluetooth.rs`, `node/src/main.rs`, `tools/mesh_host.py`.
- **Do:** Add `FrameKind::Watch = 8`, flooded down, carrying up to 16 addresses as a full replacement of the list. That list is the whitelist for paging: `cut_inquiry` stays, so nothing else gets paged. Each node pages one address at a time with `esp_bt_gap_read_remote_name`, one every 5 s, never during an inquiry. Success is a sighting of kind `Classic` with RSSI -127 and a name record. A page takes up to 5 s of radio time on a miss, so this rate is a hard limit. Stagger by node id so two nodes do not page together.
- **Done when:** `tools/mesh.sh check` passes. **You:** with your speaker paired but idle, its address appears in `map.json`. Repeat with it playing, and record both results in `docs/01`.
- **Stop if:** delivery in `--trial` falls below 95% with paging on.

## T1.6 Replay protection across reboots

**Done (2026-10-31).** Built and tested, not flashed.

- **Tier:** Sonnet. **Needs:** T1.1.
- **Why:** `Mesh::fresh` accepts any frame with a different boot id, and that frame restarts the origin's window. So one recorded frame from an earlier boot re-opens every replay of the current boot, and a recorded Command from any earlier host run still applies. Each `tools/mesh.sh host` run reboots the root, so those pile up.
- **Files:** `core/src/mesh.rs`, `core/src/wire.rs`, `core/tests/sim.rs`, `core/tests/golden.rs`, `node/src/main.rs`, `core/README.md`.
- **Do:**
  - Boot id becomes `(counter << 12) | random12`, where `counter` is a `u20` kept in NVS and raised before first use each boot. When NVS holds no counter, seed it from `esp_random` with the top bit clear. A plain restart at zero would repeat a boot id with odds of 1 in 4096 per earlier boot, and a repeated boot id with the same sequence repeats a nonce, which breaks ChaCha20-Poly1305. Seeded, that needs both the 19-bit counter and the 12 random bits to match.
  - `fresh` rejects a frame whose counter is below the stored one for that origin, and an older frame must not touch the stored entry.
  - Origin entries expire after 10 min of silence, so a board with erased NVS can rejoin. That leaves a window: a replay of a board silent for over 10 min is accepted once. Say so in the code comment and `docs/01`.
  - This changes what the boot field means, so bump `VERSION` and regenerate the goldens. Every board is reflashed together.
  - Add sim tests: an old-boot frame is rejected, it does not re-open the window for the current boot, a recorded Command does not re-apply after the ring rolls, and a new boot is accepted.
- **Done when:** `tools/mesh.sh check` passes, and the `fresh` comment no longer says old boots pass.
- **Stop if:** NVS writes would happen more than once per boot. Flash wears out.

**Known limit:** the origin table is RAM only, so a board that just rebooted accepts one recorded frame per origin until that origin's next real frame, about 2 s for a neighbour's beacon. A board with erased NVS is refused by neighbours holding a higher counter for up to 10 min.

## T1.7 Stagger inquiries

**Done (2026-10-31).**

- **Tier:** Grok. **Needs:** nothing.
- **Why:** A Classic inquiry cuts the BLE adverts a node hears (39 in 21 s against 168), and nodes commanded together inquire together.
- **Files:** `node/src/main.rs`.
- **Do:** Offset `next_inquiry` after a command, so nodes spread across the period. Period is `settings.inquiry_period_s`, in seconds, and `next_inquiry` is in milliseconds. Period 0 is valid and means no inquiries, and `--trial` sends it first, so a plain `% period` panics every node: skip the offset when it is 0. The offset is `(mesh.id % period as u32) * 1000` ms.
- **Done when:** `tools/mesh.sh check` passes. `main.rs` has no tests, so also read your line again for the 0 case.
- **Stop if:** it needs a change outside this offset.

## T1.8 Prove multi-hop on real radios

- **Tier:** You, with Grok for the write-up. **Needs:** T1.2, T1.3, a third classic board.
- **Do:** Run `tools/mesh.sh ports`, add the board to `boards.tsv`, and flash all three. Attach root, middle, and end. Start `tools/mesh.sh bench <end-port> --deny <root-id> --seconds 900` on the end node and leave it running: its port holds the deny list and prints the `@` lines. In a second shell, run `tools/mesh.sh host <root> --trial --pass-mark 90`. Unplug the middle board and confirm the end node's port prints `@P 00000000`. Then stop `bench` and start it again with no `--deny`, and confirm the end node joins the root directly.
- **Pass:** at least 90% delivery over two hops at BLE 30/100. The end node's `rank` is 2. Reports return within 30 s of clearing the deny list.
- **Done when:** results are in `docs/01`, replacing "simulated only".
- **Stop if:** delivery is under 90%. Report the `--trial` output and do not tune.

## T1.8b Route down to one node

**Done (2026-10-31).** Built and tested, not flashed.

- **Tier:** Sonnet. **Needs:** T1.2.
- **Why:** The mesh has no way to reach one node. Commands flood with no acks, and everything else goes up. OTA and GATT both need a frame that crosses hops to one target, retried at each hop.
- **Files:** `core/src/mesh.rs`, `core/src/wire.rs`, `core/tests/sim.rs`, `core/README.md`.
- **Do:** Learn routes from traffic going up. When a node forwards or receives an upward frame, record `origin -> src mac` in a fixed table of 32 entries, oldest evicted, expiring after 60 s. A downward frame carries its target id as the first 4 payload bytes. Each hop that is not the target unicasts it to the table's next hop, with the same retries as upward frames. No entry means count a drop and report it up. Add `Mesh::send_down(target, kind, payload)`.
- **Done when:** `tools/mesh.sh check --fast` passes. Sim tests: a frame crosses 4 hops down by unicast alone, a stale route expires, and a target that moved is reached again after its next report.
- **Stop if:** the table needs the heap, or a node's memory grows by more than 1 KB.

## T1.9a Signed image check

**Done (2026-10-31).** Built and tested in `core`, not used by the node yet. `ota-key` is written but not run.

- **Tier:** Sonnet. **Needs:** nothing. It needs no hardware, and T4.4 reuses it.
- **Files:** new `firmware/mesh/core/src/ota.rs`, `core/src/lib.rs`, `core/Cargo.toml`, `tools/mesh.sh`.
- **Deps:** `ed25519-compact` and `sha2`, both with `default-features = false`.
- **Do:** A `no_std`, no-heap check: given an image's sha256, a signature, and a public key, say yes or no. Keep the hash incremental so a node can feed it chunks. Add `tools/mesh.sh ota-key`: it writes the private key to gitignored `firmware/mesh/ota.key` (mode 0600, once only, never printed), and the public key to tracked `firmware/mesh/ota.pub`. The check takes the public key as an argument, so `core` builds and tests with no key file. A firmware that uses it bakes the key in from its own `build.rs` and fails to build without it, so there is no default. The mesh key alone must not allow an update.
- **Done when:** `tools/mesh.sh check` passes. Tests: a good signature, a bad signature, a bad hash, a wrong public key. The check still builds for the node.
- **Stop if:** either crate needs `alloc` or fails on the xtensa target. Report it and do not swap crates unasked.

## T1.9 OTA over the mesh

- **Tier:** Sonnet. **Needs:** T1.6, T1.8, T1.8b, T1.9a.
- **Why:** Nodes around the house cannot be flashed over USB.
- **Files:** `core/src/ota.rs`, `core/src/mesh.rs`, `core/src/wire.rs`, new `node/src/ota.rs`, `node/build.rs`, `node/sdkconfig.defaults`, `tools/mesh.sh`, `tools/mesh_bench.py` or `tallowd`.
- **Do:**
  - Enable `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`. This needs one USB flash of every board, because the bootloader changes. Write otadata at that flash.
  - Add `FrameKind::Ota = 9`, sent with `send_down`, one image at a time. Sub-kinds: Begin (size, sha256, signature), Chunk (offset, up to 170 bytes, since `send_down` takes 4), Status (bitmap of missing 256-chunk windows), Commit.
  - A node rejects an image that fails T1.9a's check. `node/build.rs` bakes in `firmware/mesh/ota.pub`, and the build fails without it.
  - Only one frame in flight per hop. Target rate is 20 frames/s, so about 6 minutes per hop for a 1.4 MB image.
  - A new image marks itself valid after **5 minutes** with a parent or as root and a clean heap. A build that dies later than a short window stays installed: that is how `toby-cam-1` was lost.
- **Done when:** `tools/mesh.sh check` passes, and a sim test covers lost chunks, a bad signature, and a bad hash. **You:** update a leaf one hop away, then flash a deliberately crashing image and watch it roll back.
- **Stop if:** either check fails on hardware. Do not roll out to any board that is not on a desk.

## T1.10 GATT client over the mesh

**Code done (2026-10-31), not flashed.** The heap check is yours.

- **Tier:** Sonnet. **Needs:** T1.6, T1.8b.
- **Why:** This is what makes central control possible.
- **Files:** new `core/src/gatt.rs`, `core/src/mesh.rs`, `core/src/wire.rs`, `core/README.md`, new `node/src/gatt.rs`, `node/src/main.rs`.
- **Do:** Add `FrameKind::Gatt = 10`, sent down with `send_down`, with request and response frames carrying a request id: Connect (addr, addr type), Write (handle, up to 170 bytes), Read, Subscribe, Disconnect. Responses go up. Notifications go up with the connection's id. Keep request ids, timeouts, and the whitelist check in `core/src/gatt.rs`, so the sim can test them. `node/src/gatt.rs` holds only the `esp_ble_gattc_*` calls. One connection per node, and connect only to addresses on a whitelist the host sends, per the isolation rule. Pause inquiry and paging while connected.
- **Done when:** `tools/mesh.sh check` passes, with sim tests for request ids, a lost response, and a refused address.
- **Stop if:** heap low falls under 60 KB while connected.

## T1.11 Drive the H7020 over the mesh

- **Tier:** You, with Sonnet. **Needs:** T1.10, and a node that hears the H7020. None has yet.
- **Do:** Power on, then off, from the host through one hop, logging each stage's time.
- **Pass:** one write under 500 ms after connect, connect under 10 s. Record the numbers in `docs/devices/govee-h7020.md`.
- **Stop if:** the numbers miss. The plan's risk section covers it. Report them and let the user decide.

## T1.12 Fleet rollout

- **Tier:** You, with Grok. **Needs:** T1.8.
- **Do:** For each remaining board, attach it, run `ESPHOME_DEVICE=<port> tools/esphome.sh mac` to read chip and flash, add it to `boards.tsv`, run `tools/mesh.sh backup <port>`, then flash it. `backup` and `flash` refuse a port until its row exists. Number from `node-2`. Skip any board that is not 4 MB or is not a classic ESP32, and report it.
- **Done when:** every node appears in `map.json`, and `docs/devices/esp32-devkit.md` lists the fleet.
