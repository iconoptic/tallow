//! A private log of what the mesh already reports, and the picture the traffic page draws.
//!
//! `tallowd run` appends one row per Report, and the latest name from each Name frame.
//! The page reads that file and decides, at read time, which stays are a linger, a move,
//! or a pass-by. Nothing here is sent off the host. The file is mode 0600.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use tallow_mesh_core::sightings::Kind;
use tallow_mesh_core::wire::FrameKind;

use crate::decode::{self, Named, Sighting};
use crate::identity::Identities;
use crate::irk::Keys;
use crate::link::parse_line;
use crate::local::Batch;

const REPEAT_MS: i64 = 5_000;
const REPEAT_DB: i16 = 3;
/// A reading older than this cannot hold a place.
const FRESH_MS: i64 = 60_000;
/// A gap shorter than this is still the same stay.
const GAP_MS: i64 = 45_000;
/// One short stay, then silence, is a pass-by.
const PASS_MS: i64 = 20_000;
/// A new node has to be this much stronger before the place changes.
const LEAD_DB: i16 = 3;
const RETAIN_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const PRUNE_EVERY_MS: i64 = 60_000;
const SAMPLE_CAP: i64 = 100_000;
const LIVE_MS: i64 = 15 * 60 * 1000;
const HOUR_MS: i64 = 60 * 60 * 1000;
const DAY_MS: i64 = 24 * HOUR_MS;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sightings (
    ts INTEGER NOT NULL,
    addr TEXT NOT NULL,
    kind TEXT NOT NULL,
    node TEXT NOT NULL,
    rssi INTEGER NOT NULL,
    hits INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sightings_when ON sightings(ts);
CREATE INDEX IF NOT EXISTS sightings_who ON sightings(addr, kind, ts);
CREATE TABLE IF NOT EXISTS names (
    addr TEXT NOT NULL,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    PRIMARY KEY (addr, kind)
);
";

/// The sighting log beside the map. A failure here never stops the map writer.
pub struct Log {
    journal: Option<Journal>,
    failures: u32,
}

impl Log {
    /// A log that records nothing. Used for `--trial`.
    pub fn off() -> Self {
        Self {
            journal: None,
            failures: 0,
        }
    }

    /// Whether `open_log` got a database.
    pub fn is_open(&self) -> bool {
        self.journal.is_some()
    }

    /// Record Report and Name frames. Errors are counted and then dropped.
    pub fn note_lines(&mut self, lines: &[String], now: f64) {
        self.record(|journal| journal.note_lines(lines, now));
    }

    /// Record one adapter poll. Errors are counted and then dropped.
    pub fn note_batch(&mut self, batch: &Batch, now: f64) {
        self.record(|journal| journal.note_batch(batch, now));
    }

    fn record(&mut self, write: impl FnOnce(&mut Journal) -> anyhow::Result<()>) {
        let Some(journal) = self.journal.as_mut() else {
            return;
        };
        match write(journal) {
            Ok(()) => self.failures = 0,
            Err(err) => {
                self.failures = self.failures.saturating_add(1);
                if self.failures == 1 || self.failures.is_multiple_of(100) {
                    eprintln!("traffic: {err:#}");
                }
                if self.failures >= 10 {
                    self.journal = None;
                    eprintln!("traffic: log stopped; the map continues");
                }
            }
        }
    }
}

/// Open the log and seed it from the map. On any error the log is off and the
/// caller keeps running.
pub fn open_log(db: &Path, map: &Path) -> Log {
    match Journal::open(db).and_then(|mut journal| {
        let copied = journal.seed_map(map)?;
        if copied > 0 {
            println!("traffic: seeded {copied} sightings from the current map");
        }
        Ok(journal)
    }) {
        Ok(journal) => Log {
            journal: Some(journal),
            failures: 0,
        },
        Err(err) => {
            eprintln!("traffic: off: {err:#}");
            Log::off()
        }
    }
}

/// The append-only log beside the map. The `tallowd run` child is the writer.
pub struct Journal {
    path: PathBuf,
    conn: Connection,
    last: HashMap<(String, String, String), (i64, i8)>,
    next_prune: i64,
}

#[derive(Clone)]
struct Row {
    ts: i64,
    addr: String,
    kind: String,
    node: String,
    rssi: i8,
    hits: i64,
}

struct NameRow {
    addr: String,
    kind: String,
    name: String,
}

struct Obs {
    ts: i64,
    node: String,
    rssi: i8,
}

struct Stay {
    node: String,
    start: i64,
    end: i64,
    rssi: i8,
}

/// `live` is the last 15 minutes, `hour` one hour, and `day` one day, ending at `now_ms`.
pub fn window(now_ms: i64, name: &str) -> Option<(i64, i64)> {
    let span = match name {
        "live" => LIVE_MS,
        "hour" => HOUR_MS,
        "day" => DAY_MS,
        _ => return None,
    };
    Some((now_ms.saturating_sub(span), now_ms))
}

impl Journal {
    /// Open or create `path`, at mode 0600, and make sure the tables exist.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
        }
        if !path.exists() {
            OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)
                .with_context(|| format!("create {}", path.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(1))?;
        secure_delete(&conn)?;
        conn.execute_batch(SCHEMA)?;
        crate::cluster::ensure(&conn)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        privatize(path);
        Ok(Self {
            path: path.to_path_buf(),
            conn,
            last: HashMap::new(),
            next_prune: 0,
        })
    }

    /// When the log is empty, copy the current map so the page has somewhere to start.
    /// A missing or unreadable map copies nothing. Returns how many sightings were copied.
    pub fn seed_map(&mut self, map_path: &Path) -> anyhow::Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sightings", [], |row| row.get(0))?;
        if count > 0 {
            return Ok(0);
        }
        let Some(text) = fs::read_to_string(map_path).ok() else {
            return Ok(0);
        };
        let Some((rows, names)) = rows_from_map(&text) else {
            return Ok(0);
        };
        let copied = rows.len();
        if copied == 0 && names.is_empty() {
            return Ok(0);
        }
        self.insert(&rows, &names, 0.0)?;
        Ok(copied)
    }

    /// Record the Report and Name frames in `lines`. `now` is unix seconds, as `unix_now` gives.
    pub fn note_lines(&mut self, lines: &[String], now: f64) -> anyhow::Result<()> {
        let mut rows = Vec::new();
        let mut names = Vec::new();
        for text in lines {
            let Some(line) = parse_line(text) else {
                continue;
            };
            let Ok(code) = u8::try_from(line.frame) else {
                continue;
            };
            match FrameKind::from_u8(code) {
                Some(FrameKind::Report) => {
                    for sighting in decode::sightings(&line.payload) {
                        rows.push(row_from(&line.origin, &sighting, now));
                    }
                }
                Some(FrameKind::Name) => {
                    for named in decode::names(&line.payload) {
                        if let Some(name) = name_row(&named) {
                            names.push(name);
                        }
                    }
                }
                _ => {}
            }
        }
        if rows.is_empty() && names.is_empty() {
            return self.maybe_prune(now);
        }
        self.insert(&rows, &names, now)
    }

    /// Record one poll from the host's own adapter.
    pub fn note_batch(&mut self, batch: &Batch, now: f64) -> anyhow::Result<()> {
        let rows: Vec<Row> = batch
            .sightings
            .iter()
            .map(|sighting| row_from(&batch.node, sighting, now))
            .collect();
        let names: Vec<NameRow> = batch.names.iter().filter_map(name_row).collect();
        if rows.is_empty() && names.is_empty() {
            return self.maybe_prune(now);
        }
        self.insert(&rows, &names, now)
    }

    fn insert(&mut self, rows: &[Row], names: &[NameRow], now: f64) -> anyhow::Result<()> {
        let mut kept = Vec::new();
        for row in rows {
            if accept(&mut self.last, row) {
                kept.push(row);
            }
        }
        let tx = self.conn.transaction()?;
        for row in kept {
            tx.execute(
                "INSERT INTO sightings (ts, addr, kind, node, rssi, hits) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![row.ts, row.addr, row.kind, row.node, i64::from(row.rssi), row.hits],
            )?;
        }
        for name in names {
            tx.execute(
                "INSERT INTO names (addr, kind, name) VALUES (?1, ?2, ?3)
                 ON CONFLICT(addr, kind) DO UPDATE SET name = excluded.name",
                params![name.addr, name.kind, name.name],
            )?;
        }
        tx.commit()?;
        self.maybe_prune(now)?;
        privatize(&self.path);
        Ok(())
    }

    fn maybe_prune(&mut self, now: f64) -> anyhow::Result<()> {
        let now_ms = ms(now);
        if now_ms < self.next_prune {
            return Ok(());
        }
        self.next_prune = now_ms.saturating_add(PRUNE_EVERY_MS);
        let cutoff = now_ms.saturating_sub(RETAIN_MS);
        self.conn
            .execute("DELETE FROM sightings WHERE ts < ?1", params![cutoff])?;
        self.conn.execute(
            "DELETE FROM names WHERE NOT EXISTS (
                SELECT 1 FROM sightings s WHERE s.addr = names.addr AND s.kind = names.kind
            )",
            [],
        )?;
        let fresh = now_ms.saturating_sub(REPEAT_MS);
        self.last.retain(|_, (ts, _)| *ts >= fresh);
        privatize(&self.path);
        Ok(())
    }

    /// How many addresses are still in the repeat cache.
    #[cfg(test)]
    fn cached(&self) -> usize {
        self.last.len()
    }

    /// Join addresses the sightings have followed for long enough.
    #[cfg(test)]
    pub fn reconcile_clusters(&mut self) -> anyhow::Result<()> {
        crate::cluster::reconcile(&mut self.conn)
    }

    #[cfg(test)]
    fn stored(&self) -> anyhow::Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM sightings", [], |row| row.get(0))?)
    }
}

/// Delete every row. A missing file is already empty.
pub fn clear(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(1))?;
    secure_delete(&conn)?;
    conn.execute_batch(SCHEMA)?;
    crate::cluster::ensure(&conn)?;
    conn.execute_batch("DELETE FROM sightings; DELETE FROM names; DELETE FROM clusters;")?;
    checkpoint_truncate(&conn)?;
    privatize(path);
    Ok(())
}

fn secure_delete(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch("PRAGMA secure_delete=ON;")
        .context("turn on secure delete")
}

/// A busy checkpoint is left for the next writer. Any other error is returned.
fn checkpoint_truncate(conn: &Connection) -> anyhow::Result<()> {
    match conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
        Ok(()) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(code, _))
            if code.code == rusqlite::ErrorCode::DatabaseBusy
                || code.code == rusqlite::ErrorCode::DatabaseLocked =>
        {
            Ok(())
        }
        Err(err) => Err(err).context("checkpoint the traffic log"),
    }
}

/// The document `GET /api/traffic` answers with. A missing log is an empty picture.
pub fn picture(db: Option<&Path>, from_ms: i64, to_ms: i64) -> anyhow::Result<Value> {
    let layout = db.map(|path| read_layout(&path.with_file_name("layout.json")));
    let rooms = db.map(|path| read_rooms(&path.with_file_name("rooms.toml")));
    let links = db
        .map(|path| read_links(&path_of_map(path)))
        .unwrap_or_default();
    let keys = match db {
        Some(path) => Keys::load(&path.with_file_name("irks.toml")).unwrap_or_else(|err| {
            eprintln!("traffic: {err:#}");
            Keys::default()
        }),
        None => Keys::default(),
    };
    let identities = match db {
        Some(path) => {
            Identities::load(&path.with_file_name("devices.toml")).unwrap_or_else(|err| {
                eprintln!("traffic: {err:#}");
                Identities::default()
            })
        }
        None => Identities::default(),
    };
    let (samples, truncated) = match db {
        Some(path) if path.exists() => load_samples(path, from_ms, to_ms, &keys, &identities)?,
        _ => (Vec::new(), false),
    };
    let mut grouped: BTreeMap<String, Group> = BTreeMap::new();
    for sample in samples {
        let group = grouped.entry(sample.key.clone()).or_insert_with(|| Group {
            label: sample.label.clone(),
            named: sample.named,
            samples: Vec::new(),
        });
        if sample.named && !group.named {
            group.label = sample.label;
            group.named = true;
        }
        group.samples.push(Obs {
            ts: sample.ts,
            node: sample.node,
            rssi: sample.rssi,
        });
    }
    let mut tracks = Vec::new();
    let mut edges: BTreeMap<(String, String), u64> = BTreeMap::new();
    let mut node_ids: BTreeMap<String, ()> = BTreeMap::new();
    for (id, mut group) in grouped {
        group.samples.sort_by_key(|sample| sample.ts);
        let (class, stays) = segment(&group.samples, to_ms);
        for pair in stays.windows(2) {
            if pair[0].node != pair[1].node {
                *edges
                    .entry((pair[0].node.clone(), pair[1].node.clone()))
                    .or_default() += 1;
            }
        }
        for stay in &stays {
            node_ids.insert(stay.node.clone(), ());
        }
        tracks.push(json!({
            "id": id,
            "label": group.label,
            "class": class,
            "named": group.named,
            "stays": stays.iter().map(|stay| json!({
                "node": stay.node,
                "start": stay.start,
                "end": stay.end,
                "rssi": stay.rssi,
            })).collect::<Vec<_>>(),
            "samples": group.samples.iter().map(|sample| json!({
                "t": sample.ts,
                "node": sample.node,
                "rssi": sample.rssi,
            })).collect::<Vec<_>>(),
        }));
    }
    tracks.sort_by(|left, right| {
        let left_end = left["stays"]
            .as_array()
            .and_then(|stays| stays.last())
            .and_then(|stay| stay["end"].as_i64())
            .unwrap_or(0);
        let right_end = right["stays"]
            .as_array()
            .and_then(|stays| stays.last())
            .and_then(|stay| stay["end"].as_i64())
            .unwrap_or(0);
        right_end.cmp(&left_end)
    });
    let layout = layout.unwrap_or_default();
    let rooms = rooms.unwrap_or_default();
    for id in layout.keys() {
        node_ids.insert(id.clone(), ());
    }
    for id in rooms.keys() {
        node_ids.insert(id.clone(), ());
    }
    for (id, (parent, _)) in &links {
        if node_ids.contains_key(id) {
            node_ids.insert(parent.clone(), ());
        }
    }
    let nodes: Vec<Value> = node_ids
        .keys()
        .map(|id| {
            let point = layout.get(id);
            let link = links.get(id);
            json!({
                "id": id,
                "label": rooms.get(id).unwrap_or(id),
                "x": point.map(|point| point.0),
                "y": point.map(|point| point.1),
                "parent": link.map(|(parent, _)| parent),
                "parent_rssi": link.map(|(_, rssi)| rssi),
            })
        })
        .collect();
    let edge_list: Vec<Value> = edges
        .iter()
        .map(|((from, to), count)| json!({"from": from, "to": to, "count": count}))
        .collect();
    Ok(json!({
        "now": to_ms,
        "from": from_ms,
        "to": to_ms,
        "truncated": truncated,
        "nodes": nodes,
        "tracks": tracks,
        "edges": edge_list,
    }))
}

/// Write `data/mesh/layout.json`. `body` is the JSON object, and it must be only `nodes`.
pub fn store_layout(path: &Path, body: &Map<String, Value>) -> Result<(), String> {
    let Some(nodes) = body.get("nodes").and_then(Value::as_object) else {
        return Err("nodes must be an object".to_owned());
    };
    if nodes.len() > 64 {
        return Err("at most 64 nodes".to_owned());
    }
    let mut clean = Map::new();
    for (id, point) in nodes {
        if !is_node_id(id) {
            return Err(format!("node id {id} is not a short token"));
        }
        let Some(point) = point.as_object() else {
            return Err(format!("node {id} must be an object"));
        };
        if point.len() != 2 || !point.contains_key("x") || !point.contains_key("y") {
            return Err(format!("node {id} takes x and y"));
        }
        let x = point
            .get("x")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
            .ok_or_else(|| format!("node {id} x must be from 0 to 1"))?;
        let y = point
            .get("y")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
            .ok_or_else(|| format!("node {id} y must be from 0 to 1"))?;
        clean.insert(id.clone(), json!({"x": x, "y": y}));
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|_| format!("create {}", parent.display()))?;
        }
    }
    let text = serde_json::to_string_pretty(&json!({"nodes": clean}))
        .map_err(|_| "could not encode the layout".to_owned())?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, text).map_err(|_| format!("write {}", temporary.display()))?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
        .map_err(|_| format!("chmod {}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|_| format!("rename into {}", path.display()))?;
    Ok(())
}

struct Group {
    label: String,
    named: bool,
    samples: Vec<Obs>,
}

struct Sample {
    key: String,
    label: String,
    named: bool,
    ts: i64,
    node: String,
    rssi: i8,
}

fn load_samples(
    path: &Path,
    from_ms: i64,
    to_ms: i64,
    keys: &Keys,
    identities: &Identities,
) -> anyhow::Result<(Vec<Sample>, bool)> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(1))?;
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sightings WHERE ts >= ?1 AND ts <= ?2",
        params![from_ms, to_ms],
        |row| row.get(0),
    )?;
    let truncated = total > SAMPLE_CAP;
    let mut names = BTreeMap::new();
    {
        let mut name_stmt = conn.prepare("SELECT addr, kind, name FROM names")?;
        let mut name_rows = name_stmt.query([])?;
        while let Some(row) = name_rows.next()? {
            let addr: String = row.get(0)?;
            let kind: String = row.get(1)?;
            let name: String = row.get(2)?;
            names.insert(format!("{addr} {kind}"), name);
        }
    }
    let clusters = crate::cluster::load(&conn).unwrap_or_default();
    let faces = track_faces(&clusters, &names, keys, identities);
    let mut stmt = conn.prepare(
        "SELECT ts, addr, kind, node, rssi FROM sightings
         WHERE ts >= ?1 AND ts <= ?2 ORDER BY ts DESC LIMIT ?3",
    )?;
    let mut rows = stmt.query(params![from_ms, to_ms, SAMPLE_CAP])?;
    let mut samples = Vec::new();
    while let Some(row) = rows.next()? {
        let ts: i64 = row.get(0)?;
        let addr: String = row.get(1)?;
        let kind: String = row.get(2)?;
        let node: String = row.get(3)?;
        let rssi = i8::try_from(row.get::<_, i64>(4)?.clamp(-127, 20)).unwrap_or(0);
        let (key, label, named) = match faces.get(&format!("{addr} {kind}")) {
            Some(face) => (face.key.clone(), face.label.clone(), face.named),
            None => unclustered(&addr, &kind, &names, keys, identities),
        };
        samples.push(Sample {
            key,
            label,
            named,
            ts,
            node,
            rssi,
        });
    }
    samples.reverse();
    Ok((samples, truncated))
}

fn segment(samples: &[Obs], now: i64) -> (&'static str, Vec<Stay>) {
    let mut stays = Vec::new();
    let mut open: Option<Stay> = None;
    let mut place: Option<String> = None;
    let mut previous: Option<i64> = None;
    let mut recent: Vec<Obs> = Vec::new();
    for sample in samples {
        if previous.is_some_and(|then| sample.ts.saturating_sub(then) > GAP_MS) {
            if let Some(stay) = open.take() {
                stays.push(stay);
            }
            place = None;
            recent.clear();
        }
        recent.retain(|old| sample.ts.saturating_sub(old.ts) <= FRESH_MS);
        recent.push(Obs {
            ts: sample.ts,
            node: sample.node.clone(),
            rssi: sample.rssi,
        });
        let next = choose_place(&recent, place.as_deref());
        match &mut open {
            Some(stay) if stay.node == next => {
                stay.end = sample.ts;
                if sample.rssi > stay.rssi {
                    stay.rssi = sample.rssi;
                }
            }
            Some(_) => {
                if let Some(stay) = open.take() {
                    stays.push(stay);
                }
                open = Some(Stay {
                    node: next.clone(),
                    start: sample.ts,
                    end: sample.ts,
                    rssi: sample.rssi,
                });
            }
            None => {
                open = Some(Stay {
                    node: next.clone(),
                    start: sample.ts,
                    end: sample.ts,
                    rssi: sample.rssi,
                });
            }
        }
        place = Some(next);
        previous = Some(sample.ts);
    }
    if let Some(stay) = open {
        stays.push(stay);
    }
    (classify(&stays, now), stays)
}

fn choose_place(recent: &[Obs], current: Option<&str>) -> String {
    let best = recent.iter().map(|obs| obs.rssi).max().unwrap_or(-127);
    if let Some(current) = current {
        if let Some(held) = recent
            .iter()
            .filter(|obs| obs.node == current)
            .map(|obs| obs.rssi)
            .max()
        {
            if i16::from(best) - i16::from(held) < LEAD_DB {
                return current.to_string();
            }
        }
    }
    recent
        .iter()
        .find(|obs| obs.rssi == best)
        .map(|obs| obs.node.clone())
        .unwrap_or_else(|| "unknown".to_string())
}

fn classify(stays: &[Stay], now: i64) -> &'static str {
    if stays.is_empty() {
        return "pass";
    }
    let mut nodes = BTreeMap::<&str, ()>::new();
    for stay in stays {
        nodes.insert(stay.node.as_str(), ());
    }
    if nodes.len() >= 2 {
        return "move";
    }
    let ended = now.saturating_sub(stays.last().map(|stay| stay.end).unwrap_or(now)) > GAP_MS;
    let short = stays
        .iter()
        .all(|stay| stay.end.saturating_sub(stay.start) <= PASS_MS);
    if ended && short {
        "pass"
    } else {
        "linger"
    }
}

fn accept(last: &mut HashMap<(String, String, String), (i64, i8)>, row: &Row) -> bool {
    let key = (row.addr.clone(), row.kind.clone(), row.node.clone());
    if let Some((prev_ts, prev_rssi)) = last.get(&key) {
        let within = row.ts >= *prev_ts && row.ts - prev_ts < REPEAT_MS;
        let close = (i16::from(row.rssi) - i16::from(*prev_rssi)).abs() <= REPEAT_DB;
        if within && close {
            return false;
        }
    }
    last.insert(key, (row.ts, row.rssi));
    true
}

fn row_from(node: &str, sighting: &Sighting, now: f64) -> Row {
    Row {
        ts: heard_ms(now, sighting.age_s),
        addr: addr_text(&sighting.addr),
        kind: kind_text(sighting.kind).to_string(),
        node: node.to_string(),
        rssi: sighting.rssi,
        hits: i64::from(sighting.count),
    }
}

fn name_row(named: &Named) -> Option<NameRow> {
    if named.name.is_empty() {
        return None;
    }
    Some(NameRow {
        addr: addr_text(&named.addr),
        kind: kind_text(named.kind).to_string(),
        name: named.name.clone(),
    })
}

fn rows_from_map(text: &str) -> Option<(Vec<Row>, Vec<NameRow>)> {
    let value: Value = serde_json::from_str(text).ok()?;
    let devices = value.get("devices")?.as_object()?;
    let mut rows = Vec::new();
    let mut names = Vec::new();
    for device in devices.values() {
        let addr = device.get("addr")?.as_str()?.to_string();
        let kind = device.get("kind")?.as_str()?.to_string();
        if let Some(name) = device.get("name").and_then(Value::as_str) {
            if !name.is_empty() {
                names.push(NameRow {
                    addr: addr.clone(),
                    kind: kind.clone(),
                    name: name.to_string(),
                });
            }
        }
        let Some(heard) = device.get("heard_by").and_then(Value::as_object) else {
            continue;
        };
        for (node, reading) in heard {
            let Some(last) = reading.get("last").and_then(Value::as_i64) else {
                continue;
            };
            if last < 0 {
                continue;
            }
            let rssi = i8::try_from(
                reading
                    .get("rssi")
                    .and_then(Value::as_i64)
                    .unwrap_or(-127)
                    .clamp(-127, 20),
            )
            .unwrap_or(0);
            let hits = reading
                .get("hits")
                .and_then(Value::as_i64)
                .unwrap_or(1)
                .max(0);
            rows.push(Row {
                ts: last.saturating_mul(1000),
                addr: addr.clone(),
                kind: kind.clone(),
                node: node.clone(),
                rssi,
                hits,
            });
        }
    }
    Some((rows, names))
}

fn read_layout(path: &Path) -> BTreeMap<String, (f64, f64)> {
    let Ok(text) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return BTreeMap::new();
    };
    let Some(nodes) = value.get("nodes").and_then(Value::as_object) else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for (id, point) in nodes {
        let Some(x) = point.get("x").and_then(Value::as_f64) else {
            continue;
        };
        let Some(y) = point.get("y").and_then(Value::as_f64) else {
            continue;
        };
        if x.is_finite() && y.is_finite() && (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y) {
            out.insert(id.clone(), (x, y));
        }
    }
    out
}

/// A real panel keeps `map.json` beside the log. A preview keeps `mesh-map.json`.
fn path_of_map(db: &Path) -> PathBuf {
    let named = db.with_file_name("map.json");
    if named.is_file() {
        return named;
    }
    db.with_file_name("mesh-map.json")
}

/// Parent id and the RSSI of that link, for the listeners this host has heard.
fn read_links(path: &Path) -> BTreeMap<String, (String, i64)> {
    let Ok(text) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return BTreeMap::new();
    };
    let Some(nodes) = value.get("nodes").and_then(Value::as_object) else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for (id, node) in nodes {
        let Some(parent) = parent_id(node.get("parent")) else {
            continue;
        };
        let Some(rssi) = node.get("parent_rssi").and_then(Value::as_i64) else {
            continue;
        };
        if !(-127..=20).contains(&rssi) {
            continue;
        }
        out.insert(id.to_ascii_lowercase(), (parent, rssi));
    }
    out
}

fn parent_id(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(number) = value.as_u64() {
        if number == 0 || number > u64::from(u32::MAX) {
            return None;
        }
        return Some(format!("{number:08x}"));
    }
    if let Some(number) = value.as_i64() {
        if number <= 0 || number > i64::from(u32::MAX) {
            return None;
        }
        return Some(format!("{number:08x}"));
    }
    let text = value.as_str()?;
    if text.is_empty() || text.eq_ignore_ascii_case("0") || text == "00000000" {
        return None;
    }
    Some(text.to_ascii_lowercase())
}

#[derive(Clone)]
struct Face {
    key: String,
    label: String,
    named: bool,
}

fn track_faces(
    clusters: &crate::cluster::Clusters,
    names: &BTreeMap<String, String>,
    keys: &Keys,
    identities: &Identities,
) -> BTreeMap<String, Face> {
    let mut faces = BTreeMap::new();
    for group in clusters.groups() {
        let face = face_of(group, names, keys, identities);
        for addr in &group.members {
            faces.insert(format!("{addr} {}", group.kind), face.clone());
        }
    }
    faces
}

fn face_of(
    group: &crate::cluster::Group,
    names: &BTreeMap<String, String>,
    keys: &Keys,
    identities: &Identities,
) -> Face {
    let kind = parse_kind(&group.kind);
    let mut members = group.members.clone();
    members.sort();
    if let Some(index) = members.iter().position(|addr| addr == &group.seed) {
        members.swap(0, index);
    }
    let mut identity_name = None;
    let mut resolved_name = None;
    let mut air_name = None;
    for addr in &members {
        let resolved =
            parse_addr(addr).and_then(|bytes| keys.resolve(&bytes, kind).map(str::to_string));
        let claimed = identities
            .of_address(addr)
            .or_else(|| resolved.as_deref().and_then(|name| identities.named(name)));
        if let Some(index) = claimed {
            if identity_name.is_none() {
                identity_name = Some(identities.devices()[index].name.clone());
            }
        } else if resolved_name.is_none() {
            resolved_name = resolved;
        }
        if air_name.is_none() {
            air_name = names.get(&format!("{addr} {}", group.kind)).cloned();
        }
    }
    if let Some(name) = identity_name {
        return Face {
            key: format!("device {name}"),
            label: name,
            named: true,
        };
    }
    let key = format!("cluster {} {}", group.seed, group.kind);
    if let Some(name) = resolved_name.or(air_name) {
        return Face {
            key,
            label: name,
            named: true,
        };
    }
    Face {
        key,
        label: crate::cluster::cute_name(&group.seed),
        named: false,
    }
}

fn unclustered(
    addr: &str,
    kind: &str,
    names: &BTreeMap<String, String>,
    keys: &Keys,
    identities: &Identities,
) -> (String, String, bool) {
    let advert = names.get(&format!("{addr} {kind}"));
    let resolved = parse_addr(addr).and_then(|bytes| keys.resolve(&bytes, parse_kind(kind)));
    let identity = identities
        .of_address(addr)
        .or_else(|| resolved.and_then(|name| identities.named(name)));
    if let Some(index) = identity {
        let name = identities.devices()[index].name.clone();
        return (format!("device {name}"), name, true);
    }
    let named = resolved.is_some() || advert.is_some();
    let label = resolved
        .map(str::to_string)
        .or_else(|| advert.cloned())
        .unwrap_or_else(|| crate::cluster::cute_name(addr));
    (format!("{addr} {kind}"), label, named)
}

fn read_rooms(path: &Path) -> BTreeMap<String, String> {
    #[derive(serde::Deserialize)]
    struct File {
        #[serde(default)]
        nodes: BTreeMap<String, String>,
    }
    let Ok(text) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    toml::from_str::<File>(&text)
        .map(|file| file.nodes)
        .unwrap_or_default()
}

fn is_node_id(id: &str) -> bool {
    let len = id.chars().count();
    (1..=32).contains(&len)
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn privatize(path: &Path) {
    let mode = fs::Permissions::from_mode(0o600);
    let _ = fs::set_permissions(path, mode.clone());
    for suffix in ["-wal", "-shm"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        let _ = fs::set_permissions(Path::new(&name), mode.clone());
    }
}

fn heard_ms(now: f64, age_s: u8) -> i64 {
    let seconds = now - f64::from(age_s);
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    (seconds * 1000.0).round() as i64
}

fn ms(now: f64) -> i64 {
    if !now.is_finite() || now <= 0.0 {
        return 0;
    }
    (now * 1000.0).round() as i64
}

fn addr_text(addr: &[u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        addr[0], addr[1], addr[2], addr[3], addr[4], addr[5]
    )
}

fn parse_addr(text: &str) -> Option<[u8; 6]> {
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

fn kind_text(kind: Option<Kind>) -> &'static str {
    match kind {
        Some(Kind::BlePublic) => "ble-public",
        Some(Kind::BleRandom) => "ble-random",
        Some(Kind::Classic) => "classic",
        None => "unknown",
    }
}

fn parse_kind(text: &str) -> Option<Kind> {
    match text {
        "ble-public" => Some(Kind::BlePublic),
        "ble-random" => Some(Kind::BleRandom),
        "classic" => Some(Kind::Classic),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "tallowd-traffic-{name}-{}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn obs(ts: i64, node: &str, rssi: i8) -> Obs {
        Obs {
            ts,
            node: node.to_string(),
            rssi,
        }
    }

    fn batch(node: &str, addr: [u8; 6], rssi: i8) -> Batch {
        Batch {
            node: node.to_string(),
            uptime_s: 0,
            sightings: vec![Sighting {
                addr,
                kind: Some(Kind::BlePublic),
                rssi,
                count: 1,
                age_s: 0,
            }],
            names: Vec::new(),
        }
    }

    #[test]
    fn a_repeat_within_five_seconds_and_three_db_is_skipped_and_a_single_ping_is_kept() {
        let dir = scratch("skip");
        let path = dir.join("traffic.db");
        let mut journal = Journal::open(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the file is private before the first insert");
        let now = 1_700_000_000.0;
        let addr = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x01];
        journal
            .note_batch(&batch("aaaaaaaa", addr, -60), now)
            .unwrap();
        journal
            .note_batch(&batch("aaaaaaaa", addr, -62), now + 4.0)
            .unwrap();
        assert_eq!(journal.stored().unwrap(), 1, "the near repeat is skipped");
        journal
            .note_batch(&batch("aaaaaaaa", addr, -50), now + 4.5)
            .unwrap();
        assert_eq!(journal.stored().unwrap(), 2, "a 10 dB change is kept");
        journal
            .note_batch(&batch("aaaaaaaa", addr, -50), now + 11.0)
            .unwrap();
        assert_eq!(
            journal.stored().unwrap(),
            3,
            "the same reading after five seconds is kept"
        );
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = fs::remove_dir_all(&dir);
    }

    fn named(node: &str, addr: [u8; 6], rssi: i8, name: &str) -> Batch {
        Batch {
            node: node.to_string(),
            uptime_s: 0,
            sightings: vec![Sighting {
                addr,
                kind: Some(Kind::BlePublic),
                rssi,
                count: 1,
                age_s: 0,
            }],
            names: vec![Named {
                addr,
                kind: Some(Kind::BlePublic),
                company: None,
                class: 0,
                name: name.to_string(),
            }],
        }
    }

    #[test]
    fn a_name_goes_when_its_last_sighting_is_pruned() {
        let dir = scratch("prune-name");
        let mut journal = Journal::open(&dir.join("traffic.db")).unwrap();
        let start = 1_700_000_000.0;
        let gone = [0x11, 0x22, 0x33, 0x44, 0x55, 0x0A];
        let kept = [0x11, 0x22, 0x33, 0x44, 0x55, 0x0B];
        journal
            .note_batch(&named("aaaaaaaa", gone, -40, "Alice"), start)
            .unwrap();
        let later = start + (RETAIN_MS as f64) / 1000.0 + (PRUNE_EVERY_MS as f64) / 1000.0;
        journal
            .note_batch(&named("aaaaaaaa", kept, -40, "Bob"), later)
            .unwrap();
        let count = |addr: [u8; 6]| -> i64 {
            journal
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM names WHERE addr = ?1",
                    params![addr_text(&addr)],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(count(gone), 0);
        assert_eq!(count(kept), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cleared_log_keeps_no_address_bytes() {
        let dir = scratch("erase");
        let db = dir.join("traffic.db");
        let mut journal = Journal::open(&db).unwrap();
        journal
            .note_batch(
                &Batch {
                    node: "aaaaaaaa".to_string(),
                    uptime_s: 0,
                    sightings: vec![Sighting {
                        addr: [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01],
                        kind: Some(Kind::BlePublic),
                        rssi: -40,
                        count: 1,
                        age_s: 0,
                    }],
                    names: vec![Named {
                        addr: [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01],
                        kind: Some(Kind::BlePublic),
                        company: None,
                        class: 0,
                        name: "Secret Phone".to_string(),
                    }],
                },
                1_700_000_000.0,
            )
            .unwrap();
        drop(journal);
        clear(&db).unwrap();
        let db_bytes = fs::read(&db).unwrap();
        let wal = PathBuf::from(format!("{}-wal", db.display()));
        let wal_bytes = fs::read(&wal).unwrap_or_default();
        for needle in [b"DE:AD:BE:EF:00:01".as_slice(), b"Secret Phone".as_slice()] {
            assert!(
                !db_bytes
                    .windows(needle.len())
                    .any(|window| window == needle),
                "traffic.db still holds {}",
                String::from_utf8_lossy(needle)
            );
            assert!(
                !wal_bytes
                    .windows(needle.len())
                    .any(|window| window == needle),
                "the wal still holds {}",
                String::from_utf8_lossy(needle)
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_log_that_cannot_be_opened_is_off_and_does_not_fail() {
        let dir = scratch("off");
        let log_path = dir.join("not-a-file");
        fs::create_dir(&log_path).unwrap();
        let mut log = open_log(&log_path, &dir.join("map.json"));
        assert!(log.journal.is_none());
        log.note_batch(
            &batch("aaaaaaaa", [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x01], -60),
            1.0,
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ten_failures_in_a_row_stop_the_log() {
        let dir = scratch("stop");
        let mut log = open_log(&dir.join("traffic.db"), &dir.join("map.json"));
        log.journal.as_mut().unwrap().conn = Connection::open_in_memory().unwrap();
        for n in 0..10u8 {
            log.note_batch(&batch("aaaaaaaa", [n, 0, 0, 0, 0, 1], -60), 1_700_000_000.0);
        }
        assert!(log.journal.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_repeat_cache_forgets_old_addresses() {
        let dir = scratch("cache");
        let mut journal = Journal::open(&dir.join("traffic.db")).unwrap();
        let start = 1_700_000_000.0;
        for n in 0..1000u16 {
            let addr = [(n >> 8) as u8, n as u8, 0, 0, 0, 1];
            journal
                .note_batch(&batch("aaaaaaaa", addr, -60), start)
                .unwrap();
        }
        assert_eq!(journal.cached(), 1000);
        let later = start + (PRUNE_EVERY_MS as f64) / 1000.0 + 1.0;
        journal
            .note_batch(&batch("aaaaaaaa", [9, 9, 9, 9, 9, 9], -60), later)
            .unwrap();
        assert_eq!(journal.cached(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_log_copies_the_map_once() {
        let dir = scratch("seed");
        let map = dir.join("map.json");
        fs::write(
            &map,
            r#"{"nodes":{},"devices":{"AA:BB:CC:DD:EE:02 ble-public":{"addr":"AA:BB:CC:DD:EE:02","kind":"ble-public","name":"Speaker","heard_by":{"aaaaaaaa":{"rssi":-55,"hits":4,"last":1700000000},"bbbbbbbb":{"rssi":-70,"hits":1,"last":1700000005}},"nearest":"aaaaaaaa"}}}"#,
        )
        .unwrap();
        let db = dir.join("traffic.db");
        let mut journal = Journal::open(&db).unwrap();
        assert_eq!(journal.seed_map(&map).unwrap(), 2);
        assert_eq!(
            journal.seed_map(&map).unwrap(),
            0,
            "a second open does not copy again"
        );
        assert_eq!(journal.stored().unwrap(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_stronger_reading_does_not_hold_the_place() {
        let stale = [
            obs(0, "a", -30),
            obs(30_000, "b", -70),
            obs(70_000, "b", -65),
        ];
        let (_class, stays) = segment(&stale, 70_000);
        assert_eq!(stays.last().unwrap().node, "b");
        let held = [obs(20_000, "a", -30), obs(40_000, "b", -65)];
        let (_class, stays) = segment(&held, 40_000);
        assert_eq!(stays.last().unwrap().node, "a");
    }

    #[test]
    fn a_tie_keeps_the_node_already_shown_and_a_lead_moves() {
        let tie = [obs(0, "a", -60), obs(10_000, "b", -60)];
        let (class, stays) = segment(&tie, 10_000);
        assert_eq!(stays.len(), 1);
        assert_eq!(stays[0].node, "a");
        assert_eq!(class, "linger");
        let lead = [obs(0, "a", -60), obs(10_000, "b", -50)];
        let (class, stays) = segment(&lead, 10_000);
        assert_eq!(class, "move");
        assert_eq!(stays[1].node, "b");
    }

    #[test]
    fn one_ping_then_silence_is_a_pass_and_a_held_place_is_a_linger() {
        let (class, _) = segment(&[obs(0, "a", -70)], 120_000);
        assert_eq!(class, "pass");
        let linger: Vec<Obs> = (0..7).map(|step| obs(step * 10_000, "a", -55)).collect();
        let (class, _) = segment(&linger, 60_000);
        assert_eq!(class, "linger");
    }

    #[test]
    fn the_picture_names_a_move_and_a_pass() {
        let dir = scratch("picture");
        let db = dir.join("traffic.db");
        let mut journal = Journal::open(&db).unwrap();
        let now = 1_700_000_000.0;
        let mover = [0xAA, 0x11, 0x22, 0x33, 0x44, 0x01];
        let passer = [0xBB, 0x11, 0x22, 0x33, 0x44, 0x02];
        journal
            .note_batch(&batch("aaaaaaaa", mover, -60), now)
            .unwrap();
        journal
            .note_batch(&batch("bbbbbbbb", mover, -40), now + 10.0)
            .unwrap();
        journal
            .note_batch(&batch("aaaaaaaa", passer, -80), now - 200.0)
            .unwrap();
        let from = ms(now - 200.0) - 1_000;
        let to = ms(now + 10.0) + 120_000;
        let drawn = picture(Some(&db), from, to).unwrap();
        let tracks = drawn["tracks"].as_array().unwrap();
        let classes: Vec<&str> = tracks
            .iter()
            .map(|track| track["class"].as_str().unwrap())
            .collect();
        assert!(classes.contains(&"move"), "{classes:?}");
        assert!(classes.contains(&"pass"), "{classes:?}");
        assert_eq!(drawn["edges"][0]["from"], "aaaaaaaa");
        assert_eq!(drawn["edges"][0]["to"], "bbbbbbbb");
        assert!(store_layout(
            &db.with_file_name("layout.json"),
            &serde_json::from_str(r#"{"nodes":{"aaaaaaaa":{"x":0.25,"y":0.5}}}"#).unwrap(),
        )
        .is_ok());
        let placed = picture(Some(&db), from, to).unwrap();
        let node = placed["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == "aaaaaaaa")
            .unwrap();
        assert_eq!(node["x"], 0.25);
        assert!(store_layout(
            &db.with_file_name("layout.json"),
            &serde_json::from_str(r#"{"nodes":{"aaaaaaaa":{"x":2,"y":0}}}"#).unwrap(),
        )
        .is_err());
        clear(&db).unwrap();
        assert_eq!(
            picture(Some(&db), from, to).unwrap()["tracks"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_log_is_an_empty_picture() {
        let empty = picture(None, 0, 1).unwrap();
        assert_eq!(empty["tracks"].as_array().unwrap().len(), 0);
        assert_eq!(empty["truncated"], json!(false));
    }

    fn random_batch(node: &str, addr: [u8; 6], rssi: i8) -> Batch {
        Batch {
            node: node.to_string(),
            uptime_s: 0,
            sightings: vec![Sighting {
                addr,
                kind: Some(Kind::BleRandom),
                rssi,
                count: 1,
                age_s: 0,
            }],
            names: Vec::new(),
        }
    }

    #[test]
    fn a_long_handoff_is_one_track_under_the_seed_name() {
        let dir = scratch("cluster");
        let db = dir.join("traffic.db");
        let mut journal = Journal::open(&db).unwrap();
        let now = 1_700_000_000.0;
        let older = [0x11, 0x22, 0x33, 0x44, 0x55, 0x01];
        let newer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x02];
        journal
            .note_batch(&random_batch("aaaaaaaa", older, -60), now)
            .unwrap();
        journal
            .note_batch(
                &random_batch("aaaaaaaa", older, -60),
                now + 15.0 * 60.0 - 60.0,
            )
            .unwrap();
        journal
            .note_batch(&random_batch("aaaaaaaa", older, -60), now + 15.0 * 60.0)
            .unwrap();
        journal
            .note_batch(
                &random_batch("aaaaaaaa", newer, -60),
                now + 15.0 * 60.0 + 10.0,
            )
            .unwrap();
        journal
            .note_batch(
                &random_batch("aaaaaaaa", newer, -60),
                now + 15.0 * 60.0 + 70.0,
            )
            .unwrap();
        journal
            .note_batch(&random_batch("aaaaaaaa", newer, -60), now + 31.0 * 60.0)
            .unwrap();
        journal.reconcile_clusters().unwrap();
        let drawn = picture(Some(&db), ms(now) - 1_000, ms(now + 31.0 * 60.0) + 1_000).unwrap();
        let tracks = drawn["tracks"].as_array().unwrap();
        assert_eq!(tracks.len(), 1, "{tracks:?}");
        let name = crate::cluster::cute_name("11:22:33:44:55:01");
        assert_eq!(tracks[0]["label"], json!(name));
        assert_eq!(tracks[0]["named"], json!(false));
        assert!(tracks[0]["samples"].as_array().unwrap().len() >= 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_from_the_air_beats_the_cute_name_and_a_listed_device_names_the_cluster() {
        let dir = scratch("names");
        let db = dir.join("traffic.db");
        let mut journal = Journal::open(&db).unwrap();
        let now = 1_700_000_000.0;
        let speaker = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x02];
        journal
            .note_batch(
                &Batch {
                    node: "aaaaaaaa".to_string(),
                    uptime_s: 0,
                    sightings: vec![Sighting {
                        addr: speaker,
                        kind: Some(Kind::BlePublic),
                        rssi: -50,
                        count: 1,
                        age_s: 0,
                    }],
                    names: vec![Named {
                        addr: speaker,
                        kind: Some(Kind::BlePublic),
                        company: None,
                        class: 0,
                        name: "Speaker".to_string(),
                    }],
                },
                now,
            )
            .unwrap();
        let quiet = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x03];
        journal
            .note_batch(&batch("aaaaaaaa", quiet, -70), now)
            .unwrap();
        let drawn = picture(Some(&db), ms(now) - 1_000, ms(now) + 1_000).unwrap();
        let tracks = drawn["tracks"].as_array().unwrap();
        let speaker_track = tracks
            .iter()
            .find(|track| track["label"] == "Speaker")
            .unwrap();
        assert_eq!(speaker_track["named"], json!(true));
        let quiet_track = tracks
            .iter()
            .find(|track| track["id"] == "AA:BB:CC:DD:EE:03 ble-public")
            .unwrap();
        assert_eq!(
            quiet_track["label"],
            json!(crate::cluster::cute_name("AA:BB:CC:DD:EE:03"))
        );
        assert_eq!(quiet_track["named"], json!(false));

        let older = [0x11, 0x22, 0x33, 0x44, 0x55, 0x01];
        let newer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x02];
        journal
            .note_batch(&random_batch("aaaaaaaa", older, -60), now)
            .unwrap();
        journal
            .note_batch(
                &random_batch("aaaaaaaa", older, -60),
                now + 15.0 * 60.0 - 60.0,
            )
            .unwrap();
        journal
            .note_batch(&random_batch("aaaaaaaa", older, -60), now + 15.0 * 60.0)
            .unwrap();
        journal
            .note_batch(
                &random_batch("aaaaaaaa", newer, -60),
                now + 15.0 * 60.0 + 10.0,
            )
            .unwrap();
        journal
            .note_batch(
                &random_batch("aaaaaaaa", newer, -60),
                now + 15.0 * 60.0 + 70.0,
            )
            .unwrap();
        journal
            .note_batch(&random_batch("aaaaaaaa", newer, -60), now + 31.0 * 60.0)
            .unwrap();
        journal.reconcile_clusters().unwrap();
        fs::write(
            db.with_file_name("devices.toml"),
            "[[device]]\nname = \"Phone\"\ntype = \"phone\"\naddresses = [\"11:22:33:44:55:01\"]\n",
        )
        .unwrap();
        fs::write(
            db.with_file_name("map.json"),
            r#"{"nodes":{"bbbbbbbb":{"parent":2863311530,"parent_rssi":-55}},"devices":{}}"#,
        )
        .unwrap();
        let drawn = picture(Some(&db), ms(now) - 1_000, ms(now + 31.0 * 60.0) + 1_000).unwrap();
        let phone = drawn["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|track| track["label"] == "Phone")
            .unwrap();
        assert_eq!(phone["id"], json!("device Phone"));
        assert_eq!(phone["named"], json!(true));
        let hall = drawn["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == "bbbbbbbb");
        assert!(hall.is_none(), "a node with no sighting is not invented");
        journal
            .note_batch(&random_batch("bbbbbbbb", newer, -50), now + 31.0 * 60.0)
            .unwrap();
        let drawn = picture(Some(&db), ms(now) - 1_000, ms(now + 31.0 * 60.0) + 1_000).unwrap();
        let hall = drawn["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == "bbbbbbbb")
            .unwrap();
        assert_eq!(hall["parent"], json!("aaaaaaaa"));
        assert_eq!(hall["parent_rssi"], json!(-55));
        let _ = fs::remove_dir_all(&dir);
    }
}
