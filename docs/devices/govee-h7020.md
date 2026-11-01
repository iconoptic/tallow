# Govee H7020

Outdoor bulb string. Each string is 15 RGBIC bulbs, and the control box can be set for one or two strings. This unit is one string. It speaks BLE. It is absent from Govee's LAN API product list, so it does not belong on the isolated access point.

## This unit

| Field | Value |
| --- | --- |
| Advertised name | `Govee_H7020_3E4A` |
| BLE address | `C5:38:30:33:3E:4A` |
| Pact | 1 / code 1 |
| Firmware | `1.00.14` |
| Hardware | `3.01.01` |
| Configured strings | 1 |
| Encryption | clear in the advertisement |

The address is a static-random Bluetooth address. It is not the Wi-Fi station MAC. The host seed is the TP-Link UB500 (`2357:0604`). Its `hci` index moves; the probe finds that USB id. On the night this note was written the index was `hci0`, and `hci1` was the blocked Realtek combo.

Advertisement manufacturer id `34818` (`0x8802`), payload `ec00010101`. The pinned codec reads that as pact 1 / code 1 with the encryption flag clear.

## GATT

| Role | UUID |
| --- | --- |
| Light service | `00010203-0405-0607-0809-0a0b0c0d1910` |
| Write | `00010203-0405-0607-0809-0a0b0c0d2b11` |
| Notify | `00010203-0405-0607-0809-0a0b0c0d2b10` |

A second service, `02f00000-0000-0000-0000-00000000fe00`, is present. The probe does not write it.

The box accepts one central. Close Govee Home before connecting.

## Frames that moved this box

Codec: `references/protocols-ble/ha-govee-led-ble` tag `v7.14.0-rc.2.h7020`. The probe calls that tree; it does not copy the frames.

```bash
.venv/bin/python tools/govee_h7020.py
```

That command reads pact, firmware, hardware, and string count, then sends power on, brightness 100, solid red, and power off. `--strings 2` widens the color mask for a second string of 15. `identify` stops after the reads.

On 2026-10-27 the live box answered the three reads, then accepted these writes for one string. The red hold was 4 seconds.

| Command | Frame |
| --- | --- |
| Power on | `3301010000000000000000000000000000000033` |
| Brightness 100 | `3304640000000000000000000000000000000053` |
| Red, 15 bulbs | `33051501ff00000000000000ff7f00000000005d` |
| Power off | `3301000000000000000000000000000000000032` |

The red frame's mask is `ff7f0000` (the low 15 bits). Direct RGB bytes are `ff0000`. That hold showed red on the string.

Pact 1/code 1 and pact 2/code 1 are the codec's color profiles. Any other pact stops the probe after the identity read.

## LAN page

`app/h7020_panel.py` is the tallow LAN panel. With the cookie, `/` is a menu; this box's page is `/lights`. The other entries are the cameras app and Settings ([keyestudio-mb0200-s3-cam.md](keyestudio-mb0200-s3-cam.md#cams-page)). The panel imports packet builders from `tools/govee_h7020.py` and does not copy frames. It holds one GATT session on the UB500. Slider writes keep only the latest color or brightness. A scene upload or a bulb paint is a list of packets and goes out in that order. The phone app cannot connect while that process is running. The box drops an idle central after about ten seconds, so the page re-reads power to keep the link. The picker sends RGB through `build_color_rgb`.

The page also lists the codec's 51 catalogue scenes, grouped as Natural, Life, Emotion, and Festival. A saved pattern is one of those scenes, a paint of bulb groups, or a sequence of scenes and paints. Paint groups use bulb numbers starting at 1. A bulb belongs to one group, and each group goes out as its own packet. A sequence has at most 32 steps. Each step holds for at least one second. This process plays the steps, and it loops when the pattern sets repeat. A color, a power change, a scene, or a paint stops that playback.

A schedule names weekdays and a local `HH:MM`. Its action is a saved pattern, a catalogue scene, or power off. Schedules stay on this page. Each saved row has Turn off or Turn on, which PUTs that same schedule with `enabled` flipped. A disabled row is shown with `(off)`. The page clock runs in this process, so a second program never opens another GATT connection. Each enabled slot fires once on a matching weekday, when it is the latest one already due; a disabled slot does not fire. A start later in the day applies only the latest slot from the past 12 hours. Saving from the form sends `enabled: true`. `tools/tests/schedule_off_harness.js`, run from `tools/tests/test_h7020_library.py`, checks the `(off)` label and that PUT. Saved patterns and schedules are `data/panel/patterns.json` and `data/panel/schedules.json`. Power, brightness, color, scene, paint, and applying a pattern share a limit of about 30 calls a minute.

DIY memory, music modes, and Kelvin are not on this page. The codec has them. Those writes are not qualified on this firmware, and a DIY upload needs a disposable slot on the box.

```bash
.venv/bin/python app/h7020_panel.py
.venv/bin/python app/h7020_panel.py --preview
.venv/bin/python app/h7020_panel.py --preview --password-file /tmp/panel-preview-password
```

The process prints `Panel http://<house-lan>:8765/`, the agent socket path, and the cam count. If the BLE connect fails at startup, it logs `Connect failed (...)`, keeps retrying as after a dropped link, and stays up; the light routes answer 503 `disconnected` until the box answers. It binds the default-route source when that address is outside `10.87.0.0/24`, and it refuses `0.0.0.0` and the IoT access point. `--preview` binds only `127.0.0.1` and does not open BLE. Closing it skips `stop_notify` when there is no BLE client. Its library is `data/panel-preview/` and its socket is `data/panel-preview/agent.sock`, so a click-through does not change the live patterns. The password is one line in `secrets/panel/password` (mode `0600`). `--password-file` reads a throwaway password instead, for contract tests, with the same one-line and `0600` checks. It works only with `--preview`: without it the process exits. `data/panel/` and `data/panel-preview/` are mode `0700`. The contract is `tools/tests/panel_contract.py`. Its header is the command.

`GET /` without a cookie is 200 and the login form (`text/html; charset=utf-8`). `GET /lights`, `GET /cams`, and `GET /settings` without a cookie are 303 to `/login`. `GET /panel.css` is served without a cookie: 200, `text/css; charset=utf-8`, `cache-control: no-cache`. A correct `POST /login` is 303 to `/` and sets `tallow_panel`. The value is HMAC-SHA256, key the password, message `tallow-h7020-panel`. The cookie is HttpOnly, `Max-Age=2592000`, `Path=/`, `SameSite=strict`. `POST /logout`, with or without a cookie, is 303 to `/login` and clears `tallow_panel`. The clear header is `tallow_panel=""` with `Max-Age=0`, `Path=/`, `SameSite=lax`. Logout leaves the password file alone. Cam route statuses are in [keyestudio-mb0200-s3-cam.md](keyestudio-mb0200-s3-cam.md#routes).

A catalogue scene, a paint, and a sequence have not been sent to this string from the page yet. Power, brightness, and the solid red frame above are the writes this unit has shown.

### Agent socket

The socket is `data/panel/agent.sock`, mode `0600`. It sees the H7020 API only. The 404 list is in [routes](keyestudio-mb0200-s3-cam.md#routes), and the body is `{"detail":"not found"}`. `POST /login` with a password field is that same 404. `GET /` on the socket is 200 and the login form. `GET /api/state` is 401 `{"detail":"login"}`, including when the browser cookie is sent. The house-LAN port accepts the browser cookie and rejects `Authorization`. The socket accepts the bearer and rejects the cookie. `tools/panel_broker.py` is the process that calls it. The broker listens on vsock port `17620` and accepts only the `bazzite-console` guest CID in `secrets/panel/broker.json`. It adds the bearer from `secrets/panel/hermes.token` (mode `0600`) and forwards read and control routes. A client `Authorization`, `Cookie`, or library or schedule write is refused before the socket.

The guest does not receive the socket file, the token, the house-LAN panel, or `10.87.0.0/24`. On the guest, `tallow-proxy` listens on `127.0.0.1:17620` and dials vsock CID 2. The Hermes container's pasta line forwards that loopback port (`-T 17620`). The container still has no route and no DNS. The user unit is `tools/tallow-panel-broker.service`.

```bash
.venv/bin/python tools/panel_token.py create hermes
.venv/bin/python tools/panel_token.py create hermes --scopes read,control
.venv/bin/python tools/panel_token.py create hermes --scopes read,control --secret-file secrets/panel/hermes.token
```

Without `--secret-file`, the command prints a bearer once. With it, the secret is written to that path at mode `0600` and stdout stays empty. Either way the sha256 is stored in `secrets/panel/agents.json` (mode `0600`). The same name replaces the stored hash. Scopes are `read` (state and lists), `control` (power, brightness, color, scene, paint, and apply), `library` (save and delete patterns), and `schedule` (save and delete schedules). The Hermes token is `read` and `control`. The browser cookie has all four. There is no scope that mints tokens or sends raw frames. `GET /api/manifest` on the socket is the tool schema: scene names, pattern and schedule fields, bulb count, and the absences (DIY, music, Kelvin, raw frames). On the house LAN it is 404 `{"detail":"not found"}`. On the socket without a bearer it is 401 `{"detail":"login"}`. Agent calls are appended to `data/panel/audit.jsonl` with the token name, not the secret.

State and the manifest include `"device": "h7020"`.

## Wi-Fi

This box still phones home if it remembers the house Wi-Fi. Forget that SSID and leave it off `tallow-iot`. BLE works with no Wi-Fi. GATT commands stay on the local link; the pinned codec does not clear a saved network. Provisioning asks the control box to reach Govee, and the sandbox offers no default route. There is no LAN Control toggle to enable on this SKU.
