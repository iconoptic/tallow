---
name: rust-review-cursor
description: Run Trail of Bits rust-review from Cursor. Use when the user asks for a Rust security review, an unsafe/FFI/concurrency/async audit, or rust-review. Key memory belongs to zeroize-audit. Timing of crypto belongs to constant-time-analysis. Fail-open config belongs to insecure-defaults.
---

# Rust review on Cursor

Follow `.cursor/skills/rust-review/SKILL.md` for phases, arguments, and cluster selection. This file only replaces the Claude Code assumptions. Do not invent a cluster list. Do not restate their bug classes.

## Plugin root

Set `RUST_REVIEW_PLUGIN_ROOT` to `.cursor/plugins/rust-review` (repo path, not a home-directory install). Confirm both of these exist before any later phase:

- `.cursor/plugins/rust-review/prompts/clusters/unsafe-boundary.md`
- `.cursor/plugins/rust-review/scripts/build_run_plan.py`

If either is missing, stop. Do not search `~/.claude` or `~/.codex`.

Run their planner with `uv run --no-project` and the arguments the upstream skill gives. `uv` is required. If `uv` is not on `PATH`, stop and say so. Do not install it in the session.

## Workers

Cursor has no `rust-review:rust-review-worker`, `rust-review:rust-review-dedup-judge`, or `rust-review:rust-review-fp-judge` subagent. Spawn each worker and both judges with the Task tool, subagent type `generalPurpose`. Put the matching file from `.cursor/plugins/rust-review/agents/` into that prompt, then the planner's worker prompt. Tell those workers not to spawn further agents.

Judge order stays the upstream order: dedup (`agents/rust-review-dedup-judge.md`), then false-positive and severity (`agents/rust-review-fp-judge.md`).

## Defaults for this repo

Use these unless the user names others:

- Threat model: `BOTH`
- Severity: `high`
- Worker model: this session's model. Do not switch the run to haiku.
- Separate runs, in this order: `host/`, `firmware/mesh/core`, `firmware/mesh/node`
- Write results under `.rust-review-results/`. That directory is gitignored.

`unsafe` in `firmware/mesh/node/src/` is a C FFI seam (radio, GATT, OTA). Run the FFI and unsafe clusters. Treat those findings as advisory until someone checks the C contract. `host/` is the cleaner first scope.

## Secrets

Never read or quote `firmware/mesh/mesh.key`, `firmware/mesh/ota.key`, `secrets/`, or `data/firmware-backups/`. If a finding needs those bytes, name the path and stop.
