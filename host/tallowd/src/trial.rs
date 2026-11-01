//! The delivery trial: how much of what each node sends reaches the root, under
//! each scan setting. [`run`] is `run_trial` in `tools/mesh_host.py`.
//!
//! The trial reads the port only through [`Wire`], and takes its times from the
//! wire's clock. So the logic runs in tests with a scripted wire and no port.

use crate::decode;
use crate::link::{parse_line, ScanSettings, DEFAULT_SCAN};
use crate::map::{handle, Map};
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use tallow_mesh_core::wire::FrameKind;

/// The scan settings each trial setting measures, in order: label, window ms,
/// interval ms, inquiry period s, inquiry length. Matches `TRIAL` in `tools/mesh_host.py`.
pub const TRIAL: [(&str, u16, u16, u16, u8); 5] = [
    ("radio alone", 0, 100, 0, 0),
    ("BLE 30/100", 30, 100, 0, 0),
    ("BLE 60/100", 60, 100, 0, 0),
    ("BLE 100/100", 100, 100, 0, 0),
    ("BLE 30/100, Classic", 30, 100, 12, 4),
];

/// Probes run a little under their rate, because each waits for the loop. A node
/// delivering less than this share of the nominal count has stopped partway.
pub const PROBE_PACING: f64 = 0.9;

/// Seconds each setting waits for the flood to settle before it counts. Matches
/// `settle` in `run_trial`.
pub const SETTLE_S: f64 = 6.0;

/// What one trial run measures and how it judges the result.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// Seconds counted per setting.
    pub hold: f64,
    /// Probes per second each node sends.
    pub hz: u8,
    /// Probe payload size in bytes.
    pub size: u8,
    /// Percent every setting must deliver for the run to pass.
    pub pass_mark: f64,
}

impl Default for Plan {
    /// The defaults of `tools/mesh_host.py --trial`.
    fn default() -> Self {
        Plan {
            hold: 45.0,
            hz: 10,
            size: 200,
            pass_mark: 95.0,
        }
    }
}

/// The port as the trial sees it: a clock, the lines read since the last call,
/// and a scan command. `Link` is the real one.
pub trait Wire {
    /// Seconds since the epoch.
    fn clock(&self) -> f64;
    /// The `@` lines read since the last call. Reading may also send the heartbeat.
    fn read(&mut self) -> Result<Vec<String>>;
    /// Sends scan settings to every node, and returns the command id they echo.
    fn command(&mut self, settings: &ScanSettings) -> Result<u16>;
}

/// One node's delivery under one setting. A node with no probes has a row
/// with the origin `-`, so the run still fails.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub label: &'static str,
    pub origin: String,
    pub delivered: usize,
    pub expected: usize,
    pub percent: f64,
    /// The node's free heap at its last stats echo for this setting.
    pub heap: Option<u32>,
}

/// One node's delivery, as [`delivery`] judges it.
#[derive(Clone, Debug, PartialEq)]
pub struct Delivery {
    pub origin: String,
    pub delivered: usize,
    pub expected: usize,
    pub percent: f64,
}

/// The probes each origin sent, by sequence number, and the heap each node
/// reported under the setting in force.
#[derive(Default)]
struct Counts {
    seqs: BTreeMap<String, BTreeSet<u32>>,
    heaps: BTreeMap<String, u32>,
}

/// Runs each setting in [`TRIAL`] and returns a row per node per setting. The
/// scan settings go back to [`DEFAULT_SCAN`] at the end, even when no node answered.
/// `report` gets each row's line as it is measured.
pub fn run<W: Wire>(
    wire: &mut W,
    world: &mut Map,
    plan: &Plan,
    report: &mut dyn FnMut(&str),
) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    for (label, window_ms, interval_ms, inquiry_period_s, inquiry_length) in TRIAL {
        let settings = ScanSettings {
            window_ms,
            interval_ms,
            inquiry_period_s,
            inquiry_length,
            probe_hz: plan.hz,
            probe_size: plan.size,
        };
        let command = wire.command(&settings)?;
        settle(wire, world)?;
        let counts = measure(wire, world, plan.hold, command)?;
        let results = delivery(&counts.seqs, &reporting(world), plan.hold, plan.hz);
        if results.is_empty() {
            report(format!("{label:22} no probes arrived").as_str());
            rows.push(Row {
                label,
                origin: "-".to_string(),
                delivered: 0,
                expected: 0,
                percent: 0.0,
                heap: None,
            });
        }
        for result in results {
            let heap = counts.heaps.get(&result.origin).copied();
            let row = Row {
                label,
                origin: result.origin,
                delivered: result.delivered,
                expected: result.expected,
                percent: result.percent,
                heap,
            };
            report(row_line(&row).as_str());
            rows.push(row);
        }
    }
    wire.command(&DEFAULT_SCAN)?;
    Ok(rows)
}

/// True when every row meets the pass mark, and there is at least one row.
pub fn passed(rows: &[Row], pass_mark: f64) -> bool {
    !rows.is_empty() && rows.iter().all(|row| row.percent >= pass_mark)
}

/// Each origin's delivery. The sequence numbers alone only show holes between
/// the first and last that arrived. So the expected count is at least what the
/// hold should have produced, and a node that reported stats over the air but
/// sent no probes counts as 0%. `reporting` holds the nodes whose stats came
/// over the air, so the root, whose link RSSI is zero, is not in it.
pub fn delivery(
    seqs: &BTreeMap<String, BTreeSet<u32>>,
    reporting: &BTreeSet<String>,
    hold: f64,
    hz: u8,
) -> Vec<Delivery> {
    let least = (hold * f64::from(hz) * PROBE_PACING) as usize;
    let origins: BTreeSet<&String> = seqs.keys().chain(reporting.iter()).collect();
    origins
        .into_iter()
        .map(|origin| {
            let seen = seqs.get(origin);
            let span = match (
                seen.and_then(|set| set.first()),
                seen.and_then(|set| set.last()),
            ) {
                (Some(&low), Some(&high)) => (u64::from(high) - u64::from(low) + 1) as usize,
                _ => 0,
            };
            let delivered = seen.map_or(0, BTreeSet::len);
            let expected = span.max(least);
            let percent = if expected == 0 {
                0.0
            } else {
                100.0 * delivered as f64 / expected as f64
            };
            Delivery {
                origin: origin.clone(),
                delivered,
                expected,
                percent,
            }
        })
        .collect()
}

/// Reads for [`SETTLE_S`], so the setting floods out and the radio settles before counting.
/// The frames are handled, but no probe is counted.
fn settle<W: Wire>(wire: &mut W, world: &mut Map) -> Result<()> {
    let until = wire.clock() + SETTLE_S;
    while wire.clock() < until {
        for text in wire.read()? {
            if let Some(line) = parse_line(&text) {
                handle(world, &line, wire.clock(), true);
            }
        }
    }
    Ok(())
}

/// Reads for `hold` seconds and counts what arrives. A probe counts when it
/// crossed the air, so the root's own probes (RSSI zero) do not. A stats frame
/// counts when it echoes `command`.
fn measure<W: Wire>(wire: &mut W, world: &mut Map, hold: f64, command: u16) -> Result<Counts> {
    let end = wire.clock() + hold;
    let mut counts = Counts::default();
    while wire.clock() < end {
        for text in wire.read()? {
            let Some(line) = parse_line(&text) else {
                continue;
            };
            handle(world, &line, wire.clock(), true);
            if line.frame == u32::from(FrameKind::Probe as u8) && line.rssi != 0 {
                if let Some(seq) = probe_seq(&line.payload) {
                    counts
                        .seqs
                        .entry(line.origin.clone())
                        .or_default()
                        .insert(seq);
                }
            }
            if line.frame == u32::from(FrameKind::Stats as u8) {
                if let Some(found) = decode::node_stats(&line.payload) {
                    if u32::from(command) == found.stats.settings {
                        counts
                            .heaps
                            .insert(line.origin.clone(), found.stats.heap_free);
                    }
                }
            }
        }
    }
    Ok(counts)
}

/// The sequence number a probe carries: its first four bytes, little-endian.
/// A shorter payload carries none.
fn probe_seq(payload: &[u8]) -> Option<u32> {
    <[u8; 4]>::try_from(payload.get(..4)?)
        .ok()
        .map(u32::from_le_bytes)
}

/// The nodes whose stats came over the air, so they should have sent probes.
fn reporting(world: &Map) -> BTreeSet<String> {
    world
        .stats_links()
        .filter(|(_, link_rssi)| *link_rssi != 0)
        .map(|(node, _)| node.to_string())
        .collect()
}

/// One row as `run_trial` prints it.
fn row_line(row: &Row) -> String {
    let heap = row
        .heap
        .map_or_else(|| "None".to_string(), |heap| heap.to_string());
    format!(
        "{:<22} node {}  {:>5}/{:<5} {:6.2}%  heap {heap}",
        row.label, row.origin, row.delivered, row.expected, row.percent
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tallow_mesh_core::stats::Stats;

    fn set(values: &[u32]) -> BTreeSet<u32> {
        values.iter().copied().collect()
    }

    fn seqs(origin: &str, values: &[u32]) -> BTreeMap<String, BTreeSet<u32>> {
        BTreeMap::from([(origin.to_string(), set(values))])
    }

    fn reporting_of(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn a_clean_run_is_measured_by_what_it_sent() {
        let rows = delivery(
            &seqs("leaf", &(1..=440).collect::<Vec<_>>()),
            &reporting_of(&["leaf"]),
            45.0,
            10,
        );
        assert_eq!(
            rows,
            vec![Delivery {
                origin: "leaf".into(),
                delivered: 440,
                expected: 440,
                percent: 100.0
            }]
        );
    }

    #[test]
    fn a_hole_in_the_middle_counts() {
        let values: Vec<u32> = (1..=440).filter(|seq| *seq != 200 && *seq != 201).collect();
        let rows = delivery(&seqs("leaf", &values), &reporting_of(&["leaf"]), 45.0, 10);
        assert_eq!((rows[0].delivered, rows[0].expected), (438, 440));
    }

    #[test]
    fn a_node_that_stops_partway_fails() {
        let rows = delivery(
            &seqs("leaf", &(1..=100).collect::<Vec<_>>()),
            &reporting_of(&["leaf"]),
            45.0,
            10,
        );
        assert_eq!(rows[0].expected, 405);
        assert!(rows[0].percent < 95.0);
    }

    #[test]
    fn a_node_that_sends_nothing_fails() {
        let rows = delivery(&BTreeMap::new(), &reporting_of(&["leaf"]), 45.0, 10);
        assert_eq!(
            rows,
            vec![Delivery {
                origin: "leaf".into(),
                delivered: 0,
                expected: 405,
                percent: 0.0
            }]
        );
    }

    #[test]
    fn the_root_is_not_expected_to_cross_the_air() {
        assert!(delivery(&BTreeMap::new(), &BTreeSet::new(), 45.0, 10).is_empty());
    }

    /// A wire that answers each read with `feed(clock, command id)`, and moves
    /// its clock 0.1 s per read. It records each scan command it is sent.
    struct Radio {
        now: f64,
        command: u16,
        sent: Vec<ScanSettings>,
        feed: Box<dyn FnMut(f64, u16) -> Vec<String>>,
    }

    impl Wire for Radio {
        fn clock(&self) -> f64 {
            self.now
        }

        fn read(&mut self) -> Result<Vec<String>> {
            self.now += 0.1;
            Ok((self.feed)(self.now, self.command))
        }

        fn command(&mut self, settings: &ScanSettings) -> Result<u16> {
            self.sent.push(*settings);
            self.command += 1;
            Ok(self.command)
        }
    }

    /// A leaf that sends a probe on every `every`th read, with `rssi` and a
    /// payload of `payload` bytes. Its stats echo the setting every 10 reads,
    /// and the root's stats come over the air at RSSI zero.
    fn leaf(every: u32, rssi: i8, payload: usize) -> Radio {
        let mut reads = 0u32;
        let mut seq = 0u32;
        Radio {
            now: 1000.0,
            command: 0,
            sent: Vec::new(),
            feed: Box::new(move |_now, command| {
                reads += 1;
                let mut lines = Vec::new();
                if reads.is_multiple_of(every) {
                    seq += 1;
                    let mut bytes = seq.to_le_bytes().to_vec();
                    bytes.resize(payload, 0);
                    lines.push(format!("@U 5 00000005 1 {seq} 1 {rssi} {}", hex(&bytes)));
                }
                if reads.is_multiple_of(10) {
                    let root = Stats::default().encode();
                    lines.push(format!("@U 4 00000001 1 {reads} 0 0 {}", hex(&root)));
                    let stats = Stats {
                        settings: u32::from(command),
                        heap_free: 1000,
                        ..Stats::default()
                    };
                    lines.push(format!(
                        "@U 4 00000005 1 {reads} 1 -50 {}",
                        hex(&stats.encode())
                    ));
                }
                lines
            }),
        }
    }

    fn trial(radio: &mut Radio) -> (Vec<Row>, Vec<String>) {
        let mut world = Map::default();
        let mut printed = Vec::new();
        let rows = run(radio, &mut world, &Plan::default(), &mut |line| {
            printed.push(line.to_string())
        })
        .expect("trial");
        (rows, printed)
    }

    #[test]
    fn a_leaf_that_delivers_every_probe_passes_every_setting() {
        let mut radio = leaf(1, 60, 8);
        let (rows, _) = trial(&mut radio);
        assert_eq!(rows.len(), TRIAL.len());
        for row in &rows {
            assert_eq!(row.origin, "00000005");
            assert!(row.percent >= 95.0, "{row:?}");
            assert_eq!(row.heap, Some(1000));
        }
        assert!(passed(&rows, 95.0));
    }

    #[test]
    fn a_leaf_that_sends_half_its_probes_fails_the_pass_mark() {
        let mut radio = leaf(2, 60, 8);
        let (rows, _) = trial(&mut radio);
        assert!(rows.iter().all(|row| row.percent < 95.0));
        assert!(!passed(&rows, 95.0));
    }

    #[test]
    fn a_probe_that_never_crossed_the_air_is_not_counted() {
        let mut radio = leaf(1, 0, 8);
        let (rows, _) = trial(&mut radio);
        assert!(rows.iter().all(|row| row.percent == 0.0), "{rows:?}");
    }

    #[test]
    fn a_probe_too_short_to_carry_a_sequence_is_not_counted() {
        let mut radio = leaf(1, 60, 2);
        let (rows, _) = trial(&mut radio);
        assert!(rows.iter().all(|row| row.delivered == 0));
    }

    #[test]
    fn a_trial_with_no_node_heard_fails() {
        let mut radio = Radio {
            now: 0.0,
            command: 0,
            sent: Vec::new(),
            feed: Box::new(|_, _| Vec::new()),
        };
        let (rows, printed) = trial(&mut radio);
        assert_eq!(rows.len(), TRIAL.len());
        assert!(rows.iter().all(|row| row.origin == "-"));
        assert!(!passed(&rows, 95.0));
        assert_eq!(printed.len(), TRIAL.len());
        assert!(printed[0].ends_with("no probes arrived"));
    }

    #[test]
    fn the_trial_sends_each_setting_then_restores_the_default_scan() {
        let mut radio = leaf(1, 60, 8);
        trial(&mut radio);
        let sent = radio.sent;
        assert_eq!(sent.len(), TRIAL.len() + 1);
        for (settings, (_, window_ms, interval_ms, inquiry_period_s, inquiry_length)) in
            sent.iter().zip(TRIAL)
        {
            assert_eq!(
                (
                    settings.window_ms,
                    settings.interval_ms,
                    settings.inquiry_period_s,
                    settings.inquiry_length
                ),
                (window_ms, interval_ms, inquiry_period_s, inquiry_length)
            );
            assert_eq!((settings.probe_hz, settings.probe_size), (10, 200));
        }
        assert_eq!(sent.last(), Some(&DEFAULT_SCAN));
    }

    #[test]
    fn the_trial_rows_are_the_pythons_table() {
        // The same five rows as TRIAL in tools/mesh_host.py.
        let want = [
            ("radio alone", 0, 100, 0, 0),
            ("BLE 30/100", 30, 100, 0, 0),
            ("BLE 60/100", 60, 100, 0, 0),
            ("BLE 100/100", 100, 100, 0, 0),
            ("BLE 30/100, Classic", 30, 100, 12, 4),
        ];
        assert_eq!(TRIAL, want);
    }

    #[test]
    fn the_pass_mark_is_met_at_equality_and_not_below() {
        let row = |percent| Row {
            label: "radio alone",
            origin: "leaf".into(),
            delivered: 0,
            expected: 0,
            percent,
            heap: None,
        };
        assert!(passed(&[row(95.0)], 95.0));
        assert!(!passed(&[row(94.99)], 95.0));
        assert!(!passed(&[], 95.0));
    }

    #[test]
    fn a_row_prints_as_the_python_does() {
        let row = Row {
            label: "BLE 30/100",
            origin: "541e99a9".into(),
            delivered: 405,
            expected: 450,
            percent: 90.0,
            heap: None,
        };
        assert_eq!(
            row_line(&row),
            format!(
                "BLE 30/100{}node 541e99a9    405/450    90.00%  heap None",
                " ".repeat(13)
            )
        );
    }

    #[test]
    fn a_probe_payload_carries_its_sequence_little_endian() {
        assert_eq!(probe_seq(&[1, 2, 0, 0, 9]), Some(0x0201));
        assert_eq!(probe_seq(&[1, 2, 3]), None);
    }
}
