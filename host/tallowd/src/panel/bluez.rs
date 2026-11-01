//! The BlueZ link to one H7020, through `bluer`. The only code in tallowd that touches
//! `bluer`. It implements `tallow_govee::ble::Transport` and holds no behaviour: the
//! session above it decides when to scan, connect, write, and redial.
//!
//! It needs the adapter and the light, so nothing here runs in the tests. It is
//! compiled, and it is run on the real H7020 by the user (see the report for T3.7).

use std::collections::BTreeMap;
use std::future::pending;
use std::pin::Pin;
use std::time::Duration;

use bluer::gatt::remote::{Characteristic, CharacteristicWriteRequest};
use bluer::gatt::WriteOp;
use bluer::{Adapter, Address, Device, DeviceEvent, DeviceProperty, Session, Uuid};
use futures_util::{Stream, StreamExt};
use tallow_govee::ble::{Seen, Transport, READ_UUID, WRITE_UUID};

type Notes = Pin<Box<dyn Stream<Item = Vec<u8>> + Send>>;
type Events = Pin<Box<dyn Stream<Item = DeviceEvent> + Send>>;

/// One link to the light over BlueZ on the named adapter.
pub struct BluerTransport {
    address: Address,
    adapter_name: String,
    session: Option<Session>,
    device: Option<Device>,
    write: Option<Characteristic>,
    notify: Option<Notes>,
    events: Option<Events>,
}

fn text(error: bluer::Error) -> String {
    error.to_string()
}

impl BluerTransport {
    pub fn new(address: Address, adapter_name: String) -> Self {
        Self {
            address,
            adapter_name,
            session: None,
            device: None,
            write: None,
            notify: None,
            events: None,
        }
    }

    /// The adapter, over a session this transport keeps alive for its life.
    async fn adapter(&mut self) -> Result<Adapter, String> {
        if self.session.is_none() {
            self.session = Some(Session::new().await.map_err(text)?);
        }
        match self.session.as_ref() {
            Some(session) => session.adapter(&self.adapter_name).map_err(text),
            None => Err("no BlueZ session".to_owned()),
        }
    }
}

impl Transport for BluerTransport {
    async fn scan(&mut self, window: Duration) -> Result<Option<Seen>, String> {
        let adapter = self.adapter().await?;
        let mut found = Box::pin(adapter.discover_devices().await.map_err(text)?);
        // Discovery runs while the stream is polled, and stops when it is dropped.
        let _ = tokio::time::timeout(window, async { while found.next().await.is_some() {} }).await;
        drop(found);
        let device = adapter.device(self.address).map_err(text)?;
        // BlueZ has no record of a light it has not heard, so that is "not seen".
        let Ok(manufacturer) = device.manufacturer_data().await else {
            return Ok(None);
        };
        let name = device.name().await.ok().flatten();
        let manufacturer_data: BTreeMap<u16, Vec<u8>> =
            manufacturer.unwrap_or_default().into_iter().collect();
        Ok(Some(Seen {
            name,
            manufacturer_data,
        }))
    }

    async fn connect(&mut self, timeout: Duration) -> Result<(), String> {
        let adapter = self.adapter().await?;
        let device = adapter.device(self.address).map_err(text)?;
        tokio::time::timeout(timeout, device.connect())
            .await
            .map_err(|_| "connect timed out".to_owned())?
            .map_err(text)?;
        let write_uuid = Uuid::parse_str(WRITE_UUID).map_err(|error| error.to_string())?;
        let read_uuid = Uuid::parse_str(READ_UUID).map_err(|error| error.to_string())?;
        let mut write = None;
        let mut read = None;
        for service in device.services().await.map_err(text)? {
            for characteristic in service.characteristics().await.map_err(text)? {
                let uuid = characteristic.uuid().await.map_err(text)?;
                if uuid == write_uuid {
                    write = Some(characteristic);
                } else if uuid == read_uuid {
                    read = Some(characteristic);
                }
            }
        }
        let (Some(write), Some(read)) = (write, read) else {
            let _ = device.disconnect().await;
            return Err("the light does not offer the write and notify characteristics".to_owned());
        };
        let notify = read.notify().await.map_err(text)?;
        let events = device.events().await.map_err(text)?;
        self.device = Some(device);
        self.write = Some(write);
        self.notify = Some(Box::pin(notify));
        self.events = Some(Box::pin(events));
        Ok(())
    }

    async fn write(&mut self, frame: &[u8]) -> Result<(), String> {
        let Some(characteristic) = self.write.as_ref() else {
            return Err("no link to write to".to_owned());
        };
        // Write without response, as the Python's `response=False`.
        let request = CharacteristicWriteRequest {
            op_type: WriteOp::Command,
            ..Default::default()
        };
        characteristic
            .write_ext(frame, &request)
            .await
            .map_err(text)
    }

    async fn next_notification(&mut self) -> Option<Vec<u8>> {
        let (Some(notify), Some(events)) = (self.notify.as_mut(), self.events.as_mut()) else {
            return pending().await;
        };
        loop {
            tokio::select! {
                frame = notify.next() => return frame,
                event = events.next() => match event {
                    None | Some(DeviceEvent::PropertyChanged(DeviceProperty::Connected(false))) => {
                        return None;
                    }
                    Some(_) => {}
                },
            }
        }
    }

    async fn disconnect(&mut self) {
        self.notify = None;
        self.events = None;
        self.write = None;
        if let Some(device) = self.device.take() {
            let _ = device.disconnect().await;
        }
    }
}
