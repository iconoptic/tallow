//! Flash for one OTA image, and what happens to it after the reboot.
//!
//! `tallow_mesh_core::ota::Receiver` decides which bytes are part of the image.
//! This module writes them, on the main loop, and sets the boot partition only
//! for [`Action::Finish`]. A radio callback never reaches these calls. The slot
//! is erased a sector at a time as bytes arrive, so no single call holds the main
//! loop for long enough to miss `cut_inquiry`.
//!
//! Two NVS keys hold versions. `ota_floor` is the highest image this board has
//! confirmed, and a lower one is refused. `ota_pending` is the image just stored:
//! it becomes the floor only once that image is confirmed, and a rollback clears
//! it. A board that cannot read the floor refuses every image.

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::sys;
use tallow_mesh_core::ota::{Action, Code, Receiver, Up};

/// The public half baked by `build.rs`. The seed is not in the image.
pub const OTA_PUB: &[u8; 32] = include_bytes!(concat!(env!("OUT_DIR"), "/ota_pub.bin"));

/// A new image still pending after this long goes back to the old one, however
/// alive it looks. It covers an image that does not crash but never earns its
/// valid mark, and without it that image would refuse every later update.
const ROLLBACK_AFTER_US: u64 = 15 * 60 * 1_000_000;

const FLOOR_KEY: &str = "ota_floor";
const PENDING_KEY: &str = "ota_pending";

/// The inactive slot while an image is being written, and the boot's versions.
pub struct Slot {
    handle: Option<sys::esp_ota_handle_t>,
    partition: *const sys::esp_partition_t,
    written: u32,
    versions: Option<EspNvs<NvsDefault>>,
    floor: u32,
    confirmed: bool,
}

impl Slot {
    /// Reads the floor, and arms the rollback timer if this image is on trial.
    /// Call it early in `main`, so a boot that hangs later still rolls back.
    pub fn open(nvs: EspDefaultNvsPartition) -> Self {
        let versions = EspNvs::new(nvs, "tallow", true)
            .inspect_err(|error| log::warn!("ota: no version store ({error})"))
            .ok();
        let floor = versions.as_ref().map_or(u32::MAX, read_floor);
        if running_is_pending() {
            arm_rollback();
        } else if let Some(store) = &versions {
            // A rollback, or a USB flash, leaves this image running and the
            // stored image's version unconfirmed for good.
            let _ = store.remove(PENDING_KEY);
        }
        Self {
            handle: None,
            partition: core::ptr::null(),
            written: 0,
            versions,
            floor,
            confirmed: false,
        }
    }

    /// The highest version this board has confirmed. Images below it are refused.
    pub fn floor(&self) -> u32 {
        self.floor
    }

    /// Performs one flash action. `false` means this action did not land, so the
    /// image must not boot.
    pub fn perform(&mut self, action: &Action) -> bool {
        match *action {
            Action::Begin { .. } => self.begin(),
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
            Action::Finish { version } => self.finish(version),
        }
    }

    /// Marks the running image valid, once, when it is still pending verify and
    /// `ready` says it has earned that. Returns the new floor when one was set.
    pub fn confirm_if_ready(&mut self, ready: bool) -> Option<u32> {
        if self.confirmed || !ready {
            return None;
        }
        if !running_is_pending() {
            self.confirmed = true;
            return None;
        }
        // SAFETY: takes no arguments. Marks the running image valid so a reboot
        // keeps it. The pending-verify check above is what makes this the first
        // confirmation rather than a mark of an image that was never pending.
        if unsafe { sys::esp_ota_mark_app_valid_cancel_rollback() } != sys::ESP_OK {
            return None;
        }
        self.confirmed = true;
        let store = self.versions.as_ref()?;
        let version = store.get_u32(PENDING_KEY).ok().flatten()?;
        let floor = self.floor.max(version);
        if let Err(error) = store.set_u32(FLOOR_KEY, floor) {
            log::warn!("ota floor not saved ({error})");
            return None;
        }
        let _ = store.remove(PENDING_KEY);
        self.floor = floor;
        Some(floor)
    }

    fn begin(&mut self) -> bool {
        self.abort();
        // SAFETY: a null start means "the running partition". The returned
        // pointer is the other OTA slot and stays valid for the life of the app.
        let partition = unsafe { sys::esp_ota_get_next_update_partition(core::ptr::null()) };
        if partition.is_null() {
            return false;
        }
        let mut handle = 0;
        // SAFETY: `partition` is the non-null update slot from the call above,
        // and `handle` is a local that call writes. Sequential writes erase each
        // sector when the write reaches it, so this call erases nothing.
        let err = unsafe {
            sys::esp_ota_begin(
                partition,
                sys::OTA_WITH_SEQUENTIAL_WRITES as usize,
                &mut handle,
            )
        };
        if err != sys::ESP_OK {
            log::warn!("ota begin failed ({err})");
            return false;
        }
        self.handle = Some(handle);
        self.partition = partition;
        self.written = 0;
        true
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> bool {
        let Some(handle) = self.handle else {
            return false;
        };
        // `esp_ota_write` appends, so a chunk out of order would land in the wrong
        // place. The receiver only hands them over in order.
        if offset != self.written {
            log::warn!("ota write at {offset}, expected {}", self.written);
            return false;
        }
        // SAFETY: `handle` came from `esp_ota_begin` in this slot and has not
        // been ended, and `bytes` is the caller's buffer for this write.
        let err = unsafe { sys::esp_ota_write(handle, bytes.as_ptr().cast(), bytes.len()) };
        if err != sys::ESP_OK {
            log::warn!("ota write failed ({err})");
            return false;
        }
        self.written += bytes.len() as u32;
        true
    }

    fn finish(&mut self, version: u32) -> bool {
        let Some(handle) = self.handle.take() else {
            return false;
        };
        let partition = self.partition;
        self.partition = core::ptr::null();
        // SAFETY: `handle` came from `esp_ota_begin` and has not been ended or
        // aborted. `esp_ota_end` frees it whether or not the image checks out.
        let err = unsafe { sys::esp_ota_end(handle) };
        if err != sys::ESP_OK {
            log::warn!("ota end failed ({err})");
            return false;
        }
        if partition.is_null() {
            return false;
        }
        // Without a saved version the next boot could not raise the floor, so a
        // store that fails here keeps the old image.
        let Some(store) = self.versions.as_ref() else {
            return false;
        };
        if let Err(error) = store.set_u32(PENDING_KEY, version) {
            log::warn!("ota version not saved ({error})");
            return false;
        }
        // SAFETY: `partition` is the slot `esp_ota_begin` wrote and `esp_ota_end`
        // just accepted. This is the only call that selects it for the next boot.
        let err = unsafe { sys::esp_ota_set_boot_partition(partition) };
        if err != sys::ESP_OK {
            log::warn!("ota set boot failed ({err})");
            let _ = store.remove(PENDING_KEY);
        }
        err == sys::ESP_OK
    }

    fn abort(&mut self) {
        self.partition = core::ptr::null();
        self.written = 0;
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

/// Performs every action the receiver has queued. `false` if any did not land.
/// A flash failure ends the transfer, so the board goes back to listening.
fn settle(receiver: &mut Receiver, slot: &mut Slot) -> bool {
    let mut landed = true;
    while let Some(action) = receiver.take_action() {
        landed &= slot.perform(&action);
    }
    if !landed {
        receiver.abandon();
        while let Some(action) = receiver.take_action() {
            slot.perform(&action);
        }
    }
    landed
}

/// Pushes one downward payload, performs every action it queues, and returns
/// the status or result to send up. A flash failure becomes [`Code::Refused`]:
/// the host must not treat an image as activated when the boot partition was
/// not set.
pub fn apply(receiver: &mut Receiver, slot: &mut Slot, payload: &[u8]) -> Option<Up> {
    let up = receiver.push(payload, OTA_PUB);
    let landed = settle(receiver, slot);
    match up {
        Some(Up::Result { code: Code::Ok }) if !landed => Some(Up::Result {
            code: Code::Refused,
        }),
        None if !landed => Some(Up::Result {
            code: Code::Refused,
        }),
        other => other,
    }
}

/// Drops a transfer the host has gone quiet on.
pub fn expire(receiver: &mut Receiver, slot: &mut Slot, now_ms: u32, last_ms: u32) {
    if receiver.expire(now_ms, last_ms) {
        log::warn!("ota: transfer went quiet, dropped");
        settle(receiver, slot);
    }
}

/// The floor from NVS. Missing means nothing was ever confirmed. Any other read
/// failure is `u32::MAX`, which refuses every image: an unknown floor must not
/// let an old image in.
fn read_floor(store: &EspNvs<NvsDefault>) -> u32 {
    match store.get_u32(FLOOR_KEY) {
        Ok(found) => found.unwrap_or(0),
        Err(error) => {
            log::warn!("ota floor unreadable ({error})");
            u32::MAX
        }
    }
}

/// Starts the one-shot timer. It runs on the timer task, so a wedged main loop
/// still rolls back. A timer that cannot start leaves only a crash to roll back.
fn arm_rollback() {
    let args = sys::esp_timer_create_args_t {
        callback: Some(rollback_due),
        arg: core::ptr::null_mut(),
        dispatch_method: sys::esp_timer_dispatch_t_ESP_TIMER_TASK,
        name: c"ota-rollback".as_ptr(),
        skip_unhandled_events: false,
    };
    let mut timer: sys::esp_timer_handle_t = core::ptr::null_mut();
    // SAFETY: `args` and `timer` are locals valid for the call, which copies what
    // it keeps. The name is a static string, and `rollback_due` is `extern "C"`
    // with the signature the timer task calls.
    let err = unsafe { sys::esp_timer_create(&args, &mut timer) };
    if err != sys::ESP_OK {
        log::warn!("ota rollback timer not created ({err})");
        return;
    }
    // SAFETY: `timer` was just created and is never deleted, so it outlives the
    // call and this shot. The handle is a value the timer service owns.
    let err = unsafe { sys::esp_timer_start_once(timer, ROLLBACK_AFTER_US) };
    if err != sys::ESP_OK {
        log::warn!("ota rollback timer not started ({err})");
    }
}

/// Fires once. If the image has not earned its mark by now, it goes.
///
/// SAFETY: only `arm_rollback` passes this to `esp_timer_create`. The timer task
/// calls it with the null argument given there, and it reads no argument.
unsafe extern "C" fn rollback_due(_: *mut core::ffi::c_void) {
    if !running_is_pending() {
        return;
    }
    log::warn!("ota: image not confirmed in time, rolling back");
    // SAFETY: takes no arguments. It marks this slot invalid and reboots into the
    // other one. It returns only on failure, and a failure leaves this image up.
    let err = unsafe { sys::esp_ota_mark_app_invalid_rollback_and_reboot() };
    log::warn!("ota rollback failed ({err})");
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
