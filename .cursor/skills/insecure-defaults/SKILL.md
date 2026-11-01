---
name: insecure-defaults
description: Detect fail-open insecure defaults, hardcoded secrets, weak auth, and permissive settings that let an app run without its real configuration. Use when auditing security, reviewing config, or checking environment-variable handling. Trail of Bits insecure-defaults plugin, Cursor entry point.
---

# Insecure defaults

Upstream `trailofbits/skills` at `2be6433b39e009cd2b6fd3be1895f6b1faa5af9a` ships this as a Claude plugin with no `SKILL.md`. The unmodified tree is `.cursor/plugins/insecure-defaults` (CC-BY-SA-4.0, `.cursor/plugins/LICENSE`).

Cursor has no Workflow tool and no `CLAUDE_PLUGIN_ROOT`. Do not execute `workflows/audit.js`. Do not invoke `insecure-defaults:audit-pipeline`.

## Run

1. Set the plugin root to `.cursor/plugins/insecure-defaults`.
2. List `references/*.md`. If that list is empty, stop and say so. Do not guess patterns.
3. Read `README.md` and `commands/audit.md` in the plugin root. The README is the procedure: one sweep per category, then a refuting pass, then a report.
4. For each `references/<id>.md`, read the paired `references/<id>.json` and sweep only that category. Use the Task tool, subagent type `generalPurpose`, one category per worker.
5. Refute each candidate the way the README specifies. An incomplete trace is refuted. A missing env var does not refute an unconditional insecure value.
6. Write the report. State coverage. `no-candidates` is a real result and is not proof of absence.

## This repo

A missing `firmware/mesh/mesh.key` is an open mesh join. Flag that pattern when the code under review accepts a build with no key, a default key, or an unauthenticated frame. Do not open the key file.

Never read or quote `firmware/mesh/mesh.key`, `firmware/mesh/ota.key`, `secrets/`, or `data/firmware-backups/`.
