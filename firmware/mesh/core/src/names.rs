//! Device names and makers, sent once rather than with every sighting.

use crate::sightings::Kind;

pub const NAME_MAX: usize = 32;
const FIXED: usize = 13;
pub const NAME_RECORD_MAX: usize = FIXED + NAME_MAX;

#[derive(Copy, Clone)]
struct Pending {
    addr: [u8; 6],
    kind: u8,
    company: u16,
    class: [u8; 3],
    length: u8,
    name: [u8; NAME_MAX],
}

/// Remembers which addresses were already named. The memory clears on a timer,
/// so a restarted host learns every name again.
pub struct Names<const SEEN: usize, const PENDING: usize> {
    seen: [u32; SEEN],
    seen_next: usize,
    pending: [Option<Pending>; PENDING],
    cleared_ms: u32,
}

impl<const SEEN: usize, const PENDING: usize> Default for Names<SEEN, PENDING> {
    fn default() -> Self {
        Self::new()
    }
}

fn print(addr: &[u8; 6], kind: Kind) -> u32 {
    let mut hash = 0x811C_9DC5u32 ^ kind as u32;
    for byte in addr {
        hash = (hash ^ *byte as u32).wrapping_mul(0x0100_0193);
    }
    hash | 1
}

impl<const SEEN: usize, const PENDING: usize> Names<SEEN, PENDING> {
    pub const CLEAR_MS: u32 = 300_000;

    pub const fn new() -> Self {
        Self {
            seen: [0; SEEN],
            seen_next: 0,
            pending: [None; PENDING],
            cleared_ms: 0,
        }
    }

    /// Offers what an advert or inquiry said about a device. Nothing to say, or
    /// already said, costs one lookup. A name beats an earlier nameless offer.
    pub fn offer(
        &mut self,
        now_ms: u32,
        addr: [u8; 6],
        kind: Kind,
        company: Option<u16>,
        class: Option<[u8; 3]>,
        name: Option<&[u8]>,
    ) {
        if now_ms.wrapping_sub(self.cleared_ms) > Self::CLEAR_MS {
            self.seen = [0; SEEN];
            self.cleared_ms = now_ms;
        }
        let name = name.filter(|name| !name.is_empty());
        if name.is_none() {
            return;
        }
        let key = print(&addr, kind);
        if self.seen.contains(&key) {
            return;
        }
        let Some(slot) = self.pending.iter().position(Option::is_none) else {
            return;
        };
        self.seen[self.seen_next] = key;
        self.seen_next = (self.seen_next + 1) % SEEN;
        let mut entry = Pending {
            addr,
            kind: kind as u8,
            company: company.unwrap_or(0xFFFF),
            class: class.unwrap_or([0; 3]),
            length: 0,
            name: [0; NAME_MAX],
        };
        if let Some(name) = name {
            let length = name.len().min(NAME_MAX);
            entry.name[..length].copy_from_slice(&name[..length]);
            entry.length = length as u8;
        }
        self.pending[slot] = Some(entry);
    }

    /// Moves records into `out`. Returns the bytes written.
    pub fn drain(&mut self, out: &mut [u8]) -> usize {
        let mut written = 0;
        for slot in self.pending.iter_mut() {
            let Some(entry) = slot else { continue };
            let size = FIXED + entry.length as usize;
            if written + size > out.len() {
                break;
            }
            let record = &mut out[written..written + size];
            record[..6].copy_from_slice(&entry.addr);
            record[6] = entry.kind;
            record[7..9].copy_from_slice(&entry.company.to_le_bytes());
            record[9..12].copy_from_slice(&entry.class);
            record[12] = entry.length;
            record[13..].copy_from_slice(&entry.name[..entry.length as usize]);
            written += size;
            *slot = None;
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_a_name_once_then_again_after_the_timer() {
        let mut names = Names::<8, 4>::new();
        names.offer(
            1,
            [1; 6],
            Kind::Classic,
            None,
            Some([1, 2, 3]),
            Some(b"Flip 5"),
        );
        names.offer(
            2,
            [1; 6],
            Kind::Classic,
            None,
            Some([1, 2, 3]),
            Some(b"Flip 5"),
        );
        let mut out = [0u8; 200];
        let length = names.drain(&mut out);
        assert_eq!(length, FIXED + 6);
        assert_eq!(&out[..6], &[1; 6]);
        assert_eq!(&out[9..12], &[1, 2, 3]);
        assert_eq!(&out[13..19], b"Flip 5");
        names.offer(3, [1; 6], Kind::Classic, None, None, Some(b"Flip 5"));
        assert_eq!(names.drain(&mut out), 0);
        names.offer(
            Names::<8, 4>::CLEAR_MS + 10,
            [1; 6],
            Kind::Classic,
            None,
            None,
            Some(b"Flip 5"),
        );
        assert!(names.drain(&mut out) > 0);
    }

    #[test]
    fn nameless_offers_do_not_use_up_the_first_chance() {
        let mut names = Names::<8, 4>::new();
        names.offer(1, [1; 6], Kind::BlePublic, Some(0x4C), None, None);
        names.offer(2, [1; 6], Kind::BlePublic, Some(0x4C), None, Some(b"x"));
        let mut out = [0u8; 200];
        assert_eq!(names.drain(&mut out), FIXED + 1);
    }

    #[test]
    fn a_long_name_is_cut_and_a_full_queue_retries_later() {
        let mut names = Names::<8, 1>::new();
        names.offer(1, [1; 6], Kind::BleRandom, None, None, Some(&[b'a'; 200]));
        names.offer(1, [2; 6], Kind::BleRandom, None, None, Some(b"later"));
        let mut out = [0u8; 200];
        assert_eq!(names.drain(&mut out), FIXED + NAME_MAX);
        names.offer(2, [2; 6], Kind::BleRandom, None, None, Some(b"later"));
        assert_eq!(names.drain(&mut out), FIXED + 5);
    }

    #[test]
    fn stops_at_the_end_of_the_buffer_and_keeps_the_rest() {
        let mut names = Names::<8, 4>::new();
        names.offer(1, [1; 6], Kind::BleRandom, None, None, Some(b"aaaa"));
        names.offer(1, [2; 6], Kind::BleRandom, None, None, Some(b"bbbb"));
        let mut out = [0u8; FIXED + 4];
        assert_eq!(names.drain(&mut out), FIXED + 4);
        assert_eq!(names.drain(&mut out), FIXED + 4);
    }
}
