//! Firmware for a Keyestudio MB0200 camera.
//!
//! This build has no Bluetooth and joins no network. Serial output is the
//! USB Serial/JTAG port.

mod camera;

use camera::Camera;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::sys;

/// Prints free internal heap and free PSRAM. The IDF heap is up by the time
/// `main` runs, and these queries only read it.
fn boot() {
    // heap_caps_get_free_size adds up free bytes in regions that match a
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

/// Takes one frame and prints its size. The frame goes back to the driver
/// when `frame` drops at the end of the match arm.
fn log_frame(camera: &mut Camera) {
    match camera.capture() {
        Some(frame) => {
            let (width, height) = frame.size();
            println!("frame {width}x{height} jpeg {} bytes", frame.jpeg().len());
        }
        None => println!("no frame ready"),
    }
}

fn main() {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    boot();
    match Camera::init() {
        Ok(mut camera) => loop {
            log_frame(&mut camera);
            FreeRtos::delay_ms(5000);
        },
        Err(err) => println!("camera init failed: {err}"),
    }
    loop {
        FreeRtos::delay_ms(1000);
    }
}
