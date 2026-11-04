//! The panel's HTTP routes. Port of the route functions in `app/h7020_panel.py`.
//!
//! Every handler keeps the Python's check order: login, then scope, then the rate
//! limit, then the link, then the body. A body is read only after those pass.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, on, post, MethodFilter, MethodRouter};
use axum::Router;
use chrono::Local;
use serde_json::{json, Map, Value};

use tallow_govee::scene;

use super::access::{
    authenticate, cookie_value, secret_eq, session_token, Agent, Channel, Principal, COOKIE, SCOPES,
};
use super::mesh_map;
use super::pages;
use super::Panel;
use crate::cams::{self, Cam, Feed};
use crate::library::{self, Error, Group};
use crate::traffic;

/// Largest request body the panel reads: 2 MiB. This is a deliberate deviation. The
/// Python has no limit. The cap keeps a LAN client from filling memory.
const BODY_MAX: usize = 2 * 1024 * 1024;
/// Control calls a minute for the one radio.
const LIMIT: usize = 30;
const LIMIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// What a route answers with when it refuses. The body is `{"detail": ...}`, as FastAPI sends.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    detail: String,
}

impl ApiError {
    pub fn new(status: u16, detail: &str) -> Self {
        Self {
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            detail: detail.to_owned(),
        }
    }
}

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        match error {
            Error::Invalid(message) => ApiError::new(400, &message),
            Error::Exists(_) => ApiError::new(409, "already saved"),
            Error::Missing(_) => ApiError::new(404, "not found"),
            Error::Device(_) => ApiError::new(503, "disconnected"),
            Error::Corrupt(_) | Error::Io(_) => ApiError::new(500, "Internal Server Error"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        json_response(self.status, &json!({"detail": self.detail}))
    }
}

/// A JSON response, with the bytes compact as FastAPI writes them.
pub fn json_response(status: StatusCode, value: &Value) -> Response {
    let bytes = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn channel(req: &Request) -> Channel {
    req.extensions()
        .get::<Channel>()
        .copied()
        .unwrap_or(Channel::Lan)
}

fn authorization(req: &Request) -> Option<&[u8]> {
    req.headers()
        .get(header::AUTHORIZATION)
        .map(HeaderValue::as_bytes)
}

fn cookie(req: &Request) -> Option<&[u8]> {
    req.headers()
        .get(header::COOKIE)
        .and_then(|value| cookie_value(value.as_bytes(), COOKIE))
}

impl Panel {
    /// The agents file, read now. A file that cannot be used counts as no agents, and
    /// its problem is logged once, as the Python does.
    fn current_agents(&self) -> Vec<Agent> {
        match super::access::load_agents(&self.agents_path) {
            Ok(agents) => agents,
            Err(error) => {
                let note = error.to_string();
                let mut seen = self
                    .agents_note
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if seen.as_deref() != Some(note.as_str()) {
                    eprintln!("Agent tokens unreadable ({note}).");
                    *seen = Some(note);
                }
                Vec::new()
            }
        }
    }

    fn cams(&self) -> Vec<Cam> {
        self.book
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .all()
    }

    fn title(&self) -> String {
        self.book
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .title()
    }

    fn bulbs(&self) -> i64 {
        self.light.bulb_count()
    }
}

/// The house LAN page check: the browser cookie only, and no bearer token.
fn lan_page(panel: &Panel, req: &Request) -> bool {
    authenticate(
        channel(req),
        authorization(req),
        cookie(req),
        &panel.browser,
        &[],
    )
    .is_some()
}

/// The menu, the pages, and the cams belong to the house LAN. The socket gets a 404.
fn lan_only(req: &Request) -> Result<(), ApiError> {
    if channel(req) == Channel::Agent {
        return Err(ApiError::new(404, "not found"));
    }
    Ok(())
}

/// A LAN route that needs the browser cookie: 404 on the socket, 401 without a login.
fn lan_viewer(panel: &Panel, req: &Request) -> Result<(), ApiError> {
    lan_only(req)?;
    if !lan_page(panel, req) {
        return Err(ApiError::new(401, "login"));
    }
    Ok(())
}

/// Login, then scope. A bearer on the socket is audited with its name and route.
fn require(panel: &Panel, req: &Request, scope: &str) -> Result<String, ApiError> {
    let chan = channel(req);
    let agents = if chan == Channel::Agent {
        panel.current_agents()
    } else {
        Vec::new()
    };
    let principal: Principal = authenticate(
        chan,
        authorization(req),
        cookie(req),
        &panel.browser,
        &agents,
    )
    .ok_or_else(|| ApiError::new(401, "login"))?;
    if !principal.scopes.iter().any(|granted| granted == scope) {
        return Err(ApiError::new(403, "missing scope"));
    }
    if chan == Channel::Agent {
        panel
            .catalog
            .audit(
                &principal.actor,
                req.method().as_str(),
                req.uri().path(),
                &Local::now(),
            )
            .map_err(|_| ApiError::new(500, "Internal Server Error"))?;
    }
    Ok(principal.actor)
}

/// A control call: login and scope, then the rate limit, then the link.
fn control(panel: &Panel, req: &Request) -> Result<(), ApiError> {
    require(panel, req, "control")?;
    let allowed = panel
        .limiter
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .allow(std::time::Instant::now());
    if !allowed {
        return Err(ApiError::new(429, "slow down"));
    }
    if !panel.light.linked() {
        return Err(ApiError::new(503, "disconnected"));
    }
    Ok(())
}

/// The body as a JSON object, read whatever the Content-Type says.
async fn read_body(req: Request) -> Result<Map<String, Value>, ApiError> {
    let bytes = to_bytes(req.into_body(), BODY_MAX)
        .await
        .map_err(|_| ApiError::new(413, "body too large"))?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| ApiError::new(400, "body must be JSON"))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(ApiError::new(400, "body must be an object")),
    }
}

fn only_fields(body: &Map<String, Value>, allowed: &[&str]) -> Result<(), ApiError> {
    let mut extra: Vec<&str> = body
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    if extra.is_empty() {
        return Ok(());
    }
    extra.sort_unstable();
    Err(ApiError::new(
        400,
        &format!("unknown fields: {}", extra.join(", ")),
    ))
}

/// A whole number from 0 to 255, or the first bad value's reason.
fn channel_value(value: &Value) -> Option<i64> {
    value.as_i64().filter(|number| (0..=255).contains(number))
}

fn scene_list() -> Result<Vec<Value>, ApiError> {
    let catalogue = scene::catalogue().map_err(|_| ApiError::new(500, "Internal Server Error"))?;
    Ok(catalogue
        .iter()
        .map(|item| {
            json!({
                "name": item.name,
                "category": item.category,
                "speeds": item.speed.as_ref().map_or(0, |speed| speed.option_count),
                "default_speed": item.speed.as_ref().map(|speed| speed.default_index),
            })
        })
        .collect())
}

fn acted(snapshot: Result<Value, Error>) -> Result<Response, ApiError> {
    let state = snapshot?;
    Ok(json_response(StatusCode::OK, &json!({"state": state})))
}

fn save_pattern(panel: &Panel, body: Map<String, Value>, create: bool) -> Result<Value, ApiError> {
    let doc = Value::Object(body);
    let pattern = library::validate_pattern(&doc, panel.bulbs())?;
    panel.catalog.save_pattern(&pattern, create)?;
    Ok(serde_json::to_value(&pattern).unwrap_or(Value::Null))
}

fn save_schedule(panel: &Panel, body: Map<String, Value>, create: bool) -> Result<Value, ApiError> {
    let doc = Value::Object(body);
    let patterns = panel.catalog.list_patterns()?;
    let schedule = library::validate_schedule(&doc, &patterns)?;
    panel.catalog.save_schedule(&schedule, create)?;
    Ok(serde_json::to_value(&schedule).unwrap_or(Value::Null))
}

fn pattern_list(panel: &Panel) -> Result<Vec<Value>, ApiError> {
    let items = panel.catalog.list_patterns()?;
    Ok(items
        .iter()
        .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
        .collect())
}

fn schedule_list(panel: &Panel) -> Result<Vec<Value>, ApiError> {
    let items = panel.catalog.list_schedules()?;
    Ok(items
        .iter()
        .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
        .collect())
}

fn camera_document(panel: &Panel) -> Value {
    let mut book = panel
        .book
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let title = book.title();
    let listed = book.all();
    let error = book.error.clone();
    json!({
        "title": title,
        "cams": listed.iter().map(cam_settings_json).collect::<Vec<_>>(),
        "error": error,
    })
}

/// Settings sees the address. A straight picture omits `rotation`, so an older file stays the same shape.
fn cam_settings_json(cam: &Cam) -> Value {
    let mut value = json!({"id": cam.id, "name": cam.name, "address": cam.address});
    if cam.rotation != 0 {
        value["rotation"] = json!(cam.rotation);
    }
    value
}

/// The cameras page sees the turn, and never the address.
fn cam_public_json(cam: &Cam) -> Value {
    let mut value = json!({"id": cam.id, "name": cam.name});
    if cam.rotation != 0 {
        value["rotation"] = json!(cam.rotation);
    }
    value
}

fn manifest(panel: &Panel) -> Result<Value, ApiError> {
    Ok(json!({
        "device": "h7020",
        "bulbs": panel.bulbs(),
        "absences": ["diy", "music", "kelvin", "raw_frames"],
        "scopes": SCOPES,
        "scenes": scene_list()?,
        "pattern": {
            "kinds": ["scene", "paint", "sequence"],
            "hold_min_seconds": 1,
            "steps_max": 32,
            "example_sequence": {
                "id": "evening-walk",
                "name": "Evening walk",
                "kind": "sequence",
                "repeat": true,
                "steps": [{"hold": 4, "paint": [{"bulbs": [1, 2, 3], "rgb": [255, 80, 0]}]}],
            },
            "example_scene": {
                "id": "sunset",
                "name": "Sunset",
                "kind": "scene",
                "scene": {"name": "Sunset Glow", "speed": 1},
            },
        },
        "schedule": {
            "days": ["mon", "tue", "wed", "thu", "fri", "sat", "sun"],
            "time": "HH:MM",
            "actions": ["pattern", "scene", "power_off"],
        },
        "routes": [
            "GET /api/state",
            "GET /api/scenes",
            "GET /api/capabilities",
            "GET /api/manifest",
            "POST /api/power",
            "POST /api/brightness",
            "POST /api/color",
            "POST /api/scene",
            "POST /api/paint",
            "GET /api/patterns",
            "POST /api/patterns",
            "GET /api/patterns/{id}",
            "PUT /api/patterns/{id}",
            "DELETE /api/patterns/{id}",
            "POST /api/patterns/{id}/apply",
            "GET /api/schedules",
            "POST /api/schedules",
            "GET /api/schedules/{id}",
            "PUT /api/schedules/{id}",
            "DELETE /api/schedules/{id}",
        ],
    }))
}

/// A GET-only route. axum answers HEAD with the GET handler unless a HEAD handler is
/// set, and FastAPI answers HEAD with 405. So HEAD gets the 405 here.
fn get_only<H, T>(handler: H) -> MethodRouter<Arc<Panel>>
where
    H: axum::handler::Handler<T, Arc<Panel>>,
    T: 'static,
{
    on(MethodFilter::GET, handler).on(MethodFilter::HEAD, method_not_allowed)
}

fn redirect_with_cookie(location: &str, cookie: Option<String>) -> Response {
    let mut response = Redirect::to(location).into_response();
    if let Some(value) = cookie.and_then(|value| HeaderValue::from_str(&value).ok()) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

async fn index(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    if !lan_page(&panel, &req) {
        return Html(pages::login_page("")).into_response();
    }
    Html(pages::menu_page(&panel.title())).into_response()
}

/// A LAN page: 404 on the socket, 303 to login without a cookie, and the page otherwise.
fn page(panel: &Panel, req: &Request, body: impl FnOnce() -> String) -> Response {
    if let Err(error) = lan_only(req) {
        return error.into_response();
    }
    if !lan_page(panel, req) {
        return Redirect::to("/login").into_response();
    }
    Html(body()).into_response()
}

async fn lights_page(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    page(&panel, &req, || pages::controls_page().to_owned())
}

async fn cams_page(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    page(&panel, &req, || pages::cams_page(&panel.title()))
}

async fn settings_page(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    page(&panel, &req, || pages::settings_page(&panel.title()))
}

async fn devices_page(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    page(&panel, &req, || pages::devices_page().to_owned())
}

async fn traffic_page(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    page(&panel, &req, || pages::traffic_page().to_owned())
}

fn traffic_db(panel: &Panel) -> Option<std::path::PathBuf> {
    panel
        .map_path
        .as_ref()
        .map(|path| path.with_file_name("traffic.db"))
}

fn traffic_bounds(req: &Request) -> Result<(i64, i64), ApiError> {
    let query: HashMap<String, String> = req
        .uri()
        .query()
        .and_then(|query| serde_urlencoded::from_str(query).ok())
        .unwrap_or_default();
    let name = query.get("window").map(String::as_str).unwrap_or("live");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    traffic::window(now, name)
        .ok_or_else(|| ApiError::new(400, "window must be live, hour, or day"))
}

async fn traffic_route(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    if let Err(error) = lan_viewer(&panel, &req) {
        return error.into_response();
    }
    let (from, to) = match traffic_bounds(&req) {
        Ok(bounds) => bounds,
        Err(error) => return error.into_response(),
    };
    let db = traffic_db(&panel);
    match tokio::task::spawn_blocking(move || traffic::picture(db.as_deref(), from, to)).await {
        Ok(Ok(document)) => json_response(StatusCode::OK, &document),
        Ok(Err(_)) | Err(_) => ApiError::new(500, "Internal Server Error").into_response(),
    }
}

async fn traffic_layout(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    if let Err(error) = lan_viewer(&panel, &req) {
        return error.into_response();
    }
    let Some(db) = traffic_db(&panel) else {
        return ApiError::new(400, "traffic is not configured").into_response();
    };
    let body = match read_body(req).await {
        Ok(body) => body,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = only_fields(&body, &["nodes"]) {
        return error.into_response();
    }
    let path = db.with_file_name("layout.json");
    match tokio::task::spawn_blocking(move || traffic::store_layout(&path, &body)).await {
        Ok(Ok(())) => json_response(StatusCode::OK, &json!({"ok": true})),
        Ok(Err(message)) => ApiError::new(400, &message).into_response(),
        Err(_) => ApiError::new(500, "Internal Server Error").into_response(),
    }
}

async fn traffic_clear(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    if let Err(error) = lan_viewer(&panel, &req) {
        return error.into_response();
    }
    let Some(db) = traffic_db(&panel) else {
        return ApiError::new(400, "traffic is not configured").into_response();
    };
    match tokio::task::spawn_blocking(move || traffic::clear(&db)).await {
        Ok(Ok(())) => json_response(StatusCode::OK, &json!({"ok": true})),
        Ok(Err(_)) | Err(_) => ApiError::new(500, "Internal Server Error").into_response(),
    }
}

async fn timeline_js(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    if let Err(error) = lan_viewer(&panel, &req) {
        return error.into_response();
    }
    asset_ok(
        pages::timeline_js(),
        "application/javascript; charset=utf-8",
    )
}

async fn timeline_css(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    if let Err(error) = lan_viewer(&panel, &req) {
        return error.into_response();
    }
    asset_ok(pages::timeline_css(), "text/css; charset=utf-8")
}

fn asset_ok(body: &'static str, content_type: &'static str) -> Response {
    let mut response = Response::new(Body::from(body));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// The mesh map, read from the file the panel was started with. Read only: the request
/// supplies no part of the path, and a file that cannot be used is a stale document, not an error.
async fn mesh_map_route(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    if let Err(error) = lan_viewer(&panel, &req) {
        return error.into_response();
    }
    let path = panel.map_path.clone();
    let now = SystemTime::now();
    let mut document =
        match tokio::task::spawn_blocking(move || mesh_map::document(path.as_deref(), now)).await {
            Ok(document) => document,
            Err(_) => return ApiError::new(500, "Internal Server Error").into_response(),
        };
    if let Some(host) = &panel.mesh_host {
        document["host"] = host.document();
    }
    json_response(StatusCode::OK, &document)
}

async fn panel_css(State(_panel): State<Arc<Panel>>, req: Request) -> Response {
    if let Err(error) = lan_only(&req) {
        return error.into_response();
    }
    let mut response = Response::new(Body::from(pages::panel_css()));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/css; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

async fn lights_redirect() -> Redirect {
    Redirect::temporary("/lights")
}

async fn cam_index(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    match lan_viewer(&panel, &req) {
        Err(error) => error.into_response(),
        Ok(()) => {
            let (listed, error) = {
                let mut book = panel
                    .book
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                let listed = book.all();
                (listed, book.error.clone())
            };
            panel.hubs.prune(&listed);
            let cams: Vec<Value> = listed.iter().map(cam_public_json).collect();
            json_response(StatusCode::OK, &json!({"cams": cams, "error": error}))
        }
    }
}

async fn cam_status(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    if let Err(error) = lan_viewer(&panel, &req) {
        return error.into_response();
    }
    let mut set = tokio::task::JoinSet::new();
    for cam in panel.cams() {
        // A cam that is streaming holds its one socket. Ask it nothing.
        let viewing = panel.hubs.watching(&cam);
        set.spawn(async move {
            let online = viewing > 0 || cams::reachable(&cam).await;
            (cam.id, json!({"online": online, "viewers": viewing}))
        });
    }
    let mut out = Map::new();
    while let Some(joined) = set.join_next().await {
        if let Ok((id, status)) = joined {
            out.insert(id, status);
        }
    }
    json_response(StatusCode::OK, &Value::Object(out))
}

async fn cam_stream(
    State(panel): State<Arc<Panel>>,
    Path(cam_id): Path<String>,
    req: Request,
) -> Response {
    if let Err(error) = lan_viewer(&panel, &req) {
        return error.into_response();
    }
    let found = panel
        .book
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&cam_id);
    let Some(cam) = found else {
        return ApiError::new(404, "no such cam").into_response();
    };
    let hub = panel.hubs.hub(&cam);
    if hub.viewers_full() {
        return ApiError::new(429, "cam busy").into_response();
    }
    match hub.join().await {
        Ok(feed) => stream_response(feed),
        Err(error) => {
            eprintln!("Cam did not answer ({error}).");
            ApiError::new(502, "The cam is not answering.").into_response()
        }
    }
}

async fn settings_cameras(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    if let Err(error) = lan_viewer(&panel, &req) {
        return error.into_response();
    }
    json_response(StatusCode::OK, &camera_document(&panel))
}

async fn save_settings_cameras(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    if let Err(error) = lan_viewer(&panel, &req) {
        return error.into_response();
    }
    let Some(path) = panel.cams_path.clone() else {
        return ApiError::new(400, "cameras are not stored in a file").into_response();
    };
    let result = async {
        let body = read_body(req).await?;
        cams::save_inventory(&path, &Value::Object(body)).map_err(|error| match error {
            cams::InventoryError::Invalid(message) => ApiError::new(400, &message),
            cams::InventoryError::Io(_) => ApiError::new(500, "Internal Server Error"),
        })?;
        Ok::<(), ApiError>(())
    }
    .await;
    if let Err(error) = result {
        return error.into_response();
    }
    panel
        .book
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .forget();
    json_response(StatusCode::OK, &camera_document(&panel))
}

async fn login_form(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    if lan_page(&panel, &req) {
        return Redirect::to("/").into_response();
    }
    let query: HashMap<String, String> = req
        .uri()
        .query()
        .and_then(|query| serde_urlencoded::from_str(query).ok())
        .unwrap_or_default();
    let error = if query.get("error").is_some_and(|value| !value.is_empty()) {
        "Wrong password."
    } else {
        ""
    };
    Html(pages::login_page(error)).into_response()
}

async fn login(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    if channel(&req) == Channel::Agent {
        return ApiError::new(404, "not found").into_response();
    }
    let bytes = match to_bytes(req.into_body(), BODY_MAX).await {
        Ok(bytes) => bytes,
        Err(_) => return ApiError::new(413, "body too large").into_response(),
    };
    let fields: Vec<(String, String)> = serde_urlencoded::from_bytes(&bytes).unwrap_or_default();
    let given = fields
        .iter()
        .rev()
        .find(|(key, _)| key == "password")
        .map(|(_, value)| value.clone());
    let Some(given) = given else {
        return json_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            &json!({"detail": [{
                "type": "missing",
                "loc": ["body", "password"],
                "msg": "Field required",
                "input": null,
            }]}),
        );
    };
    if !secret_eq(given.as_bytes(), panel.password.as_bytes()) {
        return Redirect::to("/login?error=1").into_response();
    }
    let value = format!(
        "{COOKIE}={}; Max-Age=2592000; Path=/; HttpOnly; SameSite=Strict",
        session_token(&panel.password)
    );
    redirect_with_cookie("/", Some(value))
}

async fn logout(State(_panel): State<Arc<Panel>>, req: Request) -> Response {
    if let Err(error) = lan_only(&req) {
        return error.into_response();
    }
    redirect_with_cookie(
        "/login",
        Some(format!(
            "{COOKIE}=\"\"; expires=Thu, 01 Jan 1970 00:00:00 GMT; Max-Age=0; Path=/; SameSite=lax"
        )),
    )
}

async fn state(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    match require(&panel, &req, "read") {
        Err(error) => error.into_response(),
        Ok(_) => json_response(StatusCode::OK, &panel.light.snapshot()),
    }
}

async fn scenes(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    let result = require(&panel, &req, "read").and_then(|_| scene_list());
    match result {
        Ok(list) => json_response(StatusCode::OK, &json!({"scenes": list})),
        Err(error) => error.into_response(),
    }
}

async fn capabilities(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    match require(&panel, &req, "read") {
        Err(error) => error.into_response(),
        Ok(_) => json_response(
            StatusCode::OK,
            &json!({
                "device": "h7020",
                "bulbs": panel.bulbs(),
                "diy": false,
                "music": false,
                "kelvin": false,
                "raw_frames": false,
            }),
        ),
    }
}

async fn manifest_route(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    if channel(&req) != Channel::Agent {
        return ApiError::new(404, "not found").into_response();
    }
    let result = require(&panel, &req, "read").and_then(|_| manifest(&panel));
    match result {
        Ok(body) => json_response(StatusCode::OK, &body),
        Err(error) => error.into_response(),
    }
}

async fn power(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    let result = async {
        control(&panel, &req)?;
        let body = read_body(req).await?;
        only_fields(&body, &["on"])?;
        let Some(on) = body.get("on").and_then(Value::as_bool) else {
            return Err(ApiError::new(400, "on must be a boolean"));
        };
        acted(panel.light.power(on))
    }
    .await;
    result.unwrap_or_else(IntoResponse::into_response)
}

async fn brightness(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    let result = async {
        control(&panel, &req)?;
        let body = read_body(req).await?;
        only_fields(&body, &["level"])?;
        let level = body
            .get("level")
            .and_then(Value::as_i64)
            .filter(|level| (1..=100).contains(level))
            .ok_or_else(|| ApiError::new(400, "level must be an integer 1..100"))?;
        acted(panel.light.brightness(level))
    }
    .await;
    result.unwrap_or_else(IntoResponse::into_response)
}

async fn color(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    let result = async {
        control(&panel, &req)?;
        let body = read_body(req).await?;
        only_fields(&body, &["r", "g", "b"])?;
        let (Some(r), Some(g), Some(b)) = (body.get("r"), body.get("g"), body.get("b")) else {
            return Err(ApiError::new(400, "r, g, b required"));
        };
        let values = [r, g, b].map(channel_value);
        let [Some(r), Some(g), Some(b)] = values else {
            return Err(ApiError::new(400, "r, g, b must be integers 0..255"));
        };
        acted(panel.light.color([r, g, b]))
    }
    .await;
    result.unwrap_or_else(IntoResponse::into_response)
}

async fn scene_route(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    let result = async {
        control(&panel, &req)?;
        let body = read_body(req).await?;
        only_fields(&body, &["name", "speed"])?;
        let speed = match body.get("speed") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_i64()
                    .ok_or_else(|| ApiError::new(400, "scene speed must be an integer"))?,
            ),
        };
        let name = body.get("name").and_then(Value::as_str).unwrap_or("");
        acted(panel.light.scene(name, speed))
    }
    .await;
    result.unwrap_or_else(IntoResponse::into_response)
}

async fn paint(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    let result = async {
        control(&panel, &req)?;
        let body = read_body(req).await?;
        only_fields(&body, &["groups"])?;
        let groups: Vec<Group> =
            library::validate_paint(body.get("groups").unwrap_or(&Value::Null), panel.bulbs())?;
        acted(panel.light.paint(&groups))
    }
    .await;
    result.unwrap_or_else(IntoResponse::into_response)
}

async fn patterns(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    let result = require(&panel, &req, "read").and_then(|_| pattern_list(&panel));
    match result {
        Ok(list) => json_response(StatusCode::OK, &json!({"patterns": list})),
        Err(error) => error.into_response(),
    }
}

async fn create_pattern(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    let result = async {
        require(&panel, &req, "library")?;
        let body = read_body(req).await?;
        save_pattern(&panel, body, true)
    }
    .await;
    result.map_or_else(IntoResponse::into_response, |saved| {
        json_response(StatusCode::OK, &saved)
    })
}

async fn pattern_one(
    State(panel): State<Arc<Panel>>,
    Path(pattern_id): Path<String>,
    req: Request,
) -> Response {
    if let Err(error) = require(&panel, &req, "read") {
        return error.into_response();
    }
    match panel.catalog.get_pattern(&pattern_id) {
        Ok(Some(found)) => json_response(StatusCode::OK, &to_value(&found)),
        Ok(None) => ApiError::new(404, "not found").into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

async fn update_pattern(
    State(panel): State<Arc<Panel>>,
    Path(pattern_id): Path<String>,
    req: Request,
) -> Response {
    let result = async {
        require(&panel, &req, "library")?;
        let mut body = read_body(req).await?;
        body.insert("id".to_owned(), Value::String(pattern_id));
        save_pattern(&panel, body, false)
    }
    .await;
    result.map_or_else(IntoResponse::into_response, |saved| {
        json_response(StatusCode::OK, &saved)
    })
}

async fn delete_pattern(
    State(panel): State<Arc<Panel>>,
    Path(pattern_id): Path<String>,
    req: Request,
) -> Response {
    if let Err(error) = require(&panel, &req, "library") {
        return error.into_response();
    }
    match panel.catalog.delete_pattern(&pattern_id) {
        Ok(()) => json_response(StatusCode::OK, &json!({"ok": true})),
        Err(error) => ApiError::from(error).into_response(),
    }
}

async fn apply_pattern(
    State(panel): State<Arc<Panel>>,
    Path(pattern_id): Path<String>,
    req: Request,
) -> Response {
    let result = async move {
        control(&panel, &req)?;
        let found = panel
            .catalog
            .get_pattern(&pattern_id)?
            .ok_or_else(|| ApiError::new(404, "not found"))?;
        acted(panel.light.pattern(&found))
    }
    .await;
    result.unwrap_or_else(IntoResponse::into_response)
}

async fn schedules(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    let result = require(&panel, &req, "read").and_then(|_| schedule_list(&panel));
    match result {
        Ok(list) => json_response(StatusCode::OK, &json!({"schedules": list})),
        Err(error) => error.into_response(),
    }
}

async fn create_schedule(State(panel): State<Arc<Panel>>, req: Request) -> Response {
    let result = async {
        require(&panel, &req, "schedule")?;
        let body = read_body(req).await?;
        save_schedule(&panel, body, true)
    }
    .await;
    result.map_or_else(IntoResponse::into_response, |saved| {
        json_response(StatusCode::OK, &saved)
    })
}

async fn schedule_one(
    State(panel): State<Arc<Panel>>,
    Path(schedule_id): Path<String>,
    req: Request,
) -> Response {
    if let Err(error) = require(&panel, &req, "read") {
        return error.into_response();
    }
    match panel.catalog.get_schedule(&schedule_id) {
        Ok(Some(found)) => json_response(StatusCode::OK, &to_value(&found)),
        Ok(None) => ApiError::new(404, "not found").into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

async fn update_schedule(
    State(panel): State<Arc<Panel>>,
    Path(schedule_id): Path<String>,
    req: Request,
) -> Response {
    let result = async {
        require(&panel, &req, "schedule")?;
        let mut body = read_body(req).await?;
        body.insert("id".to_owned(), Value::String(schedule_id));
        save_schedule(&panel, body, false)
    }
    .await;
    result.map_or_else(IntoResponse::into_response, |saved| {
        json_response(StatusCode::OK, &saved)
    })
}

async fn delete_schedule(
    State(panel): State<Arc<Panel>>,
    Path(schedule_id): Path<String>,
    req: Request,
) -> Response {
    if let Err(error) = require(&panel, &req, "schedule") {
        return error.into_response();
    }
    match panel.catalog.delete_schedule(&schedule_id) {
        Ok(()) => json_response(StatusCode::OK, &json!({"ok": true})),
        Err(error) => ApiError::from(error).into_response(),
    }
}

fn to_value<T: serde::Serialize>(item: &T) -> Value {
    serde_json::to_value(item).unwrap_or(Value::Null)
}

async fn not_found() -> Response {
    json_response(StatusCode::NOT_FOUND, &json!({"detail": "Not Found"}))
}

async fn method_not_allowed() -> Response {
    json_response(
        StatusCode::METHOD_NOT_ALLOWED,
        &json!({"detail": "Method Not Allowed"}),
    )
}

/// Streams one viewer's copy of a cam as Python's `Feed` does: a multipart body of
/// `part()` frames, with the same headers. Dropping the body drops the feed, which
/// tells the hub the viewer has left.
fn stream_response(feed: Feed) -> Response {
    let parts = futures_util::stream::unfold(feed, |mut feed| async move {
        let frame = feed.next_frame().await?;
        let bytes = Bytes::from(cams::part(&frame));
        Some((Ok::<Bytes, Infallible>(bytes), feed))
    });
    let mut response = Response::new(Body::from_stream(parts));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("multipart/x-mixed-replace; boundary=tallow"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// Builds the router for one listener. The channel is set on every request, so one
/// route table serves both doors and each handler knows which one it came through.
pub fn router(panel: Arc<Panel>, chan: Channel) -> Router {
    Router::new()
        .route("/", get_only(index))
        .route("/lights", get_only(lights_page))
        .route("/lights/", get(lights_redirect))
        .route("/cams", get_only(cams_page))
        .route("/settings", get_only(settings_page))
        .route("/devices", get_only(devices_page))
        .route("/traffic", get_only(traffic_page))
        .route("/vendor/vis-timeline.js", get_only(timeline_js))
        .route("/vendor/vis-timeline.css", get_only(timeline_css))
        .route("/panel.css", get_only(panel_css))
        .route("/login", get_only(login_form).post(login))
        .route("/logout", post(logout))
        .route("/api/cams", get_only(cam_index))
        .route("/api/cams/status", get_only(cam_status))
        .route("/cams/{cam_id}/stream", get_only(cam_stream))
        .route(
            "/api/settings/cameras",
            get_only(settings_cameras).put(save_settings_cameras),
        )
        .route("/api/mesh/map", get_only(mesh_map_route))
        .route("/api/traffic", get_only(traffic_route))
        .route("/api/traffic/layout", post(traffic_layout))
        .route("/api/traffic/clear", post(traffic_clear))
        .route("/api/state", get_only(state))
        .route("/api/scenes", get_only(scenes))
        .route("/api/capabilities", get_only(capabilities))
        .route("/api/manifest", get_only(manifest_route))
        .route("/api/power", post(power))
        .route("/api/brightness", post(brightness))
        .route("/api/color", post(color))
        .route("/api/scene", post(scene_route))
        .route("/api/paint", post(paint))
        .route("/api/patterns", get_only(patterns).post(create_pattern))
        .route(
            "/api/patterns/{pattern_id}",
            get_only(pattern_one)
                .put(update_pattern)
                .delete(delete_pattern),
        )
        .route("/api/patterns/{pattern_id}/apply", post(apply_pattern))
        .route("/api/schedules", get_only(schedules).post(create_schedule))
        .route(
            "/api/schedules/{schedule_id}",
            get_only(schedule_one)
                .put(update_schedule)
                .delete(delete_schedule),
        )
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(panel)
        .layer(middleware::from_fn(
            move |mut req: Request, next: Next| async move {
                req.extensions_mut().insert(chan);
                next.run(req).await
            },
        ))
}

/// The fixed limit of control calls, and the time they are counted over.
#[derive(Debug, Default)]
pub struct WriteLimiter {
    hits: Vec<std::time::Instant>,
}

impl WriteLimiter {
    /// True when this call may go. A refused call is not counted.
    pub fn allow(&mut self, now: std::time::Instant) -> bool {
        self.hits
            .retain(|hit| now.duration_since(*hit) < LIMIT_WINDOW);
        if self.hits.len() >= LIMIT {
            return false;
        }
        self.hits.push(now);
        true
    }

    #[cfg(test)]
    pub fn fill(&mut self, now: std::time::Instant) {
        self.hits = vec![now; LIMIT];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limiter_allows_thirty_a_minute_and_counts_no_refusal() {
        let mut limiter = WriteLimiter::default();
        let start = std::time::Instant::now();
        for _ in 0..LIMIT {
            assert!(limiter.allow(start));
        }
        assert!(!limiter.allow(start));
        assert_eq!(limiter.hits.len(), LIMIT);
        assert!(limiter.allow(start + LIMIT_WINDOW));
    }

    #[test]
    fn a_full_limiter_refuses_until_its_window_passes() {
        let mut limiter = WriteLimiter::default();
        let start = std::time::Instant::now();
        limiter.fill(start);
        assert!(!limiter.allow(start + std::time::Duration::from_secs(59)));
        assert!(limiter.allow(start + LIMIT_WINDOW));
    }
}
