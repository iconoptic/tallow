//! The Bluetooth link to one H7020, as a contract. Port of the transport half of
//! `tools/govee_h7020.py`, and the advertisement parse of the pinned codec.
//!
//! `Transport` is the only seam. `tallowd` implements it over BlueZ, and the tests
//! implement it with a fake. Nothing in this module touches Bluetooth: the adapter
//! lookup reads a path, and the advertisement parse reads bytes. The module sits
//! behind the `ble` feature, so the codec builds without it.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::time::Duration;

/// The TP-Link UB500 is the host seed. Its hci index moves, so it is found by this id.
pub const SEED_USB: (&str, &str) = ("2357", "0604");
/// The write characteristic on the light's service.
pub const WRITE_UUID: &str = "00010203-0405-0607-0809-0a0b0c0d2b11";
/// The notify characteristic on the light's service.
pub const READ_UUID: &str = "00010203-0405-0607-0809-0a0b0c0d2b10";
/// What `seed_adapter` answers when no UB500 is attached. Python falls back to it too.
pub const FALLBACK_ADAPTER: &str = "hci0";

/// What a scan saw of the light: its name and its manufacturer entries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Seen {
    pub name: Option<String>,
    pub manufacturer_data: BTreeMap<u16, Vec<u8>>,
}

/// One link to one light. Errors are the text to show, without a class name.
pub trait Transport: Send {
    /// Listens for `window` and returns what the light advertised, or None when it did not show.
    fn scan(
        &mut self,
        window: Duration,
    ) -> impl Future<Output = Result<Option<Seen>, String>> + Send;

    /// Connects within `timeout` and subscribes to the notify characteristic.
    fn connect(&mut self, timeout: Duration) -> impl Future<Output = Result<(), String>> + Send;

    /// Writes one frame without waiting for a reply.
    fn write(&mut self, frame: &[u8]) -> impl Future<Output = Result<(), String>> + Send;

    /// The next notification, or None once the link has dropped. With no link it waits.
    fn next_notification(&mut self) -> impl Future<Output = Option<Vec<u8>>> + Send;

    /// Drops the link. Safe to call when there is none.
    fn disconnect(&mut self) -> impl Future<Output = ()> + Send;
}

/// What the light's advertisement says: its colour pact and whether the link is encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Advertisement {
    pub pact_type: u16,
    pub pact_code: u8,
    pub broadcast_version: u8,
    pub supports_encryption: bool,
}

/// Reads one manufacturer entry the way the pinned codec does. The company id's low
/// byte is the flags, then the magic `0x88 0xec`, a big-endian pact type, and a pact
/// code. Anything shorter, or with another magic, is not this light.
pub fn parse_advertisement(company: u16, payload: &[u8]) -> Option<Advertisement> {
    let mut data = Vec::with_capacity(2 + payload.len());
    data.extend_from_slice(&company.to_le_bytes());
    data.extend_from_slice(payload);
    if data.len() < 6 || data[1..3] != [0x88, 0xec] {
        return None;
    }
    Some(Advertisement {
        pact_type: u16::from_be_bytes([data[3], data[4]]),
        pact_code: data[5],
        broadcast_version: data[0] & 0x0f,
        supports_encryption: data[0] & 0x40 != 0,
    })
}

/// The advertisement in a set of manufacturer entries. An encrypted one wins at once,
/// as the Python does, and otherwise the last one that parses.
pub fn advertisement_of(manufacturer: &BTreeMap<u16, Vec<u8>>) -> Option<Advertisement> {
    let mut found = None;
    for (&company, payload) in manufacturer {
        if let Some(advert) = parse_advertisement(company, payload) {
            if advert.supports_encryption {
                return Some(advert);
            }
            found = Some(advert);
        }
    }
    found
}

/// The hci name of the UB500 under `root`, which is `/sys/class/bluetooth` in use. An
/// adapter matches when its `device/../` directory has the seed's vendor and product id.
/// Names are tried in order, and `hci0` answers when none matches or the root is absent.
pub fn seed_adapter_in(root: &Path) -> String {
    let (vendor, product) = SEED_USB;
    let Ok(entries) = std::fs::read_dir(root) else {
        return FALLBACK_ADAPTER.to_owned();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with("hci"))
        .collect();
    names.sort();
    for name in names {
        let usb = root.join(&name).join("device").join("..");
        let read = |file: &str| {
            std::fs::read_to_string(usb.join(file))
                .map(|text| text.trim().to_owned())
                .ok()
        };
        if read("idVendor").as_deref() == Some(vendor)
            && read("idProduct").as_deref() == Some(product)
        {
            return name;
        }
    }
    FALLBACK_ADAPTER.to_owned()
}

/// The seed adapter, as the running kernel shows it.
pub fn seed_adapter() -> String {
    seed_adapter_in(Path::new("/sys/class/bluetooth"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake `/sys/class/bluetooth`: each `hciN` has a `device/` directory, so its
    /// `device/..` is the `hciN` directory itself, which holds the USB ids.
    fn tree(label: &str, adapters: &[(&str, &str, &str)]) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("tallow-govee-ble-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (name, vendor, product) in adapters {
            let dir = root.join(name);
            std::fs::create_dir_all(dir.join("device")).expect("device dir");
            std::fs::write(dir.join("idVendor"), format!("{vendor}\n")).expect("vendor");
            std::fs::write(dir.join("idProduct"), format!("{product}\n")).expect("product");
        }
        root
    }

    #[test]
    fn the_advertisement_gives_the_pact_and_the_encryption_flag_from_its_bytes() {
        let advert = parse_advertisement(0x8802, &[0xec, 0x00, 0x01, 0x01, 0x01]).expect("a Govee");
        assert_eq!(
            advert,
            Advertisement {
                pact_type: 1,
                pact_code: 1,
                broadcast_version: 2,
                supports_encryption: false,
            }
        );
        let sealed = parse_advertisement(0x8842, &[0xec, 0x00, 0x02, 0x01]).expect("a Govee");
        assert!(sealed.supports_encryption);
        assert_eq!((sealed.pact_type, sealed.pact_code), (2, 1));
    }

    #[test]
    fn an_advertisement_with_another_magic_or_too_short_is_not_this_light() {
        assert_eq!(parse_advertisement(0x8802, &[0x00, 0x00, 0x01, 0x01]), None);
        assert_eq!(parse_advertisement(0x1234, &[0xec, 0x00, 0x01, 0x01]), None);
        assert_eq!(parse_advertisement(0x8802, &[0xec, 0x00]), None);
    }

    #[test]
    fn an_encrypted_advertisement_wins_over_a_plain_one() {
        let mut manufacturer = BTreeMap::new();
        manufacturer.insert(0x8802, vec![0xec, 0x00, 0x01, 0x01, 0x01]);
        manufacturer.insert(0x8842, vec![0xec, 0x00, 0x02, 0x01]);
        let advert = advertisement_of(&manufacturer).expect("an advertisement");
        assert!(advert.supports_encryption);
        assert_eq!((advert.pact_type, advert.pact_code), (2, 1));
        assert_eq!(advertisement_of(&BTreeMap::new()), None);
    }

    #[test]
    fn the_seed_adapter_is_found_by_usb_id_not_by_hci_number() {
        let root = tree(
            "seed",
            &[("hci0", "8087", "0026"), ("hci1", "2357", "0604")],
        );
        assert_eq!(seed_adapter_in(&root), "hci1");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn with_no_seed_adapter_the_answer_is_hci0() {
        let root = tree(
            "none",
            &[("hci0", "8087", "0026"), ("hci1", "2357", "0605")],
        );
        assert_eq!(seed_adapter_in(&root), FALLBACK_ADAPTER);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            seed_adapter_in(Path::new("/tallow-no-such-bluetooth-root")),
            FALLBACK_ADAPTER
        );
    }
}
