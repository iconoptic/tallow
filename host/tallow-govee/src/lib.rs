//! Pure codec for the Govee H7020 string lights.
//!
//! Inputs go in and packet bytes come out, and reply bytes go in and a status
//! comes out. There is no Bluetooth, no socket, no clock, and no state. The
//! bytes match the pinned Python codec in `references/protocols-ble/ha-govee-led-ble`,
//! and `tests/vectors.json` is the proof, written by `tools/dump_h7020_vectors.py`.

#![forbid(unsafe_code)]

mod b64;
#[cfg(feature = "ble")]
pub mod ble;
pub mod frame;
pub mod layered;
pub mod scene;
pub mod status;

use std::fmt;

/// A request the codec refuses. The text is the message the Python codec raises,
/// without its exception class name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(pub String);

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

pub(crate) fn refuse<T>(message: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal(message.into()))
}
