---
name: improve-repo-docs
description: Reviews tallow docs against the working tree, the whole repo, or named items, and records a diff hash plus a progress summary. Use when asked to improve or expand repo documentation, or when a docs-before-summary follow-up starts.
disable-model-invocation: true
---

# Improve repo docs

Reconcile tallow's docs with the repo. Edit the home for each fact, then link to it from the indexes. Leave a ledger the next agent can read.

When a parent agent launches you as the pre-summary subagent, run **working-tree** mode and stop. Do not start other work.

## Modes

- **working-tree** (default): the uncommitted diff. Also the mode the rule and the stop hook request.
- **review-all**: the whole doc set against the repo. Use when the request says to review all documentation.
- **specified**: only the paths or topics the request names.

## Ledger

`.cursor/docs-ledger.json` is gitignored. Schema:

```json
{
  "diff_hash": "<sha256 from diff_hash.py>",
  "summary": "What was checked, which docs changed, what remains.",
  "mode": "working-tree",
  "checked_at": "YYYY-MM-DDTHH:MM:SSZ"
}
```

`summary` is a few sentences for the next agent. `checked_at` is UTC (`date -u +%Y-%m-%dT%H:%M:%SZ`).

Write the ledger last. Do not edit any other file after it.

Working-tree and review-all set `diff_hash` from a hash computed **after** the doc edits. Specified mode copies the existing `diff_hash` through unchanged, or omits it when no ledger exists yet, so a partial review does not mark the whole tree reconciled.

Hash command (the hook uses the same script):

```bash
python3 .cursor/skills/improve-repo-docs/scripts/diff_hash.py --json
```

The script hashes `git diff HEAD` and the bytes of untracked, non-ignored files. The ledger path is excluded.

## Working-tree procedure

1. Read `.cursor/docs-ledger.json` when it exists. Use `summary` as the previous pass.
2. Run the hash command.
3. When `clean` is true, stop. Do not write the ledger.
4. When `diff_hash` equals the ledger, stop. Do not edit.
5. Inspect `git diff HEAD` and `git ls-files --others --exclude-standard`. For `secrets/`, captures, APKs, firmware binaries, and local keys, record the path only. Do not copy their contents into docs.
6. Update the home doc for each change that makes a doc stale or incomplete. Expand a section when the change introduces behavior that has no home.
7. When the docs are already accurate, leave them unchanged.
8. Run the hash command again.
9. Write the ledger with that hash, `mode` `working-tree`, and the summary.

## Review all

Skip the hash-match shortcut in steps 3–4. Read each home below and compare it to the repo. Fix drift and fill gaps that have a real home. Then run the hash command and write the ledger with `mode` `review-all` and the post-edit `diff_hash`.

## Specified items

Review only the named paths or topics. Update `summary`, `mode` (`specified`), and `checked_at`. Keep the previous `diff_hash`.

## Where facts live

Edit the home file. Link from the others.

- `README.md` — quick start, repository map, reference catalog
- `AGENTS.md` — agent workflows and the "where to look" table
- `docs/01-bluetooth-server-hardware-and-proxies.md` — USB adapters and ESP32 BLE proxies
- `docs/02-reverse-engineering-playbook.md` — REA, btsnoop, and btmon workflow
- `docs/03-known-light-protocols-matrix.md` — protocol fingerprints
- `docs/04-firmware-liberation-and-network-isolation.md` — IoT network boundary, CloudCutter, OpenBeken
- `docs/05-target-webapp-architecture.md` — FastAPI app and driver shape
- `docs/devices/<slug>.md` — per-device frames, and only after a device is onboarded
- `docs/plans/` — dated plans
- `.cursor/rules/` — always-on constraints (host config stays a script under `tools/`; the IoT sandbox stays isolated)
- Script header — usage comment is the command source of truth; the README quick start must match it

When a tool or doc is added, update the README repository map and the AGENTS.md table in the same pass.

Keep each numbered doc on its topic. Add a new numbered doc only when none of these is the home.

## Edit bar

Match the repo's voice: short, concrete, command-first.

- A one-line fix does not grow a new document.
- Record commands that exist in the tree. Do not invent devices, protocols, or flags.
- One fact lives in one file. Indexes link to it.
- Host configuration stays a script the user runs. The IoT whitelist and the no-route boundary stay as written in the rules and `docs/04`.
- Leave gitignored captures, APKs, firmware, and keys out of the docs.
