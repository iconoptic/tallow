# Phase 6: the Grok run

Everything left that needs no hardware. The tasks people must do are in [hardware-checklist.md](hardware-checklist.md). After this run, [opus-review-prompt.md](opus-review-prompt.md) goes to the next planner.

Read [agent-brief.md](agent-brief.md) first. It applies, with the changes below.

## Rules for this run

- **One agent, one task, in order.** All tasks share one working tree, so nothing runs in parallel.
- **Run `tools/mesh.sh check --fast` before and after each task**, and `tools/mesh.sh check` after any task that touches firmware or `host/`. Paste the real output. If it fails before you start, stop.
- **Tiers:** the user cleared Grok for every task here, including `unsafe`, crypto, OTA, and `sdkconfig.defaults`. This replaces the brief's rule that Grok stops at those areas, for the files each task names and no others.
- **Never commit, flash, open a serial port, connect to a device, or dial a `10.87.0.x` address.** The one exception is `cam_stream_check.py`, and only the user runs that.
- **Never run** `tools/mesh.sh flash`, `monitor`, `backup`, `host`, `bench`, `daemon run`, `ota-key`, `tools/esphome.sh run`, or `tools/setup_mesh_host.sh`.
- **Never read or print** `firmware/mesh/mesh.key`, `firmware/mesh/ota.key`, `firmware/esphome/secrets.yaml` (a build step may read it), `secrets/`, `data/panel/`, or `data/mesh/`.
- **Never touch `toby-cam-1` or `10.87.0.21`.**
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
| G7 | OTA on the node | `firmware/mesh/ota.pub` (checklist step 1) |
| G8 | OTA from the host | G6, G7 |
| G9 | Cam network and stream | |
| G10 | Cam OTA | G9, `firmware/mesh/ota.pub` |
| G11 | Docs and status | all the others |

G6 to G10 write the code for T1.9, T4.3, and T4.4 ahead of their hardware. Their phase files list hardware tasks under **Needs**. Those still gate any flash, not this code.

## G1 Lint the firmware

- **Files:** `tools/mesh.sh`, `firmware/mesh/node/src/main.rs`, `firmware/cam/src/main.rs`, and any firmware file the lints flag.
- **Why:** `check` runs clippy on the core and `host/`, never on the node or the cam, which hold nearly all the `unsafe`.
- **Do:** Add `#![warn(clippy::undocumented_unsafe_blocks)]` to both crates. Add `cargo clippy --release -- -D warnings` for the node and the cam to the part of `run_check` that `--fast` skips. Fix what it reports. A missing `SAFETY` comment must say why the block is sound, not restate it.
- **Done when:** `tools/mesh.sh check` passes, and an `unsafe` block with its comment removed makes it fail (try it, then revert).
- **Stop if:** clippy does not work for the xtensa target in the pinned image. Report the error and leave `tools/mesh.sh` unchanged.

## G2 Keep the mutation tests

- **Files:** `tools/tests/mutants/panel_mutants.py`, `tools/tests/mutants/core_mutants.py`, `tools/mesh.sh`.
- **Why:** These two scripts proved the tests catch real breakage. They are throwaway copies, with paths from a past session, and code has moved since.
- **Do:** Make them real tools. Each copies what it needs to a `tempfile` directory, applies one mutation at a time, runs the tests there, and exits non-zero if a mutation is missed or its source text no longer matches. Refresh the strings to today's code. Add mutants for the replay check in `fresh`, the deny list, `send_down`, the GATT whitelist in all link states, `cut_inquiry`'s logic where it lives in the core, and `delivery` in `trial.rs`. Add a `--panel-command` option so the panel mutants can run against `tallowd`. Add `tools/mesh.sh mutants`. It is slow, so it stays out of `check`.
- **Done when:** `tools/mesh.sh mutants` exits 0 with every mutant caught, and a mutant with stale source text fails it.
- **Stop if:** a mutant survives and the fix is more than one new test. List it, and do not delete it.

## G3 Close the H7020 codec gaps

- **Files:** `tools/dump_h7020_vectors.py`, `host/tallow-govee/tests/vectors.json`, `host/tallow-govee/src/`, `host/tallow-govee/tests/vectors.rs`.
- **Why:** T3.3 and T3.7 left gaps no vector covers. Query frames are built for pact 1/1 only, while the Python builds them per profile. An unknown status domain gives `Reply::Unknown` where the Python gives `{"domain": "<int>"}`. Colour-mode (`AA05`) bodies are not decoded.
- **Do:** Add vectors for pact 2/1 queries, an unknown domain, and every colour-mode body the Python parses, generated by the script as before. Make the Rust pass them. Scene lookup lowercases where the Python uses `casefold`: add a non-ASCII lookup vector and match it.
- **Done when:** `tools/mesh.sh daemon check` passes with every vector checked, and two runs of the script give identical bytes.
- **Stop if:** a vector cannot be matched. Do not special-case it or edit it by hand.

## G4 Finish the devices page

- **Files:** `app/devices_page.html`, `tools/tests/devices_page_harness.js`, `tools/tests/test_devices_page.py`.
- **Do:** The stale block tells the reader to run `tools/mesh.sh host <port>`. Change it to `tools/mesh.sh daemon run <port> --out data/mesh/map.json`. Render `identities`, and `suggestions` as a list marked as suggestions. These are read only, with nothing to accept or merge. Every string uses `textContent`.
- **Done when:** `tools/mesh.sh check --fast` passes. The harness covers a named identity with two addresses, a suggestion, and a hostile name in each new place.
- **Stop if:** it needs a new route or a POST.

## G5 GATT from the host

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

- **Files:** new `firmware/mesh/node/src/ota.rs`, `firmware/mesh/node/src/main.rs`, `firmware/mesh/node/build.rs`, `firmware/mesh/node/sdkconfig.defaults`, `tools/mesh.sh`.
- **Needs:** `firmware/mesh/ota.pub`. If it is missing, stop: only the user creates it.
- **Do:**
  - `build.rs` bakes in `firmware/mesh/ota.pub`, and the build fails without it. The container must be able to read it: mount that one file read-only.
  - Drive `ota::Receiver` with `esp_ota_begin`, `esp_ota_write`, `esp_ota_end`, and `esp_ota_set_boot_partition`. Writing flash blocks, so it happens on the main loop, never in a radio callback.
  - Set `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y`. A new image calls `esp_ota_mark_app_valid_cancel_rollback` only after **5 minutes** with a parent or as root and a clean heap.
  - **Trap:** `espflash` writes its own bundled bootloader unless told otherwise, and rollback lives in the bootloader. So `tools/mesh.sh flash` must pass the bootloader this build produced, with `--bootloader`, and it must also leave otadata valid, because rollback depends on it. Read `espflash flash --help` in the image, and name in your report exactly how you confirmed the built bootloader has rollback on.
- **Done when:** `tools/mesh.sh check` passes, and `tools/mesh.sh size` still fits the slot. Report the size.
- **Stop if:** `espflash` cannot flash a custom bootloader or leave otadata valid. Every board then needs another plan, so report it.

## G8 OTA from the host

- **Files:** new `host/tallowd/src/ota.rs`, `host/tallowd/src/main.rs`, `host/tallowd/src/link.rs`, `tools/mesh.sh`, and the node's serial handling in `firmware/mesh/node/src/main.rs`.
- **Do:**
  - Add `tools/mesh.sh ota-image`, which writes the app image to `firmware/mesh/node/target/ota.bin` with `espflash save-image`.
  - Add `tools/mesh.sh ota-sign <image>`: it hashes the image, signs the digest with `firmware/mesh/ota.key` through `openssl`, and writes `<image>.sig`. The key is never printed, and never passes through `tallowd`. It stops if the key is missing.
  - Add `tallowd ota --port <p> --target <id> --image <file> --sig <file>`, and `tools/mesh.sh daemon ota`. It sends Begin, then chunks at no more than 20 a second with one in flight per hop, asks for Status, resends what is missing, and sends Commit. It reports progress and exits non-zero on failure.
- **Done when:** `tools/mesh.sh check` passes. A test drives the sender against `ota::Receiver` through a lossy fake, with a good image and a bad signature. A signature from `ota-sign`'s `openssl` steps, using a throwaway key in a temporary directory, verifies in `ota::verify`.
- **Stop if:** signing would need the private key on any path but `ota-sign`.

## G9 Cam network and stream

- **Files:** as T4.3 in [p4-cams.md](p4-cams.md), which is the spec. Do all of its **Do** list except the user's flash. Also `tools/mesh.sh`, `.gitignore` if needed.
- **Also:**
  - The Wi-Fi password ends up in the built image, so make `firmware/cam/target` private with the same treatment as `hide_node_target`.
  - `cam-flash` must pass the built bootloader, as in G7, or rollback will not be on.
  - `cam-flash` and `cam-backup` refuse every port except the one `boards.tsv` names `toby-cam-0`. Prove it with the guard's logic and a stub table, without opening a port.
  - `check` now needs `firmware/esphome/secrets.yaml` to build the cam. If it is missing, the build must fail with a message naming the file and the two keys.
  - `cam_stream_check.py` refuses `10.87.0.21`, tested without a socket. Do not run it against any address.
- **Done when:** `tools/mesh.sh check` passes. A unit test feeds this firmware's multipart output, built on the host, through `camlib` frame parsing. `strings` on the built image finds no default credential, and no credential appears in the build's output.
- **Stop if:** the panel would need a code change to read the stream.

## G10 Cam OTA

- **Files:** as T4.4 in [p4-cams.md](p4-cams.md). Do its **Do** list except the user's push.
- **Also:** Mount only `firmware/mesh/core` and `firmware/mesh/ota.pub` into the cam build, never all of `firmware/mesh/`. Prove it: a test script lists the container's mounts for the cam build and fails if `mesh.key` is visible. `cam-push` requires an address, refuses `10.87.0.21`, and refuses any address that is not `10.87.0.23` until T4.5.
- **Done when:** `tools/mesh.sh check` passes, and `tools/mesh.sh cam-size` fits the slot.
- **Stop if:** the listener would accept an image before checking its signature.

## G11 Docs and status

- **Files:** the docs `.cursor/skills/improve-repo-docs/SKILL.md` covers, the Status paragraph in `docs/plans/2026-10-31-rust-conversion.md`, and the stale "not flashed" and frame-version text wherever it now differs.
- **Do:** Follow that skill. Say what is code done and what ran on hardware, and only what ran. Write the ledger last.
- **Done when:** `.cursor/hooks/docs-before-stop.py` prints `{}`, and every link resolves.
