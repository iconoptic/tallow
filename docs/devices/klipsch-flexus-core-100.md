# Klipsch Flexus Core 100

Soundbar in this house. It speaks Bluetooth LE for the Klipsch Connect Plus app, and Classic Bluetooth for phone audio. It has no Wi-Fi, so the published Flexus drivers (Core 300 HTTP at `/api/getData` and `/api/setData`) have nowhere to connect. The panel has no soundbar route. This note is the 2026-11-02 GATT fingerprint, the vendor values, and the control tool. Volume works through standard LE Audio. Power, input, dialog, night, and EQ are not yet mapped.

## This unit

Read from the bar on 2026-11-02, host seed adapter `hci0` (TP-Link UB500, USB `2357:0604`). The Realtek combo (`hci1`) was powered off. The connect and the reads succeeded on the first try.

| Field | Value |
| --- | --- |
| Advertised name | `Klipsch Flexus Core 100` |
| GATT device name | `Klipsch Flexus Core 100` |
| BLE address | `DC:00:00:11:A0:38` |
| Classic address | `23:00:00:11:A0:38` |
| Manufacturer string | `Klipsch Group, Inc.` |
| Model | `1072051` |
| Hardware revision | `0.5.0` |
| Firmware revision | `1.2.3` |
| Software revision | `1.1.2` |
| RSSI at the scan | −74 dBm |

The Device Information strings other than the name are fixed-width and NUL-padded: manufacturer 40 bytes, model and the three revisions 20 bytes each. The serial string is readable the same way and is left out of this note. Firmware `1.2.3` is the floor Klipsch names for installer-mode IR. This read did not change that mode.

The mesh inquiry that first heard the Classic address is in [docs/01](../01-bluetooth-server-hardware-and-proxies.md). The map tests group the two addresses as one speaker.

## Advertisement

Company id `2042` (`0x07FA`), assigned to Klipsch Group, Inc. in the Bluetooth SIG company identifiers. One advertised service UUID, `000007fa-0000-1000-8000-00805f9b34fb`, the same number in the 16-bit form.

Manufacturer payload, 15 bytes:

```text
10 5b b3 01 00 00 00 00 23 00 00 11 a0 38 00
```

Bytes 8–13 are the Classic address. [The Three Plus](https://github.com/hbldh/bleak/issues/1491) advertises the same company id and the same 15-byte shape, with `b2` in the third byte. That speaker also exposes the `da6d0f*` services below. Nobody has published characteristic handles or command payloads for either product.

## GATT

Eighteen primary services. Nine are assigned LE Audio or GAP services. Nine are vendor services. Common Audio (`00001853`) listed no characteristics.

| Service | UUID | What was read |
| --- | --- | --- |
| Volume Control | `00001844-0000-1000-8000-00805f9b34fb` | Volume State `c2 00 50`, Volume Flags `01` |
| TMAP | `00001855-0000-1000-8000-00805f9b34fb` | Role `08 00` (unicast media receiver) |
| Coordinated Set | `00001846-0000-1000-8000-00805f9b34fb` | Size `01`, rank `01`, SIRK present with type byte `01` (encrypted). The SIRK bytes are left out of this note |
| PACS | `00001850-0000-1000-8000-00805f9b34fb` | Sink locations `03 00 00 00` (front left and front right). Source locations `01 00 00 00` (front left). Supported contexts `07 00 07 00` |
| Audio Stream Control | `0000184e-0000-1000-8000-00805f9b34fb` | Not read |
| Generic Access | `00001800-0000-1000-8000-00805f9b34fb` | Appearance `00 00`. Preferred connection interval 30–50 ms, latency 0, timeout 4 s (`18 00 28 00 00 00 90 01`) |

Volume State is the standard three-byte layout: setting `0xC2` (194 of 255), mute `0`, change counter `0x50`. Volume Flags `01` is the persisted-setting bit. The Volume Control Point (`00002b7e`, handle 68, write) was not written.

The vendor services use base `da6d0fxx-0d18-442c-babe-f85b5baa6f11` except two `e49a25f8` services. Handles are what BlueZ reported on this connect.

| Service UUID | Characteristics (short id, properties, handle) |
| --- | --- |
| `da6d0f01-…` | `02` r/w/n 137, `03` r/w/n 140, `04` r/w/n 143, `05` r/w/n 146, `09` r/w/n 149, `13` r/n 152, `14` r/w/n 155, `15` r/w/n 158, `16` r 161, `17` r/w 163, `18` r/n 165, `19` r 168, `20` r 170, `21` r/n 172, `22` r/n 175, `23` r/n 178, `24` r/w/n 181, `25` r/n 184, `26` r/w/n 187, `27` r/w/n 190, `2c` r/w/n 193 |
| `da6d0fa1-…` | `a2` r/w/n 113, `a3` r/w/n 116, `a4` r/w/n 119, `a5` w 122, `a6` w 124 |
| `da6d0fb1-…` | `b2` r/w/n 127, `b3` w/n 130, `b4` w/n 133 |
| `da6d0fc1-…` | `c2` n 93, `c3` r/n 96, `c4` r 99, `c5` n 101, `c6` w/n 104, `c7` w/n 107, `ca` r 110 |
| `da6d0fd1-…` | `d2` r/w/n 197, `d3` w/n 200, `d4` w/n 203, `d5` r/w/n 206, `d6` n 209, `d7` n 212, `d8` r/w 215, `d9` r 217 |
| `da6d0fe1-…` | `e3` w/n 220, `e4` r/n 223, `e5` r/w/n 226, `e6` r/w/n 229, `e7` r/w/n 232, `e8` r/w/n 235, `e9` r/w 238, `ea` r/w/n 240, `eb` r/w 243 |
| `da6d0f31-…` | `32` w/wnr 246, `33` r/wnr/n 248 |
| `e49a25f8-f69a-11e8-8eb2-f2801f1b9fd1` | `e49a25e0-…fd1` w/wnr 252, `e49a28e1-…fd1` n 254 |
| `e49a25f8-f69a-11e8-8eb2-f2801f1b9fd2` | `e49a25e0-…fd2` w/wnr 74, `e49a28e1-…fd2` n 76 |

`r` read, `w` write, `wnr` write-without-response, `n` notify. The same `e49a25f8-…fd1` pair shows up on watches that are not Klipsch, so that pair is a chip service sitting beside the `da6d0f*` map. The Three Plus public logs list `da6d0fc1`, `da6d0fa1`, `da6d0fb1`, `da6d0f01`, `da6d0fd1`, `da6d0fe1`, and one `e49a25f8-…fd1`. This bar adds `da6d0f31`, the second `e49a25f8` (`…fd2`), and the LE Audio services.

## Vendor values (2026-11-02, second read)

`tools/klipsch_ble.py snapshot` read every readable characteristic except the serial string and the SIRK. The output is gitignored under `data/klipsch/`. Several vendor values are plain text:

| Char | Value | Reading |
| --- | --- | --- |
| `fd9` | `["1","TV","2","BT","3","DGTL","5","USB",]` | Input table: id and label |
| `fd8` | `"BT"` | Current input label |
| `fd2` | `02 00` | Current input id (2 = BT). This is the most likely input-select write target. Not written |
| `fca` | `DSP,1.2.3,BT,1.1.2,HDMI,0.18.0` | Firmware per chip |
| `fe6` | `Klipsch Flexus Core 100`, NUL-padded | Device name. Writable, so it is probably the rename path |
| `f20` | JSON list with ids 0–3, each `"Con":"0"` | Wireless sub and surround slots. Nothing is paired |
| `fa4` | `<1><6><2><6>…<15><12>` | Id and level pairs. The meaning is unknown |
| `fc3`, `fc4` | `{"volume": 0,"input": to be do}`, `{"req": to be do}` | Firmware stubs |
| `f17` | `00 e8 03 07 00`, repeated for ids 0–6 | Seven records, each id, `0x03e8` (1000), `07`, `00` |

The one-byte values (`f02` `0a`, `f03` `0c`, `fea` `0a`, `f24` `02`, `f26` `03`, `f27` `01`, `fe8` `02`, and others) are probably EQ, dialog, night, and the power state. Which value is which needs `watch` while someone presses the bar's remote.

## Control

`tools/klipsch_ble.py` (bleak, adapter `hci0`):

- **Volume works now.** It uses the standard Volume Control Service: `volume`, `volume set <0-255>`, `up`, `down`, `mute`, `unmute`. Each write carries the change counter from the state just read. On 2026-11-02 `volume set 194`, the value the bar already had, was accepted, and the counter stayed at `0x50` because nothing changed. No audible change was written.
- **Mapping the vendor side.** With the bar on, run `watch --seconds 90` and press power, input, volume, dialog, and night on the IR remote. Each press prints the characteristic that notified. `snapshot` before and after a change, then `diff`, catches values that do not notify. Write a vendor value with `write <uuid> <hex>` only after the press has shown which value it takes. A blind write is never tried.
- **Power is unknown.** It is not yet known whether the bar's BLE stays up in standby, or whether a write can wake it. The watch with the power press answers both.

## Still open

A Connect Plus write capture (power, volume, input, dialog, night, EQ) parsed with `tools/parse_btsnoop.py`. No phone was attached on this pass, so `com.klipsch.connectxp` was not pulled. The control logic in that app is in `libapp.so`. The HTTP functions already recovered for the Core 300 are the Wi-Fi path.

The bar stays off `tallow-iot`. A later capture uses the host seed ([docs/01](../01-bluetooth-server-hardware-and-proxies.md#this-host)). The 2026-11-02 fingerprint above was read on the UB500.
