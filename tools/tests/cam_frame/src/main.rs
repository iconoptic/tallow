//! Print one multipart response on stdout.
//!
//! `tools/mesh.sh cam-frame` copies `firmware/cam/src/stream.rs` to
//! `src/stream.rs` before building this. The bytes are that file's.

#[path = "stream.rs"]
mod stream;

use std::io::{self, Write};

fn write_part(out: &mut Vec<u8>, jpeg: &[u8]) {
    out.extend_from_slice(&stream::part_prefix(jpeg.len()));
    out.extend_from_slice(jpeg);
    out.extend_from_slice(stream::PART_SUFFIX);
}

fn main() {
    let jpeg_a = b"\xff\xd8one\xff\xd9";
    let jpeg_b = b"\xff\xd8two\xff\xd9";
    let mut out = Vec::new();
    out.extend_from_slice(stream::response_head());
    write_part(&mut out, jpeg_a);
    write_part(&mut out, jpeg_b);
    io::stdout().write_all(&out).unwrap();
}
