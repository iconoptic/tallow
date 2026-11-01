//! Bakes the mesh key and the OTA public key into the image. There is no default
//! for either: joining the mesh must never be open, and an update must never
//! verify against a key this source invented. A build without one stops here.
//! The OTA seed is not read.

use std::{env, fs, io::Write, os::unix::fs::OpenOptionsExt, path::Path};

fn main() {
    embuild::espidf::sysenv::output();
    let mesh_key = env::var("MESH_KEY_FILE").unwrap_or_else(|_| "/run/mesh-key".into());
    println!("cargo:rerun-if-env-changed=MESH_KEY_FILE");
    bake(
        &mesh_key,
        "mesh_key.bin",
        "the mesh key is 64 hex characters",
        &format!("No mesh key at {mesh_key}. Run: tools/mesh.sh key"),
    );
    let ota_pub = env::var("OTA_PUB_FILE").unwrap_or_else(|_| "/run/ota.pub".into());
    println!("cargo:rerun-if-env-changed=OTA_PUB_FILE");
    bake(
        &ota_pub,
        "ota_pub.bin",
        "the OTA public key is 64 hex characters",
        &format!("No OTA public key at {ota_pub}"),
    );
}

/// Decodes 64 hex characters into a 32-byte file at mode `0600`. The panic names
/// the path and does not include the file's bytes.
fn bake(path: &str, out_name: &str, length_note: &str, missing: &str) {
    println!("cargo:rerun-if-changed={path}");
    let text = fs::read_to_string(path).unwrap_or_else(|error| panic!("{missing} ({error})"));
    let text = text.trim();
    assert_eq!(text.len(), 64, "{length_note}");
    // Checked before slicing: a slice through a multi-byte character panics with
    // the whole string in its message, and `from_str_radix` takes a leading `+`.
    assert!(
        text.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{length_note}"
    );
    let bytes: Vec<u8> = (0..32)
        .map(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).expect("hex"))
        .collect();
    let out = Path::new(&env::var("OUT_DIR").unwrap()).join(out_name);
    let _ = fs::remove_file(&out);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&out)
        .and_then(|mut file| file.write_all(&bytes))
        .unwrap();
}
