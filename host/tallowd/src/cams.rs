//! Toby's cams: the inventory, and one shared upstream connection per cam.
//! Port of app/cams.py. The cam's web server holds one connection at a time, so
//! every viewer of a cam shares one `Hub`, and the hub dials only as the Python does.
//!
//! The HTTP response that wraps a `Feed` belongs to the panel routes (T3.6).

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::{sleep, timeout, Instant};

pub const STREAM_PORT: u16 = 8080;
/// The ESPHome API, served by the node's main loop rather than the one-socket camera server.
pub const NODE_PORT: u16 = 6053;
pub const VIEWERS_MAX: usize = 8;
/// Hang up this long after the last viewer leaves, so a tab switch does not churn the cam.
pub const LINGER: Duration = Duration::from_secs(10);
/// Pause between dials after a failure. The n-th failure waits COOLDOWN[n-1], then the last value repeats.
pub const COOLDOWN: &[Duration] = &[
    Duration::from_secs(3),
    Duration::from_secs(6),
    Duration::from_secs(12),
    Duration::from_secs(24),
    Duration::from_secs(30),
];
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
pub const FRAME_TIMEOUT: Duration = Duration::from_secs(10);
/// The reachability probe, `reachable` in app/cams.py.
pub const STATUS_TIMEOUT: Duration = Duration::from_millis(1500);
pub const NAME_MAX: usize = 40;
pub const TITLE_MAX: usize = 40;
pub const DEFAULT_TITLE: &str = "Cameras";
pub const HEAD_MAX: usize = 8 * 1024;
pub const FRAME_MAX: usize = 2 * 1024 * 1024;
pub const BOUNDARY: &str = "tallow";
/// Python's asyncio StreamReader default limit, which readuntil enforces.
const LINE_LIMIT: usize = 64 * 1024;
/// A viewer's queue holds this many frames; the oldest is dropped when it is full.
const QUEUE_DEPTH: usize = 2;
const IOT_BASE: Ipv4Addr = Ipv4Addr::new(10, 87, 0, 0);
const IOT_BROADCAST: Ipv4Addr = Ipv4Addr::new(10, 87, 0, 255);
const AP_ADDR: Ipv4Addr = Ipv4Addr::new(10, 87, 0, 1);

/// The timing and limits a hub runs with. `Tuning::default()` is the production
/// set; tests shorten the timers so they run on a paused clock.
#[derive(Debug, Clone, Copy)]
pub struct Tuning {
    pub linger: Duration,
    pub cooldown: &'static [Duration],
    pub connect_timeout: Duration,
    pub header_timeout: Duration,
    pub frame_timeout: Duration,
    pub viewers_max: usize,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            linger: LINGER,
            cooldown: COOLDOWN,
            connect_timeout: CONNECT_TIMEOUT,
            header_timeout: HEADER_TIMEOUT,
            frame_timeout: FRAME_TIMEOUT,
            viewers_max: VIEWERS_MAX,
        }
    }
}

/// One cam. `address` is a host; the ports are the stream port and the node's API port.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cam {
    pub id: String,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub node_port: u16,
}

impl Cam {
    /// A cam on the production ports. The inventory builds these, after the address guard.
    pub fn new(id: &str, name: &str, address: &str) -> Cam {
        Cam {
            id: id.to_owned(),
            name: name.to_owned(),
            address: address.to_owned(),
            port: STREAM_PORT,
            node_port: NODE_PORT,
        }
    }

    pub fn with_ports(mut self, port: u16, node_port: u16) -> Cam {
        self.port = port;
        self.node_port = node_port;
        self
    }
}

/// The cam did not answer with a stream. The message is the Python one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CamDown(pub String);

impl std::fmt::Display for CamDown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CamDown {}

/// A bad inventory (the message is the Python one) or a file that could not be read or written.
#[derive(Debug)]
pub enum InventoryError {
    Invalid(String),
    Io(io::Error),
}

impl std::fmt::Display for InventoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InventoryError::Invalid(message) => f.write_str(message),
            InventoryError::Io(error) => f.write_str(&py_os_error(error, None)),
        }
    }
}

impl std::error::Error for InventoryError {}

fn invalid(message: String) -> InventoryError {
    InventoryError::Invalid(message)
}

/// Python's str.strip(): ASCII-and-Unicode whitespace, including U+001C to U+001F.
fn py_strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

fn is_ascii_ws(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Python's bytes.strip(): ASCII whitespace only.
fn ascii_trim(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|&b| !is_ascii_ws(b))
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|&b| !is_ascii_ws(b))
        .map_or(start, |i| i + 1);
    &bytes[start..end]
}

/// `^[a-z][a-z0-9-]{0,62}$`, the library's slug.
fn is_slug(text: &str) -> bool {
    let mut chars = text.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase());
    first_ok
        && text.len() <= 63
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The address guard: inside 10.87.0.0/24, and not its network, broadcast, or the AP.
fn on_iot_net(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    octets[0] == 10
        && octets[1] == 87
        && octets[2] == 0
        && address != IOT_BASE
        && address != IOT_BROADCAST
        && address != AP_ADDR
}

fn sorted_extra<'a>(keys: impl Iterator<Item = &'a String>, allowed: &[&str]) -> String {
    let mut extra: Vec<&String> = keys
        .filter(|key| !allowed.contains(&key.as_str()))
        .collect();
    extra.sort();
    extra
        .iter()
        .map(|key| key.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Read a cameras document. `title` may be omitted. Every address must be a node on tallow-iot.
pub fn parse_inventory(
    raw: &Value,
    path: Option<&Path>,
) -> Result<(String, Vec<Cam>), InventoryError> {
    let where_ = path.map_or_else(|| "cameras".to_owned(), |p| p.display().to_string());
    let shape = || invalid(format!("{where_}: expected {{\"cams\": [...]}}"));
    let root = raw.as_object().ok_or_else(shape)?;
    let entries = root
        .get("cams")
        .and_then(Value::as_array)
        .ok_or_else(shape)?;
    let extra = sorted_extra(root.keys(), &["cams", "title"]);
    if !extra.is_empty() {
        return Err(invalid(format!("{where_}: unknown fields: {extra}")));
    }
    let title = match root.get("title") {
        None => DEFAULT_TITLE.to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(_) => String::new(),
    };
    let stripped = py_strip(&title);
    if stripped.is_empty() || stripped.chars().count() > TITLE_MAX {
        return Err(invalid(format!(
            "{where_}: title needs 1 to {TITLE_MAX} characters"
        )));
    }
    let title = stripped.to_owned();

    let mut cams: Vec<Cam> = Vec::new();
    for entry in entries {
        let object = entry
            .as_object()
            .ok_or_else(|| invalid(format!("{where_}: each cam is an object")))?;
        let extra = sorted_extra(object.keys(), &["id", "name", "address"]);
        if !extra.is_empty() {
            return Err(invalid(format!("{where_}: unknown fields: {extra}")));
        }
        let id = match object.get("id") {
            Some(Value::String(text)) => Some(py_strip(text).to_owned()),
            _ => None,
        };
        let id = match id {
            Some(id) if is_slug(&id) => id,
            _ => return Err(invalid(format!("{where_}: id must be a lowercase slug"))),
        };
        if cams.iter().any(|cam| cam.id == id) {
            return Err(invalid(format!("{where_}: {id} is listed twice")));
        }
        let name = match object.get("name") {
            None => id.clone(),
            Some(Value::String(text)) => text.clone(),
            Some(_) => String::new(),
        };
        if py_strip(&name).is_empty() || name.chars().count() > NAME_MAX {
            return Err(invalid(format!(
                "{where_}: {id} needs a name of at most {NAME_MAX} characters"
            )));
        }
        let text = match object.get("address") {
            Some(Value::String(text)) => py_strip(text).to_owned(),
            _ => String::new(),
        };
        let address: IpAddr = text
            .parse()
            .map_err(|_| invalid(format!("{where_}: {id} needs an IPv4 address")))?;
        let must_be_node = || invalid(format!("{where_}: {id} must be a node on 10.87.0.0/24"));
        let address = match address {
            IpAddr::V4(address) if on_iot_net(address) => address,
            _ => return Err(must_be_node()),
        };
        cams.push(Cam {
            id,
            name: py_strip(&name).to_owned(),
            address: address.to_string(),
            port: STREAM_PORT,
            node_port: NODE_PORT,
        });
    }
    Ok((title, cams))
}

/// Python's str(OSError): `[Errno N] strerror: 'path'`, or just the message without an errno.
fn py_os_error(error: &io::Error, path: Option<&Path>) -> String {
    match (error.raw_os_error(), path) {
        (Some(code), Some(path)) => {
            format!("[Errno {code}] {}: '{}'", strerror(error), path.display())
        }
        (Some(code), None) => format!("[Errno {code}] {}", strerror(error)),
        (None, _) => error.to_string(),
    }
}

/// The OS message without Rust's " (os error N)" suffix, which Python does not print.
fn strerror(error: &io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => {
            let text = io::Error::from_raw_os_error(code).to_string();
            text.rsplit_once(" (os error")
                .map_or(text.clone(), |(message, _)| message.to_owned())
        }
        None => "OSError".to_owned(),
    }
}

/// The message json.loads gives for bad UTF-8, in Python's words.
fn utf8_message(bytes: &[u8], error: &std::str::Utf8Error) -> String {
    let position = error.valid_up_to();
    let byte = bytes.get(position).copied().unwrap_or(0);
    let reason = if error.error_len().is_none() {
        "unexpected end of data"
    } else if matches!(byte, 0x80..=0xC1 | 0xF5..=0xFF) {
        "invalid start byte"
    } else {
        "invalid continuation byte"
    };
    format!("'utf-8' codec can't decode byte 0x{byte:02x} in position {position}: {reason}")
}

/// Read the inventory file. A missing file is an empty list named Cameras.
pub fn load_inventory(path: &Path) -> Result<(String, Vec<Cam>), InventoryError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((DEFAULT_TITLE.to_owned(), Vec::new()));
        }
        Err(error) => return Err(InventoryError::Io(error)),
    };
    let text =
        std::str::from_utf8(&bytes).map_err(|error| invalid(utf8_message(&bytes, &error)))?;
    let raw: Value = serde_json::from_str(text).map_err(|error| {
        let message = error.to_string();
        let reason = message.split(" at line").next().unwrap_or("");
        invalid(format!(
            "{}: not JSON ({reason}, line {})",
            path.display(),
            error.line()
        ))
    })?;
    parse_inventory(&raw, Some(path))
}

#[derive(Serialize)]
struct Document<'a> {
    title: &'a str,
    cams: Vec<CamDocument<'a>>,
}

#[derive(Serialize)]
struct CamDocument<'a> {
    id: &'a str,
    name: &'a str,
    address: &'a str,
}

/// json.dumps escapes non-ASCII as \uXXXX. Matching that keeps the file byte-compatible with Python.
fn ascii_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

/// Validate and replace the inventory. The write is a temp file renamed into place.
pub fn save_inventory(path: &Path, raw: &Value) -> Result<(String, Vec<Cam>), InventoryError> {
    // The path stays out of the message: this answers a browser.
    let (title, cams) = parse_inventory(raw, None)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(InventoryError::Io)?;
    }
    let document = Document {
        title: &title,
        cams: cams
            .iter()
            .map(|cam| CamDocument {
                id: &cam.id,
                name: &cam.name,
                address: &cam.address,
            })
            .collect(),
    };
    let text =
        serde_json::to_string_pretty(&document).map_err(|error| invalid(error.to_string()))?;
    let text = ascii_json(&text) + "\n";
    let mut temp_name = path.file_name().unwrap_or_default().to_os_string();
    temp_name.push(".tmp");
    let temp = path.with_file_name(temp_name);
    std::fs::write(&temp, text).map_err(InventoryError::Io)?;
    std::fs::rename(&temp, path).map_err(InventoryError::Io)?;
    Ok((title, cams))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stamp {
    Unread,
    Missing,
    At(SystemTime),
}

/// The cams the panel knows. A file is re-read when it changes, so a new cam needs no restart.
/// A bad edit keeps the last good list and says what is wrong in `error`.
pub struct Book {
    path: Option<PathBuf>,
    pub error: Option<String>,
    title: String,
    cams: Vec<Cam>,
    stamp: Stamp,
}

impl Book {
    pub fn new(path: Option<PathBuf>, cams: Vec<Cam>) -> Book {
        Book {
            path,
            error: None,
            title: DEFAULT_TITLE.to_owned(),
            cams,
            stamp: Stamp::Unread,
        }
    }

    /// The name on the menu. Reloads the file when it has changed.
    pub fn title(&mut self) -> String {
        self.all();
        self.title.clone()
    }

    /// Read the file again on the next lookup, even if the mtime did not move.
    pub fn forget(&mut self) {
        self.stamp = Stamp::Unread;
    }

    pub fn all(&mut self) -> Vec<Cam> {
        let Some(path) = self.path.clone() else {
            return self.cams.clone();
        };
        let stamp = match std::fs::metadata(&path).and_then(|meta| meta.modified()) {
            Ok(modified) => Stamp::At(modified),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Stamp::Missing,
            Err(error) => {
                self.error = Some(format!("{}: {}", path.display(), strerror(&error)));
                return self.cams.clone();
            }
        };
        if stamp != self.stamp {
            self.stamp = stamp;
            match load_inventory(&path) {
                Ok((title, cams)) => {
                    self.title = title;
                    self.cams = cams;
                    self.error = None;
                }
                Err(InventoryError::Io(error)) => {
                    self.error = Some(py_os_error(&error, Some(&path)))
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        }
        self.cams.clone()
    }

    pub fn get(&mut self, cam_id: &str) -> Option<Cam> {
        self.all().into_iter().find(|cam| cam.id == cam_id)
    }
}

/// A frame of the cam's stream, as the cam framed it (the JPEG only).
pub type Frame = Arc<[u8]>;

/// What `Upstream::next_frame` and `Dial` failures look like in Python's class names.
#[derive(Debug)]
enum Fault {
    Io(io::Error),
    Eof,
    Overrun,
    Timeout,
}

impl Fault {
    fn python_name(&self) -> &'static str {
        match self {
            Fault::Io(error) => match error.kind() {
                io::ErrorKind::NotFound => "FileNotFoundError",
                io::ErrorKind::PermissionDenied => "PermissionError",
                io::ErrorKind::ConnectionRefused => "ConnectionRefusedError",
                io::ErrorKind::ConnectionReset => "ConnectionResetError",
                io::ErrorKind::ConnectionAborted => "ConnectionAbortedError",
                io::ErrorKind::BrokenPipe => "BrokenPipeError",
                io::ErrorKind::TimedOut => "TimeoutError",
                io::ErrorKind::UnexpectedEof => "IncompleteReadError",
                _ => "OSError",
            },
            Fault::Eof => "IncompleteReadError",
            Fault::Overrun => "LimitOverrunError",
            Fault::Timeout => "TimeoutError",
        }
    }
}

fn down(cam: &Cam, fault: &Fault) -> CamDown {
    CamDown(format!("{}: {}", cam.id, fault.python_name()))
}

/// readuntil(seq): bytes up to and including the first `seq`, bounded by `limit`.
async fn read_until_seq<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    seq: &[u8],
    limit: usize,
) -> Result<Vec<u8>, Fault> {
    let mut out = Vec::new();
    loop {
        let byte = {
            let available = reader.fill_buf().await.map_err(Fault::Io)?;
            match available.first() {
                Some(byte) => *byte,
                None => return Err(Fault::Eof),
            }
        };
        reader.consume(1);
        out.push(byte);
        if out.ends_with(seq) {
            return Ok(out);
        }
        if out.len() > limit {
            return Err(Fault::Overrun);
        }
    }
}

/// Python's int() on the bytes of a Content-Length value: optional sign, digits, single underscores between digits.
fn parse_py_int(raw: &[u8]) -> Option<i128> {
    let text = ascii_trim(raw);
    let (negative, digits) = match text.split_first() {
        Some((b'-', rest)) => (true, rest),
        Some((b'+', rest)) => (false, rest),
        _ => (false, text),
    };
    if digits.is_empty() {
        return None;
    }
    let mut value: i128 = 0;
    for (i, &byte) in digits.iter().enumerate() {
        match byte {
            b'0'..=b'9' => {
                value = value
                    .checked_mul(10)?
                    .checked_add(i128::from(byte - b'0'))?
            }
            b'_' => {
                let prev = i.checked_sub(1).and_then(|p| digits.get(p));
                let next = digits.get(i + 1);
                if !(prev.is_some_and(u8::is_ascii_digit) && next.is_some_and(u8::is_ascii_digit)) {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(if negative { -value } else { value })
}

/// One open connection to a cam, past its response headers.
pub struct Upstream {
    reader: BufReader<TcpStream>,
    frame_timeout: Duration,
}

impl Upstream {
    /// The next JPEG. None when the cam goes quiet, hangs up, or sends something that is not a frame.
    pub async fn next_frame(&mut self) -> Option<Frame> {
        let mut length: Option<i128> = None;
        loop {
            let line = timeout(
                self.frame_timeout,
                read_until_seq(&mut self.reader, b"\r\n", LINE_LIMIT),
            )
            .await
            .ok()?
            .ok()?;
            let line = ascii_trim(&line);
            if line.is_empty() {
                if length.is_some() {
                    break;
                }
                continue;
            }
            if let Some(colon) = line.iter().position(|&b| b == b':') {
                let (name, value) = (&line[..colon], &line[colon + 1..]);
                if ascii_trim(name).eq_ignore_ascii_case(b"content-length") {
                    length = Some(parse_py_int(value)?);
                }
            }
        }
        let length = length?;
        if !(1..=FRAME_MAX as i128).contains(&length) {
            return None;
        }
        let mut body = vec![0u8; usize::try_from(length).ok()?];
        timeout(self.frame_timeout, self.reader.read_exact(&mut body))
            .await
            .ok()?
            .ok()?;
        Some(Arc::from(body))
    }
}

/// Ask the cam for its stream. Raise CamDown unless it answers with MJPEG.
pub async fn dial(cam: &Cam) -> Result<Upstream, CamDown> {
    dial_with(cam, &Tuning::default()).await
}

pub async fn dial_with(cam: &Cam, tuning: &Tuning) -> Result<Upstream, CamDown> {
    let connect = TcpStream::connect((cam.address.as_str(), cam.port));
    let stream = match timeout(tuning.connect_timeout, connect).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => return Err(down(cam, &Fault::Io(error))),
        Err(_) => return Err(down(cam, &Fault::Timeout)),
    };
    let mut reader = BufReader::new(stream);
    // Dropping `reader` on any return below closes the socket, so a cancelled dial leaves no request queued.
    let request = format!(
        "GET / HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        cam.address
    );
    if let Err(error) = reader.get_mut().write_all(request.as_bytes()).await {
        return Err(down(cam, &Fault::Io(error)));
    }
    let head = match timeout(
        tuning.header_timeout,
        read_until_seq(&mut reader, b"\r\n\r\n", LINE_LIMIT),
    )
    .await
    {
        Ok(Ok(head)) => head,
        Ok(Err(fault)) => return Err(down(cam, &fault)),
        Err(_) => return Err(down(cam, &Fault::Timeout)),
    };
    if head.len() > HEAD_MAX {
        return Err(CamDown(format!("{}: headers too long", cam.id)));
    }
    let text: String = head.iter().map(|&b| char::from(b)).collect();
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status: Vec<&str> = status_line.splitn(3, ' ').collect();
    if status.len() < 2 || status.get(1) != Some(&"200") {
        return Err(CamDown(format!("{}: {status_line}", cam.id)));
    }
    let mut headers: HashMap<String, String> = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(py_strip(name).to_lowercase(), py_strip(value).to_owned());
        }
    }
    let content_type = headers.get("content-type").map_or("", String::as_str);
    if !content_type
        .to_lowercase()
        .starts_with("multipart/x-mixed-replace")
    {
        let shown = if content_type.is_empty() {
            "no content type"
        } else {
            content_type
        };
        return Err(CamDown(format!("{}: not a stream ({shown})", cam.id)));
    }
    Ok(Upstream {
        reader,
        frame_timeout: tuning.frame_timeout,
    })
}

/// Does the node pick up? Asks its API port, never the one-socket camera server.
pub async fn reachable(cam: &Cam) -> bool {
    let connect = TcpStream::connect((cam.address.as_str(), cam.node_port));
    matches!(timeout(STATUS_TIMEOUT, connect).await, Ok(Ok(_)))
}

/// A viewer's queue. A slow viewer skips frames instead of holding the rest back.
/// `None` is the end of the stream.
struct Queue {
    items: Mutex<VecDeque<Option<Frame>>>,
    ready: Notify,
}

impl Queue {
    fn new() -> Queue {
        Queue {
            items: Mutex::new(VecDeque::new()),
            ready: Notify::new(),
        }
    }

    fn offer(&self, item: Option<Frame>) {
        let mut items = lock(&self.items);
        if items.len() >= QUEUE_DEPTH {
            items.pop_front();
        }
        items.push_back(item);
        drop(items);
        self.ready.notify_one();
    }

    async fn take(&self) -> Option<Frame> {
        loop {
            if let Some(item) = lock(&self.items).pop_front() {
                return item;
            }
            self.ready.notified().await;
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub type DialFuture = Pin<Box<dyn Future<Output = Result<Upstream, CamDown>> + Send>>;

/// How a hub dials a cam. Production uses `dial_with`; tests substitute their own.
pub type Connect = Arc<dyn Fn(Cam) -> DialFuture + Send + Sync>;

/// The production connector, with the given timeouts.
pub fn dialer(tuning: Tuning) -> Connect {
    Arc::new(move |cam: Cam| -> DialFuture {
        Box::pin(async move { dial_with(&cam, &tuning).await })
    })
}

#[derive(Default)]
struct State {
    viewers: Vec<(u64, Arc<Queue>)>,
    next_id: u64,
    pump: Option<JoinHandle<()>>,
    stopping: bool,
    timer: Option<(u64, AbortHandle)>,
    timer_seq: u64,
    failures: u32,
    retry_at: Option<Instant>,
}

fn pump_idle(state: &State) -> bool {
    match &state.pump {
        None => true,
        Some(pump) => pump.is_finished(),
    }
}

struct Inner {
    cam: Cam,
    tuning: Tuning,
    connect: Connect,
    /// Serialises `join`, as the Python `_lock` does.
    turn: AsyncMutex<()>,
    state: Mutex<State>,
}

impl Inner {
    fn offer_all(&self, item: Option<Frame>) {
        let state = lock(&self.state);
        for (_, queue) in &state.viewers {
            queue.offer(item.clone());
        }
    }

    /// Hang up if this timer is still the armed one and nobody came back.
    fn hang_up(&self, token: u64) {
        let abort = {
            let mut state = lock(&self.state);
            match &state.timer {
                Some((armed, _)) if *armed == token => {}
                _ => return,
            }
            state.timer = None;
            if !state.viewers.is_empty() {
                return;
            }
            let handle = state.pump.as_ref().map(JoinHandle::abort_handle);
            if handle.is_some() {
                state.stopping = true;
            }
            handle
        };
        // Abort outside the lock: the pump's drop takes the lock to tell its viewers the stream ended.
        if let Some(handle) = abort {
            handle.abort();
        }
    }
}

/// Tells every viewer the stream has ended, however the pump ends.
struct Ending(Arc<Inner>);

impl Drop for Ending {
    fn drop(&mut self) {
        self.0.offer_all(None);
    }
}

async fn run_pump(inner: Arc<Inner>, mut upstream: Upstream) {
    let _ending = Ending(Arc::clone(&inner));
    while let Some(frame) = upstream.next_frame().await {
        inner.offer_all(Some(frame));
    }
    // `upstream` drops first (closing the socket), then `_ending` tells the viewers.
}

/// One connection to one cam, shared by everyone watching it.
#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

impl Hub {
    pub fn new(cam: Cam, connect: Connect, tuning: Tuning) -> Hub {
        Hub {
            inner: Arc::new(Inner {
                cam,
                tuning,
                connect,
                turn: AsyncMutex::new(()),
                state: Mutex::new(State::default()),
            }),
        }
    }

    pub fn watching(&self) -> usize {
        lock(&self.inner.state).viewers.len()
    }

    /// The panel's cap: a viewer past `viewers_max` is refused with 429.
    pub fn viewers_full(&self) -> bool {
        self.watching() >= self.inner.tuning.viewers_max
    }

    /// No connection to the cam, open or on its way out.
    pub fn idle(&self) -> bool {
        pump_idle(&lock(&self.inner.state))
    }

    /// Start watching. The first viewer wakes the cam. Raises CamDown if it will not answer.
    pub async fn join(&self) -> Result<Feed, CamDown> {
        let _turn = self.inner.turn.lock().await;
        let ended = {
            let mut state = lock(&self.inner.state);
            if let Some((_, timer)) = state.timer.take() {
                timer.abort();
            }
            let ended = state.pump.as_ref().is_some_and(JoinHandle::is_finished)
                || (state.pump.is_some() && state.stopping);
            if ended {
                state.pump.take()
            } else {
                None
            }
        };
        if let Some(pump) = ended {
            // It ended, or is being hung up on. Let its socket close before asking the cam again.
            let _ = pump.await;
            lock(&self.inner.state).stopping = false;
        }
        let needs_dial = lock(&self.inner.state).pump.is_none();
        if needs_dial {
            let upstream = self.dial_now().await?;
            let task = tokio::spawn(run_pump(Arc::clone(&self.inner), upstream));
            lock(&self.inner.state).pump = Some(task);
        }
        let queue = Arc::new(Queue::new());
        let id = {
            let mut state = lock(&self.inner.state);
            state.next_id += 1;
            let id = state.next_id;
            state.viewers.push((id, Arc::clone(&queue)));
            id
        };
        Ok(Feed {
            hub: self.clone(),
            id,
            queue,
        })
    }

    /// Dial the cam, but not while it is still catching its breath from the last failure.
    async fn dial_now(&self) -> Result<Upstream, CamDown> {
        let retry_at = lock(&self.inner.state).retry_at;
        if let Some(at) = retry_at {
            let now = Instant::now();
            if at > now {
                let wait = (at - now).as_secs_f64();
                return Err(CamDown(format!(
                    "{}: cooling off for {wait:.0} s",
                    self.inner.cam.id
                )));
            }
        }
        match (self.inner.connect)(self.inner.cam.clone()).await {
            Ok(upstream) => {
                lock(&self.inner.state).failures = 0;
                Ok(upstream)
            }
            Err(error) => {
                let mut state = lock(&self.inner.state);
                state.failures = state.failures.saturating_add(1);
                let cooldown = self.inner.tuning.cooldown;
                let index = (state.failures as usize)
                    .min(cooldown.len())
                    .saturating_sub(1);
                if let Some(pause) = cooldown.get(index) {
                    state.retry_at = Some(Instant::now() + *pause);
                }
                Err(error)
            }
        }
    }

    /// Stop watching. After a short linger the last one out hangs up, and the cam stops sending.
    fn leave(&self, id: u64) {
        let mut state = lock(&self.inner.state);
        state.viewers.retain(|(viewer, _)| *viewer != id);
        if !state.viewers.is_empty() || pump_idle(&state) || state.timer.is_some() {
            return;
        }
        state.timer_seq += 1;
        let token = state.timer_seq;
        let linger = self.inner.tuning.linger;
        let inner = Arc::clone(&self.inner);
        let task = tokio::spawn(async move {
            sleep(linger).await;
            inner.hang_up(token);
        });
        state.timer = Some((token, task.abort_handle()));
    }
}

/// One viewer's copy of a cam. Dropping it leaves the hub.
pub struct Feed {
    hub: Hub,
    id: u64,
    queue: Arc<Queue>,
}

impl Feed {
    /// The next frame, or None once the cam's stream has ended.
    pub async fn next_frame(&mut self) -> Option<Frame> {
        self.queue.take().await
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.hub.leave(self.id);
    }
}

/// A hub for each cam that has had a viewer.
pub struct Hubs {
    connect: Connect,
    tuning: Tuning,
    hubs: Mutex<HashMap<Cam, Hub>>,
}

impl Hubs {
    pub fn new(tuning: Tuning) -> Hubs {
        Hubs::with_connect(dialer(tuning), tuning)
    }

    pub fn with_connect(connect: Connect, tuning: Tuning) -> Hubs {
        Hubs {
            connect,
            tuning,
            hubs: Mutex::new(HashMap::new()),
        }
    }

    pub fn hub(&self, cam: &Cam) -> Hub {
        let mut hubs = lock(&self.hubs);
        hubs.entry(cam.clone())
            .or_insert_with(|| Hub::new(cam.clone(), Arc::clone(&self.connect), self.tuning))
            .clone()
    }

    pub fn watching(&self, cam: &Cam) -> usize {
        lock(&self.hubs).get(cam).map_or(0, Hub::watching)
    }

    /// Drop hubs for cams that left the inventory, once nobody watches them and no connection is open.
    pub fn prune(&self, keep: &[Cam]) {
        lock(&self.hubs).retain(|cam, hub| keep.contains(cam) || hub.watching() > 0 || !hub.idle());
    }
}

/// One frame re-emitted as a multipart part, as the panel sends it to a browser.
pub fn part(frame: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "--{BOUNDARY}\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
        frame.len()
    )
    .into_bytes();
    out.extend_from_slice(frame);
    out.extend_from_slice(b"\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::net::TcpListener;

    const STREAM: &str = "multipart/x-mixed-replace;boundary=123456789";

    /// A temporary directory that is removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir =
                std::env::temp_dir().join(format!("tallow-cams-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Scratch(dir)
        }

        fn path(&self, file: &str) -> PathBuf {
            self.0.join(file)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Write a file and give it a set mtime, so the reload check sees a change without sleeping.
    fn write_at(path: &Path, text: &str, secs: u64) {
        std::fs::write(path, text).expect("write inventory");
        let when = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .and_then(|file| file.set_modified(when))
            .expect("set mtime");
    }

    fn ids(cams: &[Cam]) -> Vec<&str> {
        cams.iter().map(|cam| cam.id.as_str()).collect()
    }

    fn invalid_message(raw: &Value) -> String {
        match parse_inventory(raw, None) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("expected an error for {raw}"),
        }
    }

    /// A fake cam on 127.0.0.1. It answers the way the ESPHome camera server does.
    struct FakeState {
        content_type: &'static str,
        silent: bool,
        mute: bool,
        cut_after: Mutex<Option<u64>>,
        connections: AtomicUsize,
        open: AtomicUsize,
        hung_up: Notify,
        requests: Mutex<Vec<Vec<u8>>>,
    }

    struct Fake {
        port: u16,
        state: Arc<FakeState>,
        task: JoinHandle<()>,
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl Fake {
        async fn spawn(
            content_type: &'static str,
            silent: bool,
            mute: bool,
            cut_after: Option<u64>,
        ) -> Fake {
            let listener = TcpListener::bind(("127.0.0.1", 0))
                .await
                .expect("bind fake cam");
            let port = listener.local_addr().expect("fake cam address").port();
            let state = Arc::new(FakeState {
                content_type,
                silent,
                mute,
                cut_after: Mutex::new(cut_after),
                connections: AtomicUsize::new(0),
                open: AtomicUsize::new(0),
                hung_up: Notify::new(),
                requests: Mutex::new(Vec::new()),
            });
            let accept_state = Arc::clone(&state);
            let task = tokio::spawn(async move {
                while let Ok((socket, _)) = listener.accept().await {
                    tokio::spawn(serve(socket, Arc::clone(&accept_state)));
                }
            });
            Fake { port, state, task }
        }

        fn cam(&self) -> Cam {
            cam_on(self.port)
        }

        fn connections(&self) -> usize {
            self.state.connections.load(Ordering::SeqCst)
        }

        fn open(&self) -> usize {
            self.state.open.load(Ordering::SeqCst)
        }

        fn set_cut_after(&self, value: Option<u64>) {
            *lock(&self.state.cut_after) = value;
        }
    }

    async fn serve(mut socket: TcpStream, state: Arc<FakeState>) {
        state.connections.fetch_add(1, Ordering::SeqCst);
        state.open.fetch_add(1, Ordering::SeqCst);
        let _ = serve_inner(&mut socket, &state).await;
        state.open.fetch_sub(1, Ordering::SeqCst);
        state.hung_up.notify_one();
    }

    async fn serve_inner(socket: &mut TcpStream, state: &FakeState) -> io::Result<()> {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if socket.read(&mut byte).await? == 0 {
                return Ok(());
            }
            head.push(byte[0]);
        }
        lock(&state.requests).push(head);
        if state.mute {
            let mut sink = Vec::new();
            let _ = socket.read_to_end(&mut sink).await;
            return Ok(());
        }
        let answer = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {}\r\n\r\n--123456789\r\n",
            state.content_type
        );
        socket.write_all(answer.as_bytes()).await?;
        let mut number: u64 = 0;
        loop {
            number += 1;
            let cut = *lock(&state.cut_after);
            if let Some(limit) = cut {
                if number > limit {
                    if state.silent {
                        let mut sink = Vec::new();
                        let _ = socket.read_to_end(&mut sink).await;
                    }
                    return Ok(());
                }
            }
            socket.write_all(&fake_part(number)).await?;
            sleep(Duration::from_millis(10)).await;
        }
    }

    fn fake_part(number: u64) -> Vec<u8> {
        let mut jpeg = vec![0xff, 0xd8];
        jpeg.extend(format!("{number:04}").bytes());
        jpeg.extend([0xff, 0xd9]);
        let mut out = format!(
            "Content-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
            jpeg.len()
        )
        .into_bytes();
        out.extend(jpeg);
        out.extend(b"\r\n--123456789\r\n");
        out
    }

    fn cam_on(port: u16) -> Cam {
        Cam::new("den", "Den", "127.0.0.1").with_ports(port, port)
    }

    /// A loopback port with nothing listening on it.
    fn free_port() -> u16 {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("probe port");
        listener.local_addr().expect("probe address").port()
    }

    fn tuning_with_linger(linger: Duration) -> Tuning {
        Tuning {
            linger,
            ..Tuning::default()
        }
    }

    async fn frame(feed: &mut Feed) -> Frame {
        timeout(Duration::from_secs(10), feed.next_frame())
            .await
            .expect("a frame came")
            .expect("the stream is open")
    }

    async fn drain(feed: &mut Feed) {
        timeout(Duration::from_secs(10), async {
            while feed.next_frame().await.is_some() {}
        })
        .await
        .expect("the stream ended");
    }

    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    // BookTest

    #[test]
    fn a_bad_edit_keeps_the_last_good_list_and_says_why() {
        let scratch = Scratch::new("bad-edit");
        let path = scratch.path("cams.json");
        write_at(
            &path,
            r#"{"cams": [{"id": "den", "address": "10.87.0.21"}]}"#,
            1_000,
        );
        let mut book = Book::new(Some(path.clone()), Vec::new());
        assert_eq!(ids(&book.all()), vec!["den"]);
        assert!(book.error.is_none());

        write_at(
            &path,
            r#"{"cams": [{"id": "den", "address": "192.168.1.9"}]}"#,
            1_001,
        );
        assert_eq!(ids(&book.all()), vec!["den"]);
        assert!(book
            .error
            .as_deref()
            .unwrap_or("")
            .contains("must be a node on"));

        write_at(&path, "{", 1_002);
        assert_eq!(ids(&book.all()), vec!["den"]);
        assert!(book.error.as_deref().unwrap_or("").contains("not JSON"));

        write_at(&path, r#"{"cams": []}"#, 1_003);
        assert!(book.all().is_empty());
        assert_eq!(book.title(), DEFAULT_TITLE);
        assert!(book.error.is_none());

        // A good file read again on request, even with the same mtime, and the lookup by id.
        write_at(
            &path,
            r#"{"cams": [{"id": "den", "address": "10.87.0.21"}]}"#,
            1_003,
        );
        book.forget();
        assert_eq!(
            book.get("den").map(|cam| cam.address),
            Some("10.87.0.21".to_owned())
        );
        assert!(book.get("nope").is_none());
    }

    #[test]
    fn a_title_is_optional_and_a_bad_edit_keeps_the_last_good_one() {
        let scratch = Scratch::new("title");
        let path = scratch.path("cams.json");
        write_at(
            &path,
            r#"{"cams": [{"id": "den", "address": "10.87.0.21"}]}"#,
            2_000,
        );
        let mut book = Book::new(Some(path.clone()), Vec::new());
        assert_eq!(book.title(), "Cameras");

        write_at(
            &path,
            r#"{"title": "Yard", "cams": [{"id": "den", "address": "10.87.0.21"}]}"#,
            2_001,
        );
        assert_eq!(book.title(), "Yard");

        write_at(
            &path,
            r#"{"title": "Yard", "cams": [], "note": "no"}"#,
            2_002,
        );
        assert_eq!(book.title(), "Yard");
        assert!(book
            .error
            .as_deref()
            .unwrap_or("")
            .contains("unknown fields"));

        assert!(parse_inventory(&json!({"title": "", "cams": []}), None).is_err());
        assert!(parse_inventory(&json!({"title": "x".repeat(41), "cams": []}), None).is_err());
    }

    #[test]
    fn save_then_load_round_trips_the_inventory() {
        let scratch = Scratch::new("round-trip");
        let path = scratch.path("cams.json");
        let raw = json!({"title": " Toby ", "cams": [{"id": "den", "name": "Den", "address": "10.87.0.21"}]});
        let (title, cams) = save_inventory(&path, &raw).expect("save");
        assert_eq!(title, "Toby");
        assert_eq!(
            cams.iter()
                .map(|cam| (cam.id.as_str(), cam.name.as_str(), cam.address.as_str()))
                .collect::<Vec<_>>(),
            vec![("den", "Den", "10.87.0.21")]
        );
        assert_eq!(load_inventory(&path).expect("load"), (title, cams));
        let text = std::fs::read_to_string(&path).expect("read back");
        let title_at = text.find("\"title\"").expect("title key");
        let cams_at = text.find("\"cams\"").expect("cams key");
        assert!(title_at < cams_at, "title is written before cams");
        assert!(!text.contains("\"port\""));
        assert!(
            !scratch.path("cams.json.tmp").exists(),
            "the temp file is renamed away"
        );
    }

    #[test]
    fn a_slow_viewer_skips_frames_instead_of_holding_the_rest_back() {
        let queue = Queue::new();
        for byte in [b"1", b"2", b"3", b"4"] {
            queue.offer(Some(Arc::from(&byte[..])));
        }
        let left: Vec<Option<Frame>> = lock(&queue.items).drain(..).collect();
        let bytes: Vec<&[u8]> = left
            .iter()
            .map(|item| item.as_deref().unwrap_or(&[]))
            .collect();
        assert_eq!(bytes, vec![&b"3"[..], &b"4"[..]]);
    }

    // The inventory's rules, which the Python tests reach through the routes.

    #[test]
    fn the_inventory_rejects_each_bad_entry_with_its_message() {
        let cases = [
            (json!([]), "cameras: expected {\"cams\": [...]}"),
            (json!({"cams": [], "x": 1}), "cameras: unknown fields: x"),
            (
                json!({"cams": [], "title": ""}),
                "cameras: title needs 1 to 40 characters",
            ),
            (json!({"cams": ["x"]}), "cameras: each cam is an object"),
            (
                json!({"cams": [{"id": "Den", "address": "10.87.0.21"}]}),
                "cameras: id must be a lowercase slug",
            ),
            (
                json!({"cams": [{"id": "den", "address": "10.87.0.21"}, {"id": "den", "address": "10.87.0.22"}]}),
                "cameras: den is listed twice",
            ),
            (
                json!({"cams": [{"id": "den", "name": "x".repeat(41), "address": "10.87.0.21"}]}),
                "cameras: den needs a name of at most 40 characters",
            ),
            (
                json!({"cams": [{"id": "den", "address": "nope"}]}),
                "cameras: den needs an IPv4 address",
            ),
            (
                json!({"cams": [{"id": "den", "address": "10.87.0.1"}]}),
                "cameras: den must be a node on 10.87.0.0/24",
            ),
            (
                json!({"cams": [{"id": "den", "address": "10.87.0.0"}]}),
                "cameras: den must be a node on 10.87.0.0/24",
            ),
            (
                json!({"cams": [{"id": "den", "address": "10.87.0.255"}]}),
                "cameras: den must be a node on 10.87.0.0/24",
            ),
            (
                json!({"cams": [{"id": "den", "address": "10.87.1.2"}]}),
                "cameras: den must be a node on 10.87.0.0/24",
            ),
            (
                json!({"cams": [{"id": "den", "address": "::1"}]}),
                "cameras: den must be a node on 10.87.0.0/24",
            ),
        ];
        for (raw, expected) in cases {
            assert_eq!(invalid_message(&raw), expected);
        }
        let (title, cams) = parse_inventory(
            &json!({"cams": [{"id": "den", "address": " 10.87.0.2 "}]}),
            None,
        )
        .expect("a node on the net is accepted");
        assert_eq!(title, DEFAULT_TITLE);
        assert_eq!(cams[0].address, "10.87.0.2");
        assert_eq!(cams[0].name, "den", "the name defaults to the id");
    }

    #[test]
    fn a_frame_is_re_emitted_with_its_length() {
        assert_eq!(
            part(b"abc"),
            b"--tallow\r\nContent-Type: image/jpeg\r\nContent-Length: 3\r\n\r\nabc\r\n".to_vec()
        );
    }

    // StreamTest

    async fn screens_share_one_cam_connection() {
        let fake = Fake::spawn(STREAM, false, false, None).await;
        let cam = fake.cam();
        let hubs = Hubs::new(tuning_with_linger(Duration::ZERO));
        let hub = hubs.hub(&cam);
        let mut first = hub.join().await.expect("first viewer");
        let mut second = hub.join().await.expect("second viewer");
        frame(&mut first).await;
        frame(&mut second).await;
        // The cam holds one connection at a time. Two screens share it.
        assert_eq!(fake.connections(), 1);

        drop(first);
        assert_eq!(fake.open(), 1);
        frame(&mut second).await;
        frame(&mut second).await;

        drop(second);
        timeout(Duration::from_secs(10), fake.state.hung_up.notified())
            .await
            .expect("cam hung up");
        assert_eq!(hubs.watching(&cam), 0);

        // Watching again wakes the cam again, after the old connection is gone.
        let mut again = hub.join().await.expect("watch again");
        frame(&mut again).await;
        assert_eq!(fake.connections(), 2);
        assert_eq!(fake.open(), 1);
        drop(again);
    }

    #[tokio::test]
    async fn screens_share_one_cam_connection_when_the_server_reports_disconnect() {
        screens_share_one_cam_connection().await;
    }

    #[tokio::test]
    async fn screens_share_one_cam_connection_when_the_send_fails() {
        screens_share_one_cam_connection().await;
    }

    #[tokio::test]
    async fn a_cam_serves_only_so_many_screens() {
        let fake = Fake::spawn(STREAM, false, false, None).await;
        let cam = fake.cam();
        let tuning = Tuning {
            viewers_max: 2,
            linger: Duration::ZERO,
            ..Tuning::default()
        };
        let hub = Hub::new(cam.clone(), dialer(tuning), tuning);
        let mut held = Vec::new();
        for _ in 0..2 {
            let mut feed = hub.join().await.expect("a screen");
            frame(&mut feed).await;
            held.push(feed);
        }
        assert!(
            hub.viewers_full(),
            "the ninth screen would get 429 cam busy"
        );
        assert_eq!(hub.watching(), 2);
        drop(held);
    }

    #[tokio::test]
    async fn a_dead_cam_is_a_502_and_leaves_nobody_counted() {
        let cam = cam_on(free_port());
        let hubs = Hubs::new(Tuning::default());
        let error = hubs
            .hub(&cam)
            .join()
            .await
            .err()
            .expect("the dead cam refuses");
        assert_eq!(error.0, "den: ConnectionRefusedError");
        assert_eq!(hubs.watching(&cam), 0);
    }

    #[tokio::test]
    async fn a_cam_that_is_not_a_stream_is_a_502() {
        let fake = Fake::spawn("text/html", false, false, None).await;
        let cam = fake.cam();
        let hubs = Hubs::new(Tuning::default());
        let error = hubs.hub(&cam).join().await.err().expect("not a stream");
        assert_eq!(error.0, "den: not a stream (text/html)");
        assert_eq!(hubs.watching(&cam), 0);
    }

    // RecoveryTest. Paused time, so the frame timeout runs without real waiting.

    async fn run_drop(silent: bool) {
        let fake = Fake::spawn(STREAM, silent, false, Some(5)).await;
        let cam = fake.cam();
        let tuning = Tuning {
            linger: Duration::ZERO,
            cooldown: &[Duration::ZERO],
            frame_timeout: Duration::from_millis(300),
            ..Tuning::default()
        };
        let hub = Hub::new(cam.clone(), dialer(tuning), tuning);
        let mut first = hub.join().await.expect("first viewer");
        frame(&mut first).await;
        // The cam goes. The viewer's stream ends instead of hanging on a dead feed.
        drain(&mut first).await;
        drop(first);
        assert_eq!(hub.watching(), 0);
        // The cam comes back, and a new viewer gets it with no restart.
        fake.set_cut_after(None);
        let mut again = hub.join().await.expect("the cam is back");
        frame(&mut again).await;
        drop(again);
    }

    #[tokio::test(start_paused = true)]
    async fn a_cam_that_hangs_up_mid_stream_comes_back_without_a_restart() {
        run_drop(false).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_cam_that_goes_quiet_comes_back_without_a_restart() {
        run_drop(true).await;
    }

    // GentleTest

    #[tokio::test(start_paused = true)]
    async fn a_quick_stop_and_watch_keeps_the_same_connection() {
        let fake = Fake::spawn(STREAM, false, false, None).await;
        let cam = fake.cam();
        let hub = Hub::new(cam.clone(), dialer(Tuning::default()), Tuning::default());
        let mut first = hub.join().await.expect("first viewer");
        frame(&mut first).await;
        drop(first);
        // A tab switch, a reload, Stop then Watch: back inside the linger.
        let mut again = hub.join().await.expect("watch again");
        frame(&mut again).await;
        assert_eq!(fake.connections(), 1);
        drop(again);
        // Nobody comes back, so the panel hangs up and the cam stops sending.
        timeout(Duration::from_secs(60), fake.state.hung_up.notified())
            .await
            .expect("cam hung up");
        settle().await;
        assert!(hub.idle());
    }

    #[tokio::test(start_paused = true)]
    async fn a_hub_that_is_still_lingering_is_not_pruned() {
        let fake = Fake::spawn(STREAM, false, false, None).await;
        let cam = fake.cam();
        let tuning = tuning_with_linger(Duration::from_secs(5));
        let hubs = Hubs::new(tuning);
        let hub = hubs.hub(&cam);
        let feed = hub.join().await.expect("a viewer");
        drop(feed);
        hubs.prune(&[]);
        assert!(
            Arc::ptr_eq(&hubs.hub(&cam).inner, &hub.inner),
            "a lingering hub stays"
        );

        // Hang up now, as the linger timer would.
        let token = lock(&hub.inner.state)
            .timer
            .as_ref()
            .map(|(token, _)| *token)
            .expect("a timer is armed");
        hub.inner.hang_up(token);
        settle().await;
        hubs.prune(&[]);
        assert!(
            !Arc::ptr_eq(&hubs.hub(&cam).inner, &hub.inner),
            "a hub that hung up is pruned"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn after_a_failed_dial_the_cam_is_left_alone_for_a_while() {
        let cam = cam_on(free_port());
        let tries = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&tries);
        let tuning = Tuning::default();
        let connect: Connect = Arc::new(move |cam: Cam| -> DialFuture {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { dial_with(&cam, &tuning).await })
        });
        let hub = Hub::new(cam, connect, tuning);
        for expected_wait in [3.0, 6.0, 12.0, 24.0, 30.0, 30.0] {
            assert!(hub.join().await.is_err());
            let made = tries.load(Ordering::SeqCst);
            let cooling = hub.join().await.err().expect("cooling off");
            assert!(cooling.0.contains("cooling off"), "{cooling}");
            assert_eq!(
                tries.load(Ordering::SeqCst),
                made,
                "the cam was dialed during the cooling-off"
            );
            tokio::time::advance(Duration::from_secs_f64(expected_wait + 0.1)).await;
        }
        assert_eq!(tries.load(Ordering::SeqCst), 6);
    }

    #[tokio::test(start_paused = true)]
    async fn a_successful_dial_resets_the_cooldown() {
        let fake = Fake::spawn(STREAM, false, false, None).await;
        let cam = fake.cam();
        let failing = Arc::new(AtomicBool::new(true));
        let tries = Arc::new(AtomicUsize::new(0));
        let (switch, counter) = (Arc::clone(&failing), Arc::clone(&tries));
        let tuning = Tuning {
            linger: Duration::ZERO,
            ..Tuning::default()
        };
        let connect: Connect = Arc::new(move |cam: Cam| -> DialFuture {
            counter.fetch_add(1, Ordering::SeqCst);
            if switch.load(Ordering::SeqCst) {
                Box::pin(async {
                    Err::<Upstream, CamDown>(CamDown("den: refused on purpose".to_owned()))
                })
            } else {
                Box::pin(async move { dial_with(&cam, &tuning).await })
            }
        });
        let hub = Hub::new(cam, connect, tuning);
        // One failure: 3 s of cooling off.
        assert!(hub.join().await.is_err());
        tokio::time::advance(Duration::from_secs_f64(3.1)).await;
        // A success resets the count.
        failing.store(false, Ordering::SeqCst);
        let mut feed = hub.join().await.expect("the cam answers");
        frame(&mut feed).await;
        drop(feed);
        // The linger timer fires and the pump hangs up. Then the next failure is again a first failure.
        settle().await;
        tokio::time::advance(Duration::from_millis(1)).await;
        settle().await;
        failing.store(true, Ordering::SeqCst);
        assert!(hub.join().await.is_err());
        let made = tries.load(Ordering::SeqCst);
        tokio::time::advance(Duration::from_secs_f64(3.1)).await;
        assert!(hub.join().await.is_err());
        assert_eq!(
            tries.load(Ordering::SeqCst),
            made + 1,
            "the cam is dialed again after 3 s, not 6 s"
        );
    }

    #[tokio::test]
    async fn a_dial_that_is_cancelled_does_not_leave_a_request_in_the_cams_queue() {
        let fake = Fake::spawn(STREAM, false, true, None).await;
        let cam = fake.cam();
        let task = tokio::spawn(async move { dial(&cam).await.map(|_| ()) });
        timeout(Duration::from_secs(5), async {
            while lock(&fake.state.requests).is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the request arrived");
        task.abort();
        timeout(Duration::from_secs(5), fake.state.hung_up.notified())
            .await
            .expect("the socket closed");
        assert_eq!(fake.open(), 0);
    }

    // Beyond test_cams.py: the wire bytes, the probe, and the guard's edges.

    #[tokio::test]
    async fn dial_asks_for_the_stream_with_connection_close() {
        let fake = Fake::spawn(STREAM, false, false, None).await;
        let _upstream = dial(&fake.cam()).await.expect("dial");
        let requests = lock(&fake.state.requests).clone();
        assert_eq!(
            requests,
            vec![b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n".to_vec()]
        );
    }

    #[tokio::test]
    async fn reachable_asks_the_node_port_and_never_the_stream_port() {
        let fake = Fake::spawn(STREAM, false, false, None).await;
        let node = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("node port");
        let node_port = node.local_addr().expect("node address").port();
        let up = fake.cam().with_ports(fake.port, node_port);
        assert!(reachable(&up).await);
        assert_eq!(fake.connections(), 0, "the stream port is never touched");
        let down_cam = fake.cam().with_ports(fake.port, free_port());
        assert!(!reachable(&down_cam).await);
        assert_eq!(fake.connections(), 0);
    }

    #[test]
    fn a_frame_length_must_be_a_python_int_in_range() {
        assert_eq!(parse_py_int(b" 1_024 "), Some(1024));
        assert_eq!(parse_py_int(b"+7"), Some(7));
        assert_eq!(parse_py_int(b"-3"), Some(-3));
        assert_eq!(parse_py_int(b"1__0"), None);
        assert_eq!(parse_py_int(b"_1"), None);
        assert_eq!(parse_py_int(b"12a"), None);
        assert_eq!(parse_py_int(b""), None);
    }
}
