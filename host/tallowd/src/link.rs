//! The root node's serial port.
//!
//! Opening a CP2102 resets the board, so [`Link::open`] clears DTR and RTS
//! and waits 4 s before the first heartbeat. Parsing is [`parse_line`], which
//! needs no port.

use crate::trial::Wire;
use anyhow::{ensure, Context};
use serialport::SerialPort;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The longest line this keeps. The firmware's longest is about 485 characters:
/// a 218-byte payload as hex, plus a header.
const MAX_LINE: usize = 2048;

/// The most Classic addresses one paging list holds. The firmware refuses more.
/// Matches `WATCH_MAX` in `tools/mesh_host.py`.
pub const WATCH_MAX: usize = 16;

/// Seconds since the epoch, as `time.time()` gives it in Python.
pub fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// The settings every run sends, because other nodes keep their last ones across
/// the root's reset. Matches `DEFAULT_SCAN` in `tools/mesh_host.py`.
pub const DEFAULT_SCAN: ScanSettings = ScanSettings {
    window_ms: 30,
    interval_ms: 100,
    inquiry_period_s: 60,
    inquiry_length: 4,
    probe_hz: 0,
    probe_size: 8,
};

/// What a `C` line sets on every node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanSettings {
    pub window_ms: u16,
    pub interval_ms: u16,
    pub inquiry_period_s: u16,
    pub inquiry_length: u8,
    pub probe_hz: u8,
    pub probe_size: u8,
}

/// One `@U` line from the root. Other `@` lines are noise to this parser.
#[derive(Debug, PartialEq, Eq)]
pub struct Line {
    pub frame: u32,
    pub origin: String,
    /// Not read yet: the map keys on the origin, not on the boot or sequence.
    #[allow(dead_code)]
    pub boot: u32,
    #[allow(dead_code)]
    pub seq: u32,
    pub hops: u32,
    pub rssi: i32,
    pub payload: Vec<u8>,
}

/// The root's serial port. Opening it resets the board.
pub struct Link {
    port: Box<dyn SerialPort>,
    buffer: LineBuffer,
    next_beat: Instant,
    command_id: u16,
}

impl Link {
    /// Open `path` at 115200 with DTR and RTS clear, then wait out the reset.
    pub fn open(path: &str) -> anyhow::Result<Self> {
        check_port(path)?;
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
            buffer: LineBuffer::default(),
            next_beat: Instant::now(),
            command_id: first_command_id(),
        })
    }

    /// Send scan settings to every node. Returns the command id, which a node's
    /// stats echo once it has them.
    pub fn send_command(&mut self, settings: &ScanSettings) -> anyhow::Result<u16> {
        self.command_id = self.command_id.wrapping_add(1);
        self.port
            .write_all(command_line(self.command_id, settings).as_bytes())?;
        self.port.flush()?;
        Ok(self.command_id)
    }

    /// Send a line from [`watch_line`]. The root floods it to every node.
    pub fn send_watch(&mut self, line: &[u8]) -> anyhow::Result<()> {
        self.port.write_all(line)?;
        self.port.flush()?;
        Ok(())
    }

    /// Send `H\n` when a second has passed, then return complete `@` lines.
    pub fn lines(&mut self) -> anyhow::Result<Vec<String>> {
        if arm_heartbeat(&mut self.next_beat, Instant::now()) {
            self.port.write_all(b"H\n")?;
            self.port.flush()?;
        }
        let mut chunk = [0u8; 4096];
        match self.port.read(&mut chunk) {
            Ok(size) => Ok(chunk
                .get(..size)
                .map_or_else(Vec::new, |bytes| self.buffer.push(bytes))),
            Err(err) if err.kind() == ErrorKind::TimedOut => Ok(Vec::new()),
            Err(err) => Err(err.into()),
        }
    }
}

impl Wire for Link {
    fn clock(&self) -> f64 {
        unix_now()
    }

    fn read(&mut self) -> anyhow::Result<Vec<String>> {
        self.lines()
    }

    fn command(&mut self, settings: &ScanSettings) -> anyhow::Result<u16> {
        self.send_command(settings)
    }
}

/// Only a classic board's USB-serial port. Opening an S3 cam's `ttyACM` port can
/// drop it into download mode, so this refuses before anything opens. A udev
/// symlink such as `/dev/tallow-root` is judged by the device it points at.
fn check_port(path: &str) -> anyhow::Result<()> {
    let target = std::fs::canonicalize(path).ok();
    let device = target.as_deref().and_then(Path::to_str).unwrap_or(path);
    if device.starts_with("/dev/ttyUSB") && !path.contains("..") {
        return Ok(());
    }
    anyhow::bail!("{path} is not a classic ESP32 port. The cams are not mesh nodes.")
}

/// The paging list line: `W <hex12> ...` with a newline, or `W` alone, which
/// clears the list. Each address is six colon-separated hex pairs. Anything else
/// is refused here, before the port opens, since opening it resets the root.
/// Matches `watch_line` in `tools/mesh_host.py`.
pub fn watch_line(addresses: &[String]) -> anyhow::Result<Vec<u8>> {
    ensure!(
        addresses.len() <= WATCH_MAX,
        "at most {WATCH_MAX} addresses, got {}",
        addresses.len()
    );
    let mut line = String::from("W");
    for text in addresses {
        let pairs: Vec<&str> = text.split(':').collect();
        ensure!(
            pairs.len() == 6
                && pairs
                    .iter()
                    .all(|pair| pair.len() == 2 && pair.bytes().all(|b| b.is_ascii_hexdigit())),
            "not an address: {text:?}; use six hex pairs, such as AA:BB:CC:DD:EE:FF"
        );
        line.push(' ');
        line.push_str(&pairs.concat().to_ascii_lowercase());
    }
    line.push('\n');
    Ok(line.into_bytes())
}

/// A fresh id each run, in the low half so the first few cannot wrap. Matches
/// `Link` in `tools/mesh_host.py`.
fn first_command_id() -> u16 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    (seconds & 0x7FFF) as u16
}

fn command_line(id: u16, settings: &ScanSettings) -> String {
    format!(
        "C {id} {} {} {} {} {} {}\n",
        settings.window_ms,
        settings.interval_ms,
        settings.inquiry_period_s,
        settings.inquiry_length,
        settings.probe_hz,
        settings.probe_size
    )
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

/// Bytes off the port, cut into `@` lines. A line that runs past [`MAX_LINE`] is
/// thrown away up to its newline, so a port that never sends one cannot grow this.
#[derive(Default)]
struct LineBuffer {
    bytes: Vec<u8>,
    skipping: bool,
}

impl LineBuffer {
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        for piece in chunk.split_inclusive(|&byte| byte == b'\n') {
            if !self.skipping {
                self.bytes.extend_from_slice(piece);
            }
            let ended = piece.last() == Some(&b'\n');
            if !self.skipping && self.bytes.len() > MAX_LINE {
                self.bytes.clear();
                self.skipping = !ended;
                continue;
            }
            if ended {
                if !self.skipping {
                    let text = String::from_utf8_lossy(&self.bytes);
                    let line = text.trim();
                    if line.starts_with('@') {
                        lines.push(line.to_string());
                    }
                }
                self.bytes.clear();
                self.skipping = false;
            }
        }
        lines
    }
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
        let mut buffer = LineBuffer::default();
        let lines = buffer.push(b"\nW (735) wifi:Password\n@U 2\n@B 642104ca 3594854591\npartial");
        assert_eq!(lines, ["@U 2", "@B 642104ca 3594854591"]);
        assert_eq!(buffer.bytes, b"partial");
    }

    #[test]
    fn a_line_split_across_reads_comes_out_whole() {
        let mut buffer = LineBuffer::default();
        assert!(buffer.push(b"@U 2 5").is_empty());
        assert_eq!(buffer.push(b"41e\r\n"), ["@U 2 541e"]);
    }

    #[test]
    fn bytes_that_are_not_text_do_not_break_the_next_line() {
        let mut buffer = LineBuffer::default();
        assert_eq!(
            buffer.push(b"@U \xff\xfe 2\n@P 00000000\n"),
            ["@U \u{fffd}\u{fffd} 2", "@P 00000000"]
        );
    }

    #[test]
    fn a_port_that_never_sends_a_newline_cannot_grow_the_buffer() {
        let mut buffer = LineBuffer::default();
        for _ in 0..1000 {
            assert!(buffer.push(&[b'x'; 4096]).is_empty());
            assert!(buffer.bytes.len() <= MAX_LINE);
        }
    }

    #[test]
    fn the_line_after_an_overlong_one_still_arrives() {
        let mut buffer = LineBuffer::default();
        assert!(buffer.push(&[b'@'; MAX_LINE + 1]).is_empty());
        assert!(buffer.push(b"tail of the long line\n").is_empty());
        assert_eq!(buffer.push(b"@P 00000000\n"), ["@P 00000000"]);
    }

    #[test]
    fn an_overlong_line_ending_in_one_read_is_dropped() {
        let mut long = vec![b'@'; MAX_LINE + 1];
        long.extend_from_slice(b"\n@P 00000000\n");
        let mut buffer = LineBuffer::default();
        assert_eq!(buffer.push(&long), ["@P 00000000"]);
    }

    #[test]
    fn the_longest_real_line_is_kept() {
        let payload = "ab".repeat(218);
        let line = format!("@U 5 541e99a9 4294967295 4294967295 255 -127 {payload}");
        let mut buffer = LineBuffer::default();
        assert_eq!(buffer.push(format!("{line}\n").as_bytes()), [line]);
    }

    #[test]
    fn only_a_classic_boards_port_is_opened() {
        assert!(check_port("/dev/ttyUSB0").is_ok());
        for path in [
            "/dev/ttyACM0",
            "/dev/ttyS0",
            "/tmp/port",
            "/dev/ttyUSB0/../ttyACM0",
        ] {
            assert!(check_port(path).is_err(), "{path}");
        }
    }

    #[test]
    fn a_symlink_is_judged_by_what_it_points_at() {
        let dir = std::env::temp_dir().join(format!("tallow-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let cam = dir.join("ttyACM0");
        std::fs::write(&cam, b"").expect("fake port");
        // Named like a classic port, but it leads somewhere else.
        let disguised = dir.join("ttyUSB0");
        std::os::unix::fs::symlink(&cam, &disguised).expect("symlink");
        let refused = check_port(disguised.to_str().expect("utf-8")).is_err();
        std::fs::remove_dir_all(&dir).expect("clean up");
        assert!(refused);
    }

    #[test]
    fn a_command_line_is_what_the_firmware_parses() {
        assert_eq!(command_line(5, &DEFAULT_SCAN), "C 5 30 100 60 4 0 8\n");
    }

    #[test]
    fn the_longest_command_fits_the_firmwares_line_buffer() {
        let settings = ScanSettings {
            window_ms: u16::MAX,
            interval_ms: u16::MAX,
            inquiry_period_s: u16::MAX,
            inquiry_length: u8::MAX,
            probe_hz: u8::MAX,
            probe_size: u8::MAX,
        };
        assert!(command_line(u16::MAX, &settings).len() <= 63);
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

    fn addresses(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| text.to_string()).collect()
    }

    #[test]
    fn addresses_become_one_line_of_hex_pairs() {
        let line = watch_line(&addresses(&["AA:BB:CC:DD:EE:01", "10:20:30:40:50:60"]));
        assert_eq!(line.expect("line"), b"W aabbccddee01 102030405060\n");
    }

    #[test]
    fn case_does_not_change_the_line() {
        let lower = watch_line(&addresses(&["aa:bb:cc:dd:ee:01"])).expect("lower");
        let upper = watch_line(&addresses(&["AA:BB:CC:DD:EE:01"])).expect("upper");
        assert_eq!(lower, upper);
    }

    #[test]
    fn no_address_is_a_line_that_clears_the_list() {
        assert_eq!(watch_line(&[]).expect("clear"), b"W\n");
    }

    #[test]
    fn sixteen_addresses_fit_the_firmware_line() {
        let texts: Vec<String> = (0..16)
            .map(|index| format!("00:00:00:00:00:{index:02X}"))
            .collect();
        let line = watch_line(&texts).expect("sixteen");
        let line = line.strip_suffix(b"\n").expect("newline");
        // The firmware reads at most 220 bytes of a line into its buffer.
        assert_eq!(line.len(), 2 + 16 * 13 - 1);
        assert!(line.len() <= 220);
    }

    #[test]
    fn seventeen_addresses_are_refused() {
        let texts: Vec<String> = (0..17)
            .map(|index| format!("00:00:00:00:00:{index:02X}"))
            .collect();
        let error = watch_line(&texts).expect_err("seventeen");
        assert!(error.to_string().contains("at most 16"), "{error}");
    }

    #[test]
    fn bad_addresses_are_refused() {
        let bad = [
            "AA:BB:CC:DD:EE",       // five pairs
            "AA:BB:CC:DD:EE:FF:00", // seven pairs
            "AA:BB:CC:DD:EE:GG",    // not hex
            "AA:BB:CC:DD:EE:F",     // a single digit
            "AA:BB:CC:DD:EE:FFF",   // three digits
            "AABBCCDDEEFF",         // no separators
            "AA-BB-CC-DD-EE-FF",    // dashes
            " AA:BB:CC:DD:EE:FF",   // leading space
            "AA:BB:CC:DD:EE:FF\nW", // a newline inside the address
            "",
        ];
        for text in bad {
            assert!(watch_line(&addresses(&[text])).is_err(), "{text:?}");
        }
    }

    #[test]
    fn one_bad_address_refuses_the_whole_list() {
        assert!(watch_line(&addresses(&["AA:BB:CC:DD:EE:FF", "AA:BB:CC:DD:EE"])).is_err());
    }
}
