//! Multipart MJPEG bytes the panel already parses.
//!
//! `dial` in `app/cams.py` wants status `200` and a `Content-Type` that starts
//! with `multipart/x-mixed-replace`. `Upstream.frames` then reads header lines
//! until a blank line, takes `Content-Length`, and reads that many bytes.
//! Nothing here is chunked. This file has no ESP-IDF types, so a host test can
//! build the same bytes the camera writes.

/// Boundary token in the response head and in each part.
pub const BOUNDARY: &str = "frame";

/// HTTP response head. No `Content-Length` for the whole body, and no
/// `Transfer-Encoding`, because the stream does not end.
pub fn response_head() -> &'static [u8] {
    b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\n\r\n"
}

/// The header lines of one part, including the blank line before the JPEG.
pub fn part_prefix(len: usize) -> Vec<u8> {
    format!("--{BOUNDARY}\r\nContent-Type: image/jpeg\r\nContent-Length: {len}\r\n\r\n")
        .into_bytes()
}

/// The line break after a JPEG. The next part's headers follow it.
pub const PART_SUFFIX: &[u8] = b"\r\n";

/// Missed AP records in a row before a station that is still up reconnects.
pub const MISSES: u8 = 3;

/// Whether this loop pass should reconnect, and the miss count to keep.
/// A down netif reconnects at once. A missed AP record waits for [`MISSES`].
pub fn reconnect_now(up: bool, heard: bool, misses: u8) -> (bool, u8) {
    if !up {
        return (true, 0);
    }
    if heard {
        return (false, 0);
    }
    let next = misses.saturating_add(1);
    if next >= MISSES {
        (true, 0)
    } else {
        (false, next)
    }
}
