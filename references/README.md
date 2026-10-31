# Reference clones

This directory is populated by [`../tools/clone_references.sh`](../tools/clone_references.sh) and is **gitignored**.

```bash
./tools/clone_references.sh
```

See the catalog in the root [README.md](../README.md).

Primary AI reverse-engineering bridge: `re-tooling/rea` ([morluto/rea](https://github.com/morluto/rea)). Host deps: [`../tools/install_re_toolchain_fedora.sh`](../tools/install_re_toolchain_fedora.sh). Cursor MCP: [`.cursor/mcp.json`](../.cursor/mcp.json).

`rust/` holds the libraries the [Rust conversion](../docs/plans/2026-10-31-rust-conversion.md) builds on. `presence/` is prior art for rooms and rotating addresses.

`vendor/` is not cloned. It holds partial upstream trees copied by hand from an earlier project with the MB0200 board: esp32-camera, esp-idf docs and examples, idf-extra-components `network_provisioning`, frigate docs, the go2rtc README, and `hardware/` (ESP32-S3 and WROOM datasheets, the Keyestudio MB0200 page). `vendor/VERSIONS.tsv` lists source, ref, and commit. `tools/clone_references.sh` and `tools/references.lock` do not manage it.
