# Your steps

None of these are agent tasks. Agents in this run do not stop the panel, open a port, dial a cam, or commit.

## Decisions an agent is waiting on

Settled for this run (2026-11-02), using the defaults already written here. Say otherwise and the matching task can be redone.

- **U2: the stop hook. Settled: remove.** You deleted `.cursor/hooks/docs-before-stop.py`. A1 removes the wiring and leaves the docs pass by hand. Say "restore" if you want a loop-safe script back instead.
- **U3: cute-name space. Settled: (c) leave it.** There are 1,024 names today, and 644 random addresses were heard in 16 hours, so names repeat. (a) 4,096 names from longer word lists, (b) the current names plus two hex digits (`Calm Fox 3e`), or (c) leave it. (a) and (b) rename every device once. D3 does not run.
- **U4: every ESP32-S3 is flashed. Settled: keep that.** `fleet_catalog.py` gives every S3 `flash_ok: true` and names a new one `toby-cam-N`, so `fleetd` writes the cam image to any S3 plugged in while it runs, including a non-cam dev board. No new task. Say if you want an opt-in instead (for example, flash a new S3 only after `tools/mesh.sh cam-flash` has learned it).

## Before the run

Do this before the first agent starts. It folds in step 1 of "Now".

**Install the panel and the broker as your user.** On 2026-11-02 the panel was `host/target/release/tallowd`, which `tools/mesh.sh daemon build` rewrites. `tools/deploy_panel.sh` replaces that hand start ([govee](../../devices/govee-h7020.md#rust-panel-tallowd-panel)). If `data/mesh/map.json` is still owned by root from the 2026-11-02 sudo panel, give it back first:

```bash
sudo chown "$(id -un):$(id -gn)" data/mesh/map.json
tools/deploy_panel.sh
```

Agents check `pgrep -af 'tallowd (panel|run)'`. While it still shows `host/target/release/tallowd`, they skip `daemon build` and the HTTP contract and say so.

## Now

1. **Restart the panel as your user.** Covered by [Before the run](#before-the-run). It was running as `sudo host/target/release/tallowd panel` (started about 19:26 on 2026-11-02), and its child writes `data/mesh/map.json` as root `0600`.

2. **Check the two pushed cams.** `toby-cam-2` (`10.87.0.24`) and `toby-cam-3` (`10.87.0.25`) took the signed image on the evening of 2026-11-02. Nobody watched the five-minute valid mark. On 2026-11-06 USB `cam-flash` overwrote `toby-cam-2` and erased otadata, so that board does not roll back to the evening image ([device note](../../devices/keyestudio-mb0200-s3-cam.md#esp-now-leaf)). `toby-cam-3` was not part of that flash. `tools/mesh.sh fleet` shows whether each answers on 6053. `toby-cam-3` is not in `data/panel/cams.json`, so run `tools/mesh.sh cams` to add it.
3. **Confirm `toby-cam-0`'s MAC.** The catalog, `boards.tsv`, and the cam note all give `44:bd:8d:7a:51:60`. The last docs pass flagged it. Only the board can settle it.

## After chunks B, C, and D land

1. `tools/deploy_panel.sh`, as in [Before the run](#before-the-run).
2. Wait about 10 minutes for the first two reconciles, then check that no cluster holds addresses heard at the same time:

   ```bash
   sqlite3 -readonly data/mesh/traffic.db "
   select n, count(*) from (select seed, count(*) n from clusters group by seed) group by n order by n;"
   ```

   Before the fix, the largest group was 155 addresses. A believable group is at most a few per hour of the device being in range.
3. Open `/traffic` on Live and leave it for two minutes. Dots should keep moving, and a newly heard device should appear.

## When you want to

- Commit the tree. Agents do not commit unless you ask.
- The mesh boards still run frame `0xA1`, and the source is `0xAB`. No board was flashed with `0xA8`, `0xA9`, `0xAA`, or `0xAB`. Flash every board together, by USB, when you choose ([docs/01](../../01-bluetooth-server-hardware-and-proxies.md#source-ahead-of-the-boards-frame-version-0xa9)). The first trial that includes a camera is a spare cam plus every classic board, and never `toby-cam-1`.
