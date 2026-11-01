//! The broker binary. Usage matches `tools/panel_broker.py`:
//! `tallow-broker [--config secrets/panel/broker.json]`.

use std::path::PathBuf;
use std::process::ExitCode;

use tallow_broker::config::{load_config, load_token};
use tallow_broker::serve::serve;
use tallow_broker::vsock::Vsock;
use tallow_broker::PORT;

const USAGE: &str = "usage: tallow-broker [--config secrets/panel/broker.json]";

/// The default config, as the Python's `CONFIG_PATH`: `secrets/panel/broker.json`
/// in the repo. The binary sits at `<repo>/host/target/release/`, four levels
/// under the repo root. The path is found at run time, not baked in at build,
/// because the release build runs in a container with the repo mounted elsewhere.
fn default_config() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let root = exe.ancestors().nth(4)?;
    Some(root.join("secrets/panel/broker.json"))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = match args.as_slice() {
        [] => match default_config() {
            Some(path) => path,
            None => {
                eprintln!("cannot find the repo from the binary's path. {USAGE}");
                return ExitCode::from(2);
            }
        },
        [flag, path] if flag == "--config" => PathBuf::from(path),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(&config_path).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::from(1)
        }
    }
}

async fn run(config_path: &std::path::Path) -> anyhow::Result<()> {
    let config = load_config(config_path)?;
    load_token(&config.token_file)?;
    let mut listener = Vsock::bind(PORT)?;
    eprintln!("broker vsock {PORT} cid {}", config.cid);
    match serve(&mut listener, &config).await {
        Ok(never) => match never {},
        Err(error) => Err(error.into()),
    }
}
