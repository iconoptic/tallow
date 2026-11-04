# Chunk G: docs and status

## G1 Docs and status

**Done (2026-11-02).** Docs 01, AGENTS, and the agent brief now match the join rules, the log, the live map, the broker-user panel, and the CP2102 port rule. `app/vendor/VERSIONS.tsv` records the vendored vis-timeline files. The CSS file has no version header of its own. D3 did not run. daemon build skipped: live panel runs from host/target/release. The full `tools/mesh.sh check` release build of `tallowd` was not run for the same reason.

- **Tier:** Grok.
- **Needs:** every task in the README's order table that ran. Skip the parts for tasks that did not run, and say so.
- **Files:** `AGENTS.md`, `README.md`, `docs/01-bluetooth-server-hardware-and-proxies.md` (traffic-log and devices sections), `docs/devices/keyestudio-mb0200-s3-cam.md`, `docs/plans/rust-conversion/agent-brief.md` ("Facts that trip people up" only), this folder's chunk files (done marks only), and a new `app/vendor/VERSIONS.tsv`. Follow `.cursor/skills/improve-repo-docs/SKILL.md` in working-tree mode, and write `.cursor/docs-ledger.json` last.
- **Do:**
  1. **Join rules.** Where `docs/01` and the AGENTS "Traffic page" row describe joins, state B1's rules: chains only, no join when a handoff has two candidates, the -85 dBm floor and two hearings a side, 2-minute exclusivity across the whole chain, 30-minute span. State that joins are recomputed every 5 minutes on their own thread (B3), and that a cluster keeps its seed after the seed address ages out (B2).
  2. **Log robustness.** Say that a traffic log error no longer stops `tallowd run` (C1), that Clear overwrites deleted rows and truncates the WAL (C2), and that an advertised name is deleted once its address has no sightings left in the 7 days (C4).
  3. **Placement.** Say that the three-listener fit and all distances use canvas proportion (D1), and that the live view follows the current time until the user drags the marker (D2).
  4. **Stop hook.** If A1 ran, check that no doc still names `docs-before-stop.py`, and that the docs pass is described as by hand.
  5. **agent-brief facts.** The line "`tools/mesh.sh` refuses those ports, and any port `boards.tsv` does not list as `esp32`" is out of date. A CP2102 port in any socket is accepted as a classic board, and another bridge needs an `esp32` read on that path (see the `need_classic_port` comment in `tools/mesh.sh`). Fix that sentence only.
  6. **Panel user.** AGENTS says the panel must run as the broker user. On 2026-11-02 it was running under `sudo`, so `data/mesh/map.json` became root-owned `0600`, and `tools/mesh.sh fleet` (run as the user) cannot read it. Add one sentence under "Finish the web app": run `tallowd panel` as the broker user, with `sg dialout` if the login lacks the group, never with `sudo`. Add a second sentence: run it from a copy at `host/target/live/tallowd`, because the panel respawns its map writer from its own path and `daemon build` rewrites `host/target/release/tallowd`.
  7. **Vendored script.** Write `app/vendor/VERSIONS.tsv` with one row per file: name, `vis-timeline` `7.7.3` (from the file header), source URL `https://unpkg.com/vis-timeline@7.7.3/standalone/umd/` (check the header and write only what it supports), licence `Apache-2.0 OR MIT`, and `sha256sum` of each file. Link it from the AGENTS "Traffic page" row.
  8. Mark each task that ran `**Done (date).**` in its chunk file, with the commands and results from that task's report.
- **Done when:** every relative link in the edited files resolves (the skill's link check). `python3 .cursor/skills/improve-repo-docs/scripts/diff_hash.py` matches the new ledger. `tools/mesh.sh check --fast` passes.
- **Stop if:** a doc and the code disagree on something no task here changed. Report it. Do not change the code.
