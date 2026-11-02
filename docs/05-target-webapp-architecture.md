# Target Web App Architecture

Blueprint for the future **universal, self-hosted smart-light controller**: a LAN-only web UI that drives every owned device through Bluetooth and/or Wi-Fi without vendor clouds.

This document is the design for the full controller. The LAN panel, `tallowd panel` in `host/tallowd/src/panel/`, is the phase 3 slice (the Python panel `app/h7020_panel.py` is archived in `archive/app/`). `/` is a menu with three entries: the H7020 page at `/lights` (one string, catalogue scenes, bulb paints, saved sequences, host schedules, and a host-only agent socket), the cameras app at `/cams`, and Settings at `/settings`. The cameras tile uses the inventory title, or Cameras when that field is missing. Camera inventory, the settings page, and logout are in [devices/keyestudio-mb0200-s3-cam.md](devices/keyestudio-mb0200-s3-cam.md#cams-page). Turning a schedule off stays on the light page ([devices/govee-h7020.md](devices/govee-h7020.md#lan-page)). The universal device inventory, groups, and proxies are still ahead. A Rust mesh of ESP32 nodes hears the house and writes `data/mesh/map.json` only while the host end runs. The Rust panel serves that map (`/devices`, `GET /api/mesh/map`). The map has node ids, and rooms when a `rooms.toml` is given ([doc 01](01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh)).

The [Rust conversion plan](plans/2026-10-31-rust-conversion.md) changes what this gets built with. The panel, the H7020 driver, the broker, and the mesh host end move to Rust in `host/`, on axum and `bluer`, and GATT at a distance goes over the mesh, not through ESPHome proxies. Where this design differs, the plan wins: FastAPI, uvicorn, `habluetooth`, `bleak`, and `bleak-esphome` below describe the Python target. It covers other lights too: new drivers are Rust crates, not wrapped Python, with `tinytuya`, `flux_led`, and the like kept as sources of test vectors. Only the H7020 is in the plan, so the library table under [Implementation sources](#implementation-sources) names what each crate is checked against, not what it wraps. Python was archived in phase 5 (`archive/`, [README](../archive/README.md)); T5.3 is not done, so the sections below still describe the Python target and have not been rewritten.

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
| Govee H7020 | `tallowd panel` on the house LAN. Frames from `host/tallow-govee`. See [devices/govee-h7020.md](devices/govee-h7020.md) |
| OpenBeken / ESPHome | HTTP / native API / MQTT |

## BLE connection management

This section is the target for control. `habluetooth` and `bleak-esphome` are not wired. The mesh in `firmware/mesh/` uses neither. It listens, reports over serial, and has no path to a light's GATT ([doc 01](01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh)). The boards have not paged nameless Classic devices since the 2026-10-31 flash ([flashed](01-bluetooth-server-hardware-and-proxies.md#flashed-2026-10-31-the-cross-check-build)). The GATT client over the mesh is built and not flashed.

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

- Async discovery workers per protocol (Tuya broadcast, Govee multicast). The host does not use mDNS on `tallow-iot`; ESPHome nodes set static addresses in `10.87.0.20`–`10.87.0.49` ([doc 04](04-firmware-liberation-and-network-isolation.md#esp32-nodes)).
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

The H7020 page is the flat version of this for one device, id `h7020`. House-LAN HTTP uses the password cookie. A unix socket at `data/panel/agent.sock` (mode `0600`) accepts scoped bearer tokens. The broker on this host holds the Hermes bearer and is the process that opens that socket. The libvirt guest is not given the socket file, the token, or `10.87.0.0/24`. Its container's added reach is `127.0.0.1:17620` on the guest, which dials the broker over vsock. `GET /api/manifest` on the socket lists the calls. Scenes, paints, patterns, and local schedules are in [devices/govee-h7020.md](devices/govee-h7020.md). Groups, WebSocket, and the inventory routes above are still ahead.

The menu at `/` is the first step toward one panel with several apps. Each app is a page behind the same cookie; the agent socket sees none of them, and it does not see Settings or logout. Cams are the first app that is not a light. The browser asks the panel, and the panel dials the cam on `10.87.0.0/24` once and shares that one connection among every screen watching, because the cam's web server holds one connection at a time and jams for minutes if hammered (`host/tallowd/src/panel/`; the original is `archive/app/cams.py`). The inventory file is re-read when it changes. Settings writes that file, including the cameras name, and the catalog sync writes catalog cams into the same file. `GET /api/cams` still omits addresses. Inventory, the settings API, the catalog sync, routes, limits, and page behaviour: [devices/keyestudio-mb0200-s3-cam.md](devices/keyestudio-mb0200-s3-cam.md#cams-page).

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
3. **Single-device UI** — the panel, `tallowd panel` in `host/tallowd/src/panel/` (the FastAPI original is in `archive/app/`): a menu, the cams, settings, and the H7020 page with catalogue scenes, bulb paints, saved sequences, host schedules, and a host-only agent socket.
4. **Whole-house radios**: hearing first. The Rust mesh in `firmware/mesh/` reports BLE and Classic devices to the host end, proven on two boards over one physical hop. The host end is `tallowd run` in `host/` (the Python `mesh_host.py` is archived). Control is not built, and the map is in the Rust panel ([doc 01](01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh)). ESPHome firmware is retired to `archive/firmware/esphome/`; one MB0200 ran an earlier image as a relay and as a cam on 2026-10-28. Nothing on the host consumes an ESPHome node yet, and `habluetooth` is not wired ([doc 01](01-bluetooth-server-hardware-and-proxies.md#tallow-nodes)).
5. **Groups / optional MQTT out** — niceties. Per-device scenes for the H7020 are already on that page.

The Rust conversion has its own [phases, 0 to 5](plans/2026-10-31-rust-conversion.md#phases). "Phase 3" in this document means item 3 above.

## Related

- [01 Bluetooth hardware and proxies](01-bluetooth-server-hardware-and-proxies.md)
- [02 Reverse engineering playbook](02-reverse-engineering-playbook.md)
- [03 Known light protocols matrix](03-known-light-protocols-matrix.md)
- [Rust conversion plan](plans/2026-10-31-rust-conversion.md)
- Agent handoff: [`../AGENTS.md`](../AGENTS.md)
