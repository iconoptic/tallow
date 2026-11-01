//! Which room a device is in, from which node hears it strongest.
//!
//! `data/mesh/rooms.toml` names a room for each node:
//!
//! ```toml
//! [nodes]
//! 541e99a9 = "Kitchen"
//! 2c7f0b11 = "Hall"
//! ```
//!
//! A node only reports what it hears, so a device that left keeps its last
//! reading. Readings older than [`MAX_AGE_S`] are dropped, and a device with no
//! live reading has no room. A room is a report, never a position.
//!
//! Nothing here reads the clock. Every call takes `now` in seconds, as
//! `time.time()` gives it, so the tests are exact.

use anyhow::{anyhow, bail, Context};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

/// Where `run` reads the rooms when `--rooms` is not given.
pub const DEFAULT_PATH: &str = "data/mesh/rooms.toml";

/// How many readings a node keeps for each device.
pub const RING: usize = 12;

/// A reading older than this is dropped.
pub const MAX_AGE_S: f64 = 60.0;

/// A new room must lead the current one by this many dB...
pub const MARGIN_DB: f64 = 3.0;

/// ...on this many reports in a row, before the device moves.
pub const CONFIRMS: u32 = 3;

/// The RSSI a page reports, when it succeeds: a page has no signal strength,
/// so it says nothing about distance. Such a sighting is not a reading and is
/// not a report.
const NO_SIGNAL: i8 = -127;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    nodes: BTreeMap<String, toml::Spanned<String>>,
}

/// The rooms of the nodes, and where each device is now.
#[derive(Debug, Default)]
pub struct Rooms {
    /// Node id (eight lower-case hex digits) to room name.
    names: BTreeMap<String, String>,
    /// Per device, keyed as the map groups it, with its readings and room.
    devices: BTreeMap<String, Device>,
}

#[derive(Debug, Default)]
struct Device {
    /// Only nodes that have a room are kept.
    nodes: BTreeMap<String, Ring>,
    /// The node of the current room, once one is taken.
    room: Option<String>,
    /// The node a move is waiting on, and how many reports in a row it has led.
    pending: Option<(String, u32)>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Ring {
    slots: [Option<Reading>; RING],
    next: usize,
}

#[derive(Clone, Copy, Debug)]
struct Reading {
    rssi: i8,
    /// When the sighting was heard, in seconds.
    at: f64,
}

impl Rooms {
    /// Read the rooms at `path`. A missing file means no rooms and is not an error.
    pub fn load(path: &Path) -> anyhow::Result<Rooms> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Rooms::default()),
            Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
        };
        Rooms::parse(&text).map_err(|err| anyhow!("{}: {err:#}", path.display()))
    }

    /// The file's text, parsed. Errors name the line.
    pub(crate) fn parse(text: &str) -> anyhow::Result<Rooms> {
        let file: File = toml::from_str(text).map_err(|err| match err.span() {
            Some(span) => anyhow!("line {}: {}", line_at(text, span.start), err.message()),
            None => anyhow!("{}", err.message()),
        })?;
        let mut names = BTreeMap::new();
        for (listed, room) in file.nodes {
            let line = line_at(text, room.span().start);
            let Some(node) = normal_node(&listed) else {
                bail!("line {line}: node {listed} is not eight hex digits like 541e99a9");
            };
            let name = room.into_inner();
            if name.trim().is_empty() {
                bail!("line {line}: node {node} has an empty room name");
            }
            if names.insert(node.clone(), name).is_some() {
                bail!("line {line}: node {node} is listed twice");
            }
        }
        Ok(Rooms {
            names,
            devices: BTreeMap::new(),
        })
    }

    /// True when no node has a room, so nothing is tracked and no device gets one.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// One report of `device` heard by `node` at `rssi`, heard at `at` and read at
    /// `now`. A node with no room is ignored, and so is a page's -127.
    pub fn hear(&mut self, device: &str, node: &str, rssi: i8, at: f64, now: f64) {
        if rssi == NO_SIGNAL || !self.names.contains_key(node) {
            return;
        }
        let track = self.devices.entry(device.to_string()).or_default();
        track
            .nodes
            .entry(node.to_string())
            .or_default()
            .push(Reading { rssi, at });
        let Some((best, best_median)) = track.candidate(now) else {
            track.pending = None;
            return;
        };
        match track.room.clone() {
            None => {
                track.room = Some(best);
                track.pending = None;
            }
            Some(current) if current == best => track.pending = None,
            Some(current) => {
                // A current room with no live reading leads by any amount.
                let lead = match track.node_median(&current, now) {
                    Some(median) => best_median - median,
                    None => f64::INFINITY,
                };
                if lead < MARGIN_DB {
                    track.pending = None;
                    return;
                }
                let count = match &track.pending {
                    Some((node, count)) if *node == best => count + 1,
                    _ => 1,
                };
                if count >= CONFIRMS {
                    track.room = Some(best);
                    track.pending = None;
                } else {
                    track.pending = Some((best, count));
                }
            }
        }
    }

    /// The room `device` is in at `now`, or none when it has no live reading.
    pub fn room(&self, device: &str, now: f64) -> Option<&str> {
        let track = self.devices.get(device)?;
        if !track.nodes.values().any(|ring| ring.has_live(now)) {
            return None;
        }
        let node = track.room.as_deref()?;
        self.names.get(node).map(String::as_str)
    }
}

impl Device {
    /// The node with the highest median of its live readings, with that median.
    /// A tie goes to the lowest node id, which cannot flicker: a tie leads by 0 dB.
    fn candidate(&self, now: f64) -> Option<(String, f64)> {
        let mut best: Option<(String, f64)> = None;
        for (node, ring) in &self.nodes {
            let Some(median) = ring.median(now) else {
                continue;
            };
            if best.as_ref().is_none_or(|(_, kept)| median > *kept) {
                best = Some((node.clone(), median));
            }
        }
        best
    }

    fn node_median(&self, node: &str, now: f64) -> Option<f64> {
        self.nodes.get(node)?.median(now)
    }
}

impl Ring {
    fn push(&mut self, reading: Reading) {
        self.slots[self.next] = Some(reading);
        self.next = (self.next + 1) % RING;
    }

    fn live(&self, now: f64) -> impl Iterator<Item = &Reading> {
        self.slots
            .iter()
            .flatten()
            .filter(move |reading| now - reading.at <= MAX_AGE_S)
    }

    fn has_live(&self, now: f64) -> bool {
        self.live(now).next().is_some()
    }

    fn median(&self, now: f64) -> Option<f64> {
        median(self.live(now).map(|reading| reading.rssi).collect())
    }
}

/// The median of `values`: the middle one of an odd count, and the mean of the
/// middle two of an even count. None for no values.
fn median(mut values: Vec<i8>) -> Option<f64> {
    values.sort_unstable();
    let count = values.len();
    if count == 0 {
        return None;
    }
    let middle = count / 2;
    if count % 2 == 1 {
        Some(f64::from(values[middle]))
    } else {
        Some((f64::from(values[middle - 1]) + f64::from(values[middle])) / 2.0)
    }
}

/// Eight hex digits, either case, as lower-case text.
fn normal_node(text: &str) -> Option<String> {
    let well_formed = text.len() == 8 && text.bytes().all(|b| b.is_ascii_hexdigit());
    well_formed.then(|| text.to_ascii_lowercase())
}

/// The 1-based line that `offset` falls on.
fn line_at(text: &str, offset: usize) -> usize {
    text.get(..offset).unwrap_or(text).matches('\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    const KITCHEN: &str = "541e99a9";
    const HALL: &str = "2c7f0b11";
    const SPEAKER: &str = "23:00:00:11:A0:38";

    fn rooms() -> Rooms {
        Rooms::parse(&format!(
            "[nodes]\n{KITCHEN} = \"Kitchen\"\n{HALL} = \"Hall\"\n"
        ))
        .expect("parse")
    }

    /// One report from `node` at `rssi`, heard now, at `now`.
    fn report(rooms: &mut Rooms, node: &str, rssi: i8, now: f64) {
        rooms.hear(SPEAKER, node, rssi, now, now);
    }

    fn room_now(rooms: &Rooms, now: f64) -> Option<&str> {
        rooms.room(SPEAKER, now)
    }

    #[test]
    fn a_steady_device_takes_its_room_at_once_and_keeps_it() {
        let mut rooms = rooms();
        report(&mut rooms, KITCHEN, -60, 100.0);
        assert_eq!(room_now(&rooms, 100.0), Some("Kitchen"));
        for step in 1..=10 {
            let now = 100.0 + f64::from(step);
            report(&mut rooms, KITCHEN, -60, now);
            report(&mut rooms, HALL, -75, now);
            assert_eq!(room_now(&rooms, now), Some("Kitchen"), "step {step}");
        }
    }

    #[test]
    fn a_boundary_device_at_three_db_never_flickers_between_rooms() {
        let mut rooms = rooms();
        report(&mut rooms, KITCHEN, -60, 100.0);
        // Hall's readings alternate -57 and -60. Its median is -57 on odd counts
        // (a lead of exactly 3 dB, which passes) and -58.5 on even counts (1.5 dB,
        // which fails). A pass never follows a pass, so the run never reaches three.
        for step in 0..20 {
            let now = 101.0 + f64::from(step);
            let hall = if step % 2 == 0 { -57 } else { -60 };
            report(&mut rooms, HALL, hall, now);
            assert_eq!(room_now(&rooms, now), Some("Kitchen"), "step {step}");
        }
    }

    #[test]
    fn a_real_move_takes_three_reports_in_a_row_and_not_two() {
        let mut rooms = rooms();
        report(&mut rooms, KITCHEN, -80, 100.0);
        report(&mut rooms, HALL, -55, 101.0);
        report(&mut rooms, HALL, -55, 102.0);
        assert_eq!(room_now(&rooms, 102.0), Some("Kitchen"));
        report(&mut rooms, HALL, -55, 103.0);
        assert_eq!(room_now(&rooms, 103.0), Some("Hall"));
    }

    #[test]
    fn a_failed_report_starts_the_count_again() {
        let mut rooms = rooms();
        report(&mut rooms, KITCHEN, -60, 100.0);
        report(&mut rooms, HALL, -57, 101.0); // leads by 3 dB: count 1
        report(&mut rooms, HALL, -60, 102.0); // Hall's median is -58.5: count reset
        report(&mut rooms, HALL, -57, 103.0); // count 1 again
        report(&mut rooms, HALL, -57, 104.0); // count 2
        assert_eq!(room_now(&rooms, 104.0), Some("Kitchen"));
        report(&mut rooms, HALL, -57, 105.0); // count 3
        assert_eq!(room_now(&rooms, 105.0), Some("Hall"));
    }

    #[test]
    fn a_device_one_node_stops_hearing_leaves_that_room_once_the_other_node_confirms_it() {
        let mut rooms = rooms();
        report(&mut rooms, KITCHEN, -60, 100.0);
        report(&mut rooms, HALL, -70, 100.5);
        // The kitchen reading is now 61 s old. The hall still hears the device.
        rooms.hear(SPEAKER, HALL, -70, 161.0, 161.0);
        assert_eq!(
            room_now(&rooms, 161.0),
            Some("Kitchen"),
            "not yet confirmed"
        );
        rooms.hear(SPEAKER, HALL, -70, 162.0, 162.0);
        rooms.hear(SPEAKER, HALL, -70, 163.0, 163.0);
        assert_eq!(room_now(&rooms, 163.0), Some("Hall"));
    }

    #[test]
    fn a_device_nobody_hears_for_sixty_seconds_has_no_room() {
        let mut rooms = rooms();
        report(&mut rooms, KITCHEN, -60, 100.0);
        assert_eq!(
            room_now(&rooms, 160.0),
            Some("Kitchen"),
            "60 s old is still live"
        );
        assert_eq!(room_now(&rooms, 160.5), None, "older than 60 s is dropped");
    }

    #[test]
    fn a_page_with_no_signal_is_not_a_reading_and_not_a_report() {
        let mut rooms = rooms();
        // -127 is a page that succeeded, not a distance. It never sets a room...
        report(&mut rooms, KITCHEN, -127, 100.0);
        assert_eq!(room_now(&rooms, 100.0), None);
        report(&mut rooms, KITCHEN, -60, 101.0);
        // ...and it never counts toward a move or pushes out a real reading.
        for step in 0..5 {
            let now = 102.0 + f64::from(step);
            report(&mut rooms, HALL, -127, now);
        }
        report(&mut rooms, KITCHEN, -127, 107.0);
        assert_eq!(room_now(&rooms, 107.0), Some("Kitchen"));
        let track = &rooms.devices[SPEAKER];
        assert_eq!(track.nodes[KITCHEN].live(107.0).count(), 1);
        assert!(!track.nodes.contains_key(HALL));
    }

    #[test]
    fn a_node_with_no_room_is_not_a_candidate_and_is_not_kept() {
        let mut rooms = rooms();
        report(&mut rooms, "aaaaaaaa", -40, 100.0);
        assert_eq!(room_now(&rooms, 100.0), None);
        assert!(!rooms.devices.contains_key(SPEAKER));
    }

    #[test]
    fn the_median_of_an_even_count_is_the_mean_of_the_middle_two() {
        assert_eq!(median(vec![-70, -60]), Some(-65.0));
        assert_eq!(median(vec![-71, -60, -62, -64]), Some(-63.0));
        assert_eq!(median(vec![-60, -70, -62]), Some(-62.0));
        assert_eq!(median(vec![-61]), Some(-61.0));
        assert_eq!(median(Vec::new()), None);
    }

    #[test]
    fn a_node_keeps_the_last_twelve_readings_and_medians_only_those() {
        let mut ring = Ring::default();
        for step in 0..13 {
            ring.push(Reading {
                rssi: if step == 0 { -40 } else { -70 },
                at: 100.0,
            });
        }
        // The -40 was the oldest, and is gone: the median is -70.
        assert_eq!(ring.median(100.0), Some(-70.0));
        assert_eq!(ring.live(100.0).count(), RING);
    }

    #[test]
    fn a_median_uses_the_even_count_rule_across_a_node_with_two_readings() {
        let mut rooms = rooms();
        report(&mut rooms, KITCHEN, -60, 100.0);
        report(&mut rooms, KITCHEN, -70, 101.0);
        // Hall at -64 beats Kitchen's median of -65 by 1 dB only: no move.
        for step in 0..4 {
            report(&mut rooms, HALL, -64, 102.0 + f64::from(step));
        }
        assert_eq!(room_now(&rooms, 105.0), Some("Kitchen"));
    }

    #[test]
    fn a_missing_rooms_file_means_no_rooms_and_no_error() {
        let path = std::env::temp_dir().join("tallowd-rooms-no-such-file.toml");
        assert!(Rooms::load(&path)
            .expect("missing is not an error")
            .is_empty());
    }

    #[test]
    fn a_broken_rooms_file_is_an_error_that_names_the_line() {
        let message = format!(
            "{:#}",
            Rooms::parse("[nodes]\n541e99a9 = \"Kitchen\"\n2c7f0b1 = \"Hall\"\n")
                .expect_err("refused")
        );
        assert!(message.contains("line 3"), "{message}");
        assert!(message.contains("2c7f0b1"), "{message}");
        let message = format!(
            "{:#}",
            Rooms::parse("[nodes]\n541e99a9 = \"Kitchen\"\n541E99A9 = \"Hall\"\n")
                .expect_err("refused")
        );
        assert!(message.contains("listed twice"), "{message}");
        let message = format!("{:#}", Rooms::parse("[node]\n").expect_err("refused"));
        assert!(message.starts_with("line 1"), "{message}");
    }
}
