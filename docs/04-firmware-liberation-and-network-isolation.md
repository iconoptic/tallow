# Firmware Liberation and Network Isolation

Cheap Wi-Fi lights phone home. Two complementary defenses:

1. **Contain** stock firmware on an IoT VLAN with no WAN (keep local APIs).
2. **Replace** firmware when the SoC is supported (permanent cloud cut).

## Strategy pick

| Device class | Preferred path |
| --- | --- |
| Beken `BK7231T` / `BK7231N` with known CloudCutter profile | OTA flash OpenBeken or LibreTiny ESPHome Kickstart |
| ESP8266 / ESP32 based bulbs/strips | Serial or OTA → ESPHome / WLED |
| BL602 / LN882H / W800 (OpenBeken targets) | OpenBeken when supported |
| Unknown silicon, proprietary BLE-only | Protocol RE + no cloud (BLE never needed WAN) |
| Tuya Wi-Fi you cannot flash yet | TinyTuya local keys + hard WAN block |

## Network isolation (non-invasive)

### Layout

```
[Trusted LAN]  ←→  [Controller server]
                        │
                        │ allow: server → IoT (TCP/UDP needed by drivers)
                        ▼
                   [IoT VLAN / SSID]
                        │
                        ✗ deny: IoT → WAN
                        ✗ deny: IoT → Trusted (optional; or allow mDNS reply only)
```

### Firewall principles (nftables / OpenWrt / whatever you use)

- IoT subnet: DHCP + DNS + **local NTP** only toward infrastructure.
- **Drop** IoT → RFC1918 except the controller (and maybe DNS/NTP).
- **Drop** IoT → WAN.
- **Allow** controller → IoT on ports used by your drivers (Tuya 6668, MagicHome 5577, Govee 4001–4003, HTTP 80 for OpenBeken/ESPHome, etc.).
- Optional: DNS sinkhole for `*.tuya*`, `*.govee*`, vendor MQTT hosts so devices fail fast instead of retry-storming.

### Why local NTP / DNS matter

Many stock firmwares soft-brick features or reboot-loop if they cannot reach cloud *and* cannot sync time. Serving NTP on-LAN and resolving vendor hosts to `0.0.0.0` or a blackhole IP reduces flapping.

### BLE note

Pure BLE lights never needed the internet. Isolation is about **Wi-Fi** stock firmware and any dual-radio bridge that still opens sockets. The H7020 stays off this SSID ([devices/govee-h7020.md](devices/govee-h7020.md)).

The host Bluetooth seed is `tools/setup_iot_ble.sh`. It comes up only while this access point is up, and inbound pairing stays closed. The radio, the allow file, and the commands are in [docs/01](01-bluetooth-server-hardware-and-proxies.md).

## Host access point

`tools/setup_iot_ap.sh` brings up this machine's IoT SSID. Run it with sudo; re-running it does not rotate the passphrase.

One 2.4 GHz SSID on the Kootech adapter (USB `0bda:b812`). The default name is `tallow-iot`. WPA2-PSK, HT20, station isolation. The access point is `10.87.0.1/24`. DHCP offers no default route. DNS on this host answers every name with `0.0.0.0`. Chrony serves time to `10.87.0.0/24`. Firewall zone `tallow-iot` accepts DHCP, DNS, and NTP from that subnet and drops the rest. There is no masquerade and no port forward, so the LAN, Docker, and libvirt have no path in.

The accept list starts empty. The SSID is up, and nothing can associate until its MAC is allowed. SSID, passphrase, channel, survey advice, and the accept list live in `secrets/iot-ap/` (gitignored).

```bash
sudo ./tools/setup_iot_ap.sh
sudo ./tools/setup_iot_ap.sh status
sudo ./tools/setup_iot_ap.sh allow aa:bb:cc:dd:ee:ff
sudo ./tools/setup_iot_ap.sh revoke aa:bb:cc:dd:ee:ff
sudo ./tools/setup_iot_ap.sh apply-channel
```

The Realtek combo (USB `0bda:d723`) is a survey radio. Its Bluetooth controller stays off. Every 30 minutes it scores channels 1, 6, and 11 and writes a recommendation next to the saved channel. That scan leaves hostapd running. `apply-channel` copies the recommendation into the saved channel and restarts the AP. If the combo is unplugged, the first `up` surveys once on the Kootech before hostapd starts, and later surveys record that the scanner is missing.

ESP32 nodes join this SSID later. They are not a second AP on this machine.

## tuya-cloudcutter (OTA cloud cut + flash)

Clone: `references/firmware-liberation/tuya-cloudcutter`

### What it does

Exploits vulnerable Tuya BK7231T/N firmware profiles over Wi-Fi to:

1. Detach the device from Tuya cloud, and/or
2. OTA-install third-party firmware (OpenBeken UG bins or LibreTiny ESPHome Kickstart).

No soldering when a matching **device profile** exists. Patched / newer Tuya versions may require UART.

### High-level procedure

1. Linux host with Wi-Fi + Docker + NetworkManager (per upstream `INSTRUCTIONS.md`).
2. Identify module (`WB2L` ≈ BK7231T, `CB2L` ≈ BK7231N) and Tuya firmware version from the Smart Life app.
3. Put bulb in slow-blink AP mode (usually power-cycle 3–6 times).
4. Run `./tuya-cloudcutter.sh`, select profile + custom firmware from `custom-firmware/`.
5. Reboot to AP again when instructed; OTA completes ~30s.
6. Join the new AP (`OpenBK…` or `kickstart-…`), open `192.168.4.1`, join your IoT SSID, finish pin/GPIO config.

Upstream docs: LibreTiny [CloudCutter guide](https://docs.libretiny.eu/docs/flashing/tools/cloudcutter/).

### Custom firmware choices

| Firmware | Best for |
| --- | --- |
| **OpenBeken** (`OpenBK7231T_App`) | TuyaMCU mapping, MQTT, HA discovery, Beken/BL602/LN882H/W800 family |
| **LibreTiny ESPHome Kickstart** | Stay in ESPHome YAML world after pin scan |

Bins ship under cloudcutter’s `custom-firmware/`; replace with newer releases from OpenBeken / esphome-kickstart when needed (keep `bk7231t` / `bk7231n` in filenames).

## Inspect a dumped BK7231 image before flashing

If you have a UART/SPI flash dump (or an extracted OTA blob), inspect it with REA before deciding “RE the protocol” vs “flash OpenBeken”:

```bash
# Host deps: ./tools/install_re_toolchain_fedora.sh (binwalk + unblob)
npx -y rea-agents@6.7.0 inspect-firmware-regions captures/bk7231_dump.bin
npx -y rea-agents@6.7.0 extract-firmware captures/bk7231_dump.bin captures/fw-out/
```

Look for partition layout, embedded configs, and whether the dump matches a known CloudCutter / OpenBeken target. Hand interesting ELF regions to REA’s Ghidra lane if needed. Keep dumps under `captures/` (gitignored).

## OpenBeken

Clone: `references/firmware-liberation/OpenBK7231T_App`

After flash:

- Configure Wi-Fi via captive portal.
- Import extracted Tuya GPIO / flag config when available.
- For lamps with a **separate TuyaMCU**, flash only the Wi-Fi module; sniff UART dpIDs and map them in OpenBeken (power, colour, effects).
- Prefer MQTT / HTTP local control; keep the device on the IoT VLAN anyway.

## ESPHome / WLED / LibreTiny

- ESP-based addressable strips → **WLED** or ESPHome `light` / `neopixelbus`.
- Beken via LibreTiny → ESPHome configs after Kickstart pin discovery.
- Use Ethernet ESP32 proxies for BLE leftovers ([doc 01](01-bluetooth-server-hardware-and-proxies.md)).

## Safety and expectations

- CloudCutter can soft-brick unsupported profiles — match firmware version carefully; have UART clips ready for favorites.
- Opening sealed bulbs voids warranty and can expose mains — unplug and discharge before probing.
- Liberated devices still need good Wi-Fi and unique hostnames; document MAC → room → driver in inventory (future phase).
- Stock “local only” modes are not trust boundaries; **WAN block** remains wise even with local keys.

## Related

- [02 Reverse engineering playbook](02-reverse-engineering-playbook.md)
- [03 Known light protocols matrix](03-known-light-protocols-matrix.md)
- [05 Target webapp architecture](05-target-webapp-architecture.md)
