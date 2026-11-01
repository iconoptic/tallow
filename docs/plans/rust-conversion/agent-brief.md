# Agent brief

Read this before any task in the [Rust conversion](../2026-10-31-rust-conversion.md). It applies to every model.

## How to work a task

1. Read the task in its phase file, then only the files it names.
2. Check that everything on its **Needs** line is marked done.
3. Run `tools/mesh.sh check --fast` first. If it fails before you touch anything, stop and report.
4. Make the smallest change that meets **Done when**. Do not tidy nearby code.
5. Run every command under **Done when**. Paste the real output into your report.
6. Mark the task `Done (date)` in its phase file.

Do one task, then stop. Do not start the next one unasked.

## Tiers

| Tier | Who | Use for |
| --- | --- | --- |
| **Grok** | A cheap model | Anything with a clear pattern to follow |
| **Sonnet** | Only when the task says so | `unsafe`, crypto, OTA, GATT, concurrency |
| **You** | The person | Flashing, button presses, moving boards |

A Grok-tier task that needs a change to a Sonnet-tier area stops. The areas are `firmware/mesh/node/src/radio.rs`, any new or changed `unsafe` block, `firmware/mesh/core/src/wire.rs` crypto, and `sdkconfig.defaults`.

## Stop and report

Stop, say what happened, and leave the tree as it is, when:

- The same **Done when** check has failed twice after a real fix.
- A task needs a new dependency it does not name.
- A task seems to need a change to the [settled decisions](../2026-10-31-rust-conversion.md#decisions).
- Anything would weaken a test, remove a test, or lower a pass mark to get a green result.
- You find the plan disagrees with the code. The code is the truth. Report the difference.

Reporting a blocker is a good result. A guess in firmware costs a physical trip to a board.

## Never

- Never commit, push, or open a PR unless the user asks.
- Never flash a board, press on with a failed flash, or run `tools/esphome.sh run`. Hardware steps belong to **You**: give the exact command and what to look for.
- Never touch `toby-cam-1` or point any build at `10.87.0.21`.
- Never print, log, copy, or commit `firmware/mesh/mesh.key`, or anything in `secrets/`.
- Never add an unauthenticated frame kind, a default key, or a way to build without a key.
- Never make a mesh node join Wi-Fi or open a socket. See `.cursor/rules/iot-network-isolation.mdc`.
- Never add `use` of `EspNow::take()`. It turns off modem sleep, which ESP-IDF forbids while Bluetooth runs.
- Never use `esp-idf-svc`'s `EspGap` events for a remote name or a pairing result. They run `from_utf8_unchecked` on bytes off the air, which is undefined behaviour. T1.4 replaces them.
- Never call `PropData::prop()`. It reads RSSI and class of device from the pointer rather than through it, and builds a `&str` from off-air bytes. `read_props` in `bluetooth.rs` is the way.
- Never let a node page a device that is not on a whitelist. `cut_inquiry` in `bluetooth.rs` stops the name requests that would follow an inquiry. Do not remove it.
- Never change system configuration directly. Write a script under `tools/` and tell the user the command, per `.cursor/rules/host-config-scripts.mdc`.

## Rust rules

- `firmware/mesh/core` is `no_std`, uses no heap, and makes no OS calls. Time comes in as a `now_ms` argument. Use `wrapping_sub` for all clock maths: the clock wraps.
- Every parser of bytes off the air or off the wire is bounds-checked and returns `Option`. No indexing without a length check, no `unwrap` on outside data.
- Fixed-size tables only in the core. A full table drops the oldest entry or counts a drop. It never grows.
- `unsafe` is allowed only in `firmware/mesh/node/src/radio.rs`, `bluetooth.rs`, `main.rs`, and files a task names. Each block gets a comment saying why it is sound.
- A delay in firmware is `FreeRtos::delay_ms`. `std::thread::sleep` below one tick busy-waits and trips the watchdog.
- Callbacks on the Wi-Fi or Bluetooth task copy data and return. No blocking, no serial writes.
- A change to bytes on the air bumps `VERSION` in `wire.rs`, updates the golden frames, and updates `core/README.md`. Boards on another version refuse the frames, so flash every board together. The source is `0xA8`, including OTA (`0xA7`) and the product and version in OTA Begin (`0xA8`), and has not been flashed. The boards run `0xA1`.
- New tests are named as sentences, like the ones in `firmware/mesh/core/tests/sim.rs`.

## Commands

`tools/mesh.sh --help` is the source of truth. The ones you will use:

```bash
tools/mesh.sh check --fast      # fmt, clippy, tests for core and host, Python tests, panel contract
tools/mesh.sh check             # the same, plus firmware clippy and the node and cam firmware builds
tools/mesh.sh mutants           # mutation tests; slow; not part of check
tools/mesh.sh fmt               # format all Rust
tools/mesh.sh daemon check      # host/ only
tools/mesh.sh size              # image against the 1.8 MB OTA slot
tools/mesh.sh ports             # what is attached. Opens no port and resets nothing
tools/mesh.sh backup <port>     # save a board's whole flash before replacing it
```

Rust runs in the pinned container. Do not install a toolchain on the host, and do not use the host's `cargo` for firmware.

The first firmware build after a clean takes about 5 minutes. Later ones take under 2.

## Facts that trip people up

- Opening a CP2102 port resets that board, so anything sent straight after opening is lost. Wait 4 s.
- Both CP2102s report serial `0001`. Tell them apart by USB path. `firmware/mesh/boards.tsv` has them.
- `/dev/ttyUSB0` numbers can swap. Run `tools/mesh.sh ports` first.
- Just opening an S3 cam's USB port drops it into download mode, and it falls off the network. `tools/mesh.sh` refuses those ports, and any port `boards.tsv` does not list as `esp32`. Read cams over the network.
- The `secrets/` directory is root-owned, so an agent cannot write there. `tools/mesh.sh key` keeps the mesh key beside the firmware.
- `esp-idf-svc`, `esp-idf-hal`, and `esp-idf-sys` moved to `esp-rs/esp-idf`, cloned at `references/rust/esp-rs-esp-idf`. The exact versions in use are unpacked in `firmware/mesh/.cache/registry/src/`.
- ESP-IDF's own docs and sources are in `firmware/mesh/node/.embuild/espressif/esp-idf/v5.5.5/`. See `docs/en/api-guides/coexist.rst` and `docs/en/api-reference/network/esp_now.rst`.
- Prior art for rooms and rotating addresses is in `references/presence/`.

## Docs

When a task changes behaviour, update its home doc in the same task: `docs/01` for the mesh, `docs/devices/` for boards. Write short, concrete sentences with real commands. Record only what you measured, with the date.

## Report format

```text
Task: T1.1
Result: done | blocked
Changed: file, file
Checks: the commands you ran, and their real output
Not done or unsure: anything the user should know
```
