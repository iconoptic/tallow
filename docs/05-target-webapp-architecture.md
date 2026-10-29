# Target Web App Architecture

Blueprint for the future **universal, self-hosted smart-light controller**: a LAN-only web UI that drives every owned device through Bluetooth and/or Wi-Fi without vendor clouds.

This document is the design for the full controller. The H7020 LAN page in `app/h7020_panel.py` is the phase 3 slice: one string, catalogue scenes, bulb paints, saved sequences, host schedules, and a host-only agent socket. Inventory, groups, and proxies are still ahead.

## Goals

- One UI from phone / laptop / tablet on the trusted LAN.
- Drivers for BLE GATT and local Wi-Fi protocols behind one API.
- Secure by default: bind to LAN/VPN, auth, no cloud dependency.
- Scale from one USB dongle to many ESP32 Bluetooth proxies.
- Add a new light family by dropping a driver module + inventory entry — not by forking the UI.

## Non-goals (for v1)

- Being a full Home Assistant replacement (HA can still be optional later via MQTT).
- Supporting every SKU on earth — only devices you own (inventory-driven).
- Browser-side Web Bluetooth (range, OS support, and multi-user suck; server owns radios).

## Logical architecture

```
┌─────────────────────────────────────────────────────────┐
│  Browser (any LAN device)                               │
│  REST for CRUD / scenes · WebSocket for live state      │
└────────────────────────────┬────────────────────────────┘
                             │
┌────────────────────────────▼────────────────────────────┐
│  FastAPI application                                    │
│  · Auth (session / token)                               │
│  · Device registry & rooms                              │
│  · Scene / group engine                                 │
│  · Driver router                                        │
└───────────────┬─────────────────────────┬───────────────┘
                │                         │
     ┌──────────▼──────────┐   ┌──────────▼──────────┐
     │ BLE path            │   │ LAN path            │
     │ habluetooth manager │   │ asyncio UDP/TCP     │
     │ bleak clients       │   │ tinytuya / flux_led │
     │ bleak-esphome proxy │   │ kasa / govee / HTTP │
     └──────────┬──────────┘   └──────────┬──────────┘
                │                         │
         USB HCI / ESP32             IoT VLAN lights
```

## Driver plugin interface

Sketch (Python typing for clarity):

```python
from typing import Protocol, runtime_checkable

@runtime_checkable
class LightDriver(Protocol):
    id: str
    name: str
    capabilities: set[str]  # {"onoff","rgb","cct","brightness","effect"}

    async def connect(self) -> None: ...
    async def disconnect(self) -> None: ...
    async def get_state(self) -> dict: ...
    async def set_power(self, on: bool) -> None: ...
    async def set_rgb(self, r: int, g: int, b: int) -> None: ...
    async def set_brightness(self, level: int) -> None: ...  # 0-255 or 0-100 — pick one and document
    async def set_effect(self, effect_id: str, speed: int | None = None) -> None: ...
```

### Registry

- Map `device_type` → driver factory (e.g. `elk_bledom`, `led_ble`, `tuya_lan`, `flux_led`, `openbeken_http`).
- Inventory YAML/JSON: `id`, `type`, `address` (MAC or IP), `credentials`, `proxy_hint`, `color_order`, `room`.
- Capabilities declared by driver; UI hides unsupported controls.

### Implementation sources

Wrap battle-tested libraries from `references/` rather than re-encoding frames:

| Type | Library |
| --- | --- |
| Generic Triones-like BLE | `led-ble` |
| ELK-BLEDOM family | logic from `elkbledom` |
| LEDnetWF | `lednetwf_ble` |
| MagicHome2 BLE | `magichome2-ble` |
| Tuya LAN | `tinytuya` |
| MagicHome Wi-Fi | `flux_led` |
| Kasa/Tapo | `python-kasa` |
| Govee LAN | port patterns from `govee2mqtt` or call its MQTT bridge |
| Govee H7020 | `app/h7020_panel.py` on the house LAN. Frames from `tools/govee_h7020.py`. See [devices/govee-h7020.md](devices/govee-h7020.md) |
| OpenBeken / ESPHome | HTTP / native API / MQTT |

## BLE connection management

Use **habluetooth** concepts even if you do not embed Home Assistant:

- Multiple scanners (local adapters + ESPHome proxies via **bleak-esphome**).
- Connection slot accounting and RSSI-based path selection.
- Failover when a proxy dies.
- Serialize commands per device (cheap controllers hate overlapping writes).

Operational rules:

- One GATT connection per light; disconnect idle clients after a timeout to free slots. The running H7020 page keeps its link up ([devices/govee-h7020.md](devices/govee-h7020.md)).
- Cache GATT services where the stack allows.
- Prefer Write Without Response when that is what the stock app used.
- Exponential backoff on disconnect storms.

## LAN engine

- Async discovery workers per protocol (Tuya broadcast, Govee multicast, mDNS for ESPHome).
- Per-device lock + timeout.
- Watchdog: mark unavailable when pings fail; do not block the event loop.

## API surface (proposed)

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/devices` | List inventory + state snapshot |
| GET | `/api/devices/{id}` | Detail |
| POST | `/api/devices/{id}/power` | `{ "on": true }` |
| POST | `/api/devices/{id}/color` | `{ "r","g","b" }` |
| POST | `/api/devices/{id}/brightness` | `{ "level": 0-255 }` |
| POST | `/api/devices/{id}/effect` | `{ "id","speed?" }` |
| POST | `/api/groups/{id}/…` | Fan-out |
| WS | `/ws` | State diffs, discovery events |

Keep payloads boring JSON. Push state on every successful apply and on LAN notify/poll.

The H7020 page is the flat version of this for one device, id `h7020`. House-LAN HTTP uses the password cookie. A unix socket at `data/panel/agent.sock` (mode `0600`) accepts scoped bearer tokens for a tool running on this host. The libvirt guest that runs Hermes is not given a path to that socket or to `10.87.0.0/24`. `GET /api/manifest` on the socket lists the calls. Scenes, paints, patterns, and local schedules are in [devices/govee-h7020.md](devices/govee-h7020.md). Groups, WebSocket, and the inventory routes above are still ahead.

## Frontend

- SPA or server-rendered + light JS — rooms, device cards, color picker, effect select, group scenes.
- Optimistic UI for BLE devices without notify.
- No Web Bluetooth required.

## Security

- Bind to `LAN` or WireGuard interface only (`uvicorn --host 10.x.x.x`), never expose to WAN.
- Auth: at least password session or reverse-proxy SSO (Authelia / oauth2-proxy).
- Separate IoT VLAN; server is the only host that speaks to lights ([doc 04](04-firmware-liberation-and-network-isolation.md)).
- Store Tuya local keys and Govee credentials in a secrets file / env, not in the repo.
- Rate-limit write endpoints to avoid melting BLE radios.

## Deployment sketch

```yaml
# illustrative only
services:
  smartcrap:
    build: .
    network_mode: host   # simplest for BLE + multicast; harden later
    cap_add: [NET_ADMIN, NET_RAW]
    volumes:
      - /run/dbus:/run/dbus:ro
      - ./data:/data
    environment:
      - INVENTORY=/data/inventory.yaml
```

For pure LAN Wi-Fi control (no local BLE), bridge networking is fine. BLE wants host D-Bus or a dedicated BLE sidecar.

## Phased delivery

1. **Inventory + discovery tools** (this repo’s `tools/` + docs) — identify owned devices.
2. **CLI drivers** per family — prove frames without UI.
3. **FastAPI + single-device UI** — the H7020 page in `app/h7020_panel.py`, including catalogue scenes, bulb paints, saved sequences, host schedules, and a host-only agent socket.
4. **habluetooth + proxies** — whole-house BLE.
5. **Groups / optional MQTT out** — niceties. Per-device scenes for the H7020 are already on that page.

## Related

- [01 Bluetooth hardware and proxies](01-bluetooth-server-hardware-and-proxies.md)
- [02 Reverse engineering playbook](02-reverse-engineering-playbook.md)
- [03 Known light protocols matrix](03-known-light-protocols-matrix.md)
- Agent handoff: [`../AGENTS.md`](../AGENTS.md)
