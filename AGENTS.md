# Agent handoff guide

Instructions for AI coding agents working in this repository.

## Mission

Enable **tallow**, a future **self-hosted universal smart-light controller** (BLE + local Wi-Fi, no vendor cloud). This phase is research, references, discovery tooling, node firmware, and the LAN panel (H7020 page, cams, and settings) — not the full web app yet.

## Read first

1. [README.md](README.md) — map and catalog  
2. [docs/05-target-webapp-architecture.md](docs/05-target-webapp-architecture.md) — destination design  
3. [docs/02-reverse-engineering-playbook.md](docs/02-reverse-engineering-playbook.md) — how to RE a new light  
4. [docs/03-known-light-protocols-matrix.md](docs/03-known-light-protocols-matrix.md) — fingerprint before reinventing  

## `references/` is local-only

- Directory is **gitignored**. Populate with:

  ```bash
  ./tools/clone_references.sh
  ```

  `tools/references.lock` pins each clone's revision, and a fresh clone checks out the locked one. `./tools/clone_references.sh --latest` takes the newest instead, and `--check` reports drift.

- Prefer reading and adapting code under `references/` over vendoring large trees into the app.
- If a clone is missing, run the script; do not assume GitHub contents without fetching.

## Where to look for what

| Need | Location |
| --- | --- |
| bleak / proxy connection patterns | `references/ble-stack/` |
| Existing BLE light frame codecs | `references/protocols-ble/` |
| Tuya / Kasa / MagicHome / Govee LAN | `references/protocols-wifi/` |
| Govee H7020 local BLE | `host/tallow-govee`, `docs/devices/govee-h7020.md`, `references/protocols-ble/ha-govee-led-ble` |
| CloudCutter / OpenBeken | `references/firmware-liberation/` |
| REA CLI/MCP lanes (primary AI RE) | `references/re-tooling/rea/docs/` — especially `android-analysis.md`, `firmware-analysis.md`, `packet-captures.md`, `frida-instrumentation.md`, `adb-device-analysis.md` |
| Fallback JADX / Ghidra MCP | `references/re-tooling/jadx-ai-mcp`, `references/re-tooling/GhidraMCP` |
| Host RE toolchain install | `tools/install_re_toolchain_fedora.sh` |
| Bring up the isolated IoT access point | `tools/setup_iot_ap.sh` |
| Bring up the host Bluetooth seed | `tools/setup_iot_ble.sh` |
| Cursor REA MCP config | `.cursor/mcp.json` |
| Agent config layout | `.cursor/` is the only real tree: `skills/`, `plugins/`, `rules/`, `hooks/`, `hooks.json`, `mcp.json`. Symlinks point other harnesses at it: `.agents/skills`, `.agents/plugins`, `.claude/skills` (all to `../.cursor/...`), `CLAUDE.md` (to `AGENTS.md`), and `.mcp.json` (to `.cursor/mcp.json`). Edit under `.cursor/`, never through a link. Hooks: Cursor reads `.cursor/hooks.json`. Cursor also loads Claude Code hooks by default (Settings → Agents → Third-Party Imports, on by default) from `.claude/settings.local.json` (gitignored), `.claude/settings.json` (a real file, not a link), and `~/.claude/settings.json`, and it runs every matching hook. So in Cursor the stop script can run once per source. Claude Code reads `.claude/settings.json` and runs the same `.cursor/hooks/docs-before-stop.py`. The script detects the harness from stdin (`hook_event_name` means Claude) and blocks once |
| Copied Cursor agent skills (Caveman) | `.cursor/skills/` (22 skills copied from `JuliusBrussee/caveman`; `skills-lock.json` records a `computedHash` for each, and no commit), next to Tallow-only skills such as improve-repo-docs. Refresh: `npx skills add JuliusBrussee/caveman -a cursor --skill '*' --copy -y` (it writes through the `.agents/skills` link; check that the symlink survives) |
| Privacy skills | `.cursor/skills/cursor-privacy-settings` (from `ComeOnOliver/skillshub`) and `.cursor/skills/privacy-by-design` (from `sickn33/agentic-awesome-skills`); both have a `computedHash` and no commit in `skills-lock.json`. Refresh: `npx skills add ComeOnOliver/skillshub -a cursor --copy -y --skill cursor-privacy-settings` and `npx skills add sickn33/agentic-awesome-skills -a cursor --copy -y --skill privacy-by-design` |
| Rust security skills | `.cursor/skills/` holds `zeroize-audit`, `constant-time-analysis` (with `ct_analyzer/` copied in), `differential-review` (with `agents/adversarial-modeler.md` copied in), and `rust-review` from `trailofbits/skills` at `2be6433b39e009cd2b6fd3be1895f6b1faa5af9a`, plus `rust-best-practices` from `morganlinton/BeautifulRust`. Full plugins: `.cursor/plugins/rust-review`, `constant-time-analysis`, `differential-review`, and `insecure-defaults` (no `SKILL.md` on that commit). Cursor entries: `.cursor/skills/rust-review-cursor` and `.cursor/skills/insecure-defaults`. Pin is the `commit` field in `skills-lock.json` for the four Trail of Bits skills. `rust-best-practices` has only a `computedHash`. Refresh: `npx skills add trailofbits/skills -a cursor --copy -y --skill zeroize-audit --skill constant-time-analysis --skill differential-review --skill rust-review` and `npx skills add morganlinton/BeautifulRust -a cursor --copy -y --skill rust-best-practices`, then recopy those four plugin trees, `ct_analyzer/`, and `adversarial-modeler.md` from that commit. |
| Scan nearby BLE lights | `tools/ble_discover.py` |
| LAN panel and H7020 page | `host/tallowd/src/panel/` (`tallowd panel`; menu at `/`, H7020 at `/lights`, settings at `/settings`); pages `app/*.html` and `app/panel.css`; HTTP contract `tools/tests/panel_contract.py` (stdlib; run it as `.venv/bin/python tools/tests/panel_contract.py` after `tools/mesh.sh daemon build`; it starts its own previews); run and limits in `docs/devices/govee-h7020.md` |
| Toby's cams page | `host/tallowd/src/cams.rs` (shared per-cam proxy, hot-reloaded `data/panel/cams.json` with optional `title`), `app/cams_page.html`, `app/settings_page.html`, `app/menu_page.html`, `app/panel.css`; tests `tools/tests/test_cams_page.py` + `cams_page_harness.js` (node); behaviour in `docs/devices/keyestudio-mb0200-s3-cam.md` |
| H7020 agent socket | `tools/panel_token.py` is archived; `host/tallowd/src/panel/access.rs`; broker rules in `docs/devices/govee-h7020.md` |
| Rust mesh firmware | `firmware/mesh/core` (`no_std` logic, including `gatt.rs` (`encode` for the request layouts `parse` accepts, and `Reply::decode`), `ota.rs` (`ota::Receiver`, the signed-image check), and `stats.rs`; tests in `core/tests/sim.rs`, `golden.rs`, and `routing.rs`, frame bytes in [`core/README.md`](firmware/mesh/core/README.md)), `firmware/mesh/node` (ESP-IDF firmware; `gatt.rs` is the GATT client, `ota.rs` runs the receiver on the main loop, not flashed); design, results, and limits in `docs/01-bluetooth-server-hardware-and-proxies.md`, boards in `docs/devices/esp32-devkit.md`, shared key in `docs/04-firmware-liberation-and-network-isolation.md` |
| Build, flash, and run the mesh | `tools/mesh.sh` (pinned container, rootless Podman; its header is the command source of truth); `tools/mesh_monitor.py` runs through `tools/mesh.sh monitor` |
| Rust S3 cam image | `firmware/cam` (`tallow-cam`, ESP-IDF v5.5.5, `espressif/idf-rust:esp32s3_1.95.0.0`, camera driver `src/camera.rs` and `src/bindings.h` on esp32-camera 2.1.7; Wi-Fi, MJPEG, and signed OTA in the source; not flashed); `tools/mesh.sh cam-build`, `cam-size`, `cam-image`, `cam-flash`, `cam-backup`, `cam-frame`, and `cam-push` (`fmt` formats it; `check` runs its fmt check, and without `--fast` its clippy, its build, its stack budget, and the image scan); `cam-flash`, `cam-backup`, and `cam-push` have not been run; layout and size in `docs/devices/keyestudio-mb0200-s3-cam.md` |
| Mesh host end and map | `host/tallowd` `run` (through `tools/mesh.sh daemon run <port>`; `--trial`, `--watch`, and `--set`; writes the map while it runs). Tests in `host/tallowd/src/` |
| Mesh bench | `tools/mesh_bench.py` (through `tools/mesh.sh bench <port> [--deny <id-hex>]`; holds the port, refuses root ids, prints `@` lines); tests `tools/tests/test_mesh_bench.py` |
| Rust LAN panel | `host/tallowd/src/panel/` (`tallowd panel`), and `app/devices_page.html` for `/devices`; identities and suggestions are read-only text, with no new route and no POST; in the contract, `tools/tests/panel_contract.py --rust-only` covers `/devices` and `GET /api/mesh/map`; page harness `tools/tests/devices_page_harness.js`, test `tools/tests/test_devices_page.py` (ok floor 15). Not cut over. Behaviour in `docs/01-bluetooth-server-hardware-and-proxies.md` |
| H7020 codec in Rust | `host/tallow-govee` (`ble` feature for the BlueZ link; `tests/vectors.rs` checks `tests/vectors.json`, 412 vectors, written by `tools/dump_h7020_vectors.py`); pact queries, colour-mode status, and scene lookup in `docs/devices/govee-h7020.md` |
| Agent broker in Rust | `host/tallow-broker` (not deployed). `tools/tallow-panel-broker-rust.service` is written and not installed |
| Rust host setup | `tools/rust-host.Dockerfile` (built by `podman build`) makes `tallow-rust-host:esp32_1.95.0.0`, which the `daemon` commands use (apt network on first build). `tools/tallowd.service` and `tools/setup_mesh_host.sh` are written and not run |
| Mesh signing key | `firmware/mesh/ota.key` and `firmware/mesh/ota.pub` are on disk. Details in `docs/04-firmware-liberation-and-network-isolation.md` |
| Library and map fixtures | `host/tallowd/tests/fixtures/library/` and the map fixture, read by the Rust tests. Their Python counterparts are archived |
| Archived stack | `archive/` holds the retired Python panel, broker, H7020 probe, mesh host, tests, and ESPHome firmware, for historical reference only. Nothing outside it imports from it, and it will be deleted. Cam Wi-Fi settings are in the gitignored `firmware/cam/secrets.yaml` (`wifi_ssid`, `wifi_password`) |
| ESP32-S3, esp32-camera, MB0200 vendor docs | `references/vendor/` (hand-copied; `VERSIONS.tsv`) |
| Extract ATT writes from Android HCI log | `tools/parse_btsnoop.py` |
| Rust conversion plan | `docs/plans/2026-10-31-rust-conversion.md` (scope, settled decisions, phases); tasks in `docs/plans/rust-conversion/p0-foundation.md` to `p5-retire.md` |
| Next work | `docs/plans/rust-conversion/p6-grok-run.md` (tasks that need no hardware), `hardware-checklist.md` (the user's steps), `opus-review-prompt.md` (review after the run) |
| Mutation tests | `tools/mesh.sh mutants` (slow, not part of `check`; core, host, and cam mutants). `cut_inquiry` is not covered. The gate list is in `docs/01-bluetooth-server-hardware-and-proxies.md` |
| Rust conversion agent brief | `docs/plans/rust-conversion/agent-brief.md` (rules, tiers, stop conditions; read before any task); guardrails in `.cursor/rules/rust-conversion.mdc` |
| Every offline gate: core, daemon, host tool, script syntax and H7020 vectors, cam push signature, panel tests, HTTP contract, node and cam fmt, and without `--fast` firmware clippy, the firmware builds, and the stack budgets | `tools/mesh.sh check` (`--fast` skips firmware clippy and the node and cam firmware builds); list in `docs/01-bluetooth-server-hardware-and-proxies.md` |
| Which board is on which port | `tools/mesh.sh ports` (opens no port, resets nothing) |
| Known boards by USB path | `firmware/mesh/boards.tsv` (read by `tools/mesh.sh ports`; `flash`, `monitor`, `backup`, `host`, `bench`, `daemon run`, `daemon gatt`, and `daemon ota` open only a `/dev/ttyUSB*` port listed there with chip `esp32`) |
| Back up a board's whole flash | `tools/mesh.sh backup` (writes gitignored `data/firmware-backups/` at mode `0600` in a `0700` directory, because the flash holds the mesh key; refuses to overwrite) |
| Host daemon | `host/` (Cargo workspace), `host/tallowd` (shares `tallow-mesh-core`; `run`, `panel`, `gatt`, and `ota`; `src/link.rs` is the serial link, `src/gatt.rs` is `tallowd gatt`, `src/ota.rs` is `tallowd ota`), through `tools/mesh.sh daemon check`, `daemon build`, `daemon run`, `daemon gatt`, and `daemon ota`. `daemon gatt` and `daemon ota` are wired like `daemon run` and have not been run. `daemon ota` mounts only the named image and signature |
| `esp-idf-svc`, `esp-idf-hal`, `esp-idf-sys`, and BlueZ from Rust | `references/rust/` (`esp-rs-esp-idf`, `bluer`) |
| Prior art for rooms and rotating addresses | `references/presence/` (`ESPresense`, `bermuda`, `irk-capture`) |
| Reconcile docs with the repo | `.cursor/skills/improve-repo-docs/SKILL.md` |
| Docs ledger for the next agent | `.cursor/docs-ledger.json` (gitignored) |
| Stop hook that requests the docs pass | `.cursor/hooks/docs-before-stop.py` |

## Working conventions

- **Do not commit** APKs, firmware bins, HCI captures, Tuya local keys, `secrets/`, or `firmware/mesh/mesh.key` (see `.gitignore`). Record the key's path only, never its contents. The key is also in flash backups under `data/firmware-backups/` and in `firmware/mesh/node/target/`: do not copy, print, or loosen either.
- **Do not** expand scope into unrelated home-automation domains unless asked.
- When adding a driver later: one module per protocol family, implement the `LightDriver` shape in docs/05, document frames under `docs/devices/<slug>.md` (create that tree when needed).
- Prefer wrapping upstream libraries (tinytuya, led-ble, flux_led, …) over re-copying protocol bytes.
- Close vendor phone apps before BLE connect tests (single-connection devices).
- For whole-house Bluetooth, listening runs on the **Rust mesh** in `firmware/mesh/`, not on one USB dongle ([docs/01](docs/01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh)). Control of a light over the mesh is not built. Signed OTA is in the source and has not been flashed or run. `bleak-esphome` and `habluetooth` remain unwired, and the mesh uses neither. Both boards were flashed with the cross-check build on 2026-10-31. Classic RSSI from before that flash is wrong, and nobody has read the boards back since. The source is at frame version `0xA8` and the boards run `0xA1`. Frames between them are refused, so flash every board together ([docs/01](docs/01-bluetooth-server-hardware-and-proxies.md#source-ahead-of-the-boards-frame-version-0xa8)). Flash only when the user says so. No board was flashed with `0xA8`.
- Before the final summary, reconcile docs when the working tree has moved. Follow `.cursor/rules/docs-before-summary.mdc`. The stop hook `.cursor/hooks/docs-before-stop.py` runs the same check when `.cursor/docs-ledger.json` does not yet match the tree.

## Suggested agent workflows

### Onboard an unknown BLE light

1. `python tools/ble_discover.py --scan-seconds 15 --lights-only -o /tmp/scan.json`
2. Match `family_guess` / UUIDs against docs/03.
3. If known → read matching `references/protocols-ble/*` and sketch a thin wrapper.
4. If unknown → use **REA** (prefer MCP tools when registered; else `npx -y rea-agents@6.7.0 <cmd>`):
   - Pull APK: `resolve-adb-packages` / `pull-adb-package` (or manual `adb`).
   - Static: `search-android-classes` / `inspect-android-method` / `trace-android-references` for GATT builders; Ghidra lane for native `.so`.
   - Capture: HCI snoop → `tools/parse_btsnoop.py` (quick) and/or `inspect-packet-capture` with `btatt.*` fields (full dissection).
   - Encrypted payloads → Frida lane (`instrument-with-frida`) on a rooted test device you own.
5. Document frames under `docs/devices/<slug>.md` when that tree exists.

### Onboard a Tuya Wi-Fi light

1. Confirm ports / tinytuya discovery.
2. Extract local key via TinyTuya wizard (user’s Smart Life account).
3. Allow the device MAC on the isolated IoT SSID (`sudo ./tools/setup_iot_ap.sh allow <mac>`; [docs/04](docs/04-firmware-liberation-and-network-isolation.md)).
4. If BK7231 + profile exists → offer CloudCutter / OpenBeken path (docs/04).

### Finish the web app

The panel, `tallowd panel` in `host/tallowd/src/panel/`, opens on a menu: Lights at `/lights`, cameras at `/cams`, and Settings at `/settings`. The H7020 page is the single-device slice: catalogue scenes, bulb paints, saved sequences, host schedules that can be turned off on that page, and a host-only agent socket ([docs/devices/govee-h7020.md](docs/devices/govee-h7020.md)). Cameras at `/cams` are proxied through the panel. The menu tile and the cams heading use an optional inventory `title`, and Settings writes that file ([docs/devices/keyestudio-mb0200-s3-cam.md](docs/devices/keyestudio-mb0200-s3-cam.md)). The Python panel is retired to `archive/`. Build and run the Rust one with `tools/mesh.sh daemon build` and `tallowd panel`. It passes the HTTP contract against its preview, but it has not run against the real light, and the T3.7 hardware check is the user's. The Rust side adds two routes the Python never had, `/devices` and `GET /api/mesh/map`, and `panel_contract.py` checks them. The devices page renders identities and suggestions as read-only text. Nothing there accepts or merges a suggestion. No new route and no POST ([docs/01](docs/01-bluetooth-server-hardware-and-proxies.md#not-built)). The Rust preview refuses to start without `--password-file`. `tools/mesh.sh daemon run` writes `data/mesh/map.json` only while it runs, and no service runs it yet. `tools/setup_mesh_host.sh` and `tools/tallowd.service` are written and not run. The map has node ids, and rooms when a `rooms.toml` is given ([docs/01](docs/01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh)). The rest follows docs/05: WebSocket, inventory YAML, driver registry, and host networking for BLE/D-Bus.

### Rust conversion

1. Read [docs/plans/rust-conversion/agent-brief.md](docs/plans/rust-conversion/agent-brief.md).
2. Do one task from its phase file in the [plan](docs/plans/2026-10-31-rust-conversion.md).
3. Stop. Do not start the next task unasked.

## Out of scope unless user asks

- Git commits / PRs (user will request explicitly).
- Flashing live devices without confirmation.
- Exposing the controller to the public internet.
