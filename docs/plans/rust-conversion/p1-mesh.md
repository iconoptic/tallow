# Phase 1: finish the mesh

The nodes hear the house over one hop. This phase proves many hops, finds Classic speakers by address, closes the replay gap, then adds OTA and control.

Files live in `firmware/mesh/`. Read `core/src/mesh.rs` and `core/tests/sim.rs` before any routing task.

## T1.1 Golden frames

**Done (2026-10-31).**

- **Tier:** Grok. **Needs:** nothing.
- **Files:** new `firmware/mesh/core/tests/golden.rs`, `firmware/mesh/core/README.md`.
- **Do:** Seal one frame of each `FrameKind` with key `[7; 32]`, fixed origin, boot, and seq, and assert the exact hex bytes. Add one test per `Command` field position. In the README, list every frame kind and its payload bytes, using `wire.rs`, `sightings.rs`, and `names.rs` as the source. Stats layout comes from `node/src/main.rs`.
- **Done when:** `tools/mesh.sh check --fast` passes. Changing any byte in `wire.rs`'s `seal` makes `golden.rs` fail (try it, then revert).
- **Stop if:** a frame's bytes differ between runs. The nonce must be deterministic for fixed inputs.

## T1.2 Timestamped routing events

- **Tier:** Grok. **Needs:** nothing.
- **Why:** A 10-minute run after a root handover showed 5 parent changes and 36 drops, with no way to date them.
- **Files:** `core/src/mesh.rs`, `core/src/wire.rs`, `node/src/main.rs`, `tools/mesh_host.py`.
- **Do:** Add `FrameKind::Route = 7`. On each parent change, send `{uptime_s: u32, old_parent: u32, new_parent: u32, reason: u8}`. Reason is 0 beacon timeout, 1 failed acks, 2 better path. Bump `VERSION`. A node with no parent keeps the last 4 events in a fixed array and sends them after it regains one. Print them from `mesh_host.py`, and add a test to `test_mesh_host.py`.
- **Done when:** `tools/mesh.sh check` passes, and a new sim test shows a beacon timeout, then a report carrying reason 0.
- **Stop if:** the array would need the heap.

## T1.3 Force a chain on a desk

- **Tier:** Sonnet. **Needs:** T1.2.
- **Why:** Boards on one desk all hear each other, so nothing forces a second hop.
- **Files:** `core/src/mesh.rs`, `node/src/main.rs`, `tools/mesh_host.py`.
- **Do:** Add a bench deny list to `Mesh`: `deny: [u32; 4]`, set by a serial line `X <id-hex>` on a board's own port and cleared by `X` alone. `receive` ignores beacons from denied origins before `on_beacon`, so they never become neighbours. It lives in RAM only. Add `--deny PORT ID` handling as a small `tools/mesh_bench.py` that opens a board's port, waits 4 s, sends the lines, and holds the port open for `--seconds`.
- **Done when:** `tools/mesh.sh check` passes. A sim test: three nodes all linked, with 0 and 2 denying each other, gives 2 a rank of 2.
- **Stop if:** denying needs a frame on the air. It must stay serial only, so it cannot be used remotely.

## T1.4 Replace `EspGap` with a raw callback

- **Tier:** Sonnet. **Needs:** nothing.
- **Why:** `esp-idf-svc` builds `&str` from off-air bytes with `from_utf8_unchecked` for `RemoteName` and `AuthenticationCompleted`. Merely building that value is undefined behaviour, and T1.5 needs remote names.
- **Files:** `node/src/bluetooth.rs`.
- **Do:** Call `esp_bt_gap_register_callback` from `esp-idf-sys` with an `extern "C"` function. Handle `ESP_BT_GAP_DISC_RES_EVT`, `ESP_BT_GAP_DISC_STATE_CHANGED_EVT`, and `ESP_BT_GAP_READ_REMOTE_NAME_EVT`, reading raw bytes. Names go through `from_utf8_lossy` or stay bytes. Remove `EspGap` and `eir_bytes`. Keep `start_discovery` as a raw call.
- **Done when:** `tools/mesh.sh check` passes. Then **You:** flash two boards, run `D` on one, and confirm the other logs it, as in [docs/01](../../01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh).
- **Stop if:** the crate blocks raw registration while `BtDriver` is alive.

## T1.5 Find Classic devices by address

- **Tier:** Sonnet. **Needs:** T1.4.
- **Why:** Inquiry only finds discoverable devices. A paired speaker stays silent unless paged by address.
- **Files:** `core/src/mesh.rs`, `core/src/wire.rs`, `node/src/bluetooth.rs`, `node/src/main.rs`, `tools/mesh_host.py`.
- **Do:** Add `FrameKind::Watch = 8`, flooded down, carrying up to 16 addresses as a full replacement of the list. Each node pages one address at a time with `esp_bt_gap_read_remote_name`, one every 5 s, never during an inquiry. Success is a sighting of kind `Classic` with RSSI -127 and a name record. A page takes up to 5 s of radio time on a miss, so this rate is a hard limit. Stagger by node id so two nodes do not page together.
- **Done when:** `tools/mesh.sh check` passes. **You:** with your speaker paired but idle, its address appears in `map.json`. Repeat with it playing, and record both results in `docs/01`.
- **Stop if:** delivery in `--trial` falls below 95% with paging on.

## T1.6 Replay protection across reboots

- **Tier:** Sonnet. **Needs:** T1.1.
- **Why:** `Mesh::fresh` accepts any frame with a different boot id, so a frame recorded in an earlier boot passes.
- **Files:** `core/src/mesh.rs`, `core/tests/sim.rs`, `node/src/main.rs`, `core/README.md`.
- **Do:** Boot id becomes `(counter << 12) | random12`, where `counter` is a `u20` kept in NVS and raised before first use each boot. `fresh` rejects a frame whose counter is below the stored one for that origin. Origin entries expire after 10 min of silence, so a board with erased NVS can rejoin. The random bits keep nonces unique if NVS is erased. Add sim tests: an old-boot frame is rejected, and a new boot is accepted.
- **Done when:** `tools/mesh.sh check` passes, and the `fresh` comment no longer says old boots pass.
- **Stop if:** NVS writes would happen more than once per boot. Flash wears out.

## T1.7 Stagger inquiries

- **Tier:** Grok. **Needs:** nothing.
- **Why:** A Classic inquiry cuts the BLE adverts a node hears (39 in 21 s against 168), and nodes commanded together inquire together.
- **Files:** `node/src/main.rs`.
- **Do:** Offset `next_inquiry` after a command by `mesh.id % period`, so nodes spread across the period.
- **Done when:** `tools/mesh.sh check` passes.
- **Stop if:** it needs a change outside this offset.

## T1.8 Prove multi-hop on real radios

- **Tier:** You, with Grok for the write-up. **Needs:** T1.2, T1.3, a third classic board.
- **Do:** Run `tools/mesh.sh ports`, add the board to `boards.tsv`, and flash all three. Chain them with `tools/mesh_bench.py`: root, middle, end, with root and end denied. Run `tools/mesh.sh host <root> --trial --pass-mark 90`. Unplug the middle board and confirm the end node reports no route. Then clear the deny list and confirm it joins the root directly.
- **Pass:** at least 90% delivery over two hops at BLE 30/100. The end node's `rank` is 2. Reports return within 30 s of clearing the deny list.
- **Done when:** results are in `docs/01`, replacing "simulated only".
- **Stop if:** delivery is under 90%. Report the `--trial` output and do not tune.

## T1.9 OTA over the mesh

- **Tier:** Sonnet. **Needs:** T1.6, T1.8.
- **Why:** Nodes around the house cannot be flashed over USB.
- **Files:** new `core/src/ota.rs`, `node/src/ota.rs`, `node/sdkconfig.defaults`, `tools/mesh.sh`, `tools/mesh_bench.py` or `tallowd`.
- **Do:**
  - Enable `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`. This needs one USB flash of every board, because the bootloader changes. Write otadata at that flash.
  - Add `FrameKind::Ota = 9`, unicast root to one target through the mesh, one image at a time. Sub-kinds: Begin (size, sha256), Chunk (offset, up to 180 bytes), Status (bitmap of missing 256-chunk windows), Commit.
  - Sign the image's sha256 with ed25519. The private key stays in gitignored `firmware/mesh/ota.key`, and the public key is baked in. A node rejects an unsigned image. Add `tools/mesh.sh ota-key`. The mesh key alone must not allow an update.
  - Only one frame in flight per hop. Target rate is 20 frames/s, so about 6 minutes per hop for a 1.4 MB image.
  - A new image marks itself valid after **5 minutes** with a parent or as root and a clean heap. A build that dies later than a short window stays installed: that is how `toby-cam-1` was lost.
- **Done when:** `tools/mesh.sh check` passes, and a sim test covers lost chunks, a bad signature, and a bad hash. **You:** update a leaf one hop away, then flash a deliberately crashing image and watch it roll back.
- **Stop if:** either check fails on hardware. Do not roll out to any board that is not on a desk.

## T1.10 GATT client over the mesh

- **Tier:** Sonnet. **Needs:** T1.6.
- **Why:** This is what makes central control possible.
- **Files:** new `node/src/gatt.rs`, `core/src/wire.rs`, `core/README.md`.
- **Do:** Add `FrameKind::Gatt = 10`, with request and response frames carrying a request id: Connect (addr, addr type), Write (handle, up to 180 bytes), Read, Subscribe, Disconnect. Responses go up. Notifications go up with the connection's id. Use Bluedroid's `esp_ble_gattc_*` calls directly. One connection per node, and connect only to addresses on a whitelist the host sends, per the isolation rule. Pause inquiry and paging while connected.
- **Done when:** `tools/mesh.sh check` passes, with sim tests for request ids, a lost response, and a refused address.
- **Stop if:** heap low falls under 60 KB while connected.

## T1.11 Drive the H7020 over the mesh

- **Tier:** You, with Sonnet. **Needs:** T1.10, and a node that hears the H7020. None has yet.
- **Do:** Power on, then off, from the host through one hop, logging each stage's time.
- **Pass:** one write under 500 ms after connect, connect under 10 s. Record the numbers in `docs/devices/govee-h7020.md`.
- **Stop if:** the numbers miss. The plan's risk section covers it. Report them and let the user decide.

## T1.12 Fleet rollout

- **Tier:** You, with Grok. **Needs:** T1.8.
- **Do:** For each remaining board, attach it, run `ESPHOME_DEVICE=<port> tools/esphome.sh mac` to read chip and flash, add it to `boards.tsv`, run `tools/mesh.sh backup <port>`, then flash it. Number from `node-2`. Skip any board that is not 4 MB or is not a classic ESP32, and report it.
- **Done when:** every node appears in `map.json`, and `docs/devices/esp32-devkit.md` lists the fleet.
