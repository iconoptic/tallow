//! What a node has heard since its last report.

pub const RECORD: usize = 10;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Kind {
    BlePublic = 0,
    BleRandom = 1,
    Classic = 2,
}

impl Kind {
    /// The kind a record's code names, or `None` for a code this build does not know.
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::BlePublic),
            1 => Some(Self::BleRandom),
            2 => Some(Self::Classic),
            _ => None,
        }
    }
}

/// Samples kept per device. A longer ring would push `Sightings<128>` past the
/// 12 KB node budget in `tests/sim.rs`.
const RING: usize = 8;
/// A page that found a device and has no signal strength. Not a distance, so it
/// never enters the ring. A ring with no distance still reports this.
const NO_SIGNAL: i8 = -127;

#[derive(Copy, Clone)]
struct Entry {
    addr: [u8; 6],
    kind: u8,
    /// How many of `samples` are distances. Zero when every hear was [`NO_SIGNAL`].
    filled: u8,
    /// Next write in `samples`. Once the ring is full, the oldest sample is here.
    cursor: u8,
    count: u8,
    samples: [i8; RING],
    last_ms: u32,
}

const EMPTY: Entry = Entry {
    addr: [0; 6],
    kind: 0,
    filled: 0,
    cursor: 0,
    count: 0,
    samples: [0; RING],
    last_ms: 0,
};

impl Entry {
    fn hear(&mut self, now_ms: u32, rssi: i8) {
        self.count = self.count.saturating_add(1);
        self.last_ms = now_ms;
        if rssi == NO_SIGNAL {
            return;
        }
        self.samples[self.cursor as usize] = rssi;
        self.cursor = (self.cursor + 1) % RING as u8;
        if self.filled < RING as u8 {
            self.filled += 1;
        }
    }

    /// The middle of the distances heard since the last drain. An even count
    /// averages the two central samples, with the division truncating toward zero.
    fn rssi(&self) -> i8 {
        let n = self.filled as usize;
        if n == 0 {
            return NO_SIGNAL;
        }
        // Until the ring wraps, the distances sit in the first `filled` slots.
        let mut sorted = [0i8; RING];
        if n < RING {
            sorted[..n].copy_from_slice(&self.samples[..n]);
        } else {
            sorted.copy_from_slice(&self.samples);
        }
        sorted[..n].sort_unstable();
        if n % 2 == 1 {
            sorted[n / 2]
        } else {
            let low = i16::from(sorted[n / 2 - 1]);
            let high = i16::from(sorted[n / 2]);
            ((low + high) / 2) as i8
        }
    }
}

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
                entry.hear(now_ms, rssi);
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
        self.entries[oldest] = EMPTY;
        let entry = &mut self.entries[oldest];
        entry.addr = addr;
        entry.kind = kind as u8;
        entry.hear(now_ms, rssi);
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
            // The Bluetooth task can stamp a sighting after the main loop read its
            // clock. That is no age at all, not most of a wrapped clock.
            let age = (now_ms.wrapping_sub(entry.last_ms) as i32).max(0) as u32 / 1000;
            let record = &mut out[written..written + RECORD];
            record[..6].copy_from_slice(&entry.addr);
            record[6] = entry.kind;
            record[7] = entry.rssi() as u8;
            record[8] = entry.count;
            record[9] = age.min(255) as u8;
            written += RECORD;
            *entry = EMPTY;
        }
        written
    }
}

/// One sighting, read back from a Report payload.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Record {
    pub addr: [u8; 6],
    /// The raw code, so a code this build does not know is kept, not dropped.
    pub kind: u8,
    /// Median RSSI of the distances in the report window, or -127 when the
    /// window had none.
    pub rssi: i8,
    pub count: u8,
    pub age_s: u8,
}

/// The records in a Report payload. A partial record at the end is not returned,
/// as `parse_report` in `tools/mesh_host.py` ignores it.
pub struct Records<'a> {
    chunks: core::slice::ChunksExact<'a, u8>,
}

impl<'a> Records<'a> {
    pub fn new(payload: &'a [u8]) -> Self {
        Self {
            chunks: payload.chunks_exact(RECORD),
        }
    }
}

impl Iterator for Records<'_> {
    type Item = Record;

    fn next(&mut self) -> Option<Record> {
        let record = self.chunks.next()?;
        Some(Record {
            addr: record.get(..6)?.try_into().ok()?,
            kind: *record.get(6)?,
            rssi: *record.get(7)? as i8,
            count: *record.get(8)?,
            age_s: *record.get(9)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_round_trips_through_the_table_and_the_records() {
        let mut table = Sightings::<4>::new();
        table.observe(0, [7; 6], Kind::Classic, -71);
        table.observe(0, [7; 6], Kind::Classic, -64);
        table.observe(0, [9; 6], Kind::BleRandom, -90);
        let mut out = [0u8; 40];
        let length = table.drain(3000, &mut out);
        let records: Vec<Record> = Records::new(&out[..length]).collect();
        assert_eq!(records.len(), 2);
        let first = records.iter().find(|record| record.addr == [7; 6]).unwrap();
        assert_eq!(first.kind, Kind::Classic as u8);
        assert_eq!((first.rssi, first.count, first.age_s), (-67, 2, 3));
        let second = records.iter().find(|record| record.addr == [9; 6]).unwrap();
        assert_eq!((second.kind, second.rssi, second.count), (1, -90, 1));
    }

    #[test]
    fn a_partial_report_record_is_ignored() {
        let mut out = [0u8; RECORD + 2];
        out[..RECORD].copy_from_slice(&[0xAA; RECORD]);
        assert_eq!(Records::new(&out).count(), 1);
        assert_eq!(Records::new(&[0u8; 9]).count(), 0);
    }

    #[test]
    fn an_unknown_kind_code_is_kept_and_named_by_from_u8_as_none() {
        let record = Records::new(&[1, 2, 3, 4, 5, 6, 9, 0xB0, 1, 0])
            .next()
            .unwrap();
        assert_eq!(record.kind, 9);
        assert_eq!(record.rssi, -80);
        assert_eq!(Kind::from_u8(record.kind), None);
        assert_eq!(Kind::from_u8(2), Some(Kind::Classic));
    }

    #[test]
    fn merges_repeats_and_reports_the_median_signal() {
        let mut table = Sightings::<4>::new();
        table.observe(0, [1; 6], Kind::BlePublic, -80);
        table.observe(10, [1; 6], Kind::BlePublic, -60);
        table.observe(20, [1; 6], Kind::BlePublic, -90);
        assert_eq!(table.len(), 1);
        let mut out = [0u8; 40];
        assert_eq!(table.drain(2020, &mut out), RECORD);
        assert_eq!(&out[..6], &[1; 6]);
        assert_eq!(out[7] as i8, -80);
        assert_eq!(out[8], 3);
        assert_eq!(out[9], 2);
        assert!(table.is_empty());
    }

    #[test]
    fn a_ninth_sample_drops_the_oldest() {
        let mut table = Sightings::<1>::new();
        // -10 through -80, then -90. Keeping the first would median to -45.
        for step in 1..=8 {
            table.observe(step, [1; 6], Kind::BlePublic, -10 * step as i8);
        }
        table.observe(9, [1; 6], Kind::BlePublic, -90);
        let mut out = [0u8; RECORD];
        table.drain(9, &mut out);
        assert_eq!(out[7] as i8, -55);
        assert_eq!(out[8], 9);
    }

    #[test]
    fn a_page_with_no_signal_stays_out_of_the_median() {
        let mut table = Sightings::<1>::new();
        table.observe(0, [1; 6], Kind::Classic, -70);
        table.observe(1, [1; 6], Kind::Classic, NO_SIGNAL);
        table.observe(2, [1; 6], Kind::Classic, -80);
        let mut out = [0u8; RECORD];
        table.drain(2, &mut out);
        assert_eq!(out[7] as i8, -75);
        assert_eq!(out[8], 3);
    }

    #[test]
    fn a_window_of_only_pages_reports_no_signal() {
        let mut table = Sightings::<1>::new();
        table.observe(0, [1; 6], Kind::Classic, NO_SIGNAL);
        table.observe(1, [1; 6], Kind::Classic, NO_SIGNAL);
        let mut out = [0u8; RECORD];
        table.drain(1, &mut out);
        assert_eq!(out[7] as i8, NO_SIGNAL);
        assert_eq!(out[8], 2);
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
    fn a_sighting_newer_than_the_drain_clock_has_no_age() {
        let mut table = Sightings::<1>::new();
        table.observe(1000, [1; 6], Kind::BlePublic, -80);
        let mut out = [0u8; RECORD];
        table.drain(990, &mut out);
        assert_eq!(out[9], 0);
    }

    #[test]
    fn an_old_sighting_saturates_its_age() {
        let mut table = Sightings::<1>::new();
        table.observe(1000, [1; 6], Kind::BlePublic, -80);
        let mut out = [0u8; RECORD];
        table.drain(1000 + 300_000, &mut out);
        assert_eq!(out[9], 255);
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
