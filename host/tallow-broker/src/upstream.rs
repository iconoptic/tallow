//! The hop to the panel's Unix socket. Mirrors `forward` in `tools/panel_broker.py`.

use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

use crate::http::{parse_upstream, read_message_bounded};
use crate::{Failure, MAX_RESPONSE};

/// Send one built request to the panel and return the response the guest will see.
/// Connect has its own deadline. Write and read share one deadline of `timeout`.
pub async fn forward(
    sock_path: &Path,
    upstream: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, Failure> {
    let unavailable = || Failure::status(503, "panel socket unavailable");
    if !is_socket(sock_path) {
        return Err(unavailable());
    }
    let mut stream = match tokio::time::timeout(timeout, UnixStream::connect(sock_path)).await {
        Ok(Ok(stream)) => stream,
        _ => return Err(unavailable()),
    };
    let exchange = async {
        stream
            .write_all(upstream)
            .await
            .map_err(|_| Failure::status(502, "panel unreachable"))?;
        let blob = read_message_bounded(&mut stream, MAX_RESPONSE)
            .await
            .map_err(|_| Failure::status(502, "panel unreachable"))?;
        parse_upstream(&blob)
    };
    match tokio::time::timeout(timeout, exchange).await {
        Ok(result) => result,
        Err(_) => Err(Failure::status(502, "panel unreachable")),
    }
}

/// Python's `Path.is_socket()`, which follows symlinks.
fn is_socket(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.file_type().is_socket())
        .unwrap_or(false)
}
