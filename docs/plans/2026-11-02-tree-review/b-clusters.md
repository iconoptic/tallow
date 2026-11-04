# Chunk B: address joins (`host/tallowd/src/cluster.rs`)

What the user asked for (Cursor session `48f6ca04`): *"Automatically consolidate device addresses that have been listened to for long enough to ensure that they definitely belong to the same device."* The bar is "definitely". A missed join costs a second card. A wrong join puts two people's phones on one card, so a doubtful case must not join.

What the live log showed on 2026-11-02 (16 h, 644 `ble-random` addresses): 486 of them clustered, the largest group 155 addresses over 933 minutes with up to 8 members alive at once, at -108 to -68 dBm. A phone that rotates every 15 minutes makes at most about 64 addresses in 16 hours.

Do B1, B2, and B3 in order. They touch the same file.

## B1 Join only unambiguous chains

**Done (2026-11-02).** `tools/mesh.sh daemon check` passed (tallowd 324 tests, including the six new cluster tests and the existing traffic and mesh-map handoff tests). `tools/mesh.sh check --fast` passed before and after. The handoff fixtures gained one hearing on each side of the handoff. No asserted outcome changed. daemon build skipped: live panel runs from host/target/release.

- **Tier:** Grok.
- **Files:** `host/tallowd/src/cluster.rs`; the test fixtures in `host/tallowd/src/traffic.rs` (`a_long_handoff_is_one_track_under_the_seed_name`, `a_name_from_the_air_beats_the_cute_name_and_a_listed_device_names_the_cluster`) and `host/tallowd/src/panel/mesh_map.rs` (`a_long_handoff_labels_both_addresses_as_one_device`), only as the fixture rule in the README allows.
- **Why:** `join()` collects every qualifying pair as an edge and unites them all, so one address can hand off to two others and chains branch into one big group. Weak readings near the noise floor always agree within 6 dB. `cross_ok` sums run overlaps, and with about one hearing a minute the runs are short, so two addresses heard over the same hour count as barely concurrent.
- **Do:** Replace the union-find with these rules. Keep the existing constants where they still apply and name every new number as a constant with a doc comment.
  1. **Envelope.** Each address has `first` and `last` (its earliest and latest sighting). Drop `runs`, `Run`, `concurrent`, `overlap_len`, `run_gap`, `find`, `unite`, `cross_ok` if nothing uses them afterwards.
  2. **Handoff edge A → B** only when all hold:
     - `A.first < B.first` and `A.last < B.last`.
     - `B.first - A.last` is between `-OVERLAP_MS` (20 s) and `GAP_MS` (2 min).
     - **Signal.** Medians are taken over `HANDOFF_WINDOW_MS = 120_000`: A's last two minutes, B's first two minutes, per node. At least one node heard both sides with at least `MIN_HEARINGS = 2` hearings on each side, and both medians there are at or above `FLOOR_RSSI = -85`. Every node that heard both sides agrees within `MATCH_DB` (6). `exclusive_loud` still blocks.
  3. **Ambiguity.** If A has more than one qualifying successor, A gets no successor. If B has more than one qualifying predecessor, B gets no predecessor. Do not pick the best one.
  4. **Chains.** After step 3 every address has at most one successor and one predecessor. Follow successors from each address that has no predecessor.
  5. **Exclusivity.** Walk each chain in order. If the next address's envelope overlaps any address already in the current group by more than `SPLIT_MS` (2 min), close the group and start a new one at that address. Overlap is `max(0, min(last) - max(first))`.
  6. **Store** a group only when it has at least two addresses and spans at least `SPAN_MS` (30 min) from its earliest `first` to its latest `last`. The seed is still the address with the earliest `first` (B2 changes this).
  7. Update the module doc comment to state these rules in two or three sentences.
- **New tests in `cluster.rs`, named as sentences:**
  - `two_devices_that_rotate_together_join_neither`: two devices on `n1` at -60 and -62, both rotating at the same second, 30+ minutes each side. No group.
  - `an_address_with_two_possible_successors_joins_neither`: A ends, B and C both start within 2 min at a matching level. No group contains A.
  - `a_handoff_below_the_signal_floor_does_not_join`: the `handoff` trail at -92. No group.
  - `a_handoff_with_one_hearing_a_side_does_not_join`: the original four-hearing trail (one hearing each side of the handoff). No group.
  - `a_chain_of_three_rotations_is_one_device`: A, B, C, each about 12 minutes, clean handoffs at -60. One group of three, seed A.
  - `a_crowd_of_rotating_devices_never_groups_concurrent_addresses`: 12 devices on `n1` at -70, -71, … -81, each rotating every 15 minutes with start offsets of `i * 75 s`, hearings every 30 s, 3 hours. For every stored group, no two members overlap by more than `SPLIT_MS`, and no group has more than 13 members.
- **Fixtures:** `handoff()` in `cluster.rs` and the matching four-call trails in `traffic.rs` and `mesh_map.rs` have one hearing on each side of the handoff. Add one more hearing each side, 60 s from the handoff (older at `15 min - 60 s`, newer at `15 min + 70 s`), so they still describe a clean handoff. Do not change any asserted outcome.
- **Done when:** `tools/mesh.sh daemon check` passes with the six new tests and every existing cluster, traffic, and mesh-map test. `tools/mesh.sh check --fast` passes.
- **Stop if:** an existing test can only pass by changing what it asserts, or if `a_chain_of_three_rotations_is_one_device` cannot pass under these rules. Report which rule blocks it.

## B2 Keep a cluster's seed

**Done (2026-11-02).** `tools/mesh.sh daemon check` passed (tallowd 326 tests, including `a_seed_that_ages_out_still_names_the_device` and `an_old_seed_is_given_to_one_group_only`). `tools/mesh.sh check --fast` passed. No schema change: the existing `seed` column holds an address that no longer has sightings. daemon build skipped: live panel runs from host/target/release.

- **Tier:** Grok.
- **Needs:** B1.
- **Files:** `host/tallowd/src/cluster.rs`.
- **Why:** `reconcile` deletes `clusters` and rebuilds it from the sightings. Once the seed address's sightings pass the 7-day prune, the next-earliest address becomes the seed and the device's cute name changes. The traffic plan promised that a later rotation does not rename the device.
- **Do:**
  1. In `reconcile`, read the current table into `addr -> seed` before you compute.
  2. For each new group, collect the old seeds its members carry. If there are any, the group takes the old seed carried by the most members. On a tie, prefer the old seed that is itself a member, then the smallest string. The old seed is kept even when that address has no sightings left.
  3. **An old seed goes to one new group only.** `load()` groups rows by `(seed, kind)`, so two groups with one seed would merge back into one. When two new groups want the same old seed, the group with more members carrying it wins. The other takes its earliest member, as in B1.
  4. A group with no old seed takes its earliest member.
- **New tests:**
  - `a_seed_that_ages_out_still_names_the_device`: chain A→B→C, reconcile, delete A's sightings, reconcile. The group is {B, C} with seed A, and `cute_name` of the seed is unchanged.
  - `an_old_seed_is_given_to_one_group_only`: put rows in `clusters` that give one seed to four addresses, then sightings that form two separate valid chains of two. After reconcile, `load()` returns two groups with different seeds.
- **Done when:** `tools/mesh.sh daemon check` passes. `tools/mesh.sh check --fast` passes.
- **Stop if:** keeping seeds needs a schema change to `clusters`. Report it. Do not add a column in this task.

## B3 Take reconcile off the read loop

**Done (2026-11-02).** The pairwise loop on the three-day crowd took 2.014 s. After the windowed sweep the same test took 1.298 s, under the 3 s limit. `tools/mesh.sh daemon check` passed (tallowd 328 tests, including `a_three_day_crowd_reconciles_in_seconds` and `a_reconciler_on_a_missing_file_logs_and_keeps_going`). `grep -n reconcile host/tallowd/src/traffic.rs` shows the call only in `reconcile_clusters`, not in `maybe_prune`. `tools/mesh.sh check --fast` passed. daemon build skipped: live panel runs from host/target/release.

- **Tier:** Grok. The spec below is exact. Do not go beyond it: no `async`, no channel, no `Arc`, no `Mutex`, and no shared `Connection`. The reconciler thread owns only its `PathBuf` and two `Duration`s, and opens its own `Connection` on each pass.
- **Needs:** B1.
- **Files:** `host/tallowd/src/cluster.rs`, `host/tallowd/src/traffic.rs` (`maybe_prune`), `host/tallowd/src/main.rs` (`run_command`).
- **Why:** `maybe_prune` runs `cluster::reconcile` every 60 s on the thread that reads the serial port. It loads 7 days of rows and compares every pair of addresses. A pause of a few seconds overflows the tty buffer and frames are lost. Pruning old rows can stay in the loop. Joining cannot.
- **Do:**
  1. **Sweep.** In `join`, sort spans by `first`. For each A, look only at spans whose `first` lies in `[A.last - OVERLAP_MS, A.last + GAP_MS]`, found by binary search. No all-pairs loop is left.
  2. **Own thread.** Add `pub fn spawn_reconciler(path: PathBuf, first: Duration, every: Duration) -> std::thread::JoinHandle<()>` in `cluster.rs`. It sleeps `first`, then loops: open its own `Connection` to `path` with `busy_timeout(5 s)`, `ensure`, `reconcile`, `eprintln!` any error with the prefix `traffic: clusters:`, sleep `every`. It never panics on an error, and it ends with the process.
  3. `maybe_prune` stops calling `reconcile`. It still deletes rows older than 7 days.
  4. `run_command` starts the reconciler only when the journal is open (not for `--trial`), with `first` 30 s and `every` 5 min.
  5. Keep `Journal::reconcile_clusters` for tests.
- **New test:** `a_three_day_crowd_reconciles_in_seconds`, not ignored. In a temp file database, 20 devices rotating every 15 minutes for 3 days, one hearing a minute on one node and every third minute on a second node (about 5,800 addresses). Insert untimed. Time only `reconcile`, and assert under 3 s in the debug build. Before your change, run it against the old pairwise loop and paste that time too.
- **Done when:** the new test passes and the old-loop time is in your report. `grep -n reconcile host/tallowd/src/traffic.rs` shows no call in `maybe_prune`. `tools/mesh.sh daemon check`, `tools/mesh.sh daemon build`, and `tools/mesh.sh check --fast` pass.
- **Stop if:** WAL mode does not let the reconciler write while the journal holds its connection (a lock error in the test that a 5 s busy timeout does not clear). Report the error. Also stop if the sweep changes any group that B1's tests expect, or if you find you need to pass the `Journal` or its `Connection` to the thread.
- **Also test:** `a_reconciler_on_a_missing_file_logs_and_keeps_going`: `spawn_reconciler` on a path inside a temp dir that does not exist yet, with `first` and `every` of 10 ms. Sleep 100 ms, create the database with `Journal::open`, sleep 100 ms. The thread is still running (`!handle.is_finished()`), and the `clusters` table exists. Do not join the handle.
