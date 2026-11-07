# Phase 6: the Grok run

Everything left that needs no hardware. The tasks people must do are in [hardware-checklist.md](hardware-checklist.md). After this run, [opus-review-prompt.md](opus-review-prompt.md) goes to the next planner.

Read [agent-brief.md](agent-brief.md) first. It applies, with the changes below.

## Rules for this run

- **One agent, one task, in order.** All tasks share one working tree, so nothing runs in parallel.
- **Run `tools/mesh.sh check --fast` before and after each task**, and `tools/mesh.sh check` after any task that touches firmware or `host/`. Paste the real output. If it fails before you start, stop.
- **Tiers:** the user cleared Grok for every task here, including `unsafe`, crypto, OTA, and `sdkconfig.defaults`. This replaces the brief's rule that Grok stops at those areas, for the files each task names and no others.
- **Never commit, flash, open a serial port, connect to a device, or dial a `10.87.0.x` address.** The one exception is `cam_stream_check.py`, and only the user runs that.
- **Never run** `tools/mesh.sh flash`, `monitor`, `backup`, `host`, `bench`, `daemon run`, `daemon gatt`, `daemon ota`, `ota-key`, `ota-sign`, `tools/esphome.sh run`, or `tools/setup_mesh_host.sh`.
- **Never read or print** `firmware/mesh/mesh.key`, `firmware/mesh/ota.key`, `firmware/cam/secrets.yaml` (the cam build reads it), the archived `firmware/esphome/secrets.yaml`, `secrets/`, `data/panel/`, or `data/mesh/`.
- **Never touch the `toby-cam-1` board.** `toby-cam-0`'s reservation is `10.87.0.21`; the Rust image takes its address by DHCP, and `cam-push` allows only an address reserved for a catalog cam ([device note](../../devices/keyestudio-mb0200-s3-cam.md#rust-image)). Do not dial that address.
- **Mark each task** `**Done (date).**` here, or `**Code done (date), not run on hardware.**` if a hardware check remains. Update its T-task in the phase file the same way.
- **Report blockers.** The brief's stop rules still hold. A stop is a good result.

Start each task with this prompt:

> Read `docs/plans/rust-conversion/agent-brief.md`, then the "Rules for this run" in `docs/plans/rust-conversion/p6-grok-run.md`. Do task G<n> in that file. Do only that task.

## Order

| Task | What | Blocked by |
| --- | --- | --- |
| G1 | Lint the firmware | |
| G2 | Keep the mutation tests | |
| G3 | Close the H7020 codec gaps | |
| G4 | Finish the devices page | |
| G5 | GATT from the host | |
| G6 | OTA transfer in the core | |
| G7 | OTA on the node | |
| G8 | OTA from the host | G6, G7 |
| G9 | Cam network and stream | |
| G10 | Cam OTA | G9 |
| G11 | Docs and status | all the others |

G6 to G10 write the code for T1.9, T4.3, and T4.4 ahead of their hardware. Their phase files list hardware tasks under **Needs**. Those still gate any flash, not this code.

## G1 Lint the firmware

**Done (2026-11-01).** `tools/mesh.sh check` passed. Both crates warn on `clippy::undocumented_unsafe_blocks`. A removed `SAFETY` comment made that check exit 101 at the node clippy step; the comment was restored. No flash, no serial port, no device connection. The gate is in [docs/01](../../01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh).

- **Files:** `tools/mesh.sh`, `firmware/mesh/node/src/main.rs`, `firmware/cam/src/main.rs`, and any firmware file the lints flag.
- **Why:** The node and the cam hold nearly all the `unsafe`. `check` used to lint the core and `host/` only.
- **Do:** Add `#![warn(clippy::undocumented_unsafe_blocks)]` to both crates. Add `cargo clippy --release -- -D warnings` for the node and the cam to the part of `run_check` that `--fast` skips. Fix what it reports. A missing `SAFETY` comment must say why the block is sound, not restate it.
- **Done when:** `tools/mesh.sh check` passes, and an `unsafe` block with its comment removed makes it fail (try it, then revert).
- **Stop if:** clippy does not work for the xtensa target in the pinned image. Report the error and leave `tools/mesh.sh` unchanged.

## G2 Keep the mutation tests

**Done (2026-11-01).** `tools/mesh.sh check --fast` passed before and after. `tools/mesh.sh mutants` exited 0 with every mutant caught. A needle of `not 1 <= level <= 999` printed `STALE brightness bound` and that command exited 1 before any panel started. `cut_inquiry` is not in the core. The window math lives in `firmware/mesh/node/src/bluetooth.rs`, and that crate does not `cargo test` on the host, so it was not mutated. No flash, no serial port, no device connection.

- **Files:** `tools/tests/mutants/panel_mutants.py`, `tools/tests/mutants/core_mutants.py`, `tools/mesh.sh`.
- **Why:** These two scripts proved the tests catch real breakage. They are throwaway copies, with paths from a past session, and code has moved since.
- **Do:** Make them real tools. Each copies what it needs to a `tempfile` directory, applies one mutation at a time, runs the tests there, and exits non-zero if a mutation is missed or its source text no longer matches. Refresh the strings to today's code. Add mutants for the replay check in `fresh`, the deny list, `send_down`, the GATT whitelist in all link states, `cut_inquiry`'s logic where it lives in the core, and `delivery` in `trial.rs`. Add a `--panel-command` option so the panel mutants can run against `tallowd`. Add `tools/mesh.sh mutants`. It is slow, so it stays out of `check`.
- **Done when:** `tools/mesh.sh mutants` exits 0 with every mutant caught, and a mutant with stale source text fails it.
- **Stop if:** a mutant survives and the fix is more than one new test. List it, and do not delete it.

## G3 Close the H7020 codec gaps

**Done (2026-11-01).** `tools/mesh.sh daemon check` passed, and `tools/mesh.sh check` passed. The dump now writes 412 vectors. Two runs wrote identical `vectors.json` and `scenes.json`. Pact 2/1 queries match pact 1/1 byte for byte. Colour-mode bodies and an unknown domain (`{"domain": "153"}`) are in the vectors. Scene lookup casefolds sharp s, so `Cherry bloßoms` finds Cherry blossoms. No new crate, no hand-edited vector.

- **Files:** `tools/dump_h7020_vectors.py`, `host/tallow-govee/tests/vectors.json`, `host/tallow-govee/src/`, `host/tallow-govee/tests/vectors.rs`.
- **Why:** T3.3 and T3.7 left gaps no vector covers. Query frames are built for pact 1/1 only, while the Python builds them per profile. An unknown status domain gives `Reply::Unknown` where the Python gives `{"domain": "<int>"}`. Colour-mode (`AA05`) bodies are not decoded.
- **Do:** Add vectors for pact 2/1 queries, an unknown domain, and every colour-mode body the Python parses, generated by the script as before. Make the Rust pass them. Scene lookup lowercases where the Python uses `casefold`: add a non-ASCII lookup vector and match it.
- **Done when:** `tools/mesh.sh daemon check` passes with every vector checked, and two runs of the script give identical bytes.
- **Stop if:** a vector cannot be matched. Do not special-case it or edit it by hand.

## G4 Finish the devices page

**Done (2026-11-01).** `tools/mesh.sh check --fast` passed. The stale block now says `tools/mesh.sh daemon run <port> --out data/mesh/map.json`. The page renders `identities` and `suggestions` as read-only text. The harness covers a named identity with two addresses, one suggestion, and a hostile name in the identity name, a key, and a suggestion reason. No new route and no POST.

- **Files:** `app/devices_page.html`, `tools/tests/devices_page_harness.js`, `tools/tests/test_devices_page.py`.
- **Do:** The stale block tells the reader to run `tools/mesh.sh host <port>`. Change it to `tools/mesh.sh daemon run <port> --out data/mesh/map.json`. Render `identities`, and `suggestions` as a list marked as suggestions. These are read only, with nothing to accept or merge. Every string uses `textContent`.
- **Done when:** `tools/mesh.sh check --fast` passes. The harness covers a named identity with two addresses, a suggestion, and a hostile name in each new place.
- **Stop if:** it needs a new route or a POST.

## G5 GATT from the host

**Done (2026-11-01).** `tools/mesh.sh daemon check` passed, and `tools/mesh.sh check` passed. Core `encode` writes the bytes `parse` already accepts, and `Reply::decode` reads what `Reply::encode` writes. `tallowd gatt` builds the `G` line from those bytes, refuses a line over 400 characters before sending it, and the 170-byte write is 361 characters. Tests cover each request against `gatt::parse`, an answer, a notification, a closed link, a timeout, and `Status::Refused`. No wire change and no `VERSION` bump. T1.11 is still unmarked.

- **Files:** `host/tallowd/src/link.rs`, `host/tallowd/src/decode.rs`, new `host/tallowd/src/gatt.rs`, `host/tallowd/src/map.rs`, `host/tallowd/src/main.rs`, `tools/mesh.sh`.
- **Why:** T1.10 built the node side. Nothing on the host can send a request or read an answer, so T1.11 cannot start.
- **Do:**
  - Build requests with `tallow_mesh_core::gatt`, using no offsets of its own. If the core has no encoder for a request, add one there with a test.
  - Send them as the node's `G <hex id> <hex request>` line. The node's line buffer is `LINE_MAX`, 400 bytes. Refuse a longer line before it is sent, and test the largest write, 170 bytes.
  - Decode `GattUp`, kind 12: answers, notifications, and closed. `map.rs` must ignore these frames rather than count them as sightings.
  - Add `tallowd gatt --port <p> --target <id> <subcommand>`: `whitelist`, `connect`, `write`, `read`, `subscribe`, `disconnect`. It waits for the answer with a timeout, prints each stage with its time in milliseconds, and exits non-zero on anything but ok. Logic is pure and takes lines and a clock, as `trial.rs` does.
  - Add `tools/mesh.sh daemon gatt <port> ...`, built like `daemon run`.
- **Done when:** `tools/mesh.sh daemon check` passes, with tests for each request's bytes against `gatt::parse`, each answer, a timeout, and a refused address.
- **Stop if:** a request needs a change to the wire format.

## G6 OTA transfer in the core

**Done (2026-11-01).** `tools/mesh.sh check` passed. `VERSION` is `0xA7`. The twelve frames that existed at `0xA6` differ only in byte 0 and the Poly1305 tag. `ota::Receiver` is 1,752 bytes, and with `Mesh`, the sightings table, and the GATT client the total is 10,360 bytes, under 12 KB. Sim tests cover a 400-byte image over two links, a lost chunk named by Status and resent, a bad signature, a bad hash, an oversized image, a chunk before Begin, a Commit with a hole, a chunk off a boundary, and a second Begin. No flash, no serial port, no device connection. The node did not run the receiver until G7.

- **Files:** `firmware/mesh/core/src/ota.rs`, `firmware/mesh/core/src/mesh.rs`, `firmware/mesh/core/src/wire.rs`, `firmware/mesh/core/tests/sim.rs`, `firmware/mesh/core/tests/golden.rs`, `firmware/mesh/core/README.md`.
- **Do:** Everything testable about T1.9, with no flash and no heap.
  - `FrameKind::Ota = 9` goes down with `send_down`. `FrameKind::OtaUp = 13` comes up through `send_up`. Bump `VERSION`, regenerate the goldens, and confirm only byte 0 and the tag changed.
  - Sub-kinds down: Begin (size, sha256, signature), Chunk (offset, up to 170 bytes), Query, Commit. Up: Status (bitmap of missing chunks in one 256-chunk window), Result.
  - `ota::Receiver` is a state machine that yields actions, as `gatt::Client` does: begin a write, write these bytes here, finish. It refuses a chunk before Begin, a size over the slot (`0x1C0000`), an offset past the size, an offset not on a chunk boundary, and a second Begin while one runs unless it restarts cleanly.
  - Commit hashes what was written and calls `ota::verify` with a public key passed in. A bad hash or a bad signature gives a failed Result and no activation. This is the only path to activation.
  - A Chunk whose data was already received is harmless. Only one image at a time.
- **Done when:** `tools/mesh.sh check` passes. Sim tests cover a full transfer over 2 hops, lost chunks recovered through Status, a bad signature, a bad hash, an oversized image, a chunk before Begin, and a Commit with chunks missing. The size test stays within 12 KB.
- **Stop if:** the sim needs the mesh key to sign, or the receiver needs the heap.

## G7 OTA on the node

**Code done (2026-11-01), not run on hardware.** `tools/mesh.sh check` passed. `tools/mesh.sh size` then reported `App/part. size: 1,498,320/1,835,008 bytes, 81.65%`. G8's `O` parser moved the image to 1,498,400 bytes (81.66%). `build.rs` bakes `ota.pub` from `/run/ota.pub`, and `tools/mesh.sh` mounts that file read-only. The node queues `FrameKind::Ota` and writes it on the main loop. `Finish` is the only path through `esp_ota_end` and `esp_ota_set_boot_partition`. A new image is marked valid only after 5 minutes with a parent or as root and both heap figures at least 60 KB. `espflash flash --help` (espflash 4.4.0) has `--bootloader <FILE>` and no flag that erases otadata unless `--erase-parts` or `--erase-data-parts` is passed. `tools/mesh.sh flash` passes `target/xtensa-esp32-espidf/release/bootloader.bin` and neither erase flag. Rollback in that binary: `bootloader_utility_get_selected_boot_partition` grew from `0xde` to `0x126` bytes, and its `.iram_loader.text` stores state `4` (aborted) when the slot is `1` (pending verify) and stores state `1` when the selected slot is `0` (new), then calls `write_otadata`. No flash, no serial port, no device connection. `ed25519-compact`'s `opt_size` feature is on so verify links on Xtensa.

- **Files:** new `firmware/mesh/node/src/ota.rs`, `firmware/mesh/node/src/main.rs`, `firmware/mesh/node/build.rs`, `firmware/mesh/node/sdkconfig.defaults`, `tools/mesh.sh`.
- **Needs:** `firmware/mesh/ota.pub`, which is on disk ([docs/04](../../04-firmware-liberation-and-network-isolation.md#mesh-nodes)). If it is missing, stop: only the user creates it.
- **Do:**
  - `build.rs` bakes in `firmware/mesh/ota.pub`, and the build fails without it. The container must be able to read it: mount that one file read-only.
  - Drive `ota::Receiver` with `esp_ota_begin`, `esp_ota_write`, `esp_ota_end`, and `esp_ota_set_boot_partition`. Writing flash blocks, so it happens on the main loop, never in a radio callback.
  - Set `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y`. A new image calls `esp_ota_mark_app_valid_cancel_rollback` only after **5 minutes** with a parent or as root and a clean heap.
  - **Trap:** `espflash` writes its own bundled bootloader unless told otherwise, and rollback lives in the bootloader. So `tools/mesh.sh flash` must pass the bootloader this build produced, with `--bootloader`, and it must also leave otadata valid, because rollback depends on it. Read `espflash flash --help` in the image, and name in your report exactly how you confirmed the built bootloader has rollback on.
- **Done when:** `tools/mesh.sh check` passes, and `tools/mesh.sh size` still fits the slot. Report the size.
- **Stop if:** `espflash` cannot flash a custom bootloader or leave otadata valid. Every board then needs another plan, so report it.

## G8 OTA from the host

**Code done (2026-11-01), not run on hardware.** `tools/mesh.sh check` passed. `tallowd ota` sends Begin, chunks at least 50 ms apart, a Query per window, resends whatever Status marks missing, then Commit. A lossy fake drops one chunk and the image still reaches `Finish`; a bad signature does not. A throwaway Ed25519 key signed with `openssl pkeyutl -sign -rawin` verifies in `ota::verify`. `ota-sign` was not run against `firmware/mesh/ota.key`. `daemon ota` was not run. The command now reads `data/mesh/map.json`, and `--target` is optional ([docs/01](../../01-bluetooth-server-hardware-and-proxies.md#source-ahead-of-the-boards-frame-version-0xa9)). No flash, no serial port, no device connection. After the `O` parser, `tools/mesh.sh ota-image` wrote `firmware/mesh/node/target/ota.bin` at mode `0600`, 1,498,400 bytes, 81.66% of the slot.

- **Files:** new `host/tallowd/src/ota.rs`, `host/tallowd/src/main.rs`, `host/tallowd/src/link.rs`, `tools/mesh.sh`, and the node's serial handling in `firmware/mesh/node/src/main.rs`.
- **Do:**
  - Add `tools/mesh.sh ota-image`, which writes the app image to `firmware/mesh/node/target/ota.bin` with `espflash save-image`.
  - Add `tools/mesh.sh ota-sign <image>`: it hashes the image, signs the digest with `firmware/mesh/ota.key` through `openssl`, and writes `<image>.sig`. The key is never printed, and never passes through `tallowd`. It stops if the key is missing.
  - Add `tallowd ota --port <p> --target <id> --image <file> --sig <file>`, and `tools/mesh.sh daemon ota`. It sends Begin, then chunks at no more than 20 a second with one in flight per hop, asks for Status, resends what is missing, and sends Commit. It reports progress and exits non-zero on failure.
- **Done when:** `tools/mesh.sh check` passes. A test drives the sender against `ota::Receiver` through a lossy fake, with a good image and a bad signature. A signature from `ota-sign`'s `openssl` steps, using a throwaway key in a temporary directory, verifies in `ota::verify`.
- **Stop if:** signing would need the private key on any path but `ota-sign`.

## G9 Cam network and stream

**Code done (2026-11-01), not run on hardware.** `tools/mesh.sh check --fast` passed, and `tools/mesh.sh check` passed. The image joined `tallow-iot` with WPA2 at static `10.87.0.21/24`; since 2026-11-02 it takes a per-MAC reservation by DHCP ([device note](../../devices/keyestudio-mb0200-s3-cam.md#rust-image)). The station and the listeners on `8080` and `6053` come up before the sensor starts. It serves multipart MJPEG on `8080` and accepts TCP on `6053`. `strings` on the image printed `no default credential`. The build log did not contain a credential. That pass did not flash. On 2026-11-02 `cam-flash` wrote a modem-sleep image. The 2026-11-06 leaf flash is in the [device note](../../devices/keyestudio-mb0200-s3-cam.md#esp-now-leaf). The evening `cam-push` is in the [device note](../../devices/keyestudio-mb0200-s3-cam.md#signed-push). `cam-flash` passes this build's bootloader. Rollback is on. G10 marks a pending image valid after five minutes of Wi-Fi and a frame.

- **Files:** as T4.3 in [p4-cams.md](p4-cams.md), which is the spec. Do all of its **Do** list except the user's flash. Also `tools/mesh.sh`, `.gitignore` if needed.
- **Also:**
  - The Wi-Fi password ends up in the built image, so make `firmware/cam/target` private with the same treatment as `hide_node_target`.
  - `cam-flash` must pass the built bootloader, as in G7, or rollback will not be on.
  - `cam-flash` and `cam-backup` refuse every port except the one `boards.tsv` names `toby-cam-0`. Prove it with the guard's logic and a stub table, without opening a port.
  - `check` now needs `firmware/esphome/secrets.yaml` to build the cam. If it is missing, the build must fail with a message naming the file and the two keys.
  - `archive/tools/tests/cam_stream_check.py` still allows only `10.87.0.23` and refuses `10.87.0.21`, tested without a socket. The image is at `10.87.0.21`, so the checker does not accept it. Do not run it against any address.
- **Done when:** `tools/mesh.sh check` passes. A unit test feeds this firmware's multipart output, built on the host, through `camlib` frame parsing. `strings` on the built image finds no default credential, and no credential appears in the build's output.
- **Stop if:** the panel would need a code change to read the stream.

## G10 Cam OTA

**Code done (2026-11-01), not run on hardware.** `tools/mesh.sh check --fast` passed, and `tools/mesh.sh check` passed. `tools/mesh.sh cam-size` printed `App/part. size: 1,071,264/8,126,464 bytes, 13.18%`. The listener on `3232` checks the signature before `esp_ota_begin`. The cam build mounts `firmware/mesh/core` and `firmware/mesh/ota.pub` only. `mesh.key` is not mounted. Superseded: a cam build now also mounts `firmware/mesh/mesh.key` read-only at `/run/mesh-key` and bakes it. `fmt` does not ([device note](../../devices/keyestudio-mb0200-s3-cam.md#esp-now-leaf)). A later USB `cam-flash` wrote a modem-sleep image. The 2026-11-06 leaf flash is in the [device note](../../devices/keyestudio-mb0200-s3-cam.md#esp-now-leaf). The evening `cam-push` is in the [device note](../../devices/keyestudio-mb0200-s3-cam.md#signed-push). The running image marks itself valid after 5 minutes of Wi-Fi and a captured frame, and only while the slot is pending verify. The crashing-build rollback trial stays yours.

- **Files:** as T4.4 in [p4-cams.md](p4-cams.md). Do its **Do** list except the user's push.
- **Also:** Mount only `firmware/mesh/core` and `firmware/mesh/ota.pub` into the cam build, never all of `firmware/mesh/`. Prove it: a test script lists the container's mounts for the cam build and fails if `mesh.key` is visible. `cam-push` requires an address and allows only `10.87.0.21` ([device note](../../devices/keyestudio-mb0200-s3-cam.md#rust-image)). Superseded on 2026-11-02: it allows any address reserved for a catalog cam. Superseded for the key mount (2026-11-04): the build now mounts `mesh.key` read-only at `/run/mesh-key` and still does not mount the rest of `firmware/mesh/`.
- **Done when:** `tools/mesh.sh check` passes, and `tools/mesh.sh cam-size` fits the slot.
- **Stop if:** the listener would accept an image before checking its signature.

## G11 Docs and status

**Done (2026-11-01).** `tools/mesh.sh check --fast` passed before and after. The status paragraph says G1 through G6 and G11 are done, and G7 through G10 are code done and not run on hardware. `tools/mesh.sh mutants` is in the README, AGENTS.md, and docs/01, and it stays out of `check`. Source frame version is `0xA7`. The boards still run `0xA1`. Nothing in this run was flashed. The docs stop script printed `{}`.

- **Files:** the docs `.cursor/skills/improve-repo-docs/SKILL.md` covers, the Status paragraph in `docs/plans/2026-10-31-rust-conversion.md`, and the stale "not flashed" and frame-version text wherever it now differs.
- **Do:** Follow that skill. Say what is code done and what ran on hardware, and only what ran. Write the ledger last.
- **Done when:** `python3 .cursor/skills/improve-repo-docs/scripts/diff_hash.py` matches `.cursor/docs-ledger.json`, and every link resolves.
