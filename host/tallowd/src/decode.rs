//! Typed records from the frames a node sends up. Every offset comes from
//! `tallow_mesh_core`, so the host and the firmware cannot disagree about a
//! layout. Each function takes the payload of one frame.

use tallow_mesh_core::mesh::Reason;
use tallow_mesh_core::names::NameRecords;
use tallow_mesh_core::sightings::{Kind, Records};
use tallow_mesh_core::stats::{Route, Stats};

/// One device heard by one node, from a Report frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sighting {
    pub addr: [u8; 6],
    /// `None` for a transport code this build does not know.
    pub kind: Option<Kind>,
    pub rssi: i8,
    pub count: u8,
    pub age_s: u8,
}

/// A device's name and maker, from a Name frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    pub addr: [u8; 6],
    pub kind: Option<Kind>,
    pub company: Option<u16>,
    /// Zero when unknown.
    pub class: u32,
    /// Invalid UTF-8 is replaced, as `parse_names` does.
    pub name: String,
}

/// A parent change, from a Route frame. `None` ids mean no parent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParentChange {
    pub uptime_s: u32,
    pub old: Option<u32>,
    pub new: Option<u32>,
    /// `None` for a reason code this build does not know.
    pub reason: Option<Reason>,
}

/// A node's own report, from a Stats frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeStats {
    pub stats: Stats,
    /// `None` when the node has no parent.
    pub parent: Option<u32>,
}

pub fn sightings(payload: &[u8]) -> Vec<Sighting> {
    Records::new(payload)
        .map(|record| Sighting {
            addr: record.addr,
            kind: Kind::from_u8(record.kind),
            rssi: record.best,
            count: record.count,
            age_s: record.age_s,
        })
        .collect()
}

pub fn names(payload: &[u8]) -> Vec<Named> {
    NameRecords::new(payload)
        .map(|record| Named {
            addr: record.addr,
            kind: Kind::from_u8(record.kind),
            company: record.company,
            class: record.class,
            name: String::from_utf8_lossy(record.name).into_owned(),
        })
        .collect()
}

pub fn parent_change(payload: &[u8]) -> Option<ParentChange> {
    let route = Route::decode(payload)?;
    Some(ParentChange {
        uptime_s: route.uptime_s,
        old: nonzero(route.old),
        new: nonzero(route.new),
        reason: reason(route.reason),
    })
}

pub fn node_stats(payload: &[u8]) -> Option<NodeStats> {
    let stats = Stats::decode(payload)?;
    Some(NodeStats {
        parent: nonzero(stats.parent),
        stats,
    })
}

fn nonzero(id: u32) -> Option<u32> {
    (id != 0).then_some(id)
}

fn reason(code: u8) -> Option<Reason> {
    match code {
        0 => Some(Reason::BeaconTimeout),
        1 => Some(Reason::FailedAcks),
        2 => Some(Reason::BetterPath),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tallow_mesh_core::names::Names;
    use tallow_mesh_core::sightings::Sightings;

    #[test]
    fn a_report_from_the_table_becomes_typed_sightings() {
        let mut table = Sightings::<4>::new();
        table.observe(0, [0xAB; 6], Kind::BleRandom, -70);
        table.observe(0, [0xAB; 6], Kind::BleRandom, -62);
        let mut out = [0u8; 40];
        let length = table.drain(4000, &mut out);
        assert_eq!(
            sightings(&out[..length]),
            vec![Sighting {
                addr: [0xAB; 6],
                kind: Some(Kind::BleRandom),
                rssi: -62,
                count: 2,
                age_s: 4,
            }]
        );
    }

    #[test]
    fn a_name_from_the_queue_becomes_a_typed_name_with_the_bytes_kept_when_not_utf8() {
        let mut names_queue = Names::<8, 4>::new();
        names_queue.offer(
            1,
            [3; 6],
            Kind::Classic,
            None,
            Some([9, 0, 0x24]),
            Some(b"Flip 5"),
        );
        names_queue.offer(
            1,
            [4; 6],
            Kind::BlePublic,
            Some(0x004C),
            None,
            Some(&[0xFF, b'a']),
        );
        let mut out = [0u8; 200];
        let length = names_queue.drain(&mut out);
        let named = names(&out[..length]);
        assert_eq!(named.len(), 2);
        assert_eq!(named[0].name, "Flip 5");
        assert_eq!(named[0].company, None);
        assert_eq!(named[0].class, 0x240009);
        assert_eq!(named[1].company, Some(0x004C));
        assert_eq!(named[1].name, "\u{FFFD}a");
    }

    #[test]
    fn a_name_past_the_end_stops_the_typed_parse() {
        let mut names_queue = Names::<8, 4>::new();
        names_queue.offer(1, [3; 6], Kind::Classic, None, None, Some(b"abc"));
        let mut out = [0u8; 200];
        let length = names_queue.drain(&mut out);
        assert!(names(&out[..length - 1]).is_empty());
    }

    #[test]
    fn a_parent_change_round_trips_with_zero_as_no_parent() {
        let payload = Route {
            uptime_s: 77,
            old: 0x0A0B_0C0D,
            new: 0,
            reason: 0,
        }
        .encode();
        assert_eq!(
            parent_change(&payload),
            Some(ParentChange {
                uptime_s: 77,
                old: Some(0x0A0B_0C0D),
                new: None,
                reason: Some(Reason::BeaconTimeout),
            })
        );
    }

    #[test]
    fn a_parent_change_of_the_wrong_size_is_refused_and_an_unknown_reason_is_none() {
        assert_eq!(parent_change(&[0u8; 12]), None);
        let unknown = parent_change(
            &Route {
                reason: 9,
                ..Route::default()
            }
            .encode(),
        )
        .unwrap();
        assert_eq!(unknown.reason, None);
    }

    #[test]
    fn node_stats_round_trip_and_zero_parent_is_none() {
        let stats = Stats {
            uptime_s: 10,
            rank: 255,
            parent_rssi: -40,
            ..Stats::default()
        };
        let decoded = node_stats(&stats.encode()).unwrap();
        assert_eq!(decoded.parent, None);
        assert_eq!(decoded.stats, stats);
        assert_eq!(node_stats(&[0u8; 89]), None);
    }
}
