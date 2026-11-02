//! What the panel checks before it binds: the flags, the password file, and the
//! house LAN address. Port of the startup half of `app/h7020_panel.py`.

use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// The access point's subnet. The panel never binds here, and the cams live here.
pub const IOT_OCTETS: [u8; 3] = [10, 87, 0];
pub const DEFAULT_PORT: u16 = 8765;
/// The H7020's BLE address, as `tools/govee_h7020.py` has it.
pub const DEFAULT_ADDRESS: &str = "C5:38:30:33:3E:4A";

/// Flags for `tallowd panel`. The Python's argparse names, with the same defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct Args {
    pub host: Option<String>,
    pub port: u16,
    pub address: String,
    pub adapter: Option<String>,
    pub scan_seconds: f64,
    pub timeout: f64,
    pub strings: Option<u8>,
    pub preview: bool,
    pub password_file: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub agents_file: Option<PathBuf>,
    /// `--mesh auto|off`. `None` is the default, which is `auto` on the real panel.
    pub mesh: Option<bool>,
    /// `--mesh-port`: the root board's port, in place of finding it.
    pub mesh_port: Option<PathBuf>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            host: None,
            port: DEFAULT_PORT,
            address: DEFAULT_ADDRESS.to_owned(),
            adapter: None,
            scan_seconds: 12.0,
            timeout: 20.0,
            strings: None,
            preview: false,
            password_file: None,
            data_dir: None,
            agents_file: None,
            mesh: None,
            mesh_port: None,
        }
    }
}

/// Reads the flags the way argparse does for these options. `--flag value` and
/// `--flag=value` both work. A bad flag or value is a usage error.
pub fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut args = Args::default();
    let mut index = 0;
    while index < argv.len() {
        let raw = &argv[index];
        index += 1;
        let (flag, inline) = match raw.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_owned())),
            _ => (raw.as_str(), None),
        };
        if flag == "--preview" {
            if inline.is_some() {
                return Err(format!("{flag} takes no value"));
            }
            args.preview = true;
            continue;
        }
        let known = [
            "--host",
            "--port",
            "--address",
            "--adapter",
            "--scan-seconds",
            "--timeout",
            "--strings",
            "--password-file",
            "--data-dir",
            "--agents-file",
            "--mesh",
            "--mesh-port",
        ];
        if !known.contains(&flag) {
            return Err(format!("unrecognized arguments: {raw}"));
        }
        let value = match inline {
            Some(value) => value,
            None => {
                let next = argv
                    .get(index)
                    .ok_or_else(|| format!("argument {flag}: expected one argument"))?;
                index += 1;
                next.clone()
            }
        };
        match flag {
            "--host" => args.host = Some(value),
            "--port" => {
                args.port = value
                    .parse()
                    .map_err(|_| format!("argument --port: invalid int value: '{value}'"))?;
            }
            "--address" => args.address = value,
            "--adapter" => args.adapter = Some(value),
            "--scan-seconds" => {
                args.scan_seconds = value.parse().map_err(|_| {
                    format!("argument --scan-seconds: invalid float value: '{value}'")
                })?;
            }
            "--timeout" => {
                args.timeout = value
                    .parse()
                    .map_err(|_| format!("argument --timeout: invalid float value: '{value}'"))?;
            }
            "--strings" => {
                let parsed: u8 = value
                    .parse()
                    .map_err(|_| format!("argument --strings: invalid choice: '{value}'"))?;
                if !(parsed == 1 || parsed == 2) {
                    return Err(format!(
                        "argument --strings: invalid choice: {parsed} (choose from 1, 2)"
                    ));
                }
                args.strings = Some(parsed);
            }
            "--password-file" => args.password_file = Some(PathBuf::from(value)),
            "--data-dir" => args.data_dir = Some(PathBuf::from(value)),
            "--agents-file" => args.agents_file = Some(PathBuf::from(value)),
            "--mesh" => {
                args.mesh = Some(match value.as_str() {
                    "auto" => true,
                    "off" => false,
                    _ => {
                        return Err(format!(
                            "argument --mesh: invalid choice: '{value}' (choose from auto, off)"
                        ))
                    }
                });
            }
            "--mesh-port" => {
                if !value.starts_with("/dev/") || value.contains("..") {
                    return Err(format!(
                        "argument --mesh-port: '{value}' is not a device path under /dev"
                    ));
                }
                args.mesh_port = Some(PathBuf::from(value));
            }
            _ => unreachable!("every known flag is matched above"),
        }
    }
    Ok(args)
}

/// Reads the panel password. The file must be mode 0600 and hold one non-empty line.
pub fn load_password(path: &Path) -> Result<String, String> {
    if !path.is_file() {
        return Err("Missing panel password. Create it, then run again:\n\
             \x20 install -d -m 0700 secrets/panel\n\
             \x20 umask 077 && printf '%s\\n' 'your-password' > secrets/panel/password\n\
             \x20 chmod 0600 secrets/panel/password"
            .to_owned());
    }
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o600 {
        return Err(format!(
            "{} is mode {mode:04o}. chmod 0600 {}",
            path.display(),
            path.display()
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let password = text.trim_matches('\n');
    if password.is_empty() || password.contains('\n') {
        return Err(format!("{} must be one non-empty line.", path.display()));
    }
    Ok(password.to_owned())
}

/// Refuses an unspecified address, anything on the access point, and IPv6.
pub fn reject_bind_host(host: &str) -> Option<String> {
    let text = host.trim();
    if matches!(text, "0.0.0.0" | "::" | "::0") {
        return Some("Refusing to bind 0.0.0.0.".to_owned());
    }
    let Ok(ip) = text.parse::<IpAddr>() else {
        return Some(format!("{text} is not an IP address."));
    };
    let v4 = match ip {
        IpAddr::V6(_) => return Some("Pass an IPv4 house LAN address.".to_owned()),
        IpAddr::V4(v4) => v4,
    };
    if v4.is_unspecified() {
        return Some("Refusing to bind an unspecified address.".to_owned());
    }
    if on_access_point(v4) {
        return Some(format!(
            "Refusing {text}: that address is on the IoT access point (10.87.0.0/24)."
        ));
    }
    None
}

fn on_access_point(ip: Ipv4Addr) -> bool {
    ip.octets()[..3] == IOT_OCTETS
}

fn usable_lan(host: &str) -> bool {
    if reject_bind_host(host).is_some() {
        return false;
    }
    match host.parse::<Ipv4Addr>() {
        Ok(ip) => !(ip.is_loopback() || ip.is_link_local()),
        Err(_) => false,
    }
}

/// Picks one house LAN address. An explicit `--host` must pass the bind rules. Otherwise
/// the default route's source wins when it is outside the access point.
pub fn choose_host(
    explicit: Option<&str>,
    route_source: Option<&str>,
    locals: &[String],
) -> Result<String, String> {
    if let Some(host) = explicit {
        if let Some(reason) = reject_bind_host(host) {
            return Err(reason);
        }
        return Ok(host.to_owned());
    }
    if let Some(source) = route_source.filter(|source| usable_lan(source)) {
        return Ok(source.to_owned());
    }
    let mut options: Vec<&str> = Vec::new();
    for addr in locals {
        if usable_lan(addr) && !options.contains(&addr.as_str()) {
            options.push(addr);
        }
    }
    match options.as_slice() {
        [only] => Ok((*only).to_owned()),
        [] => Err("No house LAN address to bind. Pass --host.".to_owned()),
        many => Err(format!("Pass --host. Candidates: {}", many.join(", "))),
    }
}

/// The source address the kernel would use for the default route. Connecting a UDP
/// socket sends nothing.
pub fn default_route_ipv4() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

/// Every IPv4 address on the host, from `ip -4 -j addr`. Empty when `ip` is missing.
pub fn local_ipv4() -> Vec<String> {
    let Ok(output) = Command::new("ip").args(["-4", "-j", "addr"]).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(Value::Array(interfaces)) = serde_json::from_slice::<Value>(&output.stdout) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for iface in &interfaces {
        for info in iface
            .get("addr_info")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let family = info.get("family").and_then(Value::as_str);
            let local = info.get("local").and_then(Value::as_str);
            if let (Some("inet"), Some(local)) = (family, local) {
                found.push(local.to_owned());
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn the_mesh_flags_default_to_unset_and_take_auto_off_and_a_device_path() {
        assert_eq!(parse_args(&[]).expect("args").mesh, None);
        assert_eq!(
            parse_args(&strings(&["--mesh", "auto"]))
                .expect("args")
                .mesh,
            Some(true)
        );
        assert_eq!(
            parse_args(&strings(&["--mesh=off"])).expect("args").mesh,
            Some(false)
        );
        let args = parse_args(&strings(&["--mesh-port", "/dev/ttyUSB1"])).expect("args");
        assert_eq!(args.mesh_port, Some(PathBuf::from("/dev/ttyUSB1")));
        assert!(parse_args(&strings(&["--mesh", "on"])).is_err());
        assert!(parse_args(&strings(&["--mesh-port", "ttyUSB1"])).is_err());
        assert!(parse_args(&strings(&["--mesh-port", "/dev/../etc/passwd"])).is_err());
    }

    #[test]
    fn refuses_unspecified_and_the_access_point() {
        assert!(reject_bind_host("0.0.0.0").unwrap().contains("0.0.0.0"));
        assert!(reject_bind_host("10.87.0.1")
            .unwrap()
            .contains("10.87.0.0/24"));
        assert!(reject_bind_host("10.87.0.255").is_some());
        assert_eq!(reject_bind_host("192.168.1.20"), None);
        assert!(reject_bind_host("::1").unwrap().contains("IPv4"));
        assert!(reject_bind_host("not-an-address")
            .unwrap()
            .contains("not an IP address"));
    }

    #[test]
    fn a_default_route_outside_the_access_point_wins() {
        let host = choose_host(
            None,
            Some("192.168.1.20"),
            &strings(&["10.87.0.1", "192.168.1.20", "10.0.0.8"]),
        );
        assert_eq!(host.as_deref(), Ok("192.168.1.20"));
    }

    #[test]
    fn an_access_point_route_falls_through_to_the_only_lan_address() {
        let host = choose_host(
            None,
            Some("10.87.0.1"),
            &strings(&["10.87.0.1", "127.0.0.1", "192.168.86.4"]),
        );
        assert_eq!(host.as_deref(), Ok("192.168.86.4"));
    }

    #[test]
    fn several_lan_addresses_ask_for_host() {
        let error = choose_host(None, None, &strings(&["192.168.1.5", "192.168.1.6"])).unwrap_err();
        assert!(error.contains("192.168.1.5"));
        assert!(error.contains("--host"));
    }

    #[test]
    fn an_explicit_access_point_address_is_refused() {
        assert!(choose_host(Some("0.0.0.0"), Some("192.168.1.20"), &[]).is_err());
        assert!(choose_host(Some("10.87.0.9"), None, &[]).is_err());
        assert_eq!(
            choose_host(Some("192.168.1.9"), None, &[]).as_deref(),
            Ok("192.168.1.9")
        );
    }

    #[test]
    fn the_password_file_is_mode_0600_and_one_line() {
        let dir = std::env::temp_dir().join(format!("tallowd-setup-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("password");
        fs::write(&path, "secret\n").expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
        assert!(load_password(&path).unwrap_err().contains("chmod 0600"));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
        assert_eq!(load_password(&path).as_deref(), Ok("secret"));
        fs::write(&path, "one\ntwo\n").expect("write");
        assert!(load_password(&path)
            .unwrap_err()
            .contains("one non-empty line"));
        fs::write(&path, "\n\n").expect("write");
        assert!(load_password(&path).is_err());
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_missing_password_file_explains_how_to_create_it() {
        let error = load_password(Path::new("/tmp/tallowd-setup-missing-password")).unwrap_err();
        assert!(error.contains("chmod 0600"));
        assert!(error.starts_with("Missing panel password."));
    }

    #[test]
    fn flags_take_a_value_either_way_and_refuse_strangers() {
        let parsed = parse_args(&strings(&[
            "--port",
            "9000",
            "--host=192.168.1.9",
            "--preview",
        ]))
        .expect("valid flags");
        assert_eq!(parsed.port, 9000);
        assert_eq!(parsed.host.as_deref(), Some("192.168.1.9"));
        assert!(parsed.preview);
        assert!(parse_args(&strings(&["--strings", "3"]))
            .unwrap_err()
            .contains("choose from"));
        assert!(parse_args(&strings(&["--nope"])).is_err());
        assert!(parse_args(&strings(&["--port"])).is_err());
    }
}
