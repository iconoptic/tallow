# Working-tree review, 2026-11-02

A review of the uncommitted tree on `main` after `dd96175`, and the fixes it calls for. The tasks are split into chunk files so that one implementer takes one task.

## What the tree holds

The Cursor sessions since the last commit asked for five things. The tree has all five:

| Asked for | Session | In the tree |
| --- | --- | --- |
| A pretty internal tool that passively logs every BT address, tracks moves between nodes and one-ping pass-bys, with a map and timeline | `45deb5ca`, 11:25 | `host/tallowd/src/traffic.rs`, `/traffic`, `app/traffic_page.html`, vendored vis-timeline |
| Join addresses heard long enough that they *definitely* belong to one device; a cute name seeded from the MAC; place dots by the dB from several listeners, not around the nearest AP | `48f6ca04`, 18:33 | `host/tallowd/src/cluster.rs`, labels in `panel/mesh_map.rs`, `solvePlane` in the page |
| Cam rotation setting in the panel | `18c5f17f` | `rotation` in `cams.rs`, settings and cams pages |
| Prior art and a first GATT read of the Klipsch Flexus Core 100 | `61f449ee`, `82ab527b` | `docs/devices/klipsch-flexus-core-100.md`, `tools/klipsch_ble.py` |
| Every S3 is a cam, DHCP per-MAC reservations, `cam-push` to any reserved cam, `mesh.sh fleet` | earlier on 11-02 | `tools/mesh.sh`, `fleet_catalog.py`, `fleetd.sh`, `fleet_status.py`, `firmware/cam/src/net.rs` |

The user also deleted `.cursor/hooks/docs-before-stop.py`. The docs stop hook had fired about 35 times in a row in session `45deb5ca`.

`tools/mesh.sh check --fast` passed on this tree on 2026-11-02 (daemon: 318 tests; core: 51 + 44 + 10 + 114; cam push: 29; page tests OK). The full `check` and the HTTP contract were not run.

**Second pass, 2026-11-02 evening (Claude Code).** The tree was reviewed again against this plan, and `check --fast` was run again with the same counts. Findings 1–11 and 13 were re-checked in the code and stand. Changes from that pass:

- New finding 14 (task [C4](c-journal.md#c4-names-follow-the-seven-day-retention)) and new finding 15 (a run rule and a [user step](user-actions.md#before-the-run)).
- E2 is latent, not live. No command today holds a port and then calls `cam_logged` or `sign_ota_image` in the same shell. It stays in the order because the next command that does will leave the panel paused.
- E5 is rewritten. The three `load_reservations` copies do differ: one checks the 20–49 range, one checks only the MAC, and one checks nothing. The old E5 would have stopped on its own stop condition. It now adopts `setup_iot_ap.sh`'s `valid_reservations` rule.
- E1 also skips `sudo` when the reservation is already on file. `fleetd` runs `cam-flash` inside `$(...)`, so a sudo prompt there blocks the poll loop.
- B3 is now Grok tier. Its spec is exact enough, and the guardrails are tighter.

## What the review found

Most severe first. Chunk files hold the fix for each one.

| # | Finding | Evidence | Task |
| --- | --- | --- | --- |
| 1 | **Clusters merge different devices.** The live `traffic.db` (16 h, 644 random addresses) has 486 addresses in clusters. The largest is 155 addresses over 933 minutes, with up to 8 members heard at the same moment, at -108 to -68 dBm. That breaks "definitely the same device". | Union-find in `join()` chains every qualifying edge, including branches (A→B and A→C). Weak readings near the noise floor always "agree within 6 dB". `cross_ok` sums run overlaps, and sparse hearings make runs short, so real concurrency is under-counted. | [B1](b-clusters.md#b1-join-only-unambiguous-chains) |
| 2 | **Every Claude Code session in this repo is blocked at stop.** `.claude/settings.json` runs `python3 .cursor/hooks/docs-before-stop.py`. The file is gone, `python3` exits 2, and exit 2 from a Stop hook blocks the stop. `.cursor/hooks.json`, `AGENTS.md`, `README.md`, and `p6-grok-run.md` G11 still name the script. | `python3 /nonexistent.py; echo $?` prints 2. | [A1](a-agent-tooling.md#a1-unwire-the-deleted-stop-hook) |
| 3 | **The three-listener solver steps the wrong way.** `solvePlane` solves `JᵀJ·δ = Jᵀr` with `J` the Jacobian of the residual. The step must be `−Jᵀr`. With noiseless readings from 4 listeners, 3 of 4 test points came back `null` or wrong ((0.5, 0.7) → (0.512, 0.258)). With the sign flipped, all 4 came back exact. A `null` falls back to the two loudest, so the page hid this. | Script run during review against the page's own functions. | [D1](d-traffic-page.md#d1-fix-the-three-listener-solver) |
| 4 | **Cluster work runs inside the serial read loop and grows quadratically.** `maybe_prune` calls `cluster::reconcile` every 60 s on the `tallowd run` thread. It loads every `ble-random` row for 7 days, tries every pair of addresses, and `cross_ok` rescans all spans on each union. At this house's rate, 7 days is about 6,700 addresses and 770k rows. A stall of seconds overflows the tty buffer. | `traffic.rs:229-241`, `cluster.rs:268-354`. | [B3](b-clusters.md#b3-take-reconcile-off-the-read-loop) |
| 5 | **The live map freezes at page load.** `scrub` is set once, to the first payload's `to`, and live refreshes never move it. After 60 s every dot uses old readings, and a device first heard after the page opened never gets a dot. | `traffic_page.html` `render`, `load`, `readingsAt`. | [D2](d-traffic-page.md#d2-let-live-follow-now) |
| 6 | **A traffic log error kills the map writer.** `journal.note_lines(...)?` and `note_batch(...)?` return out of `run`. A busy lock during the page's Clear, a full disk, or a bad file stops `tallowd run`. The panel waits 30 s and reopens the CP2102, which resets the root board. A traffic.db that will not open also stops `run` before the port is opened, every restart. | `main.rs` diff; `mesh_host.rs` backoff. | [C1](c-journal.md#c1-the-log-never-stops-the-map) |
| 7 | **`cam-flash` ignores a failed reservation.** `reserve_cam` runs `sudo setup_iot_ap.sh reserve` inside `$(...)`. Errexit is off in a command substitution, so a refused sudo or an address held by another MAC still prints the address. The cam is flashed and then takes a pool address, `cam-push` refuses it, and `wait_cam_on_panel` times out. | `tools/mesh.sh` `reserve_cam`; no `inherit_errexit`. | [E1](e-tools.md#e1-cam-flash-stops-when-the-reservation-fails) |
| 8 | **A cluster's name changes when its first address ages out.** `reconcile` deletes and rebuilds the table. Once the seed's sightings pass 7 days, the next-earliest address becomes the seed and the cute name changes. The plan promised a stable name. | `cluster.rs` `reconcile`, `join`. | [B2](b-clusters.md#b2-keep-a-clusters-seed) |
| 9 | **Distances on the map ignore the aspect ratio.** Places are stored as 0..1 in both axes on a 1000×640 canvas. `exponent`, `section`, and `solvePlane` measure distance in that square, so 0.1 across is 100 px and 0.1 down is 64 px. | `traffic_page.html`. | [D1](d-traffic-page.md#d1-fix-the-three-listener-solver) |
| 10 | **Blocking SQLite on the async runtime.** `traffic::picture`, `traffic::clear`, and `cluster::load_path` (in `mesh_map::document`) run on a tokio worker. A day window reads up to 100k rows. | `routes.rs` diff. | [C3](c-journal.md#c3-move-blocking-reads-off-the-runtime) |
| 11 | **Clear log does not erase.** `DELETE` leaves rows in free pages and in the WAL. The page tells the user it deleted the log. | `traffic.rs` `clear`. | [C2](c-journal.md#c2-clear-erases-the-rows) |
| 12 | Smaller items: the dedupe cache `last` is never pruned; `traffic.db` is created at the umask mode before `privatize`; `hold_port` replaces earlier EXIT traps (latent: no command chains them today); `ota-sign` still says `node or cam0`; `net.rs` points at `firmware/esphome/secrets.yaml`; Clear reports success on a 500; `sync_panel_cams` drops `rotation` when it merges two rows; a bad line in `applied.jsonl` crashes `fleet_status.py`; three copies of `load_reservations` that disagree on validation; vendored vis-timeline has no version record. | | C1, E2–E5, D2, G1 |
| 13 | Carried from the 01:39 commit review and still open: unbounded adapter channel in `local.rs`; one missed AP record makes the cam reconnect. | `988c424c` transcript. | [F1, F2](f-carried-risks.md) |
| 14 | **Advertised names outlive the 7-day retention.** `maybe_prune` deletes `sightings` older than 7 days but never touches `names`. A name a phone advertised ("Alice's iPhone") stays in `traffic.db` until someone presses Clear. The page and docs say the log keeps 7 days. | `traffic.rs` `maybe_prune`; `names` has no timestamp. | [C4](c-journal.md#c4-names-follow-the-seven-day-retention) |
| 15 | **An implementer's `daemon build` becomes the live map writer.** The panel resolves `current_exe()` once (`mesh_host.rs:147`) and respawns `tallowd run` from that path, as root, after every pause or child failure. On 2026-11-02 that path was `host/target/release/tallowd`, the file `tools/mesh.sh daemon build` rewrites. A build of a half-done task would run on the root board the next time any `mesh.sh` port command pauses the panel. | `ps` on 2026-11-02: panel and child both `host/target/release/tallowd`. | Run rule below; [user step](user-actions.md#before-the-run) |

Checked and not a finding: the cam's accept task bounds its pre-`join` read by `REQUEST_BUDGET` (3 s). The POST routes are covered by the `SameSite=Strict` cookie, like the settings `PUT`. Timeline labels pass through `escapeText`, and the page uses no `innerHTML`. `Cam` equality ignores `rotation`, but the inventory reloads by mtime, so a rotation-only edit is still picked up, and `parse_inventory` refuses any rotation other than 0, 90, 180, or 270. `identify` and `ports --read` pause the panel through `run_identify`'s `hold_port`, inside their command substitution, and the trap clears the pause file when it exits. `reserve_node` returns 1 when the MAC already holds the address, but `cmd_reserve` returns 0 on that path, so E1's premise holds. `net.rs` says modem sleep stays off, and `WIFI_PS_NONE` agrees.

## Rules for this run

- Read [agent-brief.md](../rust-conversion/agent-brief.md) first. Its rules apply, except where this list says otherwise.
- **One agent, one task, in the order below.** All tasks share one working tree, so nothing runs in parallel.
- **Run `tools/mesh.sh check --fast` before and after each task.** Paste the real output. If it fails before you start, stop. After a task that touches `host/`, also run `tools/mesh.sh daemon build` and `.venv/bin/python tools/tests/panel_contract.py`, **but only if** `pgrep -af 'tallowd (panel|run)'` prints no line that contains `host/target/release/tallowd`. If it does, the live panel respawns its map writer from that file (finding 15). Skip the build and the contract, write "daemon build skipped: live panel runs from host/target/release" in your report, and carry on. The user's [before-the-run step](user-actions.md#before-the-run) moves the panel off that path.
- **Tiers.** Each task names its tier. Grok takes Grok tasks. A Sonnet task waits for a Sonnet agent unless the user clears Grok for it.
- **Never commit, flash, open a serial port, connect to a device, or dial a `10.87.0.x` address.**
- **Never stop, restart, or signal the running panel or its `tallowd run` child.** It is live on the root board. The user restarts it ([user actions](user-actions.md)).
- **Never read or print** `firmware/mesh/mesh.key`, `firmware/mesh/ota.key`, `firmware/cam/secrets.yaml`, `secrets/`, `data/panel/`, or `data/mesh/`. Tests build their own databases in a temp directory. Do not copy rows from the real `traffic.db` into a fixture.
- **A test fixture may gain hearings** so that it still describes the same trail under a stricter rule. It may not lose an assertion or change an asserted outcome. Anything else that weakens a test is a stop.
- **Mark each task** `**Done (date).**` in its chunk file, with the commands run and their result.
- **A stop is a good result.** Report what blocked you and leave the tree as it is.

Start each task with this prompt:

> Read `docs/plans/rust-conversion/agent-brief.md`, then "Rules for this run" in `docs/plans/2026-11-02-tree-review/README.md`. Do task <ID> in its chunk file. Do only that task.

## Order

| # | Task | Chunk | Tier | Blocked by |
| --- | --- | --- | --- | --- |
| 1 | A1 Unwire the deleted stop hook | [a-agent-tooling.md](a-agent-tooling.md) | Grok | user choice U2 (default: remove) |
| 2 | B1 Join only unambiguous chains | [b-clusters.md](b-clusters.md) | Grok | |
| 3 | B2 Keep a cluster's seed | [b-clusters.md](b-clusters.md) | Grok | B1 |
| 4 | B3 Take reconcile off the read loop | [b-clusters.md](b-clusters.md) | Grok | B1 |
| 5 | C1 The log never stops the map | [c-journal.md](c-journal.md) | Grok | B3 |
| 6 | C2 Clear erases the rows | [c-journal.md](c-journal.md) | Grok | C1 |
| 6a | C4 Names follow the seven-day retention | [c-journal.md](c-journal.md) | Grok | C2 |
| 7 | C3 Move blocking reads off the runtime | [c-journal.md](c-journal.md) | Grok | |
| 8 | D1 Fix the three-listener solver | [d-traffic-page.md](d-traffic-page.md) | Grok | |
| 9 | D2 Let live follow now | [d-traffic-page.md](d-traffic-page.md) | Grok | D1 |
| 10 | E1 cam-flash stops when the reservation fails | [e-tools.md](e-tools.md) | Grok | |
| 11 | E2 Chain EXIT traps | [e-tools.md](e-tools.md) | Grok | |
| 12 | E3 Small text fixes | [e-tools.md](e-tools.md) | Grok | |
| 13 | E4 Cam sync keeps rotation; fleet status survives a bad line | [e-tools.md](e-tools.md) | Grok | |
| 14 | E5 One `load_reservations` | [e-tools.md](e-tools.md) | Grok | E1, E4 |
| 15 | F1 Bound the adapter channel | [f-carried-risks.md](f-carried-risks.md) | Sonnet (concurrency) | C1 |
| 16 | F2 One missed AP record is not a drop | [f-carried-risks.md](f-carried-risks.md) | Sonnet (firmware `unsafe` nearby) | |
| 17 | D3 A bigger name space (only if the user says yes, U3) | [d-traffic-page.md](d-traffic-page.md) | Grok | B2, user U3 |
| 18 | G1 Docs and status | [g-docs.md](g-docs.md) | Grok | every task above that ran |

The user's own steps are in [user-actions.md](user-actions.md). None of them are agent tasks.

## Not in this plan

- The Klipsch Core 100 next steps (Connect Plus APK, HCI capture). They need the user's phone. Nothing in `tools/klipsch_ble.py` needed fixing.
- Flashing `0xA8` to the mesh boards, or USB `cam-flash` of the current cam source.
- Any change to frame bytes, `wire.rs`, or OTA.
