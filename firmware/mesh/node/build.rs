//! Bakes the mesh key into the image. There is no default key: joining the mesh
//! must never be open, so a build without one stops here.

use std::{env, fs, path::Path};

fn main() {
    embuild::espidf::sysenv::output();
    let path = env::var("MESH_KEY_FILE").unwrap_or_else(|_| "/run/mesh-key".into());
    println!("cargo:rerun-if-env-changed=MESH_KEY_FILE");
    println!("cargo:rerun-if-changed={path}");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("No mesh key at {path} ({error}). Run: tools/mesh.sh key"));
    let text = text.trim();
    assert_eq!(text.len(), 64, "the mesh key is 64 hex characters");
    let key: Vec<u8> = (0..32)
        .map(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).expect("hex"))
        .collect();
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("mesh_key.bin"),
        key,
    )
    .unwrap();
}
