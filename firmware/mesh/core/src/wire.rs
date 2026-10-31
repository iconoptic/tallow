//! The frame format.
//!
//! ```text
//! 0      version
//! 1      kind
//! 2      ttl    changed by forwarders, not authenticated
//! 3      hops   changed by forwarders, not authenticated
//! 4..8   origin node id
//! 8..12  origin boot id, random each boot
//! 12..16 origin sequence number
//! 16..   ciphertext
//! end-16 Poly1305 tag
//! ```
//!
//! The nonce is origin + boot + sequence, so it never repeats while boot ids
//! differ. Every member holds the one key. A forwarder re-sends the ciphertext
//! untouched, so it never needs to decrypt to forward, but it does anyway to
//! reject frames from outside the mesh.

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, Tag};

pub const VERSION: u8 = 0xA1;
pub const HEADER: usize = 16;
pub const TAG: usize = 16;
/// ESP-NOW v1 limit. Staying here keeps airtime short on a shared radio.
pub const MAX_FRAME: usize = 250;
pub const MAX_PAYLOAD: usize = MAX_FRAME - HEADER - TAG;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FrameKind {
    Beacon = 1,
    Report = 2,
    Name = 3,
    Stats = 4,
    Probe = 5,
    Command = 6,
}

impl FrameKind {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::Beacon,
            2 => Self::Report,
            3 => Self::Name,
            4 => Self::Stats,
            5 => Self::Probe,
            6 => Self::Command,
            _ => return None,
        })
    }
}

/// Header fields of a frame that passed authentication.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub kind: FrameKind,
    pub ttl: u8,
    pub hops: u8,
    pub origin: u32,
    pub boot: u32,
    pub seq: u32,
}

fn nonce(origin: u32, boot: u32, seq: u32) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[..4].copy_from_slice(&origin.to_le_bytes());
    out[4..8].copy_from_slice(&boot.to_le_bytes());
    out[8..12].copy_from_slice(&seq.to_le_bytes());
    out
}

fn aad(header: &[u8]) -> [u8; 14] {
    let mut out = [0u8; 14];
    out[..2].copy_from_slice(&header[..2]);
    out[2..14].copy_from_slice(&header[4..16]);
    out
}

pub struct Sealer {
    cipher: ChaCha20Poly1305,
}

impl Sealer {
    pub fn new(key: &[u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
        }
    }

    /// Writes a sealed frame into `out` and returns its length, or `None` if
    /// the payload does not fit.
    pub fn seal(&self, head: &Frame, payload: &[u8], out: &mut [u8; MAX_FRAME]) -> Option<usize> {
        if payload.len() > MAX_PAYLOAD {
            return None;
        }
        out[0] = VERSION;
        out[1] = head.kind as u8;
        out[2] = head.ttl;
        out[3] = head.hops;
        out[4..8].copy_from_slice(&head.origin.to_le_bytes());
        out[8..12].copy_from_slice(&head.boot.to_le_bytes());
        out[12..16].copy_from_slice(&head.seq.to_le_bytes());
        let extra = aad(&out[..HEADER]);
        let end = HEADER + payload.len();
        out[HEADER..end].copy_from_slice(payload);
        let tag = self
            .cipher
            .encrypt_in_place_detached(
                Nonce::from_slice(&nonce(head.origin, head.boot, head.seq)),
                &extra,
                &mut out[HEADER..end],
            )
            .ok()?;
        out[end..end + TAG].copy_from_slice(&tag);
        Some(end + TAG)
    }

    /// Authenticates `data` and decrypts its payload into `plain`.
    pub fn open(&self, data: &[u8], plain: &mut [u8; MAX_PAYLOAD]) -> Option<(Frame, usize)> {
        if data.len() < HEADER + TAG || data.len() > MAX_FRAME || data[0] != VERSION {
            return None;
        }
        let kind = FrameKind::from_u8(data[1])?;
        let word =
            |at: usize| u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
        let head = Frame {
            kind,
            ttl: data[2],
            hops: data[3],
            origin: word(4),
            boot: word(8),
            seq: word(12),
        };
        let end = data.len() - TAG;
        let length = end - HEADER;
        plain[..length].copy_from_slice(&data[HEADER..end]);
        self.cipher
            .decrypt_in_place_detached(
                Nonce::from_slice(&nonce(head.origin, head.boot, head.seq)),
                &aad(&data[..HEADER]),
                &mut plain[..length],
                Tag::from_slice(&data[end..]),
            )
            .ok()?;
        Some((head, length))
    }
}

/// Scan settings sent down from the host, 11 bytes on the wire.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Command {
    pub id: u16,
    /// BLE scan window and interval in milliseconds. Window 0 stops scanning.
    pub ble_window_ms: u16,
    pub ble_interval_ms: u16,
    /// Seconds between Classic inquiries. 0 stops them.
    pub inquiry_period_s: u16,
    /// Inquiry length in units of 1.28 s.
    pub inquiry_length: u8,
    /// Delivery-test frames per second. 0 stops them.
    pub probe_hz: u8,
    pub probe_size: u8,
}

impl Command {
    pub const WIRE: usize = 11;

    pub fn encode(&self) -> [u8; Self::WIRE] {
        let mut out = [0u8; Self::WIRE];
        out[0..2].copy_from_slice(&self.id.to_le_bytes());
        out[2..4].copy_from_slice(&self.ble_window_ms.to_le_bytes());
        out[4..6].copy_from_slice(&self.ble_interval_ms.to_le_bytes());
        out[6..8].copy_from_slice(&self.inquiry_period_s.to_le_bytes());
        out[8] = self.inquiry_length;
        out[9] = self.probe_hz;
        out[10] = self.probe_size;
        out
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() != Self::WIRE {
            return None;
        }
        let half = |at: usize| u16::from_le_bytes([data[at], data[at + 1]]);
        Some(Self {
            id: half(0),
            ble_window_ms: half(2),
            ble_interval_ms: half(4),
            inquiry_period_s: half(6),
            inquiry_length: data[8],
            probe_hz: data[9],
            probe_size: data[10],
        })
    }
}
