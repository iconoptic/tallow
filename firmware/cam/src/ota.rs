//! Signed image intake on TCP port 3232, the version floor, the five-minute
//! valid mark, and the rollback timer.
//!
//! The header is checked by [`crate::push`] before `esp_ota_begin`. The body
//! is hashed while it is written. `esp_ota_end` and `esp_ota_set_boot_partition`
//! run only when that hash is the signed digest. The cam sends one result byte,
//! a `Code` from the core with `0` for ok, then reboots into a stored image.
//!
//! A pending image is marked valid once, after five continuous minutes of
//! Wi-Fi and a recent frame. A pending image that gets no mark in 15 minutes
//! rolls back. That covers a hang, which a crash-only rollback would miss.
//!
//! The floor is the highest version this board has confirmed. The version of a
//! stored image waits in NVS as `pending` and becomes the floor at the mark.

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::time::Duration;

use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::sys;
use tallow_mesh_core::ota::{
    running_version, Action, Code, Identity, Receiver, Up, CAM_BITMAP, CAM_SLOT, PRODUCT_CAM0,
};

use crate::push::{self, Idle, Offer, Steady};

const PORT: u16 = 3232;
const CHUNK: usize = 4096;
/// Most reads in one pass of the main loop, so Wi-Fi handling still runs.
const DRAIN: usize = 64;
/// Time for the result byte to leave before a reboot.
const FLUSH_MS: u32 = 1000;
const ANSWER_TIMEOUT: Duration = Duration::from_secs(2);
const NAMESPACE: &str = "tallow_ota";
const FLOOR_KEY: &str = "floor";
const PENDING_KEY: &str = "pending";

/// The public half baked by `build.rs`. The seed is not in the image.
const OTA_PUB: &[u8; 32] = include_bytes!(concat!(env!("OUT_DIR"), "/ota_pub.bin"));

/// Bind port 3232. The caller retries when the stack is not up yet.
pub fn bind() -> std::io::Result<TcpListener> {
    let sock = TcpListener::bind(("0.0.0.0", PORT))?;
    sock.set_nonblocking(true)?;
    println!("listen {PORT}");
    Ok(sock)
}

/// The floor and the pending version, in NVS.
pub struct Versions {
    nvs: Option<EspNvs<NvsDefault>>,
    floor: u32,
}

impl Versions {
    /// Reads the floor. A failed read refuses every push. A running image that
    /// is not pending was confirmed earlier or rolled back to, so a stored
    /// pending version no longer means anything.
    pub fn open(partition: Option<EspDefaultNvsPartition>) -> Self {
        let nvs = partition.and_then(|part| EspNvs::new(part, NAMESPACE, true).ok());
        let floor = match &nvs {
            Some(nvs) => push::floor_from(nvs.get_u32(FLOOR_KEY)),
            None => u32::MAX,
        };
        if let Some(nvs) = &nvs {
            if !running_is_pending() {
                let _ = nvs.remove(PENDING_KEY);
            }
        }
        Self { nvs, floor }
    }

    fn floor(&self) -> u32 {
        self.floor
    }

    /// Product, the image that is executing, and the confirmed floor.
    fn identity(&self) -> Identity {
        let pending = self
            .nvs
            .as_ref()
            .and_then(|nvs| nvs.get_u32(PENDING_KEY).ok().flatten());
        Identity {
            product: PRODUCT_CAM0,
            running: running_version(running_is_pending(), pending, self.floor),
            floor: self.floor,
        }
    }

    fn set_pending(&mut self, version: u32) -> bool {
        self.nvs
            .as_ref()
            .is_some_and(|nvs| nvs.set_u32(PENDING_KEY, version).is_ok())
    }

    /// The running image was just marked valid, so its version is the floor.
    fn promote(&mut self) {
        let Some(nvs) = &self.nvs else {
            return;
        };
        let pending = nvs.get_u32(PENDING_KEY).ok().flatten();
        if let Some(version) = push::promoted(self.floor, pending) {
            if nvs.set_u32(FLOOR_KEY, version).is_err() {
                println!("ota floor not saved");
                return;
            }
            self.floor = version;
        }
        let _ = nvs.remove(PENDING_KEY);
    }
}

/// Starts the 15-minute limit when the running image is pending. It runs on the
/// timer task, so a hung main task does not stop it.
pub fn arm_rollback() {
    if !running_is_pending() {
        return;
    }
    let args = sys::esp_timer_create_args_t {
        callback: Some(rollback_now),
        arg: core::ptr::null_mut(),
        dispatch_method: sys::esp_timer_dispatch_t_ESP_TIMER_TASK,
        name: c"rollback".as_ptr(),
        skip_unhandled_events: false,
    };
    let mut timer = core::ptr::null_mut();
    // SAFETY: `args` and `timer` are valid for this call, which copies `args`.
    // The name is a static C string. The callback is a plain `extern "C"` fn
    // that ignores its null argument.
    let made = unsafe { sys::esp_timer_create(&args, &mut timer) };
    if made != sys::ESP_OK {
        println!("ota rollback timer failed ({made})");
        return;
    }
    let after_us = u64::from(push::ROLLBACK_AFTER_MS) * 1000;
    // SAFETY: `timer` is the handle `esp_timer_create` just returned. It is
    // never deleted, so the handle stays valid until the timer fires.
    let started = unsafe { sys::esp_timer_start_once(timer, after_us) };
    if started != sys::ESP_OK {
        println!("ota rollback timer failed ({started})");
    }
}

unsafe extern "C" fn rollback_now(_arg: *mut core::ffi::c_void) {
    if !running_is_pending() {
        return;
    }
    println!("ota not confirmed, rolling back");
    // SAFETY: takes no arguments and is allowed from any task. On success it
    // resets the chip into the previous slot. On failure it returns the error.
    let err = unsafe { sys::esp_ota_mark_app_invalid_rollback_and_reboot() };
    println!("ota rollback failed ({err})");
}

pub struct Listener {
    sock: TcpListener,
    job: Option<Job>,
    steady: Steady,
    versions: Versions,
}

impl Listener {
    pub fn new(sock: TcpListener, versions: Versions) -> Self {
        Self {
            sock,
            job: None,
            steady: Steady::new(),
            versions,
        }
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// The confirmed floor, for the mesh receiver opened beside this listener.
    pub fn floor(&self) -> u32 {
        self.versions.floor()
    }

    /// What this camera reports on the mesh.
    pub fn identity(&self) -> Identity {
        self.versions.identity()
    }

    /// The version store the mesh writer shares with the TCP push.
    pub fn versions_mut(&mut self) -> &mut Versions {
        &mut self.versions
    }

    pub fn note_link(&mut self, up: bool, now_ms: u32) {
        self.steady.note_link(up, now_ms);
    }

    pub fn note_frame(&mut self, now_ms: u32) {
        self.steady.note_frame(now_ms);
    }

    /// Mark the running image valid when [`Steady`] says so and the running
    /// slot is still pending verify. One success ends the checks. The returned
    /// floor is the one just stored, so the mesh receiver can refuse anything
    /// older.
    pub fn confirm_at(&mut self, now_ms: u32) -> Option<u32> {
        if !self.steady.due(now_ms) {
            return None;
        }
        if !running_is_pending() {
            self.steady.mark();
            return None;
        }
        // SAFETY: takes no arguments. Marks the running image valid so a reboot
        // keeps it. The pending-verify check above is what makes this the first
        // confirmation rather than a mark of an image that was never pending.
        if unsafe { sys::esp_ota_mark_app_valid_cancel_rollback() } == sys::ESP_OK {
            self.steady.mark();
            let before = self.versions.floor();
            self.versions.promote();
            println!("ota valid");
            let after = self.versions.floor();
            return (after != before).then_some(after);
        }
        None
    }

    /// Accept one image, or keep reading the one already accepted. A mesh
    /// transfer already using the inactive slot refuses a new connection
    /// without writing.
    pub fn poll(&mut self, mesh_busy: bool) {
        if self.job.is_none() {
            let Ok((mut sock, _)) = self.sock.accept() else {
                return;
            };
            if mesh_busy {
                let _ = sock.set_nonblocking(false);
                let _ = sock.set_write_timeout(Some(ANSWER_TIMEOUT));
                let _ = sock.write_all(&[Code::Refused as u8]);
                println!("ota result {:?}", Code::Refused);
                return;
            }
            let _ = sock.set_nonblocking(false);
            let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
            let _ = sock.set_write_timeout(Some(ANSWER_TIMEOUT));
            self.job = Some(Job::new(sock));
        }
        for _ in 0..DRAIN {
            let Some(job) = self.job.as_mut() else {
                return;
            };
            match job.poll(&mut self.versions) {
                Step::More => {}
                Step::Waiting => break,
                Step::Done(code) => {
                    self.end(code);
                    return;
                }
            }
        }
        if self.job.as_ref().is_some_and(Job::idle) {
            self.end(Code::Refused);
        }
    }

    /// Answer the peer and drop the connection. Dropping a job that did not
    /// finish aborts its slot. A stored image reboots here.
    fn end(&mut self, code: Code) {
        let Some(mut job) = self.job.take() else {
            return;
        };
        job.answer(code);
        println!("ota result {code:?}");
        if code == Code::Ok {
            FreeRtos::delay_ms(FLUSH_MS);
            // SAFETY: takes no arguments and does not return. The new image is
            // already selected for the next boot.
            unsafe { sys::esp_restart() };
        }
    }
}

/// What one read did.
enum Step {
    /// Bytes moved. The next read may not block.
    More,
    /// Nothing more to do on this pass of the main loop.
    Waiting,
    Done(Code),
}

struct Job {
    sock: TcpStream,
    offer: Offer,
    header: [u8; push::HEADER_LEN],
    got: usize,
    admitted: bool,
    slot: Slot,
    idle: Idle,
    buf: Vec<u8>,
}

impl Job {
    fn new(sock: TcpStream) -> Self {
        Self {
            sock,
            offer: Offer::new(),
            header: [0u8; push::HEADER_LEN],
            got: 0,
            admitted: false,
            slot: Slot::new(),
            idle: Idle::new(now_ms()),
            buf: vec![0u8; CHUNK],
        }
    }

    fn poll(&mut self, versions: &mut Versions) -> Step {
        if !self.admitted {
            return self.read_header(versions);
        }
        self.read_body(versions)
    }

    /// Past the limit. Header bytes do not count as progress, so a peer that
    /// drips them has the same 30 s as one that stays silent.
    fn idle(&self) -> bool {
        self.idle.expired(now_ms())
    }

    fn answer(&mut self, code: Code) {
        let _ = self.sock.write_all(&[code as u8]);
        let _ = self.sock.shutdown(Shutdown::Write);
    }

    fn read_header(&mut self, versions: &Versions) -> Step {
        match self.sock.read(&mut self.header[self.got..]) {
            Ok(0) => Step::Done(Code::Refused),
            Ok(n) => {
                self.got += n;
                if self.got < push::HEADER_LEN {
                    return Step::Waiting;
                }
                let code = self.offer.admit(&self.header, OTA_PUB, versions.floor());
                if code != Code::Ok {
                    return Step::Done(code);
                }
                if !self.slot.begin(self.offer.remaining()) {
                    return Step::Done(Code::Refused);
                }
                self.admitted = true;
                // The erase just blocked this task, so restart the clock here.
                self.idle.note(now_ms());
                Step::More
            }
            Err(err) if paused(&err) => Step::Waiting,
            Err(_) => Step::Done(Code::Refused),
        }
    }

    fn read_body(&mut self, versions: &mut Versions) -> Step {
        let want = usize::min(CHUNK, self.offer.remaining() as usize);
        match self.sock.read(&mut self.buf[..want]) {
            Ok(0) => Step::Done(Code::Missing),
            Ok(n) => {
                let chunk = &self.buf[..n];
                let accepted = self.offer.take(chunk);
                let offset = self.offer.taken().wrapping_sub(n as u32);
                if !accepted || !self.slot.write(offset, chunk) {
                    return Step::Done(Code::Refused);
                }
                self.idle.note(now_ms());
                if self.offer.remaining() != 0 {
                    return Step::More;
                }
                if !self.offer.activate() {
                    return Step::Done(Code::BadHash);
                }
                if !self.slot.finish(self.offer.version(), versions) {
                    return Step::Done(Code::Refused);
                }
                Step::Done(Code::Ok)
            }
            Err(err) if paused(&err) => Step::Waiting,
            Err(_) => Step::Done(Code::Missing),
        }
    }
}

fn paused(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
    )
}

/// Milliseconds since boot, wrapping at 2^32. The mark subtracts with wrap.
pub fn now_ms() -> u32 {
    // SAFETY: takes no arguments and may be called from any task once the system
    // is up, which it is before any code here runs.
    (unsafe { sys::esp_timer_get_time() } / 1000) as u32
}

/// The inactive slot while an image is being written.
struct Slot {
    handle: Option<sys::esp_ota_handle_t>,
    partition: *const sys::esp_partition_t,
}

impl Slot {
    fn new() -> Self {
        Self {
            handle: None,
            partition: core::ptr::null(),
        }
    }

    fn begin(&mut self, size: u32) -> bool {
        self.abort();
        // SAFETY: a null start means "the running partition". The returned
        // pointer is the other OTA slot and stays valid for the life of the app.
        let partition = unsafe { sys::esp_ota_get_next_update_partition(core::ptr::null()) };
        if partition.is_null() {
            return false;
        }
        let mut handle = 0;
        // SAFETY: `partition` is the non-null update slot from the call above,
        // and `handle` is a local that call writes.
        let err = unsafe { sys::esp_ota_begin(partition, size as usize, &mut handle) };
        if err != sys::ESP_OK {
            return false;
        }
        self.handle = Some(handle);
        self.partition = partition;
        true
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> bool {
        let Some(handle) = self.handle else {
            return false;
        };
        // SAFETY: `handle` came from `esp_ota_begin` in this slot and has not
        // been ended, and `bytes` is the caller's buffer for this write.
        let err = unsafe {
            sys::esp_ota_write_with_offset(handle, bytes.as_ptr().cast(), bytes.len(), offset)
        };
        err == sys::ESP_OK
    }

    /// The only call that selects the new image for the next boot. The version
    /// is recorded first, so an image that boots always has its pending record.
    fn finish(&mut self, version: u32, versions: &mut Versions) -> bool {
        let Some(handle) = self.handle.take() else {
            return false;
        };
        let partition = self.partition;
        self.partition = core::ptr::null();
        // SAFETY: `handle` came from `esp_ota_begin` and has not been ended or
        // aborted. `esp_ota_end` frees it whether or not the image checks out.
        let err = unsafe { sys::esp_ota_end(handle) };
        if err != sys::ESP_OK || partition.is_null() || !versions.set_pending(version) {
            return false;
        }
        // SAFETY: `partition` is the slot `esp_ota_begin` wrote and `esp_ota_end`
        // just accepted. This is the only call that selects it for the next boot.
        let err = unsafe { sys::esp_ota_set_boot_partition(partition) };
        err == sys::ESP_OK
    }

    fn perform(&mut self, action: &Action, versions: &mut Versions) -> bool {
        match *action {
            Action::Begin { size } => self.begin(size),
            Action::Write { offset, len, data } => {
                let Some(bytes) = data.get(..usize::from(len)) else {
                    self.abort();
                    return false;
                };
                let wrote = self.write(offset, bytes);
                if !wrote {
                    self.abort();
                }
                wrote
            }
            Action::Abort => {
                self.abort();
                true
            }
            Action::Finish { version } => self.finish(version, versions),
        }
    }

    fn abort(&mut self) {
        self.partition = core::ptr::null();
        let Some(handle) = self.handle.take() else {
            return;
        };
        // SAFETY: `handle` came from `esp_ota_begin` and has not been ended.
        // Abort frees it and leaves the running image selected.
        unsafe { sys::esp_ota_abort(handle) };
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.abort();
    }
}

/// How soon after a stored image the reboot waits, so the result can leave.
const RESTART_MIN_MS: u32 = 200;
/// The longest that reboot waits for the radio.
const RESTART_MAX_MS: u32 = 2_000;

/// A mesh image written to the inactive slot the TCP push also uses.
///
/// Only one of the two writes at a time. This type does not read or write a
/// socket, and the TCP listener does not send these bytes on ESP-NOW.
pub struct MeshPush {
    receiver: Box<Receiver<CAM_SLOT, CAM_BITMAP>>,
    slot: Slot,
    last_ms: u32,
    restart_from: Option<u32>,
}

impl MeshPush {
    /// `floor` is the highest version this camera has confirmed.
    pub fn open(floor: u32) -> Self {
        Self {
            receiver: Box::new(Receiver::<CAM_SLOT, CAM_BITMAP>::new(PRODUCT_CAM0, floor)),
            slot: Slot::new(),
            last_ms: 0,
            restart_from: None,
        }
    }

    /// True while bytes are being written, or a stored image is waiting to boot.
    pub fn busy(&self) -> bool {
        self.receiver.in_progress() || self.restart_from.is_some()
    }

    /// The confirmed floor rose, so a later Begin below it is refused.
    pub fn raise_floor(&mut self, version: u32) {
        self.receiver.raise_floor(version);
    }

    /// One downward payload. A TCP push already in progress refuses a new mesh
    /// image before any flash write. After a stored image, further payloads are
    /// ignored so a new Begin cannot erase that slot.
    pub fn apply(
        &mut self,
        payload: &[u8],
        versions: &mut Versions,
        tcp_busy: bool,
        now: u32,
    ) -> Option<Up> {
        if self.restart_from.is_some() {
            return None;
        }
        if tcp_busy && !self.receiver.in_progress() {
            return Some(Up::Result {
                code: Code::Refused,
            });
        }
        self.last_ms = now;
        let up = self.receiver.push(payload, OTA_PUB);
        let landed = self.settle(versions);
        let up = match up {
            Some(Up::Result { code: Code::Ok }) if !landed => Some(Up::Result {
                code: Code::Refused,
            }),
            None if !landed => Some(Up::Result {
                code: Code::Refused,
            }),
            other => other,
        };
        if matches!(up, Some(Up::Result { code: Code::Ok })) {
            self.restart_from = Some(now);
        }
        up
    }

    /// Drops a transfer that has gone quiet. A stored image is left to reboot.
    pub fn expire(&mut self, now: u32, versions: &mut Versions) {
        if self.restart_from.is_some() {
            return;
        }
        if self.receiver.expire(now, self.last_ms) {
            println!("ota mesh quiet, dropped");
            let _ = self.settle(versions);
        }
    }

    /// True once the result has had time to leave, or the wait has run out.
    pub fn restart_due(&self, now: u32, tx_idle: bool) -> bool {
        let Some(from) = self.restart_from else {
            return false;
        };
        let waited = now.wrapping_sub(from);
        waited >= RESTART_MAX_MS || (waited >= RESTART_MIN_MS && tx_idle)
    }

    fn settle(&mut self, versions: &mut Versions) -> bool {
        let mut landed = true;
        while let Some(action) = self.receiver.take_action() {
            landed &= self.slot.perform(&action, versions);
        }
        if !landed {
            self.receiver.abandon();
            while let Some(action) = self.receiver.take_action() {
                let _ = self.slot.perform(&action, versions);
            }
        }
        landed
    }
}

fn running_is_pending() -> bool {
    // SAFETY: returns the running app's partition, or null. The pointer stays
    // valid for the life of the app.
    let partition = unsafe { sys::esp_ota_get_running_partition() };
    if partition.is_null() {
        return false;
    }
    let mut state = sys::esp_ota_img_states_t_ESP_OTA_IMG_UNDEFINED;
    // SAFETY: `partition` is the non-null running partition, and `state` is a
    // local this call writes.
    let err = unsafe { sys::esp_ota_get_state_partition(partition, &mut state) };
    err == sys::ESP_OK && state == sys::esp_ota_img_states_t_ESP_OTA_IMG_PENDING_VERIFY
}
