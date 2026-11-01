//! The panel in process, through the same router the two listeners serve. Ports of
//! `PagesTest`, `TokenDoorTest`, `LightAwayTest`, and the non-socket cases of
//! `tools/tests/test_h7020_panel.py`. The contract (`tools/tests/panel_contract.py`)
//! covers the same routes over real sockets.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{header, HeaderMap, Request};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt;

use super::access::{
    authenticate, cookie_value, load_agents, secret_eq, session_token, testing::write_agents,
    Channel,
};
use super::light::PHONE_HOME;
use super::routes;
use super::{offline_light, preview_light, Panel};
use crate::cams::{Book, Cam, Hubs, Tuning};
use crate::library::testing::TempDir;
use crate::library::Library;

const PASSWORD: &str = "secret";
const ALL: &[&str] = &["read", "control", "library", "schedule"];
const READ_ONLY: &[&str] = &["read"];
const HERMES: &str = "hermes-token-for-tests";
const READER: &str = "reader-token-for-tests";

fn browser() -> String {
    session_token(PASSWORD)
}

/// A panel whose files live in `dir`. The cams come from `cams_file` when it is given,
/// and from `cams` otherwise.
fn panel_in(
    dir: &TempDir,
    cams: Vec<Cam>,
    cams_file: Option<PathBuf>,
    preview: bool,
) -> Arc<Panel> {
    let root = dir.path();
    let library = Library::new(root.join("data"));
    let light = if preview {
        preview_light(library.clone())
    } else {
        offline_light(library.clone())
    };
    let book = match &cams_file {
        Some(path) => Book::new(Some(path.clone()), Vec::new()),
        None => Book::new(None, cams),
    };
    Arc::new(
        Panel::new(
            PASSWORD,
            light,
            library,
            root.join("agents.json"),
            cams_file,
            book,
            Hubs::new(Tuning::default()),
        )
        .with_map(root.join("mesh-map.json")),
    )
}

/// One request through the router for `channel`. `cookie` is the browser's token.
async fn send(
    panel: &Arc<Panel>,
    method: &str,
    path: &str,
    channel: Channel,
    bearer: Option<&str>,
    cookie: Option<&str>,
    body: &[u8],
) -> (u16, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, format!("tallow_panel={cookie}"));
    }
    if !body.is_empty() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    let request = builder
        .body(Body::from(body.to_vec()))
        .expect("a valid request");
    let app = routes::router(panel.clone(), channel);
    let response = app
        .oneshot(request)
        .await
        .expect("the router does not fail");
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("a finished body");
    (status, headers, bytes.to_vec())
}

fn text(body: &[u8]) -> String {
    String::from_utf8_lossy(body).into_owned()
}

fn json_body(body: &[u8]) -> Value {
    serde_json::from_slice(body).expect("a JSON body")
}

fn write_cams(path: &Path, cams: &Value) {
    std::fs::write(path, cams.to_string()).expect("write the cams file");
}

#[tokio::test]
async fn the_devices_page_and_the_mesh_map_are_lan_only_read_only_and_cookie_gated() {
    let dir = TempDir::new("devices-routes");
    let panel = panel_in(&dir, Vec::new(), None, true);
    let cookie = browser();
    for path in ["/devices", "/api/mesh/map"] {
        let (status, _, body) =
            send(&panel, "GET", path, Channel::Agent, Some(HERMES), None, b"").await;
        assert_eq!(status, 404, "{path} on the socket");
        assert_eq!(text(&body), r#"{"detail":"not found"}"#);
    }
    let (status, headers, _) = send(&panel, "GET", "/devices", Channel::Lan, None, None, b"").await;
    assert_eq!(status, 303);
    assert_eq!(
        headers.get(header::LOCATION).and_then(|v| v.to_str().ok()),
        Some("/login")
    );
    let (status, _, _) = send(
        &panel,
        "GET",
        "/api/mesh/map",
        Channel::Lan,
        None,
        None,
        b"",
    )
    .await;
    assert_eq!(status, 401);
    let (status, _, _) = send(
        &panel,
        "GET",
        "/api/mesh/map",
        Channel::Lan,
        Some(HERMES),
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(
        status, 401,
        "a bearer beside the cookie is refused on the LAN"
    );
    let (status, _, body) = send(
        &panel,
        "GET",
        "/api/mesh/map",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(status, 200);
    let doc = json_body(&body);
    assert_eq!(doc["stale"], json!(true));
    assert_eq!(doc["reason"], json!("missing"));
    let (status, _, body) = send(
        &panel,
        "GET",
        "/devices",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(status, 200);
    assert!(text(&body).contains("The map is not running."));
    let (status, _, _) = send(
        &panel,
        "POST",
        "/api/mesh/map",
        Channel::Lan,
        None,
        Some(&cookie),
        b"{}",
    )
    .await;
    assert_eq!(status, 405, "the map takes no writes");
}

// PagesTest

#[tokio::test]
async fn the_menu_leads_to_the_lights_the_cams_and_settings() {
    let dir = TempDir::new("pages-menu");
    let panel = panel_in(&dir, Vec::new(), None, true);
    let cookie = browser();
    let (status, _, body) = send(&panel, "GET", "/", Channel::Lan, None, Some(&cookie), b"").await;
    assert_eq!(status, 200);
    let page = text(&body);
    assert!(page.contains(r#"href="/lights""#));
    assert!(page.contains(r#"href="/cams""#));
    assert!(page.contains(r#"href="/settings""#));
    assert!(page.contains("<strong>Cameras</strong>"));

    let (status, _, body) = send(
        &panel,
        "GET",
        "/lights",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(status, 200);
    assert!(text(&body).contains("phones home"));

    let (status, _, body) = send(
        &panel,
        "GET",
        "/cams",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(status, 200);
    let page = text(&body);
    assert!(page.contains("Nothing is saved"));
    assert!(page.contains("<h1>Cameras</h1>"));
    assert!(page.contains(r#"href="/settings""#));

    let (status, _, body) = send(
        &panel,
        "GET",
        "/settings",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(status, 200);
    let page = text(&body);
    assert!(page.contains("Log out"));
    assert!(page.contains("10.87.0.0/24"));
    assert!(!page.contains("__IOT_NET__"));
    assert!(!page.contains("__CAMS_TITLE__"));
}

#[tokio::test]
async fn without_the_cookie_pages_go_to_login() {
    let dir = TempDir::new("pages-login");
    let cam = Cam::new("den", "Den", "10.87.0.21");
    let panel = panel_in(&dir, vec![cam], None, true);
    let (status, _, body) = send(&panel, "GET", "/", Channel::Lan, None, None, b"").await;
    assert_eq!(status, 200);
    assert!(text(&body).contains("Unlock"));
    for path in ["/lights", "/cams", "/settings"] {
        let (status, headers, _) = send(&panel, "GET", path, Channel::Lan, None, None, b"").await;
        assert_eq!(status, 303, "{path}");
        assert_eq!(
            headers.get(header::LOCATION).map(|v| v.as_bytes()),
            Some(&b"/login"[..]),
            "{path}"
        );
    }
    for path in ["/api/cams", "/cams/den/stream", "/api/settings/cameras"] {
        let (status, _, _) = send(&panel, "GET", path, Channel::Lan, None, None, b"").await;
        assert_eq!(status, 401, "{path}");
    }
}

#[tokio::test]
async fn the_agent_socket_sees_no_menu_and_no_cams() {
    let dir = TempDir::new("pages-socket");
    let agents = dir.path().join("agents.json");
    write_agents(&agents, &[("hermes", HERMES, ALL)]);
    let panel = panel_in(&dir, vec![Cam::new("den", "Den", "10.87.0.21")], None, true);
    for path in [
        "/lights",
        "/cams",
        "/settings",
        "/panel.css",
        "/api/cams",
        "/cams/den/stream",
        "/api/settings/cameras",
    ] {
        let (status, _, body) =
            send(&panel, "GET", path, Channel::Agent, Some(HERMES), None, b"").await;
        assert_eq!(status, 404, "{path}");
        assert_eq!(body, br#"{"detail":"not found"}"#, "{path}");
    }
    let (status, _, _) = send(
        &panel,
        "PUT",
        "/api/settings/cameras",
        Channel::Agent,
        Some(HERMES),
        None,
        b"{}",
    )
    .await;
    assert_eq!(status, 404);
    let (status, _, _) = send(
        &panel,
        "POST",
        "/logout",
        Channel::Agent,
        Some(HERMES),
        None,
        b"",
    )
    .await;
    assert_eq!(status, 404);
    let (status, _, body) = send(
        &panel,
        "GET",
        "/api/manifest",
        Channel::Agent,
        Some(HERMES),
        None,
        b"",
    )
    .await;
    assert_eq!(status, 200);
    let routes = json_body(&body)["routes"].clone();
    let routes = routes.as_array().expect("routes is a list");
    assert!(routes.iter().any(|route| route == "POST /api/power"));
    assert!(!routes.iter().any(|route| {
        let text = route.as_str().unwrap_or("");
        text.contains("cam") || text.contains("lights") || text.contains("settings")
    }));
}

#[tokio::test]
async fn the_cam_list_keeps_addresses_on_the_host() {
    let dir = TempDir::new("pages-cams");
    let panel = panel_in(&dir, vec![Cam::new("den", "Den", "10.87.0.21")], None, true);
    let (status, _, body) = send(
        &panel,
        "GET",
        "/api/cams",
        Channel::Lan,
        None,
        Some(&browser()),
        b"",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        json_body(&body),
        json!({"cams": [{"id": "den", "name": "Den"}], "error": null})
    );
    assert!(!text(&body).contains("10.87"));
}

#[tokio::test]
async fn a_cam_added_to_the_file_shows_up_without_a_restart() {
    let dir = TempDir::new("pages-added");
    let path = dir.path().join("cams.json");
    let panel = panel_in(&dir, Vec::new(), Some(path.clone()), true);
    let cookie = browser();
    let (_, _, body) = send(
        &panel,
        "GET",
        "/api/cams",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(json_body(&body)["cams"], json!([]));
    write_cams(
        &path,
        &json!({"cams": [{"id": "den", "name": "Den", "address": "10.87.0.21"}]}),
    );
    let (_, _, body) = send(
        &panel,
        "GET",
        "/api/cams",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    let ids: Vec<Value> = json_body(&body)["cams"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|cam| cam["id"].clone())
        .collect();
    assert_eq!(ids, vec![json!("den")]);
}

#[tokio::test]
async fn settings_saves_cameras_and_hides_addresses_everywhere_else() {
    let dir = TempDir::new("pages-settings");
    let path = dir.path().join("cams.json");
    write_cams(
        &path,
        &json!({"cams": [{"id": "den", "name": "Den", "address": "10.87.0.21"}]}),
    );
    let panel = panel_in(&dir, Vec::new(), Some(path.clone()), true);
    let cookie = browser();

    let (status, _, body) = send(
        &panel,
        "GET",
        "/api/settings/cameras",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        json_body(&body),
        json!({"title": "Cameras", "cams": [{"id": "den", "name": "Den", "address": "10.87.0.21"}], "error": null})
    );
    let (_, _, page) = send(&panel, "GET", "/", Channel::Lan, None, Some(&cookie), b"").await;
    assert!(text(&page).contains("<strong>Cameras</strong>"));

    let bad =
        json!({"title": "Yard", "cams": [{"id": "den", "name": "Den", "address": "192.168.1.9"}]});
    let (status, _, body) = send(
        &panel,
        "PUT",
        "/api/settings/cameras",
        Channel::Lan,
        None,
        Some(&cookie),
        bad.to_string().as_bytes(),
    )
    .await;
    assert_eq!(status, 400);
    assert!(text(&body).contains("must be a node on"));
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
    assert_eq!(saved["cams"][0]["address"], "10.87.0.21");

    let extra = json!({"title": "A & B", "extra": true, "cams": [{"id": "porch", "name": "Porch", "address": "10.87.0.22"}]});
    let (status, _, body) = send(
        &panel,
        "PUT",
        "/api/settings/cameras",
        Channel::Lan,
        None,
        Some(&cookie),
        extra.to_string().as_bytes(),
    )
    .await;
    assert_eq!(status, 400);
    assert!(text(&body).contains("unknown fields"));

    let good = json!({"title": "A & B", "cams": [{"id": "porch", "name": "Porch", "address": "10.87.0.22"}]});
    let (status, _, body) = send(
        &panel,
        "PUT",
        "/api/settings/cameras",
        Channel::Lan,
        None,
        Some(&cookie),
        good.to_string().as_bytes(),
    )
    .await;
    assert_eq!(status, 200, "{}", text(&body));
    let saved = json_body(&body);
    assert_eq!(saved["title"], "A & B");
    assert_eq!(
        saved["cams"],
        json!([{"id": "porch", "name": "Porch", "address": "10.87.0.22"}])
    );

    let (_, _, body) = send(
        &panel,
        "GET",
        "/api/cams",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(
        json_body(&body)["cams"],
        json!([{"id": "porch", "name": "Porch"}])
    );
    assert!(!text(&body).contains("10.87"));
    let (_, _, page) = send(&panel, "GET", "/", Channel::Lan, None, Some(&cookie), b"").await;
    assert!(text(&page).contains("<strong>A &amp; B</strong>"));
    assert!(!text(&page).contains("<strong>A & B</strong>"));
    let (_, _, page) = send(
        &panel,
        "GET",
        "/cams",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert!(text(&page).contains("<h1>A &amp; B</h1>"));
}

#[tokio::test]
async fn logout_clears_the_cookie() {
    let dir = TempDir::new("pages-logout");
    let panel = panel_in(&dir, Vec::new(), None, true);
    let (status, headers, _) = send(
        &panel,
        "POST",
        "/logout",
        Channel::Lan,
        None,
        Some(&browser()),
        b"",
    )
    .await;
    assert_eq!(status, 303);
    assert_eq!(
        headers.get(header::LOCATION).map(|v| v.as_bytes()),
        Some(&b"/login"[..])
    );
    let cleared = headers
        .get(header::SET_COOKIE)
        .expect("a Set-Cookie")
        .to_str()
        .expect("ASCII")
        .to_owned();
    assert!(cleared.starts_with("tallow_panel="));
    assert!(cleared.contains("Max-Age=0"));
}

/// A fake cam on 127.0.0.1 that serves the stream the way the ESPHome camera does: one
/// socket per viewer, a Content-Length on every part. It counts connections and the
/// sockets still open, so a test can see when the panel dials or hangs up.
struct FakeCam {
    port: u16,
    connections: Arc<AtomicUsize>,
    open: Arc<AtomicUsize>,
}

async fn fake_cam() -> FakeCam {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the fake cam");
    let port = listener.local_addr().expect("address").port();
    let connections = Arc::new(AtomicUsize::new(0));
    let open = Arc::new(AtomicUsize::new(0));
    let (counted, alive) = (connections.clone(), open.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            counted.fetch_add(1, Ordering::SeqCst);
            alive.fetch_add(1, Ordering::SeqCst);
            let alive = alive.clone();
            tokio::spawn(async move {
                serve_cam(&mut socket).await;
                alive.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    FakeCam {
        port,
        connections,
        open,
    }
}

async fn serve_cam(socket: &mut TcpStream) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match socket.read(&mut byte).await {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let boundary = "123456789";
    let start = format!("HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace;boundary={boundary}\r\n\r\n--{boundary}\r\n");
    if socket.write_all(start.as_bytes()).await.is_err() {
        return;
    }
    for number in 1u32..=u32::MAX {
        let jpeg = [
            &b"\xff\xd8"[..],
            format!("{number:04}").as_bytes(),
            &b"\xff\xd9"[..],
        ]
        .concat();
        let mut frame = format!(
            "Content-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
            jpeg.len()
        )
        .into_bytes();
        frame.extend(jpeg);
        frame.extend(format!("\r\n--{boundary}\r\n").into_bytes());
        if socket.write_all(&frame).await.is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The panel on a real socket at 127.0.0.1, so a stream body can be read as it arrives.
async fn serve_panel(panel: Arc<Panel>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the panel");
    let addr = listener.local_addr().expect("address");
    let app = routes::router(panel, Channel::Lan);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

async fn open_stream(addr: SocketAddr, path: &str, cookie: &str) -> TcpStream {
    let mut socket = TcpStream::connect(addr)
        .await
        .expect("connect to the panel");
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: tallow_panel={cookie}\r\n\r\n");
    socket
        .write_all(request.as_bytes())
        .await
        .expect("send the request");
    socket
}

fn count_jpegs(bytes: &[u8]) -> usize {
    bytes
        .windows(2)
        .filter(|pair| *pair == [0xff, 0xd9])
        .count()
}

/// Reads the response until it holds `frames` whole JPEGs. Returns the head and the body read so far.
async fn read_frames(socket: &mut TcpStream, frames: usize) -> Vec<u8> {
    let mut seen = Vec::new();
    let mut buffer = [0u8; 4096];
    let reading = async {
        while count_jpegs(&seen) < frames {
            let read = socket.read(&mut buffer).await.expect("read the stream");
            assert!(read > 0, "the stream ended before {frames} frames");
            seen.extend_from_slice(&buffer[..read]);
        }
    };
    tokio::time::timeout(Duration::from_secs(5), reading)
        .await
        .expect("the frames arrive in time");
    seen
}

/// Waits until the hub counts `expected` viewers, or fails after three seconds.
async fn wait_for_viewers(panel: &Panel, cam: &Cam, expected: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while panel.hubs.watching(cam) != expected {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the hub never counted {expected} viewers"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn status_asks_the_node_and_never_the_camera_port_of_a_streaming_cam() {
    let fake = fake_cam().await;
    let dead = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let dead_port = dead.local_addr().expect("address").port();
    drop(dead);
    let up = Cam::new("up", "Up", "127.0.0.1").with_ports(fake.port, fake.port);
    let down = Cam::new("down", "Down", "127.0.0.1").with_ports(dead_port, dead_port);
    let dir = TempDir::new("pages-status");
    let panel = panel_in(&dir, vec![up.clone(), down], None, true);
    let cookie = browser();

    let (status, _, body) = send(
        &panel,
        "GET",
        "/api/cams/status",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        json_body(&body),
        json!({"up": {"online": true, "viewers": 0}, "down": {"online": false, "viewers": 0}})
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(fake.open.load(Ordering::SeqCst), 0, "the probe hangs up");

    let addr = serve_panel(panel.clone()).await;
    let mut viewer = open_stream(addr, "/cams/up/stream", &cookie).await;
    read_frames(&mut viewer, 1).await;
    let before = fake.connections.load(Ordering::SeqCst);
    let (_, _, body) = send(
        &panel,
        "GET",
        "/api/cams/status",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(
        json_body(&body)["up"],
        json!({"online": true, "viewers": 1})
    );
    assert_eq!(
        fake.connections.load(Ordering::SeqCst),
        before,
        "a streaming cam is not dialled again"
    );
    drop(viewer);
    wait_for_viewers(&panel, &up, 0).await;
}

#[tokio::test]
async fn a_viewer_gets_whole_frames_and_leaves_the_hub_when_it_goes() {
    let fake = fake_cam().await;
    let cam = Cam::new("den", "Den", "127.0.0.1").with_ports(fake.port, fake.port);
    let dir = TempDir::new("stream");
    let panel = panel_in(&dir, vec![cam.clone()], None, true);
    let addr = serve_panel(panel.clone()).await;
    let mut viewer = open_stream(addr, "/cams/den/stream", &browser()).await;
    let seen = read_frames(&mut viewer, 2).await;
    let head = text(&seen).to_ascii_lowercase();
    assert!(
        head.contains("content-type: multipart/x-mixed-replace; boundary=tallow\r\n"),
        "{head}"
    );
    assert!(head.contains("cache-control: no-store\r\n"), "{head}");
    assert!(
        head.contains("x-content-type-options: nosniff\r\n"),
        "{head}"
    );
    let body = &seen[seen
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a head")
        + 4..];
    assert!(
        text(body).contains("Content-Type: image/jpeg"),
        "a whole part is sent"
    );
    wait_for_viewers(&panel, &cam, 1).await;
    drop(viewer);
    wait_for_viewers(&panel, &cam, 0).await;
}

// TokenDoorTest

#[test]
fn the_token_file_is_mode_0600_and_the_lanes_do_not_cross() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("token-door");
    let path = dir.path().join("agents.json");
    write_agents(&path, &[("hermes", HERMES, ALL)]);
    let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let stored = load_agents(&path).expect("a readable agents file");
    let browser = browser();
    assert_eq!(
        authenticate(
            Channel::Lan,
            Some(format!("Bearer {HERMES}").as_bytes()),
            Some(browser.as_bytes()),
            &browser,
            &stored
        ),
        None
    );
    assert_eq!(
        authenticate(
            Channel::Agent,
            None,
            Some(browser.as_bytes()),
            &browser,
            &stored
        ),
        None
    );
    let actor = authenticate(
        Channel::Agent,
        Some(format!("Bearer {HERMES}").as_bytes()),
        None,
        &browser,
        &stored,
    )
    .expect("the bearer is an agent");
    assert_eq!(actor.actor, "hermes");
    assert_eq!(
        actor.scopes,
        ALL.iter()
            .map(|scope| (*scope).to_owned())
            .collect::<Vec<_>>()
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let refused = load_agents(&path).expect_err("a loose file is refused");
    assert_eq!(refused.class, "PermissionError");
}

#[tokio::test]
async fn http_rejects_a_bearer_on_the_lan_and_a_cookie_on_the_socket() {
    let dir = TempDir::new("token-http");
    let agents = dir.path().join("agents.json");
    write_agents(
        &agents,
        &[("hermes", HERMES, ALL), ("reader", READER, READ_ONLY)],
    );
    let panel = panel_in(&dir, Vec::new(), None, true);
    let cookie = browser();

    let (status, _, _) = send(
        &panel,
        "GET",
        "/api/state",
        Channel::Lan,
        Some(HERMES),
        None,
        b"",
    )
    .await;
    assert_eq!(status, 401, "a bearer alone on the LAN");
    let (status, _, _) = send(
        &panel,
        "GET",
        "/api/state",
        Channel::Agent,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(status, 401, "a cookie on the socket");
    let (status, _, body) = send(
        &panel,
        "GET",
        "/api/state",
        Channel::Agent,
        Some(HERMES),
        None,
        b"",
    )
    .await;
    assert_eq!(status, 200);
    assert!(text(&body).contains(r#""device":"h7020""#));
    let (status, _, _) = send(
        &panel,
        "GET",
        "/api/manifest",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(status, 404);
    let (status, _, body) = send(
        &panel,
        "GET",
        "/api/manifest",
        Channel::Agent,
        Some(HERMES),
        None,
        b"",
    )
    .await;
    assert_eq!(status, 200);
    let manifest = text(&body);
    assert!(manifest.contains("Sunset Glow"));
    assert!(manifest.contains("raw_frames"));
    assert!(!manifest.contains(HERMES));

    let saved =
        json!({"id": "walk", "name": "Walk", "kind": "scene", "scene": {"name": "Sunset Glow"}});
    let (status, _, body) = send(
        &panel,
        "POST",
        "/api/patterns",
        Channel::Agent,
        Some(HERMES),
        None,
        saved.to_string().as_bytes(),
    )
    .await;
    assert_eq!(status, 200, "{}", text(&body));
    let schedule =
        json!({"id": "evening", "days": ["wed"], "time": "19:30", "action": {"pattern": "walk"}});
    let (status, _, body) = send(
        &panel,
        "POST",
        "/api/schedules",
        Channel::Agent,
        Some(HERMES),
        None,
        schedule.to_string().as_bytes(),
    )
    .await;
    assert_eq!(status, 200, "{}", text(&body));

    let (status, _, body) = send(
        &panel,
        "POST",
        "/api/scene",
        Channel::Agent,
        Some(READER),
        None,
        b"{}",
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(body, br#"{"detail":"missing scope"}"#);

    let audit = std::fs::read_to_string(dir.path().join("data").join("audit.jsonl"))
        .expect("the audit log");
    assert!(audit.contains("hermes"));
    assert!(!audit.contains(HERMES));

    panel
        .limiter
        .lock()
        .expect("the limiter")
        .fill(std::time::Instant::now());
    let (status, _, body) = send(
        &panel,
        "POST",
        "/api/power",
        Channel::Lan,
        None,
        Some(&cookie),
        br#"{"on":false}"#,
    )
    .await;
    assert_eq!(status, 429);
    assert_eq!(body, br#"{"detail":"slow down"}"#);
}

// LightAwayTest: the panel stays up when the light is not there. The retry belongs to T3.7.

#[tokio::test]
async fn the_panel_stays_up_when_the_light_does_not_answer() {
    let dir = TempDir::new("light-away");
    let panel = panel_in(&dir, Vec::new(), None, false);
    let cookie = browser();
    let (status, _, body) = send(
        &panel,
        "GET",
        "/api/state",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(status, 200);
    let state = json_body(&body);
    assert_eq!(state["connected"], false);
    assert_eq!(state["note"], PHONE_HOME);
    let (status, _, body) = send(
        &panel,
        "GET",
        "/lights",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(status, 200);
    assert!(text(&body).contains("phones home"));
    let (status, _, body) = send(
        &panel,
        "POST",
        "/api/power",
        Channel::Lan,
        None,
        Some(&cookie),
        br#"{"on":true}"#,
    )
    .await;
    assert_eq!(status, 503);
    assert_eq!(body, br#"{"detail":"disconnected"}"#);
    let (status, _, _) = send(
        &panel,
        "POST",
        "/api/scene",
        Channel::Lan,
        None,
        Some(&cookie),
        br#"{"name":"Sunset Glow"}"#,
    )
    .await;
    assert_eq!(status, 503);
}

// Routing and login: the contract's cases, in process.

#[tokio::test]
async fn unknown_paths_and_wrong_methods_answer_as_fastapi_does() {
    let dir = TempDir::new("routing");
    let panel = panel_in(&dir, Vec::new(), None, true);
    let cookie = browser();
    let (status, _, body) = send(
        &panel,
        "GET",
        "/no-such-page",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(body, br#"{"detail":"Not Found"}"#);
    let (status, _, body) = send(
        &panel,
        "DELETE",
        "/api/state",
        Channel::Lan,
        None,
        Some(&cookie),
        b"",
    )
    .await;
    assert_eq!(status, 405);
    assert_eq!(body, br#"{"detail":"Method Not Allowed"}"#);
    let (status, _, _) = send(&panel, "HEAD", "/panel.css", Channel::Lan, None, None, b"").await;
    assert_eq!(status, 405);
    let (status, headers, _) = send(&panel, "GET", "/lights/", Channel::Lan, None, None, b"").await;
    assert_eq!(status, 307);
    assert!(headers
        .get(header::LOCATION)
        .is_some_and(|v| v.as_bytes().ends_with(b"/lights")));
}

#[tokio::test]
async fn the_css_is_served_as_css_without_a_login() {
    let dir = TempDir::new("css");
    let panel = panel_in(&dir, Vec::new(), None, true);
    let (status, headers, body) =
        send(&panel, "GET", "/panel.css", Channel::Lan, None, None, b"").await;
    assert_eq!(status, 200);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).map(|v| v.as_bytes()),
        Some(&b"text/css; charset=utf-8"[..])
    );
    assert_eq!(
        headers.get(header::CACHE_CONTROL).map(|v| v.as_bytes()),
        Some(&b"no-cache"[..])
    );
    assert!(text(&body).contains("--bg: #14120f;"));
}

#[tokio::test]
async fn login_checks_the_password_and_sets_the_same_cookie_as_python() {
    let dir = TempDir::new("login");
    let panel = panel_in(&dir, Vec::new(), None, true);
    let (status, headers, _) = send(
        &panel,
        "POST",
        "/login",
        Channel::Lan,
        None,
        None,
        b"password=wrong",
    )
    .await;
    assert_eq!(status, 303);
    assert_eq!(
        headers.get(header::LOCATION).map(|v| v.as_bytes()),
        Some(&b"/login?error=1"[..])
    );
    assert!(headers.get(header::SET_COOKIE).is_none());

    let (status, _, body) = send(&panel, "POST", "/login", Channel::Lan, None, None, b"").await;
    assert_eq!(status, 422);
    let detail = json_body(&body);
    assert_eq!(detail["detail"][0]["loc"], json!(["body", "password"]));

    let (status, headers, _) = send(
        &panel,
        "POST",
        "/login",
        Channel::Lan,
        None,
        None,
        b"password=secret",
    )
    .await;
    assert_eq!(status, 303);
    assert_eq!(
        headers.get(header::LOCATION).map(|v| v.as_bytes()),
        Some(&b"/"[..])
    );
    let cookie = headers
        .get(header::SET_COOKIE)
        .expect("a cookie")
        .to_str()
        .expect("ASCII");
    assert_eq!(
        cookie,
        format!(
            "tallow_panel={}; Max-Age=2592000; Path=/; HttpOnly; SameSite=Strict",
            browser()
        )
    );
}

#[tokio::test]
async fn a_body_is_read_as_json_whatever_the_content_type_says() {
    let dir = TempDir::new("body");
    let panel = panel_in(&dir, Vec::new(), None, true);
    let cookie = browser();
    let (status, _, body) = send(
        &panel,
        "POST",
        "/api/power",
        Channel::Lan,
        None,
        Some(&cookie),
        b"not-json",
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body, br#"{"detail":"body must be JSON"}"#);
    let (status, _, body) = send(
        &panel,
        "POST",
        "/api/power",
        Channel::Lan,
        None,
        Some(&cookie),
        b"[]",
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body, br#"{"detail":"body must be an object"}"#);
    let (status, _, body) = send(
        &panel,
        "POST",
        "/api/power",
        Channel::Lan,
        None,
        Some(&cookie),
        b"{\"on\":\"\xff\"}",
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body, br#"{"detail":"body must be JSON"}"#);
    let (status, _, body) = send(
        &panel,
        "POST",
        "/api/power",
        Channel::Lan,
        None,
        Some(&cookie),
        br#"{"on":false}"#,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json_body(&body)["state"]["mode"], "off");
}

#[tokio::test]
async fn a_scene_speed_must_be_a_whole_number_and_a_refusal_changes_nothing() {
    let dir = TempDir::new("scene");
    let panel = panel_in(&dir, Vec::new(), None, true);
    let cookie = browser();
    let before = panel.light.snapshot();
    let (status, _, body) = send(
        &panel,
        "POST",
        "/api/scene",
        Channel::Lan,
        None,
        Some(&cookie),
        br#"{"name":"Sunset Glow","speed":"fast"}"#,
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body, br#"{"detail":"scene speed must be an integer"}"#);
    let (status, _, body) = send(
        &panel,
        "POST",
        "/api/scene",
        Channel::Lan,
        None,
        Some(&cookie),
        br#"{"name":"Sunset Glow","speed":99}"#,
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body, br#"{"detail":"scene speed index 99 outside 0..2"}"#);
    assert_eq!(panel.light.snapshot(), before);
}

// Helpers the port depends on: the cookie reader, constant-time compare, and the note on the page.

#[test]
fn a_secret_compares_every_byte_and_its_length() {
    assert!(secret_eq(b"abc", b"abc"));
    assert!(secret_eq(b"", b""));
    assert!(!secret_eq(b"abc", b"abd"));
    assert!(!secret_eq(b"abc", b"ab"));
    assert!(!secret_eq(b"ab", b"abc"));
    assert!(!secret_eq(b"", b"a"));
}

#[test]
fn the_cookie_is_read_the_way_starlette_reads_it() {
    assert_eq!(
        cookie_value(b"a=1; tallow_panel=xyz", "tallow_panel"),
        Some(&b"xyz"[..])
    );
    assert_eq!(
        cookie_value(b"tallow_panel=\"q\"", "tallow_panel"),
        Some(&b"q"[..])
    );
    assert_eq!(
        cookie_value(b"tallow_panel=first; tallow_panel=last", "tallow_panel"),
        Some(&b"last"[..])
    );
    assert_eq!(cookie_value(b"other=1", "tallow_panel"), None);
    assert_eq!(
        cookie_value(b"tallow_panel=", "tallow_panel"),
        Some(&b""[..])
    );
}

// Startup refusals. Each one is decided before any file is read.

#[test]
fn the_flags_that_need_preview_refuse_without_it_with_the_python_words() {
    let password = super::setup::Args {
        password_file: Some(PathBuf::from("/nonexistent/tallowd-test-password")),
        ..Default::default()
    };
    assert_eq!(
        super::start(password).expect_err("refused"),
        "--password-file needs --preview. The real panel reads secrets/panel/password."
    );
    let data = super::setup::Args {
        data_dir: Some(PathBuf::from("/nonexistent/tallowd-test-data")),
        ..Default::default()
    };
    assert_eq!(
        super::start(data).expect_err("refused"),
        "--data-dir and --agents-file need --preview. The real panel uses data/panel and secrets/panel."
    );
}

#[test]
fn a_preview_without_a_password_file_refuses_and_reads_no_secret() {
    let args = super::setup::Args {
        preview: true,
        ..Default::default()
    };
    let message = super::start(args).expect_err("refused");
    assert!(
        message.starts_with("--preview needs --password-file."),
        "{message}"
    );
}
