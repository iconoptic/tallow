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

- Prefer reading and adapting code under `references/` over vendoring large trees into the app.
- If a clone is missing, run the script; do not assume GitHub contents without fetching.

## Where to look for what

| Need | Location |
| --- | --- |
| bleak / proxy connection patterns | `references/ble-stack/` |
| Existing BLE light frame codecs | `references/protocols-ble/` |
| Tuya / Kasa / MagicHome / Govee LAN | `references/protocols-wifi/` |
| Govee H7020 local BLE | `tools/govee_h7020.py`, `docs/devices/govee-h7020.md`, `references/protocols-ble/ha-govee-led-ble` |
| CloudCutter / OpenBeken | `references/firmware-liberation/` |
| REA CLI/MCP lanes (primary AI RE) | `references/re-tooling/rea/docs/` — especially `android-analysis.md`, `firmware-analysis.md`, `packet-captures.md`, `frida-instrumentation.md`, `adb-device-analysis.md` |
| Fallback JADX / Ghidra MCP | `references/re-tooling/jadx-ai-mcp`, `references/re-tooling/GhidraMCP` |
| Host RE toolchain install | `tools/install_re_toolchain_fedora.sh` |
| Bring up the isolated IoT access point | `tools/setup_iot_ap.sh` |
| Bring up the host Bluetooth seed | `tools/setup_iot_ble.sh` |
| Cursor REA MCP config | `.cursor/mcp.json` |
| Scan nearby BLE lights | `tools/ble_discover.py` |
| Drive the Govee H7020 over BLE | `tools/govee_h7020.py` |
| LAN panel and H7020 page | `app/h7020_panel.py` (menu at `/`, H7020 at `/lights`, settings at `/settings`), `app/library.py`, `app/show.py`; run and limits in `docs/devices/govee-h7020.md` |
| Toby's cams page | `app/cams.py` (shared per-cam proxy, hot-reloaded `data/panel/cams.json` with optional `title`), `app/cams_page.html`, `app/settings_page.html`, `app/menu_page.html`, `app/panel.css`; tests `tools/tests/test_cams.py`, `tools/tests/test_cams_page.py` + `cams_page_harness.js` (node); behaviour in `docs/devices/keyestudio-mb0200-s3-cam.md` |
| H7020 agent socket | `tools/panel_token.py`, `tools/panel_broker.py`, `app/agents.py`; broker rules in `docs/devices/govee-h7020.md` |
| Rust mesh firmware | `firmware/mesh/core` (`no_std` logic, tests in `core/tests/sim.rs`), `firmware/mesh/node` (ESP-IDF firmware); design, results, and limits in `docs/01-bluetooth-server-hardware-and-proxies.md`, boards in `docs/devices/esp32-devkit.md`, shared key in `docs/04-firmware-liberation-and-network-isolation.md` |
| Build, flash, and run the mesh | `tools/mesh.sh` (pinned container; its header is the command source of truth); `tools/mesh_monitor.py` runs through `tools/mesh.sh monitor` |
| Mesh host end and map | `tools/mesh_host.py` (through `tools/mesh.sh host`; writes `data/mesh/map.json` while it runs), tests `tools/tests/test_mesh_host.py` (`.venv/bin/python -m unittest tools/tests/test_mesh_host.py`; pytest is not installed) |
| ESPHome node and cam firmware | `firmware/esphome/` (`toby-cam-1.yaml`, `toby-cam-0.yaml`; `node-1.yaml` is on no board and is the way back to ESPHome on the first board); design in `docs/01-bluetooth-server-hardware-and-proxies.md`, addresses in `docs/04-firmware-liberation-and-network-isolation.md`, cam board in `docs/devices/keyestudio-mb0200-s3-cam.md`, classic board in `docs/devices/esp32-devkit.md` |
| Build and flash ESPHome nodes | `tools/esphome.sh` (pinned container) |
| Ask an ESPHome node what it hears | `tools/node_check.py` |
| ESP32-S3, esp32-camera, MB0200 vendor docs | `references/vendor/` (hand-copied; `VERSIONS.tsv`) |
| Extract ATT writes from Android HCI log | `tools/parse_btsnoop.py` |
| Rust conversion plan | `docs/plans/2026-10-31-rust-conversion.md` (scope, settled decisions, phases); tasks in `docs/plans/rust-conversion/p0-foundation.md` to `p5-retire.md` |
| Rust conversion agent brief | `docs/plans/rust-conversion/agent-brief.md` (rules, tiers, stop conditions; read before any task); guardrails in `.cursor/rules/rust-conversion.mdc` |
| Every offline gate for the Rust and mesh host code | `tools/mesh.sh check` (`--fast` skips the firmware build) |
| Which board is on which port | `tools/mesh.sh ports` (opens no port, resets nothing) |
| Known boards by USB path | `firmware/mesh/boards.tsv` (read by `tools/mesh.sh ports`) |
| Back up a board's whole flash | `tools/mesh.sh backup` (writes gitignored `data/firmware-backups/`; refuses to overwrite) |
| Host daemon | `host/` (Cargo workspace), `host/tallowd` (stub sharing `tallow-mesh-core`), through `tools/mesh.sh daemon check` and `tools/mesh.sh daemon build` |
| `esp-idf-svc`, `esp-idf-hal`, `esp-idf-sys`, and BlueZ from Rust | `references/rust/` (`esp-rs-esp-idf`, `bluer`) |
| Prior art for rooms and rotating addresses | `references/presence/` (`ESPresense`, `bermuda`, `irk-capture`) |
| Reconcile docs with the repo | `.cursor/skills/improve-repo-docs/SKILL.md` |
| Docs ledger for the next agent | `.cursor/docs-ledger.json` (gitignored) |
| Stop hook that requests the docs pass | `.cursor/hooks/docs-before-stop.py` |

## Working conventions

- **Do not commit** APKs, firmware bins, HCI captures, Tuya local keys, `secrets/`, or `firmware/mesh/mesh.key` (see `.gitignore`). Record the key's path only, never its contents.
- **Do not** expand scope into unrelated home-automation domains unless asked.
- When adding a driver later: one module per protocol family, implement the `LightDriver` shape in docs/05, document frames under `docs/devices/<slug>.md` (create that tree when needed).
- Prefer wrapping upstream libraries (tinytuya, led-ble, flux_led, …) over re-copying protocol bytes.
- Close vendor phone apps before BLE connect tests (single-connection devices).
- For whole-house Bluetooth, listening runs on the **Rust mesh** in `firmware/mesh/`, not on one USB dongle ([docs/01](docs/01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh)). Control is not built, and neither is OTA over the mesh. `bleak-esphome` and `habluetooth` remain unwired, and the mesh uses neither.
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

The panel in `app/h7020_panel.py` opens on a menu: Lights at `/lights`, cameras at `/cams`, and Settings at `/settings`. The H7020 page is the single-device slice: catalogue scenes, bulb paints, saved sequences, host schedules that can be turned off on that page, and a host-only agent socket ([docs/devices/govee-h7020.md](docs/devices/govee-h7020.md)). Cameras at `/cams` are proxied through the panel. The menu tile and the cams heading use an optional inventory `title`, and Settings writes that file ([docs/devices/keyestudio-mb0200-s3-cam.md](docs/devices/keyestudio-mb0200-s3-cam.md)). The mesh map is not on the panel: `tools/mesh.sh host` writes `data/mesh/map.json` only while it runs, no service runs it, and it has node ids, not rooms ([docs/01](docs/01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh)). The rest follows docs/05: WebSocket, inventory YAML, driver registry, and host networking for BLE/D-Bus.

### Rust conversion

1. Read [docs/plans/rust-conversion/agent-brief.md](docs/plans/rust-conversion/agent-brief.md).
2. Do one task from its phase file in the [plan](docs/plans/2026-10-31-rust-conversion.md).
3. Stop. Do not start the next task unasked.

## Out of scope unless user asks

- Git commits / PRs (user will request explicitly).
- Flashing live devices without confirmation.
- Exposing the controller to the public internet.
