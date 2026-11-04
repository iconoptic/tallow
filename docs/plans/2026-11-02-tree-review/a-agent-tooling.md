# Chunk A: agent tooling

## A1 Unwire the deleted stop hook

**Done (2026-11-02).** `python3 -c 'import json; json.load(...)'` exited 0. `grep -rn docs-before-stop` printed nothing outside `references/` and this plan folder (`data/mesh/map.json` is root-owned and was skipped by permission). `tools/mesh.sh check --fast` passed before and after (daemon 318, core 51 + 44 + 10 + 114, cam push 29, panel OK). The G11 **Done (2026-11-01).** mark stayed. Its historical sentence named the script path, which the acceptance grep forbids, so that path was reworded to "the docs stop script" and the **Done when** line now names `diff_hash.py`. U2 is remove. daemon build skipped: live panel runs from host/target/release.

- **Tier:** Grok.
- **Files:** `.claude/settings.json`, `.cursor/hooks.json`, `AGENTS.md`, `README.md`, `.cursor/rules/docs-before-summary.mdc` (line 18 only: "The stop hook repeats this check…"), `docs/plans/rust-conversion/p6-grok-run.md` (G11's **Done when** line only).
- **Why:** The user deleted `.cursor/hooks/docs-before-stop.py` after it looped (about 35 firings in Cursor session `45deb5ca`). `.claude/settings.json` still runs `python3 "$CLAUDE_PROJECT_DIR/.cursor/hooks/docs-before-stop.py"` on Stop. A missing script makes `python3` exit 2, and Claude Code treats exit 2 from a Stop hook as "do not stop", so every Claude Code session here is held at the end of its turn. Cursor's `hooks.json` names the same missing file.
- **Do (default, unless the user chose otherwise in U2):**
  1. In `.claude/settings.json`, remove the `Stop` entry. If `hooks` is then empty, remove `hooks`. Keep the file valid JSON. It is a real file, not a link: edit it in place.
  2. In `.cursor/hooks.json`, remove the `stop` entry. Keep `"version": 1` and valid JSON.
  3. Leave the empty `.cursor/hooks/` directory alone.
  4. In `AGENTS.md`: delete the "Stop hook that requests the docs pass" row; in the "Agent config layout" row, drop the sentences about the stop script and Claude Code hooks, keeping the symlink facts; in "Working conventions", drop the sentence that says the stop hook runs the same check. Keep the pointer to `.cursor/rules/docs-before-summary.mdc` and the improve-repo-docs skill. The docs pass is now by hand.
  5. Make the same removals in `README.md` wherever it names `docs-before-stop.py` or "stop hook".
  6. In `p6-grok-run.md` G11, change the **Done when** clause that requires the script to print `{}` to: "`python3 .cursor/skills/improve-repo-docs/scripts/diff_hash.py` matches `.cursor/docs-ledger.json`". Do not change G11's done mark.
  7. In `.cursor/rules/docs-before-summary.mdc`, delete the sentence "The stop hook repeats this check if the ledger hash was not updated."
  8. `CLAUDE.md` is a link to `AGENTS.md`. Do not edit it.
- **If the user chose "restore" in U2 instead:** stop and report. A loop-safe rewrite of the script is a separate task for the user to write up.
- **Done when:**
  - `python3 -c 'import json; json.load(open(".claude/settings.json")); json.load(open(".cursor/hooks.json"))'` exits 0.
  - `grep -rn 'docs-before-stop' --include='*.md' --include='*.mdc' --include='*.json' . | grep -v '^./references/' | grep -v '^./docs/plans/2026-11-02-tree-review/'` prints nothing.
  - `tools/mesh.sh check --fast` passes.
- **Stop if:** `.claude/settings.json` holds anything besides the Stop hook that you would have to change. Report it.
