//! Saved patterns and schedules for the H7020 page. Port of `app/library.py`.
//!
//! The files are the ones the Python writes: `patterns.json` and `schedules.json`,
//! with a two-space indent, ASCII-only escapes, and a trailing newline. Validation
//! takes the raw JSON the page sends and returns the canonical document, or an
//! error whose text the page shows. The wall clock is an argument. Nothing here
//! reads it.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use chrono::{DateTime, Datelike, FixedOffset, SecondsFormat, TimeDelta, TimeZone, Timelike};
use serde::ser::SerializeMap;
use serde::{de::DeserializeOwned, Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};
use tallow_govee::{scene, Refusal};

const WEEKDAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
const HOLD_MAX: i64 = 24 * 60 * 60;
const STEPS_MAX: usize = 32;
const DUE_WINDOW_HOURS: i64 = 12;

/// What a library call refuses. `Display` is the text the page shows, the same
/// words the Python raises.
#[derive(Debug)]
pub enum Error {
    /// The page's input is wrong. The Python raises ValueError.
    Invalid(String),
    /// A document with this id already exists. The Python raises FileExistsError.
    Exists(String),
    /// No document has this id. The Python raises FileNotFoundError.
    Missing(String),
    /// A file on disk is not a library file.
    Corrupt(String),
    /// The light refused a write.
    Device(String),
    /// Reading or writing a file failed.
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::Corrupt(message) | Self::Device(message) => {
                f.write_str(message)
            }
            Self::Exists(id) | Self::Missing(id) => f.write_str(id),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

fn invalid<T>(message: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(message.into()))
}

/// A codec refusal is the page's input being wrong, so it reads as a ValueError.
pub(crate) fn refusal(refused: Refusal) -> Error {
    Error::Invalid(refused.0)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pattern {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Body {
    Scene { scene: SceneRef },
    Paint { paint: Vec<Group> },
    Sequence { repeat: bool, steps: Vec<Step> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<i64>,
}

/// One colour for some bulbs. Bulb numbers are one-based and sorted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub bulbs: Vec<i64>,
    pub rgb: [i64; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub hold: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint: Option<Vec<Group>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub enabled: bool,
    pub days: Vec<String>,
    pub time: String,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Power(bool),
    Pattern(String),
    Scene(SceneRef),
}

trait Document: Clone + Serialize + DeserializeOwned {
    const KEY: &'static str;
    const FILE: &'static str;
    fn id(&self) -> &str;
}

impl Document for Pattern {
    const KEY: &'static str = "patterns";
    const FILE: &'static str = "patterns.json";
    fn id(&self) -> &str {
        &self.id
    }
}

impl Document for Schedule {
    const KEY: &'static str = "schedules";
    const FILE: &'static str = "schedules.json";
    fn id(&self) -> &str {
        &self.id
    }
}

/// `{key: items}`, written as one object so the key comes out first.
struct Wrapped<'a, T> {
    key: &'static str,
    items: &'a [T],
}

impl<T: Serialize> Serialize for Wrapped<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(self.key, self.items)?;
        map.end()
    }
}

fn to_json<T: Serialize + ?Sized>(value: &T) -> Result<String, Error> {
    serde_json::to_string_pretty(value)
        .map(ascii_escape)
        .map_err(|error| Error::Corrupt(error.to_string()))
}

/// Escapes what Python's `json.dumps` escapes by default: anything past ASCII,
/// and DEL. serde writes non-ASCII text as UTF-8 and does not escape DEL. Those
/// characters only occur inside strings, so the structure stays as it is.
fn ascii_escape(text: String) -> String {
    if text.is_ascii() && !text.contains('\u{7f}') {
        return text;
    }
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii() && ch != '\u{7f}' {
            out.push(ch);
            continue;
        }
        let mut units = [0u16; 2];
        for unit in ch.encode_utf16(&mut units).iter() {
            out.push_str(&format!("\\u{unit:04x}"));
        }
    }
    out
}

fn is_slug(text: &str) -> bool {
    let bytes = text.as_bytes();
    (1..=63).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn two_digits(bytes: &[u8]) -> Option<u32> {
    match bytes {
        [tens, units] if tens.is_ascii_digit() && units.is_ascii_digit() => {
            Some(u32::from(tens - b'0') * 10 + u32::from(units - b'0'))
        }
        _ => None,
    }
}

/// `HH:MM` on a 24-hour clock, the same text the Python regex accepts.
fn parse_clock(text: &str) -> Option<(u32, u32)> {
    let bytes = text.as_bytes();
    if bytes.len() != 5 || bytes[2] != b':' {
        return None;
    }
    let hour = two_digits(&bytes[0..2])?;
    let minute = two_digits(&bytes[3..5])?;
    (hour <= 23 && minute <= 59).then_some((hour, minute))
}

fn unknown(map: &Map<String, Value>, allowed: &[&str]) -> Result<(), Error> {
    let mut extra: Vec<&str> = map
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    if extra.is_empty() {
        return Ok(());
    }
    extra.sort_unstable();
    invalid(format!("unknown fields: {}", extra.join(", ")))
}

fn slug(value: Option<&Value>, label: &str) -> Result<String, Error> {
    match value.and_then(Value::as_str) {
        Some(text) if is_slug(text) => Ok(text.to_owned()),
        _ => invalid(format!("{label} must be a short lowercase id")),
    }
}

fn display_name(value: Option<&Value>) -> Result<String, Error> {
    let Some(text) = value.and_then(Value::as_str) else {
        return invalid("name must be text");
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let length = text.chars().count();
    if length == 0 || length > 80 {
        return invalid("name must be 1..80 characters");
    }
    Ok(text)
}

fn int(value: Option<&Value>, label: &str, low: i64, high: i64) -> Result<i64, Error> {
    match value.and_then(Value::as_i64) {
        Some(number) if (low..=high).contains(&number) => Ok(number),
        _ => invalid(format!("{label} must be an integer {low}..{high}")),
    }
}

fn rgb_triple(value: Option<&Value>) -> Result<[i64; 3], Error> {
    match value.and_then(Value::as_array).map(Vec::as_slice) {
        Some([red, green, blue]) => Ok([
            int(Some(red), "rgb", 0, 255)?,
            int(Some(green), "rgb", 0, 255)?,
            int(Some(blue), "rgb", 0, 255)?,
        ]),
        _ => invalid("rgb must be three integers"),
    }
}

fn bulb_numbers(value: Option<&Value>, count: i64) -> Result<Vec<i64>, Error> {
    let items = match value.and_then(Value::as_array) {
        Some(items) if !items.is_empty() => items,
        _ => return invalid("a paint group needs bulbs"),
    };
    let mut bulbs = items
        .iter()
        .map(|bulb| int(Some(bulb), "bulb", 1, count))
        .collect::<Result<Vec<_>, _>>()?;
    bulbs.sort_unstable();
    if bulbs.windows(2).any(|pair| pair[0] == pair[1]) {
        return invalid("a bulb is repeated in one group");
    }
    Ok(bulbs)
}

fn paint_groups(value: Option<&Value>, count: i64) -> Result<Vec<Group>, Error> {
    let items = match value.and_then(Value::as_array) {
        Some(items) if !items.is_empty() => items,
        _ => return invalid("paint needs at least one group"),
    };
    if i64::try_from(items.len()).unwrap_or(i64::MAX) > count {
        return invalid("too many paint groups");
    }
    let mut seen: Vec<i64> = Vec::new();
    let mut groups = Vec::with_capacity(items.len());
    for item in items {
        let Some(group) = item.as_object() else {
            return invalid("a paint group must be an object");
        };
        unknown(group, &["bulbs", "rgb"])?;
        let bulbs = bulb_numbers(group.get("bulbs"), count)?;
        if let Some(overlap) = bulbs.iter().filter(|bulb| seen.contains(bulb)).min() {
            return invalid(format!("bulb {overlap} is in two groups"));
        }
        seen.extend(&bulbs);
        groups.push(Group {
            bulbs,
            rgb: rgb_triple(group.get("rgb"))?,
        });
    }
    Ok(groups)
}

fn scene_ref(value: Option<&Value>) -> Result<SceneRef, Error> {
    let Some(map) = value.and_then(Value::as_object) else {
        return invalid("scene must be an object");
    };
    unknown(map, &["name", "speed"])?;
    let Some(requested) = map.get("name").and_then(Value::as_str) else {
        return invalid("scene name is required");
    };
    let info = scene::find(requested).map_err(refusal)?;
    let speed = match map.get("speed") {
        None | Some(Value::Null) => info.speed.as_ref().map(|speed| speed.default_index),
        Some(requested_speed) => match &info.speed {
            Some(speed) if speed.option_count > 0 => Some(int(
                Some(requested_speed),
                "speed",
                0,
                speed.option_count - 1,
            )?),
            _ => return invalid(format!("{} has no speed control", info.name)),
        },
    };
    Ok(SceneRef {
        name: info.name.clone(),
        speed,
    })
}

fn sequence_step(value: &Value, count: i64) -> Result<Step, Error> {
    let Some(map) = value.as_object() else {
        return invalid("a step must be an object");
    };
    unknown(map, &["hold", "paint", "scene"])?;
    let has_paint = map.contains_key("paint");
    if has_paint == map.contains_key("scene") {
        return invalid("a step needs a paint or a scene");
    }
    let hold = int(map.get("hold"), "hold", 1, HOLD_MAX)?;
    if has_paint {
        Ok(Step {
            hold,
            paint: Some(paint_groups(map.get("paint"), count)?),
            scene: None,
        })
    } else {
        Ok(Step {
            hold,
            paint: None,
            scene: Some(scene_ref(map.get("scene"))?),
        })
    }
}

/// Validates a colour list for `bulbs` bulbs. Returns the canonical groups.
pub fn validate_paint(groups: &Value, bulbs: i64) -> Result<Vec<Group>, Error> {
    paint_groups(Some(groups), bulbs)
}

/// Returns a canonical pattern document. `bulbs` is the bulb count of the light.
pub fn validate_pattern(doc: &Value, bulbs: i64) -> Result<Pattern, Error> {
    let Some(map) = doc.as_object() else {
        return invalid("pattern must be an object");
    };
    let kind = match map.get("kind").and_then(Value::as_str) {
        Some(kind @ ("scene" | "paint" | "sequence")) => kind,
        _ => return invalid("kind must be scene, paint, or sequence"),
    };
    let allowed: &[&str] = match kind {
        "scene" => &["id", "name", "kind", "scene"],
        "paint" => &["id", "name", "kind", "paint"],
        _ => &["id", "name", "kind", "repeat", "steps"],
    };
    unknown(map, allowed)?;
    let id = slug(map.get("id"), "id")?;
    let name = display_name(map.get("name"))?;
    let body = match kind {
        "scene" => Body::Scene {
            scene: scene_ref(map.get("scene"))?,
        },
        "paint" => Body::Paint {
            paint: paint_groups(map.get("paint"), bulbs)?,
        },
        _ => {
            let repeat = match map.get("repeat") {
                None => false,
                Some(Value::Bool(flag)) => *flag,
                Some(_) => return invalid("repeat must be a boolean"),
            };
            let items = match map.get("steps").and_then(Value::as_array) {
                Some(items) if !items.is_empty() && items.len() <= STEPS_MAX => items,
                _ => return invalid(format!("a sequence needs 1..{STEPS_MAX} steps")),
            };
            let steps = items
                .iter()
                .map(|step| sequence_step(step, bulbs))
                .collect::<Result<Vec<_>, _>>()?;
            Body::Sequence { repeat, steps }
        }
    };
    Ok(Pattern { id, name, body })
}

/// Returns a canonical schedule. A pattern action must name one of `patterns`.
pub fn validate_schedule(doc: &Value, patterns: &[Pattern]) -> Result<Schedule, Error> {
    let Some(map) = doc.as_object() else {
        return invalid("schedule must be an object");
    };
    unknown(map, &["id", "enabled", "days", "time", "action"])?;
    let enabled = match map.get("enabled") {
        None => true,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return invalid("enabled must be a boolean"),
    };
    let requested = match map.get("days").and_then(Value::as_array) {
        Some(items) if !items.is_empty() => items,
        _ => return invalid("days need at least one weekday"),
    };
    let mut names: Vec<&str> = Vec::with_capacity(requested.len());
    for day in requested {
        match day.as_str() {
            Some(name) if WEEKDAYS.contains(&name) => names.push(name),
            _ => return invalid("days must be mon, tue, wed, thu, fri, sat, sun"),
        }
    }
    if names
        .iter()
        .enumerate()
        .any(|(at, day)| names[..at].contains(day))
    {
        return invalid("days are repeated");
    }
    let days: Vec<String> = WEEKDAYS
        .iter()
        .filter(|day| names.contains(*day))
        .map(|day| (*day).to_owned())
        .collect();
    let time = match map.get("time").and_then(Value::as_str) {
        Some(text) if parse_clock(text).is_some() => text.to_owned(),
        _ => return invalid("time must be HH:MM"),
    };
    let action = action(map.get("action"), patterns)?;
    let id = slug(map.get("id"), "id")?;
    Ok(Schedule {
        id,
        enabled,
        days,
        time,
        action,
    })
}

fn action(value: Option<&Value>, patterns: &[Pattern]) -> Result<Action, Error> {
    let Some(map) = value.and_then(Value::as_object) else {
        return invalid("action must be an object");
    };
    let keys: Vec<&str> = map.keys().map(String::as_str).collect();
    match keys.as_slice() {
        ["power"] => match map.get("power") {
            Some(Value::Bool(false)) => Ok(Action::Power(false)),
            _ => invalid("power action is only off"),
        },
        ["pattern"] => match map.get("pattern").and_then(Value::as_str) {
            Some(id) if patterns.iter().any(|pattern| pattern.id == id) => {
                Ok(Action::Pattern(id.to_owned()))
            }
            _ => invalid("action pattern is not saved"),
        },
        ["scene"] => Ok(Action::Scene(scene_ref(map.get("scene"))?)),
        _ => invalid("action must be a pattern, a scene, or power off"),
    }
}

/// The JSON files under one directory.
#[derive(Debug, Clone)]
pub struct Library {
    root: PathBuf,
}

impl Library {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn ensure(&self) -> Result<(), Error> {
        fs::create_dir_all(&self.root)?;
        fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    fn read<T: Document>(&self) -> Result<Vec<T>, Error> {
        let path = self.root.join(T::FILE);
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let text = fs::read_to_string(&path)?;
        let data: Value =
            serde_json::from_str(&text).map_err(|error| Error::Corrupt(error.to_string()))?;
        let Some(items) = data.get(T::KEY).and_then(Value::as_array) else {
            return Err(Error::Corrupt(format!("{} is missing {}", T::FILE, T::KEY)));
        };
        items
            .iter()
            .map(|item| {
                serde_json::from_value::<T>(item.clone())
                    .map_err(|error| Error::Corrupt(format!("{}: {error}", T::FILE)))
            })
            .collect()
    }

    /// Writes to a temp file beside the target, then renames it over the target.
    fn write<T: Document>(&self, items: &[T]) -> Result<(), Error> {
        self.ensure()?;
        let text = format!("{}\n", to_json(&Wrapped { key: T::KEY, items })?);
        let mut temp_name = T::FILE.to_owned();
        temp_name.push_str(".tmp");
        let temp = self.root.join(temp_name);
        let mut file = fs::File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, self.root.join(T::FILE))?;
        Ok(())
    }

    fn get<T: Document>(&self, id: &str) -> Result<Option<T>, Error> {
        Ok(self.read::<T>()?.into_iter().find(|item| item.id() == id))
    }

    fn save<T: Document>(&self, doc: &T, create: bool) -> Result<(), Error> {
        let mut items: Vec<T> = self.read()?;
        let found = items.iter().position(|item| item.id() == doc.id());
        match found {
            Some(_) if create => return Err(Error::Exists(doc.id().to_owned())),
            None if !create => return Err(Error::Missing(doc.id().to_owned())),
            Some(index) => items[index] = doc.clone(),
            None => items.push(doc.clone()),
        }
        self.write(&items)
    }

    fn delete<T: Document>(&self, id: &str) -> Result<(), Error> {
        let items: Vec<T> = self.read()?;
        let before = items.len();
        let kept: Vec<T> = items.into_iter().filter(|item| item.id() != id).collect();
        if kept.len() == before {
            return Err(Error::Missing(id.to_owned()));
        }
        self.write(&kept)
    }

    pub fn list_patterns(&self) -> Result<Vec<Pattern>, Error> {
        self.read()
    }

    pub fn get_pattern(&self, id: &str) -> Result<Option<Pattern>, Error> {
        self.get(id)
    }

    pub fn save_pattern(&self, pattern: &Pattern, create: bool) -> Result<(), Error> {
        self.save(pattern, create)
    }

    pub fn delete_pattern(&self, id: &str) -> Result<(), Error> {
        self.delete::<Pattern>(id)
    }

    pub fn list_schedules(&self) -> Result<Vec<Schedule>, Error> {
        self.read()
    }

    pub fn get_schedule(&self, id: &str) -> Result<Option<Schedule>, Error> {
        self.get(id)
    }

    pub fn save_schedule(&self, schedule: &Schedule, create: bool) -> Result<(), Error> {
        self.save(schedule, create)
    }

    pub fn delete_schedule(&self, id: &str) -> Result<(), Error> {
        self.delete::<Schedule>(id)
    }

    /// The latest enabled slot that came due today within 12 hours. A slot in the
    /// future is not due. Ties go to the larger id. `now` is used as its own wall
    /// clock, so the caller passes local time.
    pub fn due_schedule<Tz: TimeZone>(
        &self,
        now: &DateTime<Tz>,
    ) -> Result<Option<Schedule>, Error> {
        let now = now.fixed_offset();
        let today = WEEKDAYS[now.weekday().num_days_from_monday() as usize];
        let mut best: Option<(DateTime<FixedOffset>, Schedule)> = None;
        for schedule in self.list_schedules()? {
            if !schedule.enabled || !schedule.days.iter().any(|day| day.as_str() == today) {
                continue;
            }
            let Some((hour, minute)) = parse_clock(&schedule.time) else {
                return Err(Error::Corrupt(format!(
                    "schedule {} has a bad time",
                    schedule.id
                )));
            };
            let slot = now
                .with_hour(hour)
                .and_then(|moment| moment.with_minute(minute))
                .and_then(|moment| moment.with_second(0))
                .and_then(|moment| moment.with_nanosecond(0))
                .ok_or_else(|| {
                    Error::Corrupt(format!("schedule {} has a bad time", schedule.id))
                })?;
            if slot > now || now - slot > TimeDelta::hours(DUE_WINDOW_HOURS) {
                continue;
            }
            let newer = match &best {
                None => true,
                Some((at, current)) => slot > *at || (slot == *at && schedule.id > current.id),
            };
            if newer {
                best = Some((slot, schedule));
            }
        }
        Ok(best.map(|(_, schedule)| schedule))
    }

    /// Appends one line to `audit.jsonl`. The token name is recorded, never the token.
    pub fn audit<Tz: TimeZone>(
        &self,
        token: &str,
        method: &str,
        route: &str,
        now: &DateTime<Tz>,
    ) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Line<'a> {
            time: String,
            token: &'a str,
            method: &'a str,
            route: &'a str,
        }
        self.ensure()?;
        let line = serde_json::to_string(&Line {
            time: now
                .fixed_offset()
                .to_rfc3339_opts(SecondsFormat::Secs, false),
            token,
            method,
            route,
        })
        .map_err(|error| Error::Corrupt(error.to_string()))?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("audit.jsonl"))?;
        writeln!(file, "{}", ascii_escape(line))?;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A directory under the system temp dir, removed when dropped.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(label: &str) -> Self {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("tallowd-{label}-{}-{unique}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("a temp dir");
            Self(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{FixedOffset, TimeZone};
    use serde_json::json;

    use super::testing::TempDir;
    use super::*;

    fn zone() -> FixedOffset {
        FixedOffset::west_opt(6 * 3600).expect("a valid offset")
    }

    fn at(day: u32, hour: u32, minute: u32) -> DateTime<FixedOffset> {
        zone()
            .with_ymd_and_hms(2026, 10, day, hour, minute, 0)
            .single()
            .expect("an unambiguous time")
    }

    fn pattern(doc: Value) -> Pattern {
        validate_pattern(&doc, 15).expect("a valid pattern")
    }

    fn schedule(doc: Value) -> Schedule {
        validate_schedule(&doc, &[]).expect("a valid schedule")
    }

    fn wednesday_schedule(id: &str, clock: &str) -> Schedule {
        schedule(json!({"id": id, "days": ["wed"], "time": clock, "action": {"power": false}}))
    }

    #[test]
    fn a_scene_pattern_stores_the_catalogue_speed() {
        let saved = validate_pattern(
            &json!({"id": "sunset", "name": "Sunset", "kind": "scene", "scene": {"name": "Sunset Glow"}}),
            15,
        )
        .expect("a valid scene pattern");
        let Body::Scene { scene } = &saved.body else {
            panic!("a scene pattern has a scene body");
        };
        assert_eq!(scene.name, "Sunset Glow");
        assert!(scene.speed.is_some());
        let refused = validate_pattern(
            &json!({"id": "sunset", "name": "Sunset", "kind": "scene", "scene": {"name": "Sunset Glow", "speed": 9}}),
            15,
        );
        assert!(refused.is_err());
    }

    #[test]
    fn a_sequence_rejects_a_zero_hold_and_overlapping_bulbs() {
        let refused_hold = validate_pattern(
            &json!({"id": "walk", "name": "Walk", "kind": "sequence", "steps": [{"hold": 0, "scene": {"name": "Night"}}]}),
            15,
        );
        assert!(refused_hold.is_err());
        let refused_overlap = validate_paint(
            &json!([{"bulbs": [1, 2], "rgb": [1, 0, 0]}, {"bulbs": [2, 3], "rgb": [0, 0, 1]}]),
            15,
        );
        assert_eq!(
            refused_overlap.expect_err("overlapping bulbs").to_string(),
            "bulb 2 is in two groups"
        );
    }

    #[test]
    fn due_schedule_is_the_latest_slot_inside_twelve_hours() {
        let (_dir, catalog) = temp_library("due");
        for (id, clock) in [
            ("morning", "06:00"),
            ("afternoon", "18:00"),
            ("evening", "19:30"),
        ] {
            catalog
                .save_schedule(&wednesday_schedule(id, clock), true)
                .expect("save");
        }
        let now = at(28, 20, 0);
        assert_eq!(
            catalog.due_schedule(&now).expect("read").expect("due").id,
            "evening"
        );
        assert_eq!(
            catalog
                .due_schedule(&at(28, 19, 29))
                .expect("read")
                .expect("due")
                .id,
            "afternoon"
        );
        assert_eq!(
            catalog
                .due_schedule(&at(28, 17, 59))
                .expect("read")
                .expect("due")
                .id,
            "morning"
        );
        assert!(catalog
            .due_schedule(&(now + TimeDelta::days(1)))
            .expect("read")
            .is_none());
        let later = Library::new(catalog.root.join("later"));
        later
            .save_schedule(&wednesday_schedule("morning", "06:00"), true)
            .expect("save");
        assert!(later.due_schedule(&now).expect("read").is_none());
        let noon = schedule(json!({
            "id": "noon", "days": ["wed"], "time": "08:00", "enabled": false, "action": {"power": false}
        }));
        catalog.save_schedule(&noon, true).expect("save");
        assert_eq!(
            catalog.due_schedule(&now).expect("read").expect("due").id,
            "evening"
        );
    }

    #[test]
    fn saving_an_existing_id_as_new_says_it_exists_and_as_an_update_of_a_missing_id_says_missing() {
        let (_dir, catalog) = temp_library("saves");
        let walk = pattern(
            json!({"id": "walk", "name": "Walk", "kind": "paint", "paint": [{"bulbs": [1], "rgb": [1, 2, 3]}]}),
        );
        catalog.save_pattern(&walk, true).expect("first save");
        let exists = catalog
            .save_pattern(&walk, true)
            .expect_err("second create");
        assert_eq!(exists.to_string(), "walk");
        assert!(matches!(exists, Error::Exists(_)));
        let other = pattern(
            json!({"id": "other", "name": "Other", "kind": "paint", "paint": [{"bulbs": [1], "rgb": [0, 0, 0]}]}),
        );
        let missing = catalog
            .save_pattern(&other, false)
            .expect_err("update of nothing");
        assert!(matches!(missing, Error::Missing(_)));
        assert_eq!(
            catalog.get_pattern("walk").expect("read").expect("saved"),
            walk
        );
    }

    #[test]
    fn deleting_and_fetching_schedules_follow_the_file() {
        let (_dir, catalog) = temp_library("delete");
        catalog
            .save_schedule(&wednesday_schedule("evening", "19:30"), true)
            .expect("save");
        assert_eq!(
            catalog
                .get_schedule("evening")
                .expect("read")
                .expect("saved")
                .time,
            "19:30"
        );
        catalog.delete_schedule("evening").expect("delete");
        assert!(catalog.get_schedule("evening").expect("read").is_none());
        let gone = catalog
            .delete_schedule("evening")
            .expect_err("already deleted");
        assert!(matches!(gone, Error::Missing(_)));
    }

    #[test]
    fn deleting_a_pattern_removes_it_from_the_file() {
        let (_dir, catalog) = temp_library("delete-pattern");
        let doc = json!({
            "id": "pair",
            "name": "Pair",
            "kind": "paint",
            "paint": [{"bulbs": [1], "rgb": [1, 2, 3]}]
        });
        catalog.save_pattern(&pattern(doc), true).expect("save");
        catalog.delete_pattern("pair").expect("delete");
        assert!(catalog.list_patterns().expect("read").is_empty());
        assert!(matches!(
            catalog.delete_pattern("pair"),
            Err(Error::Missing(_))
        ));
    }

    #[test]
    fn a_library_file_without_its_key_is_refused() {
        let (dir, catalog) = temp_library("key");
        fs::write(dir.path().join("patterns.json"), "{\"other\": []}\n").expect("write");
        let refused = catalog.list_patterns().expect_err("no patterns key");
        assert_eq!(refused.to_string(), "patterns.json is missing patterns");
    }

    #[test]
    fn audit_appends_a_compact_line_with_the_token_name() {
        let (dir, catalog) = temp_library("audit");
        catalog
            .audit("hermes", "POST", "/api/scene", &at(28, 20, 0))
            .expect("audit");
        let text = fs::read_to_string(dir.path().join("audit.jsonl")).expect("read");
        assert_eq!(
            text,
            "{\"time\":\"2026-10-28T20:00:00-06:00\",\"token\":\"hermes\",\"method\":\"POST\",\"route\":\"/api/scene\"}\n"
        );
    }

    #[test]
    fn non_ascii_text_is_escaped_the_way_python_escapes_it() {
        assert_eq!(ascii_escape("Caf\u{e9}".to_owned()), "Caf\\u00e9");
        assert_eq!(ascii_escape("\u{1f600}".to_owned()), "\\ud83d\\ude00");
        assert_eq!(ascii_escape("a\u{7f}b".to_owned()), "a\\u007fb");
    }

    /// The Python writes the fixtures under host/tallowd/tests/fixtures/library. Its
    /// `inputs.json` holds the raw documents that the fixtures were made from.
    const FIXTURE_PATTERNS: &str = include_str!("../tests/fixtures/library/patterns.json");
    const FIXTURE_SCHEDULES: &str = include_str!("../tests/fixtures/library/schedules.json");
    const FIXTURE_INPUTS: &str = include_str!("../tests/fixtures/library/inputs.json");

    /// Validates the raw documents in `inputs.json` and saves them into `catalog`.
    fn save_inputs(catalog: &Library) {
        let inputs: Value = serde_json::from_str(FIXTURE_INPUTS).expect("inputs.json");
        for raw in inputs["patterns"].as_array().expect("patterns") {
            let saved = validate_pattern(raw, 15).expect("a fixture pattern is valid");
            catalog.save_pattern(&saved, true).expect("save pattern");
        }
        let patterns = catalog.list_patterns().expect("list patterns");
        for raw in inputs["schedules"].as_array().expect("schedules") {
            let saved = validate_schedule(raw, &patterns).expect("a fixture schedule is valid");
            catalog.save_schedule(&saved, true).expect("save schedule");
        }
    }

    #[test]
    fn validated_inputs_save_the_same_bytes_the_python_wrote() {
        let (dir, catalog) = temp_library("parity");
        save_inputs(&catalog);
        assert_eq!(
            fs::read_to_string(dir.path().join("patterns.json")).expect("read"),
            FIXTURE_PATTERNS
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("schedules.json")).expect("read"),
            FIXTURE_SCHEDULES
        );
    }

    #[test]
    fn a_file_the_python_wrote_loads_and_saves_back_byte_for_byte() {
        let (dir, catalog) = temp_library("roundtrip");
        fs::write(dir.path().join("patterns.json"), FIXTURE_PATTERNS).expect("write");
        fs::write(dir.path().join("schedules.json"), FIXTURE_SCHEDULES).expect("write");
        for pattern in catalog.list_patterns().expect("load patterns") {
            catalog.save_pattern(&pattern, false).expect("save pattern");
        }
        for schedule in catalog.list_schedules().expect("load schedules") {
            catalog
                .save_schedule(&schedule, false)
                .expect("save schedule");
        }
        assert_eq!(
            fs::read_to_string(dir.path().join("patterns.json")).expect("read"),
            FIXTURE_PATTERNS
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("schedules.json")).expect("read"),
            FIXTURE_SCHEDULES
        );
    }

    /// Writes the library that `tools/tests/test_library_fixture.py` reads back. The
    /// directory is `host/target/library-interop`, which is build output and not
    /// committed.
    #[test]
    fn rust_writes_a_library_the_python_reads_back() {
        let root = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/library-interop"
        ));
        let _ = fs::remove_dir_all(&root);
        let catalog = Library::new(&root);
        save_inputs(&catalog);
        assert_eq!(
            fs::read_to_string(root.join("patterns.json")).expect("read"),
            FIXTURE_PATTERNS
        );
    }

    fn temp_library(label: &str) -> (TempDir, Library) {
        let dir = TempDir::new(label);
        let catalog = Library::new(dir.path());
        (dir, catalog)
    }
}
