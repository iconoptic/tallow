//! Points the build at ESP-IDF and bakes the Wi-Fi credentials, the OTA
//! public key, and the mesh key.
//!
//! The SSID and the password come from `firmware/cam/secrets.yaml`, mounted
//! at `/run/secrets.yaml`. A missing file or an empty value stops the build, and
//! so does any SSID but `tallow-iot`: a house SSID would put a cam on the LAN.
//! The messages name the file and the keys. They do not include the values.
//! The OTA public key is `/run/ota.pub`, 64 hex characters. The mesh key is
//! `/run/mesh-key`, 64 hex characters. A missing file or a bad length stops the
//! build and does not include the key. The OTA seed is not read.

use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const SECRETS_NOTE: &str =
    "firmware/cam/secrets.yaml is missing or has an empty wifi_ssid or wifi_password";
const SSID_NOTE: &str = "firmware/cam/secrets.yaml wifi_ssid must be tallow-iot";
const SSID: &str = "tallow-iot";
const OTA_NOTE: &str = "firmware/mesh/ota.pub is missing or is not 64 hex characters";
const MESH_NOTE: &str =
    "the mesh key is missing or is not 64 hex characters. Run: tools/mesh.sh key";

fn main() {
    embuild::espidf::sysenv::output();
    let path = env::var("WIFI_SECRETS_FILE").unwrap_or_else(|_| "/run/secrets.yaml".into());
    println!("cargo:rerun-if-env-changed=WIFI_SECRETS_FILE");
    println!("cargo:rerun-if-changed={path}");
    let text = fs::read_to_string(&path).unwrap_or_else(|_| panic!("{SECRETS_NOTE}"));
    let ssid = field(&text, "wifi_ssid").unwrap_or_default();
    let password = field(&text, "wifi_password").unwrap_or_default();
    if ssid.is_empty() || password.is_empty() {
        panic!("{SECRETS_NOTE}");
    }
    if ssid.len() > 32 || password.len() > 64 {
        panic!("firmware/cam/secrets.yaml wifi_ssid or wifi_password does not fit the Wi-Fi field");
    }
    if ssid != SSID {
        panic!("{SSID_NOTE}");
    }
    write_secret("wifi_ssid.bin", ssid.as_bytes());
    write_secret("wifi_password.bin", password.as_bytes());
    let mesh_key = env::var("MESH_KEY_FILE").unwrap_or_else(|_| "/run/mesh-key".into());
    println!("cargo:rerun-if-env-changed=MESH_KEY_FILE");
    bake_hex(&mesh_key, "mesh_key.bin", MESH_NOTE);
    let ota_pub = env::var("OTA_PUB_FILE").unwrap_or_else(|_| "/run/ota.pub".into());
    println!("cargo:rerun-if-env-changed=OTA_PUB_FILE");
    bake_hex(&ota_pub, "ota_pub.bin", OTA_NOTE);
}

/// The value of one `key:` line. Quoted values keep a `#` inside the quotes.
/// An unquoted value stops at a `#` comment.
fn field(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, raw)) = line.split_once(':') else {
            continue;
        };
        if name.trim() != key {
            continue;
        }
        return Some(unquote(raw.trim()));
    }
    None
}

fn unquote(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && (bytes[0] == b'"' || bytes[0] == b'\'') {
        let quote = bytes[0];
        if let Some(end) = raw.as_bytes()[1..].iter().position(|byte| *byte == quote) {
            return raw[1..1 + end].to_string();
        }
    }
    raw.split('#').next().unwrap_or(raw).trim().to_string()
}

/// Decodes 64 hex characters into 32 bytes at `name`. The panic is `note` and
/// does not include the file's text.
fn bake_hex(path: &str, name: &str, note: &str) {
    println!("cargo:rerun-if-changed={path}");
    let text = fs::read_to_string(path).unwrap_or_else(|_| panic!("{note}"));
    let text = text.trim();
    if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        panic!("{note}");
    }
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        let pair = &text[index * 2..index * 2 + 2];
        *byte = u8::from_str_radix(pair, 16).unwrap_or_else(|_| panic!("{note}"));
    }
    write_secret(name, &bytes);
}

/// Writes the bytes to `OUT_DIR` at mode `0600`. The file is replaced on rebuild.
fn write_secret(name: &str, bytes: &[u8]) {
    let out = Path::new(&env::var("OUT_DIR").unwrap()).join(name);
    let _ = fs::remove_file(&out);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&out)
        .and_then(|mut file| file.write_all(bytes))
        .unwrap();
}
