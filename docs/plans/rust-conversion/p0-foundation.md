# Phase 0: foundation

**Done (2026-10-31).** Recorded here so nobody redoes it. Measurements are in [docs/01](../../01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh).

| Item | Result |
| --- | --- |
| Toolchain | `espressif/idf-rust:esp32_1.95.0.0`, ESP-IDF v5.5.5, through `tools/mesh.sh` |
| Mesh core | `firmware/mesh/core`, with unit, golden, routing, and simulated-radio tests |
| Firmware | `firmware/mesh/node`, on two classic ESP32s over one physical hop |
| Host end | `tools/mesh_host.py`, the map in `data/mesh/map.json` |
| Offline gate | `tools/mesh.sh check`, which also runs the panel tests and contract and builds the cam. Each of these was tried and made it fail: a failing core test, a failing daemon test, unformatted code, a clippy warning, and a firmware compile error |
| Wrapper | A failed build or flash now exits non-zero. Before this, a pipe into `tail` hid failures |
| Port guard | `flash`, `monitor`, `backup`, and `host` touch only a `/dev/ttyUSB*` port that `boards.tsv` lists as `esp32`. An S3 cam's CH340 port is a `ttyUSB` too, so the name alone was not enough |
| Backups | `tools/mesh.sh backup` saves a whole flash, falling back to a slower speed. Mode 0600, because a mesh board's flash holds the mesh key |
| Board finder | `tools/mesh.sh ports` and `firmware/mesh/boards.tsv` |
| Host crate | `host/tallowd`, a stub that shares `tallow-mesh-core`, wired into the gate |
| References | `references/rust/` and `references/presence/`. `tools/references.lock` pins every clone, and a fresh clone checks out the locked revision |
| Rules | `.cursor/rules/iot-network-isolation.mdc` reworded for mesh nodes, `.cursor/rules/rust-conversion.mdc` added |
