//! Status replies: 20 bytes, `0xaa`, a domain byte, 17 body bytes, and the XOR checksum.
//!
//! Domains and body checks follow `tools/ble/kaitai/speculative/h7020_status_reply.ksy`,
//! which the pinned codec uses for the H7020. Its candidate schema is unverified on
//! hardware, so the vectors that cover it are partly constructed. See the vector sources.
//!
//! Colour-mode (`0x05`) bodies follow that schema: static, scene, diy, and music.
//! A mode byte it does not name stays the mode byte. The tail after the fields is not checked.

const REPLY_LEN: usize = 20;
const HEADER: u8 = 0xaa;
/// Largest group number the segment page accepts: `(30 + 3 - 1) / 3`.
const SEGMENT_GROUP_MAX: u8 = 10;
const MODE_SCENE: u8 = 0x04;
const MODE_DIY: u8 = 0x0a;
const MODE_MUSIC: u8 = 0x13;
const MODE_STATIC: u8 = 0x15;
/// Music mode 4 carries one extra byte before the colour flag.
const MUSIC_SELECTOR_MODE: u8 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub brightness: u8,
    pub rgb: [u8; 3],
}

/// One colour-mode body. `Unparsed` is a mode byte the schema does not name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColourMode {
    Static {
        sub: u8,
        kelvin: u16,
    },
    Scene {
        scene_id: u16,
    },
    Diy {
        code: u16,
    },
    Music {
        mode_id: u8,
        sensitivity: u8,
        /// Set only when `mode_id` is 4.
        selector: Option<u8>,
        has_fixed_colour: bool,
        rgb: Option<[u8; 3]>,
    },
    Unparsed {
        mode: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Power {
        is_on: bool,
    },
    Brightness {
        percent: u8,
    },
    ColourMode(ColourMode),
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
        0x05 => colour_mode(body),
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

fn u16_at(bytes: &[u8], at: usize, be: bool) -> Option<u16> {
    let pair = [*bytes.get(at)?, *bytes.get(at + 1)?];
    Some(if be {
        u16::from_be_bytes(pair)
    } else {
        u16::from_le_bytes(pair)
    })
}

/// A colour-mode body: one mode byte, then the layout that mode names.
/// Returns None when a named layout runs past the body.
fn colour_mode(body: &[u8]) -> Option<Reply> {
    let mode = *body.first()?;
    let rest = body.get(1..)?;
    let parsed = match mode {
        MODE_STATIC => ColourMode::Static {
            sub: *rest.first()?,
            kelvin: u16_at(rest, 1, true)?,
        },
        MODE_SCENE => ColourMode::Scene {
            scene_id: u16_at(rest, 0, false)?,
        },
        MODE_DIY => ColourMode::Diy {
            code: u16_at(rest, 0, false)?,
        },
        MODE_MUSIC => music(rest)?,
        other => ColourMode::Unparsed { mode: other },
    };
    Some(Reply::ColourMode(parsed))
}

/// Music: mode, sensitivity, an extra byte when the mode is 4, then a colour flag and maybe RGB.
fn music(rest: &[u8]) -> Option<ColourMode> {
    let mode_id = *rest.first()?;
    let sensitivity = *rest.get(1)?;
    let mut at = 2;
    let selector = if mode_id == MUSIC_SELECTOR_MODE {
        let value = *rest.get(at)?;
        at += 1;
        Some(value)
    } else {
        None
    };
    let flag = *rest.get(at)?;
    at += 1;
    let rgb = if flag == 0 {
        None
    } else {
        Some([*rest.get(at)?, *rest.get(at + 1)?, *rest.get(at + 2)?])
    };
    Some(ColourMode::Music {
        mode_id,
        sensitivity,
        selector,
        has_fixed_colour: flag != 0,
        rgb,
    })
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
