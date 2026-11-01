//! The layered-scene body: the parameter bytes of a type-2 H7020 scene.
//!
//! Decoding keeps every field, including bits the codec does not name, and
//! encoding writes the canonical bytes back. A speed option therefore changes
//! only the speed bytes. Mirrors `layered_scene_decoder.py` and the
//! `effect_layer` and `scene_body` schemas under `tools/ble/kaitai/`.
//!
//! Stripped parameter layout: a record count, then per record a length byte and
//! that many layer bytes, then zero padding to the end.

use crate::{refuse, Refusal};

/// The A3 header the Python counts in its line-count check: marker, line count, scene type.
const A3_HEADER_LEN: usize = 3;
/// A line count is one byte and each line carries 17 bytes.
const CONTENT_MAX: usize = 255 * 17;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brightness {
    pub scope_high: u8,
    pub scope_low: u8,
    pub order: u8,
    pub change_speed: u8,
    pub brightest_retention: u8,
    pub darkest_retention: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Movement {
    pub enabled: bool,
    pub enter_exit: bool,
    pub direction: u8,
    pub distance: u8,
    pub speed: u8,
    pub unknown_flags: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    /// Low nibble of the applied-area byte.
    pub area_start: u8,
    /// High nibble of the applied-area byte.
    pub area_width: u8,
    pub select_type: u8,
    pub select_param_1: u8,
    pub select_param_2: u8,
    pub brightness_gradient: bool,
    pub brightness: Vec<Brightness>,
    pub distribution_backwards: bool,
    pub distribution_method: u8,
    pub distribution_extensions: u8,
    pub colour_speed: u8,
    pub colour_retention: u8,
    pub palette: Vec<[u8; 3]>,
    pub selected_movement: Movement,
    pub overall_movement: Movement,
    pub priority: u8,
    /// The layer-flags bits outside the gradient bit.
    pub unknown_flags: u8,
    /// Bytes after the priority byte, inside the record.
    pub excess: Vec<u8>,
}

fn truncated() -> Refusal {
    Refusal("scene body is truncated".to_owned())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn byte(&mut self) -> Option<u8> {
        let value = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(value)
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(count)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    fn rest(&mut self) -> &'a [u8] {
        let slice = self.bytes.get(self.at..).unwrap_or_default();
        self.at = self.bytes.len();
        slice
    }
}

/// Parses a stripped type-2 parameter. Returns the layers and the number of padding
/// bytes after the last record, which the encoder writes back.
pub fn decode(param: &[u8]) -> Result<(Vec<Layer>, usize), Refusal> {
    let mut cursor = Cursor::new(param);
    let count = cursor.byte().ok_or_else(truncated)?;
    let mut layers = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let length = cursor.byte().ok_or_else(truncated)?;
        let body = cursor.take(usize::from(length)).ok_or_else(truncated)?;
        layers.push(read_layer(body).ok_or_else(truncated)?);
    }
    let padding = cursor.rest();
    if padding.iter().any(|&byte| byte != 0) {
        return refuse("scene padding must be zero");
    }
    Ok((layers, padding.len()))
}

fn read_layer(body: &[u8]) -> Option<Layer> {
    let mut cursor = Cursor::new(body);
    let applied = cursor.byte()?;
    let select_type = cursor.byte()?;
    let select_param_1 = cursor.byte()?;
    let select_param_2 = cursor.byte()?;
    let flags = cursor.byte()?;
    let block_count = cursor.byte()?;
    let mut brightness = Vec::with_capacity(usize::from(block_count));
    for _ in 0..block_count {
        let [scope_high, scope_low, order, change_speed, brightest, darkest] =
            cursor.array::<6>()?;
        brightness.push(Brightness {
            scope_high,
            scope_low,
            order,
            change_speed,
            brightest_retention: brightest,
            darkest_retention: darkest,
        });
    }
    let direction = cursor.byte()?;
    let colour_speed = cursor.byte()?;
    let colour_retention = cursor.byte()?;
    let palette_count = cursor.byte()?;
    let mut palette = Vec::with_capacity(usize::from(palette_count));
    for _ in 0..palette_count {
        palette.push(cursor.array::<3>()?);
    }
    let selected_movement = read_movement(cursor.array()?);
    let overall_movement = read_movement(cursor.array()?);
    let priority = cursor.byte()?;
    let excess = cursor.rest().to_vec();
    Some(Layer {
        area_start: applied & 0x0f,
        area_width: applied >> 4,
        select_type,
        select_param_1,
        select_param_2,
        brightness_gradient: flags & 0x02 != 0,
        brightness,
        distribution_backwards: direction & 0x80 != 0,
        distribution_method: direction & 0x0f,
        distribution_extensions: direction & 0x70,
        colour_speed,
        colour_retention,
        palette,
        selected_movement,
        overall_movement,
        priority,
        unknown_flags: flags & 0xfd,
        excess,
    })
}

fn read_movement([packed, distance, speed]: [u8; 3]) -> Movement {
    Movement {
        enabled: packed & 0x10 != 0,
        enter_exit: packed & 0x04 != 0,
        direction: packed & 0x03,
        distance,
        speed,
        unknown_flags: packed & 0xe8,
    }
}

fn movement_bytes(movement: &Movement) -> [u8; 3] {
    let packed = (if movement.enabled { 0x10 } else { 0 })
        | (if movement.enter_exit { 0x04 } else { 0 })
        | (movement.direction & 0x03)
        | (movement.unknown_flags & 0xe8);
    [packed, movement.distance, movement.speed]
}

fn count_byte(count: usize, what: &str) -> Result<u8, Refusal> {
    u8::try_from(count).or_else(|_| refuse(format!("{what} holds at most 255, not {count}")))
}

fn encode_layer(layer: &Layer) -> Result<Vec<u8>, Refusal> {
    let mut out = vec![
        ((layer.area_width & 0x0f) << 4) | (layer.area_start & 0x0f),
        layer.select_type,
        layer.select_param_1,
        layer.select_param_2,
        (if layer.brightness_gradient { 0x02 } else { 0 }) | (layer.unknown_flags & 0xfd),
        count_byte(layer.brightness.len(), "a layer's brightness blocks")?,
    ];
    for block in &layer.brightness {
        out.extend_from_slice(&[
            block.scope_high,
            block.scope_low,
            block.order,
            block.change_speed,
            block.brightest_retention,
            block.darkest_retention,
        ]);
    }
    let backwards = u8::from(layer.distribution_backwards) << 7;
    out.push(
        backwards | (layer.distribution_method & 0x0f) | (layer.distribution_extensions & 0x70),
    );
    out.push(layer.colour_speed);
    out.push(layer.colour_retention);
    out.push(count_byte(layer.palette.len(), "a layer's palette")?);
    for colour in &layer.palette {
        out.extend_from_slice(colour);
    }
    out.extend_from_slice(&movement_bytes(&layer.selected_movement));
    out.extend_from_slice(&movement_bytes(&layer.overall_movement));
    out.push(layer.priority);
    out.extend_from_slice(&layer.excess);
    Ok(out)
}

/// Writes the stripped parameter for the layers, followed by `padding` zero bytes.
pub fn encode(layers: &[Layer], padding: usize) -> Result<Vec<u8>, Refusal> {
    let mut out = vec![count_byte(layers.len(), "a scene")?];
    for layer in layers {
        let body = encode_layer(layer)?;
        out.push(count_byte(body.len(), "a layer record")?);
        out.extend_from_slice(&body);
    }
    out.resize(out.len() + padding, 0);
    if A3_HEADER_LEN + out.len() > CONTENT_MAX {
        return refuse(format!(
            "scene content is {} bytes but the A3 line count only encodes {CONTENT_MAX}",
            A3_HEADER_LEN + out.len()
        ));
    }
    Ok(out)
}
