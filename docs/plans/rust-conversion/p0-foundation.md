# Phase 0: foundation

**Done (2026-10-31).** Recorded here so nobody redoes it. Measurements are in [docs/01](../../01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh).

| Item | Result |
| --- | --- |
| Toolchain | `espressif/idf-rust:esp32_1.95.0.0`, ESP-IDF v5.5.5, through `tools/mesh.sh` |
| Mesh core | `firmware/mesh/core`, 13 unit and 15 simulated-radio tests |
| Firmware | `firmware/mesh/node`, on two classic ESP32s over one physical hop |
| Host end | `tools/mesh_host.py`, the map in `data/mesh/map.json` |
| Offline gate | `tools/mesh.sh check`. Each of these was tried and made it fail: a failing core test, a failing daemon test, unformatted code, a clippy warning, and a firmware compile error |
| Wrapper | A failed build or flash now exits non-zero. Before this, a pipe into `tail` hid failures |
| Port guard | `flash`, `monitor`, `backup`, and `host` refuse anything but `/dev/ttyUSB*`, so nothing can land on or knock out an S3 cam |
| Backups | `tools/mesh.sh backup` saves a whole flash, falling back to a slower speed |
| Board finder | `tools/mesh.sh ports` and `firmware/mesh/boards.tsv` |
| Host crate | `host/tallowd`, a stub that shares `tallow-mesh-core`, wired into the gate |
| References | `references/rust/` and `references/presence/`, pinned in `tools/references.lock` |
| Rules | `.cursor/rules/iot-network-isolation.mdc` reworded for mesh nodes, `.cursor/rules/rust-conversion.mdc` added |
