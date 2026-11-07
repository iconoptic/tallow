//! Which node hears which device, kept as `tools/mesh_host.py` keeps it.
//!
//! [`handle`] is the Python `handle`, and [`Map::save`] writes the same shape
//! as `Map.save`. Nothing here reads the clock: `now` is seconds since the
//! epoch, as `time.time()` gives it in Python.

use crate::decode::{self, Named, ParentChange, Sighting};
use crate::identity::Identities;
use crate::irk::Keys;
use crate::link::{parse_line, Line};
use crate::local::Batch;
use crate::rooms::Rooms;
use anyhow::Context;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tallow_mesh_core::mesh::Reason;
use tallow_mesh_core::ota::Identity;
use tallow_mesh_core::sightings::Kind;
use tallow_mesh_core::stats::Stats;
use tallow_mesh_core::wire::FrameKind;

/// How often the live map is written. Matches `run_map` in `mesh_host.py`.
pub const SAVE_EVERY_S: f64 = 5.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Heard {
    rssi: i8,
    hits: u64,
    last: i64,
}

#[derive(Debug)]
struct Device {
    addr: String,
    bytes: [u8; 6],
    kind: &'static str,
    /// In the order nodes were first heard, so a tie goes to the first.
    heard_by: Vec<(String, Heard)>,
    name: Option<String>,
    /// The device whose IRK resolves this address. Set once, when the address
    /// is first seen, so each address is tried against the keys only once.
    resolved: Option<String>,
    company: Option<u16>,
    class: Option<u32>,
}

#[derive(Clone, Copy, Debug)]
struct NodeRow {
    stats: Stats,
    hops: u32,
    link_rssi: i32,
    seen: i64,
    firmware: Option<Identity>,
}

/// The nodes and the devices they hear. Devices are keyed `"<addr> <kind>"`, as
/// the Python keys them, so one address on two transports stays two devices.
#[derive(Debug, Default)]
pub struct Map {
    devices: BTreeMap<String, Device>,
    nodes: BTreeMap<String, NodeRow>,
    keys: Keys,
    identities: Identities,
    /// Which room each node is in. Empty when there is no `rooms.toml`.
    rooms: Rooms,
    /// The last time `Recorder::feed` ran, so a device's room is judged as of now.
    clock: f64,
}

/// Two or more map keys that may be one device, and why. Only the user merges
/// them, by listing them in `devices.toml`.
#[derive(Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub keys: Vec<String>,
    pub reason: String,
}

impl Map {
    /// An empty map that names a device when one of `keys` resolves its address,
    /// and groups devices under `identities`.
    pub fn with_keys(keys: Keys, identities: Identities) -> Map {
        Map {
            keys,
            identities,
            ..Map::default()
        }
    }

    /// How many devices and nodes the map holds.
    pub fn counts(&self) -> (usize, usize) {
        (self.devices.len(), self.nodes.len())
    }

    /// Each node that has sent stats, with the RSSI of its link to the root. The
    /// trial takes a link RSSI of zero to be the root itself, which never crosses the air.
    pub fn stats_links(&self) -> impl Iterator<Item = (&str, i32)> + '_ {
        self.nodes
            .iter()
            .map(|(id, row)| (id.as_str(), row.link_rssi))
    }

    pub fn sighting(&mut self, node: &str, sighting: &Sighting, now: f64) {
        let device = self.device(sighting.addr, sighting.kind);
        let index = match device.heard_by.iter().position(|(name, _)| name == node) {
            Some(index) => index,
            None => {
                device.heard_by.push((node.to_string(), Heard::default()));
                device.heard_by.len() - 1
            }
        };
        let heard = &mut device.heard_by[index].1;
        heard.rssi = sighting.rssi;
        heard.hits += u64::from(sighting.count);
        heard.last = py_round(now - f64::from(sighting.age_s));
        // Copied out, so the borrow of `device` ends before `self` is read again.
        let addr = device.addr.clone();
        let kind = device.kind;
        let resolved = device.resolved.clone();
        if self.rooms.is_empty() {
            return;
        }
        let group = room_group(&self.identities, &addr, kind, resolved.as_deref());
        let at = now - f64::from(sighting.age_s);
        self.rooms.hear(&group, node, sighting.rssi, at, now);
    }

    /// The room of `device` as of the map's clock, if it has one.
    fn room_of(&self, device: &Device) -> Option<&str> {
        if self.rooms.is_empty() {
            return None;
        }
        let group = room_group(
            &self.identities,
            &device.addr,
            device.kind,
            device.resolved.as_deref(),
        );
        self.rooms.room(&group, self.clock)
    }

    pub fn name(&mut self, named: &Named) {
        let device = self.device(named.addr, named.kind);
        device.name = Some(named.name.clone());
        if let Some(company) = named.company {
            device.company = Some(company);
        }
        if named.class != 0 {
            device.class = Some(named.class);
        }
    }

    /// What the host's own adapter heard. It is a node of rank 0 at no hops, and its
    /// advert counts are kept across batches as a board's are.
    pub fn local(&mut self, batch: &Batch, now: f64) {
        let mut stats = self
            .nodes
            .get(&batch.node)
            .map_or_else(Stats::default, |row| row.stats);
        stats.uptime_s = batch.uptime_s;
        for sighting in &batch.sightings {
            let seen = u32::from(sighting.count);
            if sighting.kind == Some(Kind::Classic) {
                stats.classic = stats.classic.saturating_add(seen);
            } else {
                stats.adverts = stats.adverts.saturating_add(seen);
            }
            self.sighting(&batch.node, sighting, now);
        }
        for named in &batch.names {
            self.name(named);
        }
        self.stats(&batch.node, stats, 0, 0, now);
    }

    fn stats(&mut self, node: &str, stats: Stats, hops: u32, link_rssi: i32, now: f64) {
        let firmware = self.nodes.get(node).and_then(|row| row.firmware);
        let row = NodeRow {
            stats,
            hops,
            link_rssi,
            seen: py_round(now),
            firmware,
        };
        self.nodes.insert(node.to_string(), row);
    }

    /// The product and versions from an Identity frame. Stats already stored for
    /// this node stay. A node with no stats yet keeps an empty report.
    fn identity(&mut self, node: &str, identity: Identity, hops: u32, link_rssi: i32, now: f64) {
        let previous = self.nodes.get(node).copied();
        let row = NodeRow {
            stats: previous.map(|row| row.stats).unwrap_or_default(),
            hops,
            link_rssi,
            seen: py_round(now),
            firmware: Some(identity),
        };
        self.nodes.insert(node.to_string(), row);
    }

    fn device(&mut self, addr: [u8; 6], kind: Option<Kind>) -> &mut Device {
        let text = addr_text(&addr);
        let kind_text = kind_name(kind);
        let key = format!("{text} {kind_text}");
        let keys = &self.keys;
        self.devices.entry(key).or_insert_with(|| Device {
            resolved: keys.resolve(&addr, kind).map(str::to_string),
            addr: text,
            bytes: addr,
            kind: kind_text,
            heard_by: Vec::new(),
            name: None,
            company: None,
            class: None,
        })
    }

    /// The map as `mesh_host.py` writes it, with each device's `nearest` filled in.
    /// `identities` and `suggestions` are added only when there are any, so a map
    /// with no `devices.toml` is the map `mesh_host.py` writes.
    pub fn to_json(&self) -> Value {
        let devices: serde_json::Map<String, Value> = self
            .devices
            .iter()
            .map(|(key, device)| (key.clone(), device.to_json(self.room_of(device))))
            .collect();
        let nodes: serde_json::Map<String, Value> = self
            .nodes
            .iter()
            .map(|(id, row)| (id.clone(), row.to_json()))
            .collect();
        let mut out = serde_json::Map::default();
        out.insert("nodes".into(), Value::Object(nodes));
        out.insert("devices".into(), Value::Object(devices));
        let (members, suggestions) = self.grouping();
        if !self.identities.devices().is_empty() {
            let identities: serde_json::Map<String, Value> = self
                .identities
                .devices()
                .iter()
                .zip(members)
                .map(|(device, keys)| {
                    (
                        device.name.clone(),
                        json!({ "type": device.kind, "keys": keys }),
                    )
                })
                .collect();
            out.insert("identities".into(), Value::Object(identities));
        }
        if !suggestions.is_empty() {
            let suggestions: Vec<Value> = suggestions
                .iter()
                .map(|s| json!({ "keys": s.keys, "reason": s.reason }))
                .collect();
            out.insert("suggestions".into(), Value::Array(suggestions));
        }
        Value::Object(out)
    }

    /// The suggestions, one line each, for `run` to print once at the end.
    pub fn suggestion_lines(&self) -> Vec<String> {
        self.grouping()
            .1
            .iter()
            .map(|s| format!("suggest {} ({})", s.keys.join(", "), s.reason))
            .collect()
    }

    /// The map keys in each named device, in the identities' order, and the
    /// suggestions for the keys that no named device claims. A key is claimed by
    /// its address, or by a `resolved` name that matches a device's name.
    fn grouping(&self) -> (Vec<Vec<String>>, Vec<Suggestion>) {
        let mut members = vec![Vec::new(); self.identities.devices().len()];
        let mut loose = Vec::new();
        for (key, device) in &self.devices {
            let claimed = self.identities.of_address(&device.addr).or_else(|| {
                device
                    .resolved
                    .as_deref()
                    .and_then(|name| self.identities.named(name))
            });
            match claimed {
                Some(index) => members[index].push(key.clone()),
                None => loose.push((key.as_str(), device)),
            }
        }
        (members, suggestions(&loose))
    }

    /// Write the map to `path` through `path.tmp`, so a reader never sees half a file.
    /// The map holds device addresses, so the file is private to this user.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let mut temporary_name = path.as_os_str().to_owned();
        temporary_name.push(".tmp");
        let temporary = Path::new(&temporary_name);
        let text = serde_json::to_string_pretty(&self.to_json()).context("encode the map")?;
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(temporary)
            .and_then(|mut file| file.write_all(text.as_bytes()))
            .with_context(|| format!("write {}", temporary.display()))?;
        // `.mode()` applies only when the file is created, so a stale `.tmp`
        // left at a looser mode by an earlier run would survive the truncate.
        // The map holds device addresses, so force 0600 before the rename.
        fs::set_permissions(temporary, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod {}", temporary.display()))?;
        fs::rename(temporary, path).with_context(|| format!("rename into {}", path.display()))?;
        Ok(())
    }
}

/// The key a device's readings share for rooms. A device that a named device
/// claims, by address or by resolved name, is one device, so all its addresses
/// share the identity's key. Any other device keeps its own map key.
fn room_group(identities: &Identities, addr: &str, kind: &str, resolved: Option<&str>) -> String {
    let claimed = identities
        .of_address(addr)
        .or_else(|| resolved.and_then(|name| identities.named(name)));
    match claimed {
        Some(index) => format!("device {}", identities.devices()[index].name),
        None => format!("{addr} {kind}"),
    }
}

/// The suggestions among the keys no named device claims. Two rules, each over
/// groups of two or more keys:
///
/// - The same advertised name, for any kind of address.
/// - The same low four address bytes, for Classic and public BLE addresses.
///   A `ble-random` address is skipped: a rotating one changes, so sharing low
///   bytes with another address is chance, not a sign of one device.
///
/// A group that both rules make is one suggestion with both reasons.
fn suggestions(loose: &[(&str, &Device)]) -> Vec<Suggestion> {
    let mut by_name: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut by_low: BTreeMap<[u8; 4], Vec<String>> = BTreeMap::new();
    for (key, device) in loose {
        if let Some(name) = &device.name {
            by_name
                .entry(name.as_str())
                .or_default()
                .push(key.to_string());
        }
        if device.kind != "ble-random" {
            let b = device.bytes;
            by_low
                .entry([b[2], b[3], b[4], b[5]])
                .or_default()
                .push(key.to_string());
        }
    }
    let mut groups: BTreeMap<Vec<String>, Vec<String>> = BTreeMap::new();
    for (name, keys) in by_name {
        if keys.len() >= 2 {
            let reason = format!("same name {}", py_repr(name));
            groups.entry(keys).or_default().push(reason);
        }
    }
    for (low, keys) in by_low {
        if keys.len() >= 2 {
            let reason = format!(
                "same low 4 address bytes {:02X}:{:02X}:{:02X}:{:02X}",
                low[0], low[1], low[2], low[3]
            );
            groups.entry(keys).or_default().push(reason);
        }
    }
    groups
        .into_iter()
        .map(|(keys, reasons)| Suggestion {
            keys,
            reason: reasons.join("; "),
        })
        .collect()
}

impl Device {
    /// The node that heard this device at the highest RSSI. On a tie, the first
    /// node heard keeps it, as Python's `max()` does.
    fn nearest(&self) -> Option<&str> {
        let mut best: Option<&(String, Heard)> = None;
        for entry in &self.heard_by {
            if best.is_none_or(|kept| entry.1.rssi > kept.1.rssi) {
                best = Some(entry);
            }
        }
        best.map(|(node, _)| node.as_str())
    }

    /// `room` is written only when the device has one.
    fn to_json(&self, room: Option<&str>) -> Value {
        let heard: serde_json::Map<String, Value> = self
            .heard_by
            .iter()
            .map(|(node, heard)| {
                (
                    node.clone(),
                    json!({ "rssi": heard.rssi, "hits": heard.hits, "last": heard.last }),
                )
            })
            .collect();
        let mut out = serde_json::Map::default();
        out.insert("addr".into(), json!(self.addr));
        out.insert("kind".into(), json!(self.kind));
        out.insert("heard_by".into(), Value::Object(heard));
        out.insert("nearest".into(), json!(self.nearest()));
        if let Some(room) = room {
            out.insert("room".into(), json!(room));
        }
        if let Some(name) = &self.name {
            out.insert("name".into(), json!(name));
        }
        if let Some(resolved) = &self.resolved {
            out.insert("resolved".into(), json!(resolved));
        }
        if let Some(company) = self.company {
            out.insert("company".into(), json!(company));
        }
        if let Some(class) = self.class {
            out.insert("class".into(), json!(class));
        }
        Value::Object(out)
    }
}

impl NodeRow {
    fn to_json(self) -> Value {
        let s = self.stats;
        let words: [(&str, u32); 21] = [
            ("uptime_s", s.uptime_s),
            ("heap_free", s.heap_free),
            ("heap_min", s.heap_min),
            ("heap_block", s.heap_block),
            ("adverts", s.adverts),
            ("classic", s.classic),
            ("inquiries", s.inquiries),
            ("rx_frames", s.rx_frames),
            ("rx_drops", s.rx_drops),
            ("rx_bad", s.rx_bad),
            ("rx_dup", s.rx_dup),
            ("tx_ok", s.tx_ok),
            ("tx_failed", s.tx_failed),
            ("queue_drops", s.queue_drops),
            ("parent_changes", s.parent_changes),
            ("evicted", s.evicted),
            ("parent", s.parent),
            ("settings", s.settings),
            ("inquiry_started", s.inquiry_started),
            ("inquiry_stopped", s.inquiry_stopped),
            ("inquiry_errors", s.inquiry_errors),
        ];
        let mut out = serde_json::Map::default();
        for (name, value) in words {
            out.insert(name.into(), json!(value));
        }
        out.insert("rank".into(), json!(s.rank));
        out.insert("neighbours".into(), json!(s.neighbours));
        out.insert("parent_rssi".into(), json!(s.parent_rssi));
        out.insert("parent_quality".into(), json!(s.parent_quality));
        out.insert("hops".into(), json!(self.hops));
        out.insert("link_rssi".into(), json!(self.link_rssi));
        out.insert("seen".into(), json!(self.seen));
        if let Some(firmware) = self.firmware {
            out.insert("product".into(), json!(firmware.product));
            out.insert("running".into(), json!(firmware.running));
            out.insert("floor".into(), json!(firmware.floor));
        }
        Value::Object(out)
    }
}

/// One frame from the root, as `handle` in `mesh_host.py` does it. Returns the
/// lines to print, which are none when `quiet`.
pub fn handle(world: &mut Map, line: &Line, now: f64, quiet: bool) -> Vec<String> {
    let node = line.origin.as_str();
    let mut printed = Vec::new();
    let Ok(code) = u8::try_from(line.frame) else {
        return printed;
    };
    match FrameKind::from_u8(code) {
        Some(FrameKind::Report) => {
            for sighting in decode::sightings(&line.payload) {
                world.sighting(node, &sighting, now);
            }
        }
        Some(FrameKind::Name) => {
            for named in decode::names(&line.payload) {
                world.name(&named);
                if !quiet {
                    printed.push(name_line(node, &named));
                }
            }
        }
        Some(FrameKind::Route) => {
            if let Some(change) = decode::parent_change(&line.payload) {
                if !quiet {
                    printed.push(route_line(node, &change));
                }
            }
        }
        Some(FrameKind::Stats) => {
            if let Some(found) = decode::node_stats(&line.payload) {
                world.stats(node, found.stats, line.hops, line.rssi, now);
                if !quiet {
                    printed.push(stats_line(node, &found.stats));
                }
            }
        }
        Some(FrameKind::Identity) => {
            if let Some(identity) = tallow_mesh_core::ota::Identity::decode(&line.payload) {
                world.identity(node, identity, line.hops, line.rssi, now);
                if !quiet {
                    printed.push(identity_line(node, &identity));
                }
            }
        }
        // An answer, a notification, or a closed link. Not a device the mesh heard.
        Some(FrameKind::GattUp) => {}
        // Probes and the rest are not kept. Python's handle ignores them too.
        _ => {}
    }
    printed
}

/// What one pass over the port's lines does: the map, and when it next saves.
#[derive(Debug, Default)]
pub struct Recorder {
    pub world: Map,
    next_save: f64,
}

impl Recorder {
    /// A recorder whose map names devices with `keys`, groups them under `identities`,
    /// and places them in `rooms`.
    pub fn with_keys(keys: Keys, identities: Identities, rooms: Rooms) -> Recorder {
        Recorder {
            world: Map {
                rooms,
                ..Map::with_keys(keys, identities)
            },
            next_save: 0.0,
        }
    }

    /// One pass over the lines read at `now`. Returns the lines to print, and
    /// whether the map is due a save. The first pass always saves.
    pub fn feed(&mut self, lines: &[String], now: f64) -> (Vec<String>, bool) {
        self.world.clock = now;
        let mut printed = Vec::new();
        for text in lines {
            match parse_line(text) {
                Some(line) => printed.extend(handle(&mut self.world, &line, now, false)),
                None if text.starts_with('@') => printed.push(text.clone()),
                None => {}
            }
        }
        (printed, self.due(now))
    }
}

impl Recorder {
    /// What the host's adapter heard at `now`. Returns whether the map is due a save.
    pub fn feed_local(&mut self, batch: &Batch, now: f64) -> bool {
        self.world.clock = now;
        self.world.local(batch, now);
        self.due(now)
    }

    fn due(&mut self, now: f64) -> bool {
        let save = now >= self.next_save;
        if save {
            self.next_save = now + SAVE_EVERY_S;
        }
        save
    }
}

fn kind_name(kind: Option<Kind>) -> &'static str {
    match kind {
        Some(Kind::BlePublic) => "ble-public",
        Some(Kind::BleRandom) => "ble-random",
        Some(Kind::Classic) => "classic",
        None => "unknown",
    }
}

fn reason_name(reason: Option<Reason>) -> &'static str {
    match reason {
        Some(Reason::BeaconTimeout) => "beacon timeout",
        Some(Reason::FailedAcks) => "failed acks",
        Some(Reason::BetterPath) => "better path",
        None => "unknown",
    }
}

fn name_line(node: &str, named: &Named) -> String {
    format!(
        "name {node} {} {} {}",
        addr_text(&named.addr),
        kind_name(named.kind),
        py_repr(&named.name)
    )
}

fn identity_line(node: &str, identity: &Identity) -> String {
    format!(
        "identity {node} product {} running {} floor {}",
        identity.product, identity.running, identity.floor
    )
}

fn route_line(node: &str, change: &ParentChange) -> String {
    let parent = |id: Option<u32>| id.map_or("none".to_string(), |id| format!("{id:08x}"));
    format!(
        "route {node} up {} s from {} to {} reason {}",
        change.uptime_s,
        parent(change.old),
        parent(change.new),
        reason_name(change.reason)
    )
}

fn stats_line(node: &str, s: &Stats) -> String {
    format!(
        "node {node} rank {} up {} s heap {} low {} block {} adverts {} classic {} \
         inq {}/{}/{}/{} tx {}/{} bad {} drops {}+{}",
        s.rank,
        s.uptime_s,
        s.heap_free,
        s.heap_min,
        s.heap_block,
        s.adverts,
        s.classic,
        s.inquiries,
        s.inquiry_started,
        s.inquiry_stopped,
        s.inquiry_errors,
        s.tx_ok,
        s.tx_failed,
        s.rx_bad,
        s.queue_drops,
        s.rx_drops
    )
}

fn addr_text(addr: &[u8; 6]) -> String {
    addr.iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Python's `round()` on a float: halves go to the even neighbour. Rust's
/// `round()` sends them away from zero, so `2.5` would give 3, not 2.
fn py_round(x: f64) -> i64 {
    if (x - x.trunc()).abs() == 0.5 {
        (2.0 * (x / 2.0).round()) as i64
    } else {
        x.round() as i64
    }
}

/// Python's `repr()` of a string, for the names `mesh_host.py` prints.
fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tallow_mesh_core::stats::Route;

    /// The same instant as `FIXED_NOW` in `tools/tests/test_map_fixture.py`.
    const FIXED_NOW: f64 = 1_798_761_600.5;
    const LINES: &str = include_str!("../tests/fixtures/map/map_lines.txt");
    const MAP: &str = include_str!("../tests/fixtures/map/map.json");

    fn fed(text: &str, now: f64) -> Map {
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        let mut recorder = Recorder::default();
        recorder.feed(&lines, now);
        recorder.world
    }

    fn sighting(addr: u8, kind: Kind, rssi: i8, count: u8, age_s: u8) -> Sighting {
        Sighting {
            addr: [addr; 6],
            kind: Some(kind),
            rssi,
            count,
            age_s,
        }
    }

    #[test]
    fn the_map_from_the_fixture_is_the_map_mesh_host_wrote() {
        let expected: Value = serde_json::from_str(MAP).expect("fixture map");
        assert_eq!(fed(LINES, FIXED_NOW).to_json(), expected);
    }

    #[test]
    fn the_printed_lines_for_the_fixture_are_what_mesh_host_prints() {
        // Captured from `handle` in tools/mesh_host.py with quiet off, on the same lines.
        let want = [
            "name 541e99a9 23:00:00:11:A0:38 classic 'Klipsch Soundbar'",
            "name 541e99a9 C0:11:22:33:44:AA ble-random 'WF-1000XM5'",
            "node 541e99a9 rank 0 up 600 s heap 151234 low 120000 block 65536 adverts 5000 classic 0 inq 3/0/0/0 tx 900/4 bad 0 drops 0+0",
            "node 2c7f0b11 rank 1 up 312 s heap 140000 low 118000 block 60000 adverts 2600 classic 0 inq 2/0/0/0 tx 400/1 bad 0 drops 0+0",
            "route 2c7f0b11 up 77 s from 0a0b0c0d to 01020304 reason failed acks",
            "@P 00000000",
            "@B 642104ca 3594854591",
            "@U 2 zz",
            "@U 2 2c7f0b11 1 9 1 -60 zz",
        ];
        let lines: Vec<String> = LINES.lines().map(str::to_string).collect();
        let (printed, _) = Recorder::default().feed(&lines, FIXED_NOW);
        assert_eq!(printed, want);
    }

    #[test]
    fn a_tie_goes_to_the_node_heard_first() {
        let mut world = Map::default();
        world.sighting(
            "bbbbbbbb",
            &sighting(1, Kind::Classic, -60, 1, 0),
            FIXED_NOW,
        );
        world.sighting(
            "aaaaaaaa",
            &sighting(1, Kind::Classic, -60, 1, 0),
            FIXED_NOW,
        );
        let json = world.to_json();
        assert_eq!(
            json["devices"]["01:01:01:01:01:01 classic"]["nearest"],
            "bbbbbbbb"
        );
    }

    #[test]
    fn the_strongest_node_is_the_nearest_whatever_order_it_was_heard_in() {
        let mut world = Map::default();
        world.sighting(
            "aaaaaaaa",
            &sighting(1, Kind::Classic, -80, 1, 0),
            FIXED_NOW,
        );
        world.sighting(
            "bbbbbbbb",
            &sighting(1, Kind::Classic, -50, 1, 0),
            FIXED_NOW,
        );
        let json = world.to_json();
        assert_eq!(
            json["devices"]["01:01:01:01:01:01 classic"]["nearest"],
            "bbbbbbbb"
        );
    }

    #[test]
    fn python_rounds_halves_to_even_and_rust_helper_matches() {
        let cases = [
            (0.5, 0),
            (1.5, 2),
            (2.5, 2),
            (3.5, 4),
            (-2.5, -2),
            (2.4, 2),
            (2.6, 3),
            (1_798_761_596.5, 1_798_761_596),
            (1_798_761_600.5, 1_798_761_600),
        ];
        for (x, want) in cases {
            assert_eq!(py_round(x), want, "round({x})");
        }
    }

    #[test]
    fn an_identity_is_kept_when_stats_arrive_and_is_written_on_the_node() {
        let identity = Identity {
            product: 2,
            running: 4,
            floor: 3,
        };
        let line = Line {
            frame: u32::from(FrameKind::Identity as u8),
            origin: "00000005".into(),
            boot: 0,
            seq: 0,
            hops: 1,
            rssi: -40,
            payload: identity.encode().to_vec(),
        };
        let mut world = Map::default();
        assert_eq!(
            handle(&mut world, &line, FIXED_NOW, false),
            ["identity 00000005 product 2 running 4 floor 3"]
        );
        let stats = Stats {
            uptime_s: 9,
            ..Stats::default()
        };
        world.stats("00000005", stats, 2, -50, FIXED_NOW);
        let node = world.to_json()["nodes"]["00000005"].clone();
        assert_eq!(node["product"].as_u64(), Some(2));
        assert_eq!(node["running"].as_u64(), Some(4));
        assert_eq!(node["floor"].as_u64(), Some(3));
        assert_eq!(node["uptime_s"].as_u64(), Some(9));
        assert_eq!(node["hops"].as_u64(), Some(2));
    }

    #[test]
    fn a_route_line_reads_as_mesh_host_prints_it() {
        let payload = Route {
            uptime_s: 77,
            old: 0x0A0B_0C0D,
            new: 0x0102_0304,
            reason: 1,
        }
        .encode();
        let line = Line {
            frame: 7,
            origin: "00000005".into(),
            boot: 0,
            seq: 0,
            hops: 2,
            rssi: -60,
            payload: payload.to_vec(),
        };
        assert_eq!(
            handle(&mut Map::default(), &line, FIXED_NOW, false),
            ["route 00000005 up 77 s from 0a0b0c0d to 01020304 reason failed acks"]
        );
    }

    #[test]
    fn a_quiet_pass_prints_nothing_and_still_keeps_the_names() {
        let mut world = Map::default();
        let mut payload = vec![2u8; 6];
        payload.extend_from_slice(&[2, 0xFF, 0xFF, 0, 0, 0, 3]);
        payload.extend_from_slice(b"abc");
        let line = Line {
            frame: 3,
            origin: "00000005".into(),
            boot: 0,
            seq: 0,
            hops: 1,
            rssi: -60,
            payload,
        };
        assert!(handle(&mut world, &line, FIXED_NOW, true).is_empty());
        assert_eq!(world.counts(), (1, 0));
    }

    #[test]
    fn a_name_line_quotes_the_name_as_python_does() {
        assert_eq!(py_repr("Flip 5"), "'Flip 5'");
        assert_eq!(py_repr("it's"), "\"it's\"");
        assert_eq!(py_repr("a\\b\n"), "'a\\\\b\\n'");
    }

    #[test]
    fn the_map_is_saved_on_the_first_pass_and_every_five_seconds() {
        let mut recorder = Recorder::default();
        let saves: Vec<bool> = [100.0, 104.9, 105.0, 109.99, 110.0]
            .iter()
            .map(|&now| recorder.feed(&[], now).1)
            .collect();
        assert_eq!(saves, [true, false, true, false, true]);
    }

    #[test]
    fn a_gatt_answer_is_not_counted_as_a_device() {
        let mut world = Map::default();
        let line = Line {
            frame: u32::from(FrameKind::GattUp as u8),
            origin: "00000005".into(),
            boot: 1,
            seq: 1,
            hops: 1,
            rssi: -40,
            payload: vec![0x03, 0x00, 0x01, 0x00, 0xde, 0xad],
        };
        assert!(handle(&mut world, &line, FIXED_NOW, false).is_empty());
        assert_eq!(world.counts(), (0, 0));
    }

    #[test]
    fn noise_is_dropped_and_other_at_lines_are_printed() {
        let mut recorder = Recorder::default();
        let lines = [
            "W (735) wifi:Password".to_string(),
            "@P 00000000".to_string(),
            "@U 2 zz".to_string(),
        ];
        let (printed, _) = recorder.feed(&lines, FIXED_NOW);
        assert_eq!(printed, ["@P 00000000", "@U 2 zz"]);
    }

    #[test]
    fn the_saved_file_parses_back_to_the_same_map_and_leaves_no_temporary() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tallowd-map-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("map.json");
        let world = fed(LINES, FIXED_NOW);
        world.save(&path).expect("save");
        let saved: Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("parse");
        assert_eq!(saved, world.to_json());
        assert!(!dir.join("map.json.tmp").exists());
        let mode = fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the map holds device addresses");
        fs::remove_dir_all(&dir).expect("clean up");
    }

    /// A made-up IRK. It is not any device's key.
    const MADE_UP_IRK: &str = "6d6164652d75702d69726b2d30303031";

    /// Keys loaded from a 0600 file in a directory of their own, named for `test`.
    fn keys_in_a_file(test: &str, text: &str) -> Keys {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tallowd-map-{test}-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("irks.toml");
        fs::write(&path, text).expect("write keys");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod keys");
        let keys = Keys::load(&path).expect("keys load");
        fs::remove_dir_all(&dir).expect("clean up");
        keys
    }

    fn random_sighting(addr: [u8; 6]) -> Sighting {
        Sighting {
            addr,
            kind: Some(Kind::BleRandom),
            rssi: -60,
            count: 1,
            age_s: 0,
        }
    }

    #[test]
    fn two_addresses_from_one_irk_are_both_named_for_the_one_device() {
        let keys = keys_in_a_file(
            "two-addresses",
            &format!("\"Earbuds\" = \"{MADE_UP_IRK}\"\n"),
        );
        let irk = crate::irk::parse_irk(MADE_UP_IRK).expect("made-up key");
        let first = {
            let hash = crate::irk::ah(&irk, &[0x41, 0x22, 0x13]);
            [0x41, 0x22, 0x13, hash[0], hash[1], hash[2]]
        };
        let second = {
            let hash = crate::irk::ah(&irk, &[0x5c, 0x9e, 0x07]);
            [0x5c, 0x9e, 0x07, hash[0], hash[1], hash[2]]
        };
        let mut world = Map::with_keys(keys, Identities::default());
        world.sighting("aaaaaaaa", &random_sighting(first), FIXED_NOW);
        world.sighting("bbbbbbbb", &random_sighting(second), FIXED_NOW);
        let json = world.to_json();
        for addr in [first, second] {
            let key = format!("{} ble-random", addr_text(&addr));
            assert_eq!(json["devices"][key.as_str()]["resolved"], "Earbuds");
        }
        assert!(!json.to_string().contains(MADE_UP_IRK));
    }

    #[test]
    fn an_address_no_key_resolves_stays_unnamed_and_a_classic_one_is_never_tried() {
        let keys = keys_in_a_file("no-match", &format!("\"Earbuds\" = \"{MADE_UP_IRK}\"\n"));
        let mut world = Map::with_keys(keys, Identities::default());
        world.sighting(
            "aaaaaaaa",
            &random_sighting([0x41, 0x22, 0x13, 0x00, 0x00, 0x00]),
            FIXED_NOW,
        );
        world.sighting(
            "aaaaaaaa",
            &Sighting {
                addr: [0x41, 0x22, 0x13, 0x00, 0x00, 0x00],
                kind: Some(Kind::Classic),
                rssi: -60,
                count: 1,
                age_s: 0,
            },
            FIXED_NOW,
        );
        let json = world.to_json();
        assert!(json["devices"]["41:22:13:00:00:00 ble-random"]
            .get("resolved")
            .is_none());
        assert!(json["devices"]["41:22:13:00:00:00 classic"]
            .get("resolved")
            .is_none());
    }

    fn sighting_of(addr: [u8; 6], kind: Kind) -> Sighting {
        Sighting {
            addr,
            kind: Some(kind),
            rssi: -60,
            count: 1,
            age_s: 0,
        }
    }

    /// The soundbar's Classic address and its BLE address, from the plan.
    const KLIPSCH_CLASSIC: [u8; 6] = [0x23, 0x00, 0x00, 0x11, 0xA0, 0x38];
    const KLIPSCH_BLE: [u8; 6] = [0xDC, 0x00, 0x00, 0x11, 0xA0, 0x38];
    const KLIPSCH_CLASSIC_KEY: &str = "23:00:00:11:A0:38 classic";
    const KLIPSCH_BLE_KEY: &str = "DC:00:00:11:A0:38 ble-public";

    fn klipsch_map(identities: Identities) -> Map {
        let mut world = Map::with_keys(Keys::default(), identities);
        world.sighting(
            "aaaaaaaa",
            &sighting_of(KLIPSCH_CLASSIC, Kind::Classic),
            FIXED_NOW,
        );
        world.sighting(
            "aaaaaaaa",
            &sighting_of(KLIPSCH_BLE, Kind::BlePublic),
            FIXED_NOW,
        );
        world
    }

    #[test]
    fn the_soundbars_two_addresses_are_suggested_together_without_a_devices_file() {
        let json = klipsch_map(Identities::default()).to_json();
        assert_eq!(
            json["suggestions"],
            json!([{
                "keys": [KLIPSCH_CLASSIC_KEY, KLIPSCH_BLE_KEY],
                "reason": "same low 4 address bytes 00:11:A0:38",
            }])
        );
        assert!(json.get("identities").is_none());
        assert_eq!(
            klipsch_map(Identities::default()).suggestion_lines(),
            [format!(
                "suggest {KLIPSCH_CLASSIC_KEY}, {KLIPSCH_BLE_KEY} (same low 4 address bytes 00:11:A0:38)"
            )]
        );
    }

    #[test]
    fn the_soundbars_two_addresses_are_grouped_once_when_the_devices_file_names_both() {
        let identities = Identities::parse(
            "[[device]]\nname = \"Klipsch soundbar\"\ntype = \"speaker\"\naddresses = [\"23:00:00:11:A0:38\", \"DC:00:00:11:A0:38\"]\n",
        )
        .expect("devices");
        let json = klipsch_map(identities).to_json();
        assert_eq!(
            json["identities"],
            json!({
                "Klipsch soundbar": {
                    "type": "speaker",
                    "keys": [KLIPSCH_CLASSIC_KEY, KLIPSCH_BLE_KEY],
                }
            })
        );
        assert!(json.get("suggestions").is_none());
    }

    #[test]
    fn a_named_device_with_no_sightings_is_still_listed_with_no_keys() {
        let identities = Identities::parse(
            "[[device]]\nname = \"Lamp\"\ntype = \"light\"\naddresses = [\"AA:AA:AA:AA:AA:AA\"]\n",
        )
        .expect("devices");
        let json = Map::with_keys(Keys::default(), identities).to_json();
        assert_eq!(
            json["identities"],
            json!({ "Lamp": { "type": "light", "keys": [] } })
        );
    }

    #[test]
    fn a_rotating_random_address_does_not_suggest_itself_by_its_low_bytes() {
        let mut world = Map::default();
        world.sighting(
            "aaaaaaaa",
            &sighting_of(KLIPSCH_CLASSIC, Kind::Classic),
            FIXED_NOW,
        );
        world.sighting(
            "aaaaaaaa",
            &sighting_of([0x41, 0x22, 0x13, 0x11, 0xA0, 0x38], Kind::BleRandom),
            FIXED_NOW,
        );
        assert!(world.to_json().get("suggestions").is_none());
    }

    #[test]
    fn two_devices_with_one_advertised_name_are_suggested_by_that_name() {
        let mut world = Map::default();
        for (addr, kind) in [
            ([0xC0, 0x11, 0x22, 0x33, 0x44, 0xAA], Kind::BlePublic),
            ([0x41, 0x22, 0x13, 0x55, 0x66, 0x77], Kind::BleRandom),
        ] {
            world.sighting("aaaaaaaa", &sighting_of(addr, kind), FIXED_NOW);
            world.name(&Named {
                addr,
                kind: Some(kind),
                company: None,
                class: 0,
                name: "Flip 5".into(),
            });
        }
        let json = world.to_json();
        assert_eq!(
            json["suggestions"],
            json!([{
                "keys": ["41:22:13:55:66:77 ble-random", "C0:11:22:33:44:AA ble-public"],
                "reason": "same name 'Flip 5'",
            }])
        );
    }

    #[test]
    fn a_resolved_name_groups_its_addresses_under_the_device_of_that_name() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("tallowd-map-resolved-group-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let keys_path = dir.join("irks.toml");
        fs::write(&keys_path, format!("\"Earbuds\" = \"{MADE_UP_IRK}\"\n")).expect("keys");
        fs::set_permissions(&keys_path, fs::Permissions::from_mode(0o600)).expect("chmod");
        let keys = Keys::load(&keys_path).expect("keys load");
        fs::remove_dir_all(&dir).expect("clean up");
        let irk = crate::irk::parse_irk(MADE_UP_IRK).expect("made-up key");
        let resolvable = |prand: [u8; 3]| {
            let hash = crate::irk::ah(&irk, &prand);
            [prand[0], prand[1], prand[2], hash[0], hash[1], hash[2]]
        };
        let identities = Identities::parse(
            "[[device]]\nname = \"Earbuds\"\ntype = \"earbuds\"\naddresses = []\n",
        )
        .expect("devices");
        let mut world = Map::with_keys(keys, identities);
        world.sighting(
            "aaaaaaaa",
            &random_sighting(resolvable([0x41, 0x22, 0x13])),
            FIXED_NOW,
        );
        world.sighting(
            "bbbbbbbb",
            &random_sighting(resolvable([0x5C, 0x9E, 0x07])),
            FIXED_NOW,
        );
        let json = world.to_json();
        let mut want: Vec<String> = [[0x41, 0x22, 0x13], [0x5C, 0x9E, 0x07]]
            .iter()
            .map(|&prand| format!("{} ble-random", addr_text(&resolvable(prand))))
            .collect();
        want.sort();
        assert_eq!(json["identities"]["Earbuds"]["keys"], json!(want));
        assert_eq!(json["identities"]["Earbuds"]["type"], "earbuds");
        assert!(json.get("suggestions").is_none());
        assert!(!json.to_string().contains(MADE_UP_IRK));
    }

    const ROOMS: &str = "[nodes]\n541e99a9 = \"Kitchen\"\n2c7f0b11 = \"Hall\"\n";
    const SOUNDBAR_DEVICES: &str = "[[device]]\nname = \"Klipsch soundbar\"\ntype = \"speaker\"\naddresses = [\"23:00:00:11:A0:38\", \"DC:00:00:11:A0:38\"]\n";

    fn room_in(json: &Value, key: &str) -> Option<String> {
        json["devices"][key]
            .get("room")
            .map(|room| room.as_str().expect("room is text").to_string())
    }

    #[test]
    fn a_device_under_one_identity_has_one_room_from_both_its_addresses() {
        let identities = Identities::parse(SOUNDBAR_DEVICES).expect("devices");
        let rooms = Rooms::parse(ROOMS).expect("rooms");
        let mut world = Map {
            rooms,
            ..Map::with_keys(Keys::default(), identities)
        };
        world.clock = FIXED_NOW;
        world.sighting(
            "541e99a9",
            &sighting_of(KLIPSCH_CLASSIC, Kind::Classic),
            FIXED_NOW,
        );
        assert_eq!(
            room_in(&world.to_json(), KLIPSCH_CLASSIC_KEY).as_deref(),
            Some("Kitchen")
        );
        // The BLE address is heard in the hall three times in a row, and the
        // move is for the whole soundbar, so the classic address moves too.
        for step in 1..=3 {
            let now = FIXED_NOW + f64::from(step);
            world.clock = now;
            let louder = Sighting {
                rssi: -50,
                ..sighting_of(KLIPSCH_BLE, Kind::BlePublic)
            };
            world.sighting("2c7f0b11", &louder, now);
        }
        let json = world.to_json();
        assert_eq!(room_in(&json, KLIPSCH_CLASSIC_KEY).as_deref(), Some("Hall"));
        assert_eq!(room_in(&json, KLIPSCH_BLE_KEY).as_deref(), Some("Hall"));
    }

    #[test]
    fn a_device_heard_only_by_a_node_with_no_room_has_no_room_key() {
        let rooms = Rooms::parse(ROOMS).expect("rooms");
        let mut world = Map {
            rooms,
            ..Map::with_keys(Keys::default(), Identities::default())
        };
        world.clock = FIXED_NOW;
        world.sighting(
            "aaaaaaaa",
            &sighting_of(KLIPSCH_CLASSIC, Kind::Classic),
            FIXED_NOW,
        );
        let json = world.to_json();
        assert!(json["devices"][KLIPSCH_CLASSIC_KEY].get("room").is_none());
    }
}
