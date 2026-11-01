//! The light's link, its write queue, and the rules that run them. Port of
//! `LightSession` in `app/h7020_panel.py`, over the `Transport` seam in `ble.rs`.
//!
//! `BleLamp` is what the show writes into. It queues the packets and returns at once, as
//! the Python's `submit` does. `Engine` owns the transport and runs on one task. It
//! scans and connects, sends the queue, reads replies, polls power when idle, and
//! redials after a drop. Each write is followed by a 0.2 s gap, as the Python's `_write`.
//!
//! The queue keeps what the Python keeps. Each slider (power, brightness, colour) holds
//! one unsent value, and a script goes out whole, in order. A queued script replaces
//! the one before it and drops the unsent slider values, as `submit_script` does.

use std::collections::BTreeMap;
use std::fmt::Display;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{watch, Notify};
use tokio::time::Instant;

use tallow_govee::ble::{advertisement_of, Seen, Transport};
use tallow_govee::frame::{self, Frame};
use tallow_govee::status::{self, Reply};

use super::light::{Identity, Light};
use crate::show::Session;

/// The gap after each write, as the Python's `_write` sleeps 0.2 s.
const PACE: Duration = Duration::from_millis(200);
/// How long a read waits for its reply, as the Python's `_query` does.
const QUERY_WAIT: Duration = Duration::from_secs(4);
/// The idle gap before a power read. The box drops an idle central after about 10 s.
const IDLE: Duration = Duration::from_secs(4);
/// The pause before a redial after a failed connect, as the Python sleeps 2 s.
const RETRY: Duration = Duration::from_secs(2);
/// The colour profiles the codec has. Any other pact is refused before connecting.
const QUALIFIED_PACTS: [(u16, u8); 2] = [(1, 1), (2, 1)];

// The first bytes of a command frame, from `frame.rs`: header, then the opcode.
const COMMAND: u8 = 0x33;
const POWER_OP: u8 = 0x01;
const BRIGHTNESS_OP: u8 = 0x04;
const MULTI_OP: u8 = 0x05;
/// A static colour: the multi opcode's first two body bytes.
const STATIC_COLOUR: [u8; 2] = [0x15, 0x01];

/// The unsent writes. A script is kept whole. Each slider keeps its latest value.
#[derive(Default)]
struct Writes {
    script: Option<Vec<Frame>>,
    power: Option<Frame>,
    brightness: Option<Frame>,
    colour: Option<Frame>,
    strings: Option<i64>,
}

/// One unsent value per slider, in the order the Python applies them: power, brightness, colour.
#[derive(Debug, Default, PartialEq, Eq)]
struct Batch {
    power: Option<Frame>,
    brightness: Option<Frame>,
    colour: Option<Frame>,
}

impl Batch {
    fn is_empty(&self) -> bool {
        self.power.is_none() && self.brightness.is_none() && self.colour.is_none()
    }

    fn frames(&self) -> impl Iterator<Item = Frame> {
        [self.power, self.brightness, self.colour]
            .into_iter()
            .flatten()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slider {
    Power,
    Brightness,
    Colour,
}

/// A single command frame that is one slider's write. Anything else is a script.
/// Colour is the only slider sent with `replace`, and a paint never is, so a one-group
/// paint is a script even though its bytes look like a colour.
fn slider(packets: &[Frame], replace: bool) -> Option<(Slider, Frame)> {
    let [frame] = packets else {
        return None;
    };
    if frame[0] != COMMAND {
        return None;
    }
    let kind = match frame[1] {
        POWER_OP => Slider::Power,
        BRIGHTNESS_OP => Slider::Brightness,
        MULTI_OP if replace && frame[2..4] == STATIC_COLOUR => Slider::Colour,
        _ => return None,
    };
    Some((kind, *frame))
}

/// The write queue. The show fills it through `BleLamp`, and the engine empties it.
#[derive(Default)]
pub struct Queue {
    writes: Mutex<Writes>,
    wake: Notify,
}

impl Queue {
    fn writes(&self) -> MutexGuard<'_, Writes> {
        self.writes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn strings(&self) -> Option<i64> {
        self.writes().strings
    }

    pub fn set_strings(&self, strings: Option<i64>) {
        self.writes().strings = strings;
    }

    pub fn power_queued(&self) -> bool {
        self.writes().power.is_some()
    }

    pub fn brightness_queued(&self) -> bool {
        self.writes().brightness.is_some()
    }

    /// True when nothing is waiting to be sent. The idle power read runs only then.
    pub fn idle(&self) -> bool {
        let writes = self.writes();
        writes.script.is_none()
            && writes.power.is_none()
            && writes.brightness.is_none()
            && writes.colour.is_none()
    }

    fn enqueue(&self, packets: &[Frame], replace: bool) {
        let mut writes = self.writes();
        match slider(packets, replace) {
            Some((Slider::Power, frame)) => {
                writes.script = None;
                writes.power = Some(frame);
            }
            Some((Slider::Brightness, frame)) => writes.brightness = Some(frame),
            Some((Slider::Colour, frame)) => {
                writes.script = None;
                writes.colour = Some(frame);
            }
            None => {
                writes.power = None;
                writes.brightness = None;
                writes.colour = None;
                writes.script = Some(packets.to_vec());
            }
        }
        drop(writes);
        self.wake.notify_one();
    }

    fn take(&self) -> (Option<Vec<Frame>>, Batch) {
        let mut writes = self.writes();
        let script = writes.script.take();
        let batch = Batch {
            power: writes.power.take(),
            brightness: writes.brightness.take(),
            colour: writes.colour.take(),
        };
        (script, batch)
    }

    /// Puts back what a failed write did not finish. A newer value or script wins.
    fn restore(&self, script: Option<Vec<Frame>>, batch: Batch) {
        let mut writes = self.writes();
        if writes.script.is_none() {
            writes.script = script;
        }
        writes.power = writes.power.or(batch.power);
        writes.brightness = writes.brightness.or(batch.brightness);
        writes.colour = writes.colour.or(batch.colour);
        drop(writes);
        self.wake.notify_one();
    }
}

/// The light as the show writes it. Writes are queued here, and the engine sends them.
pub struct BleLamp {
    queue: Arc<Queue>,
}

impl BleLamp {
    pub fn new(queue: Arc<Queue>) -> Self {
        Self { queue }
    }
}

impl Session for BleLamp {
    fn strings(&self) -> Option<i64> {
        self.queue.strings()
    }

    fn send(&mut self, packets: &[Frame], replace: bool) -> Result<(), String> {
        if packets.is_empty() {
            return Err("script is empty".to_owned());
        }
        self.queue.enqueue(packets, replace);
        Ok(())
    }
}

/// What the engine needs from the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub scan: Duration,
    pub timeout: Duration,
    pub strings: Option<i64>,
}

/// The reads the engine asks for. A reply of another kind does not satisfy a read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Domain {
    Firmware,
    Hardware,
    Strings,
    Power,
    Brightness,
}

fn domain_of(reply: &Reply) -> Option<Domain> {
    match reply {
        Reply::FirmwareVersion { .. } => Some(Domain::Firmware),
        Reply::HardwareVersion { .. } => Some(Domain::Hardware),
        Reply::ConfiguredStrings { .. } => Some(Domain::Strings),
        Reply::Power { .. } => Some(Domain::Power),
        Reply::Brightness { .. } => Some(Domain::Brightness),
        _ => None,
    }
}

/// The text the page shows for a transport failure. The Python shows its class name.
fn ble(message: String) -> String {
    format!("BleError: {message}")
}

/// The text the page shows for a refusal. The Python raises SystemExit with these words.
fn refusal(message: impl Into<String>) -> String {
    format!("SystemExit: {}", message.into())
}

/// A value for a log line, with `None` written as the Python writes it.
fn shown<T: Display>(value: Option<T>) -> String {
    value.map_or_else(|| "None".to_owned(), |value| value.to_string())
}

enum Event {
    Stop,
    Frame(Option<Vec<u8>>),
    Wake,
    Idle,
}

/// One light, one transport, one task. Everything the page reads from the link goes
/// through `Light`, and everything the page writes goes through `Queue`.
pub struct Engine<T: Transport> {
    transport: T,
    queue: Arc<Queue>,
    light: Arc<Light>,
    options: Options,
    identity: Option<Identity>,
    linked: bool,
    first_attempt: bool,
}

impl<T: Transport> Engine<T> {
    pub fn new(transport: T, queue: Arc<Queue>, light: Arc<Light>, options: Options) -> Self {
        Self {
            transport,
            queue,
            light,
            options,
            identity: None,
            linked: false,
            first_attempt: true,
        }
    }

    /// Runs until `stop` turns true, then drops the link if there is one.
    pub async fn run(mut self, mut stop: watch::Receiver<bool>) {
        let mut deadline = Instant::now() + IDLE;
        // The first dial is a wake, so the loop connects before anything is queued.
        self.queue.wake.notify_one();
        loop {
            let event = tokio::select! {
                _ = stop.wait_for(|stopping| *stopping) => Event::Stop,
                frame = self.transport.next_notification() => Event::Frame(frame),
                () = self.queue.wake.notified() => Event::Wake,
                () = tokio::time::sleep_until(deadline) => Event::Idle,
            };
            match event {
                Event::Stop => break,
                Event::Frame(Some(frame)) => {
                    self.on_frame(&frame);
                }
                Event::Frame(None) => self.link_dropped().await,
                Event::Wake => {
                    if self.wake(&mut stop).await {
                        break;
                    }
                    deadline = Instant::now() + IDLE;
                }
                Event::Idle => {
                    self.idle_poll().await;
                    deadline = Instant::now() + IDLE;
                }
            }
        }
        if self.linked {
            self.transport.disconnect().await;
        }
        self.linked = false;
        self.light.set_linked(false);
    }

    /// Handles a wake. A dropped link is dialled again, and otherwise the queue is sent.
    /// Returns true when `stop` turned while dialling, so the loop should end.
    async fn wake(&mut self, stop: &mut watch::Receiver<bool>) -> bool {
        if self.linked {
            self.drain().await;
            return false;
        }
        let identify = self.identity.is_none();
        let dial = self.dial(identify);
        let outcome = tokio::select! {
            _ = stop.wait_for(|stopping| *stopping) => return true,
            outcome = dial => outcome,
        };
        let first = std::mem::replace(&mut self.first_attempt, false);
        match outcome {
            Ok(()) => {
                self.linked = true;
                self.drain().await;
            }
            Err(error) => {
                self.light.set_linked(false);
                self.light.set_error(Some(error.clone()));
                if first {
                    eprintln!(
                        "Connect failed ({error}). Close Govee Home so this host can be the only central. Still trying."
                    );
                } else {
                    eprintln!("Reconnect failed ({error}).");
                }
                tokio::select! {
                    _ = stop.wait_for(|stopping| *stopping) => return true,
                    () = tokio::time::sleep(RETRY) => {}
                }
                self.queue.wake.notify_one();
            }
        }
        false
    }

    /// Scans for the light, checks its advertisement, connects, and reads its identity
    /// when `identify` is set. Every refusal happens before the connect.
    async fn dial(&mut self, identify: bool) -> Result<(), String> {
        let seen = self.transport.scan(self.options.scan).await.map_err(ble)?;
        let (name, manufacturer) = match seen {
            Some(Seen {
                name,
                manufacturer_data,
            }) => (name, manufacturer_data),
            None => (None, BTreeMap::new()),
        };
        let advert = advertisement_of(&manufacturer)
            .ok_or_else(|| refusal("Advertisement did not include a color pact."))?;
        if advert.supports_encryption {
            return Err(refusal(
                "Advertisement says the link is encrypted. The page will not send plaintext frames.",
            ));
        }
        let pact = (advert.pact_type, advert.pact_code);
        if !QUALIFIED_PACTS.contains(&pact) {
            return Err(refusal(format!(
                "Pact {}/{} is outside the codec's color profiles.",
                pact.0, pact.1
            )));
        }
        if let Err(error) = self.transport.connect(self.options.timeout).await {
            self.transport.disconnect().await;
            return Err(ble(error));
        }
        if identify {
            if let Err(error) = self.identify(name).await {
                self.transport.disconnect().await;
                return Err(error);
            }
        }
        self.light.set_linked(true);
        self.light.set_error(None);
        if let Some(identity) = &self.identity {
            eprintln!(
                "Connected {} pact {}/{} fw {} hw {} strings {}",
                identity.name,
                pact.0,
                pact.1,
                shown(identity.firmware.as_deref()),
                shown(identity.hardware.as_deref()),
                identity.strings
            );
        }
        Ok(())
    }

    /// Reads the firmware, hardware, string count, power, and brightness, in the
    /// Python's order, and stores them for the page.
    async fn identify(&mut self, name: Option<String>) -> Result<(), String> {
        let firmware = match self
            .query(frame::firmware_query(), Domain::Firmware)
            .await?
        {
            Some(Reply::FirmwareVersion { text }) => Some(text),
            _ => None,
        };
        let hardware = match self
            .query(frame::hardware_query(), Domain::Hardware)
            .await?
        {
            Some(Reply::HardwareVersion { text }) => Some(text),
            _ => None,
        };
        let counted = match self
            .query(frame::configured_strings_query(), Domain::Strings)
            .await?
        {
            Some(Reply::ConfiguredStrings { count }) => Some(i64::from(count)),
            _ => None,
        };
        let power = match self.query(frame::power_query(), Domain::Power).await? {
            Some(Reply::Power { is_on }) => Some(is_on),
            _ => None,
        };
        let brightness = match self
            .query(frame::brightness_query(), Domain::Brightness)
            .await?
        {
            Some(Reply::Brightness { percent }) => Some(i64::from(percent)),
            _ => None,
        };
        // The option wins, then the light's own count, then one string.
        let strings = self
            .options
            .strings
            .or(counted.filter(|count| *count != 0))
            .unwrap_or(1);
        let identity = Identity {
            name: name.unwrap_or_else(|| "H7020".to_owned()),
            firmware,
            hardware,
            strings,
            power,
            brightness,
        };
        self.queue.set_strings(Some(strings));
        self.light.identified(&identity);
        self.identity = Some(identity);
        Ok(())
    }

    /// Writes one read and waits up to four seconds for its reply. Frames that arrive
    /// first are taken as they come, and every reply moves the state, as the Python's
    /// `_on_notify` does.
    async fn query(&mut self, frame: Frame, want: Domain) -> Result<Option<Reply>, String> {
        // Replies already waiting are read first, as the Python drains its queue.
        loop {
            let next =
                tokio::time::timeout(Duration::ZERO, self.transport.next_notification()).await;
            match next {
                Ok(Some(stale)) => {
                    self.on_frame(&stale);
                }
                _ => break,
            }
        }
        self.transport.write(&frame).await.map_err(ble)?;
        let deadline = Instant::now() + QUERY_WAIT;
        loop {
            match tokio::time::timeout_at(deadline, self.transport.next_notification()).await {
                Err(_) => return Ok(None),
                Ok(None) => {
                    return Err(ble("the link dropped while a read was waiting".to_owned()));
                }
                Ok(Some(bytes)) => {
                    if let Some(reply) = self.on_frame(&bytes) {
                        if domain_of(&reply) == Some(want) {
                            return Ok(Some(reply));
                        }
                    }
                }
            }
        }
    }

    /// Parses one notification. Once the light is identified, a reply moves the page's
    /// power or brightness, unless a write for the same setting is still queued.
    fn on_frame(&mut self, frame: &[u8]) -> Option<Reply> {
        let reply = status::parse(frame)?;
        if self.identity.is_some() {
            match reply {
                Reply::Power { is_on } if !self.queue.power_queued() => {
                    self.light.observe_power(is_on);
                }
                Reply::Brightness { percent } if !self.queue.brightness_queued() => {
                    self.light.observe_brightness(i64::from(percent));
                }
                _ => {}
            }
        }
        Some(reply)
    }

    /// Sends the script first, in order, then the slider values. Repeats until the queue
    /// is empty. A failed write puts back what it did not finish and drops the link.
    async fn drain(&mut self) {
        loop {
            let (script, batch) = self.queue.take();
            if script.is_none() && batch.is_empty() {
                return;
            }
            match self.send_all(script.as_deref(), &batch).await {
                Ok(()) => self.light.set_error(None),
                Err(error) => {
                    self.queue.restore(script, batch);
                    self.lose_link(Some(error)).await;
                    return;
                }
            }
        }
    }

    async fn send_all(&mut self, script: Option<&[Frame]>, batch: &Batch) -> Result<(), String> {
        for frame in script.unwrap_or_default() {
            self.send(frame).await?;
        }
        for frame in batch.frames() {
            self.send(&frame).await?;
        }
        Ok(())
    }

    async fn send(&mut self, frame: &Frame) -> Result<(), String> {
        self.transport.write(frame).await.map_err(ble)?;
        tokio::time::sleep(PACE).await;
        Ok(())
    }

    /// The box dropped the link on its own. The next wake dials again.
    async fn link_dropped(&mut self) {
        eprintln!("BLE link down; reconnecting.");
        self.lose_link(None).await;
    }

    /// The page shows the link down, the transport lets go, and the next wake dials again.
    async fn lose_link(&mut self, error: Option<String>) {
        self.linked = false;
        self.light.set_linked(false);
        if let Some(error) = error {
            self.light.set_error(Some(error));
        }
        self.transport.disconnect().await;
        self.queue.wake.notify_one();
    }

    /// Reads power when nothing is waiting, so the box keeps the link and the page shows
    /// the light's own state.
    async fn idle_poll(&mut self) {
        if !self.linked || self.identity.is_none() || !self.queue.idle() {
            return;
        }
        if let Err(error) = self.query(frame::power_query(), Domain::Power).await {
            self.lose_link(Some(error)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::sync::Mutex as StdMutex;

    use serde_json::{json, Value};
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;

    use super::*;
    use crate::library::testing::TempDir;
    use crate::library::{validate_paint, Library};
    use crate::panel::light::ble_light;

    /// What the fake light has seen and what it will do.
    #[derive(Default)]
    struct Log {
        scan: Option<Seen>,
        scan_calls: usize,
        scan_blocks: bool,
        connects: usize,
        connect_failures: usize,
        disconnects: usize,
        writes: Vec<Vec<u8>>,
        fail_writes: usize,
        gate: Option<Arc<Notify>>,
        power: bool,
        brightness: u8,
    }

    /// A light that answers its reads, and hands back the frames it is written.
    struct Fake {
        log: Arc<StdMutex<Log>>,
        linked: bool,
        inbox: mpsc::UnboundedReceiver<Option<Vec<u8>>>,
        outbox: mpsc::UnboundedSender<Option<Vec<u8>>>,
    }

    /// A reply frame: header 0xaa, the domain, the body, zeros, and the XOR check.
    fn reply(domain: u8, body: &[u8]) -> Vec<u8> {
        let mut frame = vec![0xaa, domain];
        frame.extend_from_slice(body);
        frame.resize(19, 0);
        let check = frame.iter().fold(0u8, |acc, byte| acc ^ byte);
        frame.push(check);
        frame
    }

    fn reply_to(opcode: u8, log: &Log) -> Option<Vec<u8>> {
        Some(match opcode {
            0x06 => reply(0x06, b"1.00.14\0"),
            0x07 => reply(0x07, &[&[0x03][..], b"3.01.01\0"].concat()),
            0x0f => reply(0x0f, &[1]),
            0x01 => reply(0x01, &[u8::from(log.power)]),
            0x04 => reply(0x04, &[log.brightness]),
            _ => return None,
        })
    }

    impl Transport for Fake {
        async fn scan(&mut self, _window: Duration) -> Result<Option<Seen>, String> {
            let blocks = {
                let mut log = locked(&self.log);
                log.scan_calls += 1;
                log.scan_blocks
            };
            if blocks {
                return pending().await;
            }
            Ok(self
                .log
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .scan
                .clone())
        }

        async fn connect(&mut self, _timeout: Duration) -> Result<(), String> {
            let refused = {
                let mut log = locked(&self.log);
                log.connects += 1;
                if log.connect_failures > 0 {
                    log.connect_failures -= 1;
                    true
                } else {
                    false
                }
            };
            if refused {
                return Err("the adapter is busy".to_owned());
            }
            self.linked = true;
            Ok(())
        }

        async fn write(&mut self, frame: &[u8]) -> Result<(), String> {
            let gate = self
                .log
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .gate
                .take();
            if let Some(gate) = gate {
                gate.notified().await;
            }
            let answer = {
                let mut log = locked(&self.log);
                if log.fail_writes > 0 {
                    log.fail_writes -= 1;
                    return Err("the light refused the write".to_owned());
                }
                log.writes.push(frame.to_vec());
                if frame[0] == 0xaa {
                    reply_to(frame[1], &log)
                } else {
                    None
                }
            };
            if let Some(answer) = answer {
                let _ = self.outbox.send(Some(answer));
            }
            Ok(())
        }

        async fn next_notification(&mut self) -> Option<Vec<u8>> {
            if !self.linked {
                return pending().await;
            }
            match self.inbox.recv().await {
                Some(Some(frame)) => Some(frame),
                _ => None,
            }
        }

        async fn disconnect(&mut self) {
            self.linked = false;
            while self.inbox.try_recv().is_ok() {}
            self.log
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .disconnects += 1;
        }
    }

    struct Rig {
        light: Arc<Light>,
        log: Arc<StdMutex<Log>>,
        outbox: mpsc::UnboundedSender<Option<Vec<u8>>>,
        stop: watch::Sender<bool>,
        engine: JoinHandle<()>,
        _dir: TempDir,
    }

    /// The advertisement of a Govee with the given pact. The payload's type and code
    /// follow the magic `0xec` and the two zero bytes.
    fn seen(pact_type: u8, pact_code: u8) -> Seen {
        let mut manufacturer_data = BTreeMap::new();
        manufacturer_data.insert(0x8802, vec![0xec, 0x00, pact_type, pact_code, 0x01]);
        Seen {
            name: Some("Govee_H7020_3E4A".to_owned()),
            manufacturer_data,
        }
    }

    fn rig(label: &str, seen: Option<Seen>) -> Rig {
        let dir = TempDir::new(label);
        let queue = Arc::new(Queue::default());
        let light = Arc::new(ble_light(
            Library::new(dir.path().join("library")),
            queue.clone(),
        ));
        let log = Arc::new(StdMutex::new(Log {
            scan: seen,
            power: true,
            brightness: 100,
            ..Log::default()
        }));
        let (outbox, inbox) = mpsc::unbounded_channel();
        let fake = Fake {
            log: log.clone(),
            linked: false,
            inbox,
            outbox: outbox.clone(),
        };
        let (stop, stop_rx) = watch::channel(false);
        let options = Options {
            scan: Duration::from_secs(12),
            timeout: Duration::from_secs(20),
            strings: None,
        };
        let engine = tokio::spawn(Engine::new(fake, queue, light.clone(), options).run(stop_rx));
        Rig {
            light,
            log,
            outbox,
            stop,
            engine,
            _dir: dir,
        }
    }

    fn locked(log: &StdMutex<Log>) -> std::sync::MutexGuard<'_, Log> {
        log.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits for `check` to hold, a millisecond at a time, and fails if it never does.
    async fn until(mut check: impl FnMut() -> bool) {
        for _ in 0..10_000 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("the condition never held");
    }

    fn commands(log: &StdMutex<Log>) -> Vec<Vec<u8>> {
        locked(log)
            .writes
            .iter()
            .filter(|frame| frame[0] == COMMAND)
            .cloned()
            .collect()
    }

    fn writes(log: &StdMutex<Log>) -> Vec<Vec<u8>> {
        locked(log).writes.clone()
    }

    async fn linked(rig: &Rig) {
        let light = rig.light.clone();
        until(move || light.linked()).await;
    }

    fn groups() -> Vec<crate::library::Group> {
        validate_paint(
            &json!([{"bulbs": [1], "rgb": [255, 0, 0]}, {"bulbs": [2], "rgb": [0, 255, 0]}]),
            15,
        )
        .expect("valid groups")
    }

    fn paint_frames() -> Vec<Vec<u8>> {
        let painted: Vec<frame::PaintGroup> = vec![
            frame::PaintGroup {
                bulbs: vec![1],
                rgb: [255, 0, 0],
            },
            frame::PaintGroup {
                bulbs: vec![2],
                rgb: [0, 255, 0],
            },
        ];
        frame::paint(1, &painted)
            .expect("paint frames")
            .into_iter()
            .map(|frame| frame.to_vec())
            .collect()
    }

    #[test]
    fn a_power_write_keeps_an_unsent_brightness() {
        let queue = Queue::default();
        queue.enqueue(&[frame::brightness(40)], false);
        queue.enqueue(&[frame::power(false)], true);
        let (script, batch) = queue.take();
        assert_eq!(script, None);
        assert_eq!(batch.power, Some(frame::power(false)));
        assert_eq!(batch.brightness, Some(frame::brightness(40)));
        assert_eq!(batch.colour, None);
    }

    #[test]
    fn a_script_replaces_the_unsent_script_and_drops_the_slider_values() {
        let queue = Queue::default();
        queue.enqueue(&[frame::brightness(40)], false);
        queue.enqueue(&[frame::colour(1, [1, 0, 0]).expect("colour")], true);
        queue.enqueue(&[frame::power(true), frame::power(true)], false);
        queue.enqueue(&[frame::power(false), frame::power(false)], false);
        let (script, batch) = queue.take();
        assert_eq!(script, Some(vec![frame::power(false), frame::power(false)]));
        assert!(batch.is_empty());
    }

    #[test]
    fn a_one_group_paint_is_a_script_and_not_a_colour_slider() {
        let queue = Queue::default();
        let one = frame::paint(
            1,
            &[frame::PaintGroup {
                bulbs: vec![1],
                rgb: [9, 9, 9],
            }],
        )
        .expect("one group");
        queue.enqueue(&one, false);
        let (script, batch) = queue.take();
        assert_eq!(script, Some(one));
        assert!(batch.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_slider_keeps_only_the_latest_colour_while_a_write_is_in_flight() {
        let rig = rig("slider", Some(seen(1, 1)));
        linked(&rig).await;
        let gate = Arc::new(Notify::new());
        locked(&rig.log).gate = Some(gate.clone());
        rig.light.color([1, 0, 0]).expect("colour one");
        let log = rig.log.clone();
        until(move || locked(&log).gate.is_none()).await;
        rig.light.color([2, 0, 0]).expect("colour two");
        rig.light.color([9, 0, 0]).expect("colour nine");
        gate.notify_one();
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(
            commands(&rig.log),
            vec![
                frame::colour(1, [1, 0, 0]).expect("colour").to_vec(),
                frame::colour(1, [9, 0, 0]).expect("colour").to_vec(),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_scene_script_goes_out_in_order_and_the_slider_colour_follows() {
        let rig = rig("script", Some(seen(1, 1)));
        linked(&rig).await;
        let gate = Arc::new(Notify::new());
        locked(&rig.log).gate = Some(gate.clone());
        rig.light.paint(&groups()).expect("paint");
        let log = rig.log.clone();
        until(move || locked(&log).gate.is_none()).await;
        rig.light.color([9, 0, 0]).expect("colour nine");
        gate.notify_one();
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut expected = paint_frames();
        expected.push(frame::colour(1, [9, 0, 0]).expect("colour").to_vec());
        assert_eq!(commands(&rig.log), expected);
    }

    #[tokio::test(start_paused = true)]
    async fn closing_without_a_client_skips_the_link() {
        let rig = rig("close", Some(seen(1, 1)));
        let log = rig.log.clone();
        locked(&rig.log).scan_blocks = true;
        until(move || locked(&log).scan_calls == 1).await;
        rig.stop.send(true).expect("stop");
        tokio::time::timeout(Duration::from_secs(5), rig.engine)
            .await
            .expect("the engine stops")
            .expect("no panic");
        let log = locked(&rig.log);
        assert_eq!(log.connects, 0);
        assert_eq!(log.disconnects, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_snapshot_follows_the_live_link() {
        let rig = rig("live", Some(seen(1, 1)));
        linked(&rig).await;
        assert_eq!(rig.light.snapshot()["connected"], true);
        // The first redial is refused, so the page shows the link down for the retry gap.
        locked(&rig.log).connect_failures = 1;
        rig.outbox.send(None).expect("drop the link");
        let light = rig.light.clone();
        until(move || !light.linked()).await;
        assert_eq!(rig.light.snapshot()["connected"], false);
        linked(&rig).await;
        assert_eq!(rig.light.snapshot()["connected"], true);
        assert_eq!(locked(&rig.log).connects, 3);
    }

    #[test]
    fn status_queries_are_codec_packets() {
        let power = frame::power_query();
        let brightness = frame::brightness_query();
        assert_eq!(power[0], 0xaa);
        assert_eq!(brightness[0], 0xaa);
        assert_ne!(power, brightness);
    }

    #[tokio::test(start_paused = true)]
    async fn the_identity_reads_go_out_in_the_python_order_and_fill_the_page() {
        let rig = rig("identity", Some(seen(1, 1)));
        linked(&rig).await;
        let sent = writes(&rig.log);
        let reads: Vec<Vec<u8>> = [
            frame::firmware_query(),
            frame::hardware_query(),
            frame::configured_strings_query(),
            frame::power_query(),
            frame::brightness_query(),
        ]
        .iter()
        .map(|frame| frame.to_vec())
        .collect();
        assert_eq!(sent[..5], reads[..]);
        let snap = rig.light.snapshot();
        assert_eq!(snap["name"], "Govee_H7020_3E4A");
        assert_eq!(snap["firmware"], "1.00.14");
        assert_eq!(snap["hardware"], "3.01.01");
        assert_eq!(snap["strings"], 1);
        assert_eq!(snap["bulbs"], 15);
        assert_eq!(snap["on"], true);
        assert_eq!(snap["brightness"], 100);
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_link_reads_power_every_four_seconds() {
        let rig = rig("idle", Some(seen(1, 1)));
        linked(&rig).await;
        let before = writes(&rig.log).len();
        tokio::time::sleep(Duration::from_secs(4) + Duration::from_millis(50)).await;
        assert_eq!(writes(&rig.log).len(), before + 1);
        assert_eq!(
            writes(&rig.log).last(),
            Some(&frame::power_query().to_vec())
        );
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert_eq!(writes(&rig.log).len(), before + 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_advertisement_leaves_the_panel_up_and_retries_after_two_seconds() {
        let rig = rig("refused", None);
        let light = rig.light.clone();
        until(move || light.snapshot()["error"] != Value::Null).await;
        assert!(!rig.light.linked());
        assert_eq!(
            rig.light.snapshot()["error"],
            "SystemExit: Advertisement did not include a color pact."
        );
        assert_eq!(locked(&rig.log).scan_calls, 1);
        tokio::time::sleep(Duration::from_millis(2_050)).await;
        assert_eq!(locked(&rig.log).scan_calls, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn an_encrypted_advertisement_is_refused_before_connecting() {
        let mut sealed = seen(1, 1);
        sealed.manufacturer_data.clear();
        sealed
            .manufacturer_data
            .insert(0x8842, vec![0xec, 0x00, 0x01, 0x01, 0x01]);
        let rig = rig("sealed", Some(sealed));
        let light = rig.light.clone();
        until(move || light.snapshot()["error"] != Value::Null).await;
        assert_eq!(
            rig.light.snapshot()["error"],
            "SystemExit: Advertisement says the link is encrypted. The page will not send plaintext frames."
        );
        assert_eq!(locked(&rig.log).connects, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unqualified_pact_is_refused_before_connecting() {
        let rig = rig("pact", Some(seen(3, 1)));
        let light = rig.light.clone();
        until(move || light.snapshot()["error"] != Value::Null).await;
        assert_eq!(
            rig.light.snapshot()["error"],
            "SystemExit: Pact 3/1 is outside the codec's color profiles."
        );
        assert_eq!(locked(&rig.log).connects, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_write_keeps_the_script_and_redials() {
        let rig = rig("failed", Some(seen(1, 1)));
        linked(&rig).await;
        locked(&rig.log).fail_writes = 1;
        rig.light.paint(&groups()).expect("paint");
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(commands(&rig.log), paint_frames());
        assert_eq!(locked(&rig.log).connects, 2);
        assert_eq!(rig.light.snapshot()["error"], Value::Null);
    }

    #[tokio::test(start_paused = true)]
    async fn a_reply_moves_power_and_brightness_unless_a_write_is_queued() {
        let rig = rig("replies", Some(seen(1, 1)));
        linked(&rig).await;
        locked(&rig.log).power = false;
        rig.outbox
            .send(Some(reply(0x01, &[0])))
            .expect("power reply");
        let light = rig.light.clone();
        until(move || light.snapshot()["on"] == false).await;
        rig.outbox
            .send(Some(reply(0x04, &[42])))
            .expect("brightness reply");
        let light = rig.light.clone();
        until(move || light.snapshot()["brightness"] == 42).await;
    }
}
