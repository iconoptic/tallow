# Agent handoff guide

Instructions for AI coding agents working in this repository.

## Mission

Enable **tallow**, a future **self-hosted universal smart-light controller** (BLE + local Wi-Fi, no vendor cloud). This phase is research, references, discovery tooling, and the H7020 LAN page — not the full web app yet.

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
| H7020 LAN page | `app/h7020_panel.py`, `app/library.py`, `app/show.py`; run and limits in `docs/devices/govee-h7020.md` |
| H7020 agent socket | `tools/panel_token.py`, `app/agents.py`; host-only bearer rules in `docs/devices/govee-h7020.md` |
| Extract ATT writes from Android HCI log | `tools/parse_btsnoop.py` |
| Reconcile docs with the repo | `.cursor/skills/improve-repo-docs/SKILL.md` |
| Docs ledger for the next agent | `.cursor/docs-ledger.json` (gitignored) |
| Stop hook that requests the docs pass | `.cursor/hooks/docs-before-stop.py` |

## Working conventions

- **Do not commit** APKs, firmware bins, HCI captures, Tuya local keys, or `secrets/` (see `.gitignore`).
- **Do not** expand scope into unrelated home-automation domains unless asked.
- When adding a driver later: one module per protocol family, implement the `LightDriver` shape in docs/05, document frames under `docs/devices/<slug>.md` (create that tree when needed).
- Prefer wrapping upstream libraries (tinytuya, led-ble, flux_led, …) over re-copying protocol bytes.
- Close vendor phone apps before BLE connect tests (single-connection devices).
- For whole-house BLE UX, plan **ESPHome bluetooth_proxy + bleak-esphome + habluetooth**, not only one USB dongle ([docs/01](docs/01-bluetooth-server-hardware-and-proxies.md)).
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

The H7020 page in `app/h7020_panel.py` is the single-device slice: catalogue scenes, bulb paints, saved sequences, host schedules, and a host-only agent socket ([docs/devices/govee-h7020.md](docs/devices/govee-h7020.md)). The rest follows docs/05: WebSocket, inventory YAML, driver registry, and host networking for BLE/D-Bus.

## Out of scope unless user asks

- Git commits / PRs (user will request explicitly).
- Flashing live devices without confirmation.
- Exposing the controller to the public internet.
