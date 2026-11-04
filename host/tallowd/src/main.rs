//! The tallow host daemon. See docs/plans/2026-10-31-rust-conversion.md.
//!
//! It shares the mesh's frame format with the firmware through
//! `tallow-mesh-core`, so the two cannot drift apart.

#[cfg_attr(not(test), allow(dead_code))]
mod cams;
mod cluster;
mod decode;
mod gatt;
mod identity;
mod irk;
#[cfg_attr(not(test), allow(dead_code))]
mod library;
mod link;
mod local;
mod map;
mod ota;
mod panel;
mod rooms;
#[cfg_attr(not(test), allow(dead_code))]
mod show;
mod traffic;
mod trial;

use anyhow::{bail, ensure, Context};
use identity::Identities;
use irk::Keys;
use link::{unix_now, Link, ScanSettings, DEFAULT_SCAN};
use map::{Map, Recorder};
use rooms::Rooms;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use tallow_mesh_core::wire::VERSION;
use trial::Plan;

fn banner() -> String {
    format!(
        "tallowd {} mesh frame version {VERSION:#04x}",
        env!("CARGO_PKG_VERSION")
    )
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.first().map(String::as_str) {
        Some("panel") => std::process::exit(panel::run(&argv[1..])),
        Some("run") => match run_command(&argv[1..]) {
            Ok(code) => std::process::exit(code),
            Err(err) => {
                eprintln!("tallowd run: {err:#}");
                std::process::exit(1);
            }
        },
        Some("gatt") => match gatt::command(&argv[1..]) {
            Ok(code) => std::process::exit(code),
            Err(err) => {
                eprintln!("tallowd gatt: {err:#}");
                std::process::exit(1);
            }
        },
        Some("ota") => match ota::command(&argv[1..]) {
            Ok(code) => std::process::exit(code),
            Err(err) => {
                eprintln!("tallowd ota: {err:#}");
                std::process::exit(1);
            }
        },
        Some(other) => {
            eprintln!(
                "tallowd: unknown command {other}. The commands are: tallowd panel, tallowd run, tallowd gatt, tallowd ota"
            );
            std::process::exit(2);
        }
        None => println!("{}", banner()),
    }
}

#[derive(Debug, PartialEq)]
struct RunArgs {
    /// The root board's port. `None` runs on the host's adapter alone.
    port: Option<String>,
    /// `--bluez`: also listen on the host's adapter. `Some(None)` is the seed's adapter.
    bluez: Option<Option<String>>,
    seconds: f64,
    out: PathBuf,
    irks: PathBuf,
    devices: PathBuf,
    rooms: PathBuf,
    /// `--set`: the scan settings to send in place of `DEFAULT_SCAN`. The trial ignores it, as the Python does.
    set: Option<ScanSettings>,
    /// `--watch`: the paging line from `link::watch_line`. `Some` even with no
    /// addresses, which clears the list.
    watch: Option<Vec<u8>>,
    /// `--trial`: measure delivery under each setting instead of keeping a map.
    trial: Option<Plan>,
    /// `--parent-pipe`: exit when stdin closes. The panel holds the other end, so a
    /// panel that dies never leaves this process holding the root's port.
    parent_pipe: bool,
}

/// `tallowd run [--port <p>] [--bluez [hciN]] --out <path> [--seconds <n>] [--irks <path>] [--devices <path>] [--rooms <path>]
/// [--parent-pipe] [--set WINDOW INTERVAL PERIOD LENGTH] [--watch ADDR...] [--trial [--hold <s>] [--hz <n>] [--size <n>] [--pass-mark <percent>]]`.
/// One of `--port` and `--bluez` is needed. With `--bluez` and no `--port` the host's adapter is
/// the only listener, and the trial, `--set`, and `--watch` (which go to boards) are refused.
/// Seconds default to 60, the keys to `data/mesh/irks.toml`, the devices to
/// `data/mesh/devices.toml`, and the rooms to `data/mesh/rooms.toml`. The trial
/// defaults are those of `tools/mesh_host.py --trial`.
fn parse_run(argv: &[String]) -> anyhow::Result<RunArgs> {
    let mut port = None;
    let mut bluez = None;
    let mut seconds: f64 = 60.0;
    let mut out = None;
    let mut irks = PathBuf::from(irk::DEFAULT_PATH);
    let mut devices = PathBuf::from(identity::DEFAULT_PATH);
    let mut rooms = PathBuf::from(rooms::DEFAULT_PATH);
    let mut set = None;
    let mut watch = None;
    let mut trial = false;
    let mut parent_pipe = false;
    let mut plan = Plan::default();
    let mut words = argv.iter().peekable();
    while let Some(flag) = words.next() {
        match flag.as_str() {
            "--trial" => trial = true,
            "--parent-pipe" => parent_pipe = true,
            "--bluez" => bluez = Some(words.next_if(|word| !word.starts_with("--")).cloned()),
            "--watch" => {
                let mut addresses = Vec::new();
                while let Some(address) = words.next_if(|word| !word.starts_with("--")) {
                    addresses.push(address.clone());
                }
                watch = Some(link::watch_line(&addresses)?);
            }
            "--set" => {
                let values = (0..4)
                    .map(|_| words.next())
                    .collect::<Option<Vec<&String>>>()
                    .context("--set needs four values: WINDOW INTERVAL PERIOD LENGTH")?;
                set = Some(ScanSettings {
                    window_ms: parse_number("--set window", values[0])?,
                    interval_ms: parse_number("--set interval", values[1])?,
                    inquiry_period_s: parse_number("--set period", values[2])?,
                    inquiry_length: parse_number("--set length", values[3])?,
                    probe_hz: DEFAULT_SCAN.probe_hz,
                    probe_size: DEFAULT_SCAN.probe_size,
                });
            }
            "--port" | "--seconds" | "--out" | "--irks" | "--devices" | "--rooms" | "--hold"
            | "--hz" | "--size" | "--pass-mark" => {
                let value = words
                    .next()
                    .with_context(|| format!("{flag} needs a value"))?;
                match flag.as_str() {
                    "--port" => port = Some(value.clone()),
                    "--seconds" => {
                        seconds = value
                            .parse()
                            .with_context(|| format!("--seconds {value} is not a number"))?;
                    }
                    "--out" => out = Some(PathBuf::from(value)),
                    "--irks" => irks = PathBuf::from(value),
                    "--devices" => devices = PathBuf::from(value),
                    "--rooms" => rooms = PathBuf::from(value),
                    "--hold" => plan.hold = parse_number(flag, value)?,
                    "--hz" => plan.hz = parse_number(flag, value)?,
                    "--size" => plan.size = parse_number(flag, value)?,
                    _ => plan.pass_mark = parse_number(flag, value)?,
                }
            }
            other => bail!("unknown flag {other}"),
        }
    }
    ensure!(
        seconds.is_finite() && seconds >= 0.0,
        "--seconds must be zero or more"
    );
    ensure!(
        plan.hold.is_finite() && plan.hold >= 0.0,
        "--hold must be zero or more"
    );
    ensure!(plan.pass_mark.is_finite(), "--pass-mark must be a number");
    ensure!(
        port.is_some() || bluez.is_some(),
        "--port or --bluez is required"
    );
    ensure!(
        port.is_some() || (!trial && set.is_none() && watch.is_none()),
        "--trial, --set, and --watch go to boards, so they need --port"
    );
    Ok(RunArgs {
        port,
        bluez,
        seconds,
        out: out.context("--out is required")?,
        irks,
        devices,
        rooms,
        set,
        watch,
        trial: trial.then_some(plan),
        parent_pipe,
    })
}

fn parse_number<T: std::str::FromStr>(flag: &str, value: &str) -> anyhow::Result<T> {
    value
        .parse()
        .map_err(|_| anyhow::anyhow!("{flag} {value} is not a number, or is out of range"))
}

/// Runs the root's port and the host's adapter, whichever are named: a map by default, or
/// the delivery trial with `--trial`. `Link::open` refuses anything but a classic board's
/// port, and waits out the reset. The keys, devices, and rooms load first, so a bad file
/// fails before the board is reset. Returns the exit code: the trial exits 1 under its pass mark.
fn run_command(argv: &[String]) -> anyhow::Result<i32> {
    let args = parse_run(argv)?;
    if args.parent_pipe {
        exit_when_stdin_closes();
    }
    let keys = Keys::load(&args.irks)?;
    let identities = Identities::load(&args.devices)?;
    let rooms = Rooms::load(&args.rooms)?;
    // The log opens before the port. A log that will not open stays off, and the map still runs.
    // A trial does not keep one. An empty log copies the map already on disk.
    let mut log = if args.trial.is_none() {
        traffic::open_log(&args.out.with_file_name("traffic.db"), &args.out)
    } else {
        traffic::Log::off()
    };
    if log.is_open() {
        // Detached on return. The thread ends with this process and is not joined.
        drop(cluster::spawn_reconciler(
            args.out.with_file_name("traffic.db"),
            Duration::from_secs(30),
            Duration::from_secs(5 * 60),
        ));
    }
    let mut link = args.port.as_deref().map(Link::open).transpose()?;
    let local = args.bluez.clone().map(local::start);
    let mut recorder = Recorder::with_keys(keys, identities, rooms);
    if let (Some(plan), Some(link)) = (&args.trial, link.as_mut()) {
        // Read first, so the leaf finds this root before the first setting, as run_trial does.
        read_for(
            Some(link),
            local.as_ref(),
            &mut recorder,
            12.0,
            &args.out,
            &mut log,
        )?;
        if let Some(line) = &args.watch {
            link.send_watch(line)?;
        }
        let rows = trial::run(link, &mut recorder.world, plan, &mut |line| {
            println!("{line}");
        })?;
        finish(&recorder.world, &args.out)?;
        return Ok(if trial::passed(&rows, plan.pass_mark) {
            0
        } else {
            1
        });
    }
    if let Some(link) = link.as_mut() {
        // The first read sends the heartbeat, so this board is the root before the
        // settings flood out, as in mesh_host.py.
        let lines = link.lines()?;
        let now = unix_now();
        recorder.feed(&lines, now);
        log.note_lines(&lines, now);
        link.send_command(&args.set.unwrap_or(DEFAULT_SCAN))?;
        if let Some(line) = &args.watch {
            link.send_watch(line)?;
        }
    }
    read_for(
        link.as_mut(),
        local.as_ref(),
        &mut recorder,
        args.seconds,
        &args.out,
        &mut log,
    )?;
    finish(&recorder.world, &args.out)?;
    Ok(0)
}

/// Ends the process when stdin reaches end of file. Nothing is written to it.
fn exit_when_stdin_closes() {
    std::thread::spawn(|| {
        let mut sink = [0u8; 64];
        let mut stdin = std::io::stdin();
        while matches!(std::io::Read::read(&mut stdin, &mut sink), Ok(n) if n > 0) {}
        std::process::exit(0);
    });
}

/// Reads the port and the adapter for `seconds`, printing what the map does not keep and
/// saving the map each time it is due. With no port it waits on the adapter instead.
fn read_for(
    mut link: Option<&mut Link>,
    local: Option<&Receiver<local::Event>>,
    recorder: &mut Recorder,
    seconds: f64,
    out: &Path,
    log: &mut traffic::Log,
) -> anyhow::Result<()> {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < end {
        let mut save = false;
        if let Some(link) = link.as_deref_mut() {
            let lines = link.lines()?;
            let now = unix_now();
            let (printed, due) = recorder.feed(&lines, now);
            for line in printed {
                println!("{line}");
            }
            log.note_lines(&lines, now);
            save |= due;
        }
        if let Some(local) = local {
            let wait = if link.is_some() {
                Duration::ZERO
            } else {
                Duration::from_millis(200)
            };
            let mut next = match local.recv_timeout(wait) {
                Ok(event) => Some(event),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => bail!("the adapter scanner stopped"),
            };
            while let Some(event) = next {
                match event {
                    local::Event::Batch(batch) => {
                        let now = unix_now();
                        save |= recorder.feed_local(&batch, now);
                        log.note_batch(&batch, now);
                    }
                    local::Event::Note(note) => eprintln!("{note}"),
                }
                next = local.try_recv().ok();
            }
        }
        if save {
            recorder.world.save(out)?;
        }
    }
    Ok(())
}

/// Saves the map and prints its suggestions and counts.
fn finish(world: &Map, out: &Path) -> anyhow::Result<()> {
    world.save(out)?;
    for line in world.suggestion_lines() {
        println!("{line}");
    }
    let (devices, nodes) = world.counts();
    println!(
        "{devices} devices, {nodes} nodes, saved to {}",
        out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn the_banner_names_the_frame_version() {
        assert!(banner().contains(&format!("{VERSION:#04x}")));
    }

    #[test]
    fn run_takes_its_three_flags_and_sixty_seconds_by_default() {
        let args = parse_run(&words("--port /dev/ttyUSB0 --out data/mesh/map.json")).expect("args");
        assert_eq!(
            args,
            RunArgs {
                port: Some("/dev/ttyUSB0".into()),
                bluez: None,
                seconds: 60.0,
                out: PathBuf::from("data/mesh/map.json"),
                irks: PathBuf::from("data/mesh/irks.toml"),
                devices: PathBuf::from("data/mesh/devices.toml"),
                rooms: PathBuf::from("data/mesh/rooms.toml"),
                set: None,
                watch: None,
                trial: None,
                parent_pipe: false,
            }
        );
    }

    #[test]
    fn bluez_alone_runs_on_the_adapter_and_takes_an_optional_name() {
        let seed = parse_run(&words("--bluez --out m.json")).expect("seed");
        assert_eq!((seed.port, seed.bluez), (None, Some(None)));
        let named = parse_run(&words("--bluez hci1 --out m.json --seconds 5")).expect("named");
        assert_eq!(named.bluez, Some(Some("hci1".into())));
        let both = parse_run(&words("--port p --bluez --out m.json")).expect("both");
        assert_eq!(both.port.as_deref(), Some("p"));
        assert_eq!(both.bluez, Some(None));
    }

    #[test]
    fn what_goes_to_boards_needs_a_port() {
        assert!(parse_run(&words("--bluez --out m.json --trial")).is_err());
        assert!(parse_run(&words("--bluez --out m.json --set 30 100 12 4")).is_err());
        assert!(parse_run(&words("--bluez --out m.json --watch")).is_err());
        assert!(parse_run(&words("--out m.json")).is_err());
    }

    #[test]
    fn run_takes_parent_pipe_as_a_switch() {
        let args = parse_run(&words("--port p --out m.json --parent-pipe")).expect("args");
        assert!(args.parent_pipe);
    }

    #[test]
    fn run_reads_the_devices_from_the_path_it_is_given() {
        let args =
            parse_run(&words("--port p --out m.json --devices /keys/devices.toml")).expect("args");
        assert_eq!(args.devices, PathBuf::from("/keys/devices.toml"));
    }

    #[test]
    fn run_reads_the_rooms_from_the_path_it_is_given() {
        let args =
            parse_run(&words("--port p --out m.json --rooms /keys/rooms.toml")).expect("args");
        assert_eq!(args.rooms, PathBuf::from("/keys/rooms.toml"));
    }

    #[test]
    fn run_reads_the_keys_from_the_path_it_is_given() {
        let args = parse_run(&words("--port p --out m.json --irks /keys/irks.toml")).expect("args");
        assert_eq!(args.irks, PathBuf::from("/keys/irks.toml"));
    }

    #[test]
    fn run_refuses_a_missing_port_a_missing_out_an_unknown_flag_and_a_bad_number() {
        assert!(parse_run(&words("--out m.json")).is_err());
        assert!(parse_run(&words("--port /dev/ttyUSB0")).is_err());
        assert!(parse_run(&words("--port /dev/ttyUSB0 --out m.json --bogus")).is_err());
        assert!(parse_run(&words("--port /dev/ttyUSB0 --out m.json --seconds soon")).is_err());
        assert!(parse_run(&words("--port /dev/ttyUSB0 --out m.json --seconds -1")).is_err());
    }

    #[test]
    fn trial_takes_the_pythons_defaults_and_its_own_flags() {
        let plain = parse_run(&words("--port p --out m.json --trial")).expect("trial");
        assert_eq!(plain.trial, Some(Plan::default()));
        assert_eq!(
            Plan::default(),
            Plan {
                hold: 45.0,
                hz: 10,
                size: 200,
                pass_mark: 95.0,
            }
        );
        let tuned = parse_run(&words(
            "--port p --out m.json --trial --hold 20 --hz 5 --size 64 --pass-mark 90",
        ))
        .expect("tuned");
        assert_eq!(
            tuned.trial,
            Some(Plan {
                hold: 20.0,
                hz: 5,
                size: 64,
                pass_mark: 90.0,
            })
        );
    }

    #[test]
    fn no_trial_flag_means_no_trial() {
        assert_eq!(
            parse_run(&words("--port p --out m.json"))
                .expect("map")
                .trial,
            None
        );
    }

    #[test]
    fn set_takes_four_numbers_and_keeps_the_default_probe_settings() {
        let args = parse_run(&words("--port p --out m.json --set 30 100 12 4")).expect("set");
        assert_eq!(
            args.set,
            Some(ScanSettings {
                window_ms: 30,
                interval_ms: 100,
                inquiry_period_s: 12,
                inquiry_length: 4,
                probe_hz: 0,
                probe_size: 8,
            })
        );
    }

    #[test]
    fn set_needs_four_numbers() {
        assert!(parse_run(&words("--port p --out m.json --set 30 100 12")).is_err());
        assert!(parse_run(&words("--port p --out m.json --set 30 100 12 four")).is_err());
    }

    #[test]
    fn watch_takes_the_addresses_up_to_the_next_flag() {
        let args = parse_run(&words(
            "--port p --out m.json --watch AA:BB:CC:DD:EE:01 10:20:30:40:50:60 --seconds 5",
        ))
        .expect("watch");
        assert_eq!(
            args.watch.as_deref(),
            Some(&b"W aabbccddee01 102030405060\n"[..])
        );
        assert_eq!(args.seconds, 5.0);
    }

    #[test]
    fn watch_with_no_address_clears_the_list() {
        let args = parse_run(&words("--port p --out m.json --watch")).expect("clear");
        assert_eq!(args.watch.as_deref(), Some(&b"W\n"[..]));
    }

    #[test]
    fn a_bad_watch_address_is_refused_before_the_port_is_named() {
        assert!(parse_run(&words("--port p --out m.json --watch AA:BB")).is_err());
    }
}
