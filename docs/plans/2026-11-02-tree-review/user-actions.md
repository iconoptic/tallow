# Your steps

None of these are agent tasks. Agents in this run do not stop the panel, open a port, dial a cam, or commit.

## Decisions an agent is waiting on

Settled for this run (2026-11-02), using the defaults already written here. Say otherwise and the matching task can be redone.

- **U2: the stop hook. Settled: remove.** You deleted `.cursor/hooks/docs-before-stop.py`. A1 removes the wiring and leaves the docs pass by hand. Say "restore" if you want a loop-safe script back instead.
- **U3: cute-name space. Settled: (c) leave it.** There are 1,024 names today, and 644 random addresses were heard in 16 hours, so names repeat. (a) 4,096 names from longer word lists, (b) the current names plus two hex digits (`Calm Fox 3e`), or (c) leave it. (a) and (b) rename every device once. D3 does not run.
- **U4: every ESP32-S3 is flashed. Settled: keep that.** `fleet_catalog.py` gives every S3 `flash_ok: true` and names a new one `toby-cam-N`, so `fleetd` writes the cam image to any S3 plugged in while it runs, including a non-cam dev board. No new task. Say if you want an opt-in instead (for example, flash a new S3 only after `tools/mesh.sh cam-flash` has learned it).

## Before the run

Do this before the first agent starts. It folds in step 1 of "Now".

**Run the panel from a copy that builds cannot touch, as your user.** The panel resolves its own path once and respawns `tallowd run` from that file after every pause or failure. On 2026-11-02 that file was `host/target/release/tallowd`, which every agent's `tools/mesh.sh daemon build` rewrites. A copy at `host/target/live/` keeps the panel's repo-root lookup (four directories up) and is never written by cargo. Stop the panel with Ctrl-C in its terminal, then:

```bash
mkdir -p host/target/live
cp host/target/release/tallowd host/target/live/tallowd
sudo chown "$(id -un):$(id -gn)" data/mesh/map.json
sg dialout -c 'host/target/live/tallowd panel'
```

Agents check `pgrep -af 'tallowd (panel|run)'`. While it still shows `host/target/release/tallowd`, they skip `daemon build` and the HTTP contract and say so.

## Now

1. **Restart the panel as your user.** Covered by [Before the run](#before-the-run). It was running as `sudo host/target/release/tallowd panel` (started about 19:26 on 2026-11-02), and its child writes `data/mesh/map.json` as root `0600`.

2. **Check the two pushed cams.** `toby-cam-2` (`10.87.0.24`) and `toby-cam-3` (`10.87.0.25`) took the signed image on the evening of 2026-11-02. It is marked valid after five minutes of Wi-Fi and a frame, and rolls back if that does not happen within 15 minutes. Nobody watched that window. `tools/mesh.sh fleet` shows whether each answers on 6053. `toby-cam-3` is not in `data/panel/cams.json`, so run `tools/mesh.sh cams` to add it.
3. **Confirm `toby-cam-0`'s MAC.** The catalog, `boards.tsv`, and the cam note all give `44:bd:8d:7a:51:60`. The last docs pass flagged it. Only the board can settle it.

## After chunks B, C, and D land

1. `tools/mesh.sh daemon build`, copy `host/target/release/tallowd` over `host/target/live/tallowd`, and restart the panel as in [Before the run](#before-the-run).
2. Wait about 10 minutes for the first two reconciles, then check that no cluster holds addresses heard at the same time:

   ```bash
   sqlite3 -readonly data/mesh/traffic.db "
   select n, count(*) from (select seed, count(*) n from clusters group by seed) group by n order by n;"
   ```

   Before the fix, the largest group was 155 addresses. A believable group is at most a few per hour of the device being in range.
3. Open `/traffic` on Live and leave it for two minutes. Dots should keep moving, and a newly heard device should appear.

## When you want to

- Commit the tree. Agents do not commit unless you ask.
- The mesh boards still run frame `0xA1`, and the source is `0xA8`. Flash every board together, by USB, when you choose ([docs/01](../../01-bluetooth-server-hardware-and-proxies.md#source-ahead-of-the-boards-frame-version-0xa8)).
