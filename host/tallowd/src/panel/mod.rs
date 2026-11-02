//! `tallowd panel`: the house LAN panel. Port of `app/h7020_panel.py`.
//!
//! Two doors open on one route table. The house LAN listens on TCP and takes the
//! browser cookie only. The agent socket, `data/panel/agent.sock`, takes bearer tokens
//! only and sees the light's API and nothing else. The light is a preview with no radio,
//! or the BLE link: `session.rs` holds the rules, and `bluez.rs` is the BlueZ transport.

mod access;
mod bluez;
mod light;
mod mesh_host;
mod mesh_map;
mod pages;
mod routes;
mod session;
mod setup;

#[cfg(test)]
mod tests;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::{TcpListener, UnixListener};
use tokio::signal::unix::SignalKind;

use crate::cams::{self, Book, Hubs, Tuning};
use crate::library::Library;

use self::access::{session_token, Channel};
use self::light::{Light, Link, Offline, Preview};
use self::routes::WriteLimiter;
use self::setup::Args;

/// How long shutdown waits for open connections before it stops them. A viewer of a cam
/// stream is never done on its own, and the contract stops the panel with a 5 s budget.
const GRACE: Duration = Duration::from_secs(2);

/// The state every route shares.
pub struct Panel {
    password: String,
    browser: String,
    agents_path: PathBuf,
    agents_note: Mutex<Option<String>>,
    catalog: Library,
    limiter: Mutex<WriteLimiter>,
    light: Arc<Light>,
    book: Mutex<Book>,
    cams_path: Option<PathBuf>,
    hubs: Hubs,
    /// Where the mesh map is read from. Set by `with_map`; a panel without one has no map.
    map_path: Option<PathBuf>,
    /// What the mesh host supervisor is doing. `None` when the panel does not run one.
    mesh_host: Option<Arc<mesh_host::Status>>,
}

impl Panel {
    /// The panel with the given light, files, and cams. Tests and `run` both build it here.
    pub fn new(
        password: &str,
        light: Light,
        catalog: Library,
        agents_path: PathBuf,
        cams_path: Option<PathBuf>,
        book: Book,
        hubs: Hubs,
    ) -> Self {
        Self {
            password: password.to_owned(),
            browser: session_token(password),
            agents_path,
            agents_note: Mutex::new(None),
            catalog,
            limiter: Mutex::new(WriteLimiter::default()),
            light: Arc::new(light),
            book: Mutex::new(book),
            cams_path,
            hubs,
            map_path: None,
            mesh_host: None,
        }
    }

    /// The file the `/devices` page reads the mesh map from.
    pub fn with_map(mut self, map_path: PathBuf) -> Self {
        self.map_path = Some(map_path);
        self
    }

    /// The status of the mesh host this panel keeps, shown with the map.
    fn with_mesh_host(mut self, status: Arc<mesh_host::Status>) -> Self {
        self.mesh_host = Some(status);
        self
    }
}

/// The light a run starts with: the preview when asked, otherwise no light at all.
pub fn preview_light(library: Library) -> Light {
    Light::new(
        Box::new(Preview),
        library,
        Link::preview(),
        true,
        Some([255, 0, 0]),
    )
}

/// The BLE link as `start` builds it. It runs as a task inside the server's runtime.
pub struct Radio {
    queue: Arc<session::Queue>,
    transport: bluez::BluerTransport,
    options: session::Options,
}

/// A duration from a flag's seconds. A negative or non-finite value is refused.
fn seconds(value: f64, flag: &str) -> Result<Duration, String> {
    Duration::try_from_secs_f64(value).map_err(|_| format!("{flag} must be zero or more."))
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn offline_light(library: Library) -> Light {
    Light::new(Box::new(Offline), library, Link::offline(), false, None)
}

/// The directory the repository is in. The binary is built at `host/target/...`, so the
/// repository is four levels up from it, and the working directory is the fallback.
fn repo_root() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.ancestors().nth(4).map(Path::to_path_buf))
        .filter(|root| root.join("app").is_dir())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Runs `tallowd panel` and returns the process exit code.
pub fn run(argv: &[String]) -> i32 {
    let args = match setup::parse_args(argv) {
        Ok(args) => args,
        Err(message) => {
            eprintln!(
                "usage: tallowd panel [--preview] [--port N] ...\ntallowd panel: error: {message}"
            );
            return 2;
        }
    };
    match start(args) {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("{message}");
            1
        }
    }
}

fn start(args: Args) -> Result<(), String> {
    if args.password_file.is_some() && !args.preview {
        return Err(
            "--password-file needs --preview. The real panel reads secrets/panel/password."
                .to_owned(),
        );
    }
    if (args.data_dir.is_some() || args.agents_file.is_some()) && !args.preview {
        return Err(
            "--data-dir and --agents-file need --preview. The real panel uses data/panel and secrets/panel."
                .to_owned(),
        );
    }
    if args.preview && (args.mesh.is_some() || args.mesh_port.is_some()) {
        return Err(
            "--mesh and --mesh-port need the real panel. A preview never opens a serial port."
                .to_owned(),
        );
    }
    if args.mesh == Some(false) && args.mesh_port.is_some() {
        return Err("--mesh-port needs the mesh host, so it cannot go with --mesh off.".to_owned());
    }
    let root = repo_root();
    // Python falls back to secrets/panel/password here, even for a preview. A preview
    // never reads secrets/, so the flag is required.
    let password_path =
        match (&args.password_file, args.preview) {
            (Some(path), _) => path.clone(),
            (None, true) => return Err(
                "--preview needs --password-file. A preview never reads secrets/panel/password."
                    .to_owned(),
            ),
            (None, false) => root.join("secrets/panel/password"),
        };
    let password = setup::load_password(&password_path)?;
    let host = if args.preview {
        if args.host.as_deref().is_some_and(|host| host != "127.0.0.1") {
            return Err("Preview binds only 127.0.0.1.".to_owned());
        }
        "127.0.0.1".to_owned()
    } else {
        setup::choose_host(
            args.host.as_deref(),
            setup::default_route_ipv4().as_deref(),
            &setup::local_ipv4(),
        )?
    };
    let data_dir = args.data_dir.clone().unwrap_or_else(|| {
        root.join("data").join(if args.preview {
            "panel-preview"
        } else {
            "panel"
        })
    });
    // A preview never reads the live token hashes.
    let agents_path = if args.preview {
        args.agents_file
            .clone()
            .unwrap_or_else(|| data_dir.join("agents.json"))
    } else {
        root.join("secrets/panel/agents.json")
    };
    let sock_path = data_dir.join("agent.sock");
    let cams_path = data_dir.join("cams.json");
    let (_title, listed) =
        cams::load_inventory(&cams_path).map_err(|error| format!("Cams unreadable: {error}"))?;

    eprintln!("Panel http://{host}:{}/", args.port);
    eprintln!("Agent socket {}", sock_path.display());
    eprintln!(
        "Cams {} from {}, re-read when it changes",
        listed.len(),
        cams_path.display()
    );
    let library = Library::new(data_dir.clone());
    let (light, radio) = if args.preview {
        eprintln!("Preview only. BLE is not connected.");
        (preview_light(library.clone()), None)
    } else {
        let address: bluer::Address = args
            .address
            .parse()
            .map_err(|_| format!("--address {} is not a Bluetooth address.", args.address))?;
        let adapter = args
            .adapter
            .clone()
            .unwrap_or_else(tallow_govee::ble::seed_adapter);
        let options = session::Options {
            scan: seconds(args.scan_seconds, "--scan-seconds")?,
            timeout: seconds(args.timeout, "--timeout")?,
            strings: args.strings.map(i64::from),
        };
        eprintln!(
            "BLE on {adapter} to {address}. The light routes answer 503 until the light links."
        );
        let queue = Arc::new(session::Queue::default());
        let light = light::ble_light(library.clone(), queue.clone());
        let radio = Radio {
            queue,
            transport: bluez::BluerTransport::new(address, adapter),
            options,
        };
        (light, Some(radio))
    };
    // The map `tallowd run` or `tools/mesh.sh host` writes. A preview reads its own copy.
    let map_path = if args.preview {
        data_dir.join("mesh-map.json")
    } else {
        root.join("data/mesh/map.json")
    };
    let mesh = if args.preview || args.mesh == Some(false) {
        None
    } else {
        let config = mesh_host::Config::standard(&root, args.mesh_port.clone())
            .map_err(|error| format!("cannot find this program to run the mesh host: {error}"))?;
        eprintln!(
            "Mesh host: finds the root board and keeps {} fresh. --mesh off turns it off.",
            config.map_path.display()
        );
        Some((config, Arc::new(mesh_host::Status::new())))
    };
    let mut panel = Panel::new(
        &password,
        light,
        library,
        agents_path,
        Some(cams_path.clone()),
        Book::new(Some(cams_path), Vec::new()),
        Hubs::new(Tuning::default()),
    )
    .with_map(map_path);
    if let Some((_, status)) = &mesh {
        panel = panel.with_mesh_host(status.clone());
    }
    serve(Arc::new(panel), &host, args.port, sock_path, radio, mesh)
}

/// Binds the two doors, runs the BLE link if there is one, and serves until SIGTERM or SIGINT.
fn serve(
    panel: Arc<Panel>,
    host: &str,
    port: u16,
    sock_path: PathBuf,
    radio: Option<Radio>,
    mesh: Option<(mesh_host::Config, Arc<mesh_host::Status>)>,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start the runtime: {error}"))?;
    runtime.block_on(async move {
        let lan = TcpListener::bind((host, port))
            .await
            .map_err(|error| format!("cannot bind {host}:{port}: {error}"))?;
        let agent = bind_socket(&sock_path)?;
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let scheduler = tokio::spawn(light::run_scheduler(panel.light.clone()));
        let mut link = radio.map(|radio| {
            let engine = session::Engine::new(
                radio.transport,
                radio.queue,
                panel.light.clone(),
                radio.options,
            );
            tokio::spawn(engine.run(stop_rx.clone()))
        });

        let mut mesh_task = mesh.map(|(config, status)| {
            tokio::spawn(mesh_host::supervise(config, status, stop_rx.clone()))
        });

        let lan_app = routes::router(panel.clone(), Channel::Lan);
        let agent_app = routes::router(panel.clone(), Channel::Agent);
        let lan_server =
            axum::serve(lan, lan_app).with_graceful_shutdown(wait_for_stop(stop_rx.clone()));
        let agent_server =
            axum::serve(agent, agent_app).with_graceful_shutdown(wait_for_stop(stop_rx.clone()));
        let serving = async {
            let (lan_result, agent_result) = tokio::join!(lan_server, agent_server);
            for result in [lan_result, agent_result] {
                if let Err(error) = result {
                    eprintln!("The panel stopped with an error: {error}");
                }
            }
        };
        tokio::pin!(serving);
        tokio::select! {
            () = &mut serving => {}
            () = async {
                wait_for_signal().await;
                let _ = stop_tx.send(true);
                tokio::time::sleep(GRACE).await;
            } => {}
        }
        // Stopping again is harmless when a server ended first. The link drops before exit.
        let _ = stop_tx.send(true);
        if let Some(link) = link.take() {
            let _ = tokio::time::timeout(GRACE, link).await;
        }
        if let Some(task) = mesh_task.take() {
            let _ = tokio::time::timeout(GRACE, task).await;
        }
        scheduler.abort();
        let _ = std::fs::remove_file(&sock_path);
        Ok::<(), String>(())
    })
}

/// Creates the agent socket in a 0700 directory and sets it to 0600 right after the bind.
/// The directory mode keeps other users out during the moment the socket is 0666.
fn bind_socket(sock_path: &Path) -> Result<UnixListener, String> {
    if let Some(parent) = sock_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot chmod {}: {error}", parent.display()))?;
    }
    if sock_path.exists() {
        std::fs::remove_file(sock_path)
            .map_err(|error| format!("cannot remove {}: {error}", sock_path.display()))?;
    }
    let listener = UnixListener::bind(sock_path)
        .map_err(|error| format!("cannot bind {}: {error}", sock_path.display()))?;
    std::fs::set_permissions(sock_path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot chmod {}: {error}", sock_path.display()))?;
    Ok(listener)
}

async fn wait_for_stop(mut stop: tokio::sync::watch::Receiver<bool>) {
    let _ = stop.wait_for(|stopping| *stopping).await;
}

async fn wait_for_signal() {
    let terminate = tokio::signal::unix::signal(SignalKind::terminate());
    match terminate {
        Ok(mut signal) => {
            tokio::select! {
                _ = signal.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}
