//! What a node has heard since its last report.

pub const RECORD: usize = 10;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Kind {
    BlePublic = 0,
    BleRandom = 1,
    Classic = 2,
}

#[derive(Copy, Clone)]
struct Entry {
    addr: [u8; 6],
    kind: u8,
    best: i8,
    count: u8,
    last_ms: u32,
}

const EMPTY: Entry = Entry {
    addr: [0; 6],
    kind: 0,
    best: -128,
    count: 0,
    last_ms: 0,
};

/// A fixed table. When it fills, the entry heard longest ago makes way, so a
/// neighbour's busy street cannot grow this past its size.
pub struct Sightings<const N: usize> {
    entries: [Entry; N],
    next: usize,
    pub evicted: u32,
}

impl<const N: usize> Default for Sightings<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Sightings<N> {
    pub const fn new() -> Self {
        Self {
            entries: [EMPTY; N],
            next: 0,
            evicted: 0,
        }
    }

    pub fn observe(&mut self, now_ms: u32, addr: [u8; 6], kind: Kind, rssi: i8) {
        let mut oldest = 0;
        let mut oldest_age = 0;
        for (index, entry) in self.entries.iter_mut().enumerate() {
            if entry.count > 0 && entry.addr == addr && entry.kind == kind as u8 {
                entry.count = entry.count.saturating_add(1);
                entry.best = entry.best.max(rssi);
                entry.last_ms = now_ms;
                return;
            }
            let age = if entry.count == 0 {
                u32::MAX
            } else {
                now_ms.wrapping_sub(entry.last_ms)
            };
            if age > oldest_age {
                oldest_age = age;
                oldest = index;
            }
        }
        if oldest_age != u32::MAX {
            self.evicted = self.evicted.wrapping_add(1);
        }
        self.entries[oldest] = Entry {
            addr,
            kind: kind as u8,
            best: rssi,
            count: 1,
            last_ms: now_ms,
        };
    }

    pub fn is_empty(&self) -> bool {
        self.entries.iter().all(|entry| entry.count == 0)
    }

    pub fn len(&self) -> usize {
        self.entries.iter().filter(|entry| entry.count > 0).count()
    }

    /// Moves as many sightings as fit into `out`. Returns the bytes written.
    pub fn drain(&mut self, now_ms: u32, out: &mut [u8]) -> usize {
        let mut written = 0;
        for _ in 0..N {
            if written + RECORD > out.len() {
                break;
            }
            let index = self.next;
            self.next = (self.next + 1) % N;
            let entry = &mut self.entries[index];
            if entry.count == 0 {
                continue;
            }
            let age = now_ms.wrapping_sub(entry.last_ms) / 1000;
            let record = &mut out[written..written + RECORD];
            record[..6].copy_from_slice(&entry.addr);
            record[6] = entry.kind;
            record[7] = entry.best as u8;
            record[8] = entry.count;
            record[9] = age.min(255) as u8;
            written += RECORD;
            entry.count = 0;
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_repeats_and_keeps_the_strongest_signal() {
        let mut table = Sightings::<4>::new();
        table.observe(0, [1; 6], Kind::BlePublic, -80);
        table.observe(10, [1; 6], Kind::BlePublic, -60);
        table.observe(20, [1; 6], Kind::BlePublic, -90);
        assert_eq!(table.len(), 1);
        let mut out = [0u8; 40];
        assert_eq!(table.drain(2020, &mut out), RECORD);
        assert_eq!(&out[..6], &[1; 6]);
        assert_eq!(out[7] as i8, -60);
        assert_eq!(out[8], 3);
        assert_eq!(out[9], 2);
        assert!(table.is_empty());
    }

    #[test]
    fn same_address_on_two_transports_stays_separate() {
        let mut table = Sightings::<4>::new();
        table.observe(0, [1; 6], Kind::BlePublic, -80);
        table.observe(0, [1; 6], Kind::Classic, -80);
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn evicts_the_oldest_when_full() {
        let mut table = Sightings::<2>::new();
        table.observe(1, [1; 6], Kind::BlePublic, -80);
        table.observe(2, [2; 6], Kind::BlePublic, -80);
        table.observe(3, [3; 6], Kind::BlePublic, -80);
        assert_eq!(table.len(), 2);
        assert_eq!(table.evicted, 1);
        let mut out = [0u8; 40];
        table.drain(3, &mut out);
        assert!(out.chunks(RECORD).all(|record| record[0] != 1));
    }

    #[test]
    fn drains_across_frames_without_losing_any() {
        let mut table = Sightings::<8>::new();
        for byte in 0..8u8 {
            table.observe(0, [byte; 6], Kind::BleRandom, -70);
        }
        let mut seen = 0;
        let mut out = [0u8; RECORD * 3];
        loop {
            let written = table.drain(0, &mut out);
            if written == 0 {
                break;
            }
            seen += written / RECORD;
        }
        assert_eq!(seen, 8);
    }

    #[test]
    fn count_saturates() {
        let mut table = Sightings::<1>::new();
        for _ in 0..1000 {
            table.observe(0, [1; 6], Kind::BlePublic, -80);
        }
        let mut out = [0u8; RECORD];
        table.drain(0, &mut out);
        assert_eq!(out[8], 255);
    }
}
