//! Listening: BLE scanning and Bluetooth Classic inquiry.
//!
//! The callbacks run on Bluedroid's task. They only update the shared tables,
//! and the main loop does everything else.
//!
//! Classic events come from a raw `esp_bt_gap_register_callback`, not from
//! `esp-idf-svc`'s `EspGap`. `EspGap` builds `&str`s from remote name and
//! authentication bytes off the air before any of our code runs, which is
//! undefined behaviour. The raw callback copies only the bytes it keeps.
//!
//! Paging is the other way in. A Classic device that is not discoverable only
//! answers a name request sent to its address, so the mesh pages the addresses
//! on the host's Watch list, one at a time. The list is the whitelist: inquiry
//! results are never paged.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use esp_idf_svc::bt::ble::gap::{BleAddrType, BleGapEvent, EspBleGap, GapSearchEvent, ScanParams};
use esp_idf_svc::bt::{BtDriver, BtDual};
use esp_idf_svc::hal::modem::BluetoothModem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys::{self, EspError};
use tallow_mesh_core::wire::WATCH_MAX;
use tallow_mesh_core::{ad, Kind, Names, Sightings};

pub const SIGHTINGS: usize = 128;
const INQUIRY_GRACE_MS: u32 = 10_000;
/// How long before the end of its window an inquiry is cancelled.
const INQUIRY_CUT_MS: u32 = 200;
/// The fewest milliseconds between two name requests. A miss can hold the radio
/// for up to 5 s, so this is a hard limit, not a target.
pub const PAGE_GAP_MS: u32 = 5_000;
/// A name request with no answer after this long stops blocking. Without it, one
/// address that never answers would stop paging and inquiry for good.
pub const PAGE_TIMEOUT_MS: u32 = 6_000;

pub type Table = Sightings<SIGHTINGS>;
pub type NameTable = Names<64, 8>;

/// The addresses to page, in the order the host sent them. `next` is the one
/// the next name request takes.
#[derive(Copy, Clone)]
pub struct Paging {
    addresses: [[u8; 6]; WATCH_MAX],
    count: usize,
    next: usize,
}

impl Paging {
    const fn new() -> Self {
        Self {
            addresses: [[0; 6]; WATCH_MAX],
            count: 0,
            next: 0,
        }
    }
}

pub struct Heard {
    pub sightings: Mutex<Table>,
    pub names: Mutex<NameTable>,
    pub paging: Mutex<Paging>,
    pub adverts: AtomicU32,
    pub classic: AtomicU32,
    pub inquiries: AtomicU32,
    pub inquiry_started: AtomicU32,
    pub inquiry_stopped: AtomicU32,
    pub inquiry_errors: AtomicU32,
    params_ready: AtomicBool,
    inquiring: AtomicBool,
    inquiry_began_ms: AtomicU32,
    /// How long the running inquiry may last, or 0 once it has been cut.
    inquiry_window_ms: AtomicU32,
    /// A name request is in flight. Set by `page_next`, cleared by the callback.
    page_outstanding: AtomicBool,
    /// When the last name request started, answered or not.
    page_began_ms: AtomicU32,
    /// The GATT client holds a connection, or one is being made or torn down. Set
    /// by the main loop. Inquiry and paging wait while it holds.
    link_busy: AtomicBool,
    /// The host asked for scanning, so a window other than 0 is set.
    scan_wanted: AtomicBool,
    /// Scanning was stopped so a connect would not be refused. See `pause_scan_for_connect`.
    scan_paused: AtomicBool,
}

/// The tables the raw Classic callback writes to. Set once in `Bluetooth::start`,
/// before the callback is registered.
static HEARD: OnceLock<Arc<Heard>> = OnceLock::new();

pub fn now_ms() -> u32 {
    // SAFETY: takes no arguments and may be called from any task once the system
    // is up, which it is before any code here runs.
    (unsafe { sys::esp_timer_get_time() } / 1000) as u32
}

/// What one inquiry result said.
struct Inquiry<'a> {
    rssi: i8,
    class: Option<[u8; 3]>,
    name: Option<&'a [u8]>,
}

/// Reads the raw properties of one inquiry result. Each property's `val` is a
/// pointer to its value, and `len` is the value's length. `PropData::prop` is
/// wrong for this: it takes RSSI and class of device from the pointer `val`
/// rather than from what it points to, so both come out as address bits, and it
/// builds a `&str` from a stranger's name bytes with `from_utf8_unchecked`. EIR
/// goes through the same checked parser as BLE adverts.
fn read_props(props: &[sys::esp_bt_gap_dev_prop_t]) -> Inquiry<'_> {
    let mut found = Inquiry {
        rssi: -127,
        class: None,
        name: None,
    };
    for raw in props {
        let Ok(length) = usize::try_from(raw.len) else {
            continue;
        };
        if raw.val.is_null() {
            continue;
        }
        match raw.type_ {
            sys::esp_bt_gap_dev_prop_type_t_ESP_BT_GAP_DEV_PROP_RSSI if length == 1 => {
                // SAFETY: Bluedroid points `val` at the one byte `length` declares,
                // and it stays valid for this callback.
                let rssi = unsafe { (raw.val as *const i8).read() };
                found.rssi = rssi.clamp(-127, 20);
            }
            sys::esp_bt_gap_dev_prop_type_t_ESP_BT_GAP_DEV_PROP_COD if length == 4 => {
                // SAFETY: as above, for 4 bytes. It is a `uint32_t` on Bluedroid's
                // stack, read unaligned so that no alignment is assumed.
                let cod = unsafe { (raw.val as *const u32).read_unaligned() };
                // The class of device is the low 24 bits, low byte first on the wire.
                found.class = Some([cod as u8, (cod >> 8) as u8, (cod >> 16) as u8]);
            }
            sys::esp_bt_gap_dev_prop_type_t_ESP_BT_GAP_DEV_PROP_EIR => {
                // SAFETY: as above, for `length` bytes. The slice is parsed by the
                // bounds-checked `ad::parse`, and what it keeps is copied in this
                // callback.
                let eir = unsafe { core::slice::from_raw_parts(raw.val as *const u8, length) };
                found.name = ad::parse(eir).name;
            }
            _ => {}
        }
    }
    found
}

/// Sets whether the node answers Classic inquiries. Connectable is always off:
/// the node offers nothing to pair with or connect to.
fn set_discovery(mode: sys::esp_bt_discovery_mode_t) -> Result<(), EspError> {
    // SAFETY: no pointers are passed, and both modes are taken by value. The call
    // only asks Bluedroid's task to change the mode.
    sys::esp!(unsafe {
        sys::esp_bt_gap_set_scan_mode(sys::esp_bt_connection_mode_t_ESP_BT_NON_CONNECTABLE, mode)
    })
}

/// Starts a general inquiry of `length` 1.28 s units, with no response limit.
fn start_discovery(length: u8) -> Result<(), EspError> {
    // SAFETY: no pointers are passed. The caller keeps `length` in 1..=0x30, the
    // range Bluedroid accepts. The call only asks Bluedroid's task to inquire.
    sys::esp!(unsafe {
        sys::esp_bt_gap_start_discovery(
            sys::esp_bt_inq_mode_t_ESP_BT_INQ_MODE_GENERAL_INQUIRY,
            length,
            0,
        )
    })
}

/// Cancels the running inquiry, or its name requests.
fn cancel_discovery() -> Result<(), EspError> {
    // SAFETY: takes no arguments. The call only asks Bluedroid's task to cancel.
    sys::esp!(unsafe { sys::esp_bt_gap_cancel_discovery() })
}

/// Asks for the name of the device at `address`, which pages it. The answer comes
/// to `on_gap` as `ESP_BT_GAP_READ_REMOTE_NAME_EVT`.
fn read_remote_name(address: [u8; 6]) -> Result<(), EspError> {
    // Bluedroid takes the address as a pointer, so it is copied into a local that
    // lives for this call. The local is the only thing the pointer names.
    let mut bda = address;
    // SAFETY: `bda` is a local array of 6 bytes, the address length Bluedroid
    // reads, and it is valid for the whole call. The stack reads it before the
    // call returns and does not keep the pointer.
    sys::esp!(unsafe { sys::esp_bt_gap_read_remote_name(bda.as_mut_ptr()) })
}

type Driver = Arc<BtDriver<'static, BtDual>>;

pub struct Bluetooth {
    ble: EspBleGap<'static, BtDual, Driver>,
    /// Keeps Bluedroid up. Dropping the last clone of the driver disables it,
    /// and the raw Classic callback would then run into a stopped stack.
    _driver: Driver,
    pub heard: Arc<Heard>,
}

impl Bluetooth {
    pub fn start(
        modem: BluetoothModem<'static>,
        nvs: EspDefaultNvsPartition,
    ) -> Result<Self, EspError> {
        // `BtDriver::new` enables Bluedroid, so the callback can be registered below.
        let driver: Driver = Arc::new(BtDriver::<BtDual>::new(modem, Some(nvs))?);
        let ble = EspBleGap::new(driver.clone())?;
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
            inquiry_window_ms: AtomicU32::new(0),
            paging: Mutex::new(Paging::new()),
            page_outstanding: AtomicBool::new(false),
            page_began_ms: AtomicU32::new(0),
            link_busy: AtomicBool::new(false),
            scan_wanted: AtomicBool::new(false),
            scan_paused: AtomicBool::new(false),
        });
        let tables = heard.clone();
        ble.subscribe(move |event| on_ble(&tables, event))?;
        HEARD
            .set(heard.clone())
            .map_err(|_| EspError::from_infallible::<{ sys::ESP_ERR_INVALID_STATE }>())?;
        // SAFETY: `on_gap` is the only function registered, and it is `extern "C"`
        // with the signature Bluedroid expects. Bluedroid is enabled (see above),
        // and `driver` stays in the returned struct, so the stack is up whenever
        // the callback can run.
        sys::esp!(unsafe { sys::esp_bt_gap_register_callback(Some(on_gap)) })?;
        // This node listens. It offers nothing to pair with or connect to.
        set_discovery(sys::esp_bt_discovery_mode_t_ESP_BT_NON_DISCOVERABLE)?;
        Ok(Self {
            ble,
            _driver: driver,
            heard,
        })
    }

    /// Answers Classic inquiries or stops. Never connectable or pairable.
    pub fn set_visible(&self, visible: bool) -> Result<(), EspError> {
        let mode = if visible {
            sys::esp_bt_discovery_mode_t_ESP_BT_GENERAL_DISCOVERABLE
        } else {
            sys::esp_bt_discovery_mode_t_ESP_BT_NON_DISCOVERABLE
        };
        set_discovery(mode)
    }

    /// Stops scanning, then asks for new parameters. Scanning starts again once
    /// the controller confirms them, in `poll`.
    pub fn configure_scan(&self, window_ms: u16, interval_ms: u16) -> Result<(), EspError> {
        self.heard
            .scan_wanted
            .store(window_ms != 0, Ordering::Release);
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
        self.cut_inquiry();
        // A pause swallows the ready flag, and `resume_scan` sets it again.
        if self.heard.params_ready.swap(false, Ordering::AcqRel)
            && !self.heard.scan_paused.load(Ordering::Acquire)
        {
            self.ble.start_scanning(0)?;
        }
        Ok(())
    }

    /// Tells paging and inquiry to hold off: the GATT client has a connection, or
    /// an image is being written.
    pub fn set_link_busy(&self, busy: bool) {
        self.heard.link_busy.store(busy, Ordering::Release);
    }

    /// Stops a running scan so that a connect is not refused beside it. Returns
    /// true if this stopped one, so the caller knows a retry is worth making.
    /// `resume_scan` starts scanning again.
    pub fn pause_scan_for_connect(&self) -> bool {
        if !self.heard.scan_wanted.load(Ordering::Acquire)
            || self.heard.scan_paused.swap(true, Ordering::AcqRel)
        {
            return false;
        }
        if self.ble.stop_scanning().is_ok() {
            return true;
        }
        self.heard.scan_paused.store(false, Ordering::Release);
        false
    }

    /// Starts scanning again after `pause_scan_for_connect`, once no link is held.
    /// It does nothing when scanning was not paused.
    pub fn resume_scan(&self) {
        if self.heard.scan_paused.swap(false, Ordering::AcqRel)
            && self.heard.scan_wanted.load(Ordering::Acquire)
        {
            self.heard.params_ready.store(true, Ordering::Release);
        }
    }

    /// Ends an inquiry just before its window closes. Left to finish, Bluedroid
    /// follows it with a Remote Name Request to every device with no name in its
    /// EIR, which pages that device. A node connects only to whitelisted devices.
    fn cut_inquiry(&self) {
        let heard = &self.heard;
        if !heard.inquiring.load(Ordering::Acquire) {
            return;
        }
        let window = heard.inquiry_window_ms.load(Ordering::Acquire);
        let began = heard.inquiry_began_ms.load(Ordering::Acquire);
        if window == 0 || now_ms().wrapping_sub(began) < window {
            return;
        }
        heard.inquiry_window_ms.store(0, Ordering::Release);
        if cancel_discovery().is_err() {
            heard.inquiry_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Ends any running inquiry now, before work that keeps the main loop from
    /// reaching `cut_inquiry` on time. The same reason applies as there.
    pub fn stop_inquiry(&self) {
        let heard = &self.heard;
        if !heard.inquiring.load(Ordering::Acquire) {
            return;
        }
        heard.inquiry_window_ms.store(0, Ordering::Release);
        if cancel_discovery().is_err() {
            heard.inquiry_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Replaces the paging list. `count` is at most `WATCH_MAX`, as the core
    /// guarantees, and anything above it is treated as the maximum. The round
    /// robin starts again at the first address.
    pub fn set_watch(&self, count: u8, addresses: &[[u8; 6]; WATCH_MAX]) {
        if let Ok(mut paging) = self.heard.paging.lock() {
            paging.addresses = *addresses;
            paging.count = (count as usize).min(WATCH_MAX);
            paging.next = 0;
        }
    }

    /// True while a name request is in flight and has not timed out.
    fn page_busy(&self, now: u32) -> bool {
        self.heard.page_outstanding.load(Ordering::Acquire)
            && now.wrapping_sub(self.heard.page_began_ms.load(Ordering::Acquire)) < PAGE_TIMEOUT_MS
    }

    /// Starts a name request for the next address on the list, if the rules allow.
    /// The rules: the list is not empty, no inquiry is running, at most one request
    /// per `PAGE_GAP_MS`, and none is outstanding. Returns true if one started.
    pub fn page_next(&self, now: u32) -> Result<bool, EspError> {
        let heard = &self.heard;
        if heard.inquiring.load(Ordering::Acquire) || heard.link_busy.load(Ordering::Acquire) {
            return Ok(false);
        }
        if now.wrapping_sub(heard.page_began_ms.load(Ordering::Acquire)) < PAGE_GAP_MS {
            return Ok(false);
        }
        if self.page_busy(now) {
            return Ok(false);
        }
        let address = {
            let Ok(mut paging) = heard.paging.lock() else {
                return Ok(false);
            };
            if paging.count == 0 {
                return Ok(false);
            }
            let index = paging.next % paging.count;
            paging.next = (index + 1) % paging.count;
            paging.addresses[index]
        };
        heard.page_began_ms.store(now, Ordering::Release);
        heard.page_outstanding.store(true, Ordering::Release);
        match read_remote_name(address) {
            Ok(()) => Ok(true),
            Err(error) => {
                heard.page_outstanding.store(false, Ordering::Release);
                Err(error)
            }
        }
    }

    /// Starts a Classic inquiry unless one is running. Returns `Ok(false)` when a
    /// name request is outstanding, and the inquiry must wait for it. Otherwise it
    /// returns `Ok(true)`, which includes "one is already running".
    pub fn inquire(&self, length: u8) -> Result<bool, EspError> {
        let now = now_ms();
        // Paging and inquiry never overlap: a name request is not started during an
        // inquiry, and an inquiry waits for an outstanding request. Neither runs
        // while the GATT client holds a connection or an image is being written.
        if self.heard.link_busy.load(Ordering::Acquire) || self.page_busy(now) {
            return Ok(false);
        }
        if self.heard.inquiring.swap(true, Ordering::AcqRel) {
            // A stuck inquiry must not end Classic scanning for good.
            let limit = length as u32 * 1280 + INQUIRY_GRACE_MS;
            if now.wrapping_sub(self.heard.inquiry_began_ms.load(Ordering::Acquire)) > limit {
                self.heard.inquiry_errors.fetch_add(1, Ordering::Relaxed);
                let _ = cancel_discovery();
                self.heard.inquiring.store(false, Ordering::Release);
            }
            return Ok(true);
        }
        let length = length.clamp(1, 0x30);
        self.heard.inquiry_began_ms.store(now, Ordering::Release);
        self.heard
            .inquiry_window_ms
            .store(length as u32 * 1280 - INQUIRY_CUT_MS, Ordering::Release);
        self.heard.inquiries.fetch_add(1, Ordering::Relaxed);
        start_discovery(length)
            .inspect_err(|_| {
                self.heard.inquiry_errors.fetch_add(1, Ordering::Relaxed);
                self.heard.inquiring.store(false, Ordering::Release);
            })
            .map(|()| true)
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

/// The raw Classic GAP callback. Bluedroid calls it on its own task. `param` points
/// at the event's parameters, which are valid only for this call. It copies what
/// it keeps, and it does not block.
///
/// SAFETY: this function is only passed to `esp_bt_gap_register_callback`, which
/// Bluedroid calls with the event's parameters. `param` is checked for null before
/// any read, and each union member is read only for the event that sets it.
unsafe extern "C" fn on_gap(
    event: sys::esp_bt_gap_cb_event_t,
    param: *mut sys::esp_bt_gap_cb_param_t,
) {
    let Some(heard) = HEARD.get() else {
        return;
    };
    if param.is_null() {
        return;
    }
    match event {
        sys::esp_bt_gap_cb_event_t_ESP_BT_GAP_DISC_RES_EVT => {
            // SAFETY: `param` is non-null and this is DISC_RES, so `disc_res` is the
            // member Bluedroid set. The struct is copied before its pointer is used.
            let res = unsafe { (*param).disc_res };
            let props: &[sys::esp_bt_gap_dev_prop_t] = match usize::try_from(res.num_prop) {
                Ok(count) if count > 0 && !res.prop.is_null() => {
                    // SAFETY: `prop` points at `num_prop` property structs that Bluedroid
                    // keeps valid for this callback. It was checked for null, and the
                    // count is non-negative.
                    unsafe { core::slice::from_raw_parts(res.prop, count) }
                }
                _ => &[],
            };
            on_discovered(heard, res.bda, props);
        }
        sys::esp_bt_gap_cb_event_t_ESP_BT_GAP_DISC_STATE_CHANGED_EVT => {
            // SAFETY: `param` is non-null and this is DISC_STATE_CHANGED, so
            // `disc_st_chg` is the member Bluedroid set. It is a plain integer.
            let state = unsafe { (*param).disc_st_chg.state };
            on_discovery_state(heard, state);
        }
        sys::esp_bt_gap_cb_event_t_ESP_BT_GAP_READ_REMOTE_NAME_EVT => {
            // SAFETY: `param` is non-null and this is READ_REMOTE_NAME, so
            // `read_rmt_name` is the member Bluedroid set. It is copied whole, and
            // the name is read as bytes only.
            let read = unsafe { (*param).read_rmt_name };
            on_remote_name(heard, read);
        }
        _ => {}
    }
}

fn on_discovered(heard: &Heard, bda: sys::esp_bd_addr_t, props: &[sys::esp_bt_gap_dev_prop_t]) {
    let found = read_props(props);
    heard.classic.fetch_add(1, Ordering::Relaxed);
    let now = now_ms();
    if let Ok(mut table) = heard.sightings.lock() {
        table.observe(now, bda, Kind::Classic, found.rssi);
    }
    if let Ok(mut names) = heard.names.lock() {
        names.offer(now, bda, Kind::Classic, None, found.class, found.name);
    }
}

fn on_discovery_state(heard: &Heard, state: sys::esp_bt_gap_discovery_state_t) {
    if state == sys::esp_bt_gap_discovery_state_t_ESP_BT_GAP_DISCOVERY_STOPPED {
        heard.inquiry_stopped.fetch_add(1, Ordering::Relaxed);
        heard.inquiring.store(false, Ordering::Release);
    } else {
        heard.inquiry_started.fetch_add(1, Ordering::Relaxed);
    }
}

/// A remote name read, from a page or from the stack. Any answer ends the request
/// in flight. A success records a Classic sighting at -127, since a page has no
/// radio reading, and the name as bytes, up to its first NUL. The name is never
/// made into a `&str`, because it comes off the air. A failure records nothing.
fn on_remote_name(heard: &Heard, read: sys::esp_bt_gap_cb_param_t_read_rmt_name_param) {
    heard.page_outstanding.store(false, Ordering::Release);
    if read.stat != sys::esp_bt_status_t_ESP_BT_STATUS_SUCCESS {
        return;
    }
    let bytes = &read.rmt_name;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let now = now_ms();
    if let Ok(mut table) = heard.sightings.lock() {
        table.observe(now, read.bda, Kind::Classic, -127);
    }
    if let Ok(mut names) = heard.names.lock() {
        names.offer(
            now,
            read.bda,
            Kind::Classic,
            None,
            None,
            Some(&bytes[..end]),
        );
    }
}
