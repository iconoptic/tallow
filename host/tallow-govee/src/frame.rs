//! The 20-byte frames the H7020 takes: queries, power, brightness, colour, paint,
//! scene selection, and the A3 upload that carries a scene body.
//!
//! Layout follows `tools/ble/kaitai/speculative/h7020_command_write.ksy` and
//! `command_write.ksy` in the pinned codec. The last byte is the XOR of the 19 before it.

use std::collections::BTreeSet;

use crate::{refuse, Refusal};

pub const FRAME_LEN: usize = 20;
pub type Frame = [u8; FRAME_LEN];

/// Bulbs on one string. The model takes one or two strings.
pub const BULBS_PER_STRING: u32 = 15;

const COMMAND: u8 = 0x33;
const STATUS_QUERY: u8 = 0xaa;
const UPLOAD: u8 = 0xa3;
const MULTI: u8 = 0x05;
const MULTI_SCENE: u8 = 0x04;
const MULTI_STATIC: u8 = 0x15;
const STATIC_COLOUR: u8 = 0x01;
const BODY_LEN: usize = 17;
const CHUNK_LEN: usize = 17;

fn xor(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |acc, byte| acc ^ byte)
}

/// Header, opcode, body zero-padded to 17 bytes, then the XOR of the 19 bytes before.
fn frame(header: u8, opcode: u8, body: &[u8]) -> Frame {
    let mut out = [0u8; FRAME_LEN];
    out[0] = header;
    out[1] = opcode;
    let used = body.len().min(BODY_LEN);
    out[2..2 + used].copy_from_slice(&body[..used]);
    out[FRAME_LEN - 1] = xor(&out[..FRAME_LEN - 1]);
    out
}

pub fn power_query() -> Frame {
    frame(STATUS_QUERY, 0x01, &[])
}

pub fn brightness_query() -> Frame {
    frame(STATUS_QUERY, 0x04, &[])
}

pub fn firmware_query() -> Frame {
    frame(STATUS_QUERY, 0x06, &[])
}

pub fn hardware_query() -> Frame {
    frame(STATUS_QUERY, 0x07, &[0x03])
}

pub fn configured_strings_query() -> Frame {
    frame(STATUS_QUERY, 0x0f, &[])
}

pub fn power(on: bool) -> Frame {
    frame(COMMAND, 0x01, &[u8::from(on)])
}

/// Clamps to 0..=100, as the Python codec does.
pub fn brightness(percent: i64) -> Frame {
    frame(COMMAND, 0x04, &[percent.clamp(0, 100) as u8])
}

/// The bulb mask for one or two strings. Any other count is refused.
pub fn whole_device_mask(strings: i64) -> Result<u32, Refusal> {
    let bulbs = bulbs_for(strings)?;
    Ok((1u32 << bulbs) - 1)
}

fn bulbs_for(strings: i64) -> Result<u32, Refusal> {
    match strings {
        1 | 2 => Ok(BULBS_PER_STRING * strings as u32),
        _ => refuse("strings must be 1 or 2"),
    }
}

fn clamp_channel(value: i64) -> u8 {
    value.clamp(0, 255) as u8
}

/// A static colour write: direct RGB, zero kelvin, zero preview, then the bulb mask.
fn static_colour(mask: u32, rgb: [i64; 3]) -> Frame {
    let [red, green, blue] = rgb.map(clamp_channel);
    let [m0, m1, m2, m3] = mask.to_le_bytes();
    frame(
        COMMAND,
        MULTI,
        &[
            MULTI_STATIC,
            STATIC_COLOUR,
            red,
            green,
            blue,
            0,
            0,
            0,
            0,
            0,
            m0,
            m1,
            m2,
            m3,
            0,
            0,
            0,
        ],
    )
}

/// Colour for every bulb on the given number of strings.
pub fn colour(strings: i64, rgb: [i64; 3]) -> Result<Frame, Refusal> {
    Ok(static_colour(whole_device_mask(strings)?, rgb))
}

/// One colour group. Bulb numbers are one-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaintGroup {
    pub bulbs: Vec<i64>,
    pub rgb: [i64; 3],
}

/// One frame per group, since each group has its own colour.
pub fn paint(strings: i64, groups: &[PaintGroup]) -> Result<Vec<Frame>, Refusal> {
    let bulbs = bulbs_for(strings)?;
    if groups.is_empty() {
        return refuse("paint needs at least one group");
    }
    let mut seen = BTreeSet::new();
    for group in groups {
        for &bulb in &group.bulbs {
            if !seen.insert(bulb) {
                return refuse(format!("bulb {bulb} is in two groups"));
            }
        }
    }
    groups
        .iter()
        .map(|group| -> Result<Frame, Refusal> {
            Ok(static_colour(segment_mask(&group.bulbs, bulbs)?, group.rgb))
        })
        .collect()
}

fn segment_mask(segments: &[i64], count: u32) -> Result<u32, Refusal> {
    let mut mask = 0u32;
    for &segment in segments {
        if !(1..=i64::from(count)).contains(&segment) {
            return refuse(format!("segment {segment} out of range 1..{count}"));
        }
        mask |= 1u32 << (segment - 1);
    }
    if mask == 0 {
        return refuse("no segments selected");
    }
    Ok(mask)
}

/// Selects a catalogue scene by its code. Scene type is always zero here.
pub fn scene_activation(code: i64) -> Frame {
    let [low, high] = (code.clamp(0, 0xffff) as u16).to_le_bytes();
    frame(COMMAND, MULTI, &[MULTI_SCENE, low, high, 0])
}

/// Splits one A3 body into upload frames. Adds a terminator frame when the
/// payload fits in a single chunk, as the app does.
pub fn upload(type_byte: u8, body: &[u8]) -> Result<Vec<Frame>, Refusal> {
    let data_len = 1 + body.len();
    let chunks = (data_len + 2).div_ceil(CHUNK_LEN);
    let terminator = chunks == 1;
    let count = chunks + usize::from(terminator);
    let Ok(count) = u8::try_from(count) else {
        return refuse(format!("A3 upload needs {count} frames, more than 255"));
    };
    let mut payload = Vec::with_capacity(2 + data_len);
    payload.extend_from_slice(&[0x01, count, type_byte]);
    payload.extend_from_slice(body);
    let parts: Vec<&[u8]> = payload.chunks(CHUNK_LEN).collect();
    let last = parts.len() - 1;
    let mut frames: Vec<Frame> = parts
        .iter()
        .enumerate()
        .map(|(index, part)| {
            let opcode = if terminator || index != last {
                index as u8
            } else {
                0xff
            };
            frame(UPLOAD, opcode, part)
        })
        .collect();
    if terminator {
        frames.push(frame(UPLOAD, 0xff, &[]));
    }
    Ok(frames)
}
