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

One 2.4 GHz SSID on the Kootech adapter (USB `0bda:b812`). The default name is `tallow-iot`. WPA2-PSK, HT20, station isolation. The access point is `10.87.0.1/24`. DHCP offers no default route. DNS on this host answers every name with `0.0.0.0`. Chrony serves time to `10.87.0.0/24`. Firewall zone `tallow-iot` accepts DHCP, DNS, and NTP from that subnet and drops the rest. There is no masquerade and no port forward, so the LAN, Docker, Podman, and libvirt have no path in. `tools/setup_iot_ap.sh` refuses `docker*`, `podman*`, `virbr*`, and `br-*` interfaces.

The accept list starts empty. The SSID is up, and nothing can associate until its MAC is allowed. SSID, passphrase, channel, survey advice, the accept list, and node reservations live in `secrets/iot-ap/` (gitignored, directory `0700`, files `0600`).

```bash
sudo ./tools/setup_iot_ap.sh
sudo ./tools/setup_iot_ap.sh status
sudo ./tools/setup_iot_ap.sh allow aa:bb:cc:dd:ee:ff
sudo ./tools/setup_iot_ap.sh revoke aa:bb:cc:dd:ee:ff
sudo ./tools/setup_iot_ap.sh reserve aa:bb:cc:dd:ee:ff 10.87.0.22
sudo ./tools/setup_iot_ap.sh unreserve aa:bb:cc:dd:ee:ff
sudo ./tools/setup_iot_ap.sh apply-channel
```

Reservations are optional. An ESPHome node sets its own static address ([ESP32 nodes](#esp32-nodes)). `reserve` records `mac,ip` in `secrets/iot-ap/reservations` and publishes `/etc/tallow/dnsmasq.hosts` (`dhcp-hostsfile`). A node address is `10.87.0.20`–`10.87.0.49`. The dynamic pool stays `10.87.0.50`–`10.87.0.200`. `status` lists the reservations. `unreserve` drops a line, publishes the file, and signals dnsmasq to re-read it.

dnsmasq reads `dnsmasq.conf` as root, drops to user `dnsmasq`, and only then opens the hosts file. The script sets `/etc/tallow` to mode `0711` (traversable, not listable) and checks that user `dnsmasq` can read `dnsmasq.hosts`. `hostapd.conf` stays `0600`. Mode `0700` hides the file: dnsmasq logs `cannot read /etc/tallow/dnsmasq.hosts: Permission denied` and offers a pool address.

When a lease disagrees with a reservation, `up` stops dnsmasq, drops those leases, starts dnsmasq, deauthenticates those stations, and waits up to 15 seconds for the reserved address to answer. A changed `reserve` does the same while dnsmasq is running.

`up`, `reserve`, and `unreserve` pin a permanent neighbor for each reservation whose MAC is associated on the AP (`ip neigh replace <ip> lladdr <mac> dev <iface> nud permanent`). The pin is removed when that MAC is not associated, or when the address is no longer reserved. A node with no reservation gets no pin.

The Realtek combo (USB `0bda:d723`) is a survey radio. Its Bluetooth controller stays off. Every 30 minutes it scores channels 1, 6, and 11 and writes a recommendation next to the saved channel. That scan leaves hostapd running. `apply-channel` copies the recommendation into the saved channel and restarts the AP. If the combo is unplugged, the first `up` surveys once on the Kootech before hostapd starts, and later surveys record that the scanner is missing.

### ESP32 nodes

ESPHome nodes (retired; configs in `archive/firmware/esphome/`) joined this SSID as stations and talked only to this host ([docs/01](01-bluetooth-server-hardware-and-proxies.md#tallow-nodes)). They are not a second AP on this machine. Mesh nodes do not join it at all ([Mesh nodes](#mesh-nodes)). The Rust cam uses `10.87.0.23` and reads its Wi-Fi from gitignored `firmware/cam/secrets.yaml`.

| Address | Node | Config |
| --- | --- | --- |
| `10.87.0.20` | Bare `node-mb0200` | `archive/firmware/esphome/node-mb0200.yaml` |
| `10.87.0.21` | `toby-cam-1` | `archive/firmware/esphome/toby-cam-1.yaml` |
| `10.87.0.22` | `node-1`, on no board | `archive/firmware/esphome/node-1.yaml` |
| `10.87.0.23` | `toby-cam-0`, not flashed | `archive/firmware/esphome/toby-cam-0.yaml`. The Rust image uses this address and is not flashed ([device note](devices/keyestudio-mb0200-s3-cam.md#rust-image)) |

Each ESPHome node's YAML set a static address (`manual_ip`) in `10.87.0.20`–`10.87.0.49`, outside the DHCP pool. `use_address` is where the host dials. A node's mDNS stays at ESPHome's default; the host does not use it. A station MAC goes on the accept list with `allow` before it can associate. Nodes do not depend on `reserve`. It is optional. A reservation still gives a board that asks for DHCP a fixed lease, and the neighbor pin reads it.

This host opens every connection to a node: `6053` (ESPHome API), `3232` (OTA), and `8080` (a cam's stream). The zone opens nothing from its side. `ap_isolate=1` stops node-to-node Wi-Fi through the AP. On 2026-10-28 the first node joined: one MB0200, as `relay-mb0200` at `10.87.0.20` and then `toby-cam-1` at `10.87.0.21`. The host reached it on all three ports ([device note](devices/keyestudio-mb0200-s3-cam.md#on-the-board-2026-10-28)).

`secrets/iot-ap/reservations` and `/etc/tallow/dnsmasq.hosts` list `10.87.0.21` for the MB0200 (`44:bd:8d:7a:51:60`) and `10.87.0.22` for the classic ESP32 (`70:4b:ca:04:21:64`, `node-1`). Both lines predate static addresses. They are optional now. `sudo ./tools/setup_iot_ap.sh` at 2026-10-30 20:34 set `/etc/tallow` to mode `0711`. dnsmasq could read the hosts file and DHCPACK'd the MB0200 as `10.87.0.21`. Both MACs are on the accept list. On 2026-10-31 neither board is on this SSID: the MB0200 is unreachable ([device note](devices/keyestudio-mb0200-s3-cam.md#where-it-is-2026-10-31)), and the classic ESP32 runs the mesh ([its note](devices/esp32-devkit.md#where-it-is-2026-10-31)). The second MB0200's MAC is unknown, so `toby-cam-0` cannot join until it is read and allowed ([device note](devices/keyestudio-mb0200-s3-cam.md#toby-cam-0)).

### Mesh nodes

Mesh nodes join no Wi-Fi. They take no address and no accept-list line, and nothing they do touches `tallow-iot`. They speak ESP-NOW to each other on channel 1, and the root reaches this host over USB serial ([docs/01](01-bluetooth-server-hardware-and-proxies.md#multi-hop-mesh)). There is no IP on the mesh, so it adds no route in or out.

Every node shares one key, and that key is the whole membership list. It lives in gitignored `firmware/mesh/mesh.key` because `secrets/` is root-owned. `tools/mesh.sh key` writes it once, and every node is built with it. A build with no key stops, so joining is never open. A frame sealed with another key is rejected. There is no per-node revocation.

The key sits in more places than `mesh.key`, so each one is kept private:

- The build script writes `mesh_key.bin` into its output directory at mode `0600`, as private as the key file it came from.
- The built ELF and image embed the key, and cargo makes them world-readable. After a build, `tools/mesh.sh build` and `tools/mesh.sh check` remove group and other access from `firmware/mesh/node/target`. `check --fast` builds nothing, so it does not touch it.
- A mesh board's flash holds the key, so a backup does too. `tools/mesh.sh backup` writes `data/firmware-backups/` at mode `0700` and each file at `0600`. The three backups taken on 2026-10-31 were chmodded to match ([board note](devices/esp32-devkit.md#back-up)).

Do not copy any of these out of the tree, and do not paste their contents anywhere.

The OTA signing seed is a second file, gitignored `firmware/mesh/ota.key`, mode `0600`. `tools/mesh.sh ota-key` also writes the public half to tracked `firmware/mesh/ota.pub`, once, and refuses to overwrite either file. Both are on disk. `firmware/mesh/node/build.rs` bakes in the mesh key and the public half. The container mounts `firmware/mesh/ota.pub` read-only at `/run/ota.pub`. The build does not read the seed. Do not copy the seed or paste it.

`tools/mesh.sh ota-image` writes `firmware/mesh/node/target/ota.bin` at mode `0600`. That directory is private, the same treatment as the ELF, because the image contains the mesh key. `tools/mesh.sh ota-sign <image> <node|cam0> <version>` signs the image's SHA-256 with the product and version, using `openssl pkeyutl -sign -rawin`, and writes the 69-byte `<image>.sig`. It has not been run against `firmware/mesh/ota.key`. The node behaviour is in [docs/01](01-bluetooth-server-hardware-and-proxies.md#source-ahead-of-the-boards-frame-version-0xa8).

A node is never connectable, and it accepts no connection from any device. It answers Classic inquiry only for 60 s after `D` on its own serial port. Since the 2026-10-31 flash, its inquiry ends 200 ms early, so it pages no device that has no name in its EIR ([docs/01](01-bluetooth-server-hardware-and-proxies.md#flashed-2026-10-31-the-cross-check-build)). The GATT client, built and not flashed, opens connections only to addresses on its whitelist.

## tuya-cloudcutter (OTA cloud cut + flash)

Clone: `references/firmware-liberation/tuya-cloudcutter`

### What it does

Exploits vulnerable Tuya BK7231T/N firmware profiles over Wi-Fi to:

1. Detach the device from Tuya cloud, and/or
2. OTA-install third-party firmware (OpenBeken UG bins or LibreTiny ESPHome Kickstart).

No soldering when a matching **device profile** exists. Patched / newer Tuya versions may require UART.

### High-level procedure

1. Linux host with Wi-Fi + Docker + NetworkManager (per upstream `INSTRUCTIONS.md`; the rest of this repo uses rootless Podman, and CloudCutter has not been run under it).
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
