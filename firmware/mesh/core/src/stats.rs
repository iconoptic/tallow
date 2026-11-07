//! Fixed-layout records a node sends up: Stats (kind 4), Route (kind 7), and
//! Neighbours (kind 15). The layouts are in `core/README.md`. The firmware builds
//! them with `encode`, and the host reads them with `decode`, so neither side
//! keeps its own offsets.

/// A Stats payload: 21 little-endian `u32`s, then four bytes.
pub const STATS_WIRE: usize = 88;
/// A Route payload: uptime, old parent, new parent, reason.
pub const ROUTE_WIRE: usize = 13;
/// One neighbour in a kind-15 payload: id, then RSSI.
pub const HEARD_WIRE: usize = 5;

/// What a node reports about itself, every ten seconds.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Stats {
    pub uptime_s: u32,
    pub heap_free: u32,
    pub heap_min: u32,
    pub heap_block: u32,
    pub adverts: u32,
    pub classic: u32,
    pub inquiries: u32,
    pub rx_frames: u32,
    pub rx_drops: u32,
    pub rx_bad: u32,
    pub rx_dup: u32,
    pub tx_ok: u32,
    pub tx_failed: u32,
    pub queue_drops: u32,
    pub parent_changes: u32,
    pub evicted: u32,
    pub parent: u32,
    pub settings: u32,
    pub inquiry_started: u32,
    pub inquiry_stopped: u32,
    pub inquiry_errors: u32,
    pub rank: u8,
    pub neighbours: u8,
    pub parent_rssi: i8,
    pub parent_quality: u8,
}

impl Stats {
    pub fn encode(&self) -> [u8; STATS_WIRE] {
        let words = [
            self.uptime_s,
            self.heap_free,
            self.heap_min,
            self.heap_block,
            self.adverts,
            self.classic,
            self.inquiries,
            self.rx_frames,
            self.rx_drops,
            self.rx_bad,
            self.rx_dup,
            self.tx_ok,
            self.tx_failed,
            self.queue_drops,
            self.parent_changes,
            self.evicted,
            self.parent,
            self.settings,
            self.inquiry_started,
            self.inquiry_stopped,
            self.inquiry_errors,
        ];
        let mut out = [0u8; STATS_WIRE];
        for (slot, word) in out.chunks_exact_mut(4).zip(words) {
            slot.copy_from_slice(&word.to_le_bytes());
        }
        out[84] = self.rank;
        out[85] = self.neighbours;
        out[86] = self.parent_rssi as u8;
        out[87] = self.parent_quality;
        out
    }

    /// `None` unless the payload is exactly [`STATS_WIRE`] bytes.
    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() != STATS_WIRE {
            return None;
        }
        Some(Self {
            uptime_s: word(payload, 0)?,
            heap_free: word(payload, 4)?,
            heap_min: word(payload, 8)?,
            heap_block: word(payload, 12)?,
            adverts: word(payload, 16)?,
            classic: word(payload, 20)?,
            inquiries: word(payload, 24)?,
            rx_frames: word(payload, 28)?,
            rx_drops: word(payload, 32)?,
            rx_bad: word(payload, 36)?,
            rx_dup: word(payload, 40)?,
            tx_ok: word(payload, 44)?,
            tx_failed: word(payload, 48)?,
            queue_drops: word(payload, 52)?,
            parent_changes: word(payload, 56)?,
            evicted: word(payload, 60)?,
            parent: word(payload, 64)?,
            settings: word(payload, 68)?,
            inquiry_started: word(payload, 72)?,
            inquiry_stopped: word(payload, 76)?,
            inquiry_errors: word(payload, 80)?,
            rank: *payload.get(84)?,
            neighbours: *payload.get(85)?,
            parent_rssi: *payload.get(86)? as i8,
            parent_quality: *payload.get(87)?,
        })
    }
}

/// A parent change. Zero ids mean no parent. `reason` is the raw code, so a code
/// this build does not know is kept.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Route {
    pub uptime_s: u32,
    pub old: u32,
    pub new: u32,
    pub reason: u8,
}

impl Route {
    pub fn encode(&self) -> [u8; ROUTE_WIRE] {
        let mut out = [0u8; ROUTE_WIRE];
        out[0..4].copy_from_slice(&self.uptime_s.to_le_bytes());
        out[4..8].copy_from_slice(&self.old.to_le_bytes());
        out[8..12].copy_from_slice(&self.new.to_le_bytes());
        out[12] = self.reason;
        out
    }

    /// `None` unless the payload is exactly [`ROUTE_WIRE`] bytes.
    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() != ROUTE_WIRE {
            return None;
        }
        Some(Self {
            uptime_s: word(payload, 0)?,
            old: word(payload, 4)?,
            new: word(payload, 8)?,
            reason: *payload.get(12)?,
        })
    }
}

/// One neighbour a node hears. `rssi` is the short average already kept for that neighbour.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Heard {
    pub id: u32,
    pub rssi: i8,
}

/// Writes as many records as fit in `out`. Returns the bytes written.
pub fn encode_heard(out: &mut [u8], heard: impl IntoIterator<Item = Heard>) -> usize {
    let mut written = 0;
    for item in heard {
        let Some(slot) = out.get_mut(written..written + HEARD_WIRE) else {
            break;
        };
        slot[..4].copy_from_slice(&item.id.to_le_bytes());
        slot[4] = item.rssi as u8;
        written += HEARD_WIRE;
    }
    written
}

/// The neighbours in a kind-15 payload. A partial record at the end is not returned.
pub struct HeardRecords<'a> {
    chunks: core::slice::ChunksExact<'a, u8>,
}

impl<'a> HeardRecords<'a> {
    pub fn new(payload: &'a [u8]) -> Self {
        Self {
            chunks: payload.chunks_exact(HEARD_WIRE),
        }
    }
}

impl Iterator for HeardRecords<'_> {
    type Item = Heard;

    fn next(&mut self) -> Option<Heard> {
        let record = self.chunks.next()?;
        Some(Heard {
            id: u32::from_le_bytes(record.get(..4)?.try_into().ok()?),
            rssi: *record.get(4)? as i8,
        })
    }
}

fn word(payload: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        payload.get(at..at + 4)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Stats {
        Stats {
            uptime_s: 1,
            heap_free: 2,
            heap_min: 3,
            heap_block: 4,
            adverts: 5,
            classic: 6,
            inquiries: 7,
            rx_frames: 8,
            rx_drops: 9,
            rx_bad: 10,
            rx_dup: 11,
            tx_ok: 12,
            tx_failed: 13,
            queue_drops: 14,
            parent_changes: 15,
            evicted: 16,
            parent: 0x1122_3344,
            settings: 18,
            inquiry_started: 19,
            inquiry_stopped: 20,
            inquiry_errors: 21,
            rank: 2,
            neighbours: 3,
            parent_rssi: -55,
            parent_quality: 16,
        }
    }

    #[test]
    fn stats_round_trip_through_encode_and_decode() {
        let stats = sample();
        assert_eq!(Stats::decode(&stats.encode()), Some(stats));
    }

    #[test]
    fn stats_keep_the_layout_in_the_readme() {
        let bytes = sample().encode();
        assert_eq!(bytes.len(), 88);
        assert_eq!(&bytes[..4], &1u32.to_le_bytes());
        assert_eq!(&bytes[64..68], &0x1122_3344u32.to_le_bytes());
        assert_eq!(&bytes[80..84], &21u32.to_le_bytes());
        assert_eq!(&bytes[84..88], &[2, 3, (-55i8) as u8, 16]);
    }

    #[test]
    fn stats_of_the_wrong_size_are_refused() {
        assert_eq!(Stats::decode(&[0u8; 76]), None);
        assert_eq!(Stats::decode(&[0u8; 89]), None);
    }

    #[test]
    fn a_route_round_trips_through_encode_and_decode() {
        let route = Route {
            uptime_s: 1234,
            old: 0x1122_3344,
            new: 0,
            reason: 1,
        };
        assert_eq!(Route::decode(&route.encode()), Some(route));
    }

    #[test]
    fn a_route_of_the_wrong_size_is_refused() {
        assert_eq!(Route::decode(&[0u8; 12]), None);
        assert_eq!(Route::decode(&[0u8; 14]), None);
    }

    #[test]
    fn an_unknown_route_reason_is_kept_as_its_code() {
        let route = Route::decode(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9]).unwrap();
        assert_eq!(route.reason, 9);
    }

    #[test]
    fn a_neighbour_list_round_trips_and_a_partial_tail_is_dropped() {
        let heard = [
            Heard {
                id: 0x1122_3344,
                rssi: -55,
            },
            Heard { id: 2, rssi: -80 },
        ];
        let mut out = [0u8; 12];
        let length = encode_heard(&mut out, heard);
        assert_eq!(length, 10);
        let back: Vec<Heard> = HeardRecords::new(&out[..length]).collect();
        assert_eq!(back, heard);
        assert_eq!(HeardRecords::new(&out[..7]).count(), 1);
        assert_eq!(encode_heard(&mut [0u8; 4], heard), 0);
    }
}
