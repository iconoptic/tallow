//! Vsock broker for the H7020 agent socket. Rust port of `tools/panel_broker.py`.
//!
//! A guest VM sends one HTTP/1.1 request per connection over vsock. The broker
//! accepts one guest CID, checks the request against an allowlist, adds the
//! bearer token, and forwards it to `data/panel/agent.sock`. The token never
//! leaves the host. Every limit below is copied from the Python.

#![forbid(unsafe_code)]

pub mod config;
pub mod http;
pub mod serve;
pub mod upstream;
pub mod vsock;

use std::io;
use std::time::Duration;

/// The vsock port the guest dials. Python: `PORT`.
pub const PORT: u16 = 17620;
/// Largest request the broker reads, head and body together. Python: `MAX_REQUEST`.
pub const MAX_REQUEST: usize = 16 * 1024;
/// Largest response the broker reads from the panel. Python: `MAX_RESPONSE`.
pub const MAX_RESPONSE: usize = 256 * 1024;
/// Time the guest has to send its request. Python: `REQUEST_TIMEOUT`.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Time the panel has to connect, take the request, and answer. Python: `UPSTREAM_TIMEOUT`.
pub const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(15);

/// Read routes that match a path exactly. Python: `GET_EXACT`.
pub const GET_EXACT: &[&str] = &[
    "/api/state",
    "/api/scenes",
    "/api/capabilities",
    "/api/manifest",
    "/api/patterns",
    "/api/schedules",
    "/api/traffic/live",
    "/api/traffic/hour",
    "/api/traffic/day",
    "/api/traffic/week",
];
/// Control routes that match a path exactly, and the traffic alias lookup.
/// The lookup is a read. It is a POST because a query string is refused and an
/// alias can contain a space. Python: `POST_EXACT`, which does not include it.
pub const POST_EXACT: &[&str] = &[
    "/api/power",
    "/api/brightness",
    "/api/color",
    "/api/scene",
    "/api/paint",
    "/api/traffic/alias",
];
/// Headers the guest may not send. Python: `REFUSED_HEADERS`, compared lowercase.
pub const REFUSED_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "transfer-encoding",
    "proxy-authorization",
];

/// Why a request failed. `Status` is answered to the caller as JSON. `Io` is a
/// socket error or timeout, and the connection is closed with no answer,
/// as the Python does when it catches an `OSError`.
#[derive(Debug)]
pub enum Failure {
    Status { status: u16, detail: &'static str },
    Io(io::Error),
}

impl Failure {
    pub fn status(status: u16, detail: &'static str) -> Self {
        Failure::Status { status, detail }
    }
}

/// The Python's `BrokerError(400, "bad request")`.
pub fn bad_request() -> Failure {
    Failure::status(400, "bad request")
}

/// Python's `str.isspace()` for the ASCII range. Everything the broker sees is ASCII.
pub fn is_py_space(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c | 0x1c..=0x1f
    )
}

/// Python's `str.strip()` with no argument, for ASCII text.
pub fn py_strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_ascii() && is_py_space(c as u8))
}
