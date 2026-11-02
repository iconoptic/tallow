//! The mesh host the panel keeps: it runs `tallowd run` on the root board's USB port when
//! one is plugged in, so `data/mesh/map.json` stays fresh with nothing started by hand.
//! With no board it waits, and starts the child when one appears. It does not listen on
//! the host's Bluetooth adapter. That adapter stays with the light.
//!
//! The map writer stays a child process. It owns the serial port, and a fault there does
//! not take the panel down. The child gets a pipe on stdin and exits when the pipe closes,
//! so a panel that is killed leaves no process holding the port. `data/mesh/pause` stops
//! the child while it exists, so `tools/mesh.sh` can open the port for `flash` and friends.
//!
//! Opening a CP2102 port resets the board, so nothing here opens a port to probe it. A
//! board is found from sysfs and `firmware/mesh/boards.tsv`, and a port the user cannot
//! open shows up as the child's own "Permission denied". Only a classic ESP32 row of
//! `boards.tsv` qualifies: an S3 cam is never a candidate.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;

use super::mesh_map::STALE_AFTER;

/// The udev symlink `tools/setup_mesh_host.sh` makes for the root board.
const ROOT_LINK: &str = "tallow-root";
/// `tallowd run` for ten years: the panel stops the child, the clock does not.
const FOREVER: &str = "315360000";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Starting,
    /// No root board yet. The host adapter is left alone.
    Waiting,
    NoPermission,
    HeldElsewhere,
    Running,
    Backoff,
    GaveUp,
    Paused,
}

impl State {
    fn code(self) -> &'static str {
        match self {
            State::Starting => "starting",
            State::Waiting => "waiting",
            State::NoPermission => "no-permission",
            State::HeldElsewhere => "held-elsewhere",
            State::Running => "running",
            State::Backoff => "backoff",
            State::GaveUp => "gave-up",
            State::Paused => "paused",
        }
    }
}

#[derive(Debug, Clone)]
struct Snapshot {
    state: State,
    port: Option<String>,
    restarts: u32,
    last_exit: Option<String>,
}

/// What the supervisor is doing, for the `host` part of `GET /api/mesh/map`.
pub struct Status(Mutex<Snapshot>);

impl Status {
    pub fn new() -> Self {
        Self(Mutex::new(Snapshot {
            state: State::Starting,
            port: None,
            restarts: 0,
            last_exit: None,
        }))
    }

    pub fn document(&self) -> Value {
        let snapshot = self.lock().clone();
        json!({
            "state": snapshot.state.code(),
            "port": snapshot.port,
            "restarts": snapshot.restarts,
            "last_exit": snapshot.last_exit,
        })
    }

    fn set(&self, state: State, port: Option<&Path>) {
        let mut snapshot = self.lock();
        snapshot.state = state;
        snapshot.port = port.map(|port| port.display().to_string());
    }

    fn note_exit(&self, exit: String) {
        self.lock().last_exit = Some(exit);
    }

    fn failed(&self, exit: String) {
        let mut snapshot = self.lock();
        snapshot.restarts += 1;
        snapshot.last_exit = Some(exit);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Snapshot> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Where boards are looked for. Tests point these at a temp tree.
#[derive(Debug, Clone)]
pub struct Paths {
    pub dev: PathBuf,
    pub sys_tty: PathBuf,
    pub boards: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// The program to run as `<command> run --port ...`: the panel's own binary.
    pub command: PathBuf,
    /// The repository root, where the child runs so its `data/mesh` defaults resolve.
    pub root: PathBuf,
    pub map_path: PathBuf,
    pub pause_path: PathBuf,
    pub explicit_port: Option<PathBuf>,
    pub paths: Paths,
    /// How often to look for a board that is not there yet.
    pub poll: Duration,
    /// How often to look at the pause file, and at a running child.
    pub tick: Duration,
    /// The wait after the child ends: each start resets the board.
    pub backoff: Duration,
    /// This many failures inside `window` and the supervisor gives up.
    pub tries: usize,
    pub window: Duration,
}

impl Config {
    pub fn standard(root: &Path, explicit_port: Option<PathBuf>) -> std::io::Result<Self> {
        Ok(Self {
            command: std::env::current_exe()?,
            root: root.to_path_buf(),
            map_path: root.join("data/mesh/map.json"),
            pause_path: root.join("data/mesh/pause"),
            explicit_port,
            paths: Paths {
                dev: PathBuf::from("/dev"),
                sys_tty: PathBuf::from("/sys/class/tty"),
                boards: root.join("firmware/mesh/boards.tsv"),
            },
            poll: Duration::from_secs(10),
            tick: Duration::from_secs(1),
            backoff: Duration::from_secs(30),
            tries: 5,
            window: Duration::from_secs(600),
        })
    }
}

/// The USB paths `boards.tsv` lists for a classic ESP32 (column 4 where column 3 is `esp32`).
fn classic_usb_paths(boards: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(boards) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let columns: Vec<&str> = line.split('\t').collect();
            (columns.get(2) == Some(&"esp32")).then(|| columns.get(3).map(|p| (*p).to_owned()))?
        })
        .filter(|path| !path.is_empty())
        .collect()
}

/// The USB path of a `ttyUSBn`, as `boards.tsv` writes it: sysfs names the USB interface
/// `3-7:1.0`, and `7` is what follows the bus number. Opens no port.
fn usb_path(sys_tty: &Path, name: &str) -> Option<String> {
    let device = std::fs::canonicalize(sys_tty.join(name).join("device")).ok()?;
    device.ancestors().find_map(|dir| {
        let (device, _interface) = dir.file_name()?.to_str()?.split_once(':')?;
        let (_bus, ports) = device.split_once('-')?;
        Some(ports.to_owned())
    })
}

/// The root board's port: the one named, else `/dev/tallow-root`, else the first
/// `ttyUSB*` whose USB path `boards.tsv` lists as a classic ESP32.
pub fn root_port(explicit: Option<&Path>, paths: &Paths) -> Option<PathBuf> {
    if let Some(port) = explicit {
        return port.exists().then(|| port.to_path_buf());
    }
    let link = paths.dev.join(ROOT_LINK);
    if link.exists() {
        return Some(link);
    }
    let known = classic_usb_paths(&paths.boards);
    let mut found: Vec<PathBuf> = std::fs::read_dir(&paths.dev)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            name.starts_with("ttyUSB").then_some(())?;
            let path = usb_path(&paths.sys_tty, &name)?;
            known.contains(&path).then(|| entry.path())
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

/// True when some other process wrote the map within the last minute.
fn map_is_fresh(map_path: &Path) -> bool {
    std::fs::metadata(map_path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age <= STALE_AFTER)
}

/// Returns once the panel is stopping. The borrow ends here, so no guard crosses an await.
async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|stopping| *stopping).await;
}

/// Sleeps, and says whether the panel stopped meanwhile.
async fn nap(stop: &mut watch::Receiver<bool>, time: Duration) -> bool {
    tokio::select! {
        () = tokio::time::sleep(time) => *stop.borrow(),
        () = stopped(stop) => true,
    }
}

type LastLine = Arc<Mutex<String>>;

/// Starts the child on `port`. Its stdin is a pipe the caller keeps open, its stdout is
/// dropped, and its stderr is passed on and remembered one line at a time. The host
/// adapter is not passed: the light's session owns it.
fn spawn(config: &Config, port: &Path) -> std::io::Result<(Child, LastLine)> {
    if let Some(parent) = config.map_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut child = Command::new(&config.command)
        .arg("run")
        .arg("--port")
        .arg(port)
        .args(["--seconds", FOREVER, "--out"])
        .arg(&config.map_path)
        .arg("--parent-pipe")
        .current_dir(&config.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let last: LastLine = Arc::default();
    if let Some(stderr) = child.stderr.take() {
        let last = last.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                eprintln!("mesh host: {line}");
                *last.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = line;
            }
        });
    }
    Ok((child, last))
}

enum Ended {
    /// The panel is stopping.
    Stop,
    /// The pause file appeared.
    Pause,
    Exit(String),
}

/// Waits for the child, stopping it when the panel stops or the pause file appears.
async fn watch_child(
    config: &Config,
    child: &mut Child,
    stop: &mut watch::Receiver<bool>,
) -> Ended {
    loop {
        tokio::select! {
            result = child.wait() => {
                return Ended::Exit(match result {
                    Ok(status) => status.to_string(),
                    Err(error) => error.to_string(),
                });
            }
            () = stopped(stop) => {
                let _ = child.kill().await;
                return Ended::Stop;
            }
            () = tokio::time::sleep(config.tick) => {
                if config.pause_path.exists() {
                    let _ = child.kill().await;
                    return Ended::Pause;
                }
            }
        }
    }
}

/// Keeps one `tallowd run` going on the root board until the panel stops.
pub async fn supervise(config: Config, status: Arc<Status>, mut stop: watch::Receiver<bool>) {
    let mut failures: Vec<Instant> = Vec::new();
    // Only the first start looks for another writer. After a pause or a crash, the map
    // is this supervisor's own and is still fresh.
    let mut first = true;
    while !*stop.borrow() {
        if config.pause_path.exists() {
            status.set(State::Paused, None);
            if nap(&mut stop, config.tick).await {
                return;
            }
            continue;
        }
        let port = root_port(config.explicit_port.as_deref(), &config.paths);
        if first && map_is_fresh(&config.map_path) {
            status.set(State::HeldElsewhere, port.as_deref());
            if nap(&mut stop, config.poll).await {
                return;
            }
            continue;
        }
        first = false;
        let Some(port) = port else {
            status.set(State::Waiting, None);
            if nap(&mut stop, config.poll).await {
                return;
            }
            continue;
        };
        let spawned = spawn(&config, &port);
        let exit = match spawned {
            Ok((mut child, last)) => {
                status.set(State::Running, Some(port.as_path()));
                // Held until the child ends: dropping it closes the child's stdin.
                let _pipe = child.stdin.take();
                match watch_child(&config, &mut child, &mut stop).await {
                    Ended::Stop => return,
                    Ended::Pause => continue,
                    Ended::Exit(text) => {
                        // The reader task may still be draining the child's last line.
                        tokio::task::yield_now().await;
                        let line = last
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .clone();
                        if line.contains("Permission denied") {
                            status.set(State::NoPermission, Some(port.as_path()));
                            status.note_exit(line);
                            if nap(&mut stop, config.backoff).await {
                                return;
                            }
                            continue;
                        }
                        if line.is_empty() {
                            text
                        } else {
                            format!("{text}: {line}")
                        }
                    }
                }
            }
            Err(error) => format!("cannot start {}: {error}", config.command.display()),
        };
        status.failed(exit);
        let now = Instant::now();
        failures.push(now);
        failures.retain(|at| now.duration_since(*at) <= config.window);
        if failures.len() >= config.tries {
            status.set(State::GaveUp, Some(port.as_path()));
            stopped(&mut stop).await;
            return;
        }
        status.set(State::Backoff, Some(port.as_path()));
        if nap(&mut stop, config.backoff).await {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};

    use super::*;
    use crate::library::testing::TempDir;

    const BOARDS: &str = "# header\n\
        node-0\t70:4b\tesp32\t12\tnote\n\
        node-1\t20:9b\tesp32\t7\tnote\n\
        toby-cam-0\tunknown\tesp32s3\t11\tcam\n";

    /// A fake /dev and /sys/class/tty with one ttyUSB per `(name, usb path)` and a ttyACM.
    fn tree(dir: &Path, usb: &[(&str, &str)]) -> Paths {
        let dev = dir.join("dev");
        let sys = dir.join("sys");
        std::fs::create_dir_all(&dev).expect("dev");
        for (name, path) in usb {
            std::fs::write(dev.join(name), "").expect("node");
            let interface = sys
                .join("devices")
                .join(format!("usb3/3-{path}/3-{path}:1.0/{name}"));
            std::fs::create_dir_all(&interface).expect("interface");
            let tty = sys.join("class").join(name);
            std::fs::create_dir_all(&tty).expect("tty");
            symlink(interface.parent().expect("parent"), tty.join("device")).expect("link");
        }
        std::fs::write(dev.join("ttyACM0"), "").expect("acm");
        let boards = dir.join("boards.tsv");
        std::fs::write(&boards, BOARDS).expect("boards");
        Paths {
            dev,
            sys_tty: sys.join("class"),
            boards,
        }
    }

    #[test]
    fn a_listed_classic_board_is_found_and_the_lowest_port_wins() {
        let dir = TempDir::new("mesh-host-find");
        let paths = tree(dir.path(), &[("ttyUSB1", "7"), ("ttyUSB0", "12")]);
        assert_eq!(root_port(None, &paths), Some(paths.dev.join("ttyUSB0")));
    }

    #[test]
    fn a_board_not_in_boards_tsv_and_a_cam_port_are_never_candidates() {
        let dir = TempDir::new("mesh-host-skip");
        let paths = tree(dir.path(), &[("ttyUSB0", "11"), ("ttyUSB1", "9")]);
        assert_eq!(root_port(None, &paths), None);
    }

    #[test]
    fn the_udev_link_comes_first_and_a_named_port_replaces_the_search() {
        let dir = TempDir::new("mesh-host-link");
        let paths = tree(dir.path(), &[("ttyUSB0", "12")]);
        symlink(paths.dev.join("ttyUSB0"), paths.dev.join("tallow-root")).expect("link");
        assert_eq!(root_port(None, &paths), Some(paths.dev.join("tallow-root")));
        let named = paths.dev.join("ttyUSB0");
        assert_eq!(root_port(Some(&named), &paths), Some(named.clone()));
        assert_eq!(root_port(Some(&paths.dev.join("ttyUSB9")), &paths), None);
    }

    #[test]
    fn no_board_plugged_in_finds_nothing() {
        let dir = TempDir::new("mesh-host-none");
        let paths = tree(dir.path(), &[]);
        assert_eq!(root_port(None, &paths), None);
    }

    /// A stub `tallowd`: runs `body` as a shell script, in place of `tallowd run`.
    fn stub(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("stub.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("stub");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    fn config(dir: &Path, command: PathBuf, usb: &[(&str, &str)]) -> Config {
        Config {
            command,
            root: dir.to_path_buf(),
            map_path: dir.join("data/mesh/map.json"),
            pause_path: dir.join("data/mesh/pause"),
            explicit_port: None,
            paths: tree(dir, usb),
            poll: Duration::from_millis(20),
            tick: Duration::from_millis(20),
            backoff: Duration::from_millis(30),
            tries: 3,
            window: Duration::from_secs(60),
        }
    }

    async fn until(status: &Status, state: &str) -> bool {
        for _ in 0..300 {
            if status.document()["state"] == state {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    async fn args_starting_with(dir: &Path, prefix: &str) -> String {
        let mut last = String::new();
        for _ in 0..300 {
            if let Ok(text) = std::fs::read_to_string(dir.join("args.txt")) {
                if text.starts_with(prefix) && text.ends_with('\n') {
                    return text;
                }
                last = text;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        last
    }

    #[tokio::test]
    async fn no_board_waits_and_a_board_plugged_in_later_starts_the_root() {
        let dir = TempDir::new("mesh-host-late");
        let command = stub(dir.path(), "echo $@ > args.txt; exec sleep 30");
        let config = config(dir.path(), command, &[]);
        let status = Arc::new(Status::new());
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(supervise(config.clone(), status.clone(), stop_rx));
        assert!(until(&status, "waiting").await);
        assert!(status.document()["port"].is_null());
        assert!(
            !dir.path().join("args.txt").exists(),
            "a missing root starts no child"
        );
        tree(dir.path(), &[("ttyUSB0", "12")]);
        assert!(until(&status, "running").await);
        let doc = status.document();
        assert!(doc["port"].as_str().is_some_and(|p| p.ends_with("ttyUSB0")));
        let args = args_starting_with(dir.path(), "run --port ").await;
        let _ = stop_tx.send(true);
        task.await.expect("supervisor ends");
        assert!(args.starts_with("run --port "), "{args}");
        assert!(!args.contains("--bluez"), "{args}");
        assert!(args.contains("--seconds 315360000 --out "), "{args}");
        assert!(args.contains("data/mesh/map.json --parent-pipe"), "{args}");
    }

    #[tokio::test]
    async fn a_child_that_keeps_failing_is_retried_then_given_up_on() {
        let dir = TempDir::new("mesh-host-giveup");
        let command = stub(dir.path(), "echo 'boom' >&2; exit 3");
        let config = config(dir.path(), command, &[("ttyUSB0", "12")]);
        let status = Arc::new(Status::new());
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(supervise(config, status.clone(), stop_rx));
        assert!(until(&status, "gave-up").await);
        let doc = status.document();
        assert_eq!(doc["restarts"], json!(3));
        assert!(doc["last_exit"]
            .as_str()
            .is_some_and(|t| t.contains("boom")));
        let _ = stop_tx.send(true);
        task.await.expect("supervisor ends");
    }

    #[tokio::test]
    async fn a_permission_error_is_its_own_state_and_never_gives_up() {
        let dir = TempDir::new("mesh-host-perm");
        let command = stub(dir.path(), "echo 'Permission denied' >&2; exit 1");
        let config = config(dir.path(), command, &[("ttyUSB0", "12")]);
        let status = Arc::new(Status::new());
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(supervise(config, status.clone(), stop_rx));
        assert!(until(&status, "no-permission").await);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_ne!(status.document()["state"], json!("gave-up"));
        assert_eq!(status.document()["restarts"], json!(0));
        let _ = stop_tx.send(true);
        task.await.expect("supervisor ends");
    }

    #[tokio::test]
    async fn the_pause_file_stops_the_child_and_removing_it_starts_one_again() {
        let dir = TempDir::new("mesh-host-pause");
        let command = stub(dir.path(), "echo $$ >> pids.txt; exec sleep 30");
        let config = config(dir.path(), command, &[("ttyUSB0", "12")]);
        let status = Arc::new(Status::new());
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(supervise(config.clone(), status.clone(), stop_rx));
        assert!(until(&status, "running").await);
        std::fs::write(&config.pause_path, "").expect("pause");
        assert!(until(&status, "paused").await);
        std::fs::remove_file(&config.pause_path).expect("resume");
        assert!(until(&status, "running").await);
        let pids = std::fs::read_to_string(dir.path().join("pids.txt")).expect("pids");
        assert_eq!(pids.lines().count(), 2, "a pause is not a failure: {pids}");
        assert_eq!(status.document()["restarts"], json!(0));
        let _ = stop_tx.send(true);
        task.await.expect("supervisor ends");
    }

    #[tokio::test]
    async fn another_writer_keeps_the_panel_from_starting_a_second_one() {
        let dir = TempDir::new("mesh-host-held");
        let command = stub(dir.path(), "echo started >> started.txt; exec sleep 30");
        let config = config(dir.path(), command, &[("ttyUSB0", "12")]);
        std::fs::create_dir_all(config.map_path.parent().expect("parent")).expect("dir");
        std::fs::write(&config.map_path, "{}").expect("map");
        let status = Arc::new(Status::new());
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(supervise(config, status.clone(), stop_rx));
        assert!(until(&status, "held-elsewhere").await);
        assert!(!dir.path().join("started.txt").exists());
        let _ = stop_tx.send(true);
        task.await.expect("supervisor ends");
    }

    #[tokio::test]
    async fn stopping_the_panel_stops_the_child() {
        let dir = TempDir::new("mesh-host-stop");
        let command = stub(dir.path(), "echo $$ > pid.txt; exec sleep 30");
        let config = config(dir.path(), command, &[("ttyUSB0", "12")]);
        let status = Arc::new(Status::new());
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(supervise(config, status.clone(), stop_rx));
        assert!(until(&status, "running").await);
        let pid = loop {
            if let Ok(text) = std::fs::read_to_string(dir.path().join("pid.txt")) {
                if let Ok(pid) = text.trim().parse::<u32>() {
                    break pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let _ = stop_tx.send(true);
        task.await.expect("supervisor ends");
        for _ in 0..100 {
            if !Path::new(&format!("/proc/{pid}")).exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the child {pid} outlived the supervisor");
    }

    #[test]
    fn the_status_document_has_its_four_fields() {
        let status = Status::new();
        assert_eq!(
            status.document(),
            json!({"state": "starting", "port": null, "restarts": 0, "last_exit": null})
        );
    }
}
