//! Checks a signed OTA image. Nothing here holds a key: the caller passes the
//! public key in, so this builds and tests with no key file.
//!
//! The image is hashed with SHA-256 as its chunks arrive. The signature is
//! Ed25519 over [`signed_message`]: a fixed context, the product, the version,
//! and that 32-byte digest. So a signature for one product does not fit another,
//! and a version cannot be changed without the key. `ed25519-compact` and `sha2`
//! do the arithmetic and the hashing. This file only feeds them and reads the
//! answer.
//!
//! The mesh key does not take part. A frame that passes the mesh check is not
//! enough to change a node's firmware.
//!
//! `Receiver` is the transfer. It takes the downward payloads, after the target
//! id has been stripped, and yields one action at a time: begin a flash write,
//! write these bytes at this offset, abort the write, or finish. Begin is checked
//! before any flash is touched: size, product, version floor, then signature.
//! Finish is the only action that may activate an image, and it is yielded only
//! after Commit hashes the bytes that were written and `verify` accepts them
//! again. The public key is an argument. Nothing here stores a default key or the
//! image.

use ed25519_compact::{PublicKey, Signature};
use sha2::{Digest, Sha256};

/// Length of the digest that the signature covers.
pub const DIGEST: usize = 32;

/// SHA-256 of an image, fed in chunks as they arrive. Any split of the same
/// bytes gives the same digest.
#[derive(Clone)]
pub struct ImageHash {
    state: Sha256,
}

impl Default for ImageHash {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageHash {
    pub fn new() -> Self {
        Self {
            state: Sha256::new(),
        }
    }

    pub fn update(&mut self, chunk: &[u8]) {
        self.state.update(chunk);
    }

    pub fn finish(self) -> [u8; DIGEST] {
        let mut out = [0u8; DIGEST];
        out.copy_from_slice(&self.state.finalize());
        out
    }
}

/// The product id of a mesh node image.
pub const PRODUCT_NODE: u8 = 1;
/// The product id of a `toby-cam-0` image.
pub const PRODUCT_CAM0: u8 = 2;

/// Bytes in an [`Identity`] payload.
pub const IDENTITY_WIRE: usize = 9;

/// What a board is running, sent up so the host can decide whether to push.
///
/// `running` is the image that is executing. `floor` is the highest version
/// this board has confirmed. A board that has not reported this is not updated.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Identity {
    pub product: u8,
    pub running: u32,
    pub floor: u32,
}

impl Identity {
    /// The 9-byte payload: product, running version, floor, each integer little-endian.
    pub fn encode(self) -> [u8; IDENTITY_WIRE] {
        let mut out = [0u8; IDENTITY_WIRE];
        out[0] = self.product;
        out[1..5].copy_from_slice(&self.running.to_le_bytes());
        out[5..9].copy_from_slice(&self.floor.to_le_bytes());
        out
    }

    /// `None` unless the payload is exactly [`IDENTITY_WIRE`] bytes.
    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() != IDENTITY_WIRE {
            return None;
        }
        Some(Self {
            product: payload[0],
            running: u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]),
            floor: u32::from_le_bytes([payload[5], payload[6], payload[7], payload[8]]),
        })
    }
}

/// The version a board reports as running.
///
/// A pending-verify boot reports the stored pending version, so the host does
/// not push the same image over one that is still proving itself. A missing
/// pending record reports [`u32::MAX`] for the same reason. Any other boot
/// reports the confirmed floor.
pub fn running_version(pending_verify: bool, pending: Option<u32>, floor: u32) -> u32 {
    if pending_verify {
        pending.unwrap_or(u32::MAX)
    } else {
        floor
    }
}

/// The fixed prefix of the signed message. A change to the layout changes this.
pub const SIGNED_CONTEXT: &[u8; 13] = b"tallow-ota-v1";
/// Length of [`signed_message`].
pub const SIGNED: usize = SIGNED_CONTEXT.len() + 1 + 4 + DIGEST;

/// What the signature covers: the context, one product byte, the version as a
/// little-endian `u32`, then the digest.
pub fn signed_message(product: u8, version: u32, digest: &[u8; DIGEST]) -> [u8; SIGNED] {
    let context = SIGNED_CONTEXT.len();
    let mut out = [0u8; SIGNED];
    out[..context].copy_from_slice(SIGNED_CONTEXT);
    out[context] = product;
    out[context + 1..context + 5].copy_from_slice(&version.to_le_bytes());
    out[context + 5..].copy_from_slice(digest);
    out
}

/// True when `signature` is an Ed25519 signature by `public_key` over
/// [`signed_message`] of these values.
///
/// A malformed key, a small-order key, a non-canonical signature, or a wrong
/// signature all return false. None of them panic.
pub fn verify(
    public_key: &[u8; 32],
    product: u8,
    version: u32,
    digest: &[u8; DIGEST],
    signature: &[u8; 64],
) -> bool {
    check(
        public_key,
        &signed_message(product, version, digest),
        signature,
    )
}

/// Most bytes of image one Chunk carries. `send_down` takes four bytes for the
/// target id, and 175 bytes of Chunk still fit in what remains.
pub const CHUNK: usize = 170;
/// The app slot in the classic node's `partitions.csv`, `app0` and `app1`.
pub const SLOT: u32 = 0x1C_0000;
/// The app slot in the camera partition table, `app0` and `app1`.
pub const CAM_SLOT: u32 = 0x7C_0000;
/// Chunks in one Status bitmap.
pub const WINDOW: usize = 256;
/// Bytes in that bitmap.
pub const WINDOW_BYTES: usize = WINDOW / 8;

/// Bytes in a bitmap that can name every chunk of an image of `slot` bytes.
pub const fn bitmap_bytes(slot: u32) -> usize {
    (slot as usize).div_ceil(CHUNK).div_ceil(8)
}

/// The most chunks an image of [`SLOT`] bytes can need.
pub const MAX_CHUNKS: usize = (SLOT as usize).div_ceil(CHUNK);
/// One bit per chunk of [`SLOT`]. The node receiver uses this and nothing larger.
pub const BITMAP: usize = MAX_CHUNKS.div_ceil(8);
/// One bit per chunk of [`CAM_SLOT`]. Only a camera receiver uses this.
pub const CAM_BITMAP: usize = bitmap_bytes(CAM_SLOT);

const _: () = assert!(BITMAP == bitmap_bytes(SLOT));
const _: () = assert!(CAM_BITMAP == bitmap_bytes(CAM_SLOT));

const KIND_BEGIN: u8 = 1;
const KIND_CHUNK: u8 = 2;
const KIND_QUERY: u8 = 3;
const KIND_COMMIT: u8 = 4;
const KIND_STATUS: u8 = 1;
const KIND_RESULT: u8 = 2;

/// Why a Commit or a refused message ended the way it did.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Code {
    Ok = 0,
    BadSignature = 1,
    BadHash = 2,
    Missing = 3,
    Refused = 4,
    /// The version is below the highest this board has confirmed.
    Downgrade = 5,
    /// The image is for another product.
    WrongProduct = 6,
}

impl Code {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Ok,
            1 => Self::BadSignature,
            2 => Self::BadHash,
            3 => Self::Missing,
            4 => Self::Refused,
            5 => Self::Downgrade,
            6 => Self::WrongProduct,
            _ => return None,
        })
    }
}

/// One downward OTA message, after the four-byte target id.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Down {
    Begin {
        size: u32,
        product: u8,
        version: u32,
        digest: [u8; DIGEST],
        signature: [u8; 64],
    },
    Chunk {
        offset: u32,
        len: u8,
        data: [u8; CHUNK],
    },
    Query {
        window: u16,
    },
    Commit,
}

/// One upward OTA message.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Up {
    /// Bit `i` of `missing` is set when chunk `window * 256 + i` has not been
    /// accepted. Bits past the image are clear.
    Status {
        window: u16,
        missing: [u8; WINDOW_BYTES],
    },
    Result {
        code: Code,
    },
}

/// What the node does with flash. [`Action::Finish`] is the only one that may
/// activate the image.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Begin {
        size: u32,
    },
    Write {
        offset: u32,
        len: u16,
        data: [u8; CHUNK],
    },
    /// Drop the write that [`Action::Begin`] opened. The image must not boot.
    Abort,
    /// The hash and the signature both matched. The node may set the boot
    /// partition. `version` is what was signed. It is not confirmed until the new
    /// image has proved itself, so it does not raise the floor yet.
    Finish {
        version: u32,
    },
}

/// Bytes in a Begin, with its sub-kind byte.
pub const BEGIN: usize = 106;

/// Writes a downward message in the layout [`decode_down`] reads.
pub fn encode_down(message: &Down, out: &mut [u8]) -> Option<usize> {
    match *message {
        Down::Begin {
            size,
            product,
            version,
            digest,
            signature,
        } => {
            if out.len() < BEGIN {
                return None;
            }
            out[0] = KIND_BEGIN;
            out[1..5].copy_from_slice(&size.to_le_bytes());
            out[5] = product;
            out[6..10].copy_from_slice(&version.to_le_bytes());
            out[10..42].copy_from_slice(&digest);
            out[42..106].copy_from_slice(&signature);
            Some(BEGIN)
        }
        Down::Chunk { offset, len, data } => {
            let len = usize::from(len);
            if len == 0 || len > CHUNK || out.len() < 5 + len {
                return None;
            }
            out[0] = KIND_CHUNK;
            out[1..5].copy_from_slice(&offset.to_le_bytes());
            out[5..5 + len].copy_from_slice(data.get(..len)?);
            Some(5 + len)
        }
        Down::Query { window } => {
            if out.len() < 3 {
                return None;
            }
            out[0] = KIND_QUERY;
            out[1..3].copy_from_slice(&window.to_le_bytes());
            Some(3)
        }
        Down::Commit => {
            *out.first_mut()? = KIND_COMMIT;
            Some(1)
        }
    }
}

/// Reads a downward message. A short buffer, an unknown kind, or a body of the
/// wrong length is `None`.
pub fn decode_down(data: &[u8]) -> Option<Down> {
    match *data.first()? {
        KIND_BEGIN => {
            if data.len() != BEGIN {
                return None;
            }
            let size = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
            let product = data[5];
            let version = u32::from_le_bytes([data[6], data[7], data[8], data[9]]);
            let mut digest = [0u8; DIGEST];
            digest.copy_from_slice(data.get(10..42)?);
            let mut signature = [0u8; 64];
            signature.copy_from_slice(data.get(42..106)?);
            Some(Down::Begin {
                size,
                product,
                version,
                digest,
                signature,
            })
        }
        KIND_CHUNK => {
            let offset =
                u32::from_le_bytes([*data.get(1)?, *data.get(2)?, *data.get(3)?, *data.get(4)?]);
            let body = data.get(5..)?;
            if body.is_empty() || body.len() > CHUNK {
                return None;
            }
            let mut bytes = [0u8; CHUNK];
            bytes[..body.len()].copy_from_slice(body);
            Some(Down::Chunk {
                offset,
                len: body.len() as u8,
                data: bytes,
            })
        }
        KIND_QUERY => {
            if data.len() != 3 {
                return None;
            }
            Some(Down::Query {
                window: u16::from_le_bytes([data[1], data[2]]),
            })
        }
        KIND_COMMIT => (data.len() == 1).then_some(Down::Commit),
        _ => None,
    }
}

/// Writes an upward message in the layout [`decode_up`] reads.
pub fn encode_up(message: &Up, out: &mut [u8]) -> Option<usize> {
    match *message {
        Up::Status { window, missing } => {
            if out.len() < 3 + WINDOW_BYTES {
                return None;
            }
            out[0] = KIND_STATUS;
            out[1..3].copy_from_slice(&window.to_le_bytes());
            out[3..3 + WINDOW_BYTES].copy_from_slice(&missing);
            Some(3 + WINDOW_BYTES)
        }
        Up::Result { code } => {
            if out.len() < 2 {
                return None;
            }
            out[0] = KIND_RESULT;
            out[1] = code as u8;
            Some(2)
        }
    }
}

/// Reads an upward message. An unknown kind or status code is `None`.
pub fn decode_up(data: &[u8]) -> Option<Up> {
    match *data.first()? {
        KIND_STATUS => {
            if data.len() != 3 + WINDOW_BYTES {
                return None;
            }
            let mut missing = [0u8; WINDOW_BYTES];
            missing.copy_from_slice(data.get(3..3 + WINDOW_BYTES)?);
            Some(Up::Status {
                window: u16::from_le_bytes([data[1], data[2]]),
                missing,
            })
        }
        KIND_RESULT => {
            if data.len() != 2 {
                return None;
            }
            Some(Up::Result {
                code: Code::from_u8(*data.get(1)?)?,
            })
        }
        _ => None,
    }
}

#[derive(Copy, Clone)]
enum Step {
    None,
    Begin,
    Write,
    Abort,
    Finish,
}

/// A transfer that hears nothing for this long is dropped, so a dead host cannot
/// keep a node from listening.
pub const SILENCE_MS: u32 = 60_000;

/// One image being received. Chunks are accepted only in order, so the hash is
/// the hash of the bytes [`Action::Write`] carried, and a chunk that arrives
/// early is still missing. A chunk that was already accepted is ignored.
///
/// `MAX` is the largest image this receiver will accept. `N` is the bitmap for
/// that slot, from [`bitmap_bytes`]. The defaults are the classic node slot, so
/// a node receiver stays the size the 12 KB budget measures. A camera passes
/// [`CAM_SLOT`] and [`CAM_BITMAP`].
pub struct Receiver<const MAX: u32 = SLOT, const N: usize = BITMAP> {
    product: u8,
    floor: u32,
    active: bool,
    size: u32,
    version: u32,
    digest: [u8; DIGEST],
    signature: [u8; 64],
    hash: ImageHash,
    cursor: u32,
    bitmap: [u8; N],
    steps: [Step; 2],
    step_len: u8,
    write_at: u32,
    write_len: u16,
    write_data: [u8; CHUNK],
}

impl<const MAX: u32, const N: usize> Receiver<MAX, N> {
    /// `product` is what this board runs. `floor` is the highest version it has
    /// confirmed. An image below `floor` is refused, and an equal one is a retry.
    pub fn new(product: u8, floor: u32) -> Self {
        Self {
            product,
            floor,
            active: false,
            size: 0,
            version: 0,
            digest: [0; DIGEST],
            signature: [0; 64],
            hash: ImageHash::new(),
            cursor: 0,
            bitmap: [0; N],
            steps: [Step::None; 2],
            step_len: 0,
            write_at: 0,
            write_len: 0,
            write_data: [0; CHUNK],
        }
    }

    /// True from a good Begin until the transfer ends. A board holds off inquiry
    /// and paging for this long.
    pub fn in_progress(&self) -> bool {
        self.active
    }

    /// Raises the floor once a newer image is confirmed. It never goes down.
    pub fn raise_floor(&mut self, version: u32) {
        self.floor = self.floor.max(version);
    }

    /// Ends the transfer, and queues [`Action::Abort`] if one was running. A later
    /// Commit is refused. The board calls this when flash has failed.
    pub fn abandon(&mut self) {
        if !self.active {
            return;
        }
        self.clear_steps();
        self.queue(Step::Abort);
        self.reset();
    }

    /// Abandons a transfer that has heard nothing since `last_ms`. True if it did.
    pub fn expire(&mut self, now_ms: u32, last_ms: u32) -> bool {
        if !self.active || now_ms.wrapping_sub(last_ms) < SILENCE_MS {
            return false;
        }
        self.abandon();
        true
    }

    /// The next flash action, or `None`. Each one is returned once.
    pub fn take_action(&mut self) -> Option<Action> {
        if self.step_len == 0 {
            return None;
        }
        let step = self.steps[0];
        self.steps[0] = self.steps[1];
        self.steps[1] = Step::None;
        self.step_len -= 1;
        Some(match step {
            Step::None => return None,
            Step::Begin => Action::Begin { size: self.size },
            Step::Write => Action::Write {
                offset: self.write_at,
                len: self.write_len,
                data: self.write_data,
            },
            Step::Abort => Action::Abort,
            Step::Finish => Action::Finish {
                version: self.version,
            },
        })
    }

    /// Handles one downward payload. `public_key` is used for a Begin and a
    /// Commit. Returns the status or result to send up, if there is one.
    ///
    /// Take every queued action before the next push. Until then a Begin, Chunk,
    /// or Commit is ignored, since it could overwrite or drop an action flash has
    /// not seen, and a Finish could follow bytes that were hashed but never
    /// written.
    pub fn push(&mut self, payload: &[u8], public_key: &[u8; 32]) -> Option<Up> {
        let down = decode_down(payload)?;
        if self.step_len != 0 && !matches!(down, Down::Query { .. }) {
            return None;
        }
        match down {
            Down::Begin {
                size,
                product,
                version,
                digest,
                signature,
            } => self.on_begin(size, product, version, digest, signature, public_key),
            Down::Chunk { offset, len, data } => self.on_chunk(offset, &data[..usize::from(len)]),
            Down::Query { window } => Some(self.on_query(window)),
            Down::Commit => self.on_commit(public_key),
        }
    }

    /// Checks come before any change of state, so a refused Begin leaves the image
    /// in progress alone and never reaches flash. An erase only follows a good
    /// signature.
    fn on_begin(
        &mut self,
        size: u32,
        product: u8,
        version: u32,
        digest: [u8; DIGEST],
        signature: [u8; 64],
        public_key: &[u8; 32],
    ) -> Option<Up> {
        let refusal = if size == 0 || size > MAX {
            Some(Code::Refused)
        } else if product != self.product {
            Some(Code::WrongProduct)
        } else if version < self.floor {
            Some(Code::Downgrade)
        } else if !verify(public_key, product, version, &digest, &signature) {
            Some(Code::BadSignature)
        } else {
            None
        };
        if let Some(code) = refusal {
            return Some(Up::Result { code });
        }
        let restart = self.active;
        self.clear_steps();
        self.active = true;
        self.size = size;
        self.version = version;
        self.digest = digest;
        self.signature = signature;
        self.hash = ImageHash::new();
        self.cursor = 0;
        self.bitmap = [0; N];
        if restart {
            self.queue(Step::Abort);
        }
        self.queue(Step::Begin);
        None
    }

    fn on_chunk(&mut self, offset: u32, data: &[u8]) -> Option<Up> {
        if !self.active {
            return Some(Up::Result {
                code: Code::Refused,
            });
        }
        let len = data.len();
        if len == 0 || len > CHUNK || !offset.is_multiple_of(CHUNK as u32) || offset >= self.size {
            return None;
        }
        let end = offset.saturating_add(len as u32);
        if end > self.size {
            return None;
        }
        let at_end = end == self.size;
        if (!at_end && len != CHUNK) || (at_end && len != (self.size - offset) as usize) {
            return None;
        }
        if offset < self.cursor {
            return None;
        }
        if offset > self.cursor {
            return None;
        }
        self.mark((offset as usize) / CHUNK);
        self.hash.update(data);
        self.cursor = end;
        self.write_at = offset;
        self.write_len = len as u16;
        self.write_data = [0; CHUNK];
        self.write_data[..len].copy_from_slice(data);
        self.queue(Step::Write);
        None
    }

    fn on_query(&self, window: u16) -> Up {
        if !self.active {
            return Up::Result {
                code: Code::Refused,
            };
        }
        Up::Status {
            window,
            missing: self.missing(window),
        }
    }

    fn on_commit(&mut self, public_key: &[u8; 32]) -> Option<Up> {
        if !self.active {
            return Some(Up::Result {
                code: Code::Refused,
            });
        }
        // The floor can rise after Begin, when the running image is confirmed.
        let code = if self.version < self.floor {
            Code::Downgrade
        } else if self.cursor != self.size {
            Code::Missing
        } else {
            let got = self.hash.clone().finish();
            if got != self.digest {
                Code::BadHash
            } else if !verify(
                public_key,
                self.product,
                self.version,
                &got,
                &self.signature,
            ) {
                Code::BadSignature
            } else {
                Code::Ok
            }
        };
        self.clear_steps();
        self.queue(if code == Code::Ok {
            Step::Finish
        } else {
            Step::Abort
        });
        self.reset();
        Some(Up::Result { code })
    }

    fn reset(&mut self) {
        self.active = false;
        self.size = 0;
        self.cursor = 0;
        self.hash = ImageHash::new();
        self.bitmap = [0; N];
    }

    fn missing(&self, window: u16) -> [u8; WINDOW_BYTES] {
        let mut out = [0u8; WINDOW_BYTES];
        let count = (self.size as usize).div_ceil(CHUNK);
        let base = usize::from(window) * WINDOW;
        for bit in 0..WINDOW {
            let index = base + bit;
            if index < count && !self.have(index) {
                out[bit / 8] |= 1 << (bit % 8);
            }
        }
        out
    }

    fn mark(&mut self, index: usize) {
        if let Some(byte) = self.bitmap.get_mut(index / 8) {
            *byte |= 1 << (index % 8);
        }
    }

    fn have(&self, index: usize) -> bool {
        self.bitmap
            .get(index / 8)
            .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
    }

    fn queue(&mut self, step: Step) {
        if (self.step_len as usize) < self.steps.len() {
            self.steps[self.step_len as usize] = step;
            self.step_len += 1;
        }
    }

    fn clear_steps(&mut self) {
        self.steps = [Step::None; 2];
        self.step_len = 0;
    }
}

/// How long a new image must stay healthy before it is marked valid.
pub const CONFIRM_AFTER_MS: u32 = 5 * 60 * 1000;
/// Free heap, and its low-water mark, both have to be at least this.
pub const HEAP_FLOOR: u32 = 60 * 1024;

/// One reading of the board's state, taken on the stats tick.
#[derive(Copy, Clone, Debug)]
pub struct Sample {
    pub now_ms: u32,
    /// This board is the root.
    pub rooted: bool,
    pub has_parent: bool,
    /// Unicast frames a peer has acknowledged since boot. Broadcasts do not count,
    /// since the radio reports those sent with nobody listening.
    pub acked: u32,
    pub heap_free: u32,
    pub heap_min: u32,
}

/// Whether a new image has earned its valid mark. It needs a route and a clean
/// heap at every reading for [`CONFIRM_AFTER_MS`]. Any reading without them starts
/// the wait again. A leaf must also get a frame acknowledged in that time, since a
/// parent only proves beacons were heard. The root has no peer to ask, and the
/// host's heartbeat is what makes it the root, so it needs no acknowledgement.
#[derive(Copy, Clone, Debug, Default)]
pub struct Health {
    since: Option<u32>,
    acked_then: u32,
}

impl Health {
    pub const fn new() -> Self {
        Self {
            since: None,
            acked_then: 0,
        }
    }

    /// Takes one reading. True when this image has earned its valid mark.
    pub fn sample(&mut self, reading: &Sample) -> bool {
        let linked = reading.rooted || reading.has_parent;
        let clean = reading.heap_free >= HEAP_FLOOR && reading.heap_min >= HEAP_FLOOR;
        if !linked || !clean {
            self.since = None;
            return false;
        }
        let Some(since) = self.since else {
            self.since = Some(reading.now_ms);
            self.acked_then = reading.acked;
            return false;
        };
        reading.now_ms.wrapping_sub(since) >= CONFIRM_AFTER_MS
            && (reading.rooted || reading.acked != self.acked_then)
    }
}

/// The same check over any message. `verify` is this over [`signed_message`].
/// The RFC 8032 vectors sign other messages, so the tests use this to reach them.
fn check(public_key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    let public_key = PublicKey::new(*public_key);
    let signature = Signature::new(*signature);
    public_key.verify(message, &signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_compact::{KeyPair, Seed};

    /// Fixed seeds so every test key is the same on every run.
    const SEED: [u8; 32] = [0x42; 32];
    const OTHER_SEED: [u8; 32] = [0x43; 32];

    fn unhex<const N: usize>(text: &str) -> [u8; N] {
        let mut out = [0u8; N];
        for (index, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap();
        }
        out
    }

    fn digest_of(image: &[u8]) -> [u8; DIGEST] {
        let mut hash = ImageHash::new();
        hash.update(image);
        hash.finish()
    }

    fn keypair(seed: [u8; 32]) -> KeyPair {
        KeyPair::from_seed(Seed::new(seed))
    }

    const VERSION: u32 = 7;

    fn sign(seed: [u8; 32], digest: &[u8; DIGEST]) -> [u8; 64] {
        let message = signed_message(PRODUCT_NODE, VERSION, digest);
        *keypair(seed).sk.sign(message, None)
    }

    fn good(public: &[u8; 32], digest: &[u8; DIGEST], signature: &[u8; 64]) -> bool {
        verify(public, PRODUCT_NODE, VERSION, digest, signature)
    }

    const IMAGE: &[u8] = b"tallow node image, version 1";

    #[test]
    fn the_signed_message_is_context_product_version_and_digest() {
        let digest = [0xab; DIGEST];
        let message = signed_message(PRODUCT_CAM0, 0x0102_0304, &digest);
        assert_eq!(message.len(), 50);
        assert_eq!(&message[..13], b"tallow-ota-v1");
        assert_eq!(message[13], 2);
        assert_eq!(message[14..18], [0x04, 0x03, 0x02, 0x01]);
        assert_eq!(message[18..], digest);
    }

    #[test]
    fn a_signature_does_not_fit_another_product_or_version() {
        let digest = digest_of(IMAGE);
        let public = *keypair(SEED).pk;
        let signature = sign(SEED, &digest);
        assert!(good(&public, &digest, &signature));
        assert!(!verify(&public, PRODUCT_CAM0, VERSION, &digest, &signature));
        assert!(!verify(
            &public,
            PRODUCT_NODE,
            VERSION + 1,
            &digest,
            &signature
        ));
    }

    #[test]
    fn a_signature_over_the_bare_digest_is_refused() {
        let digest = digest_of(IMAGE);
        let public = *keypair(SEED).pk;
        let signature = *keypair(SEED).sk.sign(digest, None);
        assert!(!good(&public, &digest, &signature));
    }

    #[test]
    fn a_good_signature_over_the_digest_verifies() {
        let digest = digest_of(IMAGE);
        let public = *keypair(SEED).pk;
        assert!(good(&public, &digest, &sign(SEED, &digest)));
    }

    #[test]
    fn a_bad_signature_is_refused() {
        let digest = digest_of(IMAGE);
        let public = *keypair(SEED).pk;
        let mut signature = sign(SEED, &digest);
        signature[10] ^= 0x01;
        assert!(!good(&public, &digest, &signature));
    }

    #[test]
    fn a_flipped_hash_byte_is_refused() {
        let public = *keypair(SEED).pk;
        let digest = digest_of(IMAGE);
        let signature = sign(SEED, &digest);
        let mut tampered = digest;
        tampered[0] ^= 0x80;
        assert!(!good(&public, &tampered, &signature));
    }

    #[test]
    fn a_signature_from_another_key_is_refused() {
        let digest = digest_of(IMAGE);
        let other_public = *keypair(OTHER_SEED).pk;
        assert!(!good(&other_public, &digest, &sign(SEED, &digest)));
    }

    #[test]
    fn a_changed_image_with_the_old_signature_is_refused() {
        let public = *keypair(SEED).pk;
        let signature = sign(SEED, &digest_of(IMAGE));
        let changed = digest_of(b"tallow node image, version 2");
        assert!(!good(&public, &changed, &signature));
    }

    #[test]
    fn malformed_keys_and_signatures_are_refused_without_a_panic() {
        let digest = digest_of(IMAGE);
        let signature = sign(SEED, &digest);
        let public = *keypair(SEED).pk;
        // All zeros is the identity point, which has small order.
        assert!(!good(&[0u8; 32], &digest, &signature));
        // A y coordinate above the field prime is not a canonical point.
        assert!(!good(&[0xff; 32], &digest, &signature));
        // All zeros is not a signature by any key.
        assert!(!good(&public, &digest, &[0u8; 64]));
        // S at or above the group order is not canonical, so it is refused.
        let mut high_s = signature;
        high_s[32..].copy_from_slice(&[0xff; 32]);
        assert!(!good(&public, &digest, &high_s));
    }

    #[test]
    fn sha256_of_abc_matches_the_fips_vector() {
        assert_eq!(
            digest_of(b"abc"),
            unhex::<32>("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn the_digest_is_the_same_however_the_chunks_fall() {
        let image: Vec<u8> = (0..300u32).map(|value| (value * 7) as u8).collect();
        let whole = digest_of(&image);
        for split in 0..=image.len() {
            let mut hash = ImageHash::new();
            hash.update(&image[..split]);
            hash.update(&image[split..]);
            assert_eq!(hash.finish(), whole, "split at {split}");
        }
        let mut one_byte_at_a_time = ImageHash::default();
        for byte in &image {
            one_byte_at_a_time.update(std::slice::from_ref(byte));
        }
        assert_eq!(one_byte_at_a_time.finish(), whole);
    }

    #[test]
    fn rfc8032_test_1_verifies_over_the_empty_message() {
        let public =
            unhex::<32>("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
        let signature = unhex::<64>(concat!(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155",
            "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
        ));
        assert!(check(&public, &[], &signature));
    }

    #[test]
    fn rfc8032_test_2_verifies_over_the_one_byte_message() {
        let public =
            unhex::<32>("3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c");
        let signature = unhex::<64>(concat!(
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da",
            "085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00"
        ));
        assert!(check(&public, &[0x72], &signature));
        assert!(!check(&public, &[0x73], &signature));
    }

    #[test]
    fn an_openssl_signature_from_a_key_made_by_ota_key_verifies() {
        // Made by `make_ota_key` in tools/mesh.sh, in a scratch directory, and
        // signed with OpenSSL over the digest of MESSAGE. This is a throwaway
        // test key, not the image key. It proves the signature format OpenSSL
        // writes, so it checks those 32 bytes as the message.
        const MESSAGE: &[u8] = b"tallow ota-key check";
        let public =
            unhex::<32>("2cb88b8deb50744a541ea422bdf99056f68bce996dbcd2f814275b6063587468");
        let signature = unhex::<64>(concat!(
            "82663e8858c03ec1892ffffa646458d5653c37f2d3d6ce1d139c9261db7c9744",
            "fb9acea581539e312e8229e840a80a1edc0865b195d16cc349c8b0524307ab05"
        ));
        assert!(check(&public, &digest_of(MESSAGE), &signature));
    }

    #[test]
    fn an_identity_round_trips_and_a_short_payload_does_not() {
        let identity = Identity {
            product: PRODUCT_CAM0,
            running: 7,
            floor: 4,
        };
        assert_eq!(Identity::decode(&identity.encode()), Some(identity));
        assert_eq!(Identity::decode(&identity.encode()[..8]), None);
        assert_eq!(Identity::decode(&[0; 10]), None);
    }

    #[test]
    fn the_reported_running_version_is_the_image_that_is_executing() {
        assert_eq!(running_version(false, Some(9), 4), 4);
        assert_eq!(running_version(true, Some(9), 4), 9);
        assert_eq!(running_version(true, None, 4), u32::MAX);
        assert_eq!(running_version(false, None, 0), 0);
    }
}
