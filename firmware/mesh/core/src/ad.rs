//! BLE advertising data. Bytes off the air, so every read is bounds-checked.

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Advert<'a> {
    pub name: Option<&'a [u8]>,
    /// Bluetooth SIG company id from manufacturer data.
    pub company: Option<u16>,
}

pub fn parse(data: &[u8]) -> Advert<'_> {
    let mut out = Advert::default();
    let mut rest = data;
    while let Some((&length, tail)) = rest.split_first() {
        let length = length as usize;
        if length == 0 || length > tail.len() {
            break;
        }
        let (field, next) = tail.split_at(length);
        rest = next;
        let (kind, value) = (field[0], &field[1..]);
        match kind {
            0x09 => out.name = Some(value),
            0x08 if out.name.is_none() => out.name = Some(value),
            0xFF if value.len() >= 2 && out.company.is_none() => {
                out.company = Some(u16::from_le_bytes([value[0], value[1]]));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_name_and_company() {
        let data = [
            2, 1, 6, 5, 0x09, b'J', b'B', b'L', b'!', 3, 0xFF, 0x4C, 0x00,
        ];
        let advert = parse(&data);
        assert_eq!(advert.name, Some(&b"JBL!"[..]));
        assert_eq!(advert.company, Some(0x004C));
    }

    #[test]
    fn complete_name_beats_short_name() {
        let data = [3, 0x08, b'a', b'b', 4, 0x09, b'x', b'y', b'z'];
        assert_eq!(parse(&data).name, Some(&b"xyz"[..]));
        let data = [4, 0x09, b'x', b'y', b'z', 3, 0x08, b'a', b'b'];
        assert_eq!(parse(&data).name, Some(&b"xyz"[..]));
    }

    #[test]
    fn stops_at_a_length_past_the_end() {
        assert_eq!(parse(&[9, 0x09, b'a']), Advert::default());
        assert_eq!(parse(&[0, 0x09, b'a']), Advert::default());
        assert_eq!(parse(&[]), Advert::default());
        assert_eq!(parse(&[2, 0xFF, 0x4C]).company, None);
    }

    #[test]
    fn survives_every_short_garbage_input() {
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                let _ = parse(&[a, b, a, b, 3, a]);
            }
        }
    }
}
