# Review prompt for the next planner

Paste everything below the line into a fresh Opus session at the repo root.

---

You are the reviewer and planner for `tallow`, a self-hosted smart-light controller: a Rust ESP32 mesh, a Rust host daemon (`tallowd`), a LAN panel, and S3 cam firmware. Grok agents, a cheap model, have just worked through `docs/plans/rust-conversion/p6-grok-run.md`. Find what they got wrong, resolve it, and plan what comes next.

## Read first

1. `AGENTS.md`
2. `docs/plans/2026-10-31-rust-conversion.md`
3. `docs/plans/rust-conversion/agent-brief.md`
4. `docs/plans/rust-conversion/p6-grok-run.md`
5. `docs/plans/rust-conversion/hardware-checklist.md`
6. `.cursor/rules/iot-network-isolation.mdc`

The review range is `git log grok-run-start..` plus the working tree. If that tag is missing, stop and ask the user for the baseline.

## Hard limits

These hold for you and for any agent you start:

- Do not commit, flash, open a serial port, connect to a device, or run `tools/mesh.sh flash`, `monitor`, `backup`, `host`, `bench`, `daemon run`, `daemon gatt`, `daemon ota`, `ota-key`, or `ota-sign`, unless the user asks in this session.
- Never read or print `firmware/mesh/mesh.key`, `firmware/mesh/ota.key`, `firmware/esphome/secrets.yaml`, or anything in `secrets/`.
- `toby-cam-0`'s reservation is `10.87.0.21`; the Rust image takes it by DHCP, and `cam-push` allows only an address reserved for a catalog cam ([device note](../../devices/keyestudio-mb0200-s3-cam.md#rust-image)). Do not flash, do not dial it, and do not touch the `toby-cam-1` board. It is taped to a wall, and a bad build already took it down once.
- Host configuration is a script the user runs.

## Treat reports as claims

The plan's Done markers, agents' reports, and the docs are claims. Code and command output are evidence. A test passing shows only what it asserts, so read the assertions. Check first whether any test was weakened, deleted, or had its expected value changed to match the code:

```bash
git diff grok-run-start.. --stat
git diff grok-run-start.. -- '*tests*' firmware/mesh/core/tests tools/tests
```

## Run these first

```bash
tools/mesh.sh check
tools/mesh.sh mutants
.venv/bin/python tools/tests/panel_contract.py --rust-only --panel-command 'host/target/release/tallowd panel --preview'
.venv/bin/python tools/tests/panel_contract.py
```

If G2 did not land, `mutants` will be missing. The originals are in `tools/tests/mutants/`.

## Where to look hardest

Grok was cleared for work the plan rated above it, so weigh these highest:

1. **OTA (G6, G7, G8, G10).** Can any path activate an unverified image? Does the signature cover the bytes actually written? Are size and offset bounds checked before each write? Does a node refuse a downgrade, and should it? Nothing in the plan says, so decide. Is the 5-minute validity check really gated on health? Does the flashed bootloader really have rollback on? `espflash` uses its own unless given `--bootloader`, so inspect the built binary rather than the config. Can `ota.key` reach any log or argv?
2. **Isolation.** Does anything page, connect to, or pair with a device off a whitelist? Check `cut_inquiry`, Watch paging, and the GATT whitelist in every link state. Does classic `firmware/mesh/node` still join no Wi-Fi and open no socket? A cam may hold `mesh.key` and speak ESP-NOW as a leaf, and it must not bridge ([docs/01](../../01-bluetooth-server-hardware-and-proxies.md#camera-leaves)). Is the Wi-Fi password, and the mesh key in a cam image, private in `firmware/cam/target`, and is an empty credential a build failure?
3. **The wire.** Any new frame kind must go through `Sealer::open`, and a version bump must have regenerated goldens with only byte 0 and the tag changed. Any new parser takes bytes off the air, so check bounds and `unwrap`. Does `core` still have no heap?
4. **`unsafe` in `node/src/` and `firmware/cam/src/`.** Does each union read match its event? Is every slice bounded, and does anything outlive the callback? Is there any blocking in a Bluedroid or Wi-Fi callback, flash writes above all?
5. **`tools/mesh.sh`.** Port guards intact, pipefail intact, and failures exit non-zero. Prove it by breaking something.

## Not closely reviewed in the last session

Haiku built these in the session before this run. They passed their tests and the full gate, but no reviewer read them line by line. The coordinator read only the parts noted. Give each a real pass:

- `host/tallowd/src/cams.rs` (T3.5). A cooldown or linger bug wedges a real cam. Compare it with `app/cams.py`.
- `host/tallowd/src/panel/session.rs` and `bluez.rs` (T3.7). Never run on hardware. Note the listed deviations: a failed write redials, and the scan reads BlueZ's cached adverts, so an absent light can look present.
- `firmware/mesh/core/src/gatt.rs` beyond the whitelist (T1.10): subscribe chains, timeouts, stale results.
- `host/tallow-broker` beyond its constants and allowlists (T3.8): parsing and the token path. It is a security boundary to a guest VM.
- `host/tallowd/src/library.rs`, `show.rs`, `irk.rs`, `identity.rs`, `rooms.rs`.
- `tools/setup_mesh_host.sh` and `tools/tallowd.service` (T2.7). Never run. Check the restart policy: `Restart=on-failure`, `RestartSec=30`, and `StartLimitBurst=5` in 600 s, since every start resets the root board. Confirm the sandbox (`ProtectSystem=strict`, `ReadWritePaths=-data/mesh`, `PrivateTmp=true`) still lets tallowd write the map.
- `firmware/mesh/node/src/gatt.rs`: stack headroom is unmeasured on the 12,000-byte main task, and on the 4,096-byte Bluedroid task, which copies buffers in its callback.

## Hardware evidence

Check `docs/01`, `docs/devices/`, and the checklist for what has really run. Say plainly what is still proven only in simulation. Claims about boards need a dated measurement. If a checklist step failed, that outranks every code finding, so plan around it first.

## How to work

- Verify each finding by reading the code or running something, and drop what you cannot verify.
- Fix what is clearly wrong, smallest change first, with a test that fails before the fix. Prove new tests with a mutation.
- Anything that changes bytes on the air, a settled decision in the plan, or a security boundary goes to the user first.
- Use subagents only if the user asks. If you do, give each a read-only brief, and run the gate yourself between batches: they share one tree.
- Reconcile docs per `.cursor/rules/docs-before-summary.mdc`, and write the ledger last.

## Deliver

1. A verdict for each G-task: accepted, fixed, or reopened, with the evidence.
2. Findings by severity, each with file and line, a failure scenario, and what you did.
3. What is proven on hardware against what is only simulated.
4. An updated plan: the next tasks with tier, files, done-when, and stop-if, in the format of `p6-grok-run.md`, and an updated `hardware-checklist.md`.
5. A decision on what can go to a cheap model next, and what should not, based on what you found in this run.
