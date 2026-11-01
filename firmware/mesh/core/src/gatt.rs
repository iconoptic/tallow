//! The GATT client on a node, as a state machine with no radio in it.
//!
//! The host asks for a GATT operation with a downward `Gatt` frame. The root
//! sends it to one node, and that node runs it on its one BLE connection. The
//! answer and any notifications go back up as `GattUp` frames. The radio is in
//! `firmware/mesh/node/src/gatt.rs`. It reports each result as a `Done`, and this
//! module says what the node does next, either an answer to send or the next
//! `Act` for the radio.
//!
//! Rules this module keeps:
//! - A Connect goes ahead only to an address on the whitelist the host last sent.
//!   The list starts empty, so every Connect is refused until one arrives.
//! - One connection. A second Connect answers `Busy`.
//! - One request in flight. A repeat of its id is ignored, so it never reaches the
//!   radio twice. A repeat of the id of the last finished request gets that answer
//!   again, so a lost answer costs nothing. The host must not reuse an id for a
//!   new request.
//! - A request that outlives its timeout answers `TimedOut`. The link is then
//!   dropped, because the stack may still finish the old operation. A result that
//!   arrives for an operation no longer in flight is ignored.
//!
//! The byte layouts are in `core/README.md`.

use crate::wire::MAX_PAYLOAD;

/// Addresses in one whitelist. A new list replaces the old one whole.
pub const WHITELIST: usize = 8;
/// Most bytes one write or read carries.
pub const VALUE_MAX: usize = 170;
/// Most bytes one notification carries: the payload less the link id, kind, and handle.
pub const NOTIFY_MAX: usize = MAX_PAYLOAD - 5;
/// A Connect runs the whole setup: open, MTU, and service discovery.
pub const CONNECT_MS: u32 = 12_000;
/// Every other request.
pub const REQUEST_MS: u32 = 4_000;

const OP_CONNECT: u8 = 0;
const OP_WRITE: u8 = 1;
const OP_READ: u8 = 2;
const OP_SUBSCRIBE: u8 = 3;
const OP_DISCONNECT: u8 = 4;
const OP_WHITELIST: u8 = 5;

/// The kind byte of an answer, notification, and closed link, in a `GattUp` payload.
pub const KIND_ANSWER: u8 = 1;
pub const KIND_NOTIFY: u8 = 2;
pub const KIND_CLOSED: u8 = 3;

/// The result of one request, as the host reads it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Status {
    Ok = 0,
    /// The address is not on the whitelist.
    Refused = 1,
    /// Another request or a connection is in the way.
    Busy = 2,
    NotConnected = 3,
    TimedOut = 4,
    /// The stack reported an error, or the link closed under the request.
    Failed = 5,
    /// The request could not be read.
    Malformed = 6,
}

/// A request, decoded. The id is returned beside it by `parse`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Request {
    Connect {
        address: [u8; 6],
        address_kind: u8,
    },
    Write {
        handle: u16,
        len: u8,
        value: [u8; VALUE_MAX],
    },
    Read {
        handle: u16,
    },
    Subscribe {
        handle: u16,
    },
    Disconnect,
    Whitelist {
        count: u8,
        addresses: [[u8; 6]; WHITELIST],
    },
}

/// What the radio must do next. The firmware performs it and reports back with a
/// `Done`. Each one is taken once, from `Client::take_act`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Act {
    /// Open a connection. `address_kind` is 0 public or 1 random.
    Open {
        address: [u8; 6],
        address_kind: u8,
    },
    /// Negotiate the MTU on the open connection.
    Mtu,
    /// Discover services on the open connection.
    Search,
    /// Register for notifications on the characteristic at `handle`.
    Register {
        handle: u16,
    },
    /// Turn on notifications by writing the descriptor of the characteristic at `handle`.
    Enable {
        handle: u16,
    },
    Write {
        handle: u16,
        len: u8,
        value: [u8; VALUE_MAX],
    },
    Read {
        handle: u16,
    },
    /// Close the connection.
    Close,
    /// Stop an open that has not connected yet.
    Cancel,
}

/// What the radio reports. Each variant answers one `Act`, except `Notify` and
/// `Closed`, which the link raises on its own.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Done {
    Opened(bool),
    Mtu(bool),
    Searched(bool),
    Registered(bool),
    Written(bool),
    Read {
        ok: bool,
        len: u8,
        value: [u8; VALUE_MAX],
    },
    Closed,
    Notify {
        handle: u16,
        len: u8,
        value: [u8; NOTIFY_MAX],
    },
}

/// What goes up. `link` is the id of the Connect that opened the connection.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Reply {
    Answer {
        id: u16,
        status: Status,
        len: u8,
        value: [u8; VALUE_MAX],
    },
    Notify {
        link: u16,
        handle: u16,
        len: u8,
        value: [u8; NOTIFY_MAX],
    },
    /// The link closed with no request in flight.
    Closed { link: u16 },
}

impl Reply {
    /// Writes the payload of the `GattUp` frame and returns its length.
    pub fn encode(&self, out: &mut [u8; MAX_PAYLOAD]) -> usize {
        match *self {
            Reply::Answer {
                id,
                status,
                len,
                value,
            } => {
                let len = (len as usize).min(VALUE_MAX);
                out[..2].copy_from_slice(&id.to_le_bytes());
                out[2] = KIND_ANSWER;
                out[3] = status as u8;
                out[4..4 + len].copy_from_slice(&value[..len]);
                4 + len
            }
            Reply::Notify {
                link,
                handle,
                len,
                value,
            } => {
                let len = (len as usize).min(NOTIFY_MAX);
                out[..2].copy_from_slice(&link.to_le_bytes());
                out[2] = KIND_NOTIFY;
                out[3..5].copy_from_slice(&handle.to_le_bytes());
                out[5..5 + len].copy_from_slice(&value[..len]);
                5 + len
            }
            Reply::Closed { link } => {
                out[..2].copy_from_slice(&link.to_le_bytes());
                out[2] = KIND_CLOSED;
                3
            }
        }
    }
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*data.get(at)?, *data.get(at + 1)?]))
}

fn address_at(data: &[u8], at: usize) -> Option<[u8; 6]> {
    data.get(at..at + 6)?.try_into().ok()
}

/// Reads one request. The layout is: id u16 LE, operation u8, then the body for
/// that operation. Returns the id and the request, or `None` if the request is
/// too short, names an unknown operation, or has a body of the wrong length.
pub fn parse(data: &[u8]) -> Option<(u16, Request)> {
    let id = u16_at(data, 0)?;
    let op = *data.get(2)?;
    let body = data.get(3..)?;
    let request = match op {
        OP_CONNECT => {
            let address = address_at(body, 0)?;
            let address_kind = *body.get(6)?;
            if body.len() != 7 || address_kind > 1 {
                return None;
            }
            Request::Connect {
                address,
                address_kind,
            }
        }
        OP_WRITE => {
            let handle = u16_at(body, 0)?;
            let bytes = body.get(2..)?;
            if bytes.len() > VALUE_MAX {
                return None;
            }
            let mut value = [0u8; VALUE_MAX];
            value[..bytes.len()].copy_from_slice(bytes);
            Request::Write {
                handle,
                len: bytes.len() as u8,
                value,
            }
        }
        OP_READ | OP_SUBSCRIBE => {
            if body.len() != 2 {
                return None;
            }
            let handle = u16_at(body, 0)?;
            if op == OP_READ {
                Request::Read { handle }
            } else {
                Request::Subscribe { handle }
            }
        }
        OP_DISCONNECT => {
            if !body.is_empty() {
                return None;
            }
            Request::Disconnect
        }
        OP_WHITELIST => {
            let count = *body.first()?;
            if count as usize > WHITELIST || body.len() != 1 + 6 * count as usize {
                return None;
            }
            let mut addresses = [[0u8; 6]; WHITELIST];
            for (slot, chunk) in addresses.iter_mut().zip(body.get(1..)?.chunks_exact(6)) {
                slot.copy_from_slice(chunk);
            }
            Request::Whitelist { count, addresses }
        }
        _ => return None,
    };
    Some((id, request))
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Link {
    Idle,
    /// An open is in progress, and no connection exists yet.
    Opening,
    /// Connected. The MTU and services are being set up.
    Setup,
    Open,
    /// A close or a cancel was asked for, and the radio has not confirmed it.
    Closing,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Op {
    Connect,
    Write,
    Read,
    Subscribe,
    Disconnect,
}

/// The radio result the request in flight waits for next.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Waiting {
    Opened,
    Mtu,
    Searched,
    Registered,
    Written,
    Read,
    Closed,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Pending {
    id: u16,
    op: Op,
    waiting: Waiting,
    handle: u16,
    started: u32,
}

/// The node's GATT client. It holds the whitelist, the one connection, the one
/// request in flight, and the last answer, so a repeated request can be answered
/// again.
pub struct Client {
    whitelist: [[u8; 6]; WHITELIST],
    count: usize,
    link: Link,
    link_id: u16,
    /// Who the link is to, so a new whitelist can end it.
    peer: [u8; 6],
    closing_since: u32,
    pending: Option<Pending>,
    last: Option<Reply>,
    act: Option<Act>,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub const fn new() -> Self {
        Self {
            whitelist: [[0; 6]; WHITELIST],
            count: 0,
            peer: [0; 6],
            link: Link::Idle,
            link_id: 0,
            closing_since: 0,
            pending: None,
            last: None,
            act: None,
        }
    }

    /// True while a connection exists or one is being set up or torn down. The
    /// radio pauses Classic inquiry and paging while this holds.
    pub fn link_busy(&self) -> bool {
        self.link != Link::Idle
    }

    /// The radio action to perform now, if any. Each one is returned once.
    pub fn take_act(&mut self) -> Option<Act> {
        self.act.take()
    }

    fn on_list(&self, address: &[u8; 6]) -> bool {
        self.whitelist[..self.count].contains(address)
    }

    fn finish(&mut self, id: u16, status: Status, len: u8, value: [u8; VALUE_MAX]) -> Reply {
        let reply = Reply::Answer {
            id,
            status,
            len,
            value,
        };
        self.last = Some(reply);
        reply
    }

    fn answer(&mut self, id: u16, status: Status) -> Reply {
        self.finish(id, status, 0, [0; VALUE_MAX])
    }

    /// Ends the request in flight with `status`, and returns its answer.
    fn settle(
        &mut self,
        id: u16,
        status: Status,
        len: u8,
        value: [u8; VALUE_MAX],
    ) -> Option<Reply> {
        self.pending = None;
        Some(self.finish(id, status, len, value))
    }

    fn start(&mut self, pending: Pending, act: Act) {
        self.pending = Some(pending);
        self.act = Some(act);
    }

    /// Moves the request in flight on to its next radio step.
    fn advance(&mut self, pending: Pending, waiting: Waiting, act: Act) {
        self.start(Pending { waiting, ..pending }, act);
    }

    /// Drops the link, and asks the radio to. A link still opening is cancelled,
    /// and any other is closed. The radio confirms with `Done::Closed`.
    fn drop_link(&mut self, now: u32) {
        self.act = Some(if self.link == Link::Opening {
            Act::Cancel
        } else {
            Act::Close
        });
        self.link = Link::Closing;
        self.closing_since = now;
    }

    /// Ends a request that failed while setting up, and drops the link it made.
    fn abort(&mut self, now: u32, id: u16, status: Status) -> Option<Reply> {
        self.pending = None;
        self.drop_link(now);
        Some(self.answer(id, status))
    }

    /// Handles one request from the host. Returns the answer to send up now, if
    /// there is one. A request that needs the radio returns `None`, and its answer
    /// comes from `done` once the radio reports back.
    pub fn request(&mut self, now: u32, data: &[u8]) -> Option<Reply> {
        let id = u16_at(data, 0)?;
        if self.pending.is_some_and(|pending| pending.id == id) {
            return None;
        }
        if let Some(Reply::Answer { id: seen, .. }) = self.last {
            if seen == id {
                return self.last;
            }
        }
        let Some((_, request)) = parse(data) else {
            return Some(self.answer(id, Status::Malformed));
        };
        if let Request::Whitelist { count, addresses } = request {
            self.whitelist = addresses;
            self.count = count as usize;
            // A node stays connected only to a listed device, so a list that drops
            // this one ends the link. Whatever was in flight goes with it, and the
            // host sees that request time out.
            if !matches!(self.link, Link::Idle | Link::Closing) && !self.on_list(&self.peer) {
                self.pending = None;
                self.drop_link(now);
            }
            return Some(self.answer(id, Status::Ok));
        }
        if self.pending.is_some() {
            return Some(self.answer(id, Status::Busy));
        }
        let fresh = |op, waiting, handle| Pending {
            id,
            op,
            waiting,
            handle,
            started: now,
        };
        match request {
            Request::Connect {
                address,
                address_kind,
            } => {
                if self.link != Link::Idle {
                    return Some(self.answer(id, Status::Busy));
                }
                if !self.on_list(&address) {
                    return Some(self.answer(id, Status::Refused));
                }
                self.link = Link::Opening;
                self.link_id = id;
                self.peer = address;
                self.start(
                    fresh(Op::Connect, Waiting::Opened, 0),
                    Act::Open {
                        address,
                        address_kind,
                    },
                );
                None
            }
            Request::Disconnect => {
                if self.link != Link::Open {
                    return Some(self.answer(id, Status::NotConnected));
                }
                self.link = Link::Closing;
                self.closing_since = now;
                self.start(fresh(Op::Disconnect, Waiting::Closed, 0), Act::Close);
                None
            }
            Request::Write { handle, len, value } => {
                if self.link != Link::Open {
                    return Some(self.answer(id, Status::NotConnected));
                }
                self.start(
                    fresh(Op::Write, Waiting::Written, 0),
                    Act::Write { handle, len, value },
                );
                None
            }
            Request::Read { handle } => {
                if self.link != Link::Open {
                    return Some(self.answer(id, Status::NotConnected));
                }
                self.start(fresh(Op::Read, Waiting::Read, 0), Act::Read { handle });
                None
            }
            Request::Subscribe { handle } => {
                if self.link != Link::Open {
                    return Some(self.answer(id, Status::NotConnected));
                }
                self.start(
                    fresh(Op::Subscribe, Waiting::Registered, handle),
                    Act::Register { handle },
                );
                None
            }
            Request::Whitelist { .. } => None,
        }
    }

    /// Handles one result from the radio. Returns the answer or notification to
    /// send up now, if any. A result for something not in flight is ignored.
    pub fn done(&mut self, now: u32, done: Done) -> Option<Reply> {
        let pending = self.pending;
        match (done, self.link, pending) {
            (Done::Notify { handle, len, value }, Link::Open, _) => {
                if usize::from(len) > NOTIFY_MAX {
                    return None;
                }
                Some(Reply::Notify {
                    link: self.link_id,
                    handle,
                    len,
                    value,
                })
            }
            (Done::Opened(ok), Link::Opening, Some(pending))
                if pending.waiting == Waiting::Opened =>
            {
                if ok {
                    self.link = Link::Setup;
                    self.advance(pending, Waiting::Mtu, Act::Mtu);
                    None
                } else {
                    self.pending = None;
                    self.link = Link::Idle;
                    Some(self.answer(pending.id, Status::Failed))
                }
            }
            (Done::Opened(true), Link::Closing, _) => {
                // Opened after we gave up. Close it, so no connection is left over.
                self.act = Some(Act::Close);
                None
            }
            (Done::Opened(false), Link::Closing, _) => {
                self.link = Link::Idle;
                None
            }
            (Done::Mtu(ok), Link::Setup, Some(pending)) if pending.waiting == Waiting::Mtu => {
                if ok {
                    self.advance(pending, Waiting::Searched, Act::Search);
                    None
                } else {
                    self.abort(now, pending.id, Status::Failed)
                }
            }
            (Done::Searched(ok), Link::Setup, Some(pending))
                if pending.waiting == Waiting::Searched =>
            {
                if ok {
                    self.link = Link::Open;
                    self.settle(pending.id, Status::Ok, 0, [0; VALUE_MAX])
                } else {
                    self.abort(now, pending.id, Status::Failed)
                }
            }
            (Done::Registered(ok), Link::Open, Some(pending))
                if pending.waiting == Waiting::Registered =>
            {
                if ok {
                    self.advance(
                        pending,
                        Waiting::Written,
                        Act::Enable {
                            handle: pending.handle,
                        },
                    );
                    None
                } else {
                    self.settle(pending.id, Status::Failed, 0, [0; VALUE_MAX])
                }
            }
            (Done::Written(ok), Link::Open, Some(pending))
                if pending.waiting == Waiting::Written =>
            {
                let status = if ok { Status::Ok } else { Status::Failed };
                self.settle(pending.id, status, 0, [0; VALUE_MAX])
            }
            (Done::Read { ok, len, value }, Link::Open, Some(pending))
                if pending.waiting == Waiting::Read =>
            {
                if ok && usize::from(len) <= VALUE_MAX {
                    self.settle(pending.id, Status::Ok, len, value)
                } else {
                    self.settle(pending.id, Status::Failed, 0, [0; VALUE_MAX])
                }
            }
            (Done::Closed, _, _) => self.on_closed(),
            _ => None,
        }
    }

    /// The link is gone. A request in flight ends with a status that says why. With
    /// none in flight, a link that was open is reported once.
    fn on_closed(&mut self) -> Option<Reply> {
        let was = self.link;
        self.link = Link::Idle;
        match self.pending.take() {
            Some(pending) => {
                let status = match pending.op {
                    Op::Disconnect => Status::Ok,
                    Op::Connect => Status::Failed,
                    Op::Write | Op::Read | Op::Subscribe => Status::NotConnected,
                };
                Some(self.answer(pending.id, status))
            }
            None if was == Link::Open => Some(Reply::Closed { link: self.link_id }),
            None => None,
        }
    }

    /// Runs the clock. A request past its timeout answers `TimedOut` and drops the
    /// link. A link left closing with no answer from the radio goes idle, so a
    /// lost confirmation cannot block connects for good.
    pub fn tick(&mut self, now: u32) -> Option<Reply> {
        let Some(pending) = self.pending else {
            if self.link == Link::Closing && now.wrapping_sub(self.closing_since) >= REQUEST_MS {
                self.link = Link::Idle;
            }
            return None;
        };
        let limit = if pending.op == Op::Connect {
            CONNECT_MS
        } else {
            REQUEST_MS
        };
        if now.wrapping_sub(pending.started) < limit {
            return None;
        }
        self.pending = None;
        if self.link != Link::Idle {
            self.drop_link(now);
        }
        Some(self.answer(pending.id, Status::TimedOut))
    }
}
