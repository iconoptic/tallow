# Known Light Protocols Matrix

Fingerprinting cheat sheet for cheap smart lights. Exact frames vary by clone firmware; treat this as a starting index into `references/`.

## BLE families

### ELK-BLEDOM / Lotus Lantern / duoCo / MELK

| Field | Typical value |
| --- | --- |
| Advertised names | `ELK-BLEDOM`, `ELK-BLEDOB`, `MELK-*`, `LEDBLE`, … |
| Write UUID | `0000fff3-0000-1000-8000-00805f9b34fb` (or `ffe1`) |
| Notify/read | `0000fff4-…` / `ffe2` |
| Frame shape | Often `7E … EF` (length and checksum vary by model) |
| Apps | Lotus Lantern, Lotus Lamp X, duoCo Strip, Happy Lighting |
| Reference | `references/protocols-ble/elkbledom` |

Common capability set: power, RGB, brightness, built-in effects, sometimes mic modes. Many models are **optimistic** (no reliable state notify).

### Triones / LEDBLE / HappyLighting / Magic Blue (led-ble)

| Field | Typical value |
| --- | --- |
| Advertised names | `LEDnet` (not LEDnetWF), `BLE-LED`, `LEDBLE`, `Triones`, `LEDBlue`, `Dream`, `QHM`, `AP` |
| Brands | Happy Lighting, Magic Blue, SUPERNIGHT, Zengge (non-WF), … |
| Color frames | Often `56 RR GG BB … AA` style (confirm per model) |
| Reference | `references/protocols-ble/led-ble` |
| HA integration | `led_ble` |

### Zengge LEDnetWF / MagicHome2 BLE / Surplife

| Field | Typical value |
| --- | --- |
| Advertised names | `LEDnetWF…` + MAC suffix |
| Service | Often `0xFFFF` |
| Write / notify | `0xFF01` / `0xFF02` |
| Header | Shared Zengge-style 8-byte header (`cmd_family=0x0a` family appears in MagicHome2 docs) |
| References | `references/protocols-ble/lednetwf_ble`, `references/protocols-ble/magichome2-ble` |

Supports scenes/effects, HSB color, music modes on some hardware. Phone app and custom client fight over the single BLE connection.

### LEDDMX / “LED LAMP” (Zhongji)

| Field | Typical value |
| --- | --- |
| Advertised names | `LEDDMX-*` |
| Service / write | `0000ffe0-…` / `0000ffe1-…` |
| Frame | 9 bytes `7B … BF` |
| Examples | Static colour `7B FF 07 C1 C2 C3 00 FF BF` (color order may be RBG) |
| Notes | Windows Bleak GATT discovery often fails; Linux BlueZ or Bumble-by-handle works |

### MR Star / GATT-DEMO

| Field | Typical value |
| --- | --- |
| Advertised name | `GATT-DEMO` |
| App | MR Star (`com.frok.mrstar`) |
| Write char | `0000fff3-…` |
| Frame | `BC <cmd> <len> <data…> 55` (HSV colors) |
| External example | [labsensacional/mrstar-ble-light](https://github.com/labsensacional/mrstar-ble-light) (not cloned; small single-vendor RE) |

### Govee BLE

| Field | Typical value |
| --- | --- |
| Transport | BLE |
| H7020 | [devices/govee-h7020.md](devices/govee-h7020.md). Probe: `tools/govee_h7020.py`. Page: `app/h7020_panel.py` |
| Reference | `references/protocols-ble/ha-govee-led-ble` tag `v7.14.0-rc.2.h7020` |
| Notes | One central. Close the phone app before the host connects. The LAN page holds that central while it runs. H7020 has no Govee LAN API |

## Wi-Fi families

### Tuya / Smart Life (local LAN)

| Field | Typical value |
| --- | --- |
| Ports | UDP discovery; TCP **6668** (older **6666/6667**) |
| Protocol versions | 3.1, 3.2, 3.3, 3.4, 3.5 (AES / session changes across versions) |
| Credentials | Device ID + **local key** (from paired cloud account / tinytuya) |
| DPS | JSON datapoints (e.g. 20 power, 24 colour, …) — model-specific |
| Reference | `references/protocols-wifi/tinytuya` |

After extracting local keys, move devices to a no-WAN VLAN ([doc 04](04-firmware-liberation-and-network-isolation.md)).

### MagicHome / Flux LED

| Field | Typical value |
| --- | --- |
| Port | TCP **5577** |
| Framing | Binary commands (RGB, CCT, patterns); some checksums |
| Dual radios | Some controllers also expose BLE |
| Reference | `references/protocols-wifi/flux_led` |

### TP-Link Kasa / Tapo

| Field | Typical value |
| --- | --- |
| Legacy Kasa | TCP **9999**, XOR “encryption” |
| Newer Tapo | KLAP / HTTP local auth |
| Reference | `references/protocols-wifi/python-kasa` |

### Govee LAN API

| Field | Typical value |
| --- | --- |
| Ports | UDP **4001** device scan, UDP **4003** device commands, UDP **4002** replies to the controller |
| Prerequisite | SKU on Govee's LAN list, then LAN Control enabled in the Govee app |
| Reference | `references/protocols-wifi/govee2mqtt` (LAN and cloud; no BLE driver) |
| Notes | H7020 is not on that list. See [devices/govee-h7020.md](devices/govee-h7020.md) |

## Quick fingerprint table

| Clue | Likely family | Next step |
| --- | --- | --- |
| Name contains `ELK-BLE` / `MELK` | ELK-BLEDOM | elkbledom |
| Name `LEDnetWF*` | Zengge WF | lednetwf_ble / magichome2-ble |
| Name `LEDnet*` without WF | led-ble set | led-ble |
| Name `LEDDMX-*` | Zhongji LEDDMX | custom / led-hue-cycle notes |
| Name `GATT-DEMO` | MR Star | mrstar protocol docs |
| TCP 5577 open | MagicHome | flux_led |
| TCP 6668 open | Tuya | tinytuya |
| Name `Govee_H7020*` | Govee H7020 | [devices/govee-h7020.md](devices/govee-h7020.md) |
| UDP 4001/4003 | Govee LAN (SKU on the LAN list) | govee2mqtt |
| AP `OpenBK*` / `kickstart-*` | Already liberated | OpenBeken / ESPHome |

## Color-order gotcha

Many controllers swap channels. Always calibrate with pure red, green, blue test frames and record order (`RGB`, `RBG`, `GRB`, …) in the device note.

## Exclusive connection gotcha

BLE LED controllers almost always accept **one** central. Discovery from the server will fail or flake while the phone app is connected.

## Related

- [02 Reverse engineering playbook](02-reverse-engineering-playbook.md)
- [05 Target webapp architecture](05-target-webapp-architecture.md)
