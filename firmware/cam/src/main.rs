//! Firmware for a Keyestudio MB0200 camera.
//!
//! This build has no Bluetooth and joins no network. Serial output is the
//! USB Serial/JTAG port.

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

fn main() {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    boot();
    loop {
        FreeRtos::delay_ms(1000);
    }
}
