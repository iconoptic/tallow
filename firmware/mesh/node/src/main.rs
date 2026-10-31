//! A tallow mesh node: it hears Bluetooth near it and passes that on.
//!
//! Every board runs this same image. The board the host is plugged into hears
//! the host's heartbeat on serial and becomes the root. The others find a way to
//! it through each other. Nothing here joins Wi-Fi or opens a socket.
//!
//! Serial is 115200 8N1. The host sends `H` lines as a heartbeat and
//! `C id window interval inquiry length probe_hz probe_size` lines to set scan
//! settings. The root prints `@` lines for `tools/mesh_host.py`. `D` makes this
//! board visible to Classic inquiry for 60 s, not connectable, so a bench test
//! has something to find. It only works from this board's own serial port.

mod bluetooth;
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
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys;
use esp_idf_svc::wifi::{ClientConfiguration, Configuration, WifiDriver};
use tallow_mesh_core::wire::Command;
use tallow_mesh_core::{Dest, Event, FrameKind, Mesh, MAX_PAYLOAD};

const KEY: &[u8; 32] = include_bytes!(concat!(env!("OUT_DIR"), "/mesh_key.bin"));
/// The cleanest channel in the 2026-10-31 survey, away from tallow-iot on 6.
const CHANNEL: u8 = 1;
const REPORT_MS: u32 = 5_000;
const STATS_MS: u32 = 10_000;
const TX_TIMEOUT_MS: u32 = 250;
const VISIBLE_MS: u32 = 60_000;

const DEFAULTS: Command = Command {
    id: 0,
    ble_window_ms: 30,
    ble_interval_ms: 100,
    inquiry_period_s: 60,
    inquiry_length: 4,
    probe_hz: 0,
    probe_size: 8,
};

fn hex(out: &mut String, bytes: &[u8]) {
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
}

fn print(uart: &UartDriver, line: &str) {
    let _ = uart.write(line.as_bytes());
}

fn on_event(uart: &UartDriver, line: &mut String, command: &mut Option<Command>, event: Event) {
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
        Event::Parent(parent) => {
            let _ = writeln!(line, "@P {:08x}", parent.unwrap_or(0));
            print(uart, line);
        }
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

fn main() -> Result<(), sys::EspError> {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let config = Config::new().baudrate(Hertz(115_200));
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
    let boot = unsafe { sys::esp_random() };
    let mut mesh = Mesh::new(mac, KEY, boot);

    let bt = Bluetooth::start(bt_modem, nvs)?;
    let heard = bt.heard.clone();

    let mut line = String::with_capacity(600);
    let mut input = [0u8; 64];
    let mut typed = String::with_capacity(64);
    let mut pending: Option<Command> = Some(DEFAULTS);
    let mut settings = DEFAULTS;
    let mut in_flight: Option<u32> = None;
    let mut last_peer: Option<[u8; 6]> = None;
    let mut next_report = now_ms() + REPORT_MS;
    let mut next_stats = now_ms() + 1_000;
    let mut next_inquiry = now_ms() + 3_000;
    let mut next_probe = 0u32;
    let mut probe_seq = 0u32;
    let mut tx_ok = 0u32;
    let mut tx_failed = 0u32;
    let mut visible_until: Option<u32> = None;

    let _ = writeln!(line, "@B {:08x} {}", mesh.id, boot);
    print(&uart, &line);

    loop {
        let now = now_ms();
        macro_rules! events {
            () => {
                &mut |event| on_event(&uart, &mut line, &mut pending, event)
            };
        }

        // Host, on serial.
        let count = uart.read(&mut input, 0).unwrap_or(0);
        for &byte in &input[..count] {
            if byte != b'\n' {
                if typed.len() < 63 {
                    typed.push(byte as char);
                }
                continue;
            }
            if typed.starts_with('H') {
                mesh.host_heartbeat(now);
            } else if typed.starts_with('D') {
                if bt.set_visible(true).is_ok() {
                    visible_until = Some(now.wrapping_add(VISIBLE_MS));
                }
            } else if typed.starts_with('C') {
                if let Some(command) = parse_command(&typed) {
                    mesh.send_command(&command, events!());
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

        // One frame on the air at a time, so ordering and acks stay simple.
        if let Some(started) = in_flight {
            let outcome = radio::sent();
            if outcome.is_some() || now.wrapping_sub(started) > TX_TIMEOUT_MS {
                let acked = outcome.unwrap_or(false);
                if acked {
                    tx_ok = tx_ok.wrapping_add(1);
                } else {
                    tx_failed = tx_failed.wrapping_add(1);
                }
                in_flight = None;
                mesh.tx_done(acked, events!());
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
                match radio::send(target, tx.bytes()) {
                    Ok(()) => in_flight = Some(now),
                    Err(_) => {
                        tx_failed = tx_failed.wrapping_add(1);
                        mesh.tx_done(false, events!());
                    }
                }
            }
        }

        // Settings from the host, here or by flood.
        if let Some(new) = pending.take() {
            settings = new;
            let _ = bt.configure_scan(new.ble_window_ms, new.ble_interval_ms);
            next_inquiry = now.wrapping_add(1_000);
        }
        let _ = bt.poll();

        if settings.inquiry_period_s > 0 && (now.wrapping_sub(next_inquiry) as i32) >= 0 {
            next_inquiry = now.wrapping_add(settings.inquiry_period_s as u32 * 1000);
            let _ = bt.inquire(settings.inquiry_length);
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
            let fields = [
                now / 1000,
                free,
                least,
                block,
                heard.adverts.load(Ordering::Relaxed),
                heard.classic.load(Ordering::Relaxed),
                heard.inquiries.load(Ordering::Relaxed),
                radio::RX_FRAMES.load(Ordering::Relaxed),
                radio::RX_DROPS.load(Ordering::Relaxed),
                mesh.rx_bad,
                mesh.rx_dup,
                tx_ok,
                tx_failed,
                mesh.queue_drops,
                mesh.parent_changes,
                evicted,
                mesh.parent().map_or(0, |parent| parent.id),
                settings.id as u32,
                heard.inquiry_started.load(Ordering::Relaxed),
                heard.inquiry_stopped.load(Ordering::Relaxed),
                heard.inquiry_errors.load(Ordering::Relaxed),
            ];
            let mut body = [0u8; 4 * 21 + 4];
            for (index, field) in fields.iter().enumerate() {
                body[index * 4..index * 4 + 4].copy_from_slice(&field.to_le_bytes());
            }
            body[84] = mesh.rank;
            body[85] = mesh.neighbours().count() as u8;
            body[86] = mesh.parent().map_or(0, |parent| parent.rssi as u8);
            body[87] = mesh.parent().map_or(0, |parent| parent.quality() as u8);
            mesh.send_up(FrameKind::Stats, &body, now, events!());
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
