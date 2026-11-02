//! One MJPEG client on port 8080, and a TCP listener on 6053.
//!
//! The panel's status check opens 6053 and closes it. It never speaks the
//! ESPHome API, so accepting the connection is enough. The stream serves one
//! client at a time: a second connection waits until the first one drops.
//!
//! Polled from one task at a time: the accept task while the sensor starts,
//! then the main task, with capture, OTA, and Wi-Fi. Socket timeouts restart
//! on any progress, so a client that drips bytes would hold that task. The
//! request and each frame get one whole deadline instead.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use crate::stream;

const HEAD_MAX: usize = 8 * 1024;
const STREAM_PORT: u16 = 8080;
const STATUS_PORT: u16 = 6053;
/// The whole request, and the response head, from accept.
const REQUEST_BUDGET: Duration = Duration::from_secs(3);
/// The whole of one frame: headers, JPEG, and line break.
const FRAME_BUDGET: Duration = Duration::from_secs(5);
/// How long one read waits before the deadline is checked again.
pub const POLL: Duration = Duration::from_millis(250);
/// lwIP treats a timeout under 1 ms as no timeout at all.
const SHORTEST: Duration = Duration::from_millis(1);

pub struct Listeners {
    stream: TcpListener,
    status: TcpListener,
    client: Option<TcpStream>,
}

impl Listeners {
    /// Bind both ports. The caller retries when the stack is not up yet.
    pub fn bind() -> std::io::Result<Self> {
        let stream = TcpListener::bind(("0.0.0.0", STREAM_PORT))?;
        stream.set_nonblocking(true)?;
        let status = TcpListener::bind(("0.0.0.0", STATUS_PORT))?;
        status.set_nonblocking(true)?;
        println!("listen {STREAM_PORT} {STATUS_PORT}");
        Ok(Self {
            stream,
            status,
            client: None,
        })
    }

    pub fn streaming(&self) -> bool {
        self.client.is_some()
    }

    /// Accept a status connection and drop it. If no stream client is held,
    /// accept one, read its request, and answer with the multipart head.
    pub fn poll(&mut self) {
        loop {
            match self.status.accept() {
                Ok((sock, _)) => drop(sock),
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        if self.client.is_some() {
            return;
        }
        let Ok((mut sock, _)) = self.stream.accept() else {
            return;
        };
        let _ = sock.set_nonblocking(false);
        let _ = sock.set_read_timeout(Some(POLL));
        let deadline = Instant::now() + REQUEST_BUDGET;
        if !request_ok(&mut sock, deadline)
            || !write_by(&mut sock, stream::response_head(), deadline)
        {
            return;
        }
        println!("stream open");
        self.client = Some(sock);
    }

    /// Write one JPEG as a part. A failed write drops the client.
    pub fn send_jpeg(&mut self, jpeg: &[u8]) {
        if jpeg.is_empty() {
            return;
        }
        let Some(sock) = self.client.as_mut() else {
            return;
        };
        let prefix = stream::part_prefix(jpeg.len());
        let deadline = Instant::now() + FRAME_BUDGET;
        let ok = write_by(sock, &prefix, deadline)
            && write_by(sock, jpeg, deadline)
            && write_by(sock, stream::PART_SUFFIX, deadline);
        if !ok {
            println!("stream closed");
            self.client = None;
        }
    }
}

/// True when the request line starts with GET and the headers ended within [`HEAD_MAX`]
/// and before `deadline`. The socket's read timeout must be set.
pub fn request_ok(sock: &mut TcpStream, deadline: Instant) -> bool {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while buf.len() < HEAD_MAX {
        if Instant::now() >= deadline {
            return false;
        }
        match sock.read(&mut byte) {
            Ok(0) => return false,
            Ok(_) => {
                buf.push(byte[0]);
                if buf.ends_with(b"\r\n\r\n") {
                    return buf.starts_with(b"GET ");
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) => {}
            Err(_) => return false,
        }
    }
    false
}

/// Writes all of `bytes` before `deadline`, or returns false. `write_all` would
/// restart the socket timeout after every partial write.
pub fn write_by(sock: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> bool {
    while !bytes.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || sock.set_write_timeout(Some(left.max(SHORTEST))).is_err() {
            return false;
        }
        match sock.write(bytes) {
            Ok(0) => return false,
            Ok(n) => bytes = &bytes[n..],
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    true
}
