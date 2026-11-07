//! An ESP-NOW leaf. Frames go to peer MACs and nowhere else.
//!
//! This module does not read or write a socket. An OTA payload is queued for
//! the update module, which writes the flash. A socket byte is never sent on
//! ESP-NOW, and a mesh frame is never written to a socket. The leaf never
//! becomes the root: nothing here calls `host_heartbeat`.

use std::sync::mpsc::Receiver;

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};
use esp_idf_svc::sys::{self, EspError};
use tallow_mesh_core::mesh::{next_boot, CHANNEL, MAX_NEIGHBOURS};
use tallow_mesh_core::ota::Identity;
use tallow_mesh_core::stats::{Stats, HEARD_WIRE};
use tallow_mesh_core::{Dest, Event, FrameKind, Mesh, MAX_FRAME};

use crate::radio::{self, Rx};

const KEY: &[u8; 32] = include_bytes!(concat!(env!("OUT_DIR"), "/mesh_key.bin"));
const STATS_MS: u32 = 10_000;
const TX_TIMEOUT_MS: u32 = 250;
/// OTA payloads waiting for the main loop. A payload past this is dropped.
const DOWN_QUEUE: usize = 4;

/// The mesh membership for this camera, present for the life of the process
/// once the station has been on channel [`CHANNEL`].
pub struct MeshLeaf {
    nvs: Option<EspDefaultNvsPartition>,
    mesh: Option<Box<Mesh>>,
    rx: Option<Receiver<Rx>>,
    radio_up: bool,
    logged_mismatch: bool,
    logged_start: bool,
    in_flight: Option<u32>,
    last_peer: Option<[u8; 6]>,
    tx_ok: u32,
    tx_failed: u32,
    next_stats: u32,
}

impl MeshLeaf {
    pub fn open(nvs: Option<EspDefaultNvsPartition>) -> Self {
        Self {
            nvs,
            mesh: None,
            rx: None,
            radio_up: false,
            logged_mismatch: false,
            logged_start: false,
            in_flight: None,
            last_peer: None,
            tx_ok: 0,
            tx_failed: 0,
            next_stats: 0,
        }
    }

    /// Bring ESP-NOW up when the station is on the mesh channel, and take it
    /// down otherwise. A down station leaves the [`Mesh`] in place, so a
    /// reconnect continues the same boot and sequence.
    pub fn note_link(&mut self, up: bool, now: u32) {
        if !up {
            self.radio_down(now);
            return;
        }
        match ap_channel() {
            Some(channel) if channel == CHANNEL => {
                self.logged_mismatch = false;
                self.radio_up(now);
            }
            other => {
                self.radio_down(now);
                if !self.logged_mismatch {
                    match other {
                        Some(channel) => {
                            println!("mesh channel mismatch: access point is {channel}, mesh is {CHANNEL}")
                        }
                        None => println!("mesh channel mismatch: no access point record"),
                    }
                    self.logged_mismatch = true;
                }
            }
        }
    }

    /// One pass: receive, tick, finish or start a send, and send stats when due.
    /// OTA payloads aimed at this leaf are returned for the update module.
    /// Ping and GATT downs are ignored.
    pub fn poll(&mut self, now: u32, identity: Identity) -> Vec<Vec<u8>> {
        let mut downs = Vec::new();
        if !self.radio_up {
            return downs;
        }
        for _ in 0..8 {
            let frame = {
                let Some(rx) = self.rx.as_ref() else {
                    break;
                };
                match rx.try_recv() {
                    Ok(frame) => frame,
                    Err(_) => break,
                }
            };
            let Some(mesh) = self.mesh.as_mut() else {
                break;
            };
            let bytes = &frame.data[..frame.length as usize];
            mesh.receive(now, frame.src, frame.rssi, bytes, &mut |event| {
                if let Event::Down { frame, payload } = event {
                    if frame.kind == FrameKind::Ota && downs.len() < DOWN_QUEUE {
                        downs.push(payload.to_vec());
                    }
                }
            });
        }
        if let Some(mesh) = self.mesh.as_mut() {
            mesh.tick(now, &mut |_| {});
        }
        self.finish_send(now);
        if self.in_flight.is_none() {
            self.send_one(now);
        }
        if (now.wrapping_sub(self.next_stats) as i32) >= 0 {
            self.next_stats = now.wrapping_add(STATS_MS);
            self.send_stats(now);
            self.send_identity(now, identity);
            if self.in_flight.is_none() {
                self.send_one(now);
            }
        }
        downs
    }

    /// Queue one upward frame and start the radio if it is idle.
    pub fn send_up(&mut self, kind: FrameKind, payload: &[u8], now: u32) {
        let Some(mesh) = self.mesh.as_mut() else {
            return;
        };
        mesh.send_up(kind, payload, now, &mut |_| {});
        if self.in_flight.is_none() {
            self.send_one(now);
        }
    }

    /// True when nothing is on the air and nothing is waiting to be sent.
    pub fn tx_idle(&self) -> bool {
        self.in_flight.is_none()
            && self
                .mesh
                .as_ref()
                .is_none_or(|mesh| mesh.next_tx().is_none())
    }

    fn radio_up(&mut self, now: u32) {
        if self.radio_up {
            return;
        }
        self.ensure_mesh(now);
        if self.mesh.is_none() {
            return;
        }
        match radio::start() {
            Ok(rx) => {
                self.rx = Some(rx);
                self.radio_up = true;
                self.logged_start = false;
            }
            Err(err) => {
                if !self.logged_start {
                    println!("mesh radio failed: {err}");
                    self.logged_start = true;
                }
            }
        }
    }

    fn radio_down(&mut self, now: u32) {
        if !self.radio_up {
            return;
        }
        if self.in_flight.take().is_some() {
            self.tx_failed = self.tx_failed.wrapping_add(1);
            if let Some(mesh) = self.mesh.as_mut() {
                mesh.tx_done(now, false, &mut |_| {});
            }
        }
        self.rx = None;
        self.last_peer = None;
        radio::stop();
        self.radio_up = false;
    }

    fn ensure_mesh(&mut self, now: u32) {
        if self.mesh.is_some() {
            return;
        }
        let mac = match station_mac() {
            Ok(mac) => mac,
            Err(err) => {
                if !self.logged_start {
                    println!("mesh mac failed: {err}");
                    self.logged_start = true;
                }
                return;
            }
        };
        // SAFETY: takes no arguments and may be called from any task once the
        // system is up, which it is before the station joins.
        let random = unsafe { sys::esp_random() };
        let boot = boot_id(self.nvs.clone(), random);
        let mut mesh = Mesh::new(mac, KEY, boot);
        mesh.set_leaf();
        self.mesh = Some(Box::new(mesh));
        self.next_stats = now.wrapping_add(1_000);
    }

    fn finish_send(&mut self, now: u32) {
        let Some(started) = self.in_flight else {
            return;
        };
        let outcome = radio::sent();
        let timed_out = now.wrapping_sub(started) > TX_TIMEOUT_MS;
        if outcome.is_none() && !timed_out {
            return;
        }
        let acked = outcome.unwrap_or(false);
        self.in_flight = None;
        if acked {
            self.tx_ok = self.tx_ok.wrapping_add(1);
        } else {
            self.tx_failed = self.tx_failed.wrapping_add(1);
        }
        if let Some(mesh) = self.mesh.as_mut() {
            mesh.tx_done(now, acked, &mut |_| {});
        }
    }

    fn send_one(&mut self, now: u32) {
        if self.in_flight.is_some() {
            return;
        }
        let Some(copied) = self.copy_next() else {
            return;
        };
        if copied.unicast && self.last_peer != Some(copied.target) {
            if let Some(old) = self.last_peer.replace(copied.target) {
                radio::drop_peer(old);
            }
        }
        match radio::send(copied.target, &copied.data[..copied.length]) {
            Ok(()) => self.in_flight = Some(now),
            Err(_) => {
                self.tx_failed = self.tx_failed.wrapping_add(1);
                if let Some(mesh) = self.mesh.as_mut() {
                    mesh.tx_done(now, false, &mut |_| {});
                }
            }
        }
    }

    fn copy_next(&mut self) -> Option<Copied> {
        let mesh = self.mesh.as_ref()?;
        let tx = mesh.next_tx()?;
        let target = match tx.dest {
            Dest::Broadcast => radio::BROADCAST,
            Dest::Peer(mac) => mac,
        };
        let unicast = tx.dest != Dest::Broadcast;
        let length = tx.bytes().len();
        let mut data = [0u8; MAX_FRAME];
        data[..length].copy_from_slice(tx.bytes());
        Some(Copied {
            target,
            unicast,
            length,
            data,
        })
    }

    fn send_stats(&mut self, now: u32) {
        let (free, least, block) = heap();
        let tx_ok = self.tx_ok;
        let tx_failed = self.tx_failed;
        let Some(mesh) = self.mesh.as_mut() else {
            return;
        };
        let stats = Stats {
            uptime_s: now / 1000,
            heap_free: free,
            heap_min: least,
            heap_block: block,
            adverts: 0,
            classic: 0,
            inquiries: 0,
            rx_frames: radio::RX_FRAMES.load(std::sync::atomic::Ordering::Relaxed),
            rx_drops: radio::RX_DROPS.load(std::sync::atomic::Ordering::Relaxed),
            rx_bad: mesh.rx_bad,
            rx_dup: mesh.rx_dup,
            tx_ok,
            tx_failed,
            queue_drops: mesh.queue_drops,
            parent_changes: mesh.parent_changes,
            evicted: 0,
            parent: mesh.parent().map_or(0, |parent| parent.id),
            settings: 0,
            inquiry_started: 0,
            inquiry_stopped: 0,
            inquiry_errors: 0,
            rank: mesh.rank,
            neighbours: mesh.neighbours().count() as u8,
            parent_rssi: mesh.parent().map_or(0, |parent| parent.rssi),
            parent_quality: mesh.parent().map_or(0, |parent| parent.quality() as u8),
        };
        mesh.send_up(FrameKind::Stats, &stats.encode(), now, &mut |_| {});
        let mut heard = [0u8; MAX_NEIGHBOURS * HEARD_WIRE];
        let heard_len = mesh.encode_neighbours(&mut heard);
        if heard_len > 0 {
            mesh.send_up(FrameKind::Neighbours, &heard[..heard_len], now, &mut |_| {});
        }
    }

    fn send_identity(&mut self, now: u32, identity: Identity) {
        let Some(mesh) = self.mesh.as_mut() else {
            return;
        };
        let bytes = identity.encode();
        mesh.send_up(FrameKind::Identity, &bytes, now, &mut |_| {});
    }
}

struct Copied {
    target: [u8; 6],
    unicast: bool,
    length: usize,
    data: [u8; MAX_FRAME],
}

/// Free heap, the lowest it has been, and the largest free block.
fn heap() -> (u32, u32, u32) {
    // SAFETY: read-only heap queries. The capability mask is not a pointer, and
    // none of the three calls keeps a reference into the heap.
    unsafe {
        (
            sys::esp_get_free_heap_size(),
            sys::esp_get_minimum_free_heap_size(),
            sys::heap_caps_get_largest_free_block(sys::MALLOC_CAP_8BIT) as u32,
        )
    }
}

/// This boot's id. The NVS counter in `tallow/boot` is read and raised here, once,
/// before `Mesh::new`, and never written again. If NVS fails, the counter is seeded
/// from `random` instead: still unique, but not ordered against earlier boots.
fn boot_id(nvs: Option<EspDefaultNvsPartition>, random: u32) -> u32 {
    let Some(nvs) = nvs else {
        return next_boot(None, random).1;
    };
    let attempt = || -> Result<u32, EspError> {
        let store = EspNvs::new(nvs, "tallow", true)?;
        let (counter, boot) = next_boot(store.get_u32("boot")?, random);
        store.set_u32("boot", counter)?;
        Ok(boot)
    };
    match attempt() {
        Ok(boot) => boot,
        Err(error) => {
            println!("boot counter unavailable ({error})");
            next_boot(None, random).1
        }
    }
}

fn station_mac() -> Result<[u8; 6], EspError> {
    let mut mac = [0u8; 6];
    // SAFETY: Wi-Fi is started, and `mac` is the 6 bytes this call writes.
    sys::esp!(unsafe {
        sys::esp_wifi_get_mac(sys::wifi_interface_t_WIFI_IF_STA, mac.as_mut_ptr())
    })?;
    Ok(mac)
}

/// The access point's primary channel, if the station still has a record of it.
fn ap_channel() -> Option<u8> {
    // SAFETY: `info` is zeroed storage owned by this call. The getter writes
    // that record and does not keep the pointer. Wi-Fi is already started.
    // A failure means there is no access point.
    unsafe {
        let mut info: sys::wifi_ap_record_t = core::mem::zeroed();
        if sys::esp_wifi_sta_get_ap_info(&mut info) != sys::ESP_OK {
            return None;
        }
        Some(info.primary)
    }
}
