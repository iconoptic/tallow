//! OV3660 on the Keyestudio MB0200, through Espressif's esp32-camera.
//!
//! Pins and frame settings are in docs/devices/keyestudio-mb0200-s3-cam.md
//! (Pins). BOARD_ESP32S3_WROOM is the wrong pick: its PWDN on IO38 is the SD
//! card's CMD on this board, so the pins are written out here.
//!
//! Frames are JPEG, 800x600 (SVGA), quality 12, with both frame buffers in
//! PSRAM. Each `Frame` returns its buffer to the driver when it is dropped.

use core::marker::PhantomData;
use core::slice;

use esp_idf_svc::sys::{camera as cam, esp, EspError};

// Y2..Y9 on the board are D0..D7 in the driver.
const PIN_XCLK: i32 = 15;
const PIN_SIOD: i32 = 4;
const PIN_SIOC: i32 = 5;
const PIN_D0: i32 = 11;
const PIN_D1: i32 = 9;
const PIN_D2: i32 = 8;
const PIN_D3: i32 = 10;
const PIN_D4: i32 = 12;
const PIN_D5: i32 = 18;
const PIN_D6: i32 = 17;
const PIN_D7: i32 = 16;
const PIN_VSYNC: i32 = 6;
const PIN_HREF: i32 = 7;
const PIN_PCLK: i32 = 13;

/// The camera driver is global: call `init` once, then take frames from it.
pub struct Camera;

impl Camera {
    /// Starts the sensor, XCLK, and the frame buffers.
    pub fn init() -> Result<Self, EspError> {
        let config = cam::camera_config_t {
            pin_pwdn: -1,
            pin_reset: -1,
            pin_xclk: PIN_XCLK,
            __bindgen_anon_1: cam::camera_config_t__bindgen_ty_1 {
                pin_sccb_sda: PIN_SIOD,
            },
            __bindgen_anon_2: cam::camera_config_t__bindgen_ty_2 {
                pin_sccb_scl: PIN_SIOC,
            },
            pin_d7: PIN_D7,
            pin_d6: PIN_D6,
            pin_d5: PIN_D5,
            pin_d4: PIN_D4,
            pin_d3: PIN_D3,
            pin_d2: PIN_D2,
            pin_d1: PIN_D1,
            pin_d0: PIN_D0,
            pin_vsync: PIN_VSYNC,
            pin_href: PIN_HREF,
            pin_pclk: PIN_PCLK,
            xclk_freq_hz: 20_000_000,
            ledc_timer: cam::ledc_timer_t_LEDC_TIMER_0,
            ledc_channel: cam::ledc_channel_t_LEDC_CHANNEL_0,
            pixel_format: cam::pixformat_t_PIXFORMAT_JPEG,
            frame_size: cam::framesize_t_FRAMESIZE_SVGA,
            jpeg_quality: 12,
            fb_count: 2,
            fb_location: cam::camera_fb_location_t_CAMERA_FB_IN_PSRAM,
            grab_mode: cam::camera_grab_mode_t_CAMERA_GRAB_LATEST,
            // Unused: the SCCB pins above are set, so the driver owns its own bus.
            sccb_i2c_port: -1,
        };
        // SAFETY: `config` is a complete camera_config_t that lives for this call.
        // esp_camera_init copies it (`s_saved_config = *config`) and keeps no
        // pointer to it. The driver is only started here, from the main task.
        esp!(unsafe { cam::esp_camera_init(&config) })?;
        Ok(Camera)
    }

    /// Takes the newest frame, or `None` if the driver has none ready.
    /// The `&mut self` borrow means at most one frame is out at a time.
    pub fn capture(&mut self) -> Option<Frame<'_>> {
        // SAFETY: esp_camera_fb_get takes no arguments. It returns null or a
        // buffer the driver holds until esp_camera_fb_return. The non-null
        // result is wrapped in a Frame, whose Drop returns it exactly once.
        let fb = unsafe { cam::esp_camera_fb_get() };
        if fb.is_null() {
            None
        } else {
            Some(Frame {
                fb,
                _camera: PhantomData,
            })
        }
    }
}

/// One captured frame. Dropping it returns the buffer to the driver, so an
/// early return or an error path cannot leak it.
pub struct Frame<'a> {
    fb: *mut cam::camera_fb_t,
    _camera: PhantomData<&'a mut Camera>,
}

impl Frame<'_> {
    /// The JPEG bytes. The slice borrows the frame, so it cannot outlive the buffer.
    pub fn jpeg(&self) -> &[u8] {
        // SAFETY: `fb` is non-null (checked in `capture`) and is not returned
        // until this Frame drops. The driver has written `len` bytes at `buf`.
        // A null `buf` is an empty frame, so it is not passed to from_raw_parts.
        unsafe {
            let fb = &*self.fb;
            if fb.buf.is_null() {
                &[]
            } else {
                slice::from_raw_parts(fb.buf, fb.len)
            }
        }
    }

    /// Width and height in pixels.
    pub fn size(&self) -> (usize, usize) {
        // SAFETY: same as `jpeg`. `fb` is valid while this Frame exists, and the
        // two fields are plain integers that the driver sets before handing it over.
        unsafe { ((*self.fb).width, (*self.fb).height) }
    }
}

impl Drop for Frame<'_> {
    fn drop(&mut self) {
        // SAFETY: `fb` came from esp_camera_fb_get and has not been returned,
        // because Frame is the only owner and this is its single drop. After this
        // call the pointer is not used again.
        unsafe { cam::esp_camera_fb_return(self.fb) };
    }
}
