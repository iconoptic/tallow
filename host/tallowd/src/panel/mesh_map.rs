//! The mesh map, read for the panel's `/devices` page. Read only.
//!
//! `tallowd run` and `tools/mesh.sh host` write the map file as separate processes, and
//! this reads it. A missing, unreadable, malformed, oversized, or old file is not an
//! error to the page: it is a stale document with a short reason code. The path is fixed
//! at startup, `data/mesh/map.json` for the real panel and `mesh-map.json` in the data
//! directory for a preview. No request supplies any part of it.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use crate::cluster::{self, Clusters};

/// A map whose modification time is older than this is not the live map.
pub const STALE_AFTER: Duration = Duration::from_secs(60);
/// The most the panel reads. The live map is a few kilobytes.
pub const MAP_MAX: u64 = 4 * 1024 * 1024;
/// A file dated this far ahead of the clock is taken as now, since some filesystems round
/// modification times. Further ahead than this, the file is stale.
const SKEW: Duration = Duration::from_secs(5);

/// Why the map is not shown. Each code is one the page turns into words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stale {
    Missing,
    Unreadable,
    Malformed,
    TooLarge,
    Old(u64),
    Future,
}

impl Stale {
    fn code(self) -> &'static str {
        match self {
            Stale::Missing => "missing",
            Stale::Unreadable => "unreadable",
            Stale::Malformed => "malformed",
            Stale::TooLarge => "too large",
            Stale::Old(_) => "old",
            Stale::Future => "future",
        }
    }
}

/// The document `GET /api/mesh/map` answers with. A live map has `stale: false`, its age,
/// and its four parts. Anything else has `stale: true` and a reason, and `age_s` when the
/// reason is `old`. `path` is `None` when the panel has no map path, which counts as missing.
pub fn document(path: Option<&Path>, now: SystemTime) -> Value {
    let stamp = epoch_seconds(now);
    let loaded = path.map_or(Err(Stale::Missing), |path| load(path, now));
    match loaded {
        Ok((age_s, mut map)) => {
            if let Some(path) = path {
                annotate(path, &mut map);
            }
            json!({
                "stale": false,
                "now": stamp,
                "age_s": age_s,
                "nodes": map.get("nodes").cloned().unwrap_or_else(|| json!({})),
                "devices": map.get("devices").cloned().unwrap_or_else(|| json!({})),
                "identities": map.get("identities").cloned().unwrap_or_else(|| json!({})),
                "suggestions": map.get("suggestions").cloned().unwrap_or_else(|| json!([])),
            })
        }
        Err(stale) => {
            let mut out = json!({"stale": true, "reason": stale.code(), "now": stamp});
            if let Stale::Old(age_s) = stale {
                out["age_s"] = json!(age_s);
            }
            out
        }
    }
}

/// The file's age in whole seconds and its object, or why it is stale.
fn load(path: &Path, now: SystemTime) -> Result<(u64, Map<String, Value>), Stale> {
    let file = File::open(path).map_err(|error| match error.kind() {
        ErrorKind::NotFound => Stale::Missing,
        _ => Stale::Unreadable,
    })?;
    let meta = file.metadata().map_err(|_| Stale::Unreadable)?;
    if !meta.is_file() {
        return Err(Stale::Unreadable);
    }
    let modified = meta.modified().map_err(|_| Stale::Unreadable)?;
    let age = match now.duration_since(modified) {
        Ok(age) => age,
        Err(ahead) if ahead.duration() <= SKEW => Duration::ZERO,
        Err(_) => return Err(Stale::Future),
    };
    if age > STALE_AFTER {
        return Err(Stale::Old(age.as_secs()));
    }
    let mut text = String::new();
    file.take(MAP_MAX + 1)
        .read_to_string(&mut text)
        .map_err(|error| match error.kind() {
            ErrorKind::InvalidData => Stale::Malformed,
            _ => Stale::Unreadable,
        })?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAP_MAX {
        return Err(Stale::TooLarge);
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map))
            if map.get("nodes").is_some_and(Value::is_object)
                && map.get("devices").is_some_and(Value::is_object) =>
        {
            Ok((age.as_secs(), map))
        }
        _ => Err(Stale::Malformed),
    }
}

fn epoch_seconds(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// `label` is the name to show. `cluster` and `members` are set once a join is stored.
fn annotate(path: &Path, map: &mut Map<String, Value>) {
    let clusters = cluster::load_path(&path.with_file_name("traffic.db"));
    let identity_of = identity_of(map.get("identities"));
    let Some(devices) = map.get("devices").and_then(Value::as_object).cloned() else {
        return;
    };
    let stamps = stamps(&devices, &clusters, &identity_of);
    let Some(live) = map.get_mut("devices").and_then(Value::as_object_mut) else {
        return;
    };
    for (key, stamp) in stamps {
        let Some(device) = live.get_mut(&key).and_then(Value::as_object_mut) else {
            continue;
        };
        device.insert("label".into(), json!(stamp.label));
        if let Some(cluster) = stamp.cluster {
            device.insert("cluster".into(), json!(cluster));
            device.insert("members".into(), json!(stamp.members));
        }
    }
}

struct Stamp {
    label: String,
    cluster: Option<String>,
    members: Vec<String>,
}

fn stamps(
    devices: &Map<String, Value>,
    clusters: &Clusters,
    identity_of: &BTreeMap<String, String>,
) -> Vec<(String, Stamp)> {
    devices
        .iter()
        .map(|(key, device)| {
            let (addr, kind) = addr_kind(key, device);
            let stamp = match clusters.group(addr, kind) {
                Some(group) => Stamp {
                    label: cluster_label(group, devices, identity_of),
                    cluster: Some(group.seed.clone()),
                    members: group.members.clone(),
                },
                None => Stamp {
                    label: device_label(addr, kind, device, identity_of),
                    cluster: None,
                    members: Vec::new(),
                },
            };
            (key.clone(), stamp)
        })
        .collect()
}

fn identity_of(value: Option<&Value>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(identities) = value.and_then(Value::as_object) else {
        return out;
    };
    for (name, identity) in identities {
        let Some(keys) = identity.get("keys").and_then(Value::as_array) else {
            continue;
        };
        for key in keys {
            let Some(key) = key.as_str() else {
                continue;
            };
            out.entry(key.to_string()).or_insert_with(|| name.clone());
        }
    }
    out
}

fn cluster_label(
    group: &cluster::Group,
    devices: &Map<String, Value>,
    identity_of: &BTreeMap<String, String>,
) -> String {
    let mut members = group.members.clone();
    members.sort();
    if let Some(index) = members.iter().position(|addr| addr == &group.seed) {
        members.swap(0, index);
    }
    let mut identity = None;
    let mut resolved = None;
    let mut air = None;
    for addr in &members {
        let key = format!("{addr} {}", group.kind);
        if let Some(name) = identity_of.get(&key) {
            if identity.is_none() {
                identity = Some(name.clone());
            }
        }
        if let Some(device) = devices.get(&key) {
            if resolved.is_none() {
                resolved = text_field(device, "resolved");
            }
            if air.is_none() {
                air = text_field(device, "name");
            }
        }
    }
    identity
        .or(resolved)
        .or(air)
        .unwrap_or_else(|| cluster::cute_name(&group.seed))
}

fn device_label(
    addr: &str,
    kind: &str,
    device: &Value,
    identity_of: &BTreeMap<String, String>,
) -> String {
    identity_of
        .get(&format!("{addr} {kind}"))
        .cloned()
        .or_else(|| text_field(device, "resolved"))
        .or_else(|| text_field(device, "name"))
        .unwrap_or_else(|| cluster::cute_name(addr))
}

fn text_field(device: &Value, name: &str) -> Option<String> {
    device
        .get(name)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn addr_kind<'a>(key: &'a str, device: &'a Value) -> (&'a str, &'a str) {
    let addr = device
        .get("addr")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty());
    let kind = device
        .get("kind")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty());
    match (addr, kind) {
        (Some(addr), Some(kind)) => (addr, kind),
        _ => key.rsplit_once(' ').unwrap_or((key, "")),
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::*;
    use crate::library::testing::TempDir;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    const NOW: u64 = 1_000_000;

    fn a_map() -> String {
        json!({
            "nodes": {"0a0b0c0d": {"rank": 0, "parent": 0}},
            "devices": {"41:22:13:00:00:01 ble-random": {"kind": "ble-random", "heard_by": {}}},
        })
        .to_string()
    }

    /// Writes `bytes` and sets the file's modification time, so the age is the test's own.
    fn write_at(path: &Path, bytes: &[u8], modified: SystemTime) {
        std::fs::write(path, bytes).expect("write the map");
        File::options()
            .write(true)
            .open(path)
            .expect("open the map")
            .set_modified(modified)
            .expect("set the modification time");
    }

    fn reason(doc: &Value) -> &str {
        doc["reason"].as_str().unwrap_or("")
    }

    #[test]
    fn a_missing_map_is_stale_and_says_missing() {
        let dir = TempDir::new("mesh-map-missing");
        let doc = document(Some(&dir.path().join("map.json")), at(NOW));
        assert_eq!(doc, json!({"stale": true, "reason": "missing", "now": NOW}));
    }

    #[test]
    fn a_panel_with_no_map_path_has_no_map_and_says_missing() {
        assert_eq!(reason(&document(None, at(NOW))), "missing");
    }

    #[test]
    fn a_fresh_map_is_shown_with_its_nodes_and_devices() {
        let dir = TempDir::new("mesh-map-fresh");
        let path = dir.path().join("map.json");
        write_at(&path, a_map().as_bytes(), at(NOW));
        let doc = document(Some(&path), at(NOW));
        assert_eq!(doc["stale"], json!(false));
        assert_eq!(doc["age_s"], json!(0));
        assert_eq!(doc["now"], json!(NOW));
        assert_eq!(doc["nodes"]["0a0b0c0d"]["parent"], json!(0));
        assert_eq!(
            doc["devices"]["41:22:13:00:00:01 ble-random"]["kind"],
            json!("ble-random")
        );
        assert_eq!(doc["identities"], json!({}));
        assert_eq!(doc["suggestions"], json!([]));
    }

    #[test]
    fn a_map_a_minute_old_is_live_and_one_older_is_stale_with_its_age() {
        let dir = TempDir::new("mesh-map-age");
        let path = dir.path().join("map.json");
        write_at(&path, a_map().as_bytes(), at(NOW - 60));
        assert_eq!(document(Some(&path), at(NOW))["stale"], json!(false));
        write_at(&path, a_map().as_bytes(), at(NOW - 61));
        let doc = document(Some(&path), at(NOW));
        assert_eq!(doc["reason"], json!("old"));
        assert_eq!(doc["age_s"], json!(61));
        assert!(doc.get("nodes").is_none());
    }

    #[test]
    fn a_map_dated_a_little_ahead_is_live_and_one_dated_well_ahead_is_stale() {
        let dir = TempDir::new("mesh-map-future");
        let path = dir.path().join("map.json");
        write_at(&path, a_map().as_bytes(), at(NOW + 2));
        assert_eq!(document(Some(&path), at(NOW))["stale"], json!(false));
        write_at(&path, a_map().as_bytes(), at(NOW + 30));
        assert_eq!(reason(&document(Some(&path), at(NOW))), "future");
    }

    #[test]
    fn a_map_that_is_not_json_or_not_a_map_is_malformed_and_never_an_error() {
        let dir = TempDir::new("mesh-map-malformed");
        let path = dir.path().join("map.json");
        for text in [
            "{",
            "[]",
            "\"nodes\"",
            "{\"nodes\":{}}",
            "{\"nodes\":[],\"devices\":{}}",
        ] {
            write_at(&path, text.as_bytes(), at(NOW));
            let doc = document(Some(&path), at(NOW));
            assert_eq!(reason(&doc), "malformed", "for {text:?}");
            assert_eq!(doc["stale"], json!(true));
        }
    }

    #[test]
    fn a_map_that_is_not_utf8_is_malformed() {
        let dir = TempDir::new("mesh-map-utf8");
        let path = dir.path().join("map.json");
        write_at(&path, &[0xff, 0xfe, 0x00], at(NOW));
        assert_eq!(reason(&document(Some(&path), at(NOW))), "malformed");
    }

    #[test]
    fn a_map_over_four_mebibytes_is_refused_before_it_is_parsed() {
        let dir = TempDir::new("mesh-map-large");
        let path = dir.path().join("map.json");
        // Whitespace is not JSON, so only the size check can make this "too large".
        let big = vec![b' '; usize::try_from(MAP_MAX + 1).expect("a usize")];
        write_at(&path, &big, at(NOW));
        assert_eq!(reason(&document(Some(&path), at(NOW))), "too large");
    }

    #[test]
    fn a_map_of_exactly_four_mebibytes_is_still_read() {
        let dir = TempDir::new("mesh-map-edge");
        let path = dir.path().join("map.json");
        let mut text = a_map().into_bytes();
        let pad = usize::try_from(MAP_MAX).expect("a usize") - text.len();
        text.extend(std::iter::repeat_n(b' ', pad));
        write_at(&path, &text, at(NOW));
        assert_eq!(document(Some(&path), at(NOW))["stale"], json!(false));
    }

    #[test]
    fn a_directory_where_the_map_should_be_is_unreadable() {
        let dir = TempDir::new("mesh-map-dir");
        let path = dir.path().join("map.json");
        std::fs::create_dir(&path).expect("make a directory");
        assert_eq!(reason(&document(Some(&path), at(NOW))), "unreadable");
    }

    #[test]
    fn a_name_from_the_air_comes_back_as_the_same_text() {
        let dir = TempDir::new("mesh-map-name");
        let path = dir.path().join("map.json");
        let name = "<img src=x onerror=alert(1)>";
        let text = json!({
            "nodes": {},
            "devices": {"AA:BB:CC:DD:EE:01 classic": {"kind": "classic", "name": name, "heard_by": {}}},
        });
        write_at(&path, text.to_string().as_bytes(), at(NOW));
        let doc = document(Some(&path), at(NOW));
        assert_eq!(
            doc["devices"]["AA:BB:CC:DD:EE:01 classic"]["name"],
            json!(name)
        );
        assert_eq!(
            doc["devices"]["AA:BB:CC:DD:EE:01 classic"]["label"],
            json!(name)
        );
    }

    #[test]
    fn a_nameless_device_is_labeled_from_its_address() {
        let dir = TempDir::new("mesh-map-cute");
        let path = dir.path().join("map.json");
        write_at(&path, a_map().as_bytes(), at(NOW));
        let doc = document(Some(&path), at(NOW));
        let device = &doc["devices"]["41:22:13:00:00:01 ble-random"];
        assert_eq!(
            device["label"],
            json!(crate::cluster::cute_name("41:22:13:00:00:01"))
        );
        assert!(device.get("cluster").is_none());
    }

    #[test]
    fn a_long_handoff_labels_both_addresses_as_one_device() {
        let dir = TempDir::new("mesh-map-cluster");
        let db = dir.path().join("traffic.db");
        let mut journal = crate::traffic::Journal::open(&db).unwrap();
        let now = 1_700_000_000.0;
        let older = [0x11, 0x22, 0x33, 0x44, 0x55, 0x01];
        let newer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x02];
        for (addr, at_s) in [
            (older, now),
            (older, now + 15.0 * 60.0 - 60.0),
            (older, now + 15.0 * 60.0),
            (newer, now + 15.0 * 60.0 + 10.0),
            (newer, now + 15.0 * 60.0 + 70.0),
            (newer, now + 31.0 * 60.0),
        ] {
            journal.note_batch(&heard("n1", addr, -60), at_s).unwrap();
        }
        journal.reconcile_clusters().unwrap();
        drop(journal);
        let path = dir.path().join("map.json");
        let text = json!({
            "nodes": {"n1": {"parent": 0}},
            "devices": {
                "11:22:33:44:55:01 ble-random": {
                    "addr": "11:22:33:44:55:01",
                    "kind": "ble-random",
                    "heard_by": {}
                },
                "11:22:33:44:55:02 ble-random": {
                    "addr": "11:22:33:44:55:02",
                    "kind": "ble-random",
                    "heard_by": {}
                },
            }
        });
        write_at(&path, text.to_string().as_bytes(), at(NOW));
        let doc = document(Some(&path), at(NOW));
        let name = crate::cluster::cute_name("11:22:33:44:55:01");
        let older = &doc["devices"]["11:22:33:44:55:01 ble-random"];
        let newer_device = &doc["devices"]["11:22:33:44:55:02 ble-random"];
        assert_eq!(older["label"], json!(name));
        assert_eq!(newer_device["label"], json!(name));
        assert_eq!(older["cluster"], json!("11:22:33:44:55:01"));
        assert_eq!(newer_device["cluster"], json!("11:22:33:44:55:01"));
        assert_eq!(older["members"].as_array().unwrap().len(), 2);
    }

    fn heard(node: &str, addr: [u8; 6], rssi: i8) -> crate::local::Batch {
        crate::local::Batch {
            node: node.to_string(),
            uptime_s: 0,
            sightings: vec![crate::decode::Sighting {
                addr,
                kind: Some(tallow_mesh_core::sightings::Kind::BleRandom),
                rssi,
                count: 1,
                age_s: 0,
            }],
            names: Vec::new(),
        }
    }
}
