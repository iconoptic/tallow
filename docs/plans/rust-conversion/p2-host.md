# Phase 2: the host daemon

`tallowd` takes over from `tools/mesh_host.py`, keeps the map, and adds rooms and identities. It lives in `host/tallowd`. After T2.7 `tallowd` is the one you run. `mesh_host.py` stays in the tree as a fallback until T5.2 removes it.

Add dependencies only where a task names them.

## T2.1 Serial link

- **Tier:** Grok. **Needs:** nothing.
- **Files:** `host/tallowd/src/link.rs`, `main.rs`, `Cargo.toml`.
- **Deps:** `serialport` with default features off, `anyhow`.
- **Do:** Port `Link` from `tools/mesh_host.py`. Open with DTR and RTS clear, wait 4 s, send `H\n` once a second, and yield `@` lines. Move parsing into a pure `parse_line(&str) -> Option<Line>` so it can be tested with no port.
- **Done when:** `tools/mesh.sh daemon check` passes, and tests cover the lines in `test_mesh_host.py`'s `test_serial_noise_is_not_an_up_line`.
- **Stop if:** `serialport` needs libudev in the image. Use `default-features = false`.

## T2.2 Frame decoding shared with the firmware

- **Tier:** Grok. **Needs:** T1.1.
- **Files:** `firmware/mesh/core/src/sightings.rs`, `names.rs`, new `stats.rs`, `host/tallowd/src/decode.rs`.
- **Do:** Add `no_std` iterators beside the encoders: `Records::new(payload)` and `NameRecords::new(payload)`, plus a `Stats` struct with `encode` and `decode`, and use `Stats::encode` in `node/src/main.rs`. `tallowd` uses these, never its own offsets.
- **Done when:** `tools/mesh.sh check` passes, with a round-trip test for each type and the truncated-input cases from `test_mesh_host.py`.
- **Stop if:** an iterator would need the heap.

## T2.3 The map

- **Tier:** Grok. **Needs:** T2.1, T2.2.
- **Files:** `host/tallowd/src/map.rs`.
- **Deps:** `serde`, `serde_json`.
- **Do:** Port the `Map` class. Write the same `data/mesh/map.json` shape atomically, every 5 s: temporary file, then rename. Add `tallowd run --port <p> --seconds <n>`.
- **Done when:** `tools/mesh.sh daemon check` passes. A test feeds a recorded list of `@U` lines through both `mesh_host.py` and `tallowd` and compares the JSON.
- **Stop if:** the JSON differs in a way the Python's behaviour does not explain.

## T2.4 Resolve rotating addresses

- **Tier:** Sonnet. **Needs:** T2.3.
- **Why:** The WF-1000XM5 earbuds showed up as three addresses in one day. Without this, one device looks like many.
- **Files:** `host/tallowd/src/irk.rs`.
- **Deps:** `aes`.
- **Do:** Implement `ah(irk, prand)` from Bluetooth Core Vol 3 Part H 2.2.2. Check it against the sample data in the spec: IRK `ec0234a357c8ad05341010a60a397d9b`, prand `708194`, hash `0dfbaa`. Confirm those numbers against the spec, since they were written from memory. Read keys from gitignored `data/mesh/irks.toml`, mode 0600, mapping a device name to its IRK. Resolve on the host only: IRKs never go on a node. An address is resolvable only if its top two bits are `01`.
- **Done when:** `tools/mesh.sh daemon check` passes.
- **Stop if:** the spec vector fails.

**Open for the user:** getting an IRK usually means pairing a phone once, and `.cursor/rules/iot-network-isolation.mdc` keeps phones off the whitelist. Reading it from the phone's own backup avoids pairing. See `references/presence/irk-capture` and `references/presence/ESPresense`. Ask before pairing anything.

## T2.5 One device, many addresses

- **Tier:** Grok. **Needs:** T2.3.
- **Why:** The Klipsch soundbar is `23:00:00:11:A0:38` on Classic and `DC:00:00:11:A0:38` on BLE.
- **Files:** `host/tallowd/src/identity.rs`.
- **Deps:** `toml`.
- **Do:** Read `data/mesh/devices.toml`: a device name, kind, and a list of addresses. Group the map under these. Also add a suggestions list: unnamed entries sharing a name, or sharing the low 4 address bytes. Print suggestions and never merge them on their own.
- **Done when:** `tools/mesh.sh daemon check` passes.

## T2.6 Rooms

- **Tier:** Grok. **Needs:** T2.5.
- **Files:** `host/tallowd/src/rooms.rs`.
- **Do:** Read `data/mesh/rooms.toml`: node id to room name. For each device, keep the last 12 RSSI values per node. The room is the node with the highest median. Change room only when the new one leads by 3 dB for 3 reports in a row. Read `references/presence/bermuda` for ideas, but copy no code without checking its licence. Report a room, never a position.
- **Done when:** `tools/mesh.sh daemon check` passes, with tests for a steady device, a boundary device that must not flicker, and a real move.

## T2.7 Replace `mesh_host.py`

- **Tier:** Grok, with You for the service. **Needs:** T2.3.
- **Files:** `host/tallowd/src/trial.rs`, new `tools/setup_mesh_host.sh`, `tools/tallowd.service`, `tools/mesh.sh`.
- **Do:** Port `--trial` and `--set`. Write `tools/setup_mesh_host.sh`, in the style of `tools/install_re_toolchain_fedora.sh`: a udev rule giving this user the root node's port by USB path, and a user service. Do not run it.
- **Done when:** `tools/mesh.sh daemon check` passes. **You:** run the script, and `--trial` matches the Python's numbers within 3 points.
- **Stop if:** the service would need root at runtime.
