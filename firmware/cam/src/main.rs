//! Firmware for the Keyestudio MB0200 cameras (`toby-cam-N`). Every cam runs this one image.
//!
//! It takes its `tallow-iot` address by DHCP and serves MJPEG on port 8080.
//! Port 6053 accepts a TCP connection so the panel's status check can pass.
//! Port 3232 takes one signed image, and the cam reboots into it. When the
//! access point is on the mesh channel, the cam also speaks ESP-NOW as a leaf
//! and can take the same signed image from the mesh. Peer frames only, never a
//! socket, and never the root. The station and those listeners come up before
//! the sensor starts, so a stuck sensor cannot keep
//! the station off the air. There is no Bluetooth and no fallback access point.
//! Serial output is the USB Serial/JTAG port. None of this has run on a board.

#![warn(clippy::undocumented_unsafe_blocks)]

mod camera;
mod mesh_link;
mod net;
mod ota;
mod push;
mod radio;
mod serve;
mod stream;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use camera::Camera;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys;
use tallow_mesh_core::ota::{encode_up, Up};
use tallow_mesh_core::{FrameKind, MAX_PAYLOAD};

/// Prints free internal heap and free PSRAM. The IDF heap is up by the time
/// `main` runs, and these queries only read it.
fn boot() {
    // SAFETY: heap_caps_get_free_size adds up free bytes in regions that match a
    // capability mask. The mask is not a pointer, the function does not hand
    // back a reference, and it locks the heap itself. Calling it from the
    // main task after the IDF runtime is up is the documented use.
    let (heap, psram) = unsafe {
        (
            sys::heap_caps_get_free_size(sys::MALLOC_CAP_INTERNAL),
            sys::heap_caps_get_free_size(sys::MALLOC_CAP_SPIRAM),
        )
    };
    println!("boot heap {heap} psram {psram}");
}

/// Send one OTA status or result up the mesh. The bytes stay off every socket.
fn send_mesh_up(mesh: &mut mesh_link::MeshLeaf, up: &Up, now: u32) {
    let mut encoded = [0u8; MAX_PAYLOAD];
    if let Some(length) = encode_up(up, &mut encoded) {
        mesh.send_up(FrameKind::OtaUp, &encoded[..length], now);
    }
}

/// Bind 8080 and 6053. Retries until the stack accepts them.
fn listen_panel() -> serve::Listeners {
    loop {
        match serve::Listeners::bind() {
            Ok(listeners) => return listeners,
            Err(err) => {
                println!("listen failed: {err}");
                FreeRtos::delay_ms(1000);
            }
        }
    }
}

/// Start the sensor. A failure is a missing camera, and the station stays up.
fn open_camera() -> Option<Camera> {
    match Camera::init() {
        Ok(camera) => Some(camera),
        Err(err) => {
            println!("camera init failed: {err}");
            None
        }
    }
}

fn main() {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    boot();
    // First, so a new image that hangs anywhere below still rolls back.
    ota::arm_rollback();
    // Without NVS, Wi-Fi still runs. The floor then refuses every push.
    let nvs = EspDefaultNvsPartition::take().ok();
    let versions = ota::Versions::open(nvs.clone());
    let mut mesh = mesh_link::MeshLeaf::open(nvs.clone());
    let mut link = net::join(nvs);
    let mut listeners = listen_panel();
    let port = loop {
        match ota::bind() {
            Ok(port) => break port,
            Err(err) => {
                println!("ota listen failed: {err}");
                FreeRtos::delay_ms(1000);
            }
        }
    };
    let mut updates = ota::Listener::new(port, versions);
    let mut mesh_push = ota::MeshPush::open(updates.floor());
    // The sensor can hang inside init. Accept the panel ports on another task
    // until it returns, so that hang cannot leave the station unreachable.
    // This task keeps the station. A second join cannot take the peripherals.
    // OTA verify stays here too: it runs deep, and the accept stack is only
    // for ports 8080 and 6053. The OTA socket is already bound.
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    let accept = thread::Builder::new()
        .name("accept".into())
        .stack_size(8 * 1024)
        .spawn(move || {
            while !stop_flag.load(Ordering::Acquire) {
                listeners.poll();
                FreeRtos::delay_ms(200);
            }
            listeners
        });
    let (mut camera, mut listeners) = match accept {
        Ok(handle) => {
            let camera = open_camera();
            stop.store(true, Ordering::Release);
            let listeners = match handle.join() {
                Ok(listeners) => listeners,
                Err(_) => {
                    println!("accept thread stopped");
                    listen_panel()
                }
            };
            (camera, listeners)
        }
        Err(err) => {
            println!("accept thread failed: {err}");
            let listeners = listen_panel();
            (open_camera(), listeners)
        }
    };
    let mut logged_frame = false;
    let mut misses = 0u8;
    loop {
        let (again, next) = stream::reconnect_now(link.up(), link.heard(), misses);
        misses = next;
        if again {
            println!("wifi down");
            mesh.note_link(false, ota::now_ms());
            if link.reconnect().is_err() {
                println!("wifi join failed");
            }
        }
        let now = ota::now_ms();
        if let Some(floor) = updates.confirm_at(now) {
            mesh_push.raise_floor(floor);
        }
        let identity = updates.identity();
        mesh.note_link(link.up(), now);
        let downs = mesh.poll(now, identity);
        let tcp_busy = updates.busy();
        for body in downs {
            if let Some(up) = mesh_push.apply(&body, updates.versions_mut(), tcp_busy, now) {
                send_mesh_up(&mut mesh, &up, now);
            }
        }
        mesh_push.expire(now, updates.versions_mut());
        updates.note_link(link.up(), now);
        listeners.poll();
        updates.poll(mesh_push.busy());
        // A push owns the loop. Capture blocks for a frame, and with it each
        // pass would take only a few chunks.
        if !updates.busy() && !mesh_push.busy() {
            if let Some(camera) = camera.as_mut() {
                if let Some(frame) = camera.capture() {
                    if !logged_frame {
                        let (width, height) = frame.size();
                        println!("frame {width}x{height} jpeg {} bytes", frame.jpeg().len());
                        logged_frame = true;
                    }
                    updates.note_frame(ota::now_ms());
                    listeners.send_jpeg(frame.jpeg());
                }
            }
        }
        if mesh_push.restart_due(ota::now_ms(), mesh.tx_idle()) {
            // SAFETY: takes no arguments and does not return. The new image is
            // already selected for the next boot, and the result has been queued.
            unsafe { sys::esp_restart() };
        }
        let pause = if updates.busy() || mesh_push.busy() {
            1
        } else if listeners.streaming() {
            200
        } else {
            1000
        };
        FreeRtos::delay_ms(pause);
    }
}
