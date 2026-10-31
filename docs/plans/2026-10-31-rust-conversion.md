# Rust conversion plan (2026-10-31)

Move tallow's firmware and host runtime to Rust, and finish the mesh on the way. Work is split into small tasks that a cheaper model can do from the task text alone.

**Status (2026-10-31):** Phase 0 is done. The mesh runs on two classic ESP32s over one physical hop, and `tools/mesh.sh check` passes. Results are in [docs/01](../01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh). Nothing in phases 1 to 5 has started.

**Agents start at [rust-conversion/agent-brief.md](rust-conversion/agent-brief.md).** It has the rules, the tiers, and the stop conditions.

## Scope

| Part | Today | Target |
| --- | --- | --- |
| Mesh node firmware | Rust, listens only | Rust, listens, controls, updates over the mesh |
| Mesh host end | `tools/mesh_host.py` | `tallowd` in `host/` |
| LAN panel | Python, FastAPI, `app/*.py` | `tallowd`, axum. Same routes, same data files |
| H7020 driver | Python, `tools/govee_h7020.py` plus the pinned codec | Rust crate, proven byte for byte against the Python |
| Agent broker | Python, `tools/panel_broker.py` | Rust binary, same rules |
| Cam firmware | ESPHome on the S3 | Rust on ESP-IDF |

Staying as they are, on purpose:

- **Bash setup scripts** (`tools/setup_iot_ap.sh`, `tools/setup_iot_ble.sh`, and the like). `.cursor/rules/host-config-scripts.mdc` requires host configuration to be a script, and they do not run in the product.
- **Panel HTML, CSS, and page JavaScript.** Rust serves the same files. Their node harnesses keep working.
- **RE helpers** (`tools/parse_btsnoop.py`, `tools/ble_discover.py`, `tools/node_check.py`). They are one-off bench tools, not the running system. `node_check.py` retires with ESPHome in phase 5.

## Decisions

These are settled. A task does not reopen them.

| Decision | Why |
| --- | --- |
| Node firmware stays on ESP-IDF through `esp-idf-svc`, not `esp-hal` | Bluetooth Classic needs Bluedroid, and that only exists on ESP-IDF. Speakers need Classic |
| The mesh carries records, never IP | Carrying IP is what cost esphome-mesh 116 KB |
| `tallow-mesh-core` stays `no_std` with no OS calls | It is shared by firmware and `tallowd`, and it runs under host tests |
| Mesh code uses no heap | Tables are fixed size, so a node cannot run out at runtime |
| Python stays runnable until phase 5 | Every phase can stop without breaking the house |
| Panel and H7020 behaviour is pinned first by tests that run against the Python | A port then has a pass mark. The tests are the spec |
| Rust reads the same files under `data/panel/` and `secrets/panel/` | No migration, and a rollback is restarting Python |
| New light drivers are Rust crates, not wrapped Python | One crate per protocol family, built like the H7020's: golden packets dumped from the upstream Python library, a pure codec, then BLE or LAN I/O. `tinytuya`, `flux_led`, and the like stay as sources of test vectors. Only the H7020 is in this plan. No other driver exists yet |
| Cams never join the mesh | An S3 has no Bluetooth Classic, and video needs the radio |
| Experiments run on spare boards, never `toby-cam-1` | A bad build on 2026-10-31 left it crash-looping, and it is taped to a wall |

## Phases

Each phase can ship alone. Stop after any of them and the system still works.

| Phase | File | Delivers | Needs |
| --- | --- | --- | --- |
| 0 | [p0-foundation.md](rust-conversion/p0-foundation.md) | Toolchain, gates, baseline. **Done** | |
| 1 | [p1-mesh.md](rust-conversion/p1-mesh.md) | True multi-hop proven, Classic by address, replay fix, OTA, GATT control | 0, a third classic board for T1.8 |
| 2 | [p2-host.md](rust-conversion/p2-host.md) | `tallowd`: map, rooms, identities, service | 0. Can run alongside phase 1 |
| 3 | [p3-panel.md](rust-conversion/p3-panel.md) | Panel, H7020, and broker in Rust, and the device map on the panel | 2 |
| 4 | [p4-cams.md](rust-conversion/p4-cams.md) | Cam firmware in Rust | `toby-cam-0` running ESPHome as the known good |
| 5 | [p5-retire.md](rust-conversion/p5-retire.md) | ESPHome and Python removed | 1 to 4, and the user's say-so |

## Order

Do these first. They set up everything after them.

1. T1.1, the golden frames. Host and firmware both depend on the wire format holding still.
2. T2.1 and T2.2, so `tallowd` replaces `mesh_host.py`.
3. T1.2 and T1.3, the bench tools that make hardware testing cheap.

The rest can overlap, except where a task's **Needs** line says otherwise.

## Rough cost

| Tier | Tasks | Share |
| --- | --- | --- |
| Grok | Most tasks: tests, parsers, routes, tables, docs | about 70% |
| Sonnet | `unsafe` FFI, crypto, OTA, GATT, streaming | about 30% |
| You | Button presses, flashing, anything physical | a few minutes per hardware task |

Tier assignments are per task. Do not start a task on a cheaper tier than it names.

## Open risks

Things the plan cannot settle yet. A task that hits one stops and reports.

- **GATT over the mesh may be too slow for some lights.** A write crosses each hop in milliseconds, but a connect can take seconds. T1.11 measures it.
- **Paired speakers may not answer pages while in use.** T1.5 measures it. If they do not, those speakers are visible only on BLE or when idle.
- **`esp-idf-svc` is community maintained**, with little paid Espressif time. If it stalls, the raw `esp-idf-sys` calls keep working, and `radio.rs` already uses them.
- **BlueZ from Rust needs libdbus at build time**, and the pinned image does not have it. T3.7 handles that.
