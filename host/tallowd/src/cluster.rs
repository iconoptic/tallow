//! Addresses that stayed one radio, and the name seeded from a MAC.
//!
//! A rotating address joins only as one chain: a single successor and a single
//! predecessor, a short handoff, and two hearings a side at or above -85 dBm
//! whose levels agree. A handoff with two candidates joins neither. A chain that
//! overlaps itself for more than two minutes is split, and a stored group covers
//! at least half an hour. The join is stored in `clusters` beside the sightings.
//! `devices.toml` is never written.
//!
//! The name is two words from the MAC bytes. The same address always yields the
//! same name. A new cluster takes the name of its earliest address. One that
//! already has a seed keeps that seed, even after the seed address ages out.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use rusqlite::{params, Connection, OpenFlags};

/// How long two addresses may both be heard at a handoff.
const OVERLAP_MS: i64 = 20_000;
/// The next address has to start within this long of the previous one ending.
const GAP_MS: i64 = 2 * 60 * 1000;
/// Levels at the handoff have to agree by this much, on every node that heard both.
const MATCH_DB: i16 = 6;
/// One minute.
const MINUTE_MS: i64 = 60_000;
/// Readings compared on each side of a handoff: the earlier address's last window
/// and the later address's first window.
const HANDOFF_WINDOW_MS: i64 = 2 * MINUTE_MS;
/// Hearings a node needs on each side of the handoff before that side counts.
const MIN_HEARINGS: usize = 2;
/// A handoff whose median is quieter than this is not a join.
const FLOOR_RSSI: i8 = -85;
/// A joined trail has to cover at least this long.
const SPAN_MS: i64 = 30 * 60 * 1000;
/// Heard together for longer than this, the newer address leaves the cluster.
const SPLIT_MS: i64 = 2 * 60 * 1000;
/// A node this loud, that never heard the other address, blocks the join.
const LOUD_RSSI: i8 = -75;
const RANDOM: &str = "ble-random";

const TABLE: &str = "
CREATE TABLE IF NOT EXISTS clusters (
    addr TEXT NOT NULL,
    kind TEXT NOT NULL,
    seed TEXT NOT NULL,
    PRIMARY KEY (addr, kind)
);
";

const ADJECTIVES: &[&str] = &[
    "Amber", "Bright", "Calm", "Copper", "Cozy", "Dapper", "Dusky", "Fern", "Gentle", "Golden",
    "Happy", "Hazel", "Honey", "Jolly", "Kind", "Lavender", "Little", "Lucky", "Merry", "Misty",
    "Noble", "Olive", "Peppy", "Plum", "Quiet", "Rusty", "Sandy", "Silver", "Soft", "Sunny",
    "Tidy", "Warm",
];

const NOUNS: &[&str] = &[
    "Badger", "Beetle", "Birch", "Bunny", "Cricket", "Dove", "Finch", "Firefly", "Fox", "Frog",
    "Goose", "Hare", "Heron", "Lark", "Maple", "Minnow", "Mole", "Moth", "Mouse", "Newt", "Otter",
    "Owl", "Pebble", "Pipit", "Rabbit", "Robin", "Sparrow", "Squirrel", "Starling", "Thrush",
    "Toad", "Wren",
];

/// The addresses joined so far. A missing table is an empty set.
#[derive(Debug, Default)]
pub struct Clusters {
    /// `"ADDR kind"` to the index of its group.
    by_addr: BTreeMap<String, usize>,
    groups: Vec<Group>,
}

/// One joined device: the earliest address is the seed the name is taken from.
#[derive(Debug)]
pub struct Group {
    pub seed: String,
    pub kind: String,
    pub members: Vec<String>,
}

struct Hearing {
    ts: i64,
    node: String,
    rssi: i8,
}

struct Span {
    addr: String,
    first: i64,
    last: i64,
    hearings: Vec<Hearing>,
}

impl Clusters {
    /// The group this address belongs to, when it was joined with another.
    pub fn group(&self, addr: &str, kind: &str) -> Option<&Group> {
        let index = *self.by_addr.get(&format!("{addr} {kind}"))?;
        self.groups.get(index)
    }

    /// Every joined group.
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }
}

/// Create the table if this database does not have it yet.
pub fn ensure(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(TABLE)
        .context("create the clusters table")?;
    Ok(())
}

/// Replace `clusters` with what the sightings currently support.
pub fn reconcile(conn: &mut Connection) -> anyhow::Result<()> {
    ensure(conn)?;
    let previous = previous_seeds(conn)?;
    let spans = load_spans(conn)?;
    let stored = name_groups(&spans, &join(&spans), &previous);
    let tx = conn.transaction().context("begin the cluster write")?;
    tx.execute("DELETE FROM clusters", [])
        .context("clear clusters")?;
    for (addr, seed) in stored {
        tx.execute(
            "INSERT INTO clusters (addr, kind, seed) VALUES (?1, ?2, ?3)",
            params![addr, RANDOM, seed],
        )
        .context("store a cluster")?;
    }
    tx.commit().context("commit clusters")?;
    Ok(())
}

/// The joins in this database. A database with no table has none.
pub fn load(conn: &Connection) -> anyhow::Result<Clusters> {
    let mut stmt = match conn.prepare("SELECT addr, kind, seed FROM clusters ORDER BY seed, addr") {
        Ok(stmt) => stmt,
        Err(err) if missing_table(&err) => return Ok(Clusters::default()),
        Err(err) => return Err(err).context("read clusters"),
    };
    let mut rows = stmt.query([])?;
    let mut grouped: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let addr: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let seed: String = row.get(2)?;
        grouped.entry((seed, kind)).or_default().push(addr);
    }
    let mut clusters = Clusters::default();
    for ((seed, kind), members) in grouped {
        if members.len() < 2 {
            continue;
        }
        let index = clusters.groups.len();
        for addr in &members {
            clusters.by_addr.insert(format!("{addr} {kind}"), index);
        }
        clusters.groups.push(Group {
            seed,
            kind,
            members,
        });
    }
    Ok(clusters)
}

/// The joins in the file beside the map. A missing file has none.
pub fn load_path(path: &Path) -> Clusters {
    if !path.is_file() {
        return Clusters::default();
    }
    let Ok(conn) = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return Clusters::default();
    };
    let _ = conn.busy_timeout(Duration::from_secs(1));
    load(&conn).unwrap_or_default()
}

/// Two words determined by the MAC. An address that is not a MAC is returned as written.
pub fn cute_name(addr: &str) -> String {
    let Some(mac) = parse_mac(addr) else {
        return addr.to_string();
    };
    let hash = mix(mac);
    let adjective = ADJECTIVES[hash as usize % ADJECTIVES.len()];
    let noun = NOUNS[(hash as usize / ADJECTIVES.len()) % NOUNS.len()];
    format!("{adjective} {noun}")
}

fn mix(mac: [u8; 6]) -> u32 {
    let mut hash = 0x811c_9dc5u32;
    for byte in mac {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let mut out = [0u8; 6];
    let mut parts = text.split(':');
    for slot in &mut out {
        let part = parts.next()?;
        if part.len() != 2 {
            return None;
        }
        *slot = u8::from_str_radix(part, 16).ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// Recompute clusters on `path` until the process ends. The thread opens its own
/// connection. A missing file or a locked database is logged and retried.
pub fn spawn_reconciler(
    path: PathBuf,
    first: Duration,
    every: Duration,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        std::thread::sleep(first);
        loop {
            if let Err(err) = reconcile_file(&path) {
                eprintln!("traffic: clusters: {err:#}");
            }
            std::thread::sleep(every);
        }
    })
}

fn reconcile_file(path: &Path) -> anyhow::Result<()> {
    let mut conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    ensure(&conn)?;
    reconcile(&mut conn)
}

fn missing_table(err: &rusqlite::Error) -> bool {
    matches!(err, rusqlite::Error::SqliteFailure(_, Some(text)) if text.contains("no such table"))
}

fn previous_seeds(conn: &Connection) -> anyhow::Result<BTreeMap<String, String>> {
    let mut stmt = conn
        .prepare("SELECT addr, seed FROM clusters")
        .context("read the current clusters")?;
    let mut rows = stmt.query([])?;
    let mut seeds = BTreeMap::new();
    while let Some(row) = rows.next()? {
        seeds.insert(row.get(0)?, row.get(1)?);
    }
    Ok(seeds)
}

fn load_spans(conn: &Connection) -> anyhow::Result<Vec<Span>> {
    let mut stmt = conn
        .prepare("SELECT ts, addr, node, rssi FROM sightings WHERE kind = ?1 ORDER BY addr, ts")
        .context("read sightings for clusters")?;
    let mut rows = stmt.query(params![RANDOM])?;
    let mut by_addr: BTreeMap<String, Span> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let ts: i64 = row.get(0)?;
        let addr: String = row.get(1)?;
        let node: String = row.get(2)?;
        let rssi = i8::try_from(row.get::<_, i64>(3)?.clamp(-127, 20)).unwrap_or(0);
        let span = by_addr.entry(addr.clone()).or_insert_with(|| Span {
            addr,
            first: ts,
            last: ts,
            hearings: Vec::new(),
        });
        span.first = span.first.min(ts);
        span.last = span.last.max(ts);
        span.hearings.push(Hearing { ts, node, rssi });
    }
    let mut spans: Vec<Span> = by_addr.into_values().collect();
    for span in &mut spans {
        span.hearings.sort_by_key(|hearing| hearing.ts);
    }
    Ok(spans)
}

fn join(spans: &[Span]) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by(|&left, &right| {
        spans[left]
            .first
            .cmp(&spans[right].first)
            .then(spans[left].addr.cmp(&spans[right].addr))
    });
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); spans.len()];
    let mut predecessors: Vec<Vec<usize>> = vec![Vec::new(); spans.len()];
    for &earlier in &order {
        let low = spans[earlier].last.saturating_sub(OVERLAP_MS);
        let high = spans[earlier].last.saturating_add(GAP_MS);
        let start = order.partition_point(|&index| spans[index].first < low);
        let end = order.partition_point(|&index| spans[index].first <= high);
        for &later in &order[start..end] {
            if earlier != later && handoff_edge(&spans[earlier], &spans[later]) {
                successors[earlier].push(later);
                predecessors[later].push(earlier);
            }
        }
    }
    let mut next = vec![None; spans.len()];
    for earlier in 0..spans.len() {
        if successors[earlier].len() != 1 {
            continue;
        }
        let later = successors[earlier][0];
        if predecessors[later].len() == 1 {
            next[earlier] = Some(later);
        }
    }
    let mut has_predecessor = vec![false; spans.len()];
    for later in next.iter().flatten() {
        has_predecessor[*later] = true;
    }
    let mut groups = Vec::new();
    for start in 0..spans.len() {
        if has_predecessor[start] || next[start].is_none() {
            continue;
        }
        let mut chain = Vec::new();
        let mut cursor = Some(start);
        while let Some(index) = cursor {
            chain.push(index);
            cursor = next[index];
        }
        let mut group: Vec<usize> = Vec::new();
        for index in chain {
            let overlaps = group
                .iter()
                .any(|&kept| envelope_overlap(&spans[kept], &spans[index]) > SPLIT_MS);
            if overlaps {
                keep_group(&mut groups, spans, &group);
                group.clear();
            }
            group.push(index);
        }
        keep_group(&mut groups, spans, &group);
    }
    groups
}

fn keep_group(groups: &mut Vec<Vec<usize>>, spans: &[Span], group: &[usize]) {
    if group.len() < 2 {
        return;
    }
    let first = group
        .iter()
        .map(|&index| spans[index].first)
        .min()
        .unwrap_or(0);
    let last = group
        .iter()
        .map(|&index| spans[index].last)
        .max()
        .unwrap_or(0);
    if last.saturating_sub(first) < SPAN_MS {
        return;
    }
    groups.push(group.to_vec());
}

struct Claim {
    members: Vec<String>,
    earliest: String,
    wanted: Option<String>,
    support: usize,
}

/// Keep an old seed when the members already had one. A seed is given to one group.
fn name_groups(
    spans: &[Span],
    groups: &[Vec<usize>],
    previous: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let mut claims = Vec::new();
    for group in groups {
        let members: Vec<String> = group
            .iter()
            .map(|&index| spans[index].addr.clone())
            .collect();
        let earliest = earliest_addr(spans, group);
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for addr in &members {
            if let Some(seed) = previous.get(addr) {
                *counts.entry(seed.clone()).or_default() += 1;
            }
        }
        let wanted = chosen_seed(&counts, &members);
        let support = wanted
            .as_ref()
            .and_then(|seed| counts.get(seed).copied())
            .unwrap_or(0);
        claims.push(Claim {
            members,
            earliest,
            wanted,
            support,
        });
    }
    let mut owner: BTreeMap<String, usize> = BTreeMap::new();
    for (index, claim) in claims.iter().enumerate() {
        let Some(seed) = &claim.wanted else {
            continue;
        };
        let replace = match owner.get(seed) {
            None => true,
            Some(&other) => {
                let held = &claims[other];
                claim.support > held.support
                    || (claim.support == held.support && claim.earliest < held.earliest)
            }
        };
        if replace {
            owner.insert(seed.clone(), index);
        }
    }
    let mut stored = Vec::new();
    for (index, claim) in claims.iter().enumerate() {
        let seed = match &claim.wanted {
            Some(seed) if owner.get(seed) == Some(&index) => seed.clone(),
            _ => claim.earliest.clone(),
        };
        for addr in &claim.members {
            stored.push((addr.clone(), seed.clone()));
        }
    }
    stored
}

fn earliest_addr(spans: &[Span], group: &[usize]) -> String {
    group
        .iter()
        .copied()
        .min_by(|&left, &right| {
            spans[left]
                .first
                .cmp(&spans[right].first)
                .then(spans[left].addr.cmp(&spans[right].addr))
        })
        .map(|index| spans[index].addr.clone())
        .unwrap_or_default()
}

/// The old seed carried by the most members. A tie prefers a seed that is still
/// a member, then the smallest address.
fn chosen_seed(counts: &BTreeMap<String, usize>, members: &[String]) -> Option<String> {
    let mut best: Option<(&str, usize)> = None;
    for (seed, count) in counts {
        let take = match best {
            None => true,
            Some((prev, prev_count)) => {
                if *count != prev_count {
                    *count > prev_count
                } else {
                    let seed_here = members.iter().any(|member| member == seed);
                    let prev_here = members.iter().any(|member| member == prev);
                    if seed_here != prev_here {
                        seed_here
                    } else {
                        seed.as_str() < prev
                    }
                }
            }
        };
        if take {
            best = Some((seed.as_str(), *count));
        }
    }
    best.map(|(seed, _)| seed.to_string())
}

fn envelope_overlap(left: &Span, right: &Span) -> i64 {
    let start = left.first.max(right.first);
    let end = left.last.min(right.last);
    if end < start {
        0
    } else {
        end.saturating_sub(start)
    }
}

/// `earlier` hands off to `later`: the later address starts as the earlier one ends.
fn handoff_edge(earlier: &Span, later: &Span) -> bool {
    if earlier.first >= later.first || earlier.last >= later.last {
        return false;
    }
    let gap = later.first.saturating_sub(earlier.last);
    if !(-OVERLAP_MS..=GAP_MS).contains(&gap) {
        return false;
    }
    let before = medians(
        earlier,
        earlier.last.saturating_sub(HANDOFF_WINDOW_MS),
        earlier.last,
    );
    let after = medians(
        later,
        later.first,
        later.first.saturating_add(HANDOFF_WINDOW_MS),
    );
    if exclusive_loud(&before, &after) || exclusive_loud(&after, &before) {
        return false;
    }
    let before_counts = hearing_counts(
        earlier,
        earlier.last.saturating_sub(HANDOFF_WINDOW_MS),
        earlier.last,
    );
    let after_counts = hearing_counts(
        later,
        later.first,
        later.first.saturating_add(HANDOFF_WINDOW_MS),
    );
    let mut qualified = false;
    for (node, left) in &before {
        let Some(right) = after.get(node) else {
            continue;
        };
        if (i16::from(*left) - i16::from(*right)).abs() > MATCH_DB {
            return false;
        }
        let left_n = before_counts.get(node).copied().unwrap_or(0);
        let right_n = after_counts.get(node).copied().unwrap_or(0);
        if left_n >= MIN_HEARINGS
            && right_n >= MIN_HEARINGS
            && *left >= FLOOR_RSSI
            && *right >= FLOOR_RSSI
        {
            qualified = true;
        }
    }
    qualified
}

fn hearing_counts(span: &Span, start: i64, end: i64) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for hearing in &span.hearings {
        if hearing.ts >= start && hearing.ts <= end {
            *counts.entry(hearing.node.clone()).or_default() += 1;
        }
    }
    counts
}

fn exclusive_loud(side: &BTreeMap<String, i8>, other: &BTreeMap<String, i8>) -> bool {
    side.iter()
        .any(|(node, rssi)| *rssi >= LOUD_RSSI && !other.contains_key(node))
}

fn medians(span: &Span, start: i64, end: i64) -> BTreeMap<String, i8> {
    let mut by_node: BTreeMap<&str, Vec<i8>> = BTreeMap::new();
    for hearing in &span.hearings {
        if hearing.ts >= start && hearing.ts <= end {
            by_node
                .entry(hearing.node.as_str())
                .or_default()
                .push(hearing.rssi);
        }
    }
    by_node
        .into_iter()
        .filter_map(|(node, values)| median(&values).map(|rssi| (node.to_string(), rssi)))
        .collect()
}

fn median(values: &[i8]) -> Option<i8> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        return Some(sorted[mid]);
    }
    let sum = i16::from(sorted[mid - 1]) + i16::from(sorted[mid]);
    i8::try_from(sum / 2).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLDER: &str = "11:22:33:44:55:01";
    const NEWER: &str = "11:22:33:44:55:02";

    fn memory() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sightings (
                ts INTEGER NOT NULL,
                addr TEXT NOT NULL,
                kind TEXT NOT NULL,
                node TEXT NOT NULL,
                rssi INTEGER NOT NULL,
                hits INTEGER NOT NULL
            );",
        )
        .unwrap();
        ensure(&conn).unwrap();
        conn
    }

    fn hear(conn: &Connection, ts: i64, addr: &str, node: &str, rssi: i8) {
        conn.execute(
            "INSERT INTO sightings (ts, addr, kind, node, rssi, hits) VALUES (?1, ?2, ?3, ?4, ?5, 1)",
            params![ts, addr, RANDOM, node, i64::from(rssi)],
        )
        .unwrap();
    }

    fn hear_kind(conn: &Connection, ts: i64, addr: &str, kind: &str, rssi: i8) {
        conn.execute(
            "INSERT INTO sightings (ts, addr, kind, node, rssi, hits) VALUES (?1, ?2, ?3, 'n1', ?4, 1)",
            params![ts, addr, kind, i64::from(rssi)],
        )
        .unwrap();
    }

    /// The trail the join rule is written around: one address, then the next, same level.
    /// Two hearings sit on each side of the handoff, 60 s from it.
    fn handoff(conn: &Connection, newer_rssi: i8) {
        hear(conn, 0, OLDER, "n1", -60);
        hear(conn, 15 * MINUTE_MS - 60_000, OLDER, "n1", -60);
        hear(conn, 15 * MINUTE_MS, OLDER, "n1", -60);
        hear(conn, 15 * MINUTE_MS + 10_000, NEWER, "n1", newer_rssi);
        hear(conn, 15 * MINUTE_MS + 70_000, NEWER, "n1", newer_rssi);
        hear(conn, 31 * MINUTE_MS, NEWER, "n1", newer_rssi);
    }

    fn joined(conn: &mut Connection) -> Option<String> {
        reconcile(conn).unwrap();
        let clusters = load(conn).unwrap();
        let older = clusters.group(OLDER, RANDOM)?;
        let newer = clusters.group(NEWER, RANDOM)?;
        (older.seed == newer.seed).then(|| older.seed.clone())
    }

    #[test]
    fn a_thirty_minute_handoff_at_the_same_level_is_one_device() {
        let mut conn = memory();
        handoff(&conn, -60);
        assert_eq!(joined(&mut conn).as_deref(), Some(OLDER));
    }

    #[test]
    fn a_six_db_handoff_still_joins() {
        let mut conn = memory();
        handoff(&conn, -66);
        assert_eq!(joined(&mut conn).as_deref(), Some(OLDER));
    }

    #[test]
    fn addresses_heard_together_do_not_join() {
        let mut conn = memory();
        for step in 0..=31 {
            let ts = step * MINUTE_MS;
            hear(&conn, ts, OLDER, "n1", -60);
            hear(&conn, ts, NEWER, "n1", -60);
        }
        assert_eq!(joined(&mut conn), None);
    }

    #[test]
    fn a_handoff_seven_db_apart_does_not_join() {
        let mut conn = memory();
        handoff(&conn, -67);
        assert_eq!(joined(&mut conn), None);
    }

    #[test]
    fn a_short_glimpse_does_not_join() {
        let mut conn = memory();
        hear(&conn, 0, OLDER, "n1", -60);
        hear(&conn, 10_000, NEWER, "n1", -60);
        hear(&conn, 60_000, NEWER, "n1", -60);
        assert_eq!(joined(&mut conn), None);
    }

    #[test]
    fn a_later_overlap_splits_and_does_not_rejoin() {
        let mut conn = memory();
        handoff(&conn, -60);
        assert_eq!(joined(&mut conn).as_deref(), Some(OLDER));
        let start = 40 * MINUTE_MS;
        for step in 0..=6 {
            let ts = start + step * 30_000;
            hear(&conn, ts, OLDER, "n1", -60);
            hear(&conn, ts, NEWER, "n1", -60);
        }
        assert_eq!(joined(&mut conn), None);
        assert_eq!(
            joined(&mut conn),
            None,
            "the same overlap does not rejoin them"
        );
    }

    #[test]
    fn a_node_that_heard_only_one_side_loudly_blocks_the_join() {
        let mut conn = memory();
        hear(&conn, 0, OLDER, "n1", -60);
        hear(&conn, 15 * MINUTE_MS, OLDER, "n1", -60);
        hear(&conn, 15 * MINUTE_MS, OLDER, "n2", -60);
        hear(&conn, 15 * MINUTE_MS + 10_000, NEWER, "n1", -60);
        hear(&conn, 31 * MINUTE_MS, NEWER, "n1", -60);
        assert_eq!(joined(&mut conn), None);
    }

    #[test]
    fn a_public_address_is_not_joined_by_this_rule() {
        let mut conn = memory();
        hear_kind(&conn, 0, OLDER, "ble-public", -60);
        hear_kind(&conn, 15 * MINUTE_MS, OLDER, "ble-public", -60);
        hear_kind(&conn, 15 * MINUTE_MS + 10_000, NEWER, "ble-public", -60);
        hear_kind(&conn, 31 * MINUTE_MS, NEWER, "ble-public", -60);
        assert_eq!(joined(&mut conn), None);
    }

    #[test]
    fn the_same_mac_always_yields_the_same_name() {
        let name = cute_name("AA:BB:CC:DD:EE:02");
        assert_eq!(name, cute_name("aa:bb:cc:dd:ee:02"));
        assert_ne!(name, cute_name("AA:BB:CC:DD:EE:03"));
        assert_ne!(name, "AA:BB:CC:DD:EE:02");
        let (adjective, noun) = name.split_once(' ').unwrap();
        assert!(ADJECTIVES.contains(&adjective));
        assert!(NOUNS.contains(&noun));
    }

    #[test]
    fn two_devices_that_rotate_together_join_neither() {
        let mut conn = memory();
        let left = ["11:22:33:44:55:11", "11:22:33:44:55:12"];
        let right = ["11:22:33:44:55:21", "11:22:33:44:55:22"];
        let handoff_at = 40 * MINUTE_MS;
        for step in 0..=40 {
            let ts = step * MINUTE_MS;
            hear(&conn, ts, left[0], "n1", -60);
            hear(&conn, ts, right[0], "n1", -62);
        }
        for step in 0..=40 {
            let ts = handoff_at + 10_000 + step * MINUTE_MS;
            hear(&conn, ts, left[1], "n1", -60);
            hear(&conn, ts, right[1], "n1", -62);
        }
        reconcile(&mut conn).unwrap();
        assert!(load(&conn).unwrap().groups().is_empty());
    }

    #[test]
    fn an_address_with_two_possible_successors_joins_neither() {
        let mut conn = memory();
        let ended = OLDER;
        let one = NEWER;
        let other = "11:22:33:44:55:03";
        for step in 0..=20 {
            hear(&conn, step * MINUTE_MS, ended, "n1", -60);
        }
        for (addr, start) in [
            (one, 20 * MINUTE_MS + 10_000),
            (other, 20 * MINUTE_MS + 30_000),
        ] {
            hear(&conn, start, addr, "n1", -60);
            hear(&conn, start + 60_000, addr, "n1", -60);
            hear(&conn, start + 20 * MINUTE_MS, addr, "n1", -60);
        }
        reconcile(&mut conn).unwrap();
        let clusters = load(&conn).unwrap();
        assert!(clusters.group(ended, RANDOM).is_none());
    }

    #[test]
    fn a_handoff_below_the_signal_floor_does_not_join() {
        let mut conn = memory();
        for (ts, addr) in [
            (0, OLDER),
            (15 * MINUTE_MS - 60_000, OLDER),
            (15 * MINUTE_MS, OLDER),
            (15 * MINUTE_MS + 10_000, NEWER),
            (15 * MINUTE_MS + 70_000, NEWER),
            (31 * MINUTE_MS, NEWER),
        ] {
            hear(&conn, ts, addr, "n1", -92);
        }
        assert_eq!(joined(&mut conn), None);
    }

    #[test]
    fn a_handoff_with_one_hearing_a_side_does_not_join() {
        let mut conn = memory();
        hear(&conn, 0, OLDER, "n1", -60);
        hear(&conn, 15 * MINUTE_MS, OLDER, "n1", -60);
        hear(&conn, 15 * MINUTE_MS + 10_000, NEWER, "n1", -60);
        hear(&conn, 31 * MINUTE_MS, NEWER, "n1", -60);
        assert_eq!(joined(&mut conn), None);
    }

    #[test]
    fn a_chain_of_three_rotations_is_one_device() {
        let mut conn = memory();
        let addrs = [
            "11:22:33:44:55:01",
            "11:22:33:44:55:02",
            "11:22:33:44:55:03",
        ];
        let span = 12 * MINUTE_MS;
        let gap = 10_000i64;
        for (index, addr) in addrs.iter().enumerate() {
            let start = index as i64 * (span + gap);
            let end = start + span;
            hear(&conn, start, addr, "n1", -60);
            hear(&conn, start + 60_000, addr, "n1", -60);
            hear(&conn, end - 60_000, addr, "n1", -60);
            hear(&conn, end, addr, "n1", -60);
        }
        reconcile(&mut conn).unwrap();
        let clusters = load(&conn).unwrap();
        assert_eq!(clusters.groups().len(), 1);
        let group = &clusters.groups()[0];
        assert_eq!(group.members.len(), 3);
        assert_eq!(group.seed, addrs[0]);
    }

    #[test]
    fn a_crowd_of_rotating_devices_never_groups_concurrent_addresses() {
        let mut conn = memory();
        let mut envelopes: BTreeMap<String, (i64, i64)> = BTreeMap::new();
        let rotate = 15 * MINUTE_MS;
        let hours = 3 * 60 * MINUTE_MS;
        for device in 0..12 {
            let rssi = i8::try_from(-70 - device).expect("rssi fits");
            let offset = i64::from(device) * 75_000;
            let mut ts = offset;
            while ts <= offset + hours {
                let slot = (ts - offset) / rotate;
                let addr = format!("10:00:00:{device:02x}:{slot:02x}:00");
                hear(&conn, ts, &addr, "n1", rssi);
                let entry = envelopes.entry(addr).or_insert((ts, ts));
                entry.0 = entry.0.min(ts);
                entry.1 = entry.1.max(ts);
                ts += 30_000;
            }
        }
        reconcile(&mut conn).unwrap();
        for group in load(&conn).unwrap().groups() {
            assert!(
                group.members.len() <= 13,
                "group {} has {} members",
                group.seed,
                group.members.len()
            );
            for left in 0..group.members.len() {
                for right in (left + 1)..group.members.len() {
                    let (left_first, left_last) = envelopes[&group.members[left]];
                    let (right_first, right_last) = envelopes[&group.members[right]];
                    let start = left_first.max(right_first);
                    let end = left_last.min(right_last);
                    let overlap = if end < start { 0 } else { end - start };
                    assert!(
                        overlap <= SPLIT_MS,
                        "{} and {} overlap by {overlap}",
                        group.members[left],
                        group.members[right]
                    );
                }
            }
        }
    }

    fn lay(conn: &Connection, addr: &str, start: i64, end: i64) {
        hear(conn, start, addr, "n1", -60);
        hear(conn, start + 60_000, addr, "n1", -60);
        hear(conn, end - 60_000, addr, "n1", -60);
        hear(conn, end, addr, "n1", -60);
    }

    #[test]
    fn a_seed_that_ages_out_still_names_the_device() {
        let mut conn = memory();
        let addrs = [OLDER, NEWER, "11:22:33:44:55:03"];
        let span = 16 * MINUTE_MS;
        let gap = 10_000i64;
        for (index, addr) in addrs.iter().enumerate() {
            let start = index as i64 * (span + gap);
            lay(&conn, addr, start, start + span);
        }
        reconcile(&mut conn).unwrap();
        let name = cute_name(addrs[0]);
        conn.execute("DELETE FROM sightings WHERE addr = ?1", params![addrs[0]])
            .unwrap();
        reconcile(&mut conn).unwrap();
        let clusters = load(&conn).unwrap();
        assert_eq!(clusters.groups().len(), 1);
        let group = &clusters.groups()[0];
        assert_eq!(group.seed, addrs[0]);
        assert_eq!(group.members.len(), 2);
        assert!(group.members.iter().any(|member| member == addrs[1]));
        assert!(group.members.iter().any(|member| member == addrs[2]));
        assert!(!group.members.iter().any(|member| member == addrs[0]));
        assert_eq!(cute_name(&group.seed), name);
    }

    #[test]
    fn an_old_seed_is_given_to_one_group_only() {
        let mut conn = memory();
        let seed = "11:22:33:44:55:99";
        let left = ["11:22:33:44:55:11", "11:22:33:44:55:12"];
        let right = ["11:22:33:44:55:21", "11:22:33:44:55:22"];
        for addr in left.iter().chain(right.iter()) {
            conn.execute(
                "INSERT INTO clusters (addr, kind, seed) VALUES (?1, ?2, ?3)",
                params![addr, RANDOM, seed],
            )
            .unwrap();
        }
        let handoff_at = 20 * MINUTE_MS;
        for step in 0..=20 {
            let ts = step * MINUTE_MS;
            hear(&conn, ts, left[0], "n1", -60);
            hear(&conn, ts, right[0], "n1", -40);
        }
        for step in 0..=20 {
            let ts = handoff_at + 10_000 + step * MINUTE_MS;
            hear(&conn, ts, left[1], "n1", -60);
            hear(&conn, ts, right[1], "n1", -40);
        }
        reconcile(&mut conn).unwrap();
        let groups = load(&conn).unwrap();
        assert_eq!(groups.groups().len(), 2);
        assert_ne!(groups.groups()[0].seed, groups.groups()[1].seed);
    }

    #[test]
    fn a_three_day_crowd_reconciles_in_seconds() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("tallow-crowd-{}-{nanos}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("traffic.db");
        let mut conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sightings (
                ts INTEGER NOT NULL,
                addr TEXT NOT NULL,
                kind TEXT NOT NULL,
                node TEXT NOT NULL,
                rssi INTEGER NOT NULL,
                hits INTEGER NOT NULL
            );",
        )
        .unwrap();
        ensure(&conn).unwrap();
        let rotate = 15 * MINUTE_MS;
        let span = 3 * 24 * 60 * MINUTE_MS;
        let tx = conn.transaction().unwrap();
        {
            let mut insert = tx
                .prepare(
                    "INSERT INTO sightings (ts, addr, kind, node, rssi, hits) VALUES (?1, ?2, ?3, ?4, ?5, 1)",
                )
                .unwrap();
            for device in 0..20 {
                let mut ts = 0i64;
                let mut minute = 0i64;
                while ts < span {
                    let slot = ts / rotate;
                    let addr = format!("d{device:02}-s{slot:04}");
                    insert
                        .execute(params![ts, addr, RANDOM, "n1", -70i64])
                        .unwrap();
                    if minute % 3 == 0 {
                        insert
                            .execute(params![ts, addr, RANDOM, "n2", -72i64])
                            .unwrap();
                    }
                    ts += MINUTE_MS;
                    minute += 1;
                }
            }
        }
        tx.commit().unwrap();
        let started = std::time::Instant::now();
        reconcile(&mut conn).unwrap();
        let elapsed = started.elapsed();
        eprintln!("three-day reconcile: {elapsed:?}");
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "reconcile took {elapsed:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reconciler_on_a_missing_file_logs_and_keeps_going() {
        let dir = std::env::temp_dir().join(format!("tallow-reconciler-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("traffic.db");
        let handle = spawn_reconciler(
            path.clone(),
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(10),
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        let _journal = crate::traffic::Journal::open(&path).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!handle.is_finished());
        let conn = Connection::open(&path).unwrap();
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'clusters'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
        drop(handle);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_database_without_the_table_has_no_clusters() {
        let conn = Connection::open_in_memory().unwrap();
        let clusters = load(&conn).unwrap();
        assert!(clusters.groups().is_empty());
    }
}
