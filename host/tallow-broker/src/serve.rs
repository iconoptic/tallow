//! One guest connection, and the accept loop. Mirrors `handle_connection` and
//! `serve` in `tools/panel_broker.py`. The listener is a trait, so the tests
//! run with fake sockets and no vsock.

use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::path::Path;

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use crate::config::{load_token, Config};
use crate::http::{build_upstream, classify, parse_request, read_message, respond, status_of};
use crate::upstream::forward;
use crate::{Failure, MAX_REQUEST, REQUEST_TIMEOUT, UPSTREAM_TIMEOUT};

/// Accepts guest connections. The vsock implementation is in `vsock.rs`.
pub trait GuestListener {
    type Stream: AsyncRead + AsyncWrite + Unpin + Send;

    /// Returns the next connection and the peer's CID.
    fn accept(&mut self) -> impl Future<Output = io::Result<(Self::Stream, u32)>> + Send;
}

/// What the log line says about the request. Python: `method`, `path`.
struct Seen {
    method: String,
    path: String,
}

/// Answers one guest connection. A peer that is not `allowed_cid` gets no
/// bytes at all. Every other outcome is answered or logged once.
pub async fn handle_connection<S>(
    conn: &mut S,
    token: &str,
    sock_path: &Path,
    allowed_cid: u32,
    peer_cid: u32,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if peer_cid != allowed_cid {
        eprintln!("broker refused peer {peer_cid}");
        return;
    }
    let mut seen = Seen {
        method: "-".to_string(),
        path: "-".to_string(),
    };
    let status = match answer(conn, token, sock_path, &mut seen).await {
        Ok(response) => {
            let status = status_of(&response);
            if conn.write_all(&response).await.is_err() {
                502
            } else {
                status
            }
        }
        Err(Failure::Status { status, detail }) => {
            // The Python ignores a failed write here and keeps the status.
            let _ = conn.write_all(&respond(status, detail)).await;
            status
        }
        // A read or socket error gets no answer. The Python closes with no bytes.
        Err(Failure::Io(_)) => 502,
    };
    eprintln!("broker {peer_cid} {} {:?} {status}", seen.method, seen.path);
}

/// The request path: read, parse, allowlist, then forward. Returns the bytes to send.
async fn answer<S>(
    conn: &mut S,
    token: &str,
    sock_path: &Path,
    seen: &mut Seen,
) -> Result<Vec<u8>, Failure>
where
    S: AsyncRead + Unpin,
{
    let blob = read_message(conn, MAX_REQUEST, REQUEST_TIMEOUT).await?;
    let request = parse_request(&blob)?;
    seen.method = request.method.clone();
    seen.path = request.target.clone();
    if let Some(refusal) = classify(
        &request.method,
        &request.target,
        &request.headers,
        &request.body,
    ) {
        let detail = if refusal == 400 {
            "bad request"
        } else {
            "missing scope"
        };
        return Ok(respond(refusal, detail));
    }
    let upstream = build_upstream(&request.method, &request.target, &request.body, token)?;
    forward(sock_path, &upstream, UPSTREAM_TIMEOUT).await
}

/// Python's `serve` loop. It accepts one connection at a time. A foreign CID is
/// dropped unanswered. The token is re-read for every connection, so a change
/// on disk takes effect without a restart. Returns only when accept fails.
pub async fn serve<L>(listener: &mut L, config: &Config) -> io::Result<Infallible>
where
    L: GuestListener,
{
    loop {
        let (mut conn, peer) = listener.accept().await?;
        if peer != config.cid {
            eprintln!("broker refused peer {peer}");
            continue;
        }
        match load_token(&config.token_file) {
            Ok(token) => {
                handle_connection(&mut conn, &token, &config.socket, config.cid, peer).await;
            }
            Err(error) => {
                eprintln!("broker: {error:#}");
                let _ = conn.write_all(&respond(503, "token unreadable")).await;
            }
        }
    }
}
