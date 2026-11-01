# Phase 2: the host daemon

`tallowd` takes over from `tools/mesh_host.py`, keeps the map, and adds rooms and identities. It lives in `host/tallowd`. After T2.7 `tallowd` is the one you run. `mesh_host.py` stays in the tree as a fallback until T5.2 removes it.

Add dependencies only where a task names them.

## T2.1 Serial link

**Done (2026-10-31).**

- **Tier:** Grok. **Needs:** nothing.
- **Files:** `host/tallowd/src/link.rs`, `main.rs`, `Cargo.toml`.
- **Deps:** `serialport` with default features off, `anyhow`.
- **Do:** Port `Link` from `tools/mesh_host.py`. Open with DTR and RTS clear, wait 4 s, send `H\n` once a second, and yield `@` lines. Move parsing into a pure `parse_line(&str) -> Option<Line>` so it can be tested with no port.
- **Done when:** `tools/mesh.sh daemon check` passes, and tests cover the lines in `test_mesh_host.py`'s `test_serial_noise_is_not_an_up_line`.
- **Stop if:** `serialport` needs libudev in the image. Use `default-features = false`.
- **Extended at the cross-check:** the buffer is capped at 2 KB, `Link::open` refuses anything but a `/dev/ttyUSB*` port, and `Link::send_command` and `DEFAULT_SCAN` are ported.

## T2.2 Frame decoding shared with the firmware

**Done (2026-10-31).**

- **Tier:** Grok. **Needs:** T1.1.
- **Files:** `firmware/mesh/core/src/sightings.rs`, `names.rs`, new `stats.rs`, `lib.rs`, `core/README.md`, `node/src/main.rs`, `host/tallowd/src/decode.rs`.
- **Do:** Add `no_std` iterators beside the encoders: `Records::new(payload)` and `NameRecords::new(payload)`, plus a `Stats` struct with `encode` and `decode`, and use `Stats::encode` in `node/src/main.rs`. `tallowd` uses these, never its own offsets. Leave the `unsafe` heap query in `main.rs` as it is: only the bytes after it move.
- **Done when:** `tools/mesh.sh check` passes, with a round-trip test for each type and the truncated-input cases from `test_mesh_host.py`.
- **Stop if:** an iterator would need the heap.

## T2.3 The map

**Done (2026-10-31).**

- **Tier:** Grok. **Needs:** T2.1, T2.2.
- **Files:** `host/tallowd/src/map.rs`, `main.rs`, `tools/mesh_host.py`, new `tools/tests/test_map_fixture.py`, new `tools/tests/fixtures/map_lines.txt`, new `tools/tests/fixtures/map.json`.
- **Deps:** `serde`, `serde_json`.
- **Do:**
  - Port the `Map` class. `Map` takes `now` as an argument, because `handle` in the Python reads the clock itself. Write the same `data/mesh/map.json` shape atomically, every 5 s: temporary file, then rename.
  - Add `tallowd run --port <p> --seconds <n> --out <path>`. It calls `Link::open`, which refuses a cam's port, then waits, then sends `DEFAULT_SCAN` as the Python does after 4 s. Every run sends it, because other nodes keep their last settings across the root's reset.
  - Handle every frame kind `mesh_host.py` handles at the time, including any added by T1.2.
  - The two languages cannot be compared live: the build container has no `tools/`, and agents never open a port. So hand-write `map_lines.txt`, a list of `@U` lines, and have `test_map_fixture.py` feed them through `mesh_host.handle` with `time.time` patched to a fixed value and write `fixtures/map.json`, failing if it differs from what is checked in. A `tallowd` test reads the same two files through `include_str!`, using the same fixed time, and compares parsed JSON, not text.
  - Three differences to match: `nearest` is `max()` in insertion order, so ties go to the first node heard. Python's `round()` rounds halves to even, and Rust's does not. Python writes `indent=1`, which is why the test compares parsed JSON.
- **Done when:** `tools/mesh.sh check --fast` passes, and `test_map_fixture.py` is added to its list of Python tests. Editing a value in `map.json` makes both sides fail.
- **Stop if:** the JSON differs in a way the Python's behaviour does not explain.

## T2.4 Resolve rotating addresses

**Done (2026-10-31).** `host/tallowd/src/irk.rs` has `ah` and the key file; `map.rs` sets `resolved`. Keys are resolved once per address, when the address is first seen, and cached on its device. `tallowd run --irks <path>` defaults to `data/mesh/irks.toml`, and the file is read before the port opens. `tools/mesh.sh daemon check` passes, with the spec vector checked both as the hash and as the full ciphertext. Still open: getting a real IRK (see the note below), which needs the user's go-ahead.

- **Tier:** Sonnet. **Needs:** T2.3.
- **Why:** The WF-1000XM5 earbuds showed up as three addresses in one day. Without this, one device looks like many.
- **Files:** `host/tallowd/src/irk.rs`, `map.rs`.
- **Deps:** `aes`, `toml`.
- **Do:**
  - Implement `ah(irk, prand)` from Bluetooth Core Vol 3 Part H 2.2.2. Check it against the sample data: IRK `ec0234a357c8ad05341010a60a397d9b`, prand `708194`, hash `0dfbaa`. This vector was verified on 2026-10-31 with AES-128 and against `test_ah` in Linux `net/bluetooth/smp.c`.
  - Byte order: all three are as written, most significant byte first. AES runs on 13 zero bytes followed by prand, and the hash is the last 3 bytes of the output. The map's address text is also most significant first, so `prand` is its first 3 bytes and `hash` its last 3. `irks.toml` holds the IRK in the same order. Add a second test with a whole address, not only the split parts.
  - Read keys from gitignored `data/mesh/irks.toml`, mapping a device name to its IRK. Refuse to read it unless its mode is 0600. Resolve on the host only: IRKs never go on a node.
  - An address is resolvable only if it is `ble-random` and its top two bits are `01`. Try nothing else, or a Classic address starting `40` to `7F` would count.
  - Wire it in: when a device matches, `Map` sets `"resolved": "<device name>"` on that entry. Otherwise this is dead code and the three earbud addresses stay three devices.
- **Done when:** `tools/mesh.sh daemon check` passes, with a test that two addresses from one IRK resolve to one name.
- **Stop if:** the spec vector fails.

**Open for the user:** getting an IRK usually means pairing a phone once, and `.cursor/rules/iot-network-isolation.mdc` keeps phones off the whitelist. Reading it from the phone's own backup avoids pairing. See `references/presence/irk-capture` and `references/presence/ESPresense`. Ask before pairing anything.

## T2.5 One device, many addresses

**Done (2026-10-31).**

- **Tier:** Grok. **Needs:** T2.3.
- **Why:** The Klipsch soundbar is `23:00:00:11:A0:38` on Classic and `DC:00:00:11:A0:38` on BLE.
- **Files:** `host/tallowd/src/identity.rs`, `map.rs`.
- **Deps:** `toml`, which T2.4 adds.
- **Do:** Read `data/mesh/devices.toml`: a device name, a `type` such as `speaker`, and a list of addresses. The map's `kind` already means the transport, so do not reuse the word. Group the map under these, along with any `resolved` name from T2.4. Also add a suggestions list: entries not yet in `devices.toml` that have the same advertised name, or that share the low 4 address bytes. Put it under `suggestions` in `map.json`, print each once to stdout, and never merge them on their own. The fixture test from T2.3 compares `nodes` and `devices` only.
- **Done when:** `tools/mesh.sh daemon check` passes.

## T2.6 Rooms

**Done (2026-10-31).**

- **Tier:** Grok. **Needs:** T2.5.
- **Files:** `host/tallowd/src/rooms.rs`.
- **Do:** Read `data/mesh/rooms.toml`: node id to room name. For each device, keep the last 12 RSSI values per node, each with its time, and drop any older than 60 s. A node only reports what it hears, so without this a device that left keeps its old room forever. The room is the node with the highest median. Change room only when the new one leads by 3 dB on 3 consecutive reports of that device, from any node. Read `references/presence/bermuda` for ideas, but copy no code without checking its licence. Report a room, never a position.
- **Done when:** `tools/mesh.sh daemon check` passes, with tests for a steady device, a boundary device that must not flicker, a real move, and a device that one node stops hearing.

## T2.7 Replace `mesh_host.py`

**Code done (2026-10-31), not run on hardware.** The trial and the service are yours.

- **Tier:** Grok, with You for the service. **Needs:** T2.3.
- **Files:** `host/tallowd/src/trial.rs`, `link.rs`, new `tools/setup_mesh_host.sh`, `tools/tallowd.service`, `tools/mesh.sh`.
- **Do:**
  - Port `--trial` with `--hold`, `--hz`, `--size`, and `--pass-mark`, and port `--set`. `delivery` in `mesh_host.py` is the logic, and its tests port across. Add any frame kind `mesh_host.py` gained since T2.3.
  - Add `tools/mesh.sh daemon run <port> [arguments]`, using `need_classic_port`.
  - Write `tools/setup_mesh_host.sh`, in the style of `tools/install_re_toolchain_fedora.sh`: a udev rule giving this user the root node's port by USB path, and a user service. Do not run it.
  - `serialport` opens exclusively, so while the service runs, `flash`, `monitor`, and `backup` on the root's port fail. Say in the script's output to stop it first. Every restart of the service resets the root board, so use `RestartSec=10`.
- **Done when:** `tools/mesh.sh daemon check` passes. **You:** run the script, and `--trial` matches the Python's numbers within 3 points.
- **Stop if:** the service would need root at runtime.
