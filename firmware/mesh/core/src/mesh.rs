//! Routing. Each node chooses a parent from its neighbours' beacons.
//!
//! A beacon says how many hops its sender is from the host. A node takes the
//! neighbour that gives it the cheapest path, where cost counts hops and missed
//! beacons. Reports go to the parent as unicast, so the radio retries them.
//! Commands flood, and duplicates stop at the first node that has seen them.
//! When a parent goes quiet or stops acknowledging, the node drops it, says so in
//! its next beacon, and takes the next best neighbour.

use crate::wire::{Command, Frame, FrameKind, Sealer, MAX_FRAME, MAX_PAYLOAD};

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
pub const MAX_ORIGINS: usize = 16;
pub const TX_RETRIES: u8 = 2;
/// Failed unicasts in a row before a parent is dropped.
pub const PARENT_FAILURES: u8 = 4;
const HYSTERESIS: u16 = 60;
const BEACON_PAYLOAD: usize = 11;

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
    /// The parent changed. `None` means no route.
    Parent(Option<u32>),
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

#[derive(Copy, Clone)]
struct Origin {
    id: u32,
    boot: u32,
    newest: u32,
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
    pub fn tx_done(&mut self, acked: bool, on_event: &mut dyn FnMut(Event)) {
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
                    if let Some(index) = self.parent {
                        self.neighbours[index] = None;
                    }
                    self.parent = None;
                    self.parent_failures = 0;
                    self.reselect(on_event);
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
        let head = self.head(kind, DEFAULT_TTL);
        if self.is_root(now_ms) {
            on_event(Event::Up {
                frame: head,
                payload,
                rssi: 0,
            });
            return true;
        }
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

    /// False for a frame too far behind what this origin has already sent in
    /// this boot. The fingerprint ring only covers recent frames. A frame from an
    /// earlier boot still passes: boot ids are random, so there is no order to check.
    fn fresh(&mut self, frame: &Frame) -> bool {
        for entry in self.origins.iter_mut().flatten() {
            if entry.id != frame.origin {
                continue;
            }
            if entry.boot != frame.boot {
                *entry = Origin {
                    id: frame.origin,
                    boot: frame.boot,
                    newest: frame.seq,
                };
                return true;
            }
            if entry.newest.wrapping_sub(frame.seq) as i32 > REPLAY_WINDOW as i32 {
                return false;
            }
            if (frame.seq.wrapping_sub(entry.newest) as i32) > 0 {
                entry.newest = frame.seq;
            }
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
        if !self.fresh(&frame) {
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
            _ => {
                if !self.remember(&frame) {
                    self.rx_dup = self.rx_dup.wrapping_add(1);
                    return;
                }
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
            self.reselect(on_event);
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

    fn reselect(&mut self, on_event: &mut dyn FnMut(Event)) {
        let before = self.parent;
        let before_id = self.parent().map(|parent| parent.id);
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
            on_event(Event::Parent(after_id));
        }
    }

    /// Call this often. It expires neighbours and sends beacons when due.
    pub fn tick(&mut self, now_ms: u32, on_event: &mut dyn FnMut(Event)) {
        let root = self.is_root(now_ms);
        if !root && self.root_until_ms.take().is_some() {
            self.rank = NO_RANK;
        }
        let mut lost = false;
        for index in 0..MAX_NEIGHBOURS {
            if let Some(entry) = self.neighbours[index] {
                if now_ms.wrapping_sub(entry.last_ms) > NEIGHBOUR_TIMEOUT_MS {
                    self.neighbours[index] = None;
                    if self.parent == Some(index) {
                        self.parent = None;
                    }
                    lost = true;
                }
            }
        }
        if !root {
            if lost {
                on_event(Event::Parent(None));
            }
            self.reselect(on_event);
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
