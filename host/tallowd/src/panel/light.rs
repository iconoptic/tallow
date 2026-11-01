//! The light as the page sees it. Wraps the playback in `show.rs` with the link
//! state the page reports, and runs its clock.
//!
//! The light is one of three sessions. `Preview` takes writes and sends nothing, so
//! the contract runs with no radio. `Lamp` (in `session.rs`) queues writes for the
//! BLE link. `Offline` refuses every write, and only the tests use it.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::Local;
use serde_json::{json, Value};
use tokio::sync::Notify;

use tallow_govee::frame::{Frame, BULBS_PER_STRING};

use super::session::{BleLamp, Queue};
use crate::library::{Error, Group, Library, Pattern};
use crate::show::{Session, Show};

/// The note the light page carries. The Python has the same words.
pub const PHONE_HOME: &str = "This box still phones home if it remembers the house Wi-Fi. \
Forget that SSID and leave it off tallow-iot. BLE works with no Wi-Fi.";

/// How often the schedule clock fires, as the Python's `_schedule_loop` sleeps.
const FIRE_EVERY: Duration = Duration::from_secs(5);

/// The light's writes, behind the trait the show needs.
pub type Lamp = Box<dyn Session + Send>;

impl Session for Lamp {
    fn strings(&self) -> Option<i64> {
        (**self).strings()
    }

    fn send(&mut self, packets: &[Frame], replace: bool) -> Result<(), String> {
        (**self).send(packets, replace)
    }
}

/// The preview light: one string, always linked. Writes are accepted and not sent.
pub struct Preview;

impl Session for Preview {
    fn strings(&self) -> Option<i64> {
        Some(1)
    }

    fn send(&mut self, _packets: &[Frame], _replace: bool) -> Result<(), String> {
        Ok(())
    }
}

/// No light is connected. Every write is refused, and the light routes answer 503.
/// Only the tests build one, so a normal build sees no use of it.
#[cfg_attr(not(test), allow(dead_code))]
pub struct Offline;

impl Session for Offline {
    fn strings(&self) -> Option<i64> {
        None
    }

    fn send(&mut self, _packets: &[Frame], _replace: bool) -> Result<(), String> {
        Err("disconnected".to_owned())
    }
}

/// What the page reports about the link. Written by the session, read by the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub linked: bool,
    pub preview: bool,
    pub name: Option<String>,
    pub firmware: Option<String>,
    pub hardware: Option<String>,
    pub strings: Option<i64>,
    pub brightness: Option<i64>,
    /// Whether the light has reported its power. Until it has, the page shows null.
    pub power_known: bool,
}

impl Link {
    pub fn preview() -> Self {
        Self {
            linked: true,
            preview: true,
            name: Some("H7020 preview".to_owned()),
            firmware: Some("preview".to_owned()),
            hardware: Some("preview".to_owned()),
            strings: Some(1),
            brightness: Some(100),
            power_known: true,
        }
    }

    pub fn offline() -> Self {
        Self {
            linked: false,
            preview: false,
            name: None,
            firmware: None,
            hardware: None,
            strings: None,
            brightness: None,
            power_known: false,
        }
    }
}

/// The light the BLE link serves. It starts offline, with its writes queued for the link.
pub fn ble_light(library: Library, queue: Arc<Queue>) -> Light {
    Light::new(
        Box::new(BleLamp::new(queue)),
        library,
        Link::offline(),
        false,
        None,
    )
}

/// What the light reports once its identity reads are done. The BLE link fills it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub firmware: Option<String>,
    pub hardware: Option<String>,
    pub strings: i64,
    pub power: Option<bool>,
    pub brightness: Option<i64>,
}

/// The light, its link, and the clock that plays it. Lock order is always the show,
/// then the link.
pub struct Light {
    show: Mutex<Show<Lamp>>,
    link: Mutex<Link>,
    wake: Notify,
    started: std::time::Instant,
}

impl Light {
    /// `on` and `color` are what the light shows before anything is sent.
    pub fn new(
        lamp: Lamp,
        library: Library,
        link: Link,
        on: bool,
        color: Option<[i64; 3]>,
    ) -> Self {
        let mut show = Show::new(lamp, library, on);
        show.state.color = color;
        Self {
            show: Mutex::new(show),
            link: Mutex::new(link),
            wake: Notify::new(),
            started: std::time::Instant::now(),
        }
    }

    fn guard_show(&self) -> MutexGuard<'_, Show<Lamp>> {
        self.show.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn guard_link(&self) -> MutexGuard<'_, Link> {
        self.link.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The monotonic clock the show is driven by, in milliseconds since the light started.
    pub fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub fn linked(&self) -> bool {
        self.guard_link().linked
    }

    /// Bulbs on the light: 15 for each string, and one string until the light says otherwise.
    pub fn bulb_count(&self) -> i64 {
        let strings = self.guard_link().strings.unwrap_or(1);
        i64::from(BULBS_PER_STRING) * strings
    }

    /// The page's state, in the Python's shape.
    pub fn snapshot(&self) -> Value {
        let show = self.guard_show();
        let link = self.guard_link();
        let stage = &show.state;
        let scene = stage.scene.as_ref().map_or(
            Value::Null,
            |scene| json!({"name": scene.name, "speed": scene.speed}),
        );
        let paint = stage.paint.as_ref().map_or(Value::Null, |groups| {
            serde_json::to_value(groups).unwrap_or(Value::Null)
        });
        let color = stage
            .color
            .map_or(Value::Null, |[r, g, b]| json!({"r": r, "g": g, "b": b}));
        json!({
            "device": "h7020",
            "name": link.name,
            "on": if link.power_known { json!(stage.on) } else { Value::Null },
            "brightness": link.brightness,
            "color": color,
            "firmware": link.firmware,
            "hardware": link.hardware,
            "strings": link.strings,
            "bulbs": i64::from(BULBS_PER_STRING) * link.strings.unwrap_or(1),
            "connected": link.linked,
            "error": stage.error,
            "note": PHONE_HOME,
            "mode": stage.mode,
            "scene": scene,
            "paint": paint,
            "pattern_id": stage.pattern_id,
            "playing": stage.playing,
            "preview": link.preview,
        })
    }

    /// Runs one write. A failed write changes nothing the page shows. A successful one
    /// clears the error, as the Python's submit does, and wakes the clock.
    fn act<F>(&self, write: F) -> Result<Value, Error>
    where
        F: FnOnce(&mut Show<Lamp>, &mut Link, u64) -> Result<(), Error>,
    {
        let now = self.now_ms();
        {
            let mut show = self.guard_show();
            let mut link = self.guard_link();
            write(&mut show, &mut link, now)?;
            show.state.error = None;
        }
        self.wake.notify_one();
        Ok(self.snapshot())
    }

    pub fn power(&self, on: bool) -> Result<Value, Error> {
        self.act(|show, _link, _now| show.apply_power(on))
    }

    pub fn brightness(&self, level: i64) -> Result<Value, Error> {
        self.act(|show, link, _now| {
            show.apply_brightness(level)?;
            link.brightness = Some(level);
            Ok(())
        })
    }

    pub fn color(&self, rgb: [i64; 3]) -> Result<Value, Error> {
        self.act(|show, _link, _now| show.apply_color(rgb))
    }

    pub fn scene(&self, name: &str, speed: Option<i64>) -> Result<Value, Error> {
        self.act(|show, _link, _now| show.apply_scene(name, speed, None))
    }

    pub fn paint(&self, groups: &[Group]) -> Result<Value, Error> {
        self.act(|show, _link, _now| show.apply_paint(groups, None))
    }

    pub fn pattern(&self, pattern: &Pattern) -> Result<Value, Error> {
        self.act(|show, _link, now| show.apply_pattern(pattern, now))
    }

    /// Records the power the light reports.
    pub fn observe_power(&self, on: bool) {
        let mut show = self.guard_show();
        let mut link = self.guard_link();
        show.observe_on(on);
        link.power_known = true;
    }

    /// Records the brightness the light reports.
    pub fn observe_brightness(&self, percent: i64) {
        self.guard_link().brightness = Some(percent);
    }

    /// Records whether the BLE link is up. The light routes answer 503 while it is down.
    pub fn set_linked(&self, linked: bool) {
        self.guard_link().linked = linked;
    }

    /// Records the failure the page shows, or clears it with None.
    pub fn set_error(&self, error: Option<String>) {
        self.guard_show().state.error = error;
    }

    /// Stores what the identity reads found. The power and brightness the light reports
    /// are kept only when the read got an answer.
    pub fn identified(&self, identity: &Identity) {
        let mut show = self.guard_show();
        let mut link = self.guard_link();
        link.name = Some(identity.name.clone());
        link.firmware = identity.firmware.clone();
        link.hardware = identity.hardware.clone();
        link.strings = Some(identity.strings);
        if let Some(on) = identity.power {
            show.observe_on(on);
            link.power_known = true;
        }
        if let Some(percent) = identity.brightness {
            link.brightness = Some(percent);
        }
    }

    /// Sends any due step of a sequence. Returns the clock time the next step is due.
    fn tick(&self) -> Option<u64> {
        let now = self.now_ms();
        let mut show = self.guard_show();
        show.tick(now);
        show.next_due_ms()
    }

    /// Applies the latest due schedule. A failure is kept on the page, as the Python keeps it.
    fn fire_due(&self) {
        let now = self.now_ms();
        let mut show = self.guard_show();
        match show.fire_due(Local::now(), now) {
            Ok(Some(_)) => show.state.error = None,
            Ok(None) => {}
            Err(error) => show.state.error = Some(describe(&error)),
        }
    }

    /// The tokio instant a clock time falls on.
    fn due_at(&self, ms: u64) -> tokio::time::Instant {
        tokio::time::Instant::from_std(self.started) + Duration::from_millis(ms)
    }
}

/// The text the page shows for a failure. A bad value reads as a ValueError, as the Python does.
fn describe(error: &Error) -> String {
    match error {
        Error::Invalid(message) | Error::Corrupt(message) => format!("ValueError: {message}"),
        Error::Io(error) => format!("OSError: {error}"),
        other => other.to_string(),
    }
}

/// Runs the schedule clock and the sequences for the life of the panel. It fires the
/// schedules every 5 seconds, and wakes for each sequence step when it is due.
pub async fn run_scheduler(light: Arc<Light>) {
    let mut next_fire = tokio::time::Instant::now();
    loop {
        if tokio::time::Instant::now() >= next_fire {
            light.fire_due();
            next_fire = tokio::time::Instant::now() + FIRE_EVERY;
        }
        let mut wake_at = next_fire;
        if let Some(ms) = light.tick() {
            wake_at = wake_at.min(light.due_at(ms));
        }
        tokio::select! {
            () = tokio::time::sleep_until(wake_at) => {}
            () = light.wake.notified() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(label: &str) -> (crate::library::testing::TempDir, Library) {
        let dir = crate::library::testing::TempDir::new(label);
        let library = Library::new(dir.path().join("library"));
        (dir, library)
    }

    #[test]
    fn the_preview_reports_the_same_state_as_the_python_preview() {
        let (_dir, library) = library("preview-state");
        let light = Light::new(
            Box::new(Preview),
            library,
            Link::preview(),
            true,
            Some([255, 0, 0]),
        );
        let snap = light.snapshot();
        assert_eq!(snap["device"], "h7020");
        assert_eq!(snap["name"], "H7020 preview");
        assert_eq!(snap["on"], true);
        assert_eq!(snap["brightness"], 100);
        assert_eq!(snap["color"], json!({"r": 255, "g": 0, "b": 0}));
        assert_eq!(snap["firmware"], "preview");
        assert_eq!(snap["hardware"], "preview");
        assert_eq!(snap["strings"], 1);
        assert_eq!(snap["bulbs"], 15);
        assert_eq!(snap["connected"], true);
        assert_eq!(snap["error"], Value::Null);
        assert_eq!(snap["note"], PHONE_HOME);
        assert_eq!(snap["mode"], Value::Null);
        assert_eq!(snap["scene"], Value::Null);
        assert_eq!(snap["paint"], Value::Null);
        assert_eq!(snap["pattern_id"], Value::Null);
        assert_eq!(snap["playing"], false);
        assert_eq!(snap["preview"], true);
        assert_eq!(snap.as_object().map(serde_json::Map::len), Some(18));
    }

    #[test]
    fn an_offline_light_reports_nothing_it_has_not_heard() {
        let (_dir, library) = library("offline-state");
        let light = Light::new(Box::new(Offline), library, Link::offline(), false, None);
        let snap = light.snapshot();
        assert_eq!(snap["connected"], false);
        assert_eq!(snap["on"], Value::Null);
        assert_eq!(snap["brightness"], Value::Null);
        assert_eq!(snap["color"], Value::Null);
        assert_eq!(snap["bulbs"], 15);
        assert!(light.power(true).is_err());
        assert!(light.color([1, 2, 3]).is_err());
    }

    #[test]
    fn a_brightness_write_is_kept_and_a_colour_write_shows() {
        let (_dir, library) = library("writes");
        let light = Light::new(
            Box::new(Preview),
            library,
            Link::preview(),
            true,
            Some([255, 0, 0]),
        );
        assert_eq!(light.brightness(40).expect("brightness")["brightness"], 40);
        let snap = light.color([0, 0, 255]).expect("colour");
        assert_eq!(snap["color"], json!({"r": 0, "g": 0, "b": 255}));
        assert_eq!(snap["mode"], "color");
        let off = light.power(false).expect("off");
        assert_eq!(off["on"], false);
        assert_eq!(off["mode"], "off");
    }

    #[test]
    fn a_refused_write_leaves_the_page_as_it_was() {
        let (_dir, library) = library("refused");
        let light = Light::new(
            Box::new(Preview),
            library,
            Link::preview(),
            true,
            Some([255, 0, 0]),
        );
        let before = light.snapshot();
        assert!(matches!(
            light.scene("No Such Scene", None),
            Err(Error::Invalid(_))
        ));
        assert_eq!(light.snapshot(), before);
    }

    #[test]
    fn observing_power_makes_the_on_field_known() {
        let (_dir, library) = library("observe");
        let light = Light::new(Box::new(Offline), library, Link::offline(), false, None);
        light.observe_power(true);
        assert_eq!(light.snapshot()["on"], true);
    }

    #[test]
    fn a_scene_on_the_preview_names_its_speed() {
        let (_dir, library) = library("scene");
        let light = Light::new(Box::new(Preview), library, Link::preview(), true, None);
        let snap = light.scene("Sunset Glow", Some(2)).expect("scene");
        assert_eq!(snap["scene"], json!({"name": "Sunset Glow", "speed": 2}));
        assert_eq!(snap["mode"], "scene");
        assert_eq!(snap["error"], Value::Null);
    }
}
