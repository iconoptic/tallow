//! Request parsing, the allowlist, and the bytes the broker builds. Mirrors
//! `parse_request`, `read_message`, `classify`, `build_upstream`, and
//! `_read_upstream` in `tools/panel_broker.py`.

use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::{bad_request, py_strip, Failure, GET_EXACT, MAX_REQUEST, POST_EXACT, REFUSED_HEADERS};

/// A header as the guest sent it: name as written, value with Python's `strip()` applied.
pub type Header = (String, String);

/// A parsed guest request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub target: String,
    pub headers: Vec<Header>,
    pub body: Vec<u8>,
}

/// The status line and JSON body the broker answers with. Python: `respond`.
pub fn respond(status: u16, detail: &str) -> Vec<u8> {
    let body = format!("{{\"detail\": \"{detail}\"}}");
    let head = format!(
        "HTTP/1.1 {status} {phrase}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
        phrase = phrase(status).unwrap_or("OK"),
        len = body.len(),
    );
    let mut out = head.into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// Python's `PHRASES`. Only these four statuses are ever written by the broker.
fn phrase(status: u16) -> Option<&'static str> {
    match status {
        400 => Some("Bad Request"),
        403 => Some("Forbidden"),
        502 => Some("Bad Gateway"),
        503 => Some("Service Unavailable"),
        _ => None,
    }
}

/// The status a response line carries, or 502 when it has none. Python: `_status_of`.
pub fn status_of(response: &[u8]) -> u16 {
    let end = response
        .windows(2)
        .position(|pair| pair == b"\r\n")
        .unwrap_or(response.len());
    response[..end]
        .split(|&b| b == b' ')
        .nth(1)
        .and_then(|part| std::str::from_utf8(part).ok())
        .and_then(|part| part.parse::<u16>().ok())
        .unwrap_or(502)
}

fn find_terminator(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|window| window == b"\r\n\r\n")
}

fn ascii(bytes: &[u8]) -> Result<&str, Failure> {
    if !bytes.is_ascii() {
        return Err(bad_request());
    }
    // Only ASCII bytes reach here, so this is valid UTF-8.
    std::str::from_utf8(bytes).map_err(|_| bad_request())
}

/// Python's `_length`: digits only, no leading zero unless the value is `0`.
fn length_of(value: &str) -> Result<usize, Failure> {
    let digits = !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
    if !digits || (value.len() > 1 && value.starts_with('0')) {
        return Err(bad_request());
    }
    // Overflow is refused like any other bad length.
    value.parse::<usize>().map_err(|_| bad_request())
}

/// Python's `_target_rejected`. A target must be a plain absolute path.
fn target_rejected(target: &str) -> bool {
    if !target.starts_with('/') || py_strip(target) != target {
        return true;
    }
    if target.contains(['?', '#', '\\', '%']) || target.contains("//") {
        return true;
    }
    target.split('/').any(|segment| segment == "..")
}

fn is_upper_alpha(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_uppercase())
}

/// Python's `parse_request`. Every failure is a 400.
pub fn parse_request(blob: &[u8]) -> Result<Request, Failure> {
    if blob.len() > MAX_REQUEST {
        return Err(bad_request());
    }
    let split = find_terminator(blob).ok_or_else(bad_request)?;
    let head = &blob[..split];
    let rest = &blob[split + 4..];

    let text = ascii(head)?;
    let lines: Vec<&str> = text.split("\r\n").collect();
    let parts: Vec<&str> = lines[0].split(' ').collect();
    if parts.len() != 3 || parts[2] != "HTTP/1.1" || !is_upper_alpha(parts[0]) {
        return Err(bad_request());
    }
    let method = parts[0];
    let target = parts[1];

    let mut headers: Vec<Header> = Vec::new();
    for &line in &lines[1..] {
        if line.is_empty() || line.starts_with([' ', '\t']) || !line.contains(':') {
            return Err(bad_request());
        }
        let (name, value) = line.split_once(':').ok_or_else(bad_request)?;
        if name.is_empty() || name.bytes().any(|b| !(33..=126).contains(&b)) {
            return Err(bad_request());
        }
        headers.push((name.to_string(), py_strip(value).to_string()));
    }

    if target_rejected(target) {
        return Err(bad_request());
    }
    let lengths: Vec<&str> = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.as_str())
        .collect();
    if lengths.len() > 1 {
        return Err(bad_request());
    }
    if headers.iter().any(|(name, _)| is_refused_header(name)) {
        return Err(bad_request());
    }
    match lengths.first() {
        Some(value) => {
            let length = length_of(value)?;
            if length != rest.len() || split + 4 + length > MAX_REQUEST {
                return Err(bad_request());
            }
        }
        None => {
            if !rest.is_empty() {
                return Err(bad_request());
            }
        }
    }

    Ok(Request {
        method: method.to_string(),
        target: target.to_string(),
        headers,
        body: rest.to_vec(),
    })
}

fn is_refused_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    REFUSED_HEADERS.contains(&lower.as_str())
}

/// Python's `_read_slug`: `/api/patterns/<slug>` or `/api/schedules/<slug>`.
fn read_slug(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    parts.len() == 4
        && parts[1] == "api"
        && matches!(parts[2], "patterns" | "schedules")
        && slug_ok(parts[3])
}

/// Python's `_apply_slug`: `/api/patterns/<slug>/apply`.
fn apply_slug(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    parts.len() == 5
        && parts[1] == "api"
        && parts[2] == "patterns"
        && parts[4] == "apply"
        && slug_ok(parts[3])
}

/// Python's `SLUG`: `^[a-z][a-z0-9-]{0,62}$`, matched in full.
fn slug_ok(text: &str) -> bool {
    let bytes = text.as_bytes();
    match bytes.split_first() {
        Some((first, rest)) => {
            bytes.len() <= 63
                && first.is_ascii_lowercase()
                && rest
                    .iter()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        }
        None => false,
    }
}

/// Python's `classify`. `Some(status)` refuses the request. `None` forwards it.
/// The checks run in the Python's order, so the same request gets the same status.
pub fn classify(method: &str, path: &str, headers: &[Header], body: &[u8]) -> Option<u16> {
    if target_rejected(path) {
        return Some(400);
    }
    if headers.iter().any(|(name, _)| is_refused_header(name)) {
        return Some(400);
    }
    let count = |wanted: &str| {
        headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case(wanted))
            .count()
    };
    if count("content-length") > 1 || count("content-type") > 1 {
        return Some(400);
    }
    let lengths: Vec<&str> = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.as_str())
        .collect();
    match lengths.first() {
        Some(value) => match length_of(value) {
            Ok(length) if length == body.len() => {}
            _ => return Some(400),
        },
        None => {
            if !body.is_empty() {
                return Some(400);
            }
        }
    }

    match method {
        "GET" => {
            if !body.is_empty() || count("content-type") > 0 {
                return Some(400);
            }
            if GET_EXACT.contains(&path) || read_slug(path) {
                None
            } else {
                Some(403)
            }
        }
        "POST" => {
            let types: Vec<&str> = headers
                .iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                .map(|(_, value)| value.as_str())
                .collect();
            if types != ["application/json"] {
                return Some(400);
            }
            // The Python parses the body before the allowlist. A body that is
            // not a JSON object is a 400 on any path.
            match serde_json::from_slice::<Value>(body) {
                Ok(Value::Object(_)) => {}
                _ => return Some(400),
            }
            if POST_EXACT.contains(&path) || apply_slug(path) {
                None
            } else {
                Some(403)
            }
        }
        _ => Some(403),
    }
}

/// Python's `read_message`, with the whole read under one deadline. Reads one
/// HTTP message of at most `limit` bytes.
pub async fn read_message<R>(
    conn: &mut R,
    limit: usize,
    timeout: Duration,
) -> Result<Vec<u8>, Failure>
where
    R: AsyncRead + Unpin,
{
    match tokio::time::timeout(timeout, read_message_bounded(conn, limit)).await {
        Ok(result) => result,
        Err(_) => Err(Failure::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "message read timed out",
        ))),
    }
}

/// `read_message` without its own deadline. The caller bounds it.
pub async fn read_message_bounded<R>(conn: &mut R, limit: usize) -> Result<Vec<u8>, Failure>
where
    R: AsyncRead + Unpin,
{
    let mut data: Vec<u8> = Vec::new();
    let split = loop {
        if let Some(index) = find_terminator(&data) {
            break index;
        }
        let want = 4096usize.min(limit + 1 - data.len());
        let mut chunk = vec![0u8; want];
        let n = conn.read(&mut chunk).await.map_err(Failure::Io)?;
        if n == 0 {
            return Err(bad_request());
        }
        data.extend_from_slice(&chunk[..n]);
        if data.len() > limit {
            return Err(bad_request());
        }
    };

    let head = &data[..split];
    let pairs = header_pairs(head)?;
    if pairs
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("transfer-encoding"))
    {
        return Err(bad_request());
    }
    let lengths: Vec<&str> = pairs
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.as_str())
        .collect();
    if lengths.len() > 1 {
        return Err(bad_request());
    }
    let Some(value) = lengths.first() else {
        if data.len() > split + 4 {
            return Err(bad_request());
        }
        return Ok(data);
    };
    let length = length_of(value)?;
    let total = split + 4 + length;
    if total > limit {
        return Err(bad_request());
    }
    while data.len() < total {
        let mut chunk = vec![0u8; total - data.len()];
        let n = conn.read(&mut chunk).await.map_err(Failure::Io)?;
        if n == 0 {
            return Err(bad_request());
        }
        data.extend_from_slice(&chunk[..n]);
    }
    if data.len() != total {
        return Err(bad_request());
    }
    Ok(data)
}

/// Python's `_header_pairs`: the header lines of a head, values stripped.
fn header_pairs(head: &[u8]) -> Result<Vec<Header>, Failure> {
    let text = ascii(head)?;
    Ok(text
        .split("\r\n")
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_string(), py_strip(value).to_string()))
        .collect())
}

fn bad_gateway() -> Failure {
    Failure::status(502, "panel unreachable")
}

/// Python's `build_upstream`. The guest's headers are not copied. The broker
/// writes its own, and adds the bearer.
pub fn build_upstream(
    method: &str,
    path: &str,
    body: &[u8],
    token: &str,
) -> Result<Vec<u8>, Failure> {
    if token.is_empty() || !token.is_ascii() || token.bytes().any(crate::is_py_space) {
        return Err(Failure::status(503, "token unreadable"));
    }
    let mut lines = vec![
        format!("{method} {path} HTTP/1.1"),
        "Host: localhost".to_string(),
        format!("Authorization: Bearer {token}"),
        "Connection: close".to_string(),
        format!("Content-Length: {}", body.len()),
    ];
    if method == "POST" {
        lines.insert(3, "Content-Type: application/json".to_string());
    }
    let mut out = format!("{}\r\n\r\n", lines.join("\r\n")).into_bytes();
    out.extend_from_slice(body);
    Ok(out)
}

/// Python's `_read_upstream` parsing. Keeps only `Content-Type` and
/// `Content-Length` from the panel's answer, and rebuilds the status line.
pub fn parse_upstream(blob: &[u8]) -> Result<Vec<u8>, Failure> {
    let split = find_terminator(blob).ok_or_else(bad_gateway)?;
    let head = &blob[..split];
    let body = &blob[split + 4..];

    let text = ascii(head).map_err(|_| bad_gateway())?;
    let lines: Vec<&str> = text.split("\r\n").collect();
    let status_parts: Vec<&str> = lines[0].split(' ').collect();
    if status_parts.len() < 2
        || status_parts[1].is_empty()
        || !status_parts[1].bytes().all(|b| b.is_ascii_digit())
    {
        return Err(bad_gateway());
    }
    // Tighter than the Python: only a real three-digit status is forwarded.
    let status: u16 = status_parts[1]
        .parse()
        .ok()
        .filter(|s| (100..=599).contains(s))
        .ok_or_else(bad_gateway)?;
    let joined = status_parts[2..].join(" ");
    let reason = if joined.is_empty() {
        phrase(status).unwrap_or("OK").to_string()
    } else {
        joined
    };
    if reason.contains(['\r', '\n']) {
        return Err(bad_gateway());
    }

    let mut kept: Vec<String> = Vec::new();
    for &line in &lines[1..] {
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        let lower = name.to_ascii_lowercase();
        if lower == "content-type" || lower == "content-length" {
            kept.push(format!("{name}: {}", py_strip(value)));
        }
    }
    // Tighter than the Python: a bare CR or LF inside a kept value would
    // split the response the guest reads.
    if kept.iter().any(|item| item.contains(['\r', '\n'])) {
        return Err(bad_gateway());
    }
    if !kept
        .iter()
        .any(|item| item.to_ascii_lowercase().starts_with("content-length:"))
    {
        kept.push(format!("Content-Length: {}", body.len()));
    }

    let mut out = format!("HTTP/1.1 {status} {reason}\r\n").into_bytes();
    out.extend_from_slice(kept.join("\r\n").as_bytes());
    out.extend_from_slice(b"\r\nConnection: close\r\n\r\n");
    out.extend_from_slice(body);
    Ok(out)
}
