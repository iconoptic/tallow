# Phase 3: panel, H7020, and broker

Port the running panel with no visible change. The method: first pin the Python's behaviour with tests over HTTP, then make Rust pass the same tests.

**Keep:** `app/*.html` and `app/panel.css`, served as they are. The data files under `data/panel/` and `secrets/panel/`. The port, `8765`. The cookie `tallow_panel` and its HMAC, so a logged-in browser stays logged in.

## T3.1 HTTP contract tests against the Python

- **Tier:** Grok. **Needs:** nothing.
- **Files:** new `tools/tests/panel_contract.py`.
- **Do:** A standalone script taking `--base-url` and `--password-file`. It uses only the standard library. It runs against `.venv/bin/python app/h7020_panel.py --preview --port 18765 --password-file <file>`, which binds `127.0.0.1`, skips BLE, and uses `data/panel-preview/`. Write a throwaway password to a file in `/tmp` with mode 0600. Do not read `secrets/panel/`. Cover every route below: status, content type, JSON shape, the 401 without a cookie, redirects to `/login`, and bad input.
- **Done when:** it passes against `--preview` and fails when you edit one expected field.
- **Stop if:** a route's behaviour depends on a real light. Mark that case `needs-light` and skip it.

```text
GET  /  /lights  /cams  /settings  /panel.css  /login
POST /login  /logout
GET  /api/cams  /api/cams/status  /cams/{id}/stream
GET  PUT  /api/settings/cameras
GET  /api/state  /api/scenes  /api/capabilities  /api/manifest
POST /api/power  /api/brightness  /api/color  /api/scene  /api/paint
GET  POST  /api/patterns
GET  PUT  DELETE  /api/patterns/{id}
POST /api/patterns/{id}/apply
GET  POST  /api/schedules
GET  PUT  DELETE  /api/schedules/{id}
```

The agent socket's 404 list is in [docs/devices/govee-h7020.md](../../devices/govee-h7020.md#agent-socket). Cover it with `--unix-socket`.

## T3.2 Golden H7020 packets

- **Tier:** Grok. **Needs:** nothing.
- **Files:** new `tools/dump_h7020_vectors.py`, new `host/tallow-govee/tests/vectors.json`.
- **Do:** Use the Python codec through `tools/govee_h7020.py` to dump every packet the panel can send: all 51 catalogue scenes at default speed, power, brightness steps, colours, paints, and status parsing with known replies. Each entry is its input and its hex bytes.
- **Done when:** the file is deterministic: two runs give identical bytes.
- **Stop if:** anything in it depends on the time or randomness. Report it. The driver must handle it.

## T3.3 H7020 codec crate

- **Tier:** Sonnet. **Needs:** T3.2.
- **Files:** new `host/tallow-govee/`.
- **Do:** A pure crate with no I/O: inputs in, packet bytes out, and reply bytes in, status out. It does not connect or hold state. The `LightDriver` shape in [docs/05](../../05-target-webapp-architecture.md), which does both, goes in `src/ble.rs` in T3.7. Read the pinned Python in `references/protocols-ble/ha-govee-led-ble`. Add the scene table as data. Do not hand-copy 51 scenes: generate a checked-in Rust or JSON table from T3.2.
- **Done when:** the crate compiles with no Bluetooth dependency, and a test proves every vector in `vectors.json` byte for byte. `tools/mesh.sh daemon check` passes.
- **Stop if:** a vector cannot be matched. Do not special-case it.

## T3.4 Library and schedules

- **Tier:** Grok. **Needs:** nothing.
- **Files:** new `host/tallowd/src/library.rs`, `show.rs`.
- **Do:** Port `app/library.py` and `app/show.py`. Read and write the same JSON files. Use `tools/tests/test_h7020_library.py` as the list of cases, and port each test by name.
- **Done when:** `tools/mesh.sh daemon check` passes, with a test that loads a file written by the Python and writes one the Python reads.

## T3.5 Cam proxy

- **Tier:** Sonnet. **Needs:** nothing.
- **Files:** new `host/tallowd/src/cams.rs`.
- **Deps:** `tokio`, `hyper` or `reqwest` with streaming.
- **Do:** Port `Hub` and `Hubs` from `app/cams.py`. Its gentleness is the point, because a cam serves one connection and wedges easily. Keep each of these exactly: `LINGER_S` 10, `COOLDOWN_S` 3, 6, 12, 24, 30, connect timeout 5 s, header timeout 10 s, `VIEWERS_MAX` 8, per-viewer drop-oldest queues, and a cancelled dial closing its socket. Status asks port `6053` and never `8080`. See [One connection per cam](../../devices/keyestudio-mb0200-s3-cam.md#one-connection-per-cam).
- **Done when:** every case in `tools/tests/test_cams.py` has a matching test against a fake cam server.
- **Stop if:** a test needs a real cam. Never burst-test a real cam: that is what jammed it.

## T3.6 Panel routes

- **Tier:** Grok. **Needs:** T3.1, T3.3, T3.4, T3.5.
- **Files:** new `host/tallowd/src/panel/`.
- **Deps:** `axum`, `tokio`, `hmac`, `sha2`.
- **Do:** Port routes in the contract's order. Password check is constant time. Agent tokens follow `app/agents.py`: the socket `data/panel/agent.sock` is mode `0600`, and it serves the H7020 API only. Add `--preview`.
- **Done when:** `tools/tests/panel_contract.py` passes against `tallowd --preview` with the same file that passes against Python.
- **Stop if:** a contract case seems wrong. The Python is the truth until the user says otherwise.

## T3.7 BLE from Rust

- **Tier:** Sonnet. **Needs:** T3.3.
- **Files:** new `host/tallow-govee/src/ble.rs`, new `tools/rust-host.Dockerfile`, `tools/mesh.sh`.
- **Deps:** `bluer`.
- **Do:** `bluer` needs libdbus to build, and the pinned image lacks it. Add a small pinned Dockerfile on top of the same image with `libdbus-1-dev`, and have `tools/mesh.sh daemon` use it. Connect through the UB500 as `tools/govee_h7020.py` does, found by USB id `2357:0604`, never by `hci` number, since those swap. Keep the reconnect behaviour in `app/h7020_panel.py`: a failed connect logs, retries, and leaves the panel up, with light routes answering 503.
- **Done when:** `tools/mesh.sh daemon check` passes. **You:** power, brightness, and the solid red frame work on the real H7020, which are the writes this unit has shown.
- **Stop if:** a scene or paint is needed to pass. Those have not been sent to this string yet, from either language.

## T3.8 Agent broker

- **Tier:** Grok. **Needs:** T3.6.
- **Files:** new `host/tallow-broker/`, `tools/tallow-panel-broker.service`.
- **Deps:** `tokio-vsock`.
- **Do:** Port `tools/panel_broker.py` with every limit intact: port 17620, one guest CID, 16 KB request, 256 KB response, 5 s and 15 s timeouts, and the allowlist. Writes to the library and schedules stay refused. The token stays out of the guest. Port `tools/tests/test_panel_broker.py` case by case.
- **Done when:** `tools/mesh.sh daemon check` passes, with an allowlist test proving each refused route is still refused.
- **Stop if:** anything would widen the allowlist.

## T3.9 The map on the panel

- **Tier:** Grok. **Needs:** T2.6, T3.6.
- **Files:** `host/tallowd/src/panel/`, new `app/devices_page.html`, `app/menu_page.html`.
- **Do:** Add `/devices` and `GET /api/mesh/map` behind the same LAN cookie, and 404 on the agent socket. Show each device's name, kind, room, nearest node, and last heard, plus a node list with rank and heap. It is read only, and it adds no control. Add the routes to `tools/tests/panel_contract.py`, marked Rust only. Add a Devices tile to the menu. Follow `app/cams_page.html` for style, and add a node harness like `tools/tests/cams_page_harness.js`.
- **Done when:** the contract passes, and so does the harness on the empty state, a device with no room, and a node with no parent.
- **Stop if:** it would show a device's rotating address as if it were a stable identity without saying so.

## T3.10 Cut over

- **Tier:** You. **Needs:** T3.6, T3.7, T3.8.
- **Do:** Stop the Python panel, start `tallowd`, and use it for a week. To roll back, stop `tallowd` and start `.venv/bin/python app/h7020_panel.py`. Both use the same files.
- **Done when:** a week passes with no rollback, and you say so. Only then does phase 5 apply to the panel.
