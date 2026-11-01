# Phase 3: panel, H7020, and broker

Port the running panel with no visible change. The method: first pin the Python's behaviour with tests over HTTP, then make Rust pass the same tests.

**Keep:** `app/*.html` and `app/panel.css`, served as they are, except for the placeholders the Python fills: `__CAMS_TITLE__` and `__IOT_NET__` are HTML-escaped first, and `__PHONE_HOME__` is the light page's note. The data files under `data/panel/` and `secrets/panel/`. The port, `8765`. The cookie `tallow_panel` and its HMAC, so a logged-in browser stays logged in.

## T3.1 HTTP contract tests against the Python

**Done (2026-10-31), then hardened at the cross-check.** Run it with `.venv/bin/python tools/tests/panel_contract.py`. It is part of `tools/mesh.sh check`. It starts its own preview on a free port, using a throwaway password, throwaway agent tokens, and a temporary data directory with one seeded cam at `10.87.0.254`, which no device holds. Nineteen deliberate breaks in `app/` each made it fail, including forged cookies, a made-up bearer, a removed rate limit, and a socket left at mode 0666. The script header lists what it skips.

- **Tier:** Grok. **Needs:** nothing.
- **Files:** new `tools/tests/panel_contract.py`.
- **Do:** A standalone script using only the standard library. It takes `--panel-command`, the panel in preview mode before its flags. Preview takes `--port`, `--password-file`, `--data-dir`, and `--agents-file`, binds `127.0.0.1`, skips BLE, and does not read `secrets/panel/`. Cover every route below: status, content type, JSON shape, the 401 without a cookie or with a forged one, redirects to `/login`, and bad input.
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

The agent socket's 404 list is in [docs/devices/govee-h7020.md](../../devices/govee-h7020.md#agent-socket). The contract covers it, plus bearer tokens, scopes, and the audit log.

`tools/tests/test_h7020_panel.py`, `test_h7020_library.py`, `test_cams.py`, and `test_panel_broker.py` hold what HTTP cannot reach. Each task below says which of their tests it ports.

## T3.2 Golden H7020 packets

**Done (2026-10-31).** `tools/dump_h7020_vectors.py` writes 395 vectors and 51 scenes, byte-identical across two runs. The 126 figure counts only the speed options. The 11 scenes with no speed control each add one script, so the dump makes 137 scene scripts.

- **Tier:** Grok. **Needs:** nothing.
- **Files:** new `tools/dump_h7020_vectors.py`, new `host/tallow-govee/tests/vectors.json`, new `host/tallow-govee/scenes.json`.
- **Do:** Use the Python codec through `tools/govee_h7020.py` to dump every packet the panel can send: all 51 catalogue scenes at every speed they offer (126 scripts, counted from `list_scenes`), power, brightness steps, colours, paints, and status parsing with known replies. The colour frame depends on the string count, so dump power, colour, and paint for both 1 and 2 strings. Each entry is its input and its hex bytes. Write a second file, `scenes.json`, from `OFFICIAL_MODEL_SCENES`: each scene's name, category, speeds, default speed, and the fields the builders read in `native_scenes.py`. Default-speed packets alone do not contain those.
- **Done when:** both files are deterministic: two runs give identical bytes.
- **Stop if:** anything in them depends on the time or randomness. Report it. The driver must handle it.

## T3.3 H7020 codec crate

**Done (2026-10-31).** `tools/mesh.sh daemon check` passes and lists the crate's tests. `host/tallow-govee/tests/vectors.rs` checks all 395 vectors in `vectors.json` against the codec, refusals included, and fails on any unknown kind. Dependency: `serde_json` only, with base64 hand-written.

- **Tier:** Sonnet. **Needs:** T3.2.
- **Files:** new `host/tallow-govee/`, `host/Cargo.toml`.
- **Do:** Add the crate to `members` in `host/Cargo.toml` first, or `tools/mesh.sh daemon check` will not build it. A pure crate with no I/O: inputs in, packet bytes out, and reply bytes in, status out. It does not connect or hold state. The `LightDriver` shape in [docs/05](../../05-target-webapp-architecture.md), which does both, goes in `src/ble.rs` in T3.7. Read the pinned Python in `references/protocols-ble/ha-govee-led-ble`. Load the scene table from `scenes.json` with `include_str!`. Do not hand-copy 51 scenes.
- **Done when:** the crate compiles with no Bluetooth dependency, and a test proves every vector in `vectors.json` byte for byte. `tools/mesh.sh daemon check` passes and lists this crate's tests.
- **Stop if:** a vector cannot be matched. Do not special-case it.

## T3.4 Library and schedules

**Done (2026-10-31).** `host/tallowd/src/library.rs` and `show.rs` port `app/library.py` and `app/show.py`. Five of the eight Python cases are ported by name. The other three are the two `TokenDoorTest` cases (T3.6) and `SchedulePageTest` (a page harness). `tools/tests/test_library_fixture.py` checks the bytes both ways. The fixtures are under `host/tallowd/tests/fixtures/library/`, because the pinned container mounts only `host/`.

- **Tier:** Grok. **Needs:** T3.3, for the scene table and the packet builders.
- **Files:** new `host/tallowd/src/library.rs`, `show.rs`.
- **Deps:** `serde`, `serde_json`, and `chrono` with the `clock` feature, because `due_schedule` works on local time.
- **Do:** Port `app/library.py` and `app/show.py`. Read and write the same JSON files. Use `tools/tests/test_h7020_library.py` as the list of cases, and port each test by name, except `TokenDoorTest`, which T3.6 ports, and `SchedulePageTest`, which is a page harness and stays as it is.
- **Done when:** `tools/mesh.sh daemon check` passes, with a test that loads a file written by the Python and writes one the Python reads.

## T3.5 Cam proxy

**Done (2026-10-31).** Two error strings differ from Python's by design: the JSON parse reason comes from `serde_json`, and the UTF-8 message is close but not identical. `PagesTest` and `LightAwayTest` are T3.6.

- **Tier:** Sonnet. **Needs:** nothing.
- **Files:** new `host/tallowd/src/cams.rs`.
- **Deps:** `tokio`, `serde`, `serde_json`. No HTTP client: a pooling client can hold or reopen a second connection, and a cam serves one.
- **Do:** Port all of `app/cams.py`: `Book`, `parse_inventory` and `save_inventory` with the address guard, `dial`, `reachable`, `Hub`, `Hubs`, and the frame reader. `Feed` is the HTTP response, which T3.6 wraps. Its gentleness is the point, because a cam serves one connection and wedges easily. Dial with a raw `tokio::net::TcpStream` and write exactly the bytes `dial` writes: `GET / HTTP/1.1`, `Host`, `Connection: close`. Keep each of these exactly: `LINGER_S` 10, `COOLDOWN_S` 3, 6, 12, 24, 30, connect timeout 5 s, header timeout 10 s, `FRAME_TIMEOUT` 10 s, `FRAME_MAX` 2 MiB, `HEAD_MAX` 8 KiB, the 1.5 s status timeout, `VIEWERS_MAX` 8, per-viewer drop-oldest queues of 2, and a cancelled dial closing its socket. Status asks port `6053` and never `8080`. See [One connection per cam](../../devices/keyestudio-mb0200-s3-cam.md#one-connection-per-cam).
- **Done when:** each case in `tools/tests/test_cams.py` has a matching test against a fake cam server, except `PagesTest` and `LightAwayTest`, which need the panel and belong to T3.6.
- **Stop if:** a test needs a real cam. Never burst-test a real cam: that is what jammed it.

## T3.6 Panel routes

**Done (2026-10-31).** `tallowd panel` serves every route in the contract, the cam stream included. Both contracts pass: the release binary with `--panel-command 'host/target/release/tallowd panel --preview'`, and Python with no flag. Deviations from Python:
- A preview without `--password-file` refuses to start. Python reads `secrets/panel/password` there; a preview never reads `secrets/`.
- A non-integer scene speed is refused before playback is cancelled. Python cancels first.
- The agent socket is bound, then set to 0600 at once, inside a 0700 directory. Python sets 0600 after uvicorn's 0666 window. The socket file is removed on shutdown.
- Request bodies are capped at 2 MiB. Python has no limit.

- **Tier:** Grok. **Needs:** T3.1, T3.3, T3.4, T3.5.
- **Files:** new `host/tallowd/src/panel/`, `host/tallowd/src/main.rs`.
- **Deps:** `axum`, `tokio`, `hyper-util` for the unix socket, `tower`, `hmac`, `sha2`, `serde`, `serde_json`, `serde_urlencoded`.
- **Do:**
  - Add `tallowd panel`, with the Python's flags: `--host`, `--port`, `--address`, `--adapter`, `--scan-seconds`, `--timeout`, `--strings`, `--preview`, and the preview-only `--password-file`, `--data-dir`, and `--agents-file`. It is a separate command from `tallowd run`, whose `--port` is a serial port.
  - Port routes in the contract's order. Password and cookie checks are constant time. Agent tokens follow `app/agents.py`: the socket `data/panel/agent.sock` is mode `0600` in a `0700` directory, and it serves the H7020 API only.
  - Keep the check order: login, scope, rate limit, link, body.
  - A valid JSON body is accepted whatever its `Content-Type` says, so do not use axum's `Json` extractor. It would answer 415.
  - Port the bind rules: refuse `0.0.0.0` and anything in `10.87.0.0/24`, and pick the default-route source (`choose_host` and `BindHostTest`). A panel on the wrong interface breaks the isolation rule, and the contract cannot see it, because preview binds `127.0.0.1`.
  - Port the password file check: mode 0600, one line.
  - Port `TokenDoorTest`, `tools/tests/test_h7020_panel.py`, and `PagesTest` and `LightAwayTest` from `test_cams.py`.
- **Done when:** `.venv/bin/python tools/tests/panel_contract.py --panel-command 'host/target/debug/tallowd panel --preview'` passes with the same file that passes against Python.
- **Stop if:** a contract case seems wrong. The Python is the truth until the user says otherwise.

## T3.7 BLE from Rust

**Code done (2026-10-31), not run on hardware.** The light check is yours.

- **Tier:** Sonnet. **Needs:** T3.3, T3.6.
- **Files:** new `host/tallow-govee/src/ble.rs`, `host/tallowd/src/panel/`, new `tools/rust-host.Dockerfile`, `tools/mesh.sh`.
- **Deps:** `bluer`.
- **Do:**
  - `bluer` needs libdbus to build, and the pinned image lacks it. Add a small pinned Dockerfile on top of the same image with `libdbus-1-dev`, and have `tools/mesh.sh daemon` use it.
  - Connect through the UB500 as `tools/govee_h7020.py` does, found by USB id `2357:0604`, never by `hci` number, since those swap.
  - Port `LightSession` from `app/h7020_panel.py`, not only the connect: slider writes coalesce, scripts stay in order, and an idle link sends a power read every 4 s, because the box drops an idle central after about 10 s.
  - Keep the reconnect behaviour: a failed connect logs, retries, and leaves the panel up, with light routes answering 503 `disconnected`.
- **Done when:** `tools/mesh.sh daemon check` passes. **You:** stop the Python panel first, because the box takes one central. Then power, brightness, and the solid red frame work on the real H7020, which are the writes this unit has shown.
- **Stop if:** a scene or paint is needed to pass. Those have not been sent to this string yet, from either language.

## T3.8 Agent broker

**Done (2026-10-31).** Not deployed. `tools/tallow-panel-broker.service` still runs the Python, which is the rollback. The Rust unit is written and not installed. Deviations from `panel_broker.py`, all tighter: the guest's whole request read has one 5 s deadline, not one per recv, and the panel's write and read share one 15 s deadline; a bare CR or LF in a kept panel header is a 502; a panel status outside 100 to 599 is a 502; the token must be printable ASCII with no space; a POST body that is invalid UTF-8, a BOM, or NaN is a 400 (Python raises, answers 503, or forwards). `tokio-vsock` listens with a backlog of 128, not 16.

- **Tier:** Grok. **Needs:** T3.6.
- **Files:** new `host/tallow-broker/`, `host/Cargo.toml`, new `tools/tallow-panel-broker-rust.service`.
- **Deps:** `tokio-vsock`.
- **Do:** Add the crate to `members` in `host/Cargo.toml`. Leave `tools/tallow-panel-broker.service` alone: it is the live unit, it runs the Python broker, and it is the rollback. Port `tools/panel_broker.py` with every limit intact: port 17620, one guest CID, 16 KB request, 256 KB response, 5 s and 15 s timeouts, and the allowlist. Writes to the library and schedules stay refused. The token stays out of the guest. Port `tools/tests/test_panel_broker.py` case by case.
- **Done when:** `tools/mesh.sh daemon check` passes, with an allowlist test proving each refused route is still refused.
- **Stop if:** anything would widen the allowlist.

## T3.9 The map on the panel

**Done (2026-10-31).**

- **Tier:** Grok. **Needs:** T2.6, T3.6.
- **Files:** `host/tallowd/src/panel/`, new `app/devices_page.html`, `app/menu_page.html`, `tools/tests/panel_contract.py`.
- **Do:** Add `/devices` and `GET /api/mesh/map` behind the same LAN cookie, and 404 on the agent socket. The panel reads `data/mesh/map.json`, which `tallowd run` writes as a separate process, and shows an empty state when the file is missing or older than 60 s. Show each device's name, kind, room, nearest node, and last heard, plus a node list with rank and heap. It is read only, and it adds no control. Add the routes to `panel_contract.py` behind a new `--rust-only` flag, so the file still passes against Python without it. Python stays the rollback, so `app/menu_page.html` gets only a `<!-- devices tile -->` comment, and Rust fills in the tile: a dead link on the Python menu would be a bug. Follow `app/cams_page.html` for style, and add a node harness like `tools/tests/cams_page_harness.js`.
- **Done when:** the contract passes with `--rust-only`, and so does the harness on the empty state, a device with no room, and a node with no parent.
- **Stop if:** it would show a device's rotating address as if it were a stable identity without saying so.

## T3.10 Cut over

- **Tier:** You. **Needs:** T3.6, T3.7, T3.8.
- **Do:** Stop the Python panel, start `tallowd`, and use it for a week. To roll back, stop `tallowd` and start `.venv/bin/python app/h7020_panel.py`. Both use the same files.
- **Done when:** a week passes with no rollback, and you say so. Only then does phase 5 apply to the panel.
