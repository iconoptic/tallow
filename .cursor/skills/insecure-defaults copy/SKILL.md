---
name: insecure-defaults
description: "Detects fail-open insecure defaults (hardcoded secrets, weak auth, permissive security) that allow apps to run insecurely in production. Use when auditing security, reviewing config management, or analyzing environment variable handling."
---

# Insecure Defaults Detection

Vendored from [trailofbits/skills](https://github.com/trailofbits/skills) commit `2be6433b39e009cd2b6fd3be1895f6b1faa5af9a`, plugin `insecure-defaults` 2.0.3. License: CC-BY-SA-4.0. The license text is `.agents/plugins/LICENSE`. Upstream files under `.agents/plugins/insecure-defaults/` are unmodified.

The skills CLI on that commit publishes no `SKILL.md` for this plugin. The audit command is `commands/audit.md`. It stops unless `${CLAUDE_PLUGIN_ROOT}/references` lists `*.md` files.

Cursor has no `CLAUDE_PLUGIN_ROOT` and no `Workflow` tool. Do not call `insecure-defaults:audit-pipeline`. Upstream says invoking that workflow directly stops immediately.

Plugin root: `.agents/plugins/insecure-defaults`

1. List `.agents/plugins/insecure-defaults/references`. It must list `*.md` files. If it does not, stop and say so. Do not search elsewhere, do not guess, and do not run the audit.
2. Read `commands/audit.md` and `README.md` in the plugin root for the phase order. Read category ids from `workflows/audit.js`. For each category, read only that category's `references/<id>.json` and `references/<id>.md`.
3. Run the phases in this session: one recon pass, one sweep per category, a refuting verifier per candidate, then a report. A verifier starts at refuted. Clear a candidate only when the trace reaches a security decision. An incomplete trace stays refuted.
4. If the corpus cannot be read, abort. Do not invent patterns.

Do not edit the plugin files.
