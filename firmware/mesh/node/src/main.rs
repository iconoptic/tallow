//! A tallow mesh node: it hears Bluetooth near it and passes that on.
//!
//! Every board runs this same image. The board the host is plugged into hears
//! the host's heartbeat on serial and becomes the root. The others find a way to
//! it through each other. Nothing here joins Wi-Fi or opens a socket.
//!
//! Serial is 115200 8N1. The host sends `H` lines as a heartbeat and
//! `C id window interval inquiry length probe_hz probe_size` lines to set scan
//! settings. `W <hex12> ...` sets the Classic addresses to page, up to 16, and
//! `W` alone clears them. The root prints `@` lines for `tools/mesh_host.py`.
//! `D` makes this board visible to Classic inquiry for 60 s, not connectable, so
//! a bench test has something to find. `X id` refuses beacons from one board id,
//! and `X` alone clears the list. Both are kept in RAM and only work from this
//! board's own serial port. `G id hex` sends a GATT request, in hex, down to the
//! board with that id. A send the root cannot route prints `@G refused`. Answers
//! come back up as `@U 12` lines. `O id hex` sends an OTA payload, in hex, down
//! to the board with that id. The root's own id is applied here instead of on
//! the radio. A send the root cannot route prints `@O refused`.
//! A downward OTA frame is written to the inactive slot from this loop. Only a
//! finished image is set to boot, and the board restarts into it. A line longer
//! than `LINE_MAX` is dropped whole.

#![warn(clippy::undocumented_unsafe_blocks)]

mod bluetooth;
mod gatt;
mod ota;
mod radio;

use core::fmt::Write as _;
use std::sync::atomic::Ordering;

use bluetooth::{now_ms, Bluetooth};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::uart::{config::Config, UartDriver};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};
use esp_idf_svc::sys;
use esp_idf_svc::wifi::{ClientConfiguration, Configuration, WifiDriver};
use tallow_mesh_core::gatt::{Client, Done, Reply};
use tallow_mesh_core::mesh::{next_boot, CHANNEL, MAX_NEIGHBOURS};
use tallow_mesh_core::ota::{Code, Down, Health, Receiver, Sample, Up, BITMAP, PRODUCT_NODE, SLOT};
use tallow_mesh_core::stats::{Stats, HEARD_WIRE};
use tallow_mesh_core::wire::{Command, WATCH_MAX};
use tallow_mesh_core::{Dest, Event, FrameKind, Mesh, MAX_PAYLOAD};

const KEY: &[u8; 32] = include_bytes!(concat!(env!("OUT_DIR"), "/mesh_key.bin"));
const REPORT_MS: u32 = 5_000;
const STATS_MS: u32 = 10_000;
const TX_TIMEOUT_MS: u32 = 250;
const VISIBLE_MS: u32 = 60_000;
/// Nodes given one list together start their name requests this far apart at most.
const PAGE_STAGGER_MS: u32 = 5_000;
/// How soon a name request is tried again after one could not start.
const PAGE_RETRY_MS: u32 = 250;
/// A serial line from the host. Sixteen addresses plus `W` and the spaces is 209
/// bytes. A `G` line for the largest GATT write is 175 request bytes, so 350 hex
/// digits, plus the id and the words around them.
const LINE_MAX: usize = 400;
/// Requests that came down and wait for the GATT client. A request past this is dropped.
const DOWN_QUEUE: usize = 4;
/// Serial input held while the loop is busy. A full `G` or `O` line is 31 ms of
/// serial, so this rides out a stall of about 90 ms.
const SERIAL_RX: usize = 1024;
/// How often the two stack watermarks are logged.
const WATERMARK_MS: u32 = 60_000;
/// After a good Result the board restarts once its queue is empty, but not before
/// this, so the root's serial line gets out.
const RESTART_MIN_MS: u32 = 200;
/// The longest a restart waits for that queue.
const RESTART_MAX_MS: u32 = 2_000;

const DEFAULTS: Command = Command {
    id: 0,
    ble_window_ms: 30,
    ble_interval_ms: 100,
    inquiry_period_s: 60,
    inquiry_length: 4,
    probe_hz: 0,
    probe_size: 8,
};

/// One hex digit, or `None` for any other byte.
fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

fn hex(out: &mut String, bytes: &[u8]) {
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
}

fn print(uart: &UartDriver, line: &str) {
    let _ = uart.write(line.as_bytes());
}

fn on_event(
    uart: &UartDriver,
    line: &mut String,
    command: &mut Option<Command>,
    watch: &mut Option<(u8, [[u8; 6]; WATCH_MAX])>,
    downs: &mut Vec<Vec<u8>>,
    ota_downs: &mut Vec<Vec<u8>>,
    event: Event,
) {
    line.clear();
    match event {
        Event::Up {
            frame,
            payload,
            rssi,
        } => {
            let _ = write!(
                line,
                "@U {} {:08x} {} {} {} {} ",
                frame.kind as u8, frame.origin, frame.boot, frame.seq, frame.hops, rssi
            );
            hex(line, payload);
            line.push('\n');
            print(uart, line);
        }
        Event::Command(new) => *command = Some(new),
        Event::Watch { count, addresses } => *watch = Some((count, addresses)),
        Event::Parent { new, .. } => {
            let _ = writeln!(line, "@P {:08x}", new.unwrap_or(0));
            print(uart, line);
        }
        // A GATT request is queued for the client, which runs it after this frame.
        // An OTA payload is queued the same way: flash writes happen after
        // `receive` returns, never from the radio callback. Ping is for tests
        // only, and nothing reads it.
        Event::Down { frame, payload } => {
            if frame.kind == FrameKind::Gatt && downs.len() < DOWN_QUEUE {
                downs.push(payload.to_vec());
            } else if frame.kind == FrameKind::Ota && ota_downs.len() < DOWN_QUEUE {
                ota_downs.push(payload.to_vec());
            }
        }
    }
}

/// Sends one GATT reply up to the host.
fn send_gatt_reply(mesh: &mut Mesh, reply: &Reply, now: u32, on_event: &mut dyn FnMut(Event)) {
    let mut body = [0u8; MAX_PAYLOAD];
    let length = reply.encode(&mut body);
    mesh.send_up(FrameKind::GattUp, &body[..length], now, on_event);
}

/// Runs the GATT client's radio work until neither the radio nor the client has
/// anything left, and sends each reply up. Inquiry and paging pause while a link
/// is held, or while `writing` an image. Scanning starts again once no link is.
fn gatt_pump(
    client: &mut Client,
    link: &mut gatt::Link,
    bt: &Bluetooth,
    mesh: &mut Mesh,
    writing: bool,
    now: u32,
    on_event: &mut dyn FnMut(Event),
) {
    loop {
        let mut worked = false;
        while let Some(act) = client.take_act() {
            worked = true;
            let mut done = link.perform(&act);
            // A stack that refuses a connect while a scan runs gets one retry, with
            // scanning stopped. Otherwise the scan goes on as it was.
            if matches!(done, Some(Done::Opened(false))) && bt.pause_scan_for_connect() {
                done = link.perform(&act);
            }
            if let Some(done) = done {
                if let Some(reply) = client.done(now, done) {
                    send_gatt_reply(mesh, &reply, now, on_event);
                }
            }
        }
        while let Some(done) = link.take() {
            worked = true;
            if let Some(reply) = client.done(now, done) {
                send_gatt_reply(mesh, &reply, now, on_event);
            }
        }
        if !worked {
            break;
        }
    }
    bt.set_link_busy(client.link_busy() || writing);
    if !client.link_busy() {
        bt.resume_scan();
    }
}

fn parse_command(line: &str) -> Option<Command> {
    let mut words = line.split_ascii_whitespace();
    words.next()?;
    Some(Command {
        id: words.next()?.parse().ok()?,
        ble_window_ms: words.next()?.parse().ok()?,
        ble_interval_ms: words.next()?.parse().ok()?,
        inquiry_period_s: words.next()?.parse().ok()?,
        inquiry_length: words.next()?.parse().ok()?,
        probe_hz: words.next()?.parse().ok()?,
        probe_size: words.next()?.parse().ok()?,
    })
}

/// `W` alone is an empty list. `W` with up to 16 addresses sets the list. Any bad
/// address, or too many, is `None` and changes nothing.
fn parse_watch(line: &str) -> Option<(u8, [[u8; 6]; WATCH_MAX])> {
    let mut words = line.split_ascii_whitespace();
    if words.next()? != "W" {
        return None;
    }
    let mut addresses = [[0u8; 6]; WATCH_MAX];
    let mut count = 0;
    for word in words {
        if count == WATCH_MAX {
            return None;
        }
        addresses[count] = parse_address(word)?;
        count += 1;
    }
    Some((count as u8, addresses))
}

/// `G <hex id> <hex request>` sends a GATT request down to the board with that id.
/// The request is the bytes `tallow_mesh_core::gatt` reads, in hex, and it may be
/// empty. A bad digit or an odd count is `None` and sends nothing.
fn parse_gatt(line: &str) -> Option<(u32, [u8; MAX_PAYLOAD - 4], usize)> {
    parse_down(line, "G")
}

/// `O <hex id> <hex payload>` sends an OTA payload down to the board with that id.
/// The payload is the bytes `tallow_mesh_core::ota` reads, in hex. A bad digit or
/// an odd count is `None` and sends nothing.
fn parse_ota(line: &str) -> Option<(u32, [u8; MAX_PAYLOAD - 4], usize)> {
    parse_down(line, "O")
}

fn parse_down(line: &str, word: &str) -> Option<(u32, [u8; MAX_PAYLOAD - 4], usize)> {
    let mut words = line.split_ascii_whitespace();
    if words.next()? != word {
        return None;
    }
    let target = u32::from_str_radix(words.next()?, 16).ok()?;
    let digits = words.next().unwrap_or("");
    if words.next().is_some()
        || !digits.len().is_multiple_of(2)
        || digits.len() / 2 > MAX_PAYLOAD - 4
    {
        return None;
    }
    let mut body = [0u8; MAX_PAYLOAD - 4];
    for (index, pair) in digits.as_bytes().chunks_exact(2).enumerate() {
        let &[high, low] = pair else {
            return None;
        };
        *body.get_mut(index)? = (nibble(high)? << 4) | nibble(low)?;
    }
    Some((target, body, digits.len() / 2))
}

/// Twelve hex digits with no separators. Each byte is checked, so text off the
/// serial line is never read as a number it is not.
fn parse_address(word: &str) -> Option<[u8; 6]> {
    let bytes = word.as_bytes();
    if bytes.len() != 12 {
        return None;
    }
    let mut out = [0u8; 6];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        out[index] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(out)
}

/// `X` alone is `Some(None)`: clear the list. `X <hex id>` is `Some(Some(id))`.
/// Anything else, including a bad or zero id, is `None` and changes nothing.
fn parse_deny(line: &str) -> Option<Option<u32>> {
    let mut words = line.split_ascii_whitespace();
    if words.next()? != "X" {
        return None;
    }
    let Some(word) = words.next() else {
        return Some(None);
    };
    if words.next().is_some() {
        return None;
    }
    let id = u32::from_str_radix(word, 16).ok()?;
    (id != 0).then_some(Some(id))
}

/// This boot's id. The NVS counter in `tallow/boot` is read and raised here, once,
/// before `Mesh::new`, and never written again. If NVS fails, the counter is seeded
/// from `random` instead: still unique, but not ordered against earlier boots.
fn boot_id(nvs: EspDefaultNvsPartition, random: u32) -> u32 {
    let attempt = || -> Result<u32, sys::EspError> {
        let store = EspNvs::new(nvs, "tallow", true)?;
        let (counter, boot) = next_boot(store.get_u32("boot")?, random);
        store.set_u32("boot", counter)?;
        Ok(boot)
    };
    match attempt() {
        Ok(boot) => boot,
        Err(error) => {
            log::warn!("boot counter unavailable ({error}), seeding from the radio");
            next_boot(None, random).1
        }
    }
}

/// The fewest bytes each task has had free: this one, then Bluedroid's. Bluedroid's
/// is 0 if that task is not found.
fn stack_left() -> (u32, u32) {
    // SAFETY: a null handle means the calling task, which is running.
    let main_left = unsafe { sys::uxTaskGetStackHighWaterMark(core::ptr::null_mut()) };
    // SAFETY: the name is a C string that lives for the call. The call only reads it.
    let btc = unsafe { sys::xTaskGetHandle(c"BTC_TASK".as_ptr()) };
    if btc.is_null() {
        return (main_left, 0);
    }
    // SAFETY: `btc` is non-null, and Bluedroid's task is never deleted while this
    // firmware runs, since nothing here disables Bluetooth.
    (main_left, unsafe { sys::uxTaskGetStackHighWaterMark(btc) })
}

fn main() -> Result<(), sys::EspError> {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    // First, so a new image that hangs anywhere below still rolls back.
    let mut slot = ota::Slot::open(nvs.clone());

    let config = Config::new()
        .baudrate(Hertz(115_200))
        .rx_fifo_size(SERIAL_RX);
    let uart = UartDriver::new(
        peripherals.uart0,
        peripherals.pins.gpio1,
        peripherals.pins.gpio3,
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &config,
    )?;

    let (wifi_modem, bt_modem) = peripherals.modem.split();
    let mut wifi = WifiDriver::new(wifi_modem, sysloop, Some(nvs.clone()))?;
    wifi.set_configuration(&Configuration::Client(ClientConfiguration::default()))?;
    wifi.start()?;
    let rx = radio::start(CHANNEL)?;

    let mut mac = [0u8; 6];
    // SAFETY: Wi-Fi is started, and `mac` is the 6 bytes this call writes.
    sys::esp!(unsafe {
        sys::esp_wifi_get_mac(sys::wifi_interface_t_WIFI_IF_STA, mac.as_mut_ptr())
    })?;
    // SAFETY: takes no arguments and may be called from any task.
    let random = unsafe { sys::esp_random() };
    let boot = boot_id(nvs.clone(), random);
    let mut mesh = Mesh::new(mac, KEY, boot);

    let bt = Bluetooth::start(bt_modem, nvs)?;
    let heard = bt.heard.clone();
    gatt::start()?;
    let mut client = Client::new();
    let mut link = gatt::Link::default();
    // The receiver is about 1.7 KB, and it lives on the heap to keep it out of the
    // main task's stack, which `tools/tests/stack_budget.py` checks.
    let mut receiver = Box::new(Receiver::<SLOT, BITMAP>::new(PRODUCT_NODE, slot.floor()));
    let mut health = Health::new();

    let mut line = String::with_capacity(600);
    let mut input = [0u8; 64];
    let mut typed = String::with_capacity(LINE_MAX);
    let mut overflowed = false;
    let mut pending: Option<Command> = Some(DEFAULTS);
    let mut pending_watch: Option<(u8, [[u8; 6]; WATCH_MAX])> = None;
    let mut settings = DEFAULTS;
    let mut in_flight: Option<u32> = None;
    let mut in_flight_unicast = false;
    let mut last_peer: Option<[u8; 6]> = None;
    let mut next_report = now_ms() + REPORT_MS;
    let mut next_stats = now_ms() + 1_000;
    let mut next_inquiry = now_ms() + 3_000;
    let mut next_page = now_ms();
    let mut next_probe = 0u32;
    let mut probe_seq = 0u32;
    let mut tx_ok = 0u32;
    let mut tx_failed = 0u32;
    let mut unicast_acked = 0u32;
    let mut next_watermark = now_ms().wrapping_add(WATERMARK_MS);
    let mut last_ota = 0u32;
    let mut restart_from: Option<u32> = None;
    let mut visible_until: Option<u32> = None;
    let mut downs: Vec<Vec<u8>> = Vec::new();
    let mut ota_downs: Vec<Vec<u8>> = Vec::new();

    let _ = writeln!(line, "@B {:08x} {}", mesh.id, boot);
    print(&uart, &line);

    loop {
        let now = now_ms();
        macro_rules! events {
            () => {
                &mut |event| {
                    on_event(
                        &uart,
                        &mut line,
                        &mut pending,
                        &mut pending_watch,
                        &mut downs,
                        &mut ota_downs,
                        event,
                    )
                }
            };
        }

        // Host, on serial.
        let count = uart.read(&mut input, 0).unwrap_or(0);
        for &byte in &input[..count] {
            if byte != b'\n' {
                if typed.len() < LINE_MAX {
                    typed.push(byte as char);
                } else {
                    overflowed = true;
                }
                continue;
            }
            if overflowed {
                // Part of a line must not run as a shorter command.
                overflowed = false;
            } else if typed.starts_with('H') {
                mesh.host_heartbeat(now);
            } else if typed.starts_with('D') {
                if bt.set_visible(true).is_ok() {
                    visible_until = Some(now.wrapping_add(VISIBLE_MS));
                }
            } else if typed.starts_with('X') {
                match parse_deny(&typed) {
                    Some(None) => mesh.clear_deny(),
                    Some(Some(id)) => {
                        mesh.deny(id);
                    }
                    None => {}
                }
            } else if typed.starts_with('C') {
                if let Some(command) = parse_command(&typed) {
                    mesh.send_command(&command, events!());
                }
            } else if typed.starts_with('W') {
                if let Some((count, addresses)) = parse_watch(&typed) {
                    if !mesh.send_watch(&addresses[..count as usize], events!()) {
                        print(&uart, "@W refused\n");
                    }
                }
            } else if typed.starts_with('G') {
                if let Some((target, body, length)) = parse_gatt(&typed) {
                    if !mesh.send_down(now, target, FrameKind::Gatt, &body[..length]) {
                        print(&uart, "@G refused\n");
                    }
                }
            } else if typed.starts_with('O') {
                if let Some((target, body, length)) = parse_ota(&typed) {
                    if target == mesh.id {
                        if ota_downs.len() < DOWN_QUEUE {
                            ota_downs.push(body[..length].to_vec());
                        } else {
                            print(&uart, "@O refused\n");
                        }
                    } else if !mesh.send_down(now, target, FrameKind::Ota, &body[..length]) {
                        print(&uart, "@O refused\n");
                    }
                }
            }
            typed.clear();
        }

        if visible_until.is_some_and(|until| (now.wrapping_sub(until) as i32) >= 0) {
            visible_until = None;
            let _ = bt.set_visible(false);
        }

        // Neighbours, on the air.
        for frame in rx.try_iter().take(8) {
            let bytes = &frame.data[..frame.length as usize];
            mesh.receive(now, frame.src, frame.rssi, bytes, events!());
        }
        mesh.tick(now, events!());

        // OTA payloads that came down to this board. The receiver's actions are
        // performed here, and a status or result goes back up.
        for body in std::mem::take(&mut ota_downs) {
            // A stored image waits for the restart. A new Begin would pick the same
            // slot, since the choice follows the running partition, and erase it.
            if restart_from.is_some() {
                continue;
            }
            last_ota = now;
            // Flash writes delay the main loop, and with it `cut_inquiry`. Nothing
            // here may follow an inquiry into paging devices off the whitelist.
            if matches!(
                tallow_mesh_core::ota::decode_down(&body),
                Some(Down::Begin { .. })
            ) {
                bt.stop_inquiry();
            }
            if let Some(up) = ota::apply(&mut receiver, &mut slot, &body) {
                if matches!(up, Up::Result { code: Code::Ok }) {
                    restart_from = Some(now);
                }
                let mut encoded = [0u8; MAX_PAYLOAD];
                if let Some(length) = tallow_mesh_core::ota::encode_up(&up, &mut encoded) {
                    mesh.send_up(FrameKind::OtaUp, &encoded[..length], now, events!());
                }
            }
        }
        ota::expire(&mut receiver, &mut slot, now, last_ota);

        // Requests that came down to this board go to the GATT client, and its
        // timeouts and radio results follow.
        for body in std::mem::take(&mut downs) {
            if let Some(reply) = client.request(now, &body) {
                send_gatt_reply(&mut mesh, &reply, now, events!());
            }
        }
        if let Some(reply) = client.tick(now) {
            send_gatt_reply(&mut mesh, &reply, now, events!());
        }
        let writing = receiver.in_progress();
        gatt_pump(
            &mut client,
            &mut link,
            &bt,
            &mut mesh,
            writing,
            now,
            events!(),
        );

        // One frame on the air at a time, so ordering and acks stay simple.
        if let Some(started) = in_flight {
            let outcome = radio::sent();
            if outcome.is_some() || now.wrapping_sub(started) > TX_TIMEOUT_MS {
                let acked = outcome.unwrap_or(false);
                if acked {
                    tx_ok = tx_ok.wrapping_add(1);
                    if in_flight_unicast {
                        unicast_acked = unicast_acked.wrapping_add(1);
                    }
                } else {
                    tx_failed = tx_failed.wrapping_add(1);
                }
                in_flight = None;
                mesh.tx_done(now, acked, events!());
            }
        }
        if in_flight.is_none() {
            if let Some(tx) = mesh.next_tx() {
                let target = match tx.dest {
                    Dest::Broadcast => radio::BROADCAST,
                    Dest::Peer(mac) => mac,
                };
                if tx.dest != Dest::Broadcast && last_peer != Some(target) {
                    if let Some(old) = last_peer.replace(target) {
                        radio::drop_peer(old);
                    }
                }
                let unicast = tx.dest != Dest::Broadcast;
                match radio::send(target, tx.bytes()) {
                    Ok(()) => {
                        in_flight = Some(now);
                        in_flight_unicast = unicast;
                    }
                    Err(_) => {
                        tx_failed = tx_failed.wrapping_add(1);
                        mesh.tx_done(now, false, events!());
                    }
                }
            }
        }

        // A stored image takes over once its Result is on its way, or the wait is up.
        if let Some(from) = restart_from {
            let waited = now.wrapping_sub(from);
            let sent = mesh.queued() == 0 && in_flight.is_none();
            if waited >= RESTART_MAX_MS || (sent && waited >= RESTART_MIN_MS) {
                // SAFETY: takes no arguments and does not return. Nothing is being
                // written: the image is finished, and its slot is selected.
                unsafe { sys::esp_restart() };
            }
        }

        // Settings from the host, here or by flood.
        if let Some(new) = pending.take() {
            settings = new;
            let _ = bt.configure_scan(new.ble_window_ms, new.ble_interval_ms);
            next_inquiry = now.wrapping_add(1_000);
            // Spread nodes across the period so commanded nodes do not inquire together.
            // A period of 0 means no inquiries, and `% 0` would panic, so skip the offset.
            if new.inquiry_period_s > 0 {
                let offset_s = mesh.id % new.inquiry_period_s as u32;
                next_inquiry = next_inquiry.wrapping_add(offset_s * 1000);
            }
        }
        if let Some((count, addresses)) = pending_watch.take() {
            bt.set_watch(count, &addresses);
            // Nodes given one list together must not all page at once, so each one
            // waits its share of the stagger before its first name request.
            next_page = now.wrapping_add(mesh.id % PAGE_STAGGER_MS);
        }
        let _ = bt.poll();

        if settings.inquiry_period_s > 0 && (now.wrapping_sub(next_inquiry) as i32) >= 0 {
            // A name request in flight defers the inquiry by a second, not a period.
            next_inquiry = match bt.inquire(settings.inquiry_length) {
                Ok(false) => now.wrapping_add(1_000),
                _ => now.wrapping_add(settings.inquiry_period_s as u32 * 1000),
            };
        }

        // Classic name requests, one at a time, from the host's list.
        if (now.wrapping_sub(next_page) as i32) >= 0 {
            next_page = match bt.page_next(now) {
                Ok(true) => now.wrapping_add(bluetooth::PAGE_GAP_MS),
                _ => now.wrapping_add(PAGE_RETRY_MS),
            };
        }

        // What this node has heard.
        if (now.wrapping_sub(next_report) as i32) >= 0 {
            next_report = now.wrapping_add(REPORT_MS);
            let mut body = [0u8; MAX_PAYLOAD];
            for _ in 0..6 {
                let length = heard
                    .sightings
                    .lock()
                    .map_or(0, |mut table| table.drain(now, &mut body));
                if length == 0 || !mesh.send_up(FrameKind::Report, &body[..length], now, events!())
                {
                    break;
                }
            }
            let length = heard
                .names
                .lock()
                .map_or(0, |mut names| names.drain(&mut body));
            if length > 0 {
                mesh.send_up(FrameKind::Name, &body[..length], now, events!());
            }
        }

        if (now.wrapping_sub(next_stats) as i32) >= 0 {
            next_stats = now.wrapping_add(STATS_MS);
            // SAFETY: read-only heap queries with no arguments that can be wrong.
            let (free, least, block) = unsafe {
                (
                    sys::esp_get_free_heap_size(),
                    sys::esp_get_minimum_free_heap_size(),
                    sys::heap_caps_get_largest_free_block(sys::MALLOC_CAP_8BIT) as u32,
                )
            };
            let evicted = heard.sightings.lock().map_or(0, |table| table.evicted);
            let stats = Stats {
                uptime_s: now / 1000,
                heap_free: free,
                heap_min: least,
                heap_block: block,
                adverts: heard.adverts.load(Ordering::Relaxed),
                classic: heard.classic.load(Ordering::Relaxed),
                inquiries: heard.inquiries.load(Ordering::Relaxed),
                rx_frames: radio::RX_FRAMES.load(Ordering::Relaxed),
                rx_drops: radio::RX_DROPS.load(Ordering::Relaxed),
                rx_bad: mesh.rx_bad,
                rx_dup: mesh.rx_dup,
                tx_ok,
                tx_failed,
                queue_drops: mesh.queue_drops,
                parent_changes: mesh.parent_changes,
                evicted,
                parent: mesh.parent().map_or(0, |parent| parent.id),
                settings: settings.id as u32,
                inquiry_started: heard.inquiry_started.load(Ordering::Relaxed),
                inquiry_stopped: heard.inquiry_stopped.load(Ordering::Relaxed),
                inquiry_errors: heard.inquiry_errors.load(Ordering::Relaxed),
                rank: mesh.rank,
                neighbours: mesh.neighbours().count() as u8,
                parent_rssi: mesh.parent().map_or(0, |parent| parent.rssi),
                parent_quality: mesh.parent().map_or(0, |parent| parent.quality() as u8),
            };
            let ready = health.sample(&Sample {
                now_ms: now,
                rooted: mesh.is_root(now),
                has_parent: mesh.parent().is_some(),
                acked: unicast_acked,
                heap_free: free,
                heap_min: least,
            });
            if let Some(floor) = slot.confirm_if_ready(ready) {
                receiver.raise_floor(floor);
            }
            let identity = slot.identity().encode();
            mesh.send_up(FrameKind::Identity, &identity, now, events!());
            mesh.send_up(FrameKind::Stats, &stats.encode(), now, events!());
            let mut heard = [0u8; MAX_NEIGHBOURS * HEARD_WIRE];
            let heard_len = mesh.encode_neighbours(&mut heard);
            if heard_len > 0 {
                mesh.send_up(FrameKind::Neighbours, &heard[..heard_len], now, events!());
            }
        }

        // Warn level, because the build compiles out anything quieter.
        if (now.wrapping_sub(next_watermark) as i32) >= 0 {
            next_watermark = now.wrapping_add(WATERMARK_MS);
            let (main_left, btc_left) = stack_left();
            log::warn!("stack left: main {main_left}, btc {btc_left}");
        }

        // Delivery test.
        if settings.probe_hz > 0 && (now.wrapping_sub(next_probe) as i32) >= 0 {
            next_probe = now.wrapping_add(1000 / settings.probe_hz as u32);
            probe_seq = probe_seq.wrapping_add(1);
            let mut body = [0u8; MAX_PAYLOAD];
            body[..4].copy_from_slice(&probe_seq.to_le_bytes());
            let size = (settings.probe_size as usize).clamp(4, MAX_PAYLOAD);
            mesh.send_up(FrameKind::Probe, &body[..size], now, events!());
        }

        // Under one tick, std sleep spins and starves the idle task on this core.
        FreeRtos::delay_ms(2);
    }
}
