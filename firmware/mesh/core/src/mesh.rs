//! Routing. Each node chooses a parent from its neighbours' beacons.
//!
//! A beacon says how many hops its sender is from the host. A node takes the
//! neighbour that gives it the cheapest path, where cost counts hops and missed
//! beacons. Reports go to the parent as unicast, so the radio retries them.
//! Commands flood, and duplicates stop at the first node that has seen them.
//! When a parent goes quiet or stops acknowledging, the node drops it, says so in
//! its next beacon, and takes the next best neighbour.
//!
//! Downward frames go from the root to one node. Each node learns which neighbour
//! an origin's upward frames came from, and unicasts a downward frame that way.

use crate::wire::{Command, Frame, FrameKind, Sealer, Watch, MAX_FRAME, MAX_PAYLOAD, WATCH_MAX};

pub const NO_RANK: u8 = 255;
pub const MAX_RANK: u8 = 15;
pub const DEFAULT_TTL: u8 = 16;
pub const BEACON_MS: u32 = 2_000;
/// A neighbour unheard for this long is gone. Beacons get no retries and compete
/// with Bluetooth for the radio, so this allows five to go missing. A parent that
/// dies under traffic is caught sooner, by failed acknowledgements.
pub const NEIGHBOUR_TIMEOUT_MS: u32 = 11_000;
pub const ROOT_TIMEOUT_MS: u32 = 5_000;
pub const MAX_NEIGHBOURS: usize = 12;
pub const TX_QUEUE: usize = 16;
pub const SEEN: usize = 96;
/// A frame this far behind its origin's newest, in the same boot, is a replay.
pub const REPLAY_WINDOW: u32 = 64;
/// An origin with no accepted frame for this long is forgotten, so a board whose
/// NVS was erased can rejoin. A replay of such a board is accepted once.
pub const ORIGIN_SILENCE_MS: u32 = 600_000;
pub const MAX_ORIGINS: usize = 16;
/// Board ids a node refuses as neighbours. See `Mesh::deny`.
pub const DENY_LIST: usize = 4;
pub const TX_RETRIES: u8 = 2;
/// Failed unicasts in a row before a parent is dropped.
pub const PARENT_FAILURES: u8 = 4;
const HYSTERESIS: u16 = 60;
const BEACON_PAYLOAD: usize = 11;
/// Parent-change events kept while a node has no parent. See `flush_routes`.
pub const ROUTE_LOG: usize = 4;
/// uptime seconds, old parent, new parent, reason. Zero in a parent means none.
const ROUTE_PAYLOAD: usize = 13;
/// Origins remembered for downward frames. See `Mesh::learn`.
pub const ROUTES: usize = 32;
/// A route not refreshed by an upward frame for this long is no longer used.
pub const ROUTE_TIMEOUT_MS: u32 = 60_000;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Dest {
    Broadcast,
    Peer([u8; 6]),
}

#[derive(Copy, Clone)]
pub struct Tx {
    pub dest: Dest,
    pub length: u8,
    pub retries: u8,
    pub data: [u8; MAX_FRAME],
}

impl Tx {
    pub fn bytes(&self) -> &[u8] {
        &self.data[..self.length as usize]
    }
}

const NO_TX: Tx = Tx {
    dest: Dest::Broadcast,
    length: 0,
    retries: 0,
    data: [0; MAX_FRAME],
};

/// What the mesh hands to the firmware.
#[derive(Debug, Eq, PartialEq)]
pub enum Event<'a> {
    /// At the root: a frame that reached the host's node.
    Up {
        frame: Frame,
        payload: &'a [u8],
        rssi: i8,
    },
    /// At every node: settings from the host.
    Command(Command),
    /// At every node: the Classic addresses to page. The first `count` entries
    /// are the list, and a count of 0 means clear it. See `Watch`.
    Watch {
        count: u8,
        addresses: [[u8; 6]; WATCH_MAX],
    },
    /// The parent changed, once per change. `None` means no route.
    Parent {
        old: Option<u32>,
        new: Option<u32>,
        reason: Reason,
    },
    /// At the target node: a downward frame. `payload` is what followed its
    /// four-byte target id.
    Down { frame: Frame, payload: &'a [u8] },
}

/// Frames that travel from the root to one node. Each hop unicasts them to the
/// next hop in the route table. `Ota` joins this list when it exists. `GattUp`
/// does not: its answers travel up like any report.
pub fn is_down(kind: FrameKind) -> bool {
    matches!(kind, FrameKind::Ping | FrameKind::Gatt)
}

/// Why a parent changed. The code is the value sent in a Route frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Reason {
    /// The parent's beacons stopped for `NEIGHBOUR_TIMEOUT_MS`.
    BeaconTimeout = 0,
    /// `PARENT_FAILURES` unicasts in a row went unacknowledged.
    FailedAcks = 1,
    /// A neighbour gave a cheaper path, or the node had no parent before.
    BetterPath = 2,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct RouteEvent {
    uptime_s: u32,
    old: u32,
    new: u32,
    reason: u8,
}

#[derive(Copy, Clone, Debug)]
pub struct Neighbour {
    pub id: u32,
    pub mac: [u8; 6],
    pub rank: u8,
    pub parent: u32,
    pub rssi: i8,
    pub last_ms: u32,
    /// One bit per recent beacon number, newest in bit 0.
    heard: u16,
    last_beacon: u16,
}

impl Neighbour {
    pub fn quality(&self) -> u32 {
        self.heard.count_ones()
    }

    fn cost(&self) -> u16 {
        let hops = (self.rank as u16 + 1) * 100;
        let loss = (16 - self.quality() as u16) * 20;
        let weak = if self.rssi < -85 { 80 } else { 0 };
        hops + loss + weak
    }
}

/// The neighbour an origin's upward frames last came through.
#[derive(Copy, Clone)]
struct Route {
    id: u32,
    next_mac: [u8; 6],
    last_ms: u32,
}

#[derive(Copy, Clone)]
struct Origin {
    id: u32,
    boot: u32,
    newest: u32,
    /// When this origin last had a frame accepted. Refused frames do not count.
    last_ms: u32,
}

pub struct Mesh {
    pub id: u32,
    pub mac: [u8; 6],
    sealer: Sealer,
    boot: u32,
    seq: u32,
    beacon_no: u16,
    next_beacon_ms: u32,
    root_until_ms: Option<u32>,
    pub rank: u8,
    parent: Option<usize>,
    parent_failures: u8,
    neighbours: [Option<Neighbour>; MAX_NEIGHBOURS],
    seen: [u32; SEEN],
    seen_next: usize,
    origins: [Option<Origin>; MAX_ORIGINS],
    origins_next: usize,
    queue: [Tx; TX_QUEUE],
    queue_head: usize,
    queue_len: usize,
    pub rx_bad: u32,
    pub rx_dup: u32,
    pub queue_drops: u32,
    pub parent_changes: u32,
    route_log: [RouteEvent; ROUTE_LOG],
    route_count: usize,
    route_next: usize,
    /// Zero is an empty slot, so id 0 cannot be denied.
    deny: [u32; DENY_LIST],
    routes: [Option<Route>; ROUTES],
    /// Downward frames dropped for want of a live route.
    pub down_drops: u32,
    /// The id of the newest Watch list accepted. See `newer_watch`.
    watch: Option<u16>,
    /// Watch lists this boot has sent from the root. See `send_watch`.
    watch_no: u8,
}

/// Boot ids are `(counter << 12) | random12`. The counter is 20 bits, kept in NVS
/// and raised once per boot, so boot ids order boots. The random bits tell apart
/// two boots that were seeded with the same counter.
const COUNTER_MASK: u32 = (1 << 20) - 1;

/// The counter to store and the boot id for this boot. `stored` is the counter
/// read from NVS, or `None` if there is none. With none, the counter is seeded from
/// the high 20 bits of `random`, with the top bit of those 20 clear. It stays clear
/// of the low 12 bits the boot id takes from `random`, so the two are independent.
/// Otherwise the counter is one more than `stored`, and it wraps after 2^20 boots.
pub fn next_boot(stored: Option<u32>, random: u32) -> (u32, u32) {
    let counter = match stored {
        None => (random >> 12) & 0x7_FFFF,
        Some(counter) => (counter & COUNTER_MASK).wrapping_add(1) & COUNTER_MASK,
    };
    (counter, (counter << 12) | (random & 0xFFF))
}

/// The counter part of a boot id. Larger means a later boot.
pub fn counter_of(boot: u32) -> u32 {
    boot >> 12
}

fn id_of(mac: &[u8; 6]) -> u32 {
    u32::from_le_bytes([mac[2], mac[3], mac[4], mac[5]])
}

impl Mesh {
    pub fn new(mac: [u8; 6], key: &[u8; 32], boot: u32) -> Self {
        Self {
            id: id_of(&mac),
            mac,
            sealer: Sealer::new(key),
            boot,
            seq: 0,
            beacon_no: 0,
            // Spread first beacons so boards powered together do not collide.
            next_beacon_ms: boot % BEACON_MS,
            root_until_ms: None,
            rank: NO_RANK,
            parent: None,
            parent_failures: 0,
            neighbours: [None; MAX_NEIGHBOURS],
            seen: [0; SEEN],
            seen_next: 0,
            origins: [None; MAX_ORIGINS],
            origins_next: 0,
            queue: [NO_TX; TX_QUEUE],
            queue_head: 0,
            queue_len: 0,
            rx_bad: 0,
            rx_dup: 0,
            queue_drops: 0,
            parent_changes: 0,
            route_log: [RouteEvent::default(); ROUTE_LOG],
            route_count: 0,
            route_next: 0,
            deny: [0; DENY_LIST],
            routes: [None; ROUTES],
            down_drops: 0,
            watch: None,
            watch_no: 0,
        }
    }

    pub fn is_root(&self, now_ms: u32) -> bool {
        self.root_until_ms
            .is_some_and(|until| until.wrapping_sub(now_ms) as i32 > 0)
    }

    /// The host calls this over serial about once a second. Being plugged into
    /// the host is what makes a node the root, so any board can be one.
    pub fn host_heartbeat(&mut self, now_ms: u32) {
        self.root_until_ms = Some(now_ms.wrapping_add(ROOT_TIMEOUT_MS));
        self.rank = 0;
        self.parent = None;
    }

    /// Refuses beacons from `id`, so that board never becomes a neighbour. The
    /// bench uses it to force a chain. Returns false for id 0 or a full list.
    /// A full list ignores the new id and does not grow.
    pub fn deny(&mut self, id: u32) -> bool {
        if id == 0 {
            return false;
        }
        if self.denied(id) {
            return true;
        }
        match self.deny.iter_mut().find(|slot| **slot == 0) {
            Some(slot) => {
                *slot = id;
                true
            }
            None => false,
        }
    }

    /// Forgets every denied id.
    pub fn clear_deny(&mut self) {
        self.deny = [0; DENY_LIST];
    }

    fn denied(&self, id: u32) -> bool {
        id != 0 && self.deny.contains(&id)
    }

    pub fn parent(&self) -> Option<&Neighbour> {
        self.neighbours[self.parent?].as_ref()
    }

    pub fn neighbours(&self) -> impl Iterator<Item = &Neighbour> {
        self.neighbours.iter().flatten()
    }

    pub fn queued(&self) -> usize {
        self.queue_len
    }

    fn head(&mut self, kind: FrameKind, ttl: u8) -> Frame {
        self.seq = self.seq.wrapping_add(1);
        Frame {
            kind,
            ttl,
            hops: 0,
            origin: self.id,
            boot: self.boot,
            seq: self.seq,
        }
    }

    fn enqueue(&mut self, dest: Dest, data: &[u8]) -> bool {
        if self.queue_len == TX_QUEUE {
            self.queue_drops = self.queue_drops.wrapping_add(1);
            return false;
        }
        let slot = (self.queue_head + self.queue_len) % TX_QUEUE;
        let entry = &mut self.queue[slot];
        entry.dest = dest;
        entry.length = data.len() as u8;
        entry.retries = 0;
        entry.data[..data.len()].copy_from_slice(data);
        self.queue_len += 1;
        true
    }

    /// The next frame to put on the air. It stays queued until `tx_done`.
    pub fn next_tx(&self) -> Option<&Tx> {
        (self.queue_len > 0).then(|| &self.queue[self.queue_head])
    }

    /// Reports how the frame from `next_tx` went.
    pub fn tx_done(&mut self, now_ms: u32, acked: bool, on_event: &mut dyn FnMut(Event)) {
        if self.queue_len == 0 {
            return;
        }
        let head = self.queue_head;
        let dest = self.queue[head].dest;
        if let Dest::Peer(mac) = dest {
            let to_parent = self.parent().is_some_and(|parent| parent.mac == mac);
            if acked {
                if to_parent {
                    self.parent_failures = 0;
                }
            } else if self.queue[head].retries < TX_RETRIES {
                self.queue[head].retries += 1;
                return;
            } else if to_parent {
                self.parent_failures = self.parent_failures.saturating_add(1);
                if self.parent_failures >= PARENT_FAILURES {
                    let before = self.parent().map(|parent| parent.id);
                    if let Some(index) = self.parent {
                        self.neighbours[index] = None;
                    }
                    self.parent = None;
                    self.parent_failures = 0;
                    self.reselect(now_ms, before, Reason::FailedAcks, on_event);
                }
            }
        }
        self.queue_head = (head + 1) % TX_QUEUE;
        self.queue_len -= 1;
    }

    /// Sends a frame from this node up to the host.
    pub fn send_up(
        &mut self,
        kind: FrameKind,
        payload: &[u8],
        now_ms: u32,
        on_event: &mut dyn FnMut(Event),
    ) -> bool {
        if self.is_root(now_ms) {
            let head = self.head(kind, DEFAULT_TTL);
            on_event(Event::Up {
                frame: head,
                payload,
                rssi: 0,
            });
            return true;
        }
        self.send_to_parent(kind, payload)
    }

    /// Seals a frame from this node and queues it for the parent.
    fn send_to_parent(&mut self, kind: FrameKind, payload: &[u8]) -> bool {
        let head = self.head(kind, DEFAULT_TTL);
        let Some(mac) = self.parent().map(|parent| parent.mac) else {
            self.queue_drops = self.queue_drops.wrapping_add(1);
            return false;
        };
        let mut out = [0u8; MAX_FRAME];
        match self.sealer.seal(&head, payload, &mut out) {
            Some(length) => self.enqueue(Dest::Peer(mac), &out[..length]),
            None => false,
        }
    }

    /// At the root: sends a downward frame to the node `target`, unicast along the
    /// routes learned from upward frames. The payload is `target` as four
    /// little-endian bytes, then `payload`. Returns false if this node is not the
    /// root, `kind` is not downward, the payload is too long, or no live route
    /// exists. A missing route counts in `down_drops`.
    pub fn send_down(&mut self, now_ms: u32, target: u32, kind: FrameKind, payload: &[u8]) -> bool {
        if !self.is_root(now_ms) || !is_down(kind) || target == self.id {
            return false;
        }
        if payload.len() > MAX_PAYLOAD - 4 {
            return false;
        }
        let Some(mac) = self.route(now_ms, target) else {
            self.down_drops = self.down_drops.wrapping_add(1);
            return false;
        };
        let mut body = [0u8; MAX_PAYLOAD];
        body[..4].copy_from_slice(&target.to_le_bytes());
        body[4..4 + payload.len()].copy_from_slice(payload);
        let head = self.head(kind, DEFAULT_TTL);
        let mut out = [0u8; MAX_FRAME];
        match self
            .sealer
            .seal(&head, &body[..4 + payload.len()], &mut out)
        {
            Some(length) => self.enqueue(Dest::Peer(mac), &out[..length]),
            None => false,
        }
    }

    /// Keeps the last `ROUTE_LOG` parent changes. Without a parent they wait
    /// here, and `flush_routes` sends them once one is back.
    fn log_route(&mut self, event: RouteEvent) {
        self.route_log[self.route_next] = event;
        self.route_next = (self.route_next + 1) % ROUTE_LOG;
        self.route_count = (self.route_count + 1).min(ROUTE_LOG);
    }

    /// Sends the logged changes as Route frames, oldest first. A node with no
    /// parent logs only its losses, since a join flushes straight away, so the
    /// log holds at most one change in practice.
    fn flush_routes(&mut self) {
        while self.route_count > 0 {
            let oldest = (self.route_next + ROUTE_LOG - self.route_count) % ROUTE_LOG;
            let event = self.route_log[oldest];
            self.route_count -= 1;
            let mut payload = [0u8; ROUTE_PAYLOAD];
            payload[0..4].copy_from_slice(&event.uptime_s.to_le_bytes());
            payload[4..8].copy_from_slice(&event.old.to_le_bytes());
            payload[8..12].copy_from_slice(&event.new.to_le_bytes());
            payload[12] = event.reason;
            self.send_to_parent(FrameKind::Route, &payload);
        }
    }

    /// At the root: floods settings to the whole mesh, including this node.
    pub fn send_command(&mut self, command: &Command, on_event: &mut dyn FnMut(Event)) -> bool {
        let head = self.head(FrameKind::Command, DEFAULT_TTL);
        let mut out = [0u8; MAX_FRAME];
        let Some(length) = self.sealer.seal(&head, &command.encode(), &mut out) else {
            return false;
        };
        self.remember(&head);
        on_event(Event::Command(*command));
        // Broadcast has no retries, so the root says it three times.
        let mut sent = false;
        for _ in 0..3 {
            sent |= self.enqueue(Dest::Broadcast, &out[..length]);
        }
        sent
    }

    /// At the root: replaces the paging list on every node, including this one.
    /// The list is the whole set of addresses to page, and an empty one clears it.
    /// Returns false for more than `WATCH_MAX` addresses, for a list this boot can
    /// no longer number (see `watch_id`), or when the queue has no room.
    pub fn send_watch(&mut self, addresses: &[[u8; 6]], on_event: &mut dyn FnMut(Event)) -> bool {
        if addresses.len() > WATCH_MAX {
            return false;
        }
        let id = self.watch_id();
        if !self.newer_watch(id) {
            return false;
        }
        let mut list = [[0u8; 6]; WATCH_MAX];
        list[..addresses.len()].copy_from_slice(addresses);
        let watch = Watch {
            id,
            count: addresses.len() as u8,
            addresses: list,
        };
        let head = self.head(FrameKind::Watch, DEFAULT_TTL);
        let mut body = [0u8; Watch::MAX_WIRE];
        let body_length = watch.encode(&mut body);
        let mut out = [0u8; MAX_FRAME];
        let Some(length) = self.sealer.seal(&head, &body[..body_length], &mut out) else {
            return false;
        };
        self.watch_no = (self.watch_no + 1).min(WATCH_MAX as u8 - 1);
        self.remember(&head);
        self.watch = Some(id);
        on_event(Event::Watch {
            count: watch.count,
            addresses: list,
        });
        // Broadcast has no retries, so the root says it three times.
        let mut sent = false;
        for _ in 0..3 {
            sent |= self.enqueue(Dest::Broadcast, &out[..length]);
        }
        sent
    }

    /// The id for the next list from this root. Boot ids count up, one per boot, so
    /// the high 12 bits come from the boot counter's low 12 bits and the low 4 bits
    /// number the lists this boot has sent. Consecutive boots always order correctly,
    /// because the counter steps the high bits by 16. A list from a boot 2048 or more
    /// boots earlier can order wrongly. A boot seeded from the radio can too, as
    /// `next_boot` says. After 16 lists in one boot, the id stops and `send_watch`
    /// refuses the rest.
    fn watch_id(&self) -> u16 {
        let high = (counter_of(self.boot) & 0xFFF) as u16;
        (high << 4) | self.watch_no as u16
    }

    /// True if a list with `id` is newer than the one accepted last. Ids compare
    /// with wrapping subtraction, as `fresh` compares sequence numbers. An equal id
    /// is not newer, so a repeat or a stale copy is ignored.
    fn newer_watch(&self, id: u16) -> bool {
        self.watch
            .is_none_or(|last| (id.wrapping_sub(last) as i16) > 0)
    }

    fn remember(&mut self, frame: &Frame) -> bool {
        // Mixing, not secrecy: a compact fingerprint of who and which frame.
        let print = frame.origin.rotate_left(13)
            ^ frame.boot.rotate_left(7)
            ^ frame.seq.wrapping_mul(0x9E37_79B1);
        let print = if print == 0 { 1 } else { print };
        if self.seen.contains(&print) {
            return false;
        }
        self.seen[self.seen_next] = print;
        self.seen_next = (self.seen_next + 1) % SEEN;
        true
    }

    /// False for a frame that is not new. Boot counters order boots. A frame from an
    /// older boot than the one stored for its origin is refused, and it leaves the
    /// entry alone. A newer boot replaces the entry. A frame from another boot with
    /// the same counter is refused too: it is a replay or a collision. A frame from
    /// the stored boot must be within `REPLAY_WINDOW` of the newest sequence number.
    ///
    /// An origin silent for `ORIGIN_SILENCE_MS` is treated as unknown, so any boot
    /// is accepted again. That leaves a window: a replay of a board silent that long
    /// is accepted once. The table is fixed size. When it is full, the next origin
    /// in turn is evicted, and that origin's replay protection is lost until it is
    /// heard again.
    // This table is RAM only. A board that just rebooted accepts one recorded frame
    // per origin, until that origin's next real frame replaces it, about 2 s for a
    // neighbour's beacon. Closing that would mean writing flash for every frame.
    fn fresh(&mut self, frame: &Frame, now_ms: u32) -> bool {
        if let Some(entry) = self
            .origins
            .iter_mut()
            .flatten()
            .find(|entry| entry.id == frame.origin)
        {
            let silent = now_ms.wrapping_sub(entry.last_ms) > ORIGIN_SILENCE_MS;
            if !silent {
                let stored = counter_of(entry.boot);
                let counter = counter_of(frame.boot);
                if counter < stored || (counter == stored && entry.boot != frame.boot) {
                    return false;
                }
                if counter == stored {
                    if entry.newest.wrapping_sub(frame.seq) as i32 > REPLAY_WINDOW as i32 {
                        return false;
                    }
                    if (frame.seq.wrapping_sub(entry.newest) as i32) > 0 {
                        entry.newest = frame.seq;
                    }
                    entry.last_ms = now_ms;
                    return true;
                }
            }
            *entry = Origin {
                id: frame.origin,
                boot: frame.boot,
                newest: frame.seq,
                last_ms: now_ms,
            };
            return true;
        }
        let slot = self
            .origins
            .iter()
            .position(Option::is_none)
            .unwrap_or_else(|| {
                self.origins_next = (self.origins_next + 1) % MAX_ORIGINS;
                self.origins_next
            });
        self.origins[slot] = Some(Origin {
            id: frame.origin,
            boot: frame.boot,
            newest: frame.seq,
            last_ms: now_ms,
        });
        true
    }

    /// Handles one frame off the air.
    pub fn receive(
        &mut self,
        now_ms: u32,
        src: [u8; 6],
        rssi: i8,
        data: &[u8],
        on_event: &mut dyn FnMut(Event),
    ) {
        let mut plain = [0u8; MAX_PAYLOAD];
        let Some((frame, length)) = self.sealer.open(data, &mut plain) else {
            self.rx_bad = self.rx_bad.wrapping_add(1);
            return;
        };
        if frame.origin == self.id {
            return;
        }
        // Beacons are never forwarded, so the sender is the origin. A neighbour's
        // address comes from this source, which the seal does not cover, so a
        // recorded beacon sent from another radio must not repoint it.
        if frame.kind == FrameKind::Beacon && id_of(&src) != frame.origin {
            self.rx_bad = self.rx_bad.wrapping_add(1);
            return;
        }
        // A denied board's beacons stop here, before they can make it a neighbour
        // or move this node's window. Other frames from it still pass.
        if frame.kind == FrameKind::Beacon && self.denied(frame.origin) {
            return;
        }
        if !self.fresh(&frame, now_ms) {
            self.rx_dup = self.rx_dup.wrapping_add(1);
            return;
        }
        let payload = &plain[..length];
        match frame.kind {
            FrameKind::Beacon => self.on_beacon(now_ms, src, rssi, &frame, payload, on_event),
            FrameKind::Command => {
                if !self.remember(&frame) {
                    self.rx_dup = self.rx_dup.wrapping_add(1);
                    return;
                }
                if let Some(command) = Command::decode(payload) {
                    on_event(Event::Command(command));
                }
                self.forward(data, Dest::Broadcast);
            }
            FrameKind::Watch => {
                // A list of the wrong length or count is refused, never cut short,
                // and it is not forwarded.
                let Some(watch) = Watch::decode(payload) else {
                    self.rx_bad = self.rx_bad.wrapping_add(1);
                    return;
                };
                if !self.remember(&frame) {
                    self.rx_dup = self.rx_dup.wrapping_add(1);
                    return;
                }
                // An older list is ignored and not forwarded, so it does not spread.
                if !self.newer_watch(watch.id) {
                    return;
                }
                self.watch = Some(watch.id);
                on_event(Event::Watch {
                    count: watch.count,
                    addresses: watch.addresses,
                });
                self.forward(data, Dest::Broadcast);
            }
            kind if is_down(kind) => {
                if !self.remember(&frame) {
                    self.rx_dup = self.rx_dup.wrapping_add(1);
                    return;
                }
                self.on_down(now_ms, data, &frame, payload, on_event);
            }
            _ => {
                if !self.remember(&frame) {
                    self.rx_dup = self.rx_dup.wrapping_add(1);
                    return;
                }
                self.learn(now_ms, frame.origin, src);
                if self.is_root(now_ms) {
                    on_event(Event::Up {
                        frame,
                        payload,
                        rssi,
                    });
                } else if let Some(mac) = self.parent().map(|parent| parent.mac) {
                    self.forward(data, Dest::Peer(mac));
                } else {
                    self.queue_drops = self.queue_drops.wrapping_add(1);
                }
            }
        }
    }

    /// Records that upward frames from `id` came through the neighbour `mac`. A
    /// full table replaces the entry that was refreshed longest ago.
    fn learn(&mut self, now_ms: u32, id: u32, mac: [u8; 6]) {
        let route = Route {
            id,
            next_mac: mac,
            last_ms: now_ms,
        };
        let index = self
            .routes
            .iter()
            .position(|slot| slot.is_some_and(|known| known.id == id))
            .or_else(|| self.routes.iter().position(Option::is_none))
            .unwrap_or_else(|| {
                (0..ROUTES)
                    .max_by_key(|&index| {
                        let last = self.routes[index].map_or(0, |known| known.last_ms);
                        now_ms.wrapping_sub(last)
                    })
                    .unwrap_or(0)
            });
        self.routes[index] = Some(route);
    }

    /// The neighbour to unicast a downward frame for `id` to, if its route is live.
    fn route(&self, now_ms: u32, id: u32) -> Option<[u8; 6]> {
        self.routes
            .iter()
            .flatten()
            .find(|known| known.id == id && now_ms.wrapping_sub(known.last_ms) < ROUTE_TIMEOUT_MS)
            .map(|known| known.next_mac)
    }

    /// Sends a downward frame that arrived for another node to its next hop. A
    /// node that is the target gets it as `Event::Down`. One with no live route
    /// counts it in `down_drops`.
    fn on_down(
        &mut self,
        now_ms: u32,
        data: &[u8],
        frame: &Frame,
        payload: &[u8],
        on_event: &mut dyn FnMut(Event),
    ) {
        let Some((target, body)) = payload.split_first_chunk::<4>() else {
            self.down_drops = self.down_drops.wrapping_add(1);
            return;
        };
        let target = u32::from_le_bytes(*target);
        if target == self.id {
            on_event(Event::Down {
                frame: *frame,
                payload: body,
            });
        } else if let Some(mac) = self.route(now_ms, target) {
            self.forward(data, Dest::Peer(mac));
        } else {
            self.down_drops = self.down_drops.wrapping_add(1);
        }
    }

    fn forward(&mut self, data: &[u8], dest: Dest) {
        if data[2] <= 1 {
            return;
        }
        let mut copy = [0u8; MAX_FRAME];
        copy[..data.len()].copy_from_slice(data);
        copy[2] -= 1;
        copy[3] = copy[3].saturating_add(1);
        self.enqueue(dest, &copy[..data.len()]);
    }

    fn on_beacon(
        &mut self,
        now_ms: u32,
        src: [u8; 6],
        rssi: i8,
        frame: &Frame,
        payload: &[u8],
        on_event: &mut dyn FnMut(Event),
    ) {
        if payload.len() != BEACON_PAYLOAD {
            self.rx_bad = self.rx_bad.wrapping_add(1);
            return;
        }
        let rank = payload[0];
        let parent = u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]);
        let number = u16::from_le_bytes([payload[5], payload[6]]);
        let slot = self
            .neighbours
            .iter()
            .position(|entry| entry.is_some_and(|known| known.id == frame.origin));
        match slot {
            Some(index) => {
                let known = self.neighbours[index].as_mut().unwrap();
                let gap = number.wrapping_sub(known.last_beacon);
                known.heard = if gap == 0 || gap >= 16 {
                    1
                } else {
                    (known.heard << gap) | 1
                };
                known.last_beacon = number;
                known.rank = rank;
                known.parent = parent;
                known.mac = src;
                known.last_ms = now_ms;
                known.rssi = ((known.rssi as i16 * 3 + rssi as i16) / 4) as i8;
            }
            None => {
                let entry = Neighbour {
                    id: frame.origin,
                    mac: src,
                    rank,
                    parent,
                    rssi,
                    last_ms: now_ms,
                    heard: 1,
                    last_beacon: number,
                };
                match self
                    .neighbours
                    .iter()
                    .position(Option::is_none)
                    .or_else(|| self.weakest())
                {
                    Some(index) => self.neighbours[index] = Some(entry),
                    None => return,
                }
            }
        }
        if !self.is_root(now_ms) {
            let before = self.parent().map(|parent| parent.id);
            self.reselect(now_ms, before, Reason::BetterPath, on_event);
        }
    }

    /// The neighbour to give up when the table is full. Never the parent.
    fn weakest(&self) -> Option<usize> {
        let parent = self.parent;
        self.neighbours
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != parent)
            .max_by_key(|(_, entry)| entry.map_or(u16::MAX, |known| known.cost()))
            .map(|(index, _)| index)
    }

    fn usable(&self, entry: &Neighbour) -> bool {
        // A neighbour whose route runs through this node cannot be a way out.
        entry.rank < MAX_RANK && entry.parent != self.id
    }

    /// Picks a parent. `before_id` is the parent as it stood before any clearing,
    /// so a loss still counts as a change when nothing replaces it. `reason`
    /// is why the caller cleared or re-ran the choice.
    fn reselect(
        &mut self,
        now_ms: u32,
        before_id: Option<u32>,
        reason: Reason,
        on_event: &mut dyn FnMut(Event),
    ) {
        let before = self.parent;
        let mut best: Option<(usize, u16)> = None;
        for (index, entry) in self.neighbours.iter().enumerate() {
            let Some(entry) = entry else { continue };
            if !self.usable(entry) {
                continue;
            }
            let cost = entry.cost();
            if best.is_none_or(|(_, lowest)| cost < lowest) {
                best = Some((index, cost));
            }
        }
        let keep = before.and_then(|index| {
            self.neighbours[index]
                .as_ref()
                .filter(|entry| self.usable(entry))
                .map(|entry| (index, entry.cost()))
        });
        let chosen = match (keep, best) {
            (Some((index, cost)), Some((_, lowest))) if lowest + HYSTERESIS > cost => Some(index),
            (_, Some((index, _))) => Some(index),
            _ => None,
        };
        self.parent = chosen;
        self.rank = match self.parent() {
            Some(parent) => parent.rank + 1,
            None => NO_RANK,
        };
        let after_id = self.parent().map(|parent| parent.id);
        if after_id != before_id {
            self.parent_failures = 0;
            self.parent_changes = self.parent_changes.wrapping_add(1);
            self.log_route(RouteEvent {
                uptime_s: now_ms / 1000,
                old: before_id.unwrap_or(0),
                new: after_id.unwrap_or(0),
                reason: reason as u8,
            });
            on_event(Event::Parent {
                old: before_id,
                new: after_id,
                reason,
            });
            if after_id.is_some() {
                self.flush_routes();
            }
        }
    }

    /// Call this often. It expires neighbours and sends beacons when due.
    pub fn tick(&mut self, now_ms: u32, on_event: &mut dyn FnMut(Event)) {
        let root = self.is_root(now_ms);
        if !root && self.root_until_ms.take().is_some() {
            self.rank = NO_RANK;
        }
        let before = self.parent().map(|parent| parent.id);
        for index in 0..MAX_NEIGHBOURS {
            if let Some(entry) = self.neighbours[index] {
                if now_ms.wrapping_sub(entry.last_ms) > NEIGHBOUR_TIMEOUT_MS {
                    self.neighbours[index] = None;
                    if self.parent == Some(index) {
                        self.parent = None;
                    }
                }
            }
        }
        if !root {
            // A timeout only if the parent is what went. A board that stopped being
            // the root joins here too, and nothing timed out.
            let reason = if before.is_some() && self.parent.is_none() {
                Reason::BeaconTimeout
            } else {
                Reason::BetterPath
            };
            self.reselect(now_ms, before, reason, on_event);
        } else {
            self.rank = 0;
        }
        if (now_ms.wrapping_sub(self.next_beacon_ms) as i32) >= 0 {
            self.next_beacon_ms = now_ms.wrapping_add(BEACON_MS);
            self.beacon(now_ms);
        }
    }

    fn beacon(&mut self, _now_ms: u32) {
        self.beacon_no = self.beacon_no.wrapping_add(1);
        let parent = self.parent().map_or(0, |parent| parent.id);
        let mut payload = [0u8; BEACON_PAYLOAD];
        payload[0] = self.rank;
        payload[1..5].copy_from_slice(&parent.to_le_bytes());
        payload[5..7].copy_from_slice(&self.beacon_no.to_le_bytes());
        let head = self.head(FrameKind::Beacon, 1);
        let mut out = [0u8; MAX_FRAME];
        if let Some(length) = self.sealer.seal(&head, &payload, &mut out) {
            self.enqueue(Dest::Broadcast, &out[..length]);
        }
    }
}

#[cfg(test)]
mod boot_tests {
    use super::{counter_of, next_boot};

    #[test]
    fn a_board_with_no_stored_counter_seeds_it_below_the_top_bit() {
        let (counter, boot) = next_boot(None, 0xFFFF_FFFF);
        assert_eq!(counter, 0x7_FFFF);
        assert_eq!(boot, (0x7_FFFF << 12) | 0xFFF);
        assert!(counter < 1 << 19, "top bit of the 20-bit counter is set");
        assert_eq!(next_boot(None, 0x8000_0000).0, 0);
        assert_eq!(
            next_boot(None, 0x0000_0FFF).0,
            0,
            "low bits must not seed the counter"
        );
    }

    #[test]
    fn a_stored_counter_is_raised_by_one_and_keeps_the_random_bits() {
        let (counter, boot) = next_boot(Some(41), 0x123);
        assert_eq!(counter, 42);
        assert_eq!(boot, (42 << 12) | 0x123);
    }

    #[test]
    fn a_stored_counter_wraps_after_twenty_bits() {
        assert_eq!(next_boot(Some(0xF_FFFF), 0).0, 0);
    }

    #[test]
    fn two_boots_in_a_row_give_different_ids_and_later_counters() {
        let (counter, first) = next_boot(None, 0x5_5555);
        let (_, second) = next_boot(Some(counter), 0x5_5555);
        assert_ne!(first, second);
        assert!(counter_of(second) > counter_of(first));
    }

    #[test]
    fn two_seeded_boots_with_the_same_counter_still_differ_in_their_random_bits() {
        let (_, first) = next_boot(None, (5 << 12) | 0x111);
        let (_, second) = next_boot(None, (5 << 12) | 0x222);
        assert_eq!(counter_of(first), counter_of(second));
        assert_ne!(first, second);
    }

    #[test]
    fn the_counter_is_the_top_twenty_bits_of_the_boot_id() {
        let (counter, boot) = next_boot(Some(0x1234), 0xABC);
        assert_eq!(counter_of(boot), counter);
        assert_eq!(boot & 0xFFF, 0xABC);
    }
}
