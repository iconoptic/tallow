//! Firmware for the Keyestudio MB0200 camera `toby-cam-0`.
//!
//! It joins `tallow-iot` at `10.87.0.23` and serves MJPEG on port 8080.
//! Port 6053 accepts a TCP connection so the panel's status check can pass.
//! Port 3232 takes one signed image, and the cam reboots into it. There is no
//! Bluetooth and no fallback access point. Serial output is the USB Serial/JTAG
//! port. None of this has run on a board.

#![warn(clippy::undocumented_unsafe_blocks)]

mod camera;
mod net;
mod ota;
mod push;
mod serve;
mod stream;

use camera::Camera;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys;

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

fn main() {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    boot();
    // First, so a new image that hangs anywhere below still rolls back.
    ota::arm_rollback();
    // Without NVS, Wi-Fi still runs. The floor then refuses every push.
    let nvs = EspDefaultNvsPartition::take().ok();
    let versions = ota::Versions::open(nvs.clone());
    let mut camera = match Camera::init() {
        Ok(camera) => Some(camera),
        Err(err) => {
            println!("camera init failed: {err}");
            None
        }
    };
    let mut link = net::join(nvs);
    let mut logged_frame = false;
    let mut listeners = loop {
        match serve::Listeners::bind() {
            Ok(listeners) => break listeners,
            Err(err) => {
                println!("listen failed: {err}");
                FreeRtos::delay_ms(1000);
            }
        }
    };
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
    loop {
        if !link.up() {
            println!("wifi down");
            if link.reconnect().is_err() {
                println!("wifi join failed");
            }
        }
        let now = ota::now_ms();
        updates.note_link(link.up(), now);
        listeners.poll();
        updates.poll();
        // A push owns the loop. Capture blocks for a frame, and with it each
        // pass would take only a few chunks.
        if !updates.busy() {
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
        updates.confirm_at(ota::now_ms());
        let pause = if updates.busy() {
            1
        } else if listeners.streaming() {
            200
        } else {
            1000
        };
        FreeRtos::delay_ms(pause);
    }
}
