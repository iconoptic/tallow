//! Status replies: 20 bytes, `0xaa`, a domain byte, 17 body bytes, and the XOR checksum.
//!
//! Domains and body checks follow `tools/ble/kaitai/speculative/h7020_status_reply.ksy`,
//! which the pinned codec uses for the H7020. Its candidate schema is unverified on
//! hardware, so the vectors that cover it are partly constructed. See the vector sources.
//!
//! Colour-mode (`0x05`) bodies are only identified by domain and mode byte. Their
//! nested layouts are not decoded here and no vector covers them.

const REPLY_LEN: usize = 20;
const HEADER: u8 = 0xaa;
/// Largest group number the segment page accepts: `(30 + 3 - 1) / 3`.
const SEGMENT_GROUP_MAX: u8 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub brightness: u8,
    pub rgb: [u8; 3],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Power {
        is_on: bool,
    },
    Brightness {
        percent: u8,
    },
    ColourMode {
        mode: u8,
    },
    FirmwareVersion {
        text: String,
    },
    HardwareVersion {
        text: String,
    },
    ConfiguredStrings {
        count: u8,
    },
    PowerOffMemory {
        flag: bool,
    },
    Segments {
        group: u8,
        segments: [Segment; 3],
    },
    /// A domain byte the schema does not name. Its body is kept only as the domain.
    Unknown {
        domain: u8,
    },
}

/// Parses one status reply. Returns None on a bad length, checksum, header, or body.
pub fn parse(frame: &[u8]) -> Option<Reply> {
    let [header, domain, body @ .., checksum] = frame else {
        return None;
    };
    if frame.len() != REPLY_LEN || *checksum != xor(&frame[..REPLY_LEN - 1]) || *header != HEADER {
        return None;
    }
    let [first, rest @ ..] = body else {
        return None;
    };
    match *domain {
        0x01 => flag(*first).map(|is_on| Reply::Power { is_on }),
        0x04 => (*first <= 100).then_some(Reply::Brightness { percent: *first }),
        0x05 => Some(Reply::ColourMode { mode: *first }),
        0x06 => text(body).map(|text| Reply::FirmwareVersion { text }),
        0x07 => match body {
            [0x03, text_bytes @ ..] => text(text_bytes).map(|text| Reply::HardwareVersion { text }),
            _ => None,
        },
        0x0f => matches!(*first, 1 | 2).then_some(Reply::ConfiguredStrings { count: *first }),
        0x41 => flag(*first).map(|flag| Reply::PowerOffMemory { flag }),
        0xa5 => segments(*first, rest),
        other => Some(Reply::Unknown { domain: other }),
    }
}

fn xor(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |acc, byte| acc ^ byte)
}

fn flag(byte: u8) -> Option<bool> {
    match byte {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// A NUL-terminated ASCII string, with the rest of the body unchecked.
fn text(body: &[u8]) -> Option<String> {
    let end = body.iter().position(|&byte| byte == 0)?;
    let bytes = body.get(..end)?;
    if !bytes.is_ascii() {
        return None;
    }
    String::from_utf8(bytes.to_vec()).ok()
}

/// A segment page: one group byte, three 4-byte records, four unused bytes.
fn segments(group: u8, rest: &[u8]) -> Option<Reply> {
    if !(1..=SEGMENT_GROUP_MAX).contains(&group) || rest.len() != 16 {
        return None;
    }
    let record = |at: usize| -> Option<Segment> {
        let [brightness, red, green, blue] = <[u8; 4]>::try_from(rest.get(at..at + 4)?).ok()?;
        Some(Segment {
            brightness,
            rgb: [red, green, blue],
        })
    };
    let segments = [record(0)?, record(4)?, record(8)?];
    Some(Reply::Segments { group, segments })
}
