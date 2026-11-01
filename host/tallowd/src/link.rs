//! The root node's serial port.
//!
//! Opening a CP2102 resets the board, so [`Link::open`] clears DTR and RTS
//! and waits 4 s before the first heartbeat. Parsing is [`parse_line`], which
//! needs no port.

use anyhow::Context;
use serialport::SerialPort;
use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

/// One `@U` line from the root. Other `@` lines are noise to this parser.
#[derive(Debug, PartialEq, Eq)]
pub struct Line {
    pub frame: u32,
    pub origin: String,
    pub boot: u32,
    pub seq: u32,
    pub hops: u32,
    pub rssi: i32,
    pub payload: Vec<u8>,
}

/// The root's serial port. Opening it resets the board.
///
/// T2.3 is what calls [`Link::open`] from the `run` command. Until then the
/// port stays unused, and the checks below cover parsing and the heartbeat.
#[allow(dead_code)]
pub struct Link {
    port: Box<dyn SerialPort>,
    buffer: Vec<u8>,
    next_beat: Instant,
}

#[allow(dead_code)]
impl Link {
    /// Open `path` at 115200 with DTR and RTS clear, then wait out the reset.
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let mut port = serialport::new(path, 115_200)
            .timeout(Duration::from_millis(50))
            .dtr_on_open(false)
            .open()
            .with_context(|| format!("open {path}"))?;
        port.write_data_terminal_ready(false).context("clear DTR")?;
        port.write_request_to_send(false).context("clear RTS")?;
        // Linux still pulses DTR while opening, which resets the CP2102.
        // Anything written during those 4 s is lost in boot.
        std::thread::sleep(Duration::from_secs(4));
        Ok(Self {
            port,
            buffer: Vec::new(),
            next_beat: Instant::now(),
        })
    }

    /// Send `H\n` when a second has passed, then return complete `@` lines.
    pub fn lines(&mut self) -> anyhow::Result<Vec<String>> {
        if arm_heartbeat(&mut self.next_beat, Instant::now()) {
            self.port.write_all(b"H\n")?;
            self.port.flush()?;
        }
        let mut chunk = [0u8; 4096];
        match self.port.read(&mut chunk) {
            Ok(size) => {
                if let Some(bytes) = chunk.get(..size) {
                    self.buffer.extend_from_slice(bytes);
                }
            }
            Err(err) if err.kind() == ErrorKind::TimedOut => {}
            Err(err) => return Err(err.into()),
        }
        Ok(take_at_lines(&mut self.buffer))
    }
}

/// True when a heartbeat is due. The next one is one second after this call,
/// matching `Link.lines` in `tools/mesh_host.py`.
fn arm_heartbeat(next: &mut Instant, now: Instant) -> bool {
    if now < *next {
        return false;
    }
    *next = now + Duration::from_secs(1);
    true
}

fn take_at_lines(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut lines = Vec::new();
    while let Some(end) = buffer.iter().position(|&byte| byte == b'\n') {
        let mut raw: Vec<u8> = buffer.drain(..=end).collect();
        raw.pop();
        let text = String::from_utf8_lossy(&raw);
        let line = text.trim();
        if line.starts_with('@') {
            lines.push(line.to_string());
        }
    }
    lines
}

pub fn parse_line(line: &str) -> Option<Line> {
    let mut words = line.split(' ');
    if words.next()? != "@U" {
        return None;
    }
    let frame = words.next()?.parse().ok()?;
    let origin = words.next()?.to_string();
    let boot = words.next()?.parse().ok()?;
    let seq = words.next()?.parse().ok()?;
    let hops = words.next()?.parse().ok()?;
    let rssi = words.next()?.parse().ok()?;
    let payload = match words.next() {
        Some(hex) => decode_hex(hex)?,
        None => Vec::new(),
    };
    Some(Line {
        frame,
        origin,
        boot,
        seq,
        hops,
        rssi,
        payload,
    })
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let mut bytes = text.bytes();
    let mut out = Vec::with_capacity(text.len() / 2);
    while let Some(hi) = bytes.next() {
        let lo = bytes.next()?;
        out.push((hex_digit(hi)? << 4) | hex_digit(lo)?);
    }
    Some(out)
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

    #[test]
    fn serial_noise_is_not_an_up_line() {
        for line in [
            "",
            "W (735) wifi:Password",
            "@U 2",
            "@U x y z 1 2 3 zz",
            "@B 642104ca 3594854591",
        ] {
            assert_eq!(parse_line(line), None);
        }
    }

    #[test]
    fn an_up_line_keeps_the_frame_and_payload() {
        let line = parse_line("@U 2 541e99a9 123 7 1 -60 0a0b").expect("up line");
        assert_eq!(line.frame, 2);
        assert_eq!(line.origin, "541e99a9");
        assert_eq!(line.boot, 123);
        assert_eq!(line.seq, 7);
        assert_eq!(line.hops, 1);
        assert_eq!(line.rssi, -60);
        assert_eq!(line.payload, [0x0a, 0x0b]);
    }

    #[test]
    fn a_boot_id_past_i32_max_still_parses() {
        let line = parse_line("@U 2 541e99a9 3594854591 7 1 -60").expect("boot");
        assert_eq!(line.boot, 3_594_854_591);
        assert!(line.payload.is_empty());
    }

    #[test]
    fn serial_noise_is_not_yielded() {
        let mut buffer = b"\nW (735) wifi:Password\n@U 2\n@B 642104ca 3594854591\npartial".to_vec();
        let lines = take_at_lines(&mut buffer);
        assert_eq!(lines, ["@U 2", "@B 642104ca 3594854591"]);
        assert_eq!(buffer, b"partial");
    }

    #[test]
    fn the_heartbeat_is_due_once_a_second() {
        let start = Instant::now();
        let mut next = start;
        assert!(arm_heartbeat(&mut next, start));
        assert!(!arm_heartbeat(&mut next, start));
        assert!(!arm_heartbeat(
            &mut next,
            start + Duration::from_millis(999)
        ));
        assert!(arm_heartbeat(&mut next, start + Duration::from_secs(1)));
    }
}
