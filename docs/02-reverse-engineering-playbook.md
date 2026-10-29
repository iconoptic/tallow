# Reverse Engineering Playbook (Cheap Smart Lights)

Use this workflow on devices **you own**. Goal: recover the local BLE GATT or LAN command format so a self-hosted driver can replace the vendor app and cloud.

## Prerequisites

Install host tools (Fedora) once:

```bash
./tools/install_re_toolchain_fedora.sh
```

That provisions JDK 21, Ghidra, the pinned jadx-headless-mcp JAR, unblob, Frida, mitmproxy, apktool, etc. Cursor loads REA via [`.cursor/mcp.json`](../.cursor/mcp.json). Prefer **REA MCP tools** when available; otherwise:

```bash
npx -y rea-agents@6.7.0 <command>
# or, after global install: rea <command>
```

Clone REA docs with `./tools/clone_references.sh` → `references/re-tooling/rea/docs/`.

## Decision tree

1. Scan / fingerprint → match [known protocols](03-known-light-protocols-matrix.md)?  
   → Adapt an existing driver from `references/protocols-ble/` or `references/protocols-wifi/`.
2. Unknown BLE → APK static analysis + HCI capture (+ Frida if encrypted).  
3. Unknown Wi-Fi → port scan + local key extract; or [firmware liberation](04-firmware-liberation-and-network-isolation.md).

## Phase 0 — Inventory and fingerprint

### BLE

```bash
# venv with bleak recommended
python tools/ble_discover.py --scan-seconds 15 --connect-gatt
```

Record:

- Advertised name (`ELK-BLEDOM`, `LEDnetWF…`, `LEDDMX-*`, `GATT-DEMO`, …)
- MAC / OUI
- Service and characteristic UUIDs + handles
- Properties (`write`, `write-without-response`, `notify`)

### Wi-Fi

```bash
nmap -sU -sT -p 6666-6668,5577,9999,4001-4003,80,443 <device-ip>
```

| Open ports (typical) | Likely stack |
| --- | --- |
| TCP/UDP 6666–6668 | Tuya local |
| TCP 5577 | MagicHome / Flux LED |
| TCP 9999 | older Kasa |
| UDP 4001–4003 | Govee LAN |
| HTTP 80 + MQTT-ish | OpenBeken / ESPHome / WLED already |

## Phase 1 — Prefer known libraries

Before deep RE:

| Fingerprint | Start here |
| --- | --- |
| Triones / LEDBLE / HappyLighting names | `references/protocols-ble/led-ble` |
| `ELK-BLEDOM` / Lotus Lantern / duoCo | `references/protocols-ble/elkbledom` |
| `LEDnetWF` / Zengge | `references/protocols-ble/lednetwf_ble` |
| Surplife / MagicHome2 BLE | `references/protocols-ble/magichome2-ble` |
| Tuya / Smart Life | `references/protocols-wifi/tinytuya` |
| MagicHome Wi-Fi | `references/protocols-wifi/flux_led` |
| Kasa / Tapo | `references/protocols-wifi/python-kasa` |
| Govee H7020 | `references/protocols-ble/ha-govee-led-ble`, `tools/govee_h7020.py`, [devices/govee-h7020.md](devices/govee-h7020.md) |
| Govee LAN | `references/protocols-wifi/govee2mqtt` |

## Phase 2 — Android APK static analysis (AI-assisted)

Most BLE lights ship an Android APK that builds GATT payloads in Java or a native `.so`.

### Tooling

| Tool | Role | Clone / install |
| --- | --- | --- |
| **REA** (primary) | MCP/CLI over JADX, Ghidra, ADB, Frida, tshark, firmware | `references/re-tooling/rea` + `tools/install_re_toolchain_fedora.sh` |
| JADX + JADX-AI-MCP | Fallback APK decompile MCP | `references/re-tooling/jadx-ai-mcp` |
| Ghidra + GhidraMCP | Fallback native / firmware MCP | `references/re-tooling/GhidraMCP` |
| apktool | Manifest, resources, smali (also via REA) | install script |
| Frida | Runtime hooks on `writeCharacteristic` | `frida-tools` via install script |

### Pulling the APK from a phone (REA)

```bash
npx -y rea-agents@6.7.0 list-adb-devices
npx -y rea-agents@6.7.0 resolve-adb-packages <serial> led
npx -y rea-agents@6.7.0 pull-adb-package <serial> com.example.app \
  --output-directory captures/
```

Manual equivalent:

```bash
adb shell pm list packages | grep -iE 'led|light|tuya|govee|lotus|magic'
adb shell pm path com.example.app
adb pull /data/app/.../base.apk ./captures/vendor.apk
```

Work only on APKs for apps controlling **your** devices.

### High-signal search targets (REA / JADX)

```bash
npx -y rea-agents@6.7.0 inspect-android-package captures/vendor.apk
npx -y rea-agents@6.7.0 search-android-classes captures/vendor.apk BluetoothGatt
npx -y rea-agents@6.7.0 search-android-classes captures/vendor.apk writeCharacteristic
npx -y rea-agents@6.7.0 inspect-android-method captures/vendor.apk \
  <pkg.Class> writeCharacteristic
npx -y rea-agents@6.7.0 trace-android-references captures/vendor.apk \
  <pkg.Class> --method-name writeCharacteristic
```

Also search for:

- `BluetoothGattCharacteristic`, hardcoded UUIDs (`fff3`, `ffe1`, `ff01`, `0000fff0`)
- Frame magic bytes: `0x7e`, `0x7b`, `0x56`, `0xbc`
- Crypto: `AES`, `Cipher.getInstance`, `SecretKeySpec`
- Package names from the Play Store listing of the vendor app

Native `.so` / firmware blobs → REA Ghidra provider (set `GHIDRA_INSTALL_DIR` / JDK 21 via install script).

## Phase 3 — Capture live BLE traffic

Static analysis gives candidates; captures confirm byte sequences.

### A. Android HCI snoop

1. Developer options → **Enable Bluetooth HCI snoop log**.
2. Force-stop the vendor app, toggle Bluetooth, reproduce: power, red, green, blue, brightness, one effect.
3. Pull the log (path varies by OEM):

```bash
adb bugreport bugreport.zip
# or
adb pull /data/misc/bluetooth/logs/btsnoop_hci.log
```

4. Parse writes — **quick path** (this repo):

```bash
python tools/parse_btsnoop.py btsnoop_hci.log --hex
```

**Full dissection** (REA + TShark; Bluetooth HCI / btsnoop supported):

```bash
npx -y rea-agents@6.7.0 inspect-packet-capture btsnoop_hci.log \
  --fields btatt.handle --fields btatt.value \
  --display_filter 'btatt.opcode == 0x52'
```

Open the same file in Wireshark: filter `btatt` or `btatt.opcode == 0x52` (Write Command).

### B. Linux `btmon` while phone controls the light

```bash
sudo btmon -w capture.cfa
# reproduce actions on phone, then Ctrl-C
wireshark capture.cfa
```

Only works if the sniffer / adapter can see the link (same room; phone↔light connection). For encrypted LE Secure Connections you may need a Nordic nRF52840 sniffer dongle + Wireshark plugin.

### C. Active MITM (optional)

Connect with `bleak` or nRF Connect, enable notifications, and bang candidate frames from APK analysis. Many strips use **Write Without Response** and never notify — treat control as optimistic.

## Phase 4 — Dynamic instrumentation (Frida)

When payloads are built in native code or encrypted just before write. Use Frida only on a **rooted/test** device you control. Prefer plaintext GATT dumps when the APK does not encrypt.

Conceptual hook (adjust class names from JADX / REA):

```javascript
// captures/gatt_setvalue_hook.js
Java.perform(function () {
  var Char = Java.use("android.bluetooth.BluetoothGattCharacteristic");
  Char.setValue.overload("[B").implementation = function (value) {
    var hex = Array.prototype.map.call(value, function (b) {
      return ("0" + (b & 0xff).toString(16)).slice(-2);
    }).join(" ");
    console.log("setValue: " + hex);
    return this.setValue(value);
  };
});
```

Run via REA (MCP: `start_frida_session` / `load_frida_script`, or CLI):

```bash
npx -y rea-agents@6.7.0 instrument-with-frida \
  --script-file captures/gatt_setvalue_hook.js \
  # plus device / package selection per rea --help / docs
```

See `references/re-tooling/rea/docs/frida-instrumentation.md`.

## Phase 5 — Wi-Fi protocol RE

1. Put device and laptop on an isolated VLAN (see [doc 04](04-firmware-liberation-and-network-isolation.md)).
2. Capture with Wireshark / `tcpdump` while the official app runs on the phone.
3. Summarize a saved capture with REA:

```bash
npx -y rea-agents@6.7.0 inspect-pcap-capture captures/session.pcapng --json
```

4. For HTTPS apps, use `mitmproxy` with user CA installed on the phone (often certificate-pinned → Frida SSL unpin or skip to LAN plaintext protocol). REA ships `references/re-tooling/rea/bridge/mitmproxy/capture.py` as a helper.
5. Prefer **local LAN** APIs already documented in tinytuya / flux_led / kasa / govee2mqtt over reinventing cloud APIs.

Tuya local keys: use TinyTuya wizard after pairing once with Smart Life (keys are extracted for LAN use; then block WAN).

## Phase 6 — Turn findings into a driver

Minimum viable driver:

1. Connect (BLE address or LAN IP + credentials).
2. Power on/off frame(s).
3. Set RGB (document color order: RGB vs RBG vs GRB).
4. Brightness.
5. Optional: effect ID + speed.

Store protocol notes under `docs/devices/<slug>.md` (future phase) with:

- Advertisement name / OUI
- Service / char UUIDs
- Frame layout table
- Sample hex for R/G/B/off/on
- Quirks (single connection, must sync clock, rate limits)

Implement against `bleak` first; later wrap with `habluetooth` / `bleak-esphome` for production (see [doc 05](05-target-webapp-architecture.md)).

## Phase 7 — When RE is not worth it

If the radio is Beken `BK7231T`/`BK7231N` (or other OpenBeken targets), prefer **flashing open firmware** ([doc 04](04-firmware-liberation-and-network-isolation.md)) over maintaining a fragile proprietary protocol.

Before flashing, optionally inspect a UART/SPI dump:

```bash
npx -y rea-agents@6.7.0 inspect-firmware-regions captures/bk7231_dump.bin
npx -y rea-agents@6.7.0 extract-firmware captures/bk7231_dump.bin captures/fw-out/
```

## Agent-oriented tips

- Prefer **REA** (`.cursor/mcp.json`) so Cursor can query JADX/Ghidra/ADB/tshark without pasting megabytes of decompilation into chat. Fall back to `references/re-tooling/jadx-ai-mcp` / `GhidraMCP` only if REA is unavailable.
- Keep captures, APKs, and exported REA evidence **out of git** (`captures/`, `rea-evidence/`, and artifact globs are gitignored).
- One change at a time when fuzzing writes: random multi-byte spam bricks some controllers until power cycle.
- Close the vendor app before attaching your own client (exclusive connection).

## Related

- [03 Known light protocols matrix](03-known-light-protocols-matrix.md)
- [04 Firmware liberation and network isolation](04-firmware-liberation-and-network-isolation.md)
- Awesome AI RE index (external): [DiscoverBox/awesome-ai-reverse](https://github.com/DiscoverBox/awesome-ai-reverse)
