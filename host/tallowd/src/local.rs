//! The host's own Bluetooth adapter as a node of the map.
//!
//! Mesh boards reach the root over ESP-NOW, which an adapter cannot hear. So the adapter
//! is not the mesh's root: it is one more listener, reported under its own node id, with
//! rank 0 because it is wired to the host. It works with no root board, and beside one.
//!
//! BlueZ keeps one record per device with the last RSSI it saw. This polls those records,
//! so a sighting's `count` is the polls that found a live RSSI, not the adverts heard, and
//! its age is zero. Needs the adapter and bluetoothd, so only the pure parts are tested.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::Context;
use bluer::{Adapter, Address, AddressType, Session};
use tallow_mesh_core::sightings::Kind;

use crate::decode::{Named, Sighting};

const POLL: Duration = Duration::from_secs(2);
/// One poll talks to bluetoothd a few times per device. A stuck call ends the session.
const POLL_LIMIT: Duration = Duration::from_secs(20);
const RETRY: Duration = Duration::from_secs(10);

/// What one poll heard.
#[derive(Debug, PartialEq, Eq)]
pub struct Batch {
    pub node: String,
    pub uptime_s: u32,
    pub sightings: Vec<Sighting>,
    pub names: Vec<Named>,
}

/// What the scanner thread sends.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Batch(Batch),
    /// A line for stderr: the adapter is missing, or bluetoothd went away. It retries.
    Note(String),
}

/// The node id for an adapter: the last four bytes of its address, as eight hex digits
/// (the form `rooms.toml` takes).
pub fn node_id(adapter: &Address) -> String {
    adapter.0[2..]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn kind_of(address_type: AddressType) -> Kind {
    match address_type {
        AddressType::LePublic => Kind::BlePublic,
        AddressType::LeRandom => Kind::BleRandom,
        AddressType::BrEdr => Kind::Classic,
    }
}

/// Starts the scanner on its own thread and returns what it hears. The thread ends when
/// the receiver is dropped. `adapter` is an hci name, or `None` for the seed's.
pub fn start(adapter: Option<String>) -> Receiver<Event> {
    let (tx, rx) = mpsc::channel();
    let name = adapter.unwrap_or_else(tallow_govee::ble::seed_adapter);
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = tx.send(Event::Note(format!("local scan cannot start: {error}")));
                return;
            }
        };
        runtime.block_on(run(&name, &tx));
    });
    rx
}

async fn run(name: &str, tx: &Sender<Event>) {
    loop {
        if let Err(error) = session(name, tx).await {
            let note = format!("local scan on {name}: {error:#}");
            if tx.send(Event::Note(note)).is_err() {
                return;
            }
        }
        tokio::time::sleep(RETRY).await;
    }
}

/// Scans until BlueZ fails or the receiver is gone. A gone receiver is `Ok`, and the
/// caller then finds the next send failing and stops.
async fn session(name: &str, tx: &Sender<Event>) -> anyhow::Result<()> {
    let session = Session::new().await.context("connect to bluetoothd")?;
    let adapter = session.adapter(name).context("find the adapter")?;
    adapter.set_powered(true).await.context("power on")?;
    let node = node_id(
        &adapter
            .address()
            .await
            .context("read the adapter address")?,
    );
    // Discovery runs while the stream lives.
    let _discovery = adapter
        .discover_devices()
        .await
        .context("start discovery")?;
    let started = Instant::now();
    let mut named: HashMap<Address, String> = HashMap::new();
    loop {
        let (sightings, names) = tokio::time::timeout(POLL_LIMIT, poll(&adapter, &mut named))
            .await
            .context("bluetoothd stopped answering")??;
        let batch = Batch {
            node: node.clone(),
            uptime_s: u32::try_from(started.elapsed().as_secs()).unwrap_or(u32::MAX),
            sightings,
            names,
        };
        if tx.send(Event::Batch(batch)).is_err() {
            return Ok(());
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Every device BlueZ has a live RSSI for, and the names not yet sent.
async fn poll(
    adapter: &Adapter,
    named: &mut HashMap<Address, String>,
) -> anyhow::Result<(Vec<Sighting>, Vec<Named>)> {
    let mut sightings = Vec::new();
    let mut names = Vec::new();
    for address in adapter.device_addresses().await.context("list devices")? {
        let Ok(device) = adapter.device(address) else {
            continue;
        };
        // A device that went away between the list and now has no RSSI, and is skipped.
        let Ok(Some(rssi)) = device.rssi().await else {
            continue;
        };
        let Ok(address_type) = device.address_type().await else {
            continue;
        };
        let kind = kind_of(address_type);
        sightings.push(sighting(address.0, kind, rssi));
        let Ok(Some(name)) = device.name().await else {
            continue;
        };
        if named.get(&address) == Some(&name) {
            continue;
        }
        let company = device
            .manufacturer_data()
            .await
            .ok()
            .flatten()
            .and_then(|data| data.keys().min().copied());
        let class = device.class().await.ok().flatten().unwrap_or(0);
        named.insert(address, name.clone());
        names.push(Named {
            addr: address.0,
            kind: Some(kind),
            company,
            class,
            name,
        });
    }
    Ok((sightings, names))
}

/// One poll's sighting. RSSI is clamped to what a node's report holds.
fn sighting(addr: [u8; 6], kind: Kind, rssi: i16) -> Sighting {
    Sighting {
        addr,
        kind: Some(kind),
        rssi: i8::try_from(rssi.clamp(i16::from(i8::MIN), i16::from(i8::MAX))).unwrap_or(i8::MIN),
        count: 1,
        age_s: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_node_id_is_the_last_four_bytes_of_the_adapter_address() {
        let address = Address::new([0x23, 0x57, 0x06, 0x04, 0xAB, 0xCD]);
        assert_eq!(node_id(&address), "0604abcd");
    }

    #[test]
    fn bluez_address_types_map_to_the_mesh_kinds() {
        assert_eq!(kind_of(AddressType::LePublic), Kind::BlePublic);
        assert_eq!(kind_of(AddressType::LeRandom), Kind::BleRandom);
        assert_eq!(kind_of(AddressType::BrEdr), Kind::Classic);
    }

    #[test]
    fn rssi_is_clamped_to_a_byte() {
        assert_eq!(sighting([0; 6], Kind::Classic, -300).rssi, i8::MIN);
        assert_eq!(sighting([0; 6], Kind::Classic, 300).rssi, i8::MAX);
        assert_eq!(sighting([0; 6], Kind::Classic, -61).rssi, -61);
    }
}
