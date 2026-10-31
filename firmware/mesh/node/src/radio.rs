//! ESP-NOW through `esp-idf-sys`.
//!
//! The safe `EspNow` wrapper turns Wi-Fi modem sleep off, which ESP-IDF forbids
//! while Bluetooth runs, and it drops each frame's signal strength. So this file
//! is the small unsafe seam instead.

use core::ffi::c_int;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::OnceLock;

use esp_idf_svc::sys::*;
use tallow_mesh_core::MAX_FRAME;

pub const BROADCAST: [u8; 6] = [0xFF; 6];
const RX_QUEUE: usize = 12;

pub struct Rx {
    pub src: [u8; 6],
    pub rssi: i8,
    pub length: u8,
    pub data: [u8; MAX_FRAME],
}

const NOTHING: u8 = 0;
const SENT: u8 = 1;
const FAILED: u8 = 2;

static RX: OnceLock<SyncSender<Rx>> = OnceLock::new();
static TX_RESULT: AtomicU8 = AtomicU8::new(NOTHING);
pub static RX_FRAMES: AtomicU32 = AtomicU32::new(0);
pub static RX_DROPS: AtomicU32 = AtomicU32::new(0);

/// Runs on the Wi-Fi task. It only copies the frame and hands it off.
unsafe extern "C" fn on_receive(info: *const esp_now_recv_info_t, data: *const u8, length: c_int) {
    let Ok(length) = usize::try_from(length) else {
        return;
    };
    if info.is_null() || data.is_null() || length > MAX_FRAME {
        return;
    }
    let info = &*info;
    if info.src_addr.is_null() {
        return;
    }
    let mut frame = Rx {
        src: [0; 6],
        rssi: -127,
        length: length as u8,
        data: [0; MAX_FRAME],
    };
    frame
        .src
        .copy_from_slice(core::slice::from_raw_parts(info.src_addr, 6));
    if !info.rx_ctrl.is_null() {
        // The field is 8 bits of a bitfield, so the low byte holds the sign.
        frame.rssi = (*info.rx_ctrl).rssi() as u8 as i8;
    }
    frame.data[..length].copy_from_slice(core::slice::from_raw_parts(data, length));
    RX_FRAMES.fetch_add(1, Ordering::Relaxed);
    if let Some(queue) = RX.get() {
        if queue.try_send(frame).is_err() {
            RX_DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

unsafe extern "C" fn on_sent(_info: *const esp_now_send_info_t, status: esp_now_send_status_t) {
    let result = if status == esp_now_send_status_t_ESP_NOW_SEND_SUCCESS {
        SENT
    } else {
        FAILED
    };
    TX_RESULT.store(result, Ordering::Release);
}

fn check(code: esp_err_t) -> Result<(), EspError> {
    esp!(code)
}

/// Starts ESP-NOW on `channel`. Wi-Fi must already be started in STA mode.
pub fn start(channel: u8) -> Result<Receiver<Rx>, EspError> {
    let (sender, receiver) = sync_channel(RX_QUEUE);
    let _ = RX.set(sender);
    unsafe {
        check(esp_wifi_set_channel(
            channel,
            wifi_second_chan_t_WIFI_SECOND_CHAN_NONE,
        ))?;
        check(esp_now_init())?;
        check(esp_now_register_recv_cb(Some(on_receive)))?;
        check(esp_now_register_send_cb(Some(on_sent)))?;
    }
    ensure_peer(BROADCAST)?;
    Ok(receiver)
}

pub fn ensure_peer(mac: [u8; 6]) -> Result<(), EspError> {
    unsafe {
        if esp_now_is_peer_exist(mac.as_ptr()) {
            return Ok(());
        }
        let mut peer = esp_now_peer_info_t::default();
        peer.peer_addr = mac;
        peer.channel = 0;
        peer.ifidx = wifi_interface_t_WIFI_IF_STA;
        // Frames are sealed by the mesh, so the 7-peer limit on ESP-NOW's own
        // encryption does not apply.
        peer.encrypt = false;
        check(esp_now_add_peer(&peer))
    }
}

pub fn drop_peer(mac: [u8; 6]) {
    unsafe {
        esp_now_del_peer(mac.as_ptr());
    }
}

pub fn send(mac: [u8; 6], data: &[u8]) -> Result<(), EspError> {
    TX_RESULT.store(NOTHING, Ordering::Release);
    ensure_peer(mac)?;
    unsafe { check(esp_now_send(mac.as_ptr(), data.as_ptr(), data.len())) }
}

/// The last send's outcome, once the radio reports it.
pub fn sent() -> Option<bool> {
    match TX_RESULT.load(Ordering::Acquire) {
        SENT => Some(true),
        FAILED => Some(false),
        _ => None,
    }
}
