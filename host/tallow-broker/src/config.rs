//! The broker's config file and the bearer token file. Mirrors `load_config`,
//! `load_token`, `check_mode`, and the path checks in `tools/panel_broker.py`.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use crate::PORT;

/// The validated config. The token itself is never in here, only its path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub cid: u32,
    pub port: u16,
    pub socket: PathBuf,
    pub token_file: PathBuf,
}

/// Python's `check_mode`: the file must be exactly mode 0600.
pub fn check_mode(path: &Path) -> Result<()> {
    let mode = fs::metadata(path)
        .with_context(|| format!("cannot stat {}", path.display()))?
        .permissions()
        .mode()
        & 0o7777;
    if mode != 0o600 {
        bail!(
            "{} is mode {mode:04o}. chmod 0600 {}",
            path.display(),
            path.display()
        );
    }
    Ok(())
}

/// Python's `load_config`. Refuses a config that is not 0600 or names other paths.
pub fn load_config(path: &Path) -> Result<Config> {
    check_mode(path)?;
    let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not JSON", path.display()))?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("{} is not an object", path.display()))?;

    // as_u64 is None for a bool and for a float, as Python's isinstance check is for bool.
    let cid = match object.get("cid").and_then(Value::as_u64) {
        Some(cid) if (3..=0xFFFF_FFFE).contains(&cid) => cid as u32,
        _ => bail!("{} has a bad cid", path.display()),
    };
    if object.get("port").and_then(Value::as_u64) != Some(u64::from(PORT)) {
        bail!("{} must use port {PORT}", path.display());
    }
    let socket = object
        .get("socket")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|p| panel_socket(p))
        .ok_or_else(|| anyhow!("{} must name data/panel/agent.sock", path.display()))?;
    let token_file = object
        .get("token_file")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|p| token_file(p))
        .ok_or_else(|| anyhow!("{} must name secrets/panel/hermes.token", path.display()))?;

    Ok(Config {
        cid,
        port: PORT,
        socket,
        token_file,
    })
}

/// Python's `_panel_socket`: an absolute path ending `data/panel/agent.sock`.
fn panel_socket(path: &Path) -> bool {
    path.is_absolute()
        && path.file_name() == Some(OsStr::new("agent.sock"))
        && parent_name(path, 1) == Some("panel")
        && parent_name(path, 2) == Some("data")
}

/// Python's `_token_file`: an absolute path ending `secrets/panel/hermes.token`.
fn token_file(path: &Path) -> bool {
    path.is_absolute()
        && path.file_name() == Some(OsStr::new("hermes.token"))
        && parent_name(path, 1) == Some("panel")
        && parent_name(path, 2) == Some("secrets")
}

/// The name of the ancestor `levels` up, if it has one.
fn parent_name(path: &Path, levels: usize) -> Option<&str> {
    let mut current = path;
    for _ in 0..levels {
        current = current.parent()?;
    }
    current.file_name()?.to_str()
}

/// Python's `load_token`. Reads the one-line bearer, mode 0600. The error text
/// names the file. It never carries the token.
pub fn load_token(path: &Path) -> Result<String> {
    if !token_file(path) {
        bail!("{} is not the hermes token file", path.display());
    }
    check_mode(path)?;
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let token = text.strip_suffix('\n').unwrap_or(&text);
    // Tighter than the Python: the token must be printable ASCII with no
    // space. The Python would load a non-ASCII token and fail on it later.
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
        bail!("{} has a bad token", path.display());
    }
    Ok(token.to_string())
}
