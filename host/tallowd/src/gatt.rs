//! GATT from the host, over the root's serial port.
//!
//! A request is the bytes `tallow_mesh_core::gatt` writes. The node reads
//! `G <hex id> <hex request>`, and its line buffer holds [`LINE_MAX`] bytes
//! before the newline. The wait takes lines and a clock, as `trial` does, so
//! the tests open no port.
//!
//! Opening the port resets the root, so it is not root yet and has no routes.
//! [`wait_for_target`] reads, which sends the heartbeat, until the target's own
//! frame comes up. That frame is what teaches the root the way back down.

use crate::decode;
use crate::link::{self, parse_line, Link};
use anyhow::{bail, ensure, Context, Result};
use std::result::Result as StdResult;
use tallow_mesh_core::gatt::{
    self, Reply, Request, Status, CONNECT_MS, REQUEST_MS, VALUE_MAX, WHITELIST,
};
use tallow_mesh_core::wire::{FrameKind, MAX_PAYLOAD};

/// The node's `G` line buffer, not counting the newline.
pub const LINE_MAX: usize = 400;

/// How long the root gets to hear the target after a reset. A leaf loses its
/// parent after 11 s and reports every 10 s.
pub const READY_S: f64 = 20.0;

/// The host waits this much past the node, so the node's own `TimedOut` arrives.
const GRACE_S: f64 = 2.0;

/// One GATT command. The bytes are built from this, not from offsets here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Whitelist { addresses: Vec<[u8; 6]> },
    Connect { address: [u8; 6], random: bool },
    Write { handle: u16, value: Vec<u8> },
    Read { handle: u16 },
    Subscribe { handle: u16 },
    Disconnect,
}

/// What the command line named.
#[derive(Debug, PartialEq, Eq)]
pub struct Job {
    pub port: String,
    pub target: u32,
    pub command: Command,
    /// `--listen <s>`, on `subscribe`: seconds to keep printing notifications.
    pub listen: Option<u32>,
}

/// A command that was sent and did not finish with status ok.
#[derive(Debug, PartialEq, Eq)]
pub enum Fail {
    /// The line would not fit in the node's buffer, or the request will not encode.
    Line,
    /// The target sent nothing up after the root's reset, so no route exists.
    NotHeard,
    Timeout,
    /// The root printed `@G refused`.
    Route,
    /// The link closed before an ok answer.
    Closed,
    Status(Status),
}

impl std::fmt::Display for Fail {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::Line => write!(out, "the line does not fit the node's buffer"),
            Fail::NotHeard => write!(
                out,
                "the target sent nothing up in {READY_S} s, so the root has no route to it"
            ),
            Fail::Timeout => write!(out, "timed out"),
            Fail::Route => write!(out, "the root refused the line"),
            Fail::Closed => write!(out, "the link closed"),
            Fail::Status(status) => write!(out, "{status:?}"),
        }
    }
}

/// The port as this command sees it. [`Link`] is the real one.
pub trait Port {
    fn clock(&self) -> f64;
    fn read(&mut self) -> Result<Vec<String>>;
    fn send(&mut self, line: &str) -> Result<()>;
    /// A request id this run has not used.
    fn next_id(&mut self) -> u16;
}

impl Port for Link {
    fn clock(&self) -> f64 {
        link::unix_now()
    }

    fn read(&mut self) -> Result<Vec<String>> {
        self.lines()
    }

    fn send(&mut self, line: &str) -> Result<()> {
        self.send_gatt(line)
    }

    fn next_id(&mut self) -> u16 {
        Link::next_id(self)
    }
}

/// Reads until a frame from `target` comes up. Reading is what sends the
/// heartbeat. `false` means `seconds` passed with nothing from the target.
pub fn wait_for_target<P: Port>(port: &mut P, target: u32, seconds: f64) -> Result<bool> {
    let deadline = port.clock() + seconds;
    loop {
        for line in port.read()? {
            if parse_line(&line).is_some_and(|parsed| parsed.is_from(target)) {
                return Ok(true);
            }
        }
        if port.clock() >= deadline {
            return Ok(false);
        }
    }
}

/// Sends `line` only when it fits. `Ok(Err(Fail::Line))` means it was not sent.
fn begin<P: Port>(port: &mut P, line: &str) -> Result<StdResult<(), Fail>> {
    if !fits(line) {
        return Ok(Err(Fail::Line));
    }
    port.send(line)?;
    Ok(Ok(()))
}

/// True when the text before the newline fits in the node's line buffer.
pub fn fits(line: &str) -> bool {
    let body = line.strip_suffix('\n').unwrap_or(line);
    !body.contains('\n') && body.len() <= LINE_MAX
}

/// `tallowd gatt --port <p> --target <id> <subcommand> ...`. Opens the port.
pub fn command(argv: &[String]) -> Result<i32> {
    let job = parse_args(argv)?;
    let mut link = Link::open(&job.port)?;
    match run(&mut link, &job, &mut |line| {
        println!("{line}");
    })? {
        Ok(()) => Ok(0),
        Err(fail) => {
            eprintln!("tallowd gatt: {fail}");
            Ok(1)
        }
    }
}

/// One whole command on a port the root has just been reset on.
pub fn run<P: Port>(
    port: &mut P,
    job: &Job,
    report: &mut dyn FnMut(&str),
) -> Result<StdResult<(), Fail>> {
    if !wait_for_target(port, job.target, READY_S)? {
        return Ok(Err(Fail::NotHeard));
    }
    let id = port.next_id();
    if let Err(fail) = exchange(port, job.target, id, &job.command, report)? {
        return Ok(Err(fail));
    }
    match job.listen {
        Some(seconds) => listen(port, job.target, seconds, report),
        None => Ok(Ok(())),
    }
}

/// Prints each notification from `target` for `seconds`. Reading keeps the
/// heartbeat going. A closed link ends it as a failure.
fn listen<P: Port>(
    port: &mut P,
    target: u32,
    seconds: u32,
    report: &mut dyn FnMut(&str),
) -> Result<StdResult<(), Fail>> {
    let start = port.clock();
    let deadline = start + f64::from(seconds);
    loop {
        for line in port.read()? {
            let Some(reply) = reply_from(&line, target) else {
                continue;
            };
            let ms = elapsed_ms(start, port.clock());
            match reply {
                Reply::Notify {
                    handle, len, value, ..
                } => {
                    let hex: String = value
                        .iter()
                        .take(usize::from(len))
                        .map(|byte| format!("{byte:02x}"))
                        .collect();
                    report(&format!("notify {ms} ms handle {handle:#06x} value {hex}"));
                }
                Reply::Closed { .. } => {
                    report(&format!("closed {ms} ms"));
                    return Ok(Err(Fail::Closed));
                }
                Reply::Answer { .. } => {}
            }
        }
        if port.clock() >= deadline {
            return Ok(Ok(()));
        }
    }
}

/// Sends one request and waits for its answer. `report` gets each stage, with
/// the milliseconds since the send. `Ok(Ok(()))` is status ok. `Ok(Err)` is a
/// finished command that was not ok. A port error is the outer `Err`.
pub fn exchange<P: Port>(
    port: &mut P,
    target: u32,
    id: u16,
    command: &Command,
    report: &mut dyn FnMut(&str),
) -> Result<StdResult<(), Fail>> {
    let Some(line) = request_line(target, id, command) else {
        return Ok(Err(Fail::Line));
    };
    if let Err(fail) = begin(port, &line)? {
        return Ok(Err(fail));
    }
    let start = port.clock();
    report(&format!("send {} ms", elapsed_ms(start, port.clock())));
    let deadline = start + timeout_s(command);
    loop {
        for line in port.read()? {
            if let Some(done) = on_line(target, id, &line, start, port.clock(), report) {
                return Ok(done);
            }
        }
        let now = port.clock();
        if now >= deadline {
            report(&format!("timeout {} ms", elapsed_ms(start, now)));
            return Ok(Err(Fail::Timeout));
        }
    }
}

/// The `G` line for one request, or `None` when it will not encode or does not fit.
pub fn request_line(target: u32, id: u16, command: &Command) -> Option<String> {
    let request = request_of(command)?;
    let mut out = [0u8; MAX_PAYLOAD];
    let length = gatt::encode(id, &request, &mut out)?;
    let hex: String = out[..length]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let line = format!("G {target:08x} {hex}\n");
    fits(&line).then_some(line)
}

fn request_of(command: &Command) -> Option<Request> {
    Some(match command {
        Command::Whitelist { addresses } => {
            if addresses.len() > WHITELIST {
                return None;
            }
            let mut slots = [[0u8; 6]; WHITELIST];
            for (slot, address) in slots.iter_mut().zip(addresses) {
                *slot = *address;
            }
            Request::Whitelist {
                count: addresses.len() as u8,
                addresses: slots,
            }
        }
        Command::Connect { address, random } => Request::Connect {
            address: *address,
            address_kind: u8::from(*random),
        },
        Command::Write { handle, value } => {
            if value.len() > VALUE_MAX {
                return None;
            }
            let mut bytes = [0u8; VALUE_MAX];
            bytes[..value.len()].copy_from_slice(value);
            Request::Write {
                handle: *handle,
                len: value.len() as u8,
                value: bytes,
            }
        }
        Command::Read { handle } => Request::Read { handle: *handle },
        Command::Subscribe { handle } => Request::Subscribe { handle: *handle },
        Command::Disconnect => Request::Disconnect,
    })
}

fn timeout_s(command: &Command) -> f64 {
    let ms = match command {
        Command::Connect { .. } => CONNECT_MS,
        _ => REQUEST_MS,
    };
    f64::from(ms) / 1000.0 + GRACE_S
}

fn elapsed_ms(start: f64, now: f64) -> u64 {
    let ms = (now - start) * 1000.0;
    if !ms.is_finite() || ms < 0.0 {
        0
    } else {
        ms as u64
    }
}

/// The GATT reply in one line, when it came from `target`.
fn reply_from(line: &str, target: u32) -> Option<Reply> {
    let parsed = parse_line(line)?;
    if parsed.frame != u32::from(FrameKind::GattUp as u8) || !parsed.is_from(target) {
        return None;
    }
    decode::gatt_up(&parsed.payload)
}

fn on_line(
    target: u32,
    id: u16,
    line: &str,
    start: f64,
    now: f64,
    report: &mut dyn FnMut(&str),
) -> Option<StdResult<(), Fail>> {
    let ms = elapsed_ms(start, now);
    if line.trim() == "@G refused" {
        report(&format!("refused {ms} ms"));
        return Some(Err(Fail::Route));
    }
    match reply_from(line, target)? {
        Reply::Notify { .. } => {
            report(&format!("notify {ms} ms"));
            None
        }
        Reply::Closed { .. } => {
            report(&format!("closed {ms} ms"));
            Some(Err(Fail::Closed))
        }
        Reply::Answer {
            id: answer_id,
            status,
            ..
        } => {
            if answer_id != id {
                return None;
            }
            report(&format!("answer {ms} ms"));
            Some(if status == Status::Ok {
                Ok(())
            } else {
                Err(Fail::Status(status))
            })
        }
    }
}

/// `--port` and `--target` come first. The subcommand and its arguments follow.
pub fn parse_args(argv: &[String]) -> Result<Job> {
    let mut port = None;
    let mut target = None;
    let mut words = argv.iter();
    let mut rest = Vec::new();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--port" => {
                port = Some(words.next().context("--port needs a value")?.clone());
            }
            "--target" => {
                let text = words.next().context("--target needs a node id")?;
                target = Some(node_id(text)?);
            }
            other if other.starts_with("--") => bail!("unknown flag {other}"),
            _ => {
                rest.push(word.clone());
                rest.extend(words.cloned());
                break;
            }
        }
    }
    let port = port.context("--port is required")?;
    let target = target.context("--target is required")?;
    let listen = take_listen(&mut rest)?;
    let command = parse_command(&rest)?;
    ensure!(
        listen.is_none() || matches!(command, Command::Subscribe { .. }),
        "--listen goes with subscribe"
    );
    Ok(Job {
        port,
        target,
        command,
        listen,
    })
}

/// Takes `--listen <s>` out of the subcommand's words.
fn take_listen(words: &mut Vec<String>) -> Result<Option<u32>> {
    let Some(at) = words.iter().position(|word| word == "--listen") else {
        return Ok(None);
    };
    let value = words.get(at + 1).context("--listen needs seconds")?;
    let seconds = value
        .parse()
        .with_context(|| format!("--listen {value} is not a number of seconds"))?;
    words.drain(at..at + 2);
    Ok(Some(seconds))
}

fn parse_command(words: &[String]) -> Result<Command> {
    let (name, args) = words
        .split_first()
        .context("name a subcommand: whitelist, connect, write, read, subscribe, or disconnect")?;
    Ok(match name.as_str() {
        "whitelist" => {
            ensure!(
                args.len() <= WHITELIST,
                "at most {WHITELIST} addresses, got {}",
                args.len()
            );
            Command::Whitelist {
                addresses: args
                    .iter()
                    .map(|text| address(text))
                    .collect::<Result<_>>()?,
            }
        }
        "connect" => {
            ensure!(
                args.len() == 2,
                "connect needs an address and public or random"
            );
            let random = match args[1].as_str() {
                "public" => false,
                "random" => true,
                other => bail!("{other} is not public or random"),
            };
            Command::Connect {
                address: address(&args[0])?,
                random,
            }
        }
        "write" => {
            ensure!(args.len() == 2, "write needs a handle and a hex value");
            let value = unhex(&args[1])?;
            ensure!(
                value.len() <= VALUE_MAX,
                "a write is at most {VALUE_MAX} bytes, got {}",
                value.len()
            );
            Command::Write {
                handle: handle(&args[0])?,
                value,
            }
        }
        "read" => {
            ensure!(args.len() == 1, "read needs a handle");
            Command::Read {
                handle: handle(&args[0])?,
            }
        }
        "subscribe" => {
            ensure!(args.len() == 1, "subscribe needs a handle");
            Command::Subscribe {
                handle: handle(&args[0])?,
            }
        }
        "disconnect" => {
            ensure!(args.is_empty(), "disconnect takes no arguments");
            Command::Disconnect
        }
        other => bail!("unknown subcommand {other}"),
    })
}

fn node_id(text: &str) -> Result<u32> {
    let hex = text.strip_prefix("0x").unwrap_or(text);
    u32::from_str_radix(hex, 16).with_context(|| format!("{text} is not a node id"))
}

fn handle(text: &str) -> Result<u16> {
    let parsed = if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16)
    } else {
        text.parse()
    };
    parsed.with_context(|| format!("{text} is not a handle"))
}

/// Six colon-separated hex bytes, the same spelling as `watch_line`.
fn address(text: &str) -> Result<[u8; 6]> {
    let pairs: Vec<&str> = text.split(':').collect();
    ensure!(pairs.len() == 6, "{text} is not six hex bytes");
    let mut out = [0u8; 6];
    for (index, pair) in pairs.iter().enumerate() {
        ensure!(pair.len() == 2, "{text} is not six hex bytes");
        out[index] = u8::from_str_radix(pair, 16).with_context(|| format!("{text} is not hex"))?;
    }
    Ok(out)
}

fn unhex(text: &str) -> Result<Vec<u8>> {
    ensure!(
        text.len().is_multiple_of(2),
        "write value needs an even number of hex digits"
    );
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(text.len() / 2);
    let mut at = 0;
    while at < bytes.len() {
        let hi = hex_digit(bytes[at]).context("write value is not hex")?;
        let lo = hex_digit(bytes[at + 1]).context("write value is not hex")?;
        out.push((hi << 4) | lo);
        at += 2;
    }
    Ok(out)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tallow_mesh_core::gatt::{self, Reply, Request, Status, VALUE_MAX};
    use tallow_mesh_core::wire::MAX_PAYLOAD;

    struct Fake {
        now: f64,
        step: f64,
        lines: Vec<String>,
        sent: Vec<String>,
    }

    impl Port for Fake {
        fn clock(&self) -> f64 {
            self.now
        }

        fn read(&mut self) -> Result<Vec<String>> {
            self.now += self.step;
            Ok(std::mem::take(&mut self.lines))
        }

        fn send(&mut self, line: &str) -> Result<()> {
            self.sent.push(line.to_string());
            Ok(())
        }

        fn next_id(&mut self) -> u16 {
            1
        }
    }

    /// A root just after its reset. It is not root until it has read a heartbeat,
    /// and it has no route until the target has sent up. Until then it refuses,
    /// as `send_down` does. `heard_at` is when the target's first frame arrives.
    struct Root {
        now: f64,
        heard_at: f64,
        from: u32,
        rooted: bool,
        routed: bool,
        inbox: Vec<String>,
        sent: Vec<String>,
        next: u16,
    }

    impl Root {
        fn new(heard_at: f64) -> Self {
            Self {
                now: 0.0,
                heard_at,
                from: TARGET,
                rooted: false,
                routed: false,
                inbox: Vec::new(),
                sent: Vec::new(),
                next: 40,
            }
        }
    }

    impl Port for Root {
        fn clock(&self) -> f64 {
            self.now
        }

        fn read(&mut self) -> Result<Vec<String>> {
            self.now += 1.0;
            self.rooted = true;
            if self.now >= self.heard_at && !self.routed {
                self.routed = self.from == TARGET;
                return Ok(vec![format!("@U 2 {:08x} 1 1 1 -40", self.from)]);
            }
            Ok(std::mem::take(&mut self.inbox))
        }

        fn send(&mut self, line: &str) -> Result<()> {
            if !self.rooted || !self.routed {
                self.inbox.push("@G refused".into());
                return Ok(());
            }
            self.sent.push(line.to_string());
            let (id, _) = gatt::parse(&payload(line)).expect("parse");
            self.inbox.push(up(answer(id, Status::Ok)));
            Ok(())
        }

        fn next_id(&mut self) -> u16 {
            self.next = self.next.wrapping_add(1);
            self.next
        }
    }

    const TARGET: u32 = 0x0a0b_0c0d;

    fn job(command: Command, listen: Option<u32>) -> Job {
        Job {
            port: "/dev/ttyUSB0".into(),
            target: TARGET,
            command,
            listen,
        }
    }

    fn up(reply: Reply) -> String {
        let mut out = [0u8; MAX_PAYLOAD];
        let length = reply.encode(&mut out);
        let hex: String = out[..length]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("@U 12 {TARGET:08x} 1 1 1 -40 {hex}")
    }

    fn answer(id: u16, status: Status) -> Reply {
        Reply::Answer {
            id,
            status,
            len: 0,
            value: [0; VALUE_MAX],
        }
    }

    fn run(
        command: Command,
        lines: Vec<String>,
        step: f64,
    ) -> (Vec<String>, StdResult<(), Fail>, Vec<String>) {
        let mut port = Fake {
            now: 0.0,
            step,
            lines,
            sent: Vec::new(),
        };
        let mut stages = Vec::new();
        let result = exchange(&mut port, TARGET, 1, &command, &mut |line| {
            stages.push(line.to_string());
        })
        .expect("the fake port does not fail");
        (stages, result, port.sent)
    }

    fn payload(line: &str) -> Vec<u8> {
        let hex = line.split_whitespace().nth(2).expect("hex");
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn each_request_line_parses_back_through_the_core() {
        let address = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
        let commands = [
            Command::Whitelist {
                addresses: vec![address],
            },
            Command::Connect {
                address,
                random: true,
            },
            Command::Write {
                handle: 0x21,
                value: vec![1, 2, 3],
            },
            Command::Read { handle: 0x21 },
            Command::Subscribe { handle: 0x12 },
            Command::Disconnect,
        ];
        for command in commands {
            let line = request_line(0x0a0b_0c0d, 7, &command).expect("line");
            let (id, request) = gatt::parse(&payload(&line)).expect("parse");
            assert_eq!(id, 7);
            assert_eq!(request, request_of(&command).unwrap());
        }
    }

    #[test]
    fn the_largest_write_fits_an_eight_digit_id() {
        let line = request_line(
            0xffff_ffff,
            1,
            &Command::Write {
                handle: 0x21,
                value: vec![0x5a; 170],
            },
        )
        .expect("the 170 byte write fits");
        let text = line.trim_end_matches('\n');
        assert_eq!(text.len(), 361);
        assert!(text.len() <= LINE_MAX);
        let (id, request) = gatt::parse(&payload(&line)).expect("parse");
        assert_eq!(id, 1);
        match request {
            Request::Write { len, .. } => assert_eq!(len, 170),
            other => panic!("not a write: {other:?}"),
        }
    }

    #[test]
    fn a_line_of_401_characters_is_refused_before_it_is_sent() {
        let line = format!("{}\n", "g".repeat(401));
        let mut port = Fake {
            now: 0.0,
            step: 1.0,
            lines: Vec::new(),
            sent: Vec::new(),
        };
        assert_eq!(
            begin(&mut port, &line).expect("no port error"),
            Err(Fail::Line)
        );
        assert!(port.sent.is_empty());
    }

    #[test]
    fn an_ok_answer_is_a_stage_and_success() {
        let (stages, result, _) = run(
            Command::Read { handle: 1 },
            vec![up(answer(1, Status::Ok))],
            0.01,
        );
        assert_eq!(result, Ok(()));
        assert_eq!(stages, ["send 0 ms", "answer 10 ms"]);
    }

    #[test]
    fn a_notification_is_a_stage_and_the_answer_still_decides() {
        let mut value = [0u8; tallow_mesh_core::gatt::NOTIFY_MAX];
        value[0] = 0xff;
        let (stages, result, _) = run(
            Command::Subscribe { handle: 1 },
            vec![
                up(Reply::Notify {
                    link: 1,
                    handle: 0x21,
                    len: 1,
                    value,
                }),
                up(answer(1, Status::Ok)),
            ],
            0.02,
        );
        assert_eq!(result, Ok(()));
        assert_eq!(stages, ["send 0 ms", "notify 20 ms", "answer 20 ms"]);
    }

    #[test]
    fn a_closed_link_fails() {
        let (stages, result, _) = run(
            Command::Disconnect,
            vec![up(Reply::Closed { link: 1 })],
            0.01,
        );
        assert_eq!(result, Err(Fail::Closed));
        assert_eq!(stages.last().map(String::as_str), Some("closed 10 ms"));
    }

    #[test]
    fn a_wait_with_no_answer_times_out() {
        let (stages, result, _) = run(Command::Read { handle: 1 }, Vec::new(), 5.0);
        assert_eq!(result, Err(Fail::Timeout));
        assert!(stages.iter().any(|stage| stage.starts_with("timeout ")));
    }

    #[test]
    fn a_refused_address_is_not_ok() {
        let (stages, result, _) = run(
            Command::Connect {
                address: [1, 2, 3, 4, 5, 6],
                random: false,
            },
            vec![up(answer(1, Status::Refused))],
            0.01,
        );
        assert_eq!(result, Err(Fail::Status(Status::Refused)));
        assert_eq!(stages.last().map(String::as_str), Some("answer 10 ms"));
    }

    #[test]
    fn the_root_refusing_the_line_fails() {
        let (stages, result, _) = run(Command::Disconnect, vec!["@G refused".into()], 0.01);
        assert_eq!(result, Err(Fail::Route));
        assert_eq!(stages.last().map(String::as_str), Some("refused 10 ms"));
    }

    #[test]
    fn a_command_waits_for_the_target_before_it_sends() {
        let mut port = Root::new(3.0);
        let result =
            super::run(&mut port, &job(Command::Disconnect, None), &mut |_| {}).expect("port");
        assert_eq!(result, Ok(()));
        assert_eq!(port.sent.len(), 1);
        assert!(port.inbox.iter().all(|line| line != "@G refused"));
    }

    #[test]
    fn a_target_that_never_speaks_fails_without_sending() {
        let mut port = Root::new(f64::INFINITY);
        let result =
            super::run(&mut port, &job(Command::Disconnect, None), &mut |_| {}).expect("port");
        assert_eq!(result, Err(Fail::NotHeard));
        assert!(port.now >= READY_S);
        assert!(port.sent.is_empty());
        assert!(port.inbox.is_empty());
    }

    #[test]
    fn another_nodes_frame_does_not_count_as_the_target_being_heard() {
        let mut port = Root::new(1.0);
        port.from = 5;
        let result =
            super::run(&mut port, &job(Command::Disconnect, None), &mut |_| {}).expect("port");
        assert_eq!(result, Err(Fail::NotHeard));
    }

    #[test]
    fn two_requests_use_two_ids() {
        let mut port = Root::new(1.0);
        let request = job(Command::Read { handle: 1 }, None);
        super::run(&mut port, &request, &mut |_| {})
            .expect("port")
            .expect("first");
        // A second run resets the root, so it has to hear the target again.
        port.routed = false;
        super::run(&mut port, &request, &mut |_| {})
            .expect("port")
            .expect("second");
        let ids: Vec<u16> = port
            .sent
            .iter()
            .map(|line| gatt::parse(&payload(line)).expect("parse").0)
            .collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        assert!(ids.iter().all(|id| *id != 1));
    }

    #[test]
    fn another_nodes_answer_and_closed_link_are_not_this_commands() {
        let other = |line: String| line.replace(&format!("{TARGET:08x}"), "00000005");
        let (_, result, _) = run(
            Command::Read { handle: 1 },
            vec![
                other(up(Reply::Closed { link: 1 })),
                other(up(answer(1, Status::Refused))),
                up(answer(1, Status::Ok)),
            ],
            0.01,
        );
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn the_host_outlasts_the_nodes_own_timeout() {
        for command in [
            Command::Read { handle: 1 },
            Command::Connect {
                address: [1; 6],
                random: false,
            },
        ] {
            let node_ms = if matches!(command, Command::Connect { .. }) {
                CONNECT_MS
            } else {
                REQUEST_MS
            };
            assert!(timeout_s(&command) >= f64::from(node_ms) / 1000.0 + 2.0);
        }
    }

    #[test]
    fn listen_prints_each_notification_until_its_time_is_up() {
        let mut value = [0u8; tallow_mesh_core::gatt::NOTIFY_MAX];
        value[0] = 0xab;
        let notify = up(Reply::Notify {
            link: 1,
            handle: 0x21,
            len: 1,
            value,
        });
        let mut port = Fake {
            now: 0.0,
            step: 1.0,
            lines: vec![
                notify.clone(),
                notify.replace(&format!("{TARGET:08x}"), "00000005"),
            ],
            sent: Vec::new(),
        };
        let mut stages = Vec::new();
        let result = listen(&mut port, TARGET, 3, &mut |line| {
            stages.push(line.to_string());
        })
        .expect("port");
        assert_eq!(result, Ok(()));
        assert_eq!(stages, ["notify 1000 ms handle 0x0021 value ab"]);
        assert!(port.now >= 3.0);
    }

    #[test]
    fn a_closed_link_ends_a_listen_as_a_failure() {
        let mut port = Fake {
            now: 0.0,
            step: 1.0,
            lines: vec![up(Reply::Closed { link: 1 })],
            sent: Vec::new(),
        };
        let result = listen(&mut port, TARGET, 3, &mut |_| {}).expect("port");
        assert_eq!(result, Err(Fail::Closed));
    }

    #[test]
    fn listen_goes_with_subscribe_only() {
        let job = parse_args(&words(
            "--port /dev/ttyUSB0 --target 0a0b0c0d subscribe 0x21 --listen 30",
        ))
        .expect("subscribe");
        assert_eq!(job.listen, Some(30));
        assert_eq!(job.command, Command::Subscribe { handle: 0x21 });
        assert!(parse_args(&words("--port p --target 1 read 0x21 --listen 30")).is_err());
        assert!(parse_args(&words("--port p --target 1 subscribe 0x21 --listen")).is_err());
        assert!(parse_args(&words("--port p --target 1 subscribe 0x21 --listen soon")).is_err());
    }

    #[test]
    fn the_arguments_name_the_port_the_target_and_the_subcommand() {
        let job = parse_args(&words(
            "--port /dev/ttyUSB0 --target 0a0b0c0d connect AA:BB:CC:DD:EE:FF public",
        ))
        .expect("args");
        assert_eq!(job.port, "/dev/ttyUSB0");
        assert_eq!(job.target, 0x0a0b_0c0d);
        assert_eq!(
            job.command,
            Command::Connect {
                address: [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
                random: false,
            }
        );
        let write = parse_args(&words(
            "--target ffffffff --port /dev/ttyUSB1 write 0x21 0102",
        ))
        .expect("write");
        assert_eq!(
            write.command,
            Command::Write {
                handle: 0x21,
                value: vec![1, 2],
            }
        );
    }

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }
}
