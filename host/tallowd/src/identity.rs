//! Which addresses belong to one device, from `data/mesh/devices.toml`.
//!
//! The file is an array of tables, one per device:
//!
//! ```toml
//! [[device]]
//! name = "Klipsch soundbar"
//! type = "speaker"
//! addresses = ["23:00:00:11:A0:38", "DC:00:00:11:A0:38"]
//! ```
//!
//! Addresses match case-insensitively and are stored upper-case, as the map
//! writes them. Nothing here is secret, unlike the keys in `irk.rs`.

use anyhow::{anyhow, bail, Context};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

/// Where `run` reads the devices when `--devices` is not given.
pub const DEFAULT_PATH: &str = "data/mesh/devices.toml";

/// One device the user named, with the addresses it answers on.
#[derive(Debug, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    pub kind: String,
    /// Upper-case, as the map writes them, with no duplicates.
    pub addresses: Vec<String>,
}

/// The named devices, indexed by address and by name.
#[derive(Debug, Default)]
pub struct Identities {
    devices: Vec<Device>,
    by_address: BTreeMap<String, usize>,
    by_name: BTreeMap<String, usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    device: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: toml::Spanned<String>,
    #[serde(rename = "type")]
    kind: String,
    addresses: Vec<toml::Spanned<String>>,
}

impl Identities {
    /// Read the devices at `path`. A missing file means no devices.
    pub fn load(path: &Path) -> anyhow::Result<Identities> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Identities::default()),
            Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
        };
        Identities::parse(&text).map_err(|err| anyhow!("{}: {err:#}", path.display()))
    }

    /// The file's text, parsed. Errors name the line, and a bad address names its device.
    pub(crate) fn parse(text: &str) -> anyhow::Result<Identities> {
        let file: File = toml::from_str(text).map_err(|err| match err.span() {
            Some(span) => anyhow!("line {}: {}", line_at(text, span.start), err.message()),
            None => anyhow!("{}", err.message()),
        })?;
        let mut identities = Identities::default();
        for entry in file.device {
            let line = line_at(text, entry.name.span().start);
            let name = entry.name.into_inner();
            if identities.by_name.contains_key(&name) {
                bail!("line {line}: device {name} is listed twice");
            }
            let index = identities.devices.len();
            let mut addresses: Vec<String> = Vec::new();
            for listed in entry.addresses {
                let line = line_at(text, listed.span().start);
                let Some(address) = normal_address(listed.get_ref()) else {
                    bail!(
                        "line {line}: device {name} has address {} that is not six hex pairs like 23:00:00:11:A0:38",
                        listed.get_ref()
                    );
                };
                if let Some(&other) = identities.by_address.get(&address) {
                    if other != index {
                        bail!(
                            "line {line}: address {address} is listed under {} and {name}",
                            identities.devices[other].name
                        );
                    }
                    continue;
                }
                identities.by_address.insert(address.clone(), index);
                addresses.push(address);
            }
            identities.by_name.insert(name.clone(), index);
            identities.devices.push(Device {
                name,
                kind: entry.kind,
                addresses,
            });
        }
        Ok(identities)
    }

    /// The named devices, in the order the file lists them.
    pub fn devices(&self) -> &[Device] {
        &self.devices
    }

    /// The index of the device that lists `address`, matched case-insensitively.
    pub fn of_address(&self, address: &str) -> Option<usize> {
        self.by_address.get(&address.to_ascii_uppercase()).copied()
    }

    /// The index of the device with this name.
    pub fn named(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }
}

/// Six hex pairs joined by colons, either case, as upper-case text.
fn normal_address(text: &str) -> Option<String> {
    let pairs: Vec<&str> = text.split(':').collect();
    let well_formed = pairs.len() == 6
        && pairs
            .iter()
            .all(|pair| pair.len() == 2 && pair.bytes().all(|b| b.is_ascii_hexdigit()));
    well_formed.then(|| text.to_ascii_uppercase())
}

/// The 1-based line that `offset` falls on.
fn line_at(text: &str, offset: usize) -> usize {
    text.get(..offset).unwrap_or(text).matches('\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    const KLIPSCH: &str = "[[device]]\nname = \"Klipsch soundbar\"\ntype = \"speaker\"\naddresses = [\"23:00:00:11:A0:38\", \"dc:00:00:11:a0:38\"]\n";

    /// A file in a directory of its own, named for `test`, for the duration of `check`.
    fn with_file<T>(test: &str, text: &str, check: impl FnOnce(&Path) -> T) -> T {
        let dir =
            std::env::temp_dir().join(format!("tallowd-identity-{test}-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("devices.toml");
        fs::write(&path, text).expect("write devices");
        let out = check(&path);
        fs::remove_dir_all(&dir).expect("clean up");
        out
    }

    fn parse_error(text: &str) -> String {
        format!("{:#}", Identities::parse(text).expect_err("should refuse"))
    }

    #[test]
    fn a_missing_devices_file_means_no_devices_and_no_error() {
        let path = std::env::temp_dir().join("tallowd-identity-no-such-file-devices.toml");
        let identities = Identities::load(&path).expect("missing is not an error");
        assert!(identities.devices().is_empty());
    }

    #[test]
    fn an_address_matches_whatever_the_case_it_is_written_in() {
        let identities = Identities::parse(KLIPSCH).expect("parse");
        assert_eq!(identities.of_address("dc:00:00:11:a0:38"), Some(0));
        assert_eq!(identities.of_address("DC:00:00:11:A0:38"), Some(0));
        assert_eq!(identities.devices()[0].addresses[1], "DC:00:00:11:A0:38");
        assert_eq!(identities.devices()[0].kind, "speaker");
        assert_eq!(identities.named("Klipsch soundbar"), Some(0));
    }

    #[test]
    fn a_file_read_from_disk_loads_the_devices_it_names() {
        let identities = with_file("from-disk", KLIPSCH, |path| {
            Identities::load(path).expect("load")
        });
        assert_eq!(identities.devices().len(), 1);
    }

    #[test]
    fn an_address_under_two_devices_is_an_error_that_names_both_and_the_line() {
        let text = format!(
            "{KLIPSCH}\n[[device]]\nname = \"Other\"\ntype = \"speaker\"\naddresses = [\"DC:00:00:11:A0:38\"]\n"
        );
        let message = parse_error(&text);
        assert!(message.contains("line 9"), "{message}");
        assert!(message.contains("DC:00:00:11:A0:38"), "{message}");
        assert!(message.contains("Klipsch soundbar"), "{message}");
        assert!(message.contains("Other"), "{message}");
    }

    #[test]
    fn a_bad_address_is_an_error_that_names_its_device_and_line() {
        let text =
            "[[device]]\nname = \"Lamp\"\ntype = \"light\"\naddresses = [\"23:00:00:11:A0\"]\n";
        let message = parse_error(text);
        assert!(message.contains("line 4"), "{message}");
        assert!(message.contains("device Lamp"), "{message}");
        assert!(message.contains("23:00:00:11:A0"), "{message}");
    }

    #[test]
    fn a_misspelt_field_is_refused_with_its_line() {
        let text = "[[device]]\nname = \"Lamp\"\ntype = \"light\"\naddress = []\n";
        let message = parse_error(text);
        assert!(message.starts_with("line "), "{message}");
        assert!(message.contains("unknown field"), "{message}");
    }

    #[test]
    fn a_device_listed_twice_by_name_is_refused() {
        let text = format!("{KLIPSCH}\n{KLIPSCH}");
        assert!(parse_error(&text).contains("listed twice"));
    }

    #[test]
    fn a_device_with_no_addresses_is_kept_so_a_resolved_name_can_group_under_it() {
        let text = "[[device]]\nname = \"Earbuds\"\ntype = \"earbuds\"\naddresses = []\n";
        let identities = Identities::parse(text).expect("parse");
        assert_eq!(identities.named("Earbuds"), Some(0));
        assert!(identities.devices()[0].addresses.is_empty());
    }

    #[test]
    fn the_same_address_twice_under_one_device_is_kept_once() {
        let text = "[[device]]\nname = \"Lamp\"\ntype = \"light\"\naddresses = [\"AA:AA:AA:AA:AA:AA\", \"aa:aa:aa:aa:aa:aa\"]\n";
        let identities = Identities::parse(text).expect("parse");
        assert_eq!(identities.devices()[0].addresses, ["AA:AA:AA:AA:AA:AA"]);
    }

    #[test]
    fn a_broken_file_is_an_error_that_names_the_path_and_the_line() {
        let message = with_file("broken", "[[device]]\nname = \n", |path| {
            format!("{:#}", Identities::load(path).expect_err("refused"))
        });
        assert!(message.contains("devices.toml"), "{message}");
        assert!(message.contains("line 2"), "{message}");
    }
}
