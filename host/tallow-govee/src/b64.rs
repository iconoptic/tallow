//! Standard base64 with padding, as `base64.b64decode(..., validate=True)` reads it.
//! Written by hand so the crate takes no base64 dependency.

fn value(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decodes text whose length is a multiple of four, with `=` only as trailing padding.
/// Returns None for anything else.
pub(crate) fn decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let quads = bytes.len() / 4;
    let mut out = Vec::with_capacity(quads * 3);
    for (index, quad) in bytes.chunks(4).enumerate() {
        let padding = quad.iter().rev().take_while(|&&byte| byte == b'=').count();
        let last = index + 1 == quads;
        if padding > 2 || (padding > 0 && !last) {
            return None;
        }
        let data = quad.get(..4 - padding)?;
        let mut bits = 0u32;
        for &byte in data {
            bits = (bits << 6) | value(byte)?;
        }
        bits <<= 6 * padding as u32;
        let triple = bits.to_be_bytes();
        out.extend_from_slice(triple.get(1..4 - padding)?);
    }
    Some(out)
}
