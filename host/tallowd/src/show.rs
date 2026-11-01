//! Playback and the schedule clock. Port of `app/show.py`, with no runtime.
//!
//! Nothing here sleeps or spawns a task. The caller passes the monotonic clock in
//! milliseconds to the calls that start or advance a sequence, calls `tick` when
//! `next_due_ms` says so, and passes the wall clock to `fire_due`. The light is a
//! `Session`, so the tests record packets instead of sending them.

use chrono::{DateTime, NaiveDate, TimeZone};
use tallow_govee::frame::{self, Frame, PaintGroup};
use tallow_govee::scene;

use crate::library::{refusal, Action, Body, Error, Group, Library, Pattern, Schedule, Step};

/// What the show needs from the light. The panel's session implements it over BLE.
/// The tests record the packets.
pub trait Session {
    /// The strings the light has, from its colour profile. None until it is known.
    fn strings(&self) -> Option<i64>;
    /// Sends packets in order. With `replace`, writes still queued behind them are
    /// dropped first.
    fn send(&mut self, packets: &[Frame], replace: bool) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneState {
    pub name: String,
    pub speed: Option<i64>,
}

/// What the light is showing, as the show last set it. The page reads this.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stage {
    pub on: bool,
    pub playing: bool,
    pub mode: Option<&'static str>,
    pub pattern_id: Option<String>,
    pub scene: Option<SceneState>,
    pub paint: Option<Vec<Group>>,
    pub color: Option<[i64; 3]>,
    /// The last failure, as the page shows it. A ValueError is prefixed with its class name.
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct Playback {
    pattern_id: String,
    steps: Vec<Step>,
    repeat: bool,
    /// The step that is playing, or next to play once `started` is false.
    index: usize,
    /// Whether step `index` has been sent. Its hold runs until `next_at`.
    started: bool,
    next_at: u64,
}

pub struct Show<S: Session> {
    session: S,
    library: Library,
    pub state: Stage,
    playback: Option<Playback>,
    fired: Option<(String, NaiveDate)>,
}

impl<S: Session> Show<S> {
    /// `on` is the light's power as it is now. The caller keeps it current with `observe_on`.
    pub fn new(session: S, library: Library, on: bool) -> Self {
        Self {
            session,
            library,
            state: Stage {
                on,
                ..Stage::default()
            },
            playback: None,
            fired: None,
        }
    }

    /// Records the light's power as the device reports it.
    pub fn observe_on(&mut self, on: bool) {
        self.state.on = on;
    }

    /// When `tick` should next run, in the caller's monotonic milliseconds. None when
    /// no sequence is holding a step.
    pub fn next_due_ms(&self) -> Option<u64> {
        self.playback
            .as_ref()
            .filter(|playback| playback.started)
            .map(|playback| playback.next_at)
    }

    fn cancel_playback(&mut self) {
        self.playback = None;
        self.state.playing = false;
    }

    fn send(&mut self, packets: &[Frame], replace: bool) -> Result<(), Error> {
        self.session.send(packets, replace).map_err(Error::Device)
    }

    /// A light that is off gets its power-on frame first, so a scene or paint lights it.
    fn with_power(&self, packets: Vec<Frame>) -> Result<Vec<Frame>, Error> {
        if self.state.on {
            return Ok(packets);
        }
        if self.session.strings().is_none() {
            return Err(Error::Invalid("the light has no colour profile yet".into()));
        }
        let mut out = vec![frame::power(true)];
        out.extend(packets);
        Ok(out)
    }

    fn paint_packets(&self, groups: &[Group]) -> Result<Vec<Frame>, Error> {
        let strings = self
            .session
            .strings()
            .ok_or_else(|| Error::Invalid("no colour profile".into()))?;
        let painted: Vec<PaintGroup> = groups
            .iter()
            .map(|group| PaintGroup {
                bulbs: group.bulbs.clone(),
                rgb: group.rgb,
            })
            .collect();
        frame::paint(strings, &painted).map_err(refusal)
    }

    fn show_light(
        &mut self,
        mode: &'static str,
        pattern_id: Option<String>,
        scene: Option<SceneState>,
        paint: Option<Vec<Group>>,
        playing: bool,
    ) {
        self.state.mode = Some(mode);
        self.state.pattern_id = pattern_id;
        self.state.scene = scene;
        self.state.paint = paint;
        self.state.playing = playing;
        self.state.on = true;
    }

    pub fn apply_power(&mut self, on: bool) -> Result<(), Error> {
        self.cancel_playback();
        self.send(&[frame::power(on)], true)?;
        self.state.on = on;
        self.state.playing = false;
        if !on {
            self.state.mode = Some("off");
        }
        Ok(())
    }

    /// Does not stop a playing sequence. The Python does not either.
    pub fn apply_brightness(&mut self, level: i64) -> Result<(), Error> {
        self.send(&[frame::brightness(level)], false)
    }

    pub fn apply_color(&mut self, rgb: [i64; 3]) -> Result<(), Error> {
        self.cancel_playback();
        let strings = self
            .session
            .strings()
            .ok_or_else(|| Error::Invalid("no colour profile".into()))?;
        let packet = frame::colour(strings, rgb).map_err(refusal)?;
        self.send(&[packet], true)?;
        self.state.color = Some(rgb);
        self.state.mode = Some("color");
        self.state.scene = None;
        self.state.paint = None;
        self.state.pattern_id = None;
        self.state.playing = false;
        Ok(())
    }

    pub fn apply_scene(
        &mut self,
        name: &str,
        speed: Option<i64>,
        pattern_id: Option<String>,
    ) -> Result<(), Error> {
        self.cancel_playback();
        let script = scene::script(name, speed).map_err(refusal)?;
        let packets = self.with_power(script.packets)?;
        self.send(&packets, false)?;
        let shown = SceneState {
            name: script.name,
            speed: script.speed,
        };
        self.show_light("scene", pattern_id, Some(shown), None, false);
        Ok(())
    }

    pub fn apply_paint(
        &mut self,
        groups: &[Group],
        pattern_id: Option<String>,
    ) -> Result<(), Error> {
        self.cancel_playback();
        let frames = self.paint_packets(groups)?;
        let packets = self.with_power(frames)?;
        self.send(&packets, false)?;
        self.show_light("paint", pattern_id, None, Some(groups.to_vec()), false);
        Ok(())
    }

    /// Starts a pattern. A sequence sends its first step now and the rest from `tick`.
    pub fn apply_pattern(&mut self, pattern: &Pattern, now_ms: u64) -> Result<(), Error> {
        match &pattern.body {
            Body::Scene { scene } => {
                self.apply_scene(&scene.name, scene.speed, Some(pattern.id.clone()))
            }
            Body::Paint { paint } => self.apply_paint(paint, Some(pattern.id.clone())),
            Body::Sequence { repeat, steps } => {
                self.cancel_playback();
                self.state.mode = Some("sequence");
                self.state.pattern_id = Some(pattern.id.clone());
                self.state.scene = None;
                self.state.paint = None;
                self.state.playing = true;
                self.playback = Some(Playback {
                    pattern_id: pattern.id.clone(),
                    steps: steps.clone(),
                    repeat: *repeat,
                    index: 0,
                    started: false,
                    next_at: now_ms,
                });
                self.tick(now_ms);
                Ok(())
            }
        }
    }

    pub fn apply_action(&mut self, action: &Action, now_ms: u64) -> Result<(), Error> {
        match action {
            Action::Power(_) => self.apply_power(false),
            Action::Pattern(id) => {
                let pattern = self
                    .library
                    .get_pattern(id)?
                    .ok_or_else(|| Error::Invalid(format!("unknown pattern {id}")))?;
                self.apply_pattern(&pattern, now_ms)
            }
            Action::Scene(reference) => self.apply_scene(&reference.name, reference.speed, None),
        }
    }

    /// Sends every step whose hold has run out by `now_ms`. A failed step stops the
    /// sequence and sets `state.error`, as the Python does.
    pub fn tick(&mut self, now_ms: u64) {
        loop {
            let Some(playback) = self.playback.as_mut() else {
                return;
            };
            if playback.started {
                if now_ms < playback.next_at {
                    return;
                }
                playback.index += 1;
                if playback.index == playback.steps.len() {
                    if !playback.repeat {
                        self.finish();
                        return;
                    }
                    playback.index = 0;
                }
            }
            let Some(step) = playback.steps.get(playback.index).cloned() else {
                self.finish();
                return;
            };
            let pattern_id = playback.pattern_id.clone();
            match self.play_step(&pattern_id, &step) {
                Ok(()) => {
                    if let Some(playback) = self.playback.as_mut() {
                        playback.started = true;
                        playback.next_at = now_ms + u64::try_from(step.hold).unwrap_or(0) * 1000;
                    }
                }
                Err(error) => {
                    self.state.error = Some(describe(&error));
                    self.finish();
                    return;
                }
            }
        }
    }

    fn finish(&mut self) {
        self.playback = None;
        self.state.playing = false;
    }

    fn play_step(&mut self, pattern_id: &str, step: &Step) -> Result<(), Error> {
        if let Some(reference) = &step.scene {
            let script = scene::script(&reference.name, reference.speed).map_err(refusal)?;
            let packets = self.with_power(script.packets)?;
            self.send(&packets, false)?;
            let shown = SceneState {
                name: script.name,
                speed: script.speed,
            };
            self.show_light(
                "sequence",
                Some(pattern_id.to_owned()),
                Some(shown),
                None,
                true,
            );
            return Ok(());
        }
        let Some(groups) = &step.paint else {
            return Err(Error::Invalid("a step needs a paint or a scene".into()));
        };
        let frames = self.paint_packets(groups)?;
        let packets = self.with_power(frames)?;
        self.send(&packets, false)?;
        self.show_light(
            "sequence",
            Some(pattern_id.to_owned()),
            None,
            Some(groups.clone()),
            true,
        );
        Ok(())
    }

    /// Applies the latest due schedule once for its local date. Returns the schedule
    /// it applied, or None when none is due or it already ran today.
    pub fn fire_due<Tz: TimeZone>(
        &mut self,
        now: DateTime<Tz>,
        now_ms: u64,
    ) -> Result<Option<Schedule>, Error> {
        let Some(chosen) = self.library.due_schedule(&now)? else {
            return Ok(None);
        };
        let key = (chosen.id.clone(), now.date_naive());
        if self.fired.as_ref() == Some(&key) {
            return Ok(None);
        }
        self.apply_action(&chosen.action, now_ms)?;
        self.fired = Some(key);
        Ok(Some(chosen))
    }
}

/// The text `state.error` holds. A ValueError keeps the class name the Python shows.
fn describe(error: &Error) -> String {
    match error {
        Error::Invalid(message) => format!("ValueError: {message}"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, FixedOffset, TimeZone};
    use serde_json::{json, Value};
    use tallow_govee::frame;

    use super::*;
    use crate::library::testing::TempDir;
    use crate::library::{validate_paint, validate_pattern, validate_schedule, Library};

    /// Records every write, and can be told to refuse them.
    struct FakeLight {
        strings: Option<i64>,
        refuse: bool,
        writes: Vec<(Vec<Frame>, bool)>,
    }

    impl FakeLight {
        fn new(strings: Option<i64>) -> Self {
            Self {
                strings,
                refuse: false,
                writes: Vec::new(),
            }
        }
    }

    impl Session for FakeLight {
        fn strings(&self) -> Option<i64> {
            self.strings
        }

        fn send(&mut self, packets: &[Frame], replace: bool) -> Result<(), String> {
            if self.refuse {
                return Err("the light refused the write".into());
            }
            self.writes.push((packets.to_vec(), replace));
            Ok(())
        }
    }

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

    fn temp_library(label: &str) -> (TempDir, Library) {
        let dir = TempDir::new(label);
        let library = Library::new(dir.path().join("library"));
        (dir, library)
    }

    #[test]
    fn sequence_holds_and_a_color_cancels_it() {
        let (_dir, library) = temp_library("hold");
        let mut show = Show::new(FakeLight::new(Some(1)), library, true);
        let walk = pattern(json!({
            "id": "walk",
            "name": "Walk",
            "kind": "sequence",
            "repeat": false,
            "steps": [
                {"hold": 1, "scene": {"name": "Sunset Glow"}},
                {"hold": 2, "scene": {"name": "Night"}}
            ]
        }));
        show.apply_pattern(&walk, 0).expect("start");
        assert_eq!(show.next_due_ms(), Some(1_000));
        show.tick(1_000);
        assert_eq!(show.session.writes.len(), 2);
        assert_eq!(show.next_due_ms(), Some(3_000));
        show.tick(3_000);
        assert_eq!(show.session.writes.len(), 2);
        assert!(!show.state.playing);
        assert_eq!(show.next_due_ms(), None);
        assert_eq!(show.session.writes[0].0[0][0], 0xA3);

        show.apply_pattern(&walk, 10_000).expect("start again");
        show.apply_color([1, 2, 3]).expect("colour");
        assert_eq!(show.state.mode, Some("color"));
        assert_eq!(show.state.color, Some([1, 2, 3]));
        assert!(!show.state.playing);
        let before = show.session.writes.len();
        show.tick(100_000);
        assert_eq!(show.session.writes.len(), before);
    }

    #[test]
    fn startup_applies_only_the_latest_due_schedule() {
        let (_dir, library) = temp_library("startup");
        for (id, clock) in [("afternoon", "18:00"), ("evening", "19:30")] {
            let schedule = validate_schedule(
                &json!({"id": id, "days": ["wed"], "time": clock, "action": {"power": false}}),
                &[],
            )
            .expect("a valid schedule");
            library.save_schedule(&schedule, true).expect("save");
        }
        let mut show = Show::new(FakeLight::new(Some(1)), library, true);
        let now = at(28, 20, 0);
        let chosen = show
            .fire_due(now, 0)
            .expect("fire")
            .expect("a due schedule");
        assert_eq!(chosen.id, "evening");
        assert!(show.fire_due(now, 0).expect("fire again").is_none());
        assert_eq!(show.session.writes, vec![(vec![frame::power(false)], true)]);
        assert_eq!(show.state.mode, Some("off"));
        assert!(!show.state.on);
    }

    #[test]
    fn a_scene_on_a_light_that_is_off_powers_it_on_first() {
        let (_dir, library) = temp_library("power-on");
        let mut show = Show::new(FakeLight::new(Some(1)), library, true);
        show.observe_on(false);
        show.apply_scene("Sunset Glow", None, Some("sunset".into()))
            .expect("scene");
        let (first, replace) = &show.session.writes[0];
        assert!(!replace);
        assert_eq!(first[0], frame::power(true));
        assert_eq!(first[1][0], 0xA3);
        assert!(show.state.on);
        assert_eq!(show.state.pattern_id.as_deref(), Some("sunset"));
        assert_eq!(
            show.state.scene,
            Some(SceneState {
                name: "Sunset Glow".into(),
                speed: Some(2),
            })
        );
    }

    #[test]
    fn a_paint_pattern_sends_one_frame_per_group() {
        let (_dir, library) = temp_library("paint");
        let mut show = Show::new(FakeLight::new(Some(1)), library, true);
        let groups = validate_paint(
            &json!([{"bulbs": [2, 1], "rgb": [255, 0, 0]}, {"bulbs": [3], "rgb": [0, 0, 255]}]),
            15,
        )
        .expect("valid groups");
        show.apply_paint(&groups, Some("pair".into()))
            .expect("paint");
        assert_eq!(show.session.writes[0].0.len(), 2);
        assert_eq!(show.state.mode, Some("paint"));
        assert_eq!(show.state.paint, Some(groups));
    }

    #[test]
    fn a_colour_with_no_profile_is_refused() {
        let (_dir, library) = temp_library("no-profile");
        let mut show = Show::new(FakeLight::new(None), library, true);
        let refused = show.apply_color([1, 2, 3]).expect_err("no profile");
        assert_eq!(refused.to_string(), "no colour profile");
        assert!(show.session.writes.is_empty());
    }

    #[test]
    fn brightness_is_sent_and_leaves_a_sequence_playing() {
        let (_dir, library) = temp_library("brightness");
        let mut show = Show::new(FakeLight::new(Some(1)), library, true);
        let walk = pattern(json!({
            "id": "walk", "name": "Walk", "kind": "sequence",
            "steps": [{"hold": 5, "scene": {"name": "Sunset Glow"}}]
        }));
        show.apply_pattern(&walk, 0).expect("start");
        show.apply_brightness(40).expect("brightness");
        assert_eq!(show.session.writes[1], (vec![frame::brightness(40)], false));
        assert!(show.state.playing);
    }

    #[test]
    fn a_failing_light_stops_the_sequence_and_shows_the_error() {
        let (_dir, library) = temp_library("failing");
        let mut show = Show::new(FakeLight::new(Some(1)), library, true);
        show.session.refuse = true;
        let walk = pattern(json!({
            "id": "walk", "name": "Walk", "kind": "sequence",
            "steps": [{"hold": 1, "scene": {"name": "Sunset Glow"}}]
        }));
        show.apply_pattern(&walk, 0).expect("start");
        assert!(!show.state.playing);
        assert_eq!(show.next_due_ms(), None);
        assert_eq!(
            show.state.error.as_deref(),
            Some("the light refused the write")
        );
    }

    #[test]
    fn an_action_naming_an_unsaved_pattern_is_refused() {
        let (_dir, library) = temp_library("unsaved");
        let mut show = Show::new(FakeLight::new(Some(1)), library, true);
        let action = Action::Pattern("nope".into());
        let refused = show.apply_action(&action, 0).expect_err("not saved");
        assert_eq!(refused.to_string(), "unknown pattern nope");
    }
}
