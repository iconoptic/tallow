//! Listening: BLE scanning and Bluetooth Classic inquiry.
//!
//! The callbacks run on Bluedroid's task. They only update the shared tables,
//! and the main loop does everything else.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use esp_idf_svc::bt::ble::gap::{BleAddrType, BleGapEvent, EspBleGap, GapSearchEvent, ScanParams};
use esp_idf_svc::bt::gap::{DeviceProp, DiscoveryMode, Eir, EspGap, GapEvent, InqMode};
use esp_idf_svc::bt::{BtDriver, BtDual};
use esp_idf_svc::hal::modem::BluetoothModem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys::EspError;
use tallow_mesh_core::{ad, Kind, Names, Sightings};

pub const SIGHTINGS: usize = 128;
const INQUIRY_GRACE_MS: u32 = 10_000;

pub type Table = Sightings<SIGHTINGS>;
pub type NameTable = Names<64, 8>;

pub struct Heard {
    pub sightings: Mutex<Table>,
    pub names: Mutex<NameTable>,
    pub adverts: AtomicU32,
    pub classic: AtomicU32,
    pub inquiries: AtomicU32,
    pub inquiry_started: AtomicU32,
    pub inquiry_stopped: AtomicU32,
    pub inquiry_errors: AtomicU32,
    params_ready: AtomicBool,
    inquiring: AtomicBool,
    inquiry_began_ms: AtomicU32,
}

pub fn now_ms() -> u32 {
    (unsafe { esp_idf_svc::sys::esp_timer_get_time() } / 1000) as u32
}

/// `Eir` is `repr(transparent)` over its bytes, but its name reader runs
/// `from_utf8_unchecked` on whatever a stranger's device sent. These bytes go
/// through the same checked parser as BLE adverts instead.
fn eir_bytes<'a>(eir: &Eir<'a>) -> &'a [u8] {
    unsafe { core::mem::transmute_copy::<Eir<'a>, &'a [u8]>(eir) }
}

type Driver = Arc<BtDriver<'static, BtDual>>;

pub struct Bluetooth {
    ble: EspBleGap<'static, BtDual, Driver>,
    classic: EspGap<'static, BtDual, Driver>,
    pub heard: Arc<Heard>,
}

impl Bluetooth {
    pub fn start(
        modem: BluetoothModem<'static>,
        nvs: EspDefaultNvsPartition,
    ) -> Result<Self, EspError> {
        let driver: Driver = Arc::new(BtDriver::<BtDual>::new(modem, Some(nvs))?);
        let ble = EspBleGap::new(driver.clone())?;
        let classic = EspGap::new(driver)?;
        let heard = Arc::new(Heard {
            sightings: Mutex::new(Table::new()),
            names: Mutex::new(NameTable::new()),
            adverts: AtomicU32::new(0),
            classic: AtomicU32::new(0),
            inquiries: AtomicU32::new(0),
            inquiry_started: AtomicU32::new(0),
            inquiry_stopped: AtomicU32::new(0),
            inquiry_errors: AtomicU32::new(0),
            params_ready: AtomicBool::new(false),
            inquiring: AtomicBool::new(false),
            inquiry_began_ms: AtomicU32::new(0),
        });
        let tables = heard.clone();
        ble.subscribe(move |event| on_ble(&tables, event))?;
        let tables = heard.clone();
        classic.subscribe(move |event| on_classic(&tables, event))?;
        // This node listens. It offers nothing to pair with or connect to.
        classic.set_scan_mode(false, DiscoveryMode::NonDiscoverable)?;
        Ok(Self {
            ble,
            classic,
            heard,
        })
    }

    /// Answers Classic inquiries or stops. Never connectable or pairable.
    pub fn set_visible(&self, visible: bool) -> Result<(), EspError> {
        let mode = if visible {
            DiscoveryMode::Discoverable
        } else {
            DiscoveryMode::NonDiscoverable
        };
        self.classic.set_scan_mode(false, mode)
    }

    /// Stops scanning, then asks for new parameters. Scanning starts again once
    /// the controller confirms them, in `poll`.
    pub fn configure_scan(&self, window_ms: u16, interval_ms: u16) -> Result<(), EspError> {
        self.heard.params_ready.store(false, Ordering::Release);
        let _ = self.ble.stop_scanning();
        if window_ms == 0 {
            return Ok(());
        }
        let units = |ms: u16| ((ms as u32 * 1000 / 625).clamp(4, 0x4000)) as u16;
        let params = ScanParams {
            scan_window: units(window_ms),
            scan_interval: units(interval_ms),
            ..Default::default()
        };
        self.ble.set_scan_params(&params)
    }

    pub fn poll(&self) -> Result<(), EspError> {
        if self.heard.params_ready.swap(false, Ordering::AcqRel) {
            self.ble.start_scanning(0)?;
        }
        Ok(())
    }

    /// Starts a Classic inquiry unless one is running.
    pub fn inquire(&self, length: u8) -> Result<(), EspError> {
        let now = now_ms();
        if self.heard.inquiring.swap(true, Ordering::AcqRel) {
            // A stuck inquiry must not end Classic scanning for good.
            let limit = length as u32 * 1280 + INQUIRY_GRACE_MS;
            if now.wrapping_sub(self.heard.inquiry_began_ms.load(Ordering::Acquire)) > limit {
                self.heard.inquiry_errors.fetch_add(1, Ordering::Relaxed);
                let _ = self.classic.stop_discovery();
                self.heard.inquiring.store(false, Ordering::Release);
            }
            return Ok(());
        }
        self.heard.inquiry_began_ms.store(now, Ordering::Release);
        self.heard.inquiries.fetch_add(1, Ordering::Relaxed);
        self.classic
            .start_discovery(InqMode::General, length.clamp(1, 0x30), 0)
            .inspect_err(|_| {
                self.heard.inquiry_errors.fetch_add(1, Ordering::Relaxed);
                self.heard.inquiring.store(false, Ordering::Release);
            })
    }
}

fn on_ble(heard: &Heard, event: BleGapEvent) {
    match event {
        BleGapEvent::ScanParameterConfigured(_) => {
            heard.params_ready.store(true, Ordering::Release)
        }
        BleGapEvent::ScanResult(GapSearchEvent::InquiryResult(result)) => {
            heard.adverts.fetch_add(1, Ordering::Relaxed);
            let kind = match result.ble_addr_type {
                BleAddrType::Public | BleAddrType::RpaPublic => Kind::BlePublic,
                _ => Kind::BleRandom,
            };
            let now = now_ms();
            let addr = result.bda.addr();
            if let Ok(mut table) = heard.sightings.lock() {
                table.observe(now, addr, kind, result.rssi.clamp(-127, 20) as i8);
            }
            if let Some(data) = result.ble_adv {
                let length =
                    (result.adv_data_len as usize + result.scan_rsp_len as usize).min(data.len());
                let advert = ad::parse(&data[..length]);
                if let Ok(mut names) = heard.names.lock() {
                    names.offer(now, addr, kind, advert.company, None, advert.name);
                }
            }
        }
        _ => {}
    }
}

fn on_classic(heard: &Heard, event: GapEvent) {
    match event {
        GapEvent::DeviceDiscovered { bd_addr, props } => {
            let mut rssi = -127i8;
            let mut class = None;
            let mut name: Option<&[u8]> = None;
            for prop in props {
                match prop.prop() {
                    DeviceProp::Rssi(value) => rssi = value,
                    DeviceProp::Cod(cod) => {
                        // A 32-bit bitfield struct. The class of device is its low 24 bits.
                        let raw: u32 = unsafe { core::mem::transmute(cod.raw()) };
                        class = Some([raw as u8, (raw >> 8) as u8, (raw >> 16) as u8]);
                    }
                    DeviceProp::Eir(eir) => name = ad::parse(eir_bytes(&eir)).name,
                    _ => {}
                }
            }
            heard.classic.fetch_add(1, Ordering::Relaxed);
            let now = now_ms();
            let addr = bd_addr.addr();
            if let Ok(mut table) = heard.sightings.lock() {
                table.observe(now, addr, Kind::Classic, rssi);
            }
            if let Ok(mut names) = heard.names.lock() {
                names.offer(now, addr, Kind::Classic, None, class, name);
            }
        }
        GapEvent::DeviceDiscoveryStarted => {
            heard.inquiry_started.fetch_add(1, Ordering::Relaxed);
        }
        GapEvent::DeviceDiscoveryStopped => {
            heard.inquiry_stopped.fetch_add(1, Ordering::Relaxed);
            heard.inquiring.store(false, Ordering::Release);
        }
        other => log::warn!("gap {other:?}"),
    }
}
