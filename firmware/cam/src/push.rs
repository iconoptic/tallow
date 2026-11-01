//! Whether a signed cam image may be written, and when the running image
//! may be marked valid.
//!
//! Nothing here touches flash or a socket. The header is a big-endian `u32`
//! size, the product byte, a little-endian `u32` version, the 32-byte SHA-256,
//! and the 64-byte Ed25519 signature. The signature covers
//! `signed_message(product, version, digest)` from the core, not the digest
//! alone. `admit` refuses a size of zero, a size past the cam slot, another
//! product, a version below the floor, or a bad signature, and it does that
//! before the caller takes any image byte. `activate` is the only yes, and
//! only when the bytes that were taken hash to that signed digest.

use tallow_mesh_core::ota::{verify, Code, ImageHash, PRODUCT_CAM0};

/// `app0` and `app1` in the cam partition table. Not the mesh node's slot.
pub const SLOT: u32 = 0x7C_0000;
/// Size, product, version, digest, signature.
pub const HEADER_LEN: usize = 4 + 1 + 4 + 32 + 64;
/// Five minutes, in the wrapping millisecond clock the caller supplies.
pub const FIVE_MIN_MS: u32 = 5 * 60 * 1000;
/// The mark needs a frame this recent. A sensor that died after boot fails it.
pub const FRAME_FRESH_MS: u32 = 5 * 1000;
/// A pending image still not marked by now rolls back. The node uses the same.
pub const ROLLBACK_AFTER_MS: u32 = 15 * 60 * 1000;
/// A connection with no new bytes for this long is dropped.
pub const IDLE_MS: u32 = 30 * 1000;

/// One push. Create it, `admit` the header, `take` the body, then `activate`.
#[derive(Default)]
pub struct Offer {
    admitted: bool,
    size: u32,
    version: u32,
    digest: [u8; 32],
    taken: u32,
    hash: ImageHash,
}

impl Offer {
    pub fn new() -> Self {
        Self::default()
    }

    /// [`Code::Ok`] when this header may be written. Anything else leaves
    /// [`taken`](Self::taken) at zero, and a later [`take`](Self::take) refuses
    /// the body. `floor` is the highest version this board has confirmed. An
    /// equal version is a retry and passes.
    pub fn admit(&mut self, header: &[u8], public_key: &[u8; 32], floor: u32) -> Code {
        *self = Self::new();
        if header.len() != HEADER_LEN {
            return Code::Refused;
        }
        let size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        if size == 0 || size > SLOT {
            return Code::Refused;
        }
        let product = header[4];
        if product != PRODUCT_CAM0 {
            return Code::WrongProduct;
        }
        let version = u32::from_le_bytes([header[5], header[6], header[7], header[8]]);
        if version < floor {
            return Code::Downgrade;
        }
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&header[9..41]);
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&header[41..105]);
        if !verify(public_key, product, version, &digest, &signature) {
            return Code::BadSignature;
        }
        self.admitted = true;
        self.size = size;
        self.version = version;
        self.digest = digest;
        Code::Ok
    }

    /// The signed version of the admitted image. Zero before that.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Image bytes accepted so far. Zero when `admit` refused the header.
    pub fn taken(&self) -> u32 {
        self.taken
    }

    /// Bytes still required. Zero when the header was refused.
    pub fn remaining(&self) -> u32 {
        if !self.admitted {
            return 0;
        }
        self.size - self.taken
    }

    /// Hash the next body bytes. Refuses them when `admit` did not accept,
    /// and does not count a refused slice.
    pub fn take(&mut self, bytes: &[u8]) -> bool {
        if !self.admitted || bytes.is_empty() {
            return false;
        }
        let room = self.size - self.taken;
        if bytes.len() as u32 > room {
            return false;
        }
        self.hash.update(bytes);
        self.taken += bytes.len() as u32;
        true
    }

    /// `true` only when every admitted byte was taken and it hashes to the
    /// signed digest. This is the only activate answer.
    pub fn activate(&self) -> bool {
        if !self.admitted || self.taken != self.size {
            return false;
        }
        self.hash.clone().finish() == self.digest
    }
}

/// The stored floor, from one NVS read. Nothing stored is zero. A failed read
/// refuses every push, because a lost floor must not allow a downgrade.
pub fn floor_from<E>(read: Result<Option<u32>, E>) -> u32 {
    match read {
        Ok(Some(version)) => version,
        Ok(None) => 0,
        Err(_) => u32::MAX,
    }
}

/// The new floor when a confirmed image's pending version is above the old one.
pub fn promoted(floor: u32, pending: Option<u32>) -> Option<u32> {
    pending.filter(|version| *version > floor)
}

/// When a connection last moved bytes. The clock is the caller's and wraps.
pub struct Idle {
    last: u32,
}

impl Idle {
    pub fn new(now_ms: u32) -> Self {
        Self { last: now_ms }
    }

    pub fn note(&mut self, now_ms: u32) {
        self.last = now_ms;
    }

    pub fn expired(&self, now_ms: u32) -> bool {
        now_ms.wrapping_sub(self.last) >= IDLE_MS
    }
}

/// The five-minute mark. Wi-Fi down clears the start. A frame has to have been
/// captured in the last [`FRAME_FRESH_MS`]. The clock is the caller's, and the
/// gaps are wrapping subtractions so a rollover does not look like a huge uptime.
#[derive(Default)]
pub struct Steady {
    since: Option<u32>,
    frame: Option<u32>,
    done: bool,
}

impl Steady {
    pub fn new() -> Self {
        Self::default()
    }

    /// `up` false forgets when the association started. The first `up` true
    /// after that starts it again.
    pub fn note_link(&mut self, up: bool, now_ms: u32) {
        if self.done {
            return;
        }
        if !up {
            self.since = None;
            return;
        }
        if self.since.is_none() {
            self.since = Some(now_ms);
        }
    }

    pub fn note_frame(&mut self, now_ms: u32) {
        self.frame = Some(now_ms);
    }

    /// Both conditions hold, and this image has not been marked yet.
    pub fn due(&self, now_ms: u32) -> bool {
        if self.done {
            return false;
        }
        let (Some(since), Some(frame)) = (self.since, self.frame) else {
            return false;
        };
        now_ms.wrapping_sub(frame) <= FRAME_FRESH_MS && now_ms.wrapping_sub(since) >= FIVE_MIN_MS
    }

    /// The caller has applied the answer, or the running image was not pending.
    pub fn mark(&mut self) {
        self.done = true;
    }
}
