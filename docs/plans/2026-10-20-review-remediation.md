# Review remediation plan (2026-10-20)

Fixes for the three-model adversarial review (Grok, Fable, Kimi) of the research foundation. Every item below was re-verified against the files and the clones under `references/`. Items where a reviewer was wrong are listed under [Not acting](#not-acting).

**Status (2026-10-20 21:40):** Workstreams **A** and **C** are done and verified: 17 unit tests pass, and a real BLE scan worked with no connections made. Workstream **B** (docs) is still open. The decisions below are unresolved.

## Before you start

- **The tree changed after the reviews.** A later pass (21:20–21:23) made REA the primary RE tool. It added `references/re-tooling/rea`, `.cursor/mcp.json` and the host RE toolchain installer (now `tools/install_re_toolchain_fedora.sh`), and edited README, AGENTS, `.gitignore`, docs 02 and 04, and the clone script. The clone count was **17** after that pass, not 16. The H7020 pin makes it **18**. Re-read each file before editing. Fixes are anchored by section heading, not line number.
- `tools/install_re_toolchain_fedora.sh` and `.cursor/mcp.json` belong to that REA work. Leave them alone except for item A13.
- Do not run `git` commits (AGENTS.md). Do not connect to or flash devices.
- The three workstreams touch **disjoint files**, so they can run in parallel:
  - **A:** `tools/*`
  - **B:** `docs/*`, `README.md`, `AGENTS.md`
  - **C:** `.gitignore`

  B's command examples assume A's new CLI flags. If B runs first, B must use the flag names given here.
- [Decisions](#decisions-defaults-apply-unless-the-user-overrides) lists four choices with defaults. Use the defaults unless the user overrides them.

---

## Workstream A — tools

### `tools/parse_btsnoop.py` (highest priority: it silently returns wrong bytes)

Reproduced with synthetic captures. A 40-byte Write Command split at a 27-byte ACL buffer is reported as **20 bytes**, with exit 0. Datalink 1001 (H1) and 2001 (btmon) captures return **zero writes**, also with exit 0.

- **A1 — ACL/L2CAP reassembly.**
  - Decode the ACL handle word: `conn = hw & 0x0FFF`, `pb = (hw >> 12) & 0b11`.
  - Key the reassembly buffers by `(direction, conn)`.
  - `pb` 0b00 or 0b10 starts a frame. The expected length is `l2cap_len + 4`. If an unfinished buffer exists, emit it with `truncated=True`.
  - `pb` 0b01 appends to the buffer. A continuation with no open buffer is counted as an orphan and skipped.
  - Process the L2CAP frame (CID 0x0004 → ATT) only when it is complete. Emit leftover buffers at EOF as `truncated=True`.
- **A2 — Honour the datalink type.** Read it from header bytes 12–15 (big-endian).
  - **1002 (H4, Android):** a 1-byte packet type precedes the packet, and 0x02 means ACL. Direction is flags bit 0 (0 = sent).
  - **1001 (H1):** there is no type byte. Flags bit 1 is 0 for data and 1 for command/event. Direction is bit 0.
  - **2001 (Linux monitor, `btmon -w`):** the opcode is `flags & 0xFFFF`. Opcode 4 is ACL TX (sent) and 5 is ACL RX (received). The adapter index is `flags >> 16`.
  - **Anything else:** raise `ValueError`. Name the datalink and point to Wireshark or `rea inspect-packet-capture`.
  - Do **not** copy the datalink comments from `references/protocols-ble/magichome2-ble/tools/parse_btsnoop.py`. They mislabel 1001, 1002 and 2001.
- **A3 — Output fields.** Add `direction` (`sent`/`received`), `conn_handle`, `timestamp` and `truncated` to `AttWrite`.
  - Convert the timestamp to Unix seconds: `(ts - 0x00DCDDB30F2F8000) / 1e6`.
  - Also set `truncated=True` when `inc_len < orig_len`. Android's "filtered" snoop mode does this.
  - Add `--direction {sent,received,any}`, default `sent`. In text output, show direction and conn handle.
- **A4 — Cleanup.**
  - Remove the dead `if raw[0:8] != ...: pass` block and the unreachable `inc_len < 0` check.
  - If the magic isn't at offset 0, keep the magic-search fallback but **warn on stderr**.
  - Rewrite the `--json` help text to match the behaviour: JSON when `--json` is set or the output file ends in `.json`.
  - Update the docstring: supported datalinks, and that a bugreport containing only a `btsnooz` section (snoop log not enabled) is not supported.
- **A5 — Tests.** Add `tools/tests/test_parse_btsnoop.py` using stdlib `unittest` only. It should build synthetic captures in a temp dir for:
  - an H4 small write;
  - an H4 40-byte write fragmented at 27 bytes, which must yield 40 bytes;
  - H1 and monitor-2001 small writes;
  - a snaplen-truncated record;
  - an orphan continuation;
  - an unknown datalink, which must raise.

  A working generator is at `/tmp/claude-1000/-home-btgrant-Projects-home-automation/046b4d72-ee6e-4088-8d5f-1b240cea48f0/scratchpad/mk_snoop.py` (session scratch; copy the logic, not the file).

### `tools/ble_discover.py`

- **A6 — The `--address` result is discarded.**
  - Look up the address (case-insensitive) in the **unfiltered** scan results, before the `--lights-only` filter.
  - If the device wasn't seen, synthesise a complete row: all keys present, `rssi: None`, `seen_in_scan: False`.
  - Make sure that row ends up in `payload["devices"]` and in `device_count`.
- **A7 — No more GATT connections to every nearby device.** If `--connect-gatt` is given without `--address` or `--lights-only`, call `parser.error(...)` with a message that names both options.
- **A8 — Fingerprints return candidates, not one guess.** Anchor every pattern at the start of the name, case-insensitive. Mirror the upstream matchers:

  | Family | Name prefixes | Source |
  | --- | --- | --- |
  | elk-bledom | `ELK`, `MELK`, `XROCKER`, `JACKYLED`, `DMRRBA`, `LED-`, `LEDBLE` | `references/protocols-ble/elkbledom/custom_components/elkbledom/manifest.json` |
  | led-ble | `LEDBLE`, `BLE-LED`, `Triones`, `LEDBlue`, `Dream` (prefix, not `^Dream$`), `QHM`, `AP`, `LEDnet` not followed by `WF` | `references/protocols-ble/led-ble/src/led_ble/model_db.py`, `led_ble.py` (`startswith("Dream")`) |
  | lednetwf | `LEDnetWF`, `IOTWF`, `IOTB` | `references/protocols-ble/lednetwf_ble/custom_components/lednetwf_ble/manifest.json` |
  | leddmx / mrstar / govee / magichome2-ble | keep the current patterns, anchored | — |

  - Drop the unanchored `Happy`. It is an app name, not an advertised name.
  - Output `family_candidates: [...]`.
  - Set `family_guess` only when there is exactly one candidate. With two or more, set `ambiguous: true`. `LEDBLE*` is claimed by both elk-bledom and led-ble upstream, so it is genuinely ambiguous.
- **A8b — Resolve ambiguity from GATT.** After a dump, set `family_from_gatt`:
  - write characteristic `fff3`/`ffe1` → elk-bledom;
  - `ffd9`, `ffe9`, `ffd5` or `ffe5` → led-ble;
  - `ff01` under service `ffff` → lednetwf/magichome2.

  Sources: elkbledom README "Write UUID" and `led_ble/const.py` `POSSIBLE_WRITE_CHARACTERISTIC_UUIDS`.
- **A9 — Make `--lights-only` consistent.** `--lights-only` keeps rows that match a service UUID but have no name match. `--lights-only --connect-gatt` currently dumps only rows with `family_guess`. Dump every row that was kept.
- **A10 — CLI polish.**
  - Move the `bleak` import into `run()`/`dump_gatt()`, with `TYPE_CHECKING` for the hints, so `--help` works without bleak.
  - Fix the `--address` help text. It currently says "implies connect if --connect-gatt", but the code always enables connect.
  - Add `--adapter hciN`. Pass it as `bluez={"adapter": ...}` to both `BleakScanner.discover` and `BleakClient` (bleak 3.x API; see `references/ble-stack/bleak/bleak/args/bluez.py`) and record it in `adapter_note`.
  - Note in `--lights-only` help that the `fff0`/`ffe0` UUID hints also match non-light gadgets.
- **A11 — Tests.** Add `tools/tests/test_ble_discover.py` to unit-test `fingerprint_name` and the GATT resolver against names taken from the upstream sources. It must run without bleak installed; A10 makes that possible.

### Other tools

- **A12 — `tools/requirements.txt`:** use `bleak>=3.0,<4`, with a comment "tested: bleak 3.0.2, Python 3.14". The tool depends on 3.x behaviour (`return_adv`, `bluez=` kwarg).
- **A13 — `tools/install_re_toolchain_fedora.sh`:** done. Doctor stdout and stderr already use `mktemp` paths.
- **A14 — `tools/clone_references.sh`** (decision 3):
  - After each clone or skip, record `<dest> <url> <HEAD sha> [ref]` to a tracked `tools/references.lock`. A pinned ref is the fourth field.
  - Add `--check`, which reports any clone whose HEAD differs from the lock, and a missing H7020 `generated_protocol/`.
  - If `dest` exists but has no `.git`, exit with a clear message instead of letting `git clone` fail under `set -e`.

**Acceptance A:**

- `.venv/bin/python -m unittest discover -s tools/tests -v` passes.
- `python3 tools/ble_discover.py --help` works with system Python, which has no bleak.
- `.venv/bin/python tools/ble_discover.py --connect-gatt` exits non-zero with the A7 message.
- `.venv/bin/python tools/parse_btsnoop.py <h4_frag fixture>` prints `len= 40`.

---

## Workstream B — docs

### `docs/01-bluetooth-server-hardware-and-proxies.md`

- **B1 — "Verify adapter capabilities":** replace `hciconfig -a` with `bluetoothctl show` and `btmgmt -i hci0 info`. `hciconfig` is a deprecated BlueZ tool and is not installed on this host.
- **B2 — "Distributed Bluetooth proxies":** keep Pico W, but correct the details. Since **ESPHome 2026.8.0**, Pico W and Pico 2 W can act as bluetooth_proxy, with active connections and 3 slots.
  - The tracker component is `rp2_ble_tracker`, not `esp32_ble_tracker`.
  - Show both YAML variants. Confirm the exact keys at <https://esphome.io/components/bluetooth_proxy/> before writing them.
  - Keep PoE ESP32 ranked first. The Pico W's CYW43439 shares one radio between Wi-Fi and BT, so it has the same contention as a Wi-Fi ESP32.
  - Release notes: <https://esphome.io/blog/2026/08/19/esphome-2026-8/>.
- **B3 — "Docker / container passthrough":** add one line saying that BLE and multicast discovery also need `network_mode: host` (see the doc 05 deployment sketch).

### `docs/02-reverse-engineering-playbook.md`

- **B4 — Phase 0 BLE:** replace `python tools/ble_discover.py --scan-seconds 15 --connect-gatt` with two steps:
  1. `.venv/bin/python tools/ble_discover.py --scan-seconds 15 --lights-only -o captures/scan.json`
  2. `.venv/bin/python tools/ble_discover.py --address <MAC> -o captures/gatt_<slug>.json`
- **B5 — Phase 0 Wi-Fi:** replace the nmap line and port table with these verified roles.

  | Port | Role | Source |
  | --- | --- | --- |
  | TCP 6668 | Tuya local control | tinytuya `core/const.py` |
  | UDP 6666 / 6667 / 7000 | Tuya **broadcasts from the device** (3.1 plaintext / 3.3+ / 3.5 app). nmap will not see these; use `python -m tinytuya scan`, which listens for them. | tinytuya `core/const.py` |
  | TCP 5577 | MagicHome | — |
  | TCP 9999 | Kasa legacy control | python-kasa `discover.py` |
  | UDP 9999 | Kasa legacy discovery | python-kasa `discover.py` |
  | UDP 20002 | Kasa/Tapo newer discovery (`20004` also exists) | python-kasa `discover.py` |
  | UDP 4001 | Govee: the device listens for scans | govee2mqtt `src/lan_api.rs` |
  | UDP 4003 | Govee: the device listens for commands | govee2mqtt `src/lan_api.rs` |
  | UDP 4002 | Govee: bound **on the controller**, never open on the device | govee2mqtt `src/lan_api.rs` |
  | TCP 80 | Tapo (KLAP) / OpenBeken / ESPHome / WLED | — |

  The suggested scan becomes `nmap -sT -p 80,443,5577,6668,9999 <ip>`. Add `sudo nmap -sU -p 4001,4003 <ip>` only for suspected Govee devices.
- **B6 — Phase 3A, pulling the log:**
  - Plain `adb pull /data/misc/bluetooth/...` needs root.
  - The normal route is `adb bugreport captures/bugreport.zip`, then unzip, then `FS/data/misc/bluetooth/logs/btsnoop_hci.log`. The path varies by OEM, and the log may rotate to `btsnoop_hci.log.last`.
  - If the bugreport only has a `btsnooz` section, full logging wasn't on. Enable it, toggle Bluetooth, reproduce the actions and capture again.
  - Warn that the bugreport contains personal data, which is why it goes under `captures/`.
- **B7 — Phase 3B, retitle the btmon section to "btmon (this host's own traffic)".**
  - `btmon` records only the local controller's HCI. Use it while **this host** is the central, for example when replaying candidate frames with bleak.
  - It cannot see a phone↔light link. Over-the-air sniffing needs an nRF52840 running nRF Sniffer for Bluetooth LE plus Wireshark, and encrypted links also need the LTK.
  - Android HCI snoop (3A) remains the preferred capture because it records data before encryption.
  - `btmon -w` writes BTSnoop datalink 2001, which `parse_btsnoop.py` will support after A2.
- **B8 — Phase 3C:** rename "Active MITM" to "Active probing". Nothing in that section is a man-in-the-middle.
- **B9 — Phase 4, Frida:**
  - Replace the hook with one that wraps both `BluetoothGattCharacteristic.setValue([B)` (up to API 32) and `BluetoothGatt.writeCharacteristic(BluetoothGattCharacteristic, [B, int)` (API 33+, which never calls `setValue`). Log the characteristic UUID plus the hex payload.
  - Fix the REA snippet. It ends in `\` followed by a comment line, so the command runs without its required arguments.
  - `instrument-with-frida` takes positional `spawn <target>` or `attach <pid>`, plus `--device-id <serial>`, `--script-file` and `--duration-ms` (**default 1000, max 60000**). Source: `references/re-tooling/rea/src/cli/fridaCommands.ts`.
  - For interactive capture (tapping colours in the app), recommend the MCP session tools instead: `start_frida_session` → `load_frida_script` → `resume_frida_session` → … → `close_frida_session` (`rea/docs/frida-instrumentation.md`).
  - Confirm how an Android spawn target is written with `npx -y rea-agents@6.7.0 instrument-with-frida --help`.
- **B10 — Throughout:**
  - Change every `python tools/...` to `.venv/bin/python tools/...`.
  - In "Pulling the APK", note that `pm path` can list several split APKs and all of them should be pulled.
  - In "Agent-oriented tips", make the wording editor-neutral (see decision 2) rather than "so Cursor can query".

### `docs/03-known-light-protocols-matrix.md`

- **B11 — ELK row:** list the upstream names: `ELK-*` (BLEDOM/BLEDOB/BTC/BULB/LAMPL), `MELK*`, `XROCKER*`, `JACKYLED*`, `DMRRBA*`, `LED-*`, `LEDBLE*`.
  - Mark `LEDBLE*` as **shared with led-ble**: elkbledom's manifest has `LEDBLE*`, and led-ble's model_db has `LEDBLE-DE1254F9`.
  - Disambiguate by write characteristic: ELK uses `fff3`/`ffe1`; led-ble uses `ffd9`/`ffe9`/`ffd5`/`ffe5`/`ff01`.
  - Note that Happy Lighting is claimed by both projects, so the app name alone doesn't identify the family.
- **B12 — led-ble row:** names should read `LEDBLE-<id>`, `Dream*` (prefix), `Triones*`, `LEDBlue-*`, `QHM-*`, `BLE-LED`, `AP`, and `LEDnet*` not followed by WF.
- **B13 — LEDnetWF row:**
  - Names: `LEDnetWF*`, `IOTWF*`, `IOTB*`. IOTBT comes in Telink and "segment" protocol families; see the lednetwf README.
  - Replace the header note. Byte 7 is a command-family byte that **varies**: lednetwf protocol_docs use `0x0b` for effects and `0x0a` for music mode, and magichome2 `surplife_ble.py` wraps with `0x0a`. Don't assume one value.
- **B14 — Tuya, Kasa and Govee LAN rows:** use the port roles from B5. In the Govee row, link `references/protocols-wifi/govee2mqtt/docs/LAN.md` for the multicast/VLAN caveats.
- **B15 — Quick fingerprint table:**
  - The LEDDMX next step currently says "custom / led-hue-cycle notes", which points at nothing in the repo. Change it to "no cloned reference — RE via doc 02".
  - Add an `IOTB*`/`IOTWF*` → lednetwf_ble row.
  - Fix the Govee row wording.

### `docs/04-firmware-liberation-and-network-isolation.md`

- **B16 — Strategy table:**
  - Add Realtek **RTL8720CF** (`WBR*` modules) to the CloudCutter row. The cloudcutter README covers it.
  - ESP8266 → "Serial flash (OTA via tuya-convert only on old, pre-patch firmware)".
- **B17 — Network isolation rewrite** (decision 1). The current rule "drop IoT → RFC1918 except the controller" lets bulbs reach every port on the controller. Broadcast and multicast discovery (Tuya, Govee, Kasa, mDNS) also does not cross a routed VLAN boundary, which breaks doc 05's discovery workers. Replace the rules with:
  - controller → IoT: driver ports only (TCP 6668, 5577, 9999, 80; UDP 4001, 4003, 9999, 20002), plus established/related return traffic;
  - IoT → controller: **only** UDP 4002 (Govee replies) and TCP 1883 if OpenBeken or ESPHome publish to an MQTT broker on the controller;
  - IoT → infrastructure: DHCP, DNS and local NTP only. IoT → WAN and IoT → trusted LAN are dropped;
  - discovery: give the controller a VLAN-tagged subinterface on the IoT network, with host nftables on that interface allowing only the inbound list above. The alternative is static inventory with DHCP reservations, running discovery only during onboarding.

  Update the ASCII diagram to match.
- **B18 — CloudCutter "High-level procedure":** follow `references/firmware-liberation/tuya-cloudcutter/INSTRUCTIONS.md`.
  - Prerequisites: non-virtualised Linux, `iw`, Docker with the user in the `docker` group. **NetworkManager is no longer required.**
  - Use a spare AP-capable Wi-Fi adapter passed as `-w <adapter>` (default `wlan0`). The adapter **disappears from the host** for the whole run, so never use a remote box's only NIC.
  - The command is `sudo ./tuya-cloudcutter.sh -w <adapter> [-p <profile>] -s <SSID> <password>`.
  - Generated device keys land in the cloudcutter clone's `configured-devices/`. Treat that folder as a secret.
  - Module IDs: `WB2L` ≈ BK7231T **except `WB2L_M1`, which is BK7231N** (OpenBeken README). `CB2L` ≈ BK7231N.

### `docs/05-target-webapp-architecture.md`

- **B19 — Registry:** replace the inventory `credentials` field with `credentials_ref`, a key into a gitignored `secrets.yaml` or the environment. Add a 6-line inventory example. This brings it in line with the Security section.
- **B20 — Driver interface:** the `set_brightness` comment says "0-255 or 0-100 — pick one", but the API table already uses 0–255. Fix it to **0–255** (decision 4); drivers convert internally.
- **B21 — Implementation sources:** add Form and Licence columns.

  | Library | Form | Licence |
  | --- | --- | --- |
  | led-ble | pip library | Apache-2.0 |
  | elkbledom | HA custom integration | MIT |
  | lednetwf_ble | HA custom integration | MIT |
  | magichome2-ble | docs + tools | MIT |
  | tinytuya | pip library | MIT |
  | flux_led | pip library | **LGPL-3.0+** |
  | python-kasa | pip library | **GPL-3.0+** |
  | govee2mqtt | Rust app | MIT |
  | OpenBK7231T_App | firmware | **no LICENSE at repo root**; check before copying |

  Add one sentence: importing a library and copying its code have different licence consequences if the app is ever distributed.
- **B22 — LAN engine:** cross-reference the B17 discovery strategy.
- **B23 — API surface:** add one line: every `/api/*` route and `/ws` require auth, and `/ws` validates `Origin`.
- **B24 — Deployment sketch:**
  - Rename the service `smartcrap` → `light-controller`.
  - Bind explicitly, e.g. `command: uvicorn app.main:app --host ${LAN_IP} --port 8080`, because with host networking the default `0.0.0.0` listens on every NIC, including the IoT leg.
  - Add `env_file: secrets.env`.
  - Note that `./data/` is gitignored.

### `README.md`, `AGENTS.md`

- **B25 — README quick start:** change `-o discovery.json` to `-o captures/discovery.json`.
- **B26 — AGENTS "Onboard an unknown BLE light":**
  - Step 1 becomes `.venv/bin/python tools/ble_discover.py --scan-seconds 15 --lights-only -o captures/scan.json`.
  - Step 2 should say to match `family_candidates`/`family_guess`, and to resolve `ambiguous` rows with `--address` and `family_from_gatt`.
- **B27 — AGENTS "Onboard a Tuya Wi-Fi light":**
  - Record device id, IP and local key (in secrets, not inventory) **before** moving the device to the IoT VLAN.
  - Broadcast discovery only keeps working if the controller has an IoT leg (doc 04).

**Acceptance B:**

- `grep -rn 'python tools/' docs AGENTS.md` returns only `.venv/bin/python` forms.
- `grep -rn 'led-hue-cycle\|smartcrap\|hciconfig\|NetworkManager' docs` returns nothing.
- `grep -n '4001' docs/*.md` shows no line calling 4001 "control".
- Every flag used in the docs exists in `--help` after Workstream A.

---

## Workstream C — `.gitignore`

A simulated `git add -A` with the current file stages `bugreport.zip`, `btsnoop_hci.log.last`, `discovery.json`, `data/inventory.yaml` and a root-level `configured-devices/`.

The current `!references/*/` rule also un-ignores **any repo cloned directly at `references/<name>`**, which would then be added as an embedded gitlink. `rea` briefly sat there before it moved to `re-tooling/`.

- **C1 —** Replace the references block with an explicit whitelist of the category directories. Tested: depth-1 stray repos and nested clones are ignored, and the five `.gitkeep` files plus `references/README.md` stay tracked.

  ```gitignore
  /references/*
  !/references/README.md
  !/references/ble-stack/
  !/references/protocols-ble/
  !/references/protocols-wifi/
  !/references/firmware-liberation/
  !/references/re-tooling/
  /references/*/*
  !/references/*/.gitkeep
  ```

- **C2 —** Add `bugreport*.zip`, `btsnoop_hci.log*` (this replaces `btsnoop_hci.log`), `configured-devices/`, `secrets.yaml`, `secrets.env` and `/data/`. Change `discovery_*.json` to `discovery*.json`.
  - Keep the existing `captures/` and `rea-evidence/` entries.
  - A root `inventory.yaml` stays trackable on purpose: after B19 it holds no secrets.
  - Do not ignore `tools/references.lock`; it is meant to be tracked.

**Acceptance C:** in a scratch repo carrying this `.gitignore`, `git add -A --dry-run` over the fixture tree (a stray `references/foo/.git`, `bugreport.zip`, `btsnoop_hci.log.last`, `discovery.json`, `data/x`, `configured-devices/x`, `secrets.yaml`) lists none of them.

---

## Decisions (defaults apply unless the user overrides)

1. **IoT VLAN discovery (B17):** give the controller a tagged leg on the IoT VLAN with a host firewall *(default)*. The alternative is static inventory with DHCP reservations, plus discovery only during onboarding.
2. **MCP for Claude Code:** currently only Cursor gets REA, via `.cursor/mcp.json`, which also hard-codes `/home/btgrant/...`. Options are to also add a root `.mcp.json` with `${HOME}`-relative paths, or to leave Cursor-only *(default: ask the user; until then, make B10's wording editor-neutral)*.
3. **Reference pinning (A14):** record SHAs and offer `--check` *(default)*, rather than enforcing a checkout of the locked SHAs.
4. **Brightness scale (B20):** 0–255 *(default)*.

## Not acting

| Claim | Reviewer | Why not |
| --- | --- | --- |
| Delete "(or Pico W)" — ESPHome has no RP2040 BLE | Fable | Outdated. ESPHome 2026.8.0 added `rp2_ble_tracker` and Pico W proxy support (handled by B2). |
| Remove `LEDBLE` from the ELK row; "no LEDBLE string in elkbledom" | Fable | False. elkbledom's README and `manifest.json` (`LEDBLE*`) claim it. Removing it would misroute devices (handled by B11/A8). |
| "No .gitkeep files exist" | Fable | False. All five category `.gitkeep` files exist. |
| "Govee 4001–4003 and Tuya 6668 check out" | Kimi | False for Govee roles and Tuya 6666/6667 (see B5). |
| Ctrl-C mid-clone leaves a broken dir | Kimi, orchestrator | `git clone` removes its own partial directory on SIGINT. The real edge case (a pre-existing non-git dir) is covered in A14. |
| Continuation fragment emitted as a write (`aabb`) | Grok | Needs contrived bytes that look like an L2CAP header with CID 4. Subsumed by A1 anyway. |
| Unauthenticated FastAPI routes | Grok | Blueprint only; B23 adds a one-line requirement. |
| `*.so` / `**/devices.json` too broad | Fable | No compiled code or app inventory exists yet. Revisit when the app lands. |
| Repo not `git init`'d | all | Out of scope; the user controls git. |
