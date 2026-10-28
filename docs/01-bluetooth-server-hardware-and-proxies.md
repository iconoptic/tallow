# Bluetooth Server Hardware and Proxies

Goal: control cheap BLE smart lights from a self-hosted web app reachable from any device on the LAN. The browser never speaks Bluetooth; the **server** owns the radios.

## Recommendation (TL;DR)

| Scale | Approach |
| --- | --- |
| Few lights near the server | One high-quality USB BLE adapter on a **USB 2.0** extension, away from the machine |
| Whole-house / multi-floor | Distributed **ESP32 Bluetooth proxies** (Ethernet/PoE preferred) + optional local USB adapter |
| Many concurrent GATT connections | Proxies + multi-adapter HA stack (`habluetooth`); do not rely on one dongle’s 5–7 connection slots |

A “top tier” single dongle helps, but **proxies usually beat a single long-range radio** for reliability in a house.

## Transport stack used by this project

```
Web UI  →  FastAPI backend  →  habluetooth / bleak
                                      ├─ local BlueZ adapter(s)  →  BLE lights
                                      └─ bleak-esphome          →  ESP32 proxies  →  BLE lights
```

Reference clones:

- `references/ble-stack/bleak`
- `references/ble-stack/habluetooth`
- `references/ble-stack/bleak-esphome`
- `references/ble-stack/bumble` (raw HCI / ATT when BlueZ discovery is broken)

## Local USB adapters (Linux / BlueZ)

### Requirements

- BlueZ **≥ 5.63** strongly recommended (passive scanning / experimental features).
- Prefer **dbus-broker** and kernel **≥ 5.15.62**.
- Adapter must be usable from D-Bus (`bluetoothd`).

### High-performance class (Home Assistant Bluetooth docs)

**CSR8510A10**-based Class 1 sticks with external antenna are the most consistently recommended for active GATT:

- Feasycom **FSC-BP119** (CSR8510A10, external antenna) — HA Bluetooth maintainers’ primary test dongle
- Sena **UD100-G03** (CSR8510A10, external antenna)
- Many ORICO / UGREEN / Digitus CSR 4.0 sticks (same chip family)

Typical connection budget: about **5** simultaneous connections.

**Avoid:**

- Unbranded CSR clones with USB ID `0a12:0001` (kernel workarounds; often unrecoverable failures)
- Many Realtek RTL8761BU sticks as *primary* active-connection adapters (no reset pin; may need physical re-plug when they hang). Fine as secondary / long-range experiment.
- ATS2851 dongles still listed as unsupported in HA (missing Linux drivers for some USB IDs)

### Long-range / Coded PHY notes

BLE Long Range (LE Coded PHY) needs adapter + BlueZ + app support. Cheap LED controllers almost always use **1M PHY**, so Coded PHY rarely helps *light* control. Buy Class 1 + good antenna placement instead of chasing “BT 5.4 Long Range” marketing.

Verify adapter capabilities:

```bash
hciconfig -a
btmgmt -i hci0 info
btmgmt -i hci0 phy   # look for LECODEDTX / LECODEDRX if experimenting
```

### Placement and interference (critical)

2.4 GHz BLE is fragile next to USB 3.0 and Wi-Fi APs:

1. Plug the dongle into a **USB 2.0** port (or a powered USB 2.0 hub if the host only has USB 3.x).
2. Use a **shielded USB extension** and put the antenna near the middle of the coverage area, away from the server chassis and Wi-Fi APs.
3. Prefer vertical antenna orientation; try a few positions and measure RSSI with `tools/ble_discover.py`.
4. Keep multiple USB adapters physically separated if using more than one.

### Docker / container passthrough

If the controller runs in Docker:

```yaml
cap_add:
  - NET_ADMIN
  - NET_RAW
volumes:
  - /run/dbus:/run/dbus:ro
```

Without those caps, BlueZ management APIs fail and adapters cannot be reset when they wedge.

## Distributed Bluetooth proxies (preferred for houses)

An ESP32 (or Pico W) running ESPHome `bluetooth_proxy` scans (and optionally opens GATT connections) near the lights, then relays over Wi-Fi or Ethernet to the server. The Python side uses `bleak-esphome` so existing `bleak`-style drivers keep working.

### Proxy hardware preference (best → good)

1. **Ethernet / PoE ESP32** — e.g. Olimex `ESP32-POE-ISO-EA` (+ matching enclosure). Avoids Wi-Fi/BLE radio contention on the proxy itself.
2. **Wi-Fi ESP32** with external antenna — place one per zone; expect more interference than PoE.
3. Shelly Gen2+ / some SMLIGHT sticks — advertisement listening only (no active GATT on many models).

### ESPHome sketch outline

```yaml
esp32_ble_tracker:

bluetooth_proxy:
  active: true          # required for writing to lights
  # connection_slots: 3 # raise carefully; RAM limited (~5 max recommended on ESP32)
```

Performance tips from HA docs:

- Ethernet proxies with **passive** scanning outperform Wi-Fi proxies and local USB in many setups.
- Advertisement bundling (ESPHome ≥ 2023.6) reduces Wi-Fi chatter.
- Separate proxies spatially; overlapping radios fight each other.

### When to still buy a “top tier” dongle

- Server room has several BLE devices in the same room as the host.
- You need a reliable adapter for **RE work** (`btmon`, `bumble`, pairing experiments) without depending on a proxy.
- You want a fallback path if a proxy is offline.

For whole-house convenience of a LAN web app: **proxies first, dongle second**.

## Connection-slot reality

| Path | Approx. concurrent GATT connections |
| --- | --- |
| One CSR USB adapter | ~5 |
| One ESP32 proxy | ~3 default (configurable, RAM-limited) |
| N proxies + local adapters via `habluetooth` | Sum of healthy scanners; automatic failover |

Cheap LED controllers often allow **only one** central connection. Close the vendor phone app before the server connects.

## Quick hardware shopping shortlist

| Role | Example |
| --- | --- |
| Local RE / nearby lights | Feasycom FSC-BP119 or Sena UD100-G03 + USB 2.0 extension |
| House coverage | 2–4× ESP32-PoE (EA antenna) flashed as ESPHome Bluetooth proxy |
| Optional long-range USB experiment | EDUP EP-B3536 (RTL8761BUE, external antenna) — treat as secondary |

## Related docs

- [02 Reverse engineering playbook](02-reverse-engineering-playbook.md)
- [05 Target webapp architecture](05-target-webapp-architecture.md)
- Upstream: [Home Assistant Bluetooth](https://www.home-assistant.io/integrations/bluetooth/), [ESPHome bluetooth_proxy](https://esphome.io/components/bluetooth_proxy/)
