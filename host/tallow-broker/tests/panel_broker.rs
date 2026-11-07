//! Port of `tools/tests/test_panel_broker.py`, case by case. The Python test
//! name is in each doc comment. The rest are the allowlist walk, the panel route
//! refusals, the Python reference table, and the limits. Every test uses fake
//! sockets in a temp directory. None opens vsock or touches `secrets/` or `data/`.

use std::collections::VecDeque;
use std::fs;
use std::future::Future;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tallow_broker::config::{load_config, load_token, Config};
use tallow_broker::http::{
    build_upstream, classify, parse_request, parse_upstream, read_message, respond, status_of,
    Header,
};
use tallow_broker::serve::{handle_connection, handle_within, serve, GuestListener};
use tallow_broker::upstream::forward;
use tallow_broker::{Failure, MAX_REQUEST, MAX_RESPONSE, PORT, REFUSED_HEADERS};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

const JSON: [(&str, &str); 1] = [("Content-Type", "application/json")];

/// Builds a raw HTTP/1.1 request: request line, headers, blank line, body.
fn raw(line: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = line.as_bytes().to_vec();
    out.extend_from_slice(b"\r\n");
    for (name, value) in headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

/// A raw request padded with an `X-Pad` header to exactly `total` bytes.
fn padded(total: usize, line: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let base = raw(
        line,
        &headers
            .iter()
            .copied()
            .chain([("X-Pad", "")])
            .collect::<Vec<_>>(),
        body,
    );
    let fill = "a".repeat(total - base.len());
    raw(
        line,
        &headers
            .iter()
            .copied()
            .chain([("X-Pad", fill.as_str())])
            .collect::<Vec<_>>(),
        body,
    )
}

/// The Python's `_request` helper for classify tests: headers as owned pairs.
fn owned(pairs: &[(&str, &str)]) -> Vec<Header> {
    pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// What the guest sees for a raw request: the status the broker refuses with,
/// or "forward". Parses, then classifies, as `handle_connection` does.
fn label(blob: &[u8]) -> String {
    match parse_request(blob) {
        Err(Failure::Status { status, .. }) => status.to_string(),
        Err(Failure::Io(error)) => panic!("parsing a byte slice gave an io error: {error}"),
        Ok(request) => match classify(
            &request.method,
            &request.target,
            &request.headers,
            &request.body,
        ) {
            None => "forward".to_string(),
            Some(status) => status.to_string(),
        },
    }
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A temp directory removed on drop. Socket paths stay under 108 bytes.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("tb-{}-{tag}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set mode");
}

/// A broker config file in `<root>/secrets/panel/broker.json`, mode 0600.
fn write_config(root: &Path, cid: u64, socket: &Path, token_file: &Path) -> PathBuf {
    let path = root.join("secrets/panel/broker.json");
    fs::create_dir_all(path.parent().expect("parent")).expect("create secrets dir");
    let body = serde_json::json!({
        "cid": cid,
        "port": PORT,
        "socket": socket,
        "token_file": token_file,
    });
    fs::write(&path, body.to_string()).expect("write config");
    set_mode(&path, 0o600);
    path
}

/// The bearer file, one line, mode 0600. The value is a test value, not a secret.
fn write_token(root: &Path, value: &str) -> PathBuf {
    let path = root.join("secrets/panel/hermes.token");
    fs::create_dir_all(path.parent().expect("parent")).expect("create secrets dir");
    fs::write(&path, format!("{value}\n")).expect("write token");
    set_mode(&path, 0o600);
    path
}

/// A guest-side pair: `client` is the guest, `server` is what the broker holds.
fn guest_pair() -> (UnixStream, UnixStream) {
    UnixStream::pair().expect("socket pair")
}

/// A fake `GuestListener` that yields the queued connections, then fails.
struct FakeListener {
    queue: VecDeque<(UnixStream, u32)>,
}

impl GuestListener for FakeListener {
    type Stream = UnixStream;

    fn accept(&mut self) -> impl Future<Output = io::Result<(UnixStream, u32)>> + Send {
        let next = self.queue.pop_front();
        async move { next.ok_or_else(|| io::Error::other("no more fake guests")) }
    }
}

// ---------------------------------------------------------------------------
// Ports of the five Python tests
// ---------------------------------------------------------------------------

/// Python: `AllowlistTest.test_read_and_control_pass_and_library_writes_do_not`
#[test]
fn read_and_control_pass_and_library_writes_do_not() {
    let json = owned(&[
        ("Content-Type", "application/json"),
        ("Content-Length", "2"),
    ]);
    assert_eq!(classify("GET", "/api/manifest", &[], b""), None);
    assert_eq!(
        classify("GET", "/api/patterns/evening-walk", &[], b""),
        None
    );
    assert_eq!(classify("GET", "/api/schedules/evening", &[], b""), None);
    assert_eq!(classify("POST", "/api/power", &json, b"{}"), None);
    assert_eq!(
        classify("POST", "/api/patterns/evening-walk/apply", &json, b"{}"),
        None
    );
    assert_eq!(classify("POST", "/api/patterns", &json, b"{}"), Some(403));
    assert_eq!(classify("POST", "/api/schedules", &json, b"{}"), Some(403));
    assert_eq!(
        classify("PUT", "/api/patterns/evening-walk", &json, b"{}"),
        Some(403)
    );
    assert_eq!(
        classify("DELETE", "/api/schedules/evening", &[], b""),
        Some(403)
    );
    assert_eq!(
        classify("GET", "/api/patterns/Not-A-Slug", &[], b""),
        Some(403)
    );
    assert_eq!(classify("POST", "/login", &json, b"{}"), Some(403));
}

/// Python: `AllowlistTest.test_client_credentials_query_and_chunking_are_refused`
#[test]
fn client_credentials_query_and_chunking_are_refused() {
    assert_eq!(
        classify("GET", "/api/state", &owned(&[("Cookie", "a")]), b""),
        Some(400)
    );
    assert_eq!(
        classify(
            "GET",
            "/api/state",
            &owned(&[("Authorization", "Bearer x")]),
            b""
        ),
        Some(400)
    );
    assert_eq!(classify("GET", "/api/state?x=1", &[], b""), Some(400));

    let chunked = raw(
        "POST /api/power HTTP/1.1",
        &[
            ("Transfer-Encoding", "chunked"),
            ("Content-Type", "application/json"),
        ],
        b"{}",
    );
    match parse_request(&chunked) {
        Err(Failure::Status { status, .. }) => assert_eq!(status, 400),
        other => panic!("chunked request was not refused with 400: {other:?}"),
    }

    let doubled = raw(
        "POST /api/power HTTP/1.1",
        &[
            ("Content-Type", "application/json"),
            ("Content-Length", "2"),
            ("Content-Length", "2"),
        ],
        b"{}",
    );
    match parse_request(&doubled) {
        Err(Failure::Status { status, .. }) => assert_eq!(status, 400),
        other => panic!("doubled Content-Length was not refused with 400: {other:?}"),
    }
}

/// Python: `ForwardTest.test_bearer_is_added_and_the_cookie_is_not_passed_back`
#[tokio::test]
async fn bearer_is_added_and_the_cookie_is_not_passed_back() {
    let dir = TempDir::new("bearer");
    let sock = dir.path().join("agent.sock");
    let panel = UnixListener::bind(&sock).expect("bind fake panel");
    let panel_task = tokio::spawn(async move {
        let (mut conn, _) = panel.accept().await.expect("accept");
        let seen = read_message(&mut conn, MAX_REQUEST, Duration::from_secs(2))
            .await
            .expect("read upstream request");
        let body = br#"{"device":"h7020"}"#;
        let mut reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nSet-Cookie: tallow=browser\r\n\r\n",
            body.len()
        )
        .into_bytes();
        reply.extend_from_slice(body);
        let _ = conn.write_all(&reply).await;
        seen
    });

    let (mut client, mut server) = guest_pair();
    client
        .write_all(&raw("GET /api/state HTTP/1.1", &[], b""))
        .await
        .expect("guest writes");
    handle_connection(&mut server, "unit-token", &sock, 3, 3).await;
    drop(server);
    let mut response = Vec::new();
    client
        .read_to_end(&mut response)
        .await
        .expect("guest reads");

    let seen = panel_task.await.expect("panel task");
    assert!(contains(&seen, b"Authorization: Bearer unit-token\r\n"));
    assert!(!contains(&seen, b"Cookie"));
    assert!(!contains(&response, b"unit-token"));
    assert!(!contains(&response, b"Set-Cookie"));
    assert!(contains(&response, br#""device":"h7020""#));
    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
}

/// Python: `ForwardTest.test_missing_socket_is_503_and_a_foreign_cid_gets_nothing`
#[tokio::test]
async fn missing_socket_is_503_and_a_foreign_cid_gets_nothing() {
    let dir = TempDir::new("missing");
    let missing = dir.path().join("agent.sock");

    let (mut client, mut server) = guest_pair();
    client
        .write_all(&raw("GET /api/state HTTP/1.1", &[], b""))
        .await
        .expect("guest writes");
    handle_connection(&mut server, "unit-token", &missing, 3, 3).await;
    drop(server);
    let mut response = Vec::new();
    client
        .read_to_end(&mut response)
        .await
        .expect("guest reads");
    assert!(
        response.starts_with(b"HTTP/1.1 503 "),
        "got {:?}",
        String::from_utf8_lossy(&response)
    );

    let (mut foreign, mut foreign_server) = guest_pair();
    foreign
        .write_all(&raw("GET /api/state HTTP/1.1", &[], b""))
        .await
        .expect("foreign writes");
    handle_connection(&mut foreign_server, "unit-token", &missing, 3, 9).await;
    drop(foreign_server);
    // Unread bytes plus a close is a reset on Linux. Either way no HTTP answer arrives.
    let mut buf = Vec::new();
    match foreign.read_to_end(&mut buf).await {
        Ok(_) => assert!(buf.is_empty(), "foreign CID got bytes: {buf:?}"),
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::ConnectionReset),
    }
}

/// Python: `SecretFileTest.test_secret_file_is_mode_0600_and_stdout_stays_empty`.
/// The Python mints the token with `panel_token.py` first. That is not ported,
/// so this test writes the file itself. The stdout check does not apply to Rust,
/// so the token-never-echoed checks cover it instead.
#[test]
fn secret_file_is_mode_0600_and_config_names_the_panel_socket() {
    let dir = TempDir::new("secret");
    let root = dir.path();
    let token_path = write_token(root, "unit-token-value");
    assert_eq!(
        fs::metadata(&token_path)
            .expect("stat")
            .permissions()
            .mode()
            & 0o7777,
        0o600
    );
    assert_eq!(
        load_token(&token_path).expect("token loads"),
        "unit-token-value"
    );

    let sock = root.join("data/panel/agent.sock");
    let config_path = write_config(root, 3, &sock, &token_path);
    let config = load_config(&config_path).expect("config loads");
    assert_eq!(config.cid, 3);
    assert_eq!(config.port, PORT);
    assert_eq!(config.socket, sock);
    assert_eq!(config.token_file, token_path);

    let wrong_sock = root.join("data/panel-preview/agent.sock");
    let wrong_path = write_config(root, 3, &wrong_sock, &token_path);
    assert!(load_config(&wrong_path).is_err());
}

// ---------------------------------------------------------------------------
// Allowlist walks
// ---------------------------------------------------------------------------

/// Every (method, path) the broker forwards. Written out by hand, not read from
/// the code, so a change to the allowlist shows up here.
const ALLOWED: &[(&str, &str)] = &[
    ("GET", "/api/state"),
    ("GET", "/api/scenes"),
    ("GET", "/api/capabilities"),
    ("GET", "/api/manifest"),
    ("GET", "/api/patterns"),
    ("GET", "/api/schedules"),
    ("GET", "/api/patterns/evening-walk"),
    ("GET", "/api/schedules/evening"),
    ("GET", "/api/traffic/live"),
    ("GET", "/api/traffic/hour"),
    ("GET", "/api/traffic/day"),
    ("GET", "/api/traffic/week"),
    ("POST", "/api/traffic/alias"),
    ("POST", "/api/power"),
    ("POST", "/api/brightness"),
    ("POST", "/api/color"),
    ("POST", "/api/scene"),
    ("POST", "/api/paint"),
    ("POST", "/api/patterns/evening-walk/apply"),
    ("POST", "/api/patterns/a--1/apply"),
];

/// Every path the matrix tries: the allowlist, the panel's own routes, and
/// the variants a path check has to refuse.
const PATHS: &[&str] = &[
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
    "/api/traffic/alias",
    "/api/traffic",
    "/api/traffic/clear",
    "/api/traffic/layout",
    "/api/power",
    "/api/brightness",
    "/api/color",
    "/api/scene",
    "/api/paint",
    "/api/patterns/evening-walk",
    "/api/schedules/evening",
    "/api/patterns/evening-walk/apply",
    "/api/patterns/evening-walk/",
    "/api/patterns/evening-walk/apply/x",
    "/api/schedules/evening/apply",
    "/api/patterns/Not-A-Slug",
    "/api/patterns/",
    "/api/patterns/a--1/apply",
    "/api/patterns/./evening-walk",
    "/api/patterns/../state",
    "/api/state/",
    "/api/state/..",
    "/api/state/.",
    "/api/state?x=1",
    "/api/state?",
    "/api/state#x",
    "/api/%73tate",
    "/api/state%",
    "//api/state",
    "/api//state",
    "/api\\state",
    "/API/STATE",
    "/api/state.json",
    "/api/state\t",
    "/",
    "/lights",
    "/lights/",
    "/cams",
    "/settings",
    "/panel.css",
    "/login",
    "/logout",
    "/api/cams",
    "/api/cams/status",
    "/cams/den/stream",
    "/api/settings/cameras",
    "/api/power/",
];

/// The request a matrix cell sends. POST carries a JSON object, as the control routes need.
fn matrix_request(method: &str, path: &str) -> Vec<u8> {
    if method == "POST" {
        let mut all = JSON.to_vec();
        all.push(("Content-Length", "2"));
        raw(&format!("POST {path} HTTP/1.1"), &all, b"{}")
    } else {
        raw(&format!("{method} {path} HTTP/1.1"), &[], b"")
    }
}

/// Python: no test of this shape. It walks the allowlist as the Python defines
/// it. Each refused route is still refused with 400 or 403, and nothing else
/// is forwarded.
#[test]
fn walk_every_refused_route_is_still_refused() {
    let methods = [
        "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "get", "post",
    ];
    let mut failures = Vec::new();
    let mut forwarded = Vec::new();
    for method in methods {
        for &path in PATHS {
            let got = label(&matrix_request(method, path));
            let allowed = ALLOWED.contains(&(method, path));
            if got == "forward" {
                forwarded.push((method, path));
            }
            if allowed != (got == "forward") {
                failures.push(format!(
                    "{method} {path}: expected allowed={allowed}, got {got}"
                ));
            } else if !allowed && got != "400" && got != "403" {
                failures.push(format!(
                    "{method} {path}: refused with {got}, not 400 or 403"
                ));
            }
        }
    }
    forwarded.sort();
    let mut expected: Vec<(&str, &str)> = ALLOWED.to_vec();
    expected.sort();
    assert_eq!(forwarded, expected, "the forwarded set changed");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every route the panel exposes outside the allowlist, from `app/h7020_panel.py`
/// (`@app.get`, `@app.post`, `@app.put`, `@app.delete`). Each must be refused with 403.
#[test]
fn every_route_the_panel_exposes_outside_the_allowlist_is_refused() {
    let panel_routes: &[(&str, &str)] = &[
        ("GET", "/"),
        ("GET", "/lights"),
        ("GET", "/cams"),
        ("GET", "/settings"),
        ("GET", "/panel.css"),
        ("GET", "/login"),
        ("GET", "/api/cams"),
        ("GET", "/api/cams/status"),
        ("GET", "/cams/den/stream"),
        ("GET", "/api/settings/cameras"),
        ("POST", "/login"),
        ("POST", "/logout"),
        ("POST", "/api/patterns"),
        ("POST", "/api/schedules"),
        ("PUT", "/api/settings/cameras"),
        ("PUT", "/api/patterns/evening-walk"),
        ("PUT", "/api/schedules/evening"),
        ("DELETE", "/api/patterns/evening-walk"),
        ("DELETE", "/api/schedules/evening"),
        ("GET", "/api/traffic"),
        ("GET", "/traffic"),
        ("POST", "/api/traffic/layout"),
        ("POST", "/api/traffic/clear"),
    ];
    for (method, path) in panel_routes {
        assert_eq!(
            label(&matrix_request(method, path)),
            "403",
            "{method} {path} should be refused with 403"
        );
    }
}

#[test]
fn traffic_summaries_are_forwarded_and_the_log_writes_are_not() {
    for path in [
        "/api/traffic/live",
        "/api/traffic/hour",
        "/api/traffic/day",
        "/api/traffic/week",
    ] {
        assert_eq!(
            label(&raw(&format!("GET {path} HTTP/1.1"), &[], b"")),
            "forward",
            "{path}"
        );
    }
    assert_eq!(
        label(&raw("GET /api/traffic/live?window=day HTTP/1.1", &[], b"")),
        "400"
    );
    assert_eq!(label(&raw("GET /api/traffic HTTP/1.1", &[], b"")), "403");
    let json = [
        ("Content-Type", "application/json"),
        ("Content-Length", "2"),
    ];
    assert_eq!(
        label(&raw("POST /api/traffic/clear HTTP/1.1", &json, b"{}")),
        "403"
    );
    assert_eq!(
        label(&raw("POST /api/traffic/layout HTTP/1.1", &json, b"{}")),
        "403"
    );
    let alias = [
        ("Content-Type", "application/json"),
        ("Content-Length", "20"),
    ];
    assert_eq!(
        label(&raw(
            "POST /api/traffic/alias HTTP/1.1",
            &alias,
            br#"{"alias":"Calm Fox"}"#,
        )),
        "forward"
    );
    assert_eq!(
        label(&raw("GET /api/traffic/alias HTTP/1.1", &[], b"")),
        "403"
    );
    assert_eq!(
        label(&raw(
            "POST /api/traffic/alias?x=1 HTTP/1.1",
            &alias,
            br#"{"alias":"Calm Fox"}"#,
        )),
        "400"
    );
}

// ---------------------------------------------------------------------------
// The Python reference table: every row was produced by running
// tools/panel_broker.py's parse_request and classify on the same bytes.
// ---------------------------------------------------------------------------

const PYTHON_TABLE: &[(&str, &[u8], &str)] = &[
    ("get exact /api/state", b"GET /api/state HTTP/1.1\r\nHost: x\r\n\r\n", "forward"),
    ("get exact /api/scenes", b"GET /api/scenes HTTP/1.1\r\nHost: x\r\n\r\n", "forward"),
    ("get exact /api/capabilities", b"GET /api/capabilities HTTP/1.1\r\nHost: x\r\n\r\n", "forward"),
    ("get exact /api/manifest", b"GET /api/manifest HTTP/1.1\r\nHost: x\r\n\r\n", "forward"),
    ("get exact /api/patterns", b"GET /api/patterns HTTP/1.1\r\nHost: x\r\n\r\n", "forward"),
    ("get exact /api/schedules", b"GET /api/schedules HTTP/1.1\r\nHost: x\r\n\r\n", "forward"),
    ("get pattern slug", b"GET /api/patterns/evening-walk HTTP/1.1\r\n\r\n", "forward"),
    ("get schedule slug", b"GET /api/schedules/evening HTTP/1.1\r\n\r\n", "forward"),
    ("get schedule slug 63 chars", b"GET /api/schedules/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa HTTP/1.1\r\n\r\n", "forward"),
    ("get schedule slug 64 chars", b"GET /api/schedules/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa HTTP/1.1\r\n\r\n", "403"),
    ("get state content-length 0", b"GET /api/state HTTP/1.1\r\nContent-Length: 0\r\n\r\n", "forward"),
    ("get state padded header value", b"GET /api/state HTTP/1.1\r\nX-Note:   spaced  \t\r\n\r\n", "forward"),
    ("get state header holds Authorization text", b"GET /api/state HTTP/1.1\r\nX-Note: Authorization: Bearer x\r\n\r\n", "forward"),
    ("get state header value with control byte", b"GET /api/state HTTP/1.1\r\nX-Note: a\x01b\r\n\r\n", "forward"),
    ("get state connection keep-alive", b"GET /api/state HTTP/1.1\r\nConnection: keep-alive\r\n\r\n", "forward"),
    ("get '/api/patterns/Not-A-Slug'", b"GET /api/patterns/Not-A-Slug HTTP/1.1\r\n\r\n", "403"),
    ("get '/api/patterns/'", b"GET /api/patterns/ HTTP/1.1\r\n\r\n", "403"),
    ("get '/api/patterns/a/b'", b"GET /api/patterns/a/b HTTP/1.1\r\n\r\n", "403"),
    ("get '/api/patterns/evening-walk/'", b"GET /api/patterns/evening-walk/ HTTP/1.1\r\n\r\n", "403"),
    ("get '/api/patterns/evening-walk/apply'", b"GET /api/patterns/evening-walk/apply HTTP/1.1\r\n\r\n", "403"),
    ("get '/api/schedules/1evening'", b"GET /api/schedules/1evening HTTP/1.1\r\n\r\n", "403"),
    ("get '/api/schedules/Evening'", b"GET /api/schedules/Evening HTTP/1.1\r\n\r\n", "403"),
    ("get '/api/schedules/eve_ning'", b"GET /api/schedules/eve_ning HTTP/1.1\r\n\r\n", "403"),
    ("get '/api/state/'", b"GET /api/state/ HTTP/1.1\r\n\r\n", "403"),
    ("get '/api/state/.'", b"GET /api/state/. HTTP/1.1\r\n\r\n", "403"),
    ("get '/api/patterns/./evening-walk'", b"GET /api/patterns/./evening-walk HTTP/1.1\r\n\r\n", "403"),
    ("get '/api/state?x=1'", b"GET /api/state?x=1 HTTP/1.1\r\n\r\n", "400"),
    ("get '/api/state?'", b"GET /api/state? HTTP/1.1\r\n\r\n", "400"),
    ("get '/api/state#x'", b"GET /api/state#x HTTP/1.1\r\n\r\n", "400"),
    ("get '/api/%73tate'", b"GET /api/%73tate HTTP/1.1\r\n\r\n", "400"),
    ("get '/api/state%'", b"GET /api/state% HTTP/1.1\r\n\r\n", "400"),
    ("get '//api/state'", b"GET //api/state HTTP/1.1\r\n\r\n", "400"),
    ("get '/api//state'", b"GET /api//state HTTP/1.1\r\n\r\n", "400"),
    ("get '/api/patterns/../state'", b"GET /api/patterns/../state HTTP/1.1\r\n\r\n", "400"),
    ("get '/api/state/..'", b"GET /api/state/.. HTTP/1.1\r\n\r\n", "400"),
    ("get '/api\\\\state'", b"GET /api\\state HTTP/1.1\r\n\r\n", "400"),
    ("get 'api/state'", b"GET api/state HTTP/1.1\r\n\r\n", "400"),
    ("get 'http://host/api/state'", b"GET http://host/api/state HTTP/1.1\r\n\r\n", "400"),
    ("get '/API/STATE'", b"GET /API/STATE HTTP/1.1\r\n\r\n", "403"),
    ("get '/api/state.json'", b"GET /api/state.json HTTP/1.1\r\n\r\n", "403"),
    ("get '/'", b"GET / HTTP/1.1\r\n\r\n", "403"),
    ("get '/login'", b"GET /login HTTP/1.1\r\n\r\n", "403"),
    ("get '/panel.css'", b"GET /panel.css HTTP/1.1\r\n\r\n", "403"),
    ("get '/lights'", b"GET /lights HTTP/1.1\r\n\r\n", "403"),
    ("get '/api/cams'", b"GET /api/cams HTTP/1.1\r\n\r\n", "403"),
    ("get '/cams/den/stream'", b"GET /cams/den/stream HTTP/1.1\r\n\r\n", "403"),
    ("get '/api/settings/cameras'", b"GET /api/settings/cameras HTTP/1.1\r\n\r\n", "403"),
    ("get '/api/schedules/evening\\n'", b"GET /api/schedules/evening\n HTTP/1.1\r\n\r\n", "400"),
    ("get '/api/state\\t'", b"GET /api/state\t HTTP/1.1\r\n\r\n", "400"),
    ("get state with a bare LF in the target", b"GET /api/state\nx HTTP/1.1\r\n\r\n", "403"),
    ("get non-ascii target", b"GET /api/caf\xc3\xa9 HTTP/1.1\r\n\r\n", "400"),
    ("line 'GET  /api/state HTTP/1.1'", b"GET  /api/state HTTP/1.1\r\n\r\n", "400"),
    ("line 'get /api/state HTTP/1.1'", b"get /api/state HTTP/1.1\r\n\r\n", "400"),
    ("line 'Get /api/state HTTP/1.1'", b"Get /api/state HTTP/1.1\r\n\r\n", "400"),
    ("line ' /api/state HTTP/1.1'", b" /api/state HTTP/1.1\r\n\r\n", "400"),
    ("line 'PATCH /api/state HTTP/1.1'", b"PATCH /api/state HTTP/1.1\r\n\r\n", "403"),
    ("line 'HEAD /api/state HTTP/1.1'", b"HEAD /api/state HTTP/1.1\r\n\r\n", "403"),
    ("line 'OPTIONS /api/state HTTP/1.1'", b"OPTIONS /api/state HTTP/1.1\r\n\r\n", "403"),
    ("line 'DELETE /api/patterns/evening-walk HTTP/1.1'", b"DELETE /api/patterns/evening-walk HTTP/1.1\r\n\r\n", "403"),
    ("line 'GET /api/state HTTP/1.0'", b"GET /api/state HTTP/1.0\r\n\r\n", "400"),
    ("line 'GET /api/state HTTP/2.0'", b"GET /api/state HTTP/2.0\r\n\r\n", "400"),
    ("line 'OPTIONS * HTTP/1.1'", b"OPTIONS * HTTP/1.1\r\n\r\n", "400"),
    ("line 'GET /api/state'", b"GET /api/state\r\n\r\n", "400"),
    ("line 'GET\\t/api/state HTTP/1.1'", b"GET\t/api/state HTTP/1.1\r\n\r\n", "400"),
    ("post exact or apply /api/power", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "forward"),
    ("post exact or apply /api/brightness", b"POST /api/brightness HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "forward"),
    ("post exact or apply /api/color", b"POST /api/color HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "forward"),
    ("post exact or apply /api/scene", b"POST /api/scene HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "forward"),
    ("post exact or apply /api/paint", b"POST /api/paint HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "forward"),
    ("post exact or apply /api/patterns/evening-walk/apply", b"POST /api/patterns/evening-walk/apply HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "forward"),
    ("post json with trailing spaces", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 4\r\n\r\n{}  ", "forward"),
    ("post apply with nested object", b"POST /api/patterns/evening-walk/apply HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"a\":[1,2]}", "forward"),
    ("post refused /api/patterns", b"POST /api/patterns HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "403"),
    ("post refused /api/schedules", b"POST /api/schedules HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "403"),
    ("post refused /api/patterns/evening-walk", b"POST /api/patterns/evening-walk HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "403"),
    ("post refused /api/schedules/evening", b"POST /api/schedules/evening HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "403"),
    ("post refused /api/schedules/evening/apply", b"POST /api/schedules/evening/apply HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "403"),
    ("post refused /api/patterns/evening-walk/apply/x", b"POST /api/patterns/evening-walk/apply/x HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "403"),
    ("post refused /api/power/", b"POST /api/power/ HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "403"),
    ("post refused /login", b"POST /login HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "403"),
    ("post refused /logout", b"POST /logout HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "403"),
    ("post refused /api/state", b"POST /api/state HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "403"),
    ("post refused /api/nope", b"POST /api/nope HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "403"),
    ("post refused /api/patterns/Bad_Slug/apply", b"POST /api/patterns/Bad_Slug/apply HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "403"),
    ("post power with query", b"POST /api/power? HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "400"),
    ("post power with query value", b"POST /api/power?x=1 HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "400"),
    ("post content-type None", b"POST /api/power HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}", "400"),
    ("post content-type 'text/plain'", b"POST /api/power HTTP/1.1\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\n{}", "400"),
    ("post content-type 'Application/JSON'", b"POST /api/power HTTP/1.1\r\nContent-Type: Application/JSON\r\nContent-Length: 2\r\n\r\n{}", "400"),
    ("post content-type 'application/json; charset=utf-8'", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: 2\r\n\r\n{}", "400"),
    ("post content-type 'application/json '", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json \r\nContent-Length: 2\r\n\r\n{}", "forward"),
    ("post content-type twice", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "400"),
    ("post body array to power", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n[]", "400"),
    ("post body array to nope", b"POST /api/nope HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n[]", "400"),
    ("post body unterminated", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 1\r\n\r\n{", "400"),
    ("post body empty with length 0", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 0\r\n\r\n", "400"),
    ("post body null", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 4\r\n\r\nnull", "400"),
    ("post body number", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 1\r\n\r\n1", "400"),
    ("post body string", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 3\r\n\r\n\"x\"", "400"),
    ("post body object to nope", b"POST /api/nope HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "403"),
    ("post content-length longer than body", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 3\r\n\r\n{}", "400"),
    ("post content-length shorter than body", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}x", "400"),
    ("post body without content-length", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\n\r\n{}", "400"),
    ("post no body and no content-length", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\n\r\n", "400"),
    ("post content-length 02", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 02\r\n\r\n{}", "400"),
    ("post body with leading zero length", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: +2\r\n\r\n{}", "400"),
    ("put pattern with json", b"PUT /api/patterns/evening-walk HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "403"),
    ("put settings cameras", b"PUT /api/settings/cameras HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "403"),
    ("put pattern without body", b"PUT /api/patterns/evening-walk HTTP/1.1\r\n\r\n", "403"),
    ("delete schedule", b"DELETE /api/schedules/evening HTTP/1.1\r\n\r\n", "403"),
    ("delete schedule with json", b"DELETE /api/schedules/evening HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}", "403"),
    ("get state with refused header Cookie", b"GET /api/state HTTP/1.1\r\nCookie: tallow=browser\r\n\r\n", "400"),
    ("get state with refused header Authorization", b"GET /api/state HTTP/1.1\r\nAuthorization: Bearer x\r\n\r\n", "400"),
    ("get state with refused header authorization", b"GET /api/state HTTP/1.1\r\nauthorization: Bearer x\r\n\r\n", "400"),
    ("get state with refused header COOKIE", b"GET /api/state HTTP/1.1\r\nCOOKIE: a=b\r\n\r\n", "400"),
    ("get state with refused header Proxy-Authorization", b"GET /api/state HTTP/1.1\r\nProxy-Authorization: x\r\n\r\n", "400"),
    ("get state with refused header Transfer-Encoding", b"GET /api/state HTTP/1.1\r\nTransfer-Encoding: identity\r\n\r\n", "400"),
    ("post with transfer-encoding", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n{}", "400"),
    ("post with cookie", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\nCookie: a=b\r\nContent-Length: 2\r\n\r\n{}", "400"),
    ("post with authorization", b"POST /api/power HTTP/1.1\r\nContent-Type: application/json\r\nAuthorization: Bearer x\r\nContent-Length: 2\r\n\r\n{}", "400"),
    ("get content-length 0 twice", b"GET /api/state HTTP/1.1\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n", "400"),
    ("get content-length '00'", b"GET /api/state HTTP/1.1\r\nContent-Length: 00\r\n\r\n", "400"),
    ("get content-length '+0'", b"GET /api/state HTTP/1.1\r\nContent-Length: +0\r\n\r\n", "400"),
    ("get content-length '0x0'", b"GET /api/state HTTP/1.1\r\nContent-Length: 0x0\r\n\r\n", "400"),
    ("get content-length ''", b"GET /api/state HTTP/1.1\r\nContent-Length: \r\n\r\n", "400"),
    ("get content-length '99999999999999999999999'", b"GET /api/state HTTP/1.1\r\nContent-Length: 99999999999999999999999\r\n\r\n", "400"),
    ("get content-length 3 without body", b"GET /api/state HTTP/1.1\r\nContent-Length: 3\r\n\r\n", "400"),
    ("get body without content-length", b"GET /api/state HTTP/1.1\r\n\r\nx", "400"),
    ("get with body and content-length", b"GET /api/state HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}", "400"),
    ("get with content-type and no body", b"GET /api/state HTTP/1.1\r\nContent-Type: application/json\r\n\r\n", "400"),
    ("get with content-type and content-length 0", b"GET /api/state HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 0\r\n\r\n", "400"),
    ("header without colon", b"GET /api/state HTTP/1.1\r\nFoo\r\n\r\n", "400"),
    ("header with leading space", b"GET /api/state HTTP/1.1\r\n X: y\r\n\r\n", "400"),
    ("header with leading tab", b"GET /api/state HTTP/1.1\r\n\tX: y\r\n\r\n", "400"),
    ("header name with space", b"GET /api/state HTTP/1.1\r\nX Y: z\r\n\r\n", "400"),
    ("header name empty", b"GET /api/state HTTP/1.1\r\n: z\r\n\r\n", "400"),
    ("header name with control byte", b"GET /api/state HTTP/1.1\r\nX\x01: y\r\n\r\n", "400"),
    ("header value non-ascii", b"GET /api/state HTTP/1.1\r\nX: caf\xc3\xa9\r\n\r\n", "400"),
    ("header value padded", b"GET /api/state HTTP/1.1\r\nX-Note: \t value \t\r\n\r\n", "forward"),
    ("bare lf line endings", b"GET /api/state HTTP/1.1\nHost: x\n\n", "400"),
    ("no header terminator", b"GET /api/state HTTP/1.1\r\nHost: x\r\n", "400"),
    ("empty head", b"\r\n\r\n", "400"),
    ("extra crlf after head", b"GET /api/state HTTP/1.1\r\n\r\n\r\n", "400"),
    ("pipelined second request", b"GET /api/state HTTP/1.1\r\n\r\nGET /api/state HTTP/1.1\r\n\r\n", "400"),
    ("cookie name padded", b"GET /api/state HTTP/1.1\r\nCookie : a=b\r\n\r\n", "400"),
    ("chunked with content-length", b"GET /api/state HTTP/1.1\r\nTransfer-Encoding: chunked\r\nContent-Length: 0\r\n\r\n", "400"),
];

#[test]
fn python_refusal_table_matches_the_rust_port() {
    assert_eq!(PYTHON_TABLE.len(), 146, "the reference table shrank");
    let mut mismatches = Vec::new();
    for (name, bytes, expected) in PYTHON_TABLE {
        let got = label(bytes);
        if got != *expected {
            mismatches.push(format!("{name}: python {expected}, rust {got}"));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/// Python: `MAX_REQUEST = 16 * 1024`. Exactly 16 KiB passes. One byte more is a 400.
#[test]
fn request_limit_is_16_kib_on_the_head_and_body_together() {
    assert_eq!(MAX_REQUEST, 16 * 1024);
    assert_eq!(
        label(&padded(16384, "GET /api/state HTTP/1.1", &[], b"")),
        "forward"
    );
    assert_eq!(
        label(&padded(16385, "GET /api/state HTTP/1.1", &[], b"")),
        "400"
    );
    let post = [
        ("Content-Type", "application/json"),
        ("Content-Length", "2"),
    ];
    assert_eq!(
        label(&padded(16384, "POST /api/power HTTP/1.1", &post, b"{}")),
        "forward"
    );
    assert_eq!(
        label(&padded(16385, "POST /api/power HTTP/1.1", &post, b"{}")),
        "400"
    );
}

/// The same limit applied to the read itself, on a fake socket.
#[tokio::test]
async fn read_refuses_a_request_one_byte_over_the_limit() {
    for (total, ok) in [(16384usize, true), (16385usize, false)] {
        let (mut client, mut server) = UnixStream::pair().expect("pair");
        let blob = padded(total, "GET /api/state HTTP/1.1", &[], b"");
        client.write_all(&blob).await.expect("write");
        drop(client);
        let result = read_message(&mut server, MAX_REQUEST, Duration::from_secs(2)).await;
        assert_eq!(result.is_ok(), ok, "total {total}");
    }
}

/// `read_message` gives up after its deadline, with no answer.
#[tokio::test]
async fn a_stalled_guest_times_out_without_an_answer() {
    let (mut client, mut server) = UnixStream::pair().expect("pair");
    client
        .write_all(b"GET /api/st")
        .await
        .expect("write partial");
    let result = read_message(&mut server, MAX_REQUEST, Duration::from_millis(50)).await;
    match result {
        Err(Failure::Io(error)) => assert_eq!(error.kind(), io::ErrorKind::TimedOut),
        other => panic!("expected a timeout, got {other:?}"),
    }
}

/// `forward` gives up on a silent panel and answers 502. Python: `UPSTREAM_TIMEOUT`.
#[tokio::test]
async fn a_silent_panel_is_a_502() {
    let dir = TempDir::new("silent");
    let sock = dir.path().join("agent.sock");
    let panel = UnixListener::bind(&sock).expect("bind");
    let silent = tokio::spawn(async move {
        let (_conn, _) = panel.accept().await.expect("accept");
        tokio::time::sleep(Duration::from_millis(500)).await;
    });
    let upstream = build_upstream("GET", "/api/state", b"", "unit-token").expect("build");
    match forward(&sock, &upstream, Duration::from_millis(50)).await {
        Err(Failure::Status { status, detail }) => {
            assert_eq!(status, 502);
            assert_eq!(detail, "panel unreachable");
        }
        other => panic!("expected 502, got {other:?}"),
    }
    silent.abort();
}

/// Python: `MAX_RESPONSE = 256 * 1024`. A panel answer of exactly 256 KiB is
/// passed on. One byte more is a 502.
#[tokio::test]
async fn panel_answer_is_capped_at_256_kib() {
    assert_eq!(MAX_RESPONSE, 256 * 1024);
    for (total, ok) in [(MAX_RESPONSE, true), (MAX_RESPONSE + 1, false)] {
        let dir = TempDir::new("cap");
        let sock = dir.path().join("agent.sock");
        let panel = UnixListener::bind(&sock).expect("bind");
        let head_for = |n: usize| {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {n}\r\n\r\n"
            )
        };
        // The head's own length depends on the body's length, so settle on a fixed point.
        let mut body_len = total;
        let head = loop {
            let head = head_for(body_len);
            let next = total - head.len();
            if next == body_len {
                break head;
            }
            body_len = next;
        };
        assert_eq!(head.len() + body_len, total);
        let panel_task = tokio::spawn(async move {
            let (mut conn, _) = panel.accept().await.expect("accept");
            let mut reply = head.into_bytes();
            reply.extend(std::iter::repeat_n(b'x', body_len));
            let _ = conn.write_all(&reply).await;
        });
        let upstream = build_upstream("GET", "/api/state", b"", "unit-token").expect("build");
        let result = forward(&sock, &upstream, Duration::from_secs(5)).await;
        assert_eq!(result.is_ok(), ok, "panel answer of {total} bytes");
        let _ = panel_task.await;
    }
}

// ---------------------------------------------------------------------------
// Bytes the broker builds, against the Python's output
// ---------------------------------------------------------------------------

#[test]
fn upstream_request_has_the_bearer_and_no_guest_header() {
    let post = build_upstream("POST", "/api/power", b"{}", "tok").expect("build");
    assert_eq!(
        post,
        b"POST /api/power HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer tok\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{}".to_vec()
    );
    let get = build_upstream("GET", "/api/state", b"", "tok").expect("build");
    assert_eq!(
        get,
        b"GET /api/state HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer tok\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".to_vec()
    );
}

#[test]
fn refusals_are_the_python_json_bodies() {
    assert_eq!(
        respond(403, "missing scope"),
        b"HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: 27\r\nConnection: close\r\n\r\n{\"detail\": \"missing scope\"}".to_vec()
    );
    assert_eq!(
        respond(503, "panel socket unavailable"),
        b"HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: 38\r\nConnection: close\r\n\r\n{\"detail\": \"panel socket unavailable\"}".to_vec()
    );
    assert_eq!(status_of(&respond(400, "bad request")), 400);
}

#[test]
fn panel_answer_keeps_only_type_and_length_and_rebuilds_the_status_line() {
    let blob = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nSet-Cookie: x=1\r\nContent-Length: 2\r\n\r\n{}".to_vec();
    assert_eq!(
        parse_upstream(&blob).expect("parse"),
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_vec()
    );
    let no_length = b"HTTP/1.1 204\r\nX: y\r\n\r\n".to_vec();
    assert_eq!(
        parse_upstream(&no_length).expect("parse"),
        b"HTTP/1.1 204 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
    );
}

// ---------------------------------------------------------------------------
// Secret handling
// ---------------------------------------------------------------------------

const SECRET: &str = "tok-secret-7f3a9c";

/// The token never appears in any answer the guest can get. Each request below
/// fails a different way, and the token must stay out of all of them.
#[tokio::test]
async fn the_token_never_appears_in_any_answer() {
    let dir = TempDir::new("leak");
    let missing = dir.path().join("agent.sock");
    let bearer = format!("Bearer {SECRET}");
    let cases: Vec<Vec<u8>> = vec![
        raw(
            "GET /api/state HTTP/1.1",
            &[("Authorization", bearer.as_str())],
            b"",
        ),
        raw("GET /api/state HTTP/1.1", &[("Cookie", SECRET)], b""),
        raw("GET /api/cams HTTP/1.1", &[], b""),
        raw("GET /api/state HTTP/1.1", &[], b""),
        b"GET /api/state\r\n".to_vec(),
    ];
    for request in cases {
        let (mut client, mut server) = guest_pair();
        client.write_all(&request).await.expect("guest writes");
        // Half-close, so a request that never completes ends at EOF, not at the 5 s timeout.
        client.shutdown().await.expect("guest half-closes");
        handle_connection(&mut server, SECRET, &missing, 3, 3).await;
        drop(server);
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .await
            .expect("guest reads");
        assert!(
            !contains(&response, SECRET.as_bytes()),
            "token leaked in {:?}",
            String::from_utf8_lossy(&response)
        );
    }
}

/// A token file that is not 0600 is refused. The guest gets 503, and the token
/// is not in the answer or the error. Python: `load_token`, `check_mode`.
#[tokio::test]
async fn token_file_that_is_not_0600_is_refused_and_never_echoed() {
    let dir = TempDir::new("badmode");
    let root = dir.path();
    let token_path = write_token(root, SECRET);
    set_mode(&token_path, 0o644);
    let error = load_token(&token_path).expect_err("0644 must be refused");
    assert!(!format!("{error:#}").contains(SECRET));

    let sock = root.join("data/panel/agent.sock");
    let config = Config {
        cid: 3,
        port: PORT,
        socket: sock,
        token_file: token_path,
    };
    let (mut client, server) = guest_pair();
    client
        .write_all(&raw("GET /api/state HTTP/1.1", &[], b""))
        .await
        .expect("guest writes");
    let mut listener = FakeListener {
        queue: VecDeque::from([(server, 3)]),
    };
    let result = serve(&mut listener, &config).await;
    assert!(result.is_err(), "serve only returns when accept fails");
    // The broker answers before it reads the guest's request, so the close can
    // reset the connection and the 503 is lost. The Python does the same. Either
    // outcome is fine here. The token must be in neither.
    let mut response = Vec::new();
    match client.read_to_end(&mut response).await {
        Ok(_) => {
            assert!(
                response.starts_with(b"HTTP/1.1 503 "),
                "got {:?}",
                String::from_utf8_lossy(&response)
            );
            assert!(contains(&response, b"token unreadable"));
        }
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::ConnectionReset),
    }
    assert!(!contains(&response, SECRET.as_bytes()));
}

/// Whitespace, control bytes, and non-ASCII in the token are refused at load.
#[test]
fn a_token_with_space_or_non_ascii_is_refused_at_load() {
    for bad in ["tok en", "tök", "tok\x01en", ""] {
        let dir = TempDir::new("badtoken");
        let path = write_token(dir.path(), bad);
        assert!(load_token(&path).is_err(), "token {bad:?} was accepted");
    }
}

/// The token file must be named `secrets/panel/hermes.token`. Anything else is refused.
#[test]
fn the_token_file_must_be_the_hermes_token_in_secrets_panel() {
    let dir = TempDir::new("badname");
    let path = dir.path().join("secrets/panel/other.token");
    fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    fs::write(&path, "tok\n").expect("write");
    set_mode(&path, 0o600);
    assert!(load_token(&path).is_err());
}

// ---------------------------------------------------------------------------
// The serve loop, with a fake listener
// ---------------------------------------------------------------------------

/// A foreign CID gets no bytes, and the allowed CID is answered from the panel.
#[tokio::test]
async fn serve_drops_a_foreign_cid_and_answers_the_allowed_one() {
    let dir = TempDir::new("serve");
    let root = dir.path();
    let sock = root.join("data/panel/agent.sock");
    fs::create_dir_all(sock.parent().expect("parent")).expect("dirs");
    let token_path = write_token(root, "unit-token");
    let config = Config {
        cid: 3,
        port: PORT,
        socket: sock.clone(),
        token_file: token_path,
    };

    let panel = UnixListener::bind(&sock).expect("bind fake panel");
    let panel_task = tokio::spawn(async move {
        let (mut conn, _) = panel.accept().await.expect("accept");
        let seen = read_message(&mut conn, MAX_REQUEST, Duration::from_secs(2))
            .await
            .expect("read");
        let _ = conn
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
            )
            .await;
        seen
    });

    let (mut foreign, foreign_server) = guest_pair();
    foreign
        .write_all(&raw("GET /api/state HTTP/1.1", &[], b""))
        .await
        .expect("foreign writes");
    let (mut allowed, allowed_server) = guest_pair();
    allowed
        .write_all(&raw("GET /api/state HTTP/1.1", &[], b""))
        .await
        .expect("allowed writes");
    let mut listener = FakeListener {
        queue: VecDeque::from([(foreign_server, 9), (allowed_server, 3)]),
    };
    let result = serve(&mut listener, &config).await;
    assert!(result.is_err());

    let mut foreign_bytes = Vec::new();
    match foreign.read_to_end(&mut foreign_bytes).await {
        Ok(_) => assert!(foreign_bytes.is_empty(), "foreign CID got bytes"),
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::ConnectionReset),
    }
    let mut allowed_bytes = Vec::new();
    allowed
        .read_to_end(&mut allowed_bytes)
        .await
        .expect("allowed reads");
    assert!(allowed_bytes.starts_with(b"HTTP/1.1 200 OK\r\n"));

    let seen = panel_task.await.expect("panel");
    assert!(contains(&seen, b"Authorization: Bearer unit-token\r\n"));
}

// ---------------------------------------------------------------------------
// Where the Rust is stricter than the Python, and each is pinned here
// ---------------------------------------------------------------------------

/// Python: `json.loads` raises UnicodeDecodeError here. That escapes `classify`,
/// and the serve loop answers 503 "token unreadable". The Rust answers 400.
#[test]
fn invalid_utf8_json_body_is_a_400() {
    let body = b"{\"\xff\":1}";
    let blob = raw(
        "POST /api/power HTTP/1.1",
        &[
            ("Content-Type", "application/json"),
            ("Content-Length", "8"),
        ],
        body,
    );
    assert_eq!(label(&blob), "400");
}

/// Python: a UTF-8 byte-order mark is accepted and forwarded. Rust refuses it.
#[test]
fn byte_order_mark_json_body_is_a_400() {
    let body = b"\xef\xbb\xbf{}";
    let blob = raw(
        "POST /api/power HTTP/1.1",
        &[
            ("Content-Type", "application/json"),
            ("Content-Length", "5"),
        ],
        body,
    );
    assert_eq!(label(&blob), "400");
}

/// Python: `NaN` is accepted by `json.loads` and forwarded. Rust refuses it.
#[test]
fn nan_json_body_is_a_400() {
    let body = b"{\"a\":NaN}";
    let blob = raw(
        "POST /api/power HTTP/1.1",
        &[
            ("Content-Type", "application/json"),
            ("Content-Length", "9"),
        ],
        body,
    );
    assert_eq!(label(&blob), "400");
}

/// Python: forwards any three-digit-or-not status. Rust forwards 100 to 599 only.
#[test]
fn a_panel_status_outside_100_to_599_is_a_502() {
    let blob = b"HTTP/1.1 999 Weird\r\nContent-Length: 0\r\n\r\n".to_vec();
    assert!(parse_upstream(&blob).is_err());
}

/// Python: a bare LF inside a kept header value is forwarded. Rust answers 502.
#[test]
fn a_panel_header_with_a_bare_lf_is_a_502() {
    let blob =
        b"HTTP/1.1 200 OK\r\nContent-Type: a\nX-Injected: 1\r\nContent-Length: 0\r\n\r\n".to_vec();
    assert!(parse_upstream(&blob).is_err());
}

/// `REFUSED_HEADERS` is the Python's set, in lowercase. A guest may not send any of them.
#[test]
fn refused_headers_are_the_python_set() {
    assert_eq!(
        REFUSED_HEADERS,
        &[
            "authorization",
            "cookie",
            "transfer-encoding",
            "proxy-authorization"
        ]
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// A guest that stops reading cannot hold the one-at-a-time accept loop.
#[tokio::test]
async fn a_guest_that_stops_reading_cannot_hold_the_broker() {
    let dir = TempDir::new("stalled-reader");
    let missing = dir.path().join("agent.sock");
    for line in ["GET /api/state HTTP/1.1", "POST /login HTTP/1.1"] {
        let (mut guest, mut server) = tokio::io::duplex(8);
        let request = raw(line, &[("Content-Length", "0")], b"");
        let writer = tokio::spawn(async move {
            let _ = guest.write_all(&request).await;
            // Holds the stream open and never reads.
            tokio::time::sleep(Duration::from_secs(3600)).await;
            drop(guest);
        });
        let answered = tokio::time::timeout(
            Duration::from_secs(2),
            handle_within(
                &mut server,
                "unit-token",
                &missing,
                3,
                3,
                Duration::from_millis(50),
            ),
        )
        .await;
        writer.abort();
        assert!(answered.is_ok(), "{line} held the connection");
    }
}
