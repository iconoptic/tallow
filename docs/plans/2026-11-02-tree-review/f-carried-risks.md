# Chunk F: risks carried from the 01:39 commit review

The Cursor session that made commits `f5d1cbb`…`dd96175` ended with four review notes. One was about `hold_port` and is E2. One was about the cam's accept task: its pre-`join` read is bounded by `REQUEST_BUDGET` (3 s), so the main loop waits at most 3 s once, at boot. That one is closed. These two remain.

Both are Sonnet tier. Neither needs a flash to be written. F2 has no effect until a cam is flashed or pushed, which is the user's step.

## F1 Bound the adapter channel

**Done (2026-11-02).** The adapter uses `sync_channel(256)` and `try_send`. `a_full_adapter_channel_drops_the_overflow_without_blocking` passed: 10 batches past a full channel were dropped and the call returned in under a second. daemon build skipped: live panel runs from host/target/release.

- **Tier:** Sonnet (concurrency).
- **Needs:** C1 (both edit `read_for`).
- **Files:** `host/tallowd/src/local.rs`, `host/tallowd/src/main.rs` (`read_for`, only if the receive side must change).
- **Why:** `local::start` sends adapter events over `mpsc::channel()` with no bound. When a serial link is open, `read_for` drains the adapter only after `link.lines()` returns, so a slow serial read lets adapter batches pile up without limit.
- **Do:** Use `mpsc::sync_channel(N)` with a named constant `N` (256 is a fair start), and `try_send` on the adapter side. On `Full`, drop the batch and count it. Every 100th drop, or once a minute, send (or print) one `Event::Note` saying how many batches were dropped. A dropped `Note` is fine. A disconnected receiver ends the adapter thread as it does now.
- **New test in `local.rs`:** a sender that fills the channel past `N` without a receiver draining: it does not block, and the drop count equals the overflow.
- **Done when:** `tools/mesh.sh daemon check`, `tools/mesh.sh daemon build`, and `tools/mesh.sh check --fast` pass.
- **Stop if:** the BlueZ side needs an async channel (`tokio::sync::mpsc`) to avoid blocking a runtime thread. Report the call site.

## F2 One missed AP record is not a drop

**Done (2026-11-02).** `stream::reconnect_now` reconnects at once when the netif is down, and only after three missed AP records when it is up. Host tests `two_missed_records_do_not_reconnect_and_three_do` and `a_down_netif_reconnects_at_once` are in `tools/tests/cam_push`. Node and cam clippy, release builds, image scan, and stack budgets passed. No `unsafe` was added. Nothing was flashed or pushed. `tools/mesh.sh check`'s release rebuild of `host/target/release/tallowd` was skipped: the live panel runs from that file.

- **Tier:** Sonnet (firmware; `unsafe` sits next to the change).
- **Files:** `firmware/cam/src/main.rs` (the main loop's `if !link.up() || !link.heard()`), `firmware/cam/src/net.rs`, and the host-tested cam logic if you put the counter there (see how `tools/tests/cam_push` compiles cam sources on the host).
- **Why:** The main loop calls `link.reconnect()` the first time `esp_wifi_sta_get_ap_info` fails, even while the netif is up. One missed beacon makes the station reconnect, which flaps the stream and the OTA listener.
- **Do:** Reconnect at once when `link.up()` is false. When only `heard()` is false, reconnect after it has been false for `MISSES` loop passes in a row (a named constant, 3 to start), and reset the count when `heard()` is true. Put the counting in a pure function or a small struct with no ESP-IDF calls, so a host test can cover it. Do not add or change any `unsafe` block. Do not touch `sdkconfig.defaults`.
- **New host test:** `two_missed_records_do_not_reconnect_and_three_do`, plus `a_down_netif_reconnects_at_once`.
- **Done when:** `tools/mesh.sh check` (full: it runs cam clippy, the cam build, the cam stack budget, and the image scan) passes. Do not flash or push.
- **Stop if:** the counter cannot be host-tested without moving `unsafe` code. Report it and leave the firmware unchanged.
