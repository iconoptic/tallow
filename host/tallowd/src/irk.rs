//! Resolve a resolvable private address to the device that made it, from the
//! device's identity resolving key (IRK). The function is `ah` in Bluetooth
//! Core Vol 3 Part H 2.2.2.
//!
//! Byte order is as the spec writes it, most significant byte first. The IRK
//! is the AES-128 key as written. The block is 13 zero bytes and then `prand`.
//! The 24-bit hash is the last three bytes of the output. In an address the
//! first three bytes are `prand` and the last three are the hash.
//!
//! Keys are read on the host only. Nothing here goes to a node.

use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;
use anyhow::{anyhow, bail, Context};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tallow_mesh_core::sightings::Kind;

/// Where `run` reads the keys when `--irks` is not given.
pub const DEFAULT_PATH: &str = "data/mesh/irks.toml";

/// One identity resolving key. Its `Debug` is redacted, so a stray `{:?}`
/// cannot print it.
pub struct Irk([u8; 16]);

impl fmt::Debug for Irk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Irk(<redacted>)")
    }
}

/// The keys, by device name, in the order the file lists them.
#[derive(Debug, Default)]
pub struct Keys {
    devices: Vec<(String, Irk)>,
}

/// The `ah` function: the 24-bit hash of `prand` under `irk`.
pub fn ah(irk: &Irk, prand: &[u8; 3]) -> [u8; 3] {
    let output = ah_block(irk, prand);
    [output[13], output[14], output[15]]
}

/// The whole AES output, which `ah` keeps the last three bytes of.
fn ah_block(irk: &Irk, prand: &[u8; 3]) -> [u8; 16] {
    let cipher = Aes128::new(&irk.0.into());
    let mut block: aes::cipher::Block<Aes128> = [0u8; 16].into();
    block[13..].copy_from_slice(prand);
    cipher.encrypt_block(&mut block);
    let mut out = [0u8; 16];
    out.copy_from_slice(&block);
    out
}

/// Compares every byte whatever the first difference is.
fn same_hash(a: &[u8; 3], b: &[u8; 3]) -> bool {
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

impl Keys {
    /// Read the keys at `path`. A missing file means no keys. The file must be
    /// mode 0600. The error names the path and the mode, never the contents.
    pub fn load(path: &Path) -> anyhow::Result<Keys> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Keys::default()),
            Err(err) => return Err(err).with_context(|| format!("open {}", path.display())),
        };
        let mode = file
            .metadata()
            .with_context(|| format!("stat {}", path.display()))?
            .permissions()
            .mode()
            & 0o777;
        if mode != 0o600 {
            bail!(
                "{} is mode {mode:o}; it must be 600. Run chmod 600 {}",
                path.display(),
                path.display()
            );
        }
        let mut text = String::new();
        file.read_to_string(&mut text)
            .with_context(|| format!("read {}", path.display()))?;
        // One flat message, so a plain `to_string` shows the device name too.
        Keys::parse(&text).map_err(|err| anyhow!("{}: {err:#}", path.display()))
    }

    /// The file is a table from device name to 32 hex digits.
    fn parse(text: &str) -> anyhow::Result<Keys> {
        // The TOML error can quote the line it failed on, which holds a key.
        // Only its position goes in the message.
        let table: BTreeMap<String, String> = toml::from_str(text).map_err(|err| {
            match err.span().and_then(|span| text.get(..span.start)) {
                Some(before) => {
                    anyhow!(
                        "not valid TOML at line {}",
                        before.matches('\n').count() + 1
                    )
                }
                None => anyhow!("not valid TOML"),
            }
        })?;
        let mut devices = Vec::with_capacity(table.len());
        for (name, hex) in table {
            let irk = parse_irk(&hex)
                .ok_or_else(|| anyhow!("the key for {name} must be 32 hex digits"))?;
            devices.push((name, irk));
        }
        Ok(Keys { devices })
    }

    /// The device whose IRK makes `addr` resolve. Only a `ble-random` address
    /// whose top two bits are `01` is tried. Any other address resolves to nothing.
    pub fn resolve(&self, addr: &[u8; 6], kind: Option<Kind>) -> Option<&str> {
        if kind != Some(Kind::BleRandom) || addr[0] & 0xC0 != 0x40 {
            return None;
        }
        let prand = [addr[0], addr[1], addr[2]];
        let hash = [addr[3], addr[4], addr[5]];
        self.devices
            .iter()
            .find(|(_, irk)| same_hash(&ah(irk, &prand), &hash))
            .map(|(name, _)| name.as_str())
    }
}

/// 32 hex digits, either case, as 16 bytes in the order written.
pub(crate) fn parse_irk(text: &str) -> Option<Irk> {
    let digits = text.as_bytes();
    if digits.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (byte, pair) in out.iter_mut().zip(digits.chunks_exact(2)) {
        *byte = (hex_digit(pair[0])? << 4) | hex_digit(pair[1])?;
    }
    Some(Irk(out))
}

fn hex_digit(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// The spec sample: IRK, prand, and hash, all as written.
    const SPEC_IRK: &str = "ec0234a357c8ad05341010a60a397d9b";
    const SPEC_PRAND: [u8; 3] = [0x70, 0x81, 0x94];
    const SPEC_HASH: [u8; 3] = [0x0d, 0xfb, 0xaa];
    const SPEC_CIPHERTEXT: &str = "159d5fb72ebe2311a48c1bdcc40dfbaa";

    /// A key made up for these tests. It is not any device's key.
    const MADE_UP_IRK: &str = "6d6164652d75702d69726b2d30303031";

    fn irk(hex: &str) -> Irk {
        parse_irk(hex).expect("test key is 32 hex digits")
    }

    fn hex_text(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// An address, most significant byte first, with `prand` as its first three
    /// bytes and the hash `irk` gives for it as its last three.
    fn address_for(irk: &Irk, prand: [u8; 3]) -> [u8; 6] {
        let hash = ah(irk, &prand);
        [prand[0], prand[1], prand[2], hash[0], hash[1], hash[2]]
    }

    fn temp_file(name: &str, contents: &str, mode: u32) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tallowd-irk-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(name);
        fs::write(&path, contents).expect("write keys");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod keys");
        path
    }

    #[test]
    fn ah_matches_the_spec_sample_hash() {
        assert_eq!(ah(&irk(SPEC_IRK), &SPEC_PRAND), SPEC_HASH);
    }

    #[test]
    fn ah_matches_the_spec_sample_full_ciphertext() {
        assert_eq!(
            hex_text(&ah_block(&irk(SPEC_IRK), &SPEC_PRAND)),
            SPEC_CIPHERTEXT
        );
    }

    #[test]
    fn the_hash_comparison_fails_on_a_difference_at_either_end() {
        assert!(same_hash(&SPEC_HASH, &SPEC_HASH));
        assert!(!same_hash(&SPEC_HASH, &[0x00, 0xfb, 0xaa]));
        assert!(!same_hash(&SPEC_HASH, &[0x0d, 0xfb, 0x00]));
    }

    #[test]
    fn a_whole_address_resolves_to_the_device_whose_irk_made_it() {
        let keys = Keys {
            devices: vec![("Spec light".into(), irk(SPEC_IRK))],
        };
        // The spec prand and hash, laid out as one six-byte address.
        let address = [0x70, 0x81, 0x94, 0x0d, 0xfb, 0xaa];
        assert_eq!(
            keys.resolve(&address, Some(Kind::BleRandom)),
            Some("Spec light")
        );
    }

    #[test]
    fn two_addresses_from_one_irk_resolve_to_one_name() {
        let made_up = irk(MADE_UP_IRK);
        let first = address_for(&made_up, [0x41, 0x22, 0x13]);
        let second = address_for(&made_up, [0x5c, 0x9e, 0x07]);
        assert_ne!(first, second);
        let keys = Keys {
            devices: vec![("Earbuds".into(), made_up)],
        };
        assert_eq!(keys.resolve(&first, Some(Kind::BleRandom)), Some("Earbuds"));
        assert_eq!(
            keys.resolve(&second, Some(Kind::BleRandom)),
            Some("Earbuds")
        );
    }

    #[test]
    fn a_public_or_classic_address_starting_40_to_7f_never_resolves() {
        let made_up = irk(MADE_UP_IRK);
        let address = address_for(&made_up, [0x41, 0x22, 0x13]);
        let keys = Keys {
            devices: vec![("Earbuds".into(), made_up)],
        };
        assert_eq!(keys.resolve(&address, Some(Kind::Classic)), None);
        assert_eq!(keys.resolve(&address, Some(Kind::BlePublic)), None);
        assert_eq!(keys.resolve(&address, None), None);
    }

    #[test]
    fn a_random_address_whose_top_bits_are_not_01_never_resolves() {
        let made_up = irk(MADE_UP_IRK);
        // Top bits 11, which is a static random address, with a correct hash.
        let address = address_for(&made_up, [0xc1, 0x22, 0x13]);
        let keys = Keys {
            devices: vec![("Earbuds".into(), made_up)],
        };
        assert_eq!(keys.resolve(&address, Some(Kind::BleRandom)), None);
    }

    #[test]
    fn an_address_that_matches_no_key_resolves_to_nothing() {
        let keys = Keys {
            devices: vec![("Spec light".into(), irk(SPEC_IRK))],
        };
        let address = [0x41, 0x22, 0x13, 0x00, 0x00, 0x00];
        assert_eq!(keys.resolve(&address, Some(Kind::BleRandom)), None);
    }

    #[test]
    fn a_missing_key_file_means_no_keys_and_no_error() {
        let path = std::env::temp_dir().join("tallowd-irk-no-such-file.toml");
        let _ = fs::remove_file(&path);
        let keys = Keys::load(&path).expect("missing file is not an error");
        assert!(keys.devices.is_empty());
    }

    #[test]
    fn a_key_file_that_is_not_mode_0600_is_refused_and_named_without_its_contents() {
        let contents = format!("\"Spec light\" = \"{SPEC_IRK}\"\n");
        let path = temp_file("loose.toml", &contents, 0o644);
        let err = Keys::load(&path).expect_err("0644 is refused").to_string();
        assert!(err.contains(&path.display().to_string()), "{err}");
        assert!(err.contains("644"), "{err}");
        assert!(!err.contains(SPEC_IRK), "{err}");
        assert!(!err.contains("ec0234"), "{err}");
    }

    #[test]
    fn a_key_file_with_mode_0600_loads_and_resolves() {
        let contents = format!("\"Spec light\" = \"{SPEC_IRK}\"\n");
        let path = temp_file("tight.toml", &contents, 0o600);
        let keys = Keys::load(&path).expect("0600 loads");
        let address = [0x70, 0x81, 0x94, 0x0d, 0xfb, 0xaa];
        assert_eq!(
            keys.resolve(&address, Some(Kind::BleRandom)),
            Some("Spec light")
        );
    }

    #[test]
    fn a_key_of_the_wrong_length_is_an_error_that_names_the_device_not_the_bytes() {
        let contents = "\"Spec light\" = \"ec0234a357c8ad05\"\n";
        let path = temp_file("short.toml", contents, 0o600);
        let err = Keys::load(&path).expect_err("short key").to_string();
        assert!(err.contains("Spec light"), "{err}");
        assert!(!err.contains("ec0234"), "{err}");
    }

    #[test]
    fn a_key_that_is_not_hex_is_an_error_that_names_the_device_not_the_bytes() {
        let contents = "\"Spec light\" = \"zc0234a357c8ad05341010a60a397d9b\"\n";
        let path = temp_file("nothex.toml", contents, 0o600);
        let err = Keys::load(&path).expect_err("not hex").to_string();
        assert!(err.contains("Spec light"), "{err}");
        assert!(!err.contains("zc0234"), "{err}");
    }

    #[test]
    fn a_file_that_is_not_toml_is_an_error_that_does_not_quote_a_key() {
        let contents = format!("Spec light = {SPEC_IRK}\n");
        let path = temp_file("broken.toml", &contents, 0o600);
        let err = Keys::load(&path).expect_err("not TOML").to_string();
        assert!(!err.contains(SPEC_IRK), "{err}");
        assert!(!err.contains("ec0234"), "{err}");
    }

    #[test]
    fn a_key_never_prints_in_debug_output() {
        let made_up = irk(MADE_UP_IRK);
        let keys = Keys {
            devices: vec![("Earbuds".into(), made_up)],
        };
        let shown = format!("{keys:?}");
        assert!(shown.contains("Earbuds"));
        assert!(shown.contains("redacted"));
        assert!(!shown.contains(MADE_UP_IRK));
        assert!(!shown.contains("6d6164"));
    }
}
