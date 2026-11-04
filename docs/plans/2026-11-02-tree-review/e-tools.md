# Chunk E: host scripts (`tools/`)

Tests for `tools/mesh.sh` source the script in a temp shell. See `shell()` near the top of `tools/tests/test_mesh_tool.py` and copy its pattern. Variables such as `PAUSE_FILE` and `RESERVATIONS` are set when the script is sourced, so override them after the `source` line. No test may open a port, run `sudo`, or reach the network: stub those as shell functions.

## E1 cam-flash stops when the reservation fails

**Done (2026-11-02).** `ReservationAndTraps` in `tools/tests/test_mesh_tool.py` passed: a refused `sudo` prints `STOPPED` and no address, an accepted reservation prints `10.87.0.21`, and a reservation already on file does not call `sudo`.

- **Tier:** Grok.
- **Files:** `tools/mesh.sh` (`reserve_cam`), `tools/fleet_catalog.py` (`main` only: the new `has-reservation` subcommand, and its line in the usage text), `tools/tests/test_mesh_tool.py`.
- **Why:** `cam-flash` runs `address="$(reserve_cam "$mac")" || exit 1`. Inside the command substitution errexit is off (no `inherit_errexit`), so when `sudo tools/setup_iot_ap.sh reserve` fails (sudo refused, or `die "<ip> is reserved for <mac>"`), `printf` still prints the address and the subshell exits 0. The cam is flashed with no reservation, takes a pool address, `cam-push` refuses it, and `wait_cam_on_panel` waits the full 2 minutes. `cmd_reserve` exits 0 both when it adds a reservation and when the MAC already holds that address, so a non-zero exit is always a real failure.
- **Also:** `tools/fleetd.sh` runs `mesh.sh cam-flash` inside `output="$(...)"`. Re-flashing a known cam with a new image still calls `sudo` for a reservation that is already on file. Without a cached sudo ticket, that blocks the fleetd poll loop at a password prompt.
- **Do:**
  1. In `reserve_cam`, when `python3 tools/fleet_catalog.py cam-address` returned this MAC's existing reservation, print it and return without calling `sudo`. To tell the cases apart, compare against the reservations file through the same Python helper. Add a `fleet_catalog.py has-reservation RESERVATIONS MAC ADDRESS` subcommand that exits 0 only when that exact `mac,address` pair is on file, and use it. Do not parse the file in shell.
  2. Otherwise run `sudo "${ROOT}/tools/setup_iot_ap.sh" reserve "$mac" "$address" >&2 || die "Could not reserve ${address} for ${mac}. Nothing was flashed."`. `die` inside the substitution exits it non-zero, and the caller's `|| exit 1` stops before `espflash`.
- **New tests:**
  - `test_a_refused_reservation_stops_cam_flash_before_writing`: source `mesh.sh`, set `RESERVATIONS` to an empty temp file, define `sudo() { return 1; }`, run `address="$(reserve_cam aa:bb:cc:dd:ee:01)" || echo STOPPED`. Output has `STOPPED` and no `10.87.0.`.
  - `test_a_new_reservation_that_sudo_accepts_is_printed`: empty reservations file, `sudo() { return 0; }`. Prints `10.87.0.21`, and the exit is 0.
  - `test_a_reservation_already_on_file_does_not_call_sudo`: reservations file `aa:bb:cc:dd:ee:01,10.87.0.23`, `sudo() { echo SUDO; return 1; }`. Prints `10.87.0.23`, exit 0, no `SUDO` in the output.
- **Done when:** both tests pass under `.venv/bin/python -m unittest tools/tests/test_mesh_tool.py`, and `tools/mesh.sh check --fast` passes.
- **Stop if:** `cmd_reserve` in `tools/setup_iot_ap.sh` can exit non-zero on a success path. Report it. Do not edit `setup_iot_ap.sh`.

## E2 Chain EXIT traps

**Done (2026-11-02).** `trap -p` cannot round-trip `cam_logged`'s single-quoted `rm`, so EXIT commands are kept in `EXIT_CMDS` and run by `run_exit_cmds`. `test_holding_a_port_keeps_an_earlier_exit_trap` printed `FIRST` and removed the pause file. The existing pause tests passed.

- **Tier:** Grok.
- **Files:** `tools/mesh.sh` (`hold_port`, `cam_logged`, `sign_ota_image`), `tools/tests/test_mesh_tool.py`.
- **Why:** `hold_port` runs `trap 'rm -f "$PAUSE_FILE"' EXIT`, and `cam_logged` and `sign_ota_image` set their own EXIT traps. Each replaces the one before. A command that holds a port and then builds or signs would leave `data/mesh/pause` behind, and the panel would stay paused. The 01:39 commit review flagged this. It is latent today: no command calls `need_classic_port` and then `cam_logged` or `sign_ota_image` in the same shell. This fix keeps the next such command from leaving the panel paused for good.
- **Do:** Add `add_exit_trap() { local new="$1" old; old="$(trap -p EXIT | sed -E "s/^trap -- '(.*)' EXIT$/\1/")"; trap -- "${old:+$old; }$new" EXIT; }`, or an equivalent that keeps the earlier command. Use it in those three places. `sign_ota_image` ends with `trap - EXIT`. Change that to restore the trap it found on entry, not to clear all traps.
- **New test:** `test_holding_a_port_keeps_an_earlier_exit_trap`: in a sourced shell, `trap 'echo FIRST' EXIT`, stub `fuser` to return 1, set `PAUSE_FILE` to a temp path, call `hold_port /dev/null`, exit. Output has `FIRST`, and the pause file is gone.
- **Done when:** the new test and the existing `test_a_port_command_asks_the_panel_to_pause…` and `test_the_pause_file_is_removed_when_the_command_fails` pass. `tools/mesh.sh check --fast` passes.
- **Stop if:** `trap -p` quoting breaks a trap that contains single quotes, such as `cam_logged`'s `rm -f '$log'`. Then use a small array of commands and one dispatcher function instead, and report the choice.

## E3 Small text fixes

**Done (2026-11-02).** `grep -n 'node or cam0' tools/mesh.sh` prints nothing. `grep -n esphome firmware/cam/src/net.rs` prints nothing. The `cam0` product name is still accepted.

- **Tier:** Grok.
- **Files:** `tools/mesh.sh` (`sign_ota_image`, the `ota-sign` and `cam-push` cases), `firmware/cam/src/net.rs` (module doc only).
- **Why:** `ota-sign` accepts `cam` and `cam0`, but its errors still say `node or cam0`, and the `cam-push` hint says `ota-sign … cam0 <version>`. `net.rs` says the SSID and password come from `firmware/esphome/secrets.yaml`. `firmware/cam/build.rs` reads `firmware/cam/secrets.yaml`.
- **Do:** Say `node or cam` in both `ota-sign` messages and `cam` in the `cam-push` hint. In `net.rs`, change the path in the doc comment to `firmware/cam/secrets.yaml`. Change no code.
- **Done when:** `grep -n 'node or cam0' tools/mesh.sh` prints nothing. `grep -n esphome firmware/cam/src/net.rs` prints nothing. `tools/mesh.sh check --fast` passes (it runs the cam `fmt` check).

## E4 Cam sync keeps rotation; fleet status survives a bad line

**Done (2026-11-02).** `test_a_merge_keeps_the_rotation_of_the_row_it_drops` and `test_an_unreadable_journal_line_is_skipped` passed.

- **Tier:** Grok.
- **Files:** `tools/sync_panel_cams.py` (`sync_inventory`), `tools/tests/test_sync_panel_cams.py`, `tools/fleet_status.py` (`_json_lines`), `tools/tests/test_fleet_status.py`.
- **Why:**
  - When one catalog cam matches one row by id and a different row by address, `sync_inventory` keeps the address row and removes the id row. A `rotation` set on the id row is lost. The panel has written `rotation` since the settings change.
  - `_json_lines` calls `json.loads` on every line. One torn line in `data/fleet/applied.jsonl` (fleetd appends while you run `mesh.sh fleet`) crashes `tools/mesh.sh fleet`.
- **Do:**
  1. In the merge branch, if the address row has no `rotation` and the id row has one, copy it over before removing the id row.
  2. In `_json_lines`, skip a line that does not parse, and count it. `main` prints `skipped N unreadable lines in <file>` on stderr when N > 0.
- **New tests:** `test_a_merge_keeps_the_rotation_of_the_row_it_drops` and `test_an_unreadable_journal_line_is_skipped`.
- **Done when:** both test files pass and `tools/mesh.sh check --fast` passes.

## E5 One `load_reservations`

**Done (2026-11-02).** `grep -n 'def load_reservations' tools/*.py` prints one line, in `tools/fleet_catalog.py`. `tools/tests/test_fleet_catalog.py` passed, and the existing sync, fleet-status, and mesh-tool reservation tests passed. `tools/mesh.sh check --fast` passed.

- **Tier:** Grok.
- **Needs:** E4, and E1 (its `has-reservation` must use the one parser).
- **Files:** `tools/fleet_catalog.py`, `tools/sync_panel_cams.py`, `tools/fleet_status.py`, `tools/mesh.sh` (`run_check` list only, if you add a test file), and their tests.
- **Why:** Three copies parse `secrets/iot-ap/reservations`, and they disagree. The authority is `valid_reservations` in `tools/setup_iot_ap.sh`, because dnsmasq gets only what that awk prints. The cam-push allowlist (`fleet_catalog.cam_reserved_addresses`) uses the loosest copy, so a line dnsmasq ignores could still open `cam-push` to an address.

  | Copy | MAC check | Address check | Comments | Missing or unreadable file |
  | --- | --- | --- | --- | --- |
  | `setup_iot_ap.sh` `valid_reservations` (authority) | `^([0-9a-f]{2}:){5}[0-9a-f]{2}$`, any case, lowercased | `10.87.0.N`, N from 20 to 49 | dropped (fail the regex) | prints nothing |
  | `fleet_catalog.py` | `serial_mac` | none | dropped | missing: empty; unreadable: raises |
  | `sync_panel_cams.py` | `MAC` regex | `on_iot_net` (any 10.87.0.x) | skipped | missing: empty; unreadable: raises |
  | `fleet_status.py` | none | none | kept as a row | empty |

- **Do:**
  1. Write one `load_reservations(path) -> dict[str, str]` in `tools/fleet_catalog.py` that keeps a line only when, after `strip()`, it splits on `,` into exactly two fields, field 1 matches the MAC regex above (any case), and field 2 is `10.87.0.N` with N an integer from 20 to 49 and no leading zero. Keys are lowercased. A missing or unreadable file (`OSError`) gives `{}`. Name the two bounds `RESERVED_FIRST = 20` and `RESERVED_LAST = 49`, with a comment pointing at `NODE_FIRST` and `NODE_LAST` in `setup_iot_ap.sh`. Keep `CAM_FIRST = 21` for `cam_address`.
  2. `sync_panel_cams.py` and `fleet_status.py` import it, the way `cam_push_send.py` imports `cam_reserved_addresses`. Delete their copies.
  3. In `fleet_catalog.py main`, the `cam-address` branch prints `cannot read <path>` on stderr and returns 1 when the file exists but `os.access(path, os.R_OK)` is false. Without this, `reserve_cam` would pick `10.87.0.21` for a new cam.
- **New tests** in `tools/tests/test_fleet_status.py` or a new `tools/tests/test_fleet_catalog.py` (add a new file to `run_check`'s unit-test list in `tools/mesh.sh`):
  - `test_a_reservation_outside_twenty_to_forty_nine_is_dropped`: `10.87.0.60` and `10.87.0.19` are dropped, and `10.87.0.20` and `10.87.0.49` are kept.
  - `test_an_uppercase_mac_is_kept_lowercased`.
  - `test_comments_blank_lines_and_three_field_lines_are_dropped`.
  - `test_an_unreadable_file_is_empty`: a directory in place of the file gives `{}`.
  - `test_cam_push_refuses_an_address_dnsmasq_would_ignore`: a catalog cam whose reservation line is `…,10.87.0.60`. `cam_reserved_addresses` does not contain `10.87.0.60`.
- **Done when:** `grep -n 'def load_reservations' tools/*.py` prints one line. Every existing test passes unchanged. `tools/mesh.sh check --fast` passes.
- **Stop if:** an existing test only passes with a looser rule. Name it and change nothing. (On 2026-11-02 the test fixtures' reservation lines were all inside 20–49. `10.87.0.60` and `10.87.0.77` appear only as leases.)
