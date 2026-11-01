//! The node's GATT client: the `esp_ble_gattc_*` calls for one BLE connection, and
//! a callback that copies each event into a queue and returns.
//!
//! What happens next is decided in `tallow_mesh_core::gatt::Client`. This module
//! performs each `Act` the client asks for, and turns the stack's events into the
//! `Done` the client waits for. Nothing here blocks or writes to serial. The
//! callback runs on Bluedroid's task, so it only copies. The main loop does the
//! rest.

use std::sync::Mutex;

use esp_idf_svc::sys::{self, EspError};
use tallow_mesh_core::gatt::{Act, Done, NOTIFY_MAX, VALUE_MAX};

/// The one app this node registers with Bluedroid.
const APP_ID: u16 = 0x7A11;
/// Events copied and not yet taken. A full queue drops the newest event, and the
/// request that waited for it times out. The client then drops the link.
const DEPTH: usize = 8;
/// The MTU asked for before a connect. A 170-byte write needs at least 173, and
/// the device's own MTU can lower this.
const LOCAL_MTU: u16 = 256;
/// The Client Characteristic Configuration descriptor, where notifications are switched on.
const CCCD_UUID: u16 = 0x2902;

/// One event as the callback copied it. Values are byte copies, and nothing is
/// read as text.
#[derive(Copy, Clone)]
enum Raw {
    App {
        ok: bool,
        gattc_if: u8,
    },
    Opened {
        ok: bool,
        conn: u16,
    },
    Mtu(bool),
    Searched(bool),
    Closed,
    Written(bool),
    Read {
        ok: bool,
        len: u8,
        value: [u8; VALUE_MAX],
    },
    Registered(bool),
    Notify {
        handle: u16,
        len: u8,
        value: [u8; NOTIFY_MAX],
    },
}

struct Queue {
    items: [Option<Raw>; DEPTH],
    head: usize,
    len: usize,
}

/// Written by the callback, and read by the main loop.
static EVENTS: Mutex<Queue> = Mutex::new(Queue {
    items: [None; DEPTH],
    head: 0,
    len: 0,
});

fn push(raw: Raw) {
    // A blocking lock here would stall Bluedroid's task. A lock held elsewhere, or
    // a full queue, drops the event, and the request waiting for it times out.
    let Ok(mut queue) = EVENTS.try_lock() else {
        return;
    };
    if queue.len == DEPTH {
        return;
    }
    let slot = (queue.head + queue.len) % DEPTH;
    queue.items[slot] = Some(raw);
    queue.len += 1;
}

fn pop() -> Option<Raw> {
    let mut queue = EVENTS.lock().ok()?;
    if queue.len == 0 {
        return None;
    }
    let head = queue.head;
    let raw = queue.items[head].take();
    queue.head = (head + 1) % DEPTH;
    queue.len -= 1;
    raw
}

/// Registers the callback and the app, and sets the MTU to ask for. Call once,
/// after Bluetooth has started. The app is ready when `Link::take` reports it.
pub fn start() -> Result<(), EspError> {
    // SAFETY: `on_gattc` is the only callback registered here, and it is
    // `extern "C"` with the signature Bluedroid calls it with. Bluetooth is up
    // before this runs, so the stack can call it.
    sys::esp!(unsafe { sys::esp_ble_gattc_register_callback(Some(on_gattc)) })?;
    // SAFETY: takes a plain integer and no pointers. It must run before a connect,
    // and it does, since no connect is made before this returns.
    sys::esp!(unsafe { sys::esp_ble_gatt_set_local_mtu(LOCAL_MTU) })?;
    // SAFETY: takes a plain integer and no pointers. The stack answers on
    // `ESP_GATTC_REG_EVT`, which `on_gattc` copies.
    sys::esp!(unsafe { sys::esp_ble_gattc_app_register(APP_ID) })
}

/// Logs a failed call, from the main loop. Never from the callback.
fn checked(what: &str, result: Result<(), EspError>) -> bool {
    match result {
        Ok(()) => true,
        Err(error) => {
            log::warn!("gatt: {what} failed ({error})");
            false
        }
    }
}

/// The one connection's details that the calls need.
#[derive(Default)]
pub struct Link {
    gattc_if: u8,
    ready: bool,
    conn: u16,
    address: [u8; 6],
}

impl Link {
    /// Performs one radio action. Returns the result now if the call failed on the
    /// spot. Otherwise the result comes from `take`, once the stack reports it.
    pub fn perform(&mut self, act: &Act) -> Option<Done> {
        if !self.ready && !matches!(act, Act::Cancel | Act::Close) {
            return Some(failed(act));
        }
        let (gattc_if, conn) = (self.gattc_if, self.conn);
        let ok = match *act {
            Act::Open {
                address,
                address_kind,
            } => {
                self.address = address;
                let mut bda = address;
                // SAFETY: `bda` is a local array of the six address bytes the stack
                // reads. It stays valid for the call, and the stack copies it before
                // the call returns.
                checked(
                    "open",
                    sys::esp!(unsafe {
                        sys::esp_ble_gattc_open(
                            gattc_if,
                            bda.as_mut_ptr(),
                            u32::from(address_kind),
                            true,
                        )
                    }),
                )
            }
            Act::Mtu => checked(
                "mtu",
                // SAFETY: takes the interface and connection id, and no pointers.
                sys::esp!(unsafe { sys::esp_ble_gattc_send_mtu_req(gattc_if, conn) }),
            ),
            Act::Search => checked(
                "search",
                // SAFETY: a null filter asks for every service. The stack writes
                // nothing through it.
                sys::esp!(unsafe {
                    sys::esp_ble_gattc_search_service(gattc_if, conn, core::ptr::null_mut())
                }),
            ),
            Act::Register { handle } => {
                let mut bda = self.address;
                checked(
                    "register for notify",
                    // SAFETY: `bda` is a local array of six bytes, valid for the call.
                    sys::esp!(unsafe {
                        sys::esp_ble_gattc_register_for_notify(gattc_if, bda.as_mut_ptr(), handle)
                    }),
                )
            }
            Act::Enable { handle } => return enable(gattc_if, conn, handle),
            Act::Write { handle, len, value } => {
                let mut bytes = value;
                let length = (len as usize).min(VALUE_MAX) as u16;
                checked(
                    "write",
                    // SAFETY: `bytes` is a local array of `VALUE_MAX` bytes and `length`
                    // never exceeds it. The stack copies the value before the call
                    // returns, and asks for a response.
                    sys::esp!(unsafe {
                        sys::esp_ble_gattc_write_char(
                            gattc_if,
                            conn,
                            handle,
                            length,
                            bytes.as_mut_ptr(),
                            sys::esp_gatt_write_type_t_ESP_GATT_WRITE_TYPE_RSP,
                            sys::esp_gatt_auth_req_t_ESP_GATT_AUTH_REQ_NONE,
                        )
                    }),
                )
            }
            Act::Read { handle } => checked(
                "read",
                // SAFETY: takes the interface, connection, and handle, and no pointers.
                sys::esp!(unsafe {
                    sys::esp_ble_gattc_read_char(
                        gattc_if,
                        conn,
                        handle,
                        sys::esp_gatt_auth_req_t_ESP_GATT_AUTH_REQ_NONE,
                    )
                }),
            ),
            Act::Close => checked(
                "close",
                // SAFETY: takes the interface and connection id, and no pointers.
                sys::esp!(unsafe { sys::esp_ble_gattc_close(gattc_if, conn) }),
            ),
            Act::Cancel => {
                let params = sys::esp_ble_gattc_cancel_open_params_t {
                    gattc_if,
                    remote_bda: self.address,
                };
                checked(
                    "cancel open",
                    // SAFETY: `params` is a local that lives for the call, and the stack
                    // reads it before the call returns.
                    sys::esp!(unsafe { sys::esp_ble_gattc_cancel_open(&params) }),
                )
            }
        };
        if ok {
            None
        } else {
            Some(failed(act))
        }
    }

    /// The next result from the stack, as the client takes it. Also records the
    /// connection, and whether the app has registered.
    pub fn take(&mut self) -> Option<Done> {
        loop {
            let raw = pop()?;
            return Some(match raw {
                Raw::App { ok, gattc_if } => {
                    if ok {
                        self.gattc_if = gattc_if;
                        self.ready = true;
                    }
                    continue;
                }
                Raw::Opened { ok, conn } => {
                    if ok {
                        self.conn = conn;
                    }
                    Done::Opened(ok)
                }
                Raw::Mtu(ok) => Done::Mtu(ok),
                Raw::Searched(ok) => Done::Searched(ok),
                Raw::Closed => {
                    self.conn = 0;
                    Done::Closed
                }
                Raw::Written(ok) => Done::Written(ok),
                Raw::Read { ok, len, value } => Done::Read { ok, len, value },
                Raw::Registered(ok) => Done::Registered(ok),
                Raw::Notify { handle, len, value } => Done::Notify { handle, len, value },
            });
        }
    }
}

/// The result a call reports when it fails on the spot. The client waits for the
/// stack's result otherwise, so a failure has to be reported here.
fn failed(act: &Act) -> Done {
    match act {
        Act::Open { .. } => Done::Opened(false),
        Act::Mtu => Done::Mtu(false),
        Act::Search => Done::Searched(false),
        Act::Register { .. } => Done::Registered(false),
        Act::Enable { .. } | Act::Write { .. } => Done::Written(false),
        Act::Read { .. } => Done::Read {
            ok: false,
            len: 0,
            value: [0; VALUE_MAX],
        },
        Act::Close | Act::Cancel => Done::Closed,
    }
}

/// Switches on notifications for the characteristic at `handle`. The descriptor is
/// found in the service cache, which discovery has filled by now, then written.
fn enable(gattc_if: u8, conn: u16, handle: u16) -> Option<Done> {
    let uuid = sys::esp_bt_uuid_t {
        len: 2,
        uuid: sys::esp_bt_uuid_t__bindgen_ty_1 { uuid16: CCCD_UUID },
    };
    let mut descr = sys::esp_gattc_descr_elem_t::default();
    let mut count: u16 = 1;
    // SAFETY: `descr` and `count` are locals that live for the call, and both are
    // non-null. The stack writes at most `count` descriptors, which is one.
    let status = unsafe {
        sys::esp_ble_gattc_get_descr_by_char_handle(
            gattc_if, conn, handle, uuid, &mut descr, &mut count,
        )
    };
    if status != sys::esp_gatt_status_t_ESP_GATT_OK || count == 0 {
        return Some(Done::Written(false));
    }
    let mut on = [1u8, 0];
    let ok = checked(
        "enable notifications",
        // SAFETY: `on` is a local two-byte array, and the length passed is two. It
        // stays valid for the call, and the stack copies it before returning.
        sys::esp!(unsafe {
            sys::esp_ble_gattc_write_char_descr(
                gattc_if,
                conn,
                descr.handle,
                2,
                on.as_mut_ptr(),
                sys::esp_gatt_write_type_t_ESP_GATT_WRITE_TYPE_RSP,
                sys::esp_gatt_auth_req_t_ESP_GATT_AUTH_REQ_NONE,
            )
        }),
    );
    if ok {
        None
    } else {
        Some(Done::Written(false))
    }
}

/// The GATT client callback. Bluedroid calls it on its own task, with pointers to
/// the event's parameters that are valid for this call only. It copies what it
/// keeps and returns. It never blocks and never writes to serial.
///
/// SAFETY: this function is only passed to `esp_ble_gattc_register_callback`,
/// which Bluedroid calls with the event's parameters. `param` is checked for null
/// before any read. Each union member is read only for the event that sets it, and
/// each byte pointer is checked for null and its length before it is read.
unsafe extern "C" fn on_gattc(
    event: sys::esp_gattc_cb_event_t,
    gattc_if: sys::esp_gatt_if_t,
    param: *mut sys::esp_ble_gattc_cb_param_t,
) {
    if param.is_null() {
        return;
    }
    let raw = match event {
        sys::esp_gattc_cb_event_t_ESP_GATTC_REG_EVT => {
            // SAFETY: `param` is non-null and this is REG, so `reg` is the member
            // Bluedroid set. The struct is copied whole.
            let reg = unsafe { (*param).reg };
            Raw::App {
                ok: reg.status == sys::esp_gatt_status_t_ESP_GATT_OK,
                gattc_if,
            }
        }
        sys::esp_gattc_cb_event_t_ESP_GATTC_OPEN_EVT => {
            // SAFETY: `param` is non-null and this is OPEN, so `open` is the member set.
            let open = unsafe { (*param).open };
            Raw::Opened {
                ok: open.status == sys::esp_gatt_status_t_ESP_GATT_OK,
                conn: open.conn_id,
            }
        }
        sys::esp_gattc_cb_event_t_ESP_GATTC_CLOSE_EVT
        | sys::esp_gattc_cb_event_t_ESP_GATTC_CANCEL_OPEN_EVT => Raw::Closed,
        sys::esp_gattc_cb_event_t_ESP_GATTC_CFG_MTU_EVT => {
            // SAFETY: `param` is non-null and this is CFG_MTU, so `cfg_mtu` is set.
            let mtu = unsafe { (*param).cfg_mtu };
            Raw::Mtu(mtu.status == sys::esp_gatt_status_t_ESP_GATT_OK)
        }
        sys::esp_gattc_cb_event_t_ESP_GATTC_SEARCH_CMPL_EVT => {
            // SAFETY: `param` is non-null and this is SEARCH_CMPL, so `search_cmpl` is set.
            let search = unsafe { (*param).search_cmpl };
            Raw::Searched(search.status == sys::esp_gatt_status_t_ESP_GATT_OK)
        }
        sys::esp_gattc_cb_event_t_ESP_GATTC_WRITE_CHAR_EVT
        | sys::esp_gattc_cb_event_t_ESP_GATTC_WRITE_DESCR_EVT => {
            // SAFETY: `param` is non-null and this is a write, so `write` is the member set.
            let write = unsafe { (*param).write };
            Raw::Written(write.status == sys::esp_gatt_status_t_ESP_GATT_OK)
        }
        sys::esp_gattc_cb_event_t_ESP_GATTC_READ_CHAR_EVT => {
            // SAFETY: `param` is non-null and this is READ_CHAR, so `read` is the member set.
            let read = unsafe { (*param).read };
            let length = usize::from(read.value_len);
            let mut value = [0u8; VALUE_MAX];
            let ok = read.status == sys::esp_gatt_status_t_ESP_GATT_OK
                && length <= VALUE_MAX
                && (length == 0 || !read.value.is_null());
            if ok && length > 0 {
                // SAFETY: `read.value` was checked for null, and Bluedroid keeps
                // `length` bytes there for this callback. `length` is no more than
                // `VALUE_MAX`, which is the size of `value`.
                let bytes = unsafe { core::slice::from_raw_parts(read.value, length) };
                value[..length].copy_from_slice(bytes);
            }
            Raw::Read {
                ok,
                len: if ok { length as u8 } else { 0 },
                value,
            }
        }
        sys::esp_gattc_cb_event_t_ESP_GATTC_REG_FOR_NOTIFY_EVT => {
            // SAFETY: `param` is non-null and this is REG_FOR_NOTIFY, so that member is set.
            let registered = unsafe { (*param).reg_for_notify };
            Raw::Registered(registered.status == sys::esp_gatt_status_t_ESP_GATT_OK)
        }
        sys::esp_gattc_cb_event_t_ESP_GATTC_NOTIFY_EVT => {
            // SAFETY: `param` is non-null and this is NOTIFY, so `notify` is the member set.
            let notify = unsafe { (*param).notify };
            let length = usize::from(notify.value_len);
            if length > NOTIFY_MAX || (length > 0 && notify.value.is_null()) {
                return;
            }
            let mut value = [0u8; NOTIFY_MAX];
            if length > 0 {
                // SAFETY: `notify.value` was checked for null, and Bluedroid keeps
                // `length` bytes there for this callback. `length` is no more than
                // `NOTIFY_MAX`, which is the size of `value`.
                let bytes = unsafe { core::slice::from_raw_parts(notify.value, length) };
                value[..length].copy_from_slice(bytes);
            }
            Raw::Notify {
                handle: notify.handle,
                len: length as u8,
                value,
            }
        }
        _ => return,
    };
    push(raw);
}
