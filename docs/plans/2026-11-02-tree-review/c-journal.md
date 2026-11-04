# Chunk C: the traffic log (`traffic.rs`, `main.rs`, panel routes)

The map writer, `tallowd run`, is the only thing that keeps `data/mesh/map.json` fresh. The traffic log was added beside it. The log must never be able to stop the map.

## C1 The log never stops the map

**Done (2026-11-02).** `tools/mesh.sh daemon check` passed (tallowd 331 tests, including the three new log tests and the private-file check before the first insert). `tools/mesh.sh check --fast` passed. daemon build skipped: live panel runs from host/target/release. HTTP contract skipped for the same reason.

- **Tier:** Grok.
- **Needs:** B3 (both edit `maybe_prune`).
- **Files:** `host/tallowd/src/main.rs` (`run_command`, `read_for`), `host/tallowd/src/traffic.rs`.
- **Why:**
  - `journal.note_lines(&lines, now)?` and `journal.note_batch(&batch, now)?` return out of `run` on any SQLite error. The page's Clear holds a write lock while it deletes, and the writer's busy timeout is 1 s. A busy lock, a full disk, or a damaged file ends `tallowd run`. The panel waits 30 s and reopens the CP2102, which resets the root board.
  - `Journal::open` and `seed_map` failing also ends `run`, before the port opens, on every restart. A damaged `traffic.db` would stop the map for good.
  - `Journal.last` gains one entry per address, node, and kind, and is never pruned. A street of rotating addresses grows it without bound.
  - `Connection::open` creates the file at the umask mode, and `privatize` sets `0600` only afterwards.
- **Do:**
  1. Add a small type in `traffic.rs`, for example `pub struct Log { journal: Option<Journal>, failures: u32 }`, with `note_lines` and `note_batch` that return nothing. On error: print `traffic: <error>` on stderr for the first failure, and again every 100th. After 10 failures in a row, drop the journal and print `traffic: log stopped; the map continues`. A success resets the count.
  2. `pub fn open_log(db: &Path, map: &Path) -> Log`: open and seed. On error, print `traffic: off: <error>` and return a `Log` with no journal. `run_command` uses it in place of the current `if args.trial.is_none() { … }` block, and still opens no log for `--trial`.
  3. `read_for` and the first read in `run_command` call the `Log` methods with no `?`.
  4. In `maybe_prune`, also drop every `last` entry whose timestamp is older than `REPEAT_MS` before `now`.
  5. In `Journal::open`, before `Connection::open`, create the file if it is missing with `OpenOptions::new().create(true).append(true).mode(0o600)` (`std::os::unix::fs::OpenOptionsExt`). SQLite gives the `-wal` and `-shm` files the database file's mode.
- **New tests in `traffic.rs`:**
  - `a_log_that_cannot_be_opened_is_off_and_does_not_fail`: `open_log` on a path that is a directory returns a `Log` with no journal, and its `note_batch` returns without panic.
  - `ten_failures_in_a_row_stop_the_log`: a `Log` whose journal's file was replaced by a directory, or whose connection was closed. Feed it 10 batches. The journal is gone afterwards.
  - `the_repeat_cache_forgets_old_addresses`: note 1,000 distinct addresses, advance past `PRUNE_EVERY_MS`, note one more batch. `last` holds only recent entries (add a `#[cfg(test)]` accessor for its length).
  - Extend `a_repeat_within_five_seconds…`: a file created by `Journal::open` is `0600` before the first insert.
- **Done when:** `tools/mesh.sh daemon check`, `tools/mesh.sh daemon build`, the HTTP contract, and `tools/mesh.sh check --fast` pass.
- **Stop if:** keeping the "bad path fails before the board resets" behaviour is needed for a test. Report the test name. Do not delete it.

## C2 Clear erases the rows

**Done (2026-11-02).** `a_cleared_log_keeps_no_address_bytes` passed: after Clear, `traffic.db` and the WAL hold neither `DE:AD:BE:EF:00:01` nor `Secret Phone`. `tools/mesh.sh check --fast` passed. The cam hang-up test failed once under load (`Elapsed`) and passed on the rerun. daemon build skipped: live panel runs from host/target/release.

- **Tier:** Grok.
- **Needs:** C1.
- **Files:** `host/tallowd/src/traffic.rs` (`clear`, `Journal::open`).
- **Why:** The page asks "Delete the traffic log on this machine?". `DELETE` leaves the rows in free pages and in the WAL until they are overwritten. The 7-day prune has the same problem.
- **Do:**
  1. In `Journal::open` and in `clear`, run `PRAGMA secure_delete=ON` on the connection.
  2. In `clear`, after the deletes, run `PRAGMA wal_checkpoint(TRUNCATE)`. A busy checkpoint is not an error: the next writer checkpoint carries it.
  3. Do not `VACUUM` while the writer may hold the file.
- **New test:** `a_cleared_log_keeps_no_address_bytes`: write rows with address `DE:AD:BE:EF:00:01` and name `Secret Phone`, drop the journal, `clear`, then read the raw bytes of `traffic.db` and `traffic.db-wal` (if present). Neither holds `DE:AD:BE:EF:00:01` or `Secret Phone`.
- **Done when:** `tools/mesh.sh daemon check` and `tools/mesh.sh check --fast` pass.
- **Stop if:** the bundled SQLite in `rusqlite 0.32` ignores `secure_delete`. Report the version.

## C4 Names follow the seven-day retention

**Done (2026-11-02).** `a_name_goes_when_its_last_sighting_is_pruned` passed. `tools/mesh.sh check --fast` passed. No schema change. daemon build skipped: live panel runs from host/target/release.

- **Tier:** Grok.
- **Needs:** C2 (all three edit `traffic.rs` near `maybe_prune` and `clear`).
- **Files:** `host/tallowd/src/traffic.rs` (`maybe_prune` and its tests only).
- **Why:** `maybe_prune` runs `DELETE FROM sightings WHERE ts < cutoff` and never touches `names`. `names` has no timestamp, so a name a phone advertised once ("Alice's iPhone") stays in `traffic.db` until Clear. The page says the log covers 7 days, and so do the docs. `picture` also reads the whole `names` table on every request, so it grows forever.
- **Do:**
  1. In `maybe_prune`, after the sightings delete, run `DELETE FROM names WHERE NOT EXISTS (SELECT 1 FROM sightings s WHERE s.addr = names.addr AND s.kind = names.kind)`. The `sightings_who` index covers `(addr, kind)`.
  2. Do not add a column or change `SCHEMA`.
  3. Do not touch `clear`, which already deletes `names`.
- **New test in `traffic.rs`:** `a_name_goes_when_its_last_sighting_is_pruned`. Note a batch with one sighting and one name for address X at `t`. Note a batch with only a sighting for address Y at `t + RETAIN_MS + PRUNE_EVERY_MS`, which prunes X's sighting. `SELECT COUNT(*) FROM names WHERE addr = X` is 0. A second name for Y, noted with a sighting in the same batch, is still there.
- **Done when:** `tools/mesh.sh daemon check` and `tools/mesh.sh check --fast` pass.
- **Stop if:** a test needs a name to outlive its sightings. Report the test name.

## C3 Move blocking reads off the runtime

**Done (2026-11-02).** The four blocking reads run in `spawn_blocking`. A join failure answers 500 with `Internal Server Error`. No test assertion changed. `tools/mesh.sh check --fast` passed with the traffic-page work. daemon build skipped: live panel runs from host/target/release. HTTP contract skipped for the same reason.

- **Tier:** Grok.
- **Files:** `host/tallowd/src/panel/routes.rs` (`traffic_route`, `traffic_clear`, `traffic_layout`, `mesh_map_route`).
- **Why:** `traffic::picture` reads up to 100,000 rows and builds the JSON on a tokio worker. `traffic::clear`, `store_layout`, and `mesh_map::document` (which opens `traffic.db` through `cluster::load_path`) also block. While they run, that worker serves nothing else, including cam streams.
- **Do:** Wrap each of those four calls in `tokio::task::spawn_blocking`, moving in owned copies of the path and bounds. A `JoinError` answers 500 with the same body as the current error path. Do not change any status code, body, or header.
- **Done when:** `tools/mesh.sh daemon check`, `tools/mesh.sh daemon build`, the HTTP contract, and `tools/mesh.sh check --fast` pass, with no test changed.
- **Stop if:** `Panel` or a value the call needs is not `Send + 'static` and would need a new `Arc`. Report the type.
