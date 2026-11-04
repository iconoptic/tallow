# Phase 5: retire the old stack

Archived 2026-11-01 on the user's word, overruling T3.10 and T4.5: the legacy files were moved to `archive/` (same relative paths) instead of deleted, and `archive/` is to be deleted later. `firmware/cam/secrets.yaml` now holds the cam's Wi-Fi settings. T5.3 is not done: docs 01, 04, 05 and `docs/devices/` still describe the ESPHome and Python stack in places.

Deletion only. Every task here needs the user's explicit say-so in that session. Git history keeps everything.

## T5.1 Retire ESPHome

- **Tier:** Grok. **Needs:** T4.5, and the user's word.
- **Do:** Remove `firmware/esphome/`, `tools/esphome.sh`, `tools/node_check.py`, and `aioesphomeapi` from `tools/requirements.txt`. Move `secrets.yaml`'s Wi-Fi settings to the cam's own gitignored file. Update `docs/01`, `docs/04`, `docs/devices/`.
- **Done when:** `tools/mesh.sh check` passes, and `grep -ri esphome` finds only dated history.
- **Stop if:** a board still runs an ESPHome image.

## T5.2 Retire the Python panel

- **Tier:** Grok. **Needs:** T3.10, and the user's word.
- **Do:** Remove `app/*.py`, `tools/panel_broker.py`, `tools/panel_token.py`, `tools/govee_h7020.py`, and `tools/mesh_host.py`, with their tests. Keep `app/*.html` and `app/panel.css`, or move them to `host/`. Keep `tools/tests/panel_contract.py` and the page harnesses. Trim `tools/requirements.txt` to what the RE helpers need.
- **Done when:** `tools/mesh.sh check` passes, `panel_contract.py` passes against `tallowd`, and the page harnesses pass.
- **Stop if:** a kept tool still imports a removed file.

## T5.3 Final docs

- **Tier:** Grok. **Needs:** T5.1, T5.2.
- **Do:** Update `README.md`, `AGENTS.md`, and `docs/05` to describe the system as it now is, and mark this plan finished. `docs/05` still names FastAPI, uvicorn, `habluetooth`, `bleak`, `bleak-esphome`, ESPHome proxies, and a `smartcrap` service. The `docs/01` TL;DR and the README's hardware stance need the same rewrite. Change the `AGENTS.md` convention "Prefer wrapping upstream libraries" to the decision on other lights in the main plan.
- **Also fold in the still-open doc fixes from** [the 2026-10-20 review](../2026-10-20-review-remediation.md) **workstream B**, except B17. B17's tagged-VLAN rewrite is not the house network: isolation is the host access point `tallow-iot` in [docs/04](../../04-firmware-liberation-and-network-isolation.md), with no default route and no NAT. Leave that VLAN rewrite unwritten. The leftovers that are still in the docs are `hciconfig` in `docs/01`, `smartcrap` in `docs/05`, and `led-hue-cycle` in `docs/03`, plus the other B items (B1–B16 and B18–B27) that those files still show. Do not do them inside the 2026-11-02 tree-review docs task.
