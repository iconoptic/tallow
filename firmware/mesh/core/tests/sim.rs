//! The mesh on a simulated radio: virtual time, lossy links, boards that die.

use ed25519_compact::{KeyPair, Seed};
use tallow_mesh_core::gatt::{
    Act, Client, Done, Reply, Status, CONNECT_MS, NOTIFY_MAX, REQUEST_MS, VALUE_MAX,
};
use tallow_mesh_core::mesh::{
    counter_of, next_boot, Reason, BEACON_MS, MAX_RANK, NEIGHBOUR_TIMEOUT_MS, ORIGIN_SILENCE_MS,
    PARENT_FAILURES, SEEN, TX_RETRIES,
};
use tallow_mesh_core::ota::{
    self, Action, Code, Down, Receiver, Up, BEGIN, BITMAP, CAM_BITMAP, CAM_SLOT, CHUNK, DIGEST,
    PRODUCT_CAM0, PRODUCT_NODE, SLOT, WINDOW_BYTES,
};
use tallow_mesh_core::wire::{Command, Frame, Sealer, Watch, MAX_FRAME, VERSION, WATCH_MAX};
use tallow_mesh_core::{Dest, Event, FrameKind, Mesh, Sightings, NO_RANK};

const KEY: [u8; 32] = [7; 32];
const STEP_MS: u32 = 10;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

fn mac(index: usize) -> [u8; 6] {
    [0x70, 0x4B, 0xCA, 0x10, 0x20, index as u8 + 1]
}

#[derive(Debug)]
struct Arrival {
    origin: u32,
    hops: u8,
    seq_value: u32,
    kind: FrameKind,
    payload: Vec<u8>,
}

struct Sim {
    nodes: Vec<Mesh>,
    alive: Vec<bool>,
    /// Percent chance a frame crosses each link. 0 means no link.
    links: Vec<Vec<u64>>,
    now: u32,
    rng: Rng,
    roots: Vec<usize>,
    arrivals: Vec<Arrival>,
    commands: Vec<Vec<u16>>,
    parents: Vec<Vec<Option<u32>>>,
    /// The paging list each board last applied, or `None` if it never got one.
    watched: Vec<Option<Vec<[u8; 6]>>>,
    airtime: u64,
    /// Ping frames sent to one neighbour, and sent to all of them.
    unicast_pings: u64,
    broadcast_pings: u64,
}

impl Sim {
    fn new(count: usize, edges: &[(usize, usize)], percent: u64) -> Self {
        let mut links = vec![vec![0; count]; count];
        for &(a, b) in edges {
            links[a][b] = percent;
            links[b][a] = percent;
        }
        Self {
            nodes: (0..count)
                .map(|index| Mesh::new(mac(index), &KEY, 1000 + index as u32 * 137))
                .collect(),
            alive: vec![true; count],
            links,
            now: 1,
            rng: Rng(0x1234_5678_9ABC_DEF1),
            roots: vec![0],
            arrivals: Vec::new(),
            commands: vec![Vec::new(); count],
            parents: vec![Vec::new(); count],
            watched: vec![None; count],
            airtime: 0,
            unicast_pings: 0,
            broadcast_pings: 0,
        }
    }

    fn id(&self, index: usize) -> u32 {
        self.nodes[index].id
    }

    fn step(&mut self) {
        let now = self.now;
        for index in 0..self.nodes.len() {
            if !self.alive[index] {
                continue;
            }
            if self.roots.contains(&index) {
                self.nodes[index].host_heartbeat(now);
            }
            let (arrivals, commands, parents, watched) = (
                &mut self.arrivals,
                &mut self.commands[index],
                &mut self.parents[index],
                &mut self.watched[index],
            );
            let mut sink = sink(arrivals, commands, parents, watched);
            self.nodes[index].tick(now, &mut sink);
        }
        for index in 0..self.nodes.len() {
            if !self.alive[index] {
                continue;
            }
            let Some(tx) = self.nodes[index].next_tx().copied() else {
                continue;
            };
            self.airtime += 1;
            if tx.bytes()[1] == FrameKind::Ping as u8 {
                match tx.dest {
                    Dest::Broadcast => self.broadcast_pings += 1,
                    Dest::Peer(_) => self.unicast_pings += 1,
                }
            }
            let mut acked = false;
            for other in 0..self.nodes.len() {
                if other == index || !self.alive[other] {
                    continue;
                }
                let wanted = match tx.dest {
                    Dest::Broadcast => true,
                    Dest::Peer(target) => target == mac(other),
                };
                if !wanted
                    || self.links[index][other] == 0
                    || !self.rng.chance(self.links[index][other])
                {
                    continue;
                }
                acked = true;
                let (arrivals, commands, parents, watched) = (
                    &mut self.arrivals,
                    &mut self.commands[other],
                    &mut self.parents[other],
                    &mut self.watched[other],
                );
                let mut sink = sink(arrivals, commands, parents, watched);
                self.nodes[other].receive(now, mac(index), -60, tx.bytes(), &mut sink);
            }
            let (arrivals, commands, parents, watched) = (
                &mut self.arrivals,
                &mut self.commands[index],
                &mut self.parents[index],
                &mut self.watched[index],
            );
            let mut sink = sink(arrivals, commands, parents, watched);
            self.nodes[index].tx_done(now, acked || tx.dest == Dest::Broadcast, &mut sink);
        }
        self.now = self.now.wrapping_add(STEP_MS);
    }

    fn run_ms(&mut self, ms: u32) {
        for _ in 0..ms / STEP_MS {
            self.step();
        }
    }

    fn report(&mut self, index: usize, value: u32) -> bool {
        let now = self.now;
        let (arrivals, commands, parents, watched) = (
            &mut self.arrivals,
            &mut self.commands[index],
            &mut self.parents[index],
            &mut self.watched[index],
        );
        let mut sink = sink(arrivals, commands, parents, watched);
        self.nodes[index].send_up(FrameKind::Probe, &value.to_le_bytes(), now, &mut sink)
    }

    /// Sets the link between two boards, in percent. Zero cuts it.
    fn set_link(&mut self, a: usize, b: usize, percent: u64) {
        self.links[a][b] = percent;
        self.links[b][a] = percent;
    }

    /// The root sends a Ping to `target`, with a four-byte value as its payload.
    fn send_ping(&mut self, target: u32, value: u32) -> bool {
        let now = self.now;
        self.nodes[0].send_down(now, target, FrameKind::Ping, &value.to_le_bytes())
    }

    /// Ping values that reached their target, and how many times each arrived.
    fn pings_arrived(&self, value: u32) -> Vec<u8> {
        self.arrivals
            .iter()
            .filter(|entry| entry.kind == FrameKind::Ping && entry.seq_value == value)
            .map(|entry| entry.hops)
            .collect()
    }

    /// Probe payloads that reached the root from `index`.
    fn arrived(&self, index: usize) -> Vec<u32> {
        let id = self.id(index);
        self.arrivals
            .iter()
            .filter(|entry| entry.origin == id && entry.kind == FrameKind::Probe)
            .map(|entry| entry.seq_value)
            .collect()
    }
}

fn sink<'a>(
    arrivals: &'a mut Vec<Arrival>,
    commands: &'a mut Vec<u16>,
    parents: &'a mut Vec<Option<u32>>,
    watched: &'a mut Option<Vec<[u8; 6]>>,
) -> impl FnMut(Event) + 'a {
    move |event| match event {
        Event::Up { frame, payload, .. } | Event::Down { frame, payload } => {
            let seq_value = payload
                .first_chunk::<4>()
                .map_or(0, |bytes| u32::from_le_bytes(*bytes));
            arrivals.push(Arrival {
                origin: frame.origin,
                hops: frame.hops,
                seq_value,
                kind: frame.kind,
                payload: payload.to_vec(),
            });
        }
        Event::Command(command) => commands.push(command.id),
        Event::Parent { new, .. } => parents.push(new),
        Event::Watch { count, addresses } => {
            *watched = Some(addresses[..count as usize].to_vec());
        }
    }
}

fn chain(count: usize) -> Vec<(usize, usize)> {
    (1..count).map(|index| (index - 1, index)).collect()
}

fn quiet(_: Event) {}

const COMMAND: Command = Command {
    id: 42,
    ble_window_ms: 30,
    ble_interval_ms: 100,
    inquiry_period_s: 60,
    inquiry_length: 4,
    probe_hz: 0,
    probe_size: 0,
};

#[test]
fn a_report_crosses_four_hops() {
    let mut sim = Sim::new(5, &chain(5), 100);
    sim.run_ms(20_000);
    assert_eq!(sim.nodes[4].rank, 4);
    assert!(sim.report(4, 1));
    sim.run_ms(1_000);
    let arrivals: Vec<_> = sim
        .arrivals
        .iter()
        .filter(|entry| entry.origin == sim.id(4) && entry.kind == FrameKind::Probe)
        .collect();
    assert_eq!(arrivals.len(), 1);
    assert_eq!(arrivals[0].hops, 3);
}

#[test]
fn takes_the_short_path_when_there_are_two() {
    // 0-1-2-3 and 0-3 directly.
    let mut sim = Sim::new(4, &[(0, 1), (1, 2), (2, 3), (0, 3)], 100);
    sim.run_ms(20_000);
    assert_eq!(sim.nodes[3].rank, 1);
    assert_eq!(sim.nodes[3].parent().unwrap().id, sim.id(0));
}

#[test]
fn routes_around_a_dead_board() {
    // 0 reaches 3 through 1 or 2.
    let mut sim = Sim::new(4, &[(0, 1), (0, 2), (1, 3), (2, 3)], 100);
    sim.run_ms(20_000);
    let parent = sim.nodes[3].parent().unwrap().id;
    let (dead, other) = if parent == sim.id(1) { (1, 2) } else { (2, 1) };
    sim.alive[dead] = false;
    let mut delivered_after = None;
    for second in 0..30 {
        sim.report(3, 1000 + second);
        sim.run_ms(1_000);
        if sim.arrived(3).contains(&(1000 + second)) {
            delivered_after = Some(second);
            break;
        }
    }
    let seconds = delivered_after.expect("the mesh never recovered");
    assert!(seconds <= 10, "took {seconds} s");
    assert_eq!(sim.nodes[3].parent().unwrap().id, sim.id(other));
}

#[test]
fn a_silent_parent_is_dropped_by_failed_acks_before_the_timeout() {
    let mut sim = Sim::new(4, &[(0, 1), (0, 2), (1, 3), (2, 3)], 100);
    sim.run_ms(20_000);
    let parent = sim.nodes[3].parent().unwrap().id;
    let dead = if parent == sim.id(1) { 1 } else { 2 };
    sim.alive[dead] = false;
    for value in 0..8 {
        sim.report(3, value);
    }
    sim.run_ms(500);
    assert_ne!(sim.nodes[3].parent().map(|entry| entry.id), Some(parent));
}

#[test]
fn delivers_over_bad_links() {
    let mut sim = Sim::new(4, &chain(4), 70);
    sim.run_ms(40_000);
    assert_eq!(sim.nodes[3].rank, 3);
    let mut sent = 0;
    for value in 0..200 {
        if sim.report(3, value) {
            sent += 1;
        }
        sim.run_ms(500);
    }
    let received = sim.arrived(3).len();
    assert!(sent >= 195, "sent {sent}");
    assert!(received * 100 >= sent * 90, "{received} of {sent}");
    let mut unique = sim.arrived(3);
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), received, "duplicates reached the host");
}

#[test]
fn a_command_reaches_every_board_exactly_once() {
    let mut sim = Sim::new(6, &chain(6), 100);
    sim.run_ms(20_000);
    let (arrivals, commands, parents, watched) = (
        &mut sim.arrivals,
        &mut sim.commands[0],
        &mut sim.parents[0],
        &mut sim.watched[0],
    );
    let mut root_sink = sink(arrivals, commands, parents, watched);
    assert!(sim.nodes[0].send_command(&COMMAND, &mut root_sink));
    drop(root_sink);
    sim.run_ms(2_000);
    for index in 0..6 {
        assert_eq!(sim.commands[index], vec![42], "node {index}");
    }
}

#[test]
fn any_board_can_become_the_root() {
    let mut sim = Sim::new(3, &chain(3), 100);
    sim.run_ms(20_000);
    assert_eq!(sim.nodes[2].rank, 2);
    sim.roots = vec![2];
    sim.run_ms(30_000);
    assert_eq!(sim.nodes[2].rank, 0);
    assert_eq!(sim.nodes[0].rank, 2);
    sim.report(0, 9);
    sim.run_ms(1_000);
    assert_eq!(sim.arrived(0), vec![9]);
}

#[test]
fn without_a_root_the_ranks_clear_and_traffic_stops() {
    let mut sim = Sim::new(4, &[(0, 1), (1, 2), (2, 3), (3, 1)], 100);
    sim.run_ms(20_000);
    sim.alive[0] = false;
    sim.run_ms(120_000);
    for index in 1..4 {
        let rank = sim.nodes[index].rank;
        assert!(
            rank == NO_RANK || rank <= MAX_RANK,
            "node {index} rank {rank}"
        );
    }
    let mut unrouted = 0;
    for index in 1..4 {
        if sim.nodes[index].rank == NO_RANK {
            unrouted += 1;
        }
    }
    assert_eq!(unrouted, 3, "a routing loop outlived the root");
    let before = sim.airtime;
    for value in 0..3 {
        sim.report(2, value);
    }
    sim.run_ms(BEACON_MS * 10);
    // Three boards beaconing for 20 s, and nothing circling.
    assert!(sim.airtime - before <= 3 * 11);
}

#[test]
fn a_full_queue_drops_and_counts_rather_than_growing() {
    let mut sim = Sim::new(2, &chain(2), 100);
    sim.run_ms(20_000);
    let mut accepted = 0;
    for value in 0..100 {
        if sim.report(1, value) {
            accepted += 1;
        }
    }
    assert!(accepted <= 16);
    assert!(sim.nodes[1].queue_drops >= 84);
}

#[test]
fn a_board_with_the_wrong_key_cannot_join_or_inject() {
    let mut sim = Sim::new(2, &chain(2), 100);
    sim.run_ms(20_000);
    let mut stranger = Mesh::new(mac(9), &[8; 32], 5);
    stranger.host_heartbeat(sim.now);
    stranger.tick(sim.now, &mut quiet);
    let tx = *stranger.next_tx().unwrap();
    let parent_before = sim.nodes[1].parent().unwrap().id;
    let now = sim.now;
    sim.nodes[1].receive(now, mac(9), -40, tx.bytes(), &mut quiet);
    assert_eq!(sim.nodes[1].rx_bad, 1);
    assert_eq!(sim.nodes[1].parent().unwrap().id, parent_before);
    assert_eq!(sim.nodes[1].neighbours().count(), 1);
}

fn sealed_probe() -> (Mesh, Mesh, [u8; 250], usize) {
    let mut root = Mesh::new(mac(0), &KEY, 1);
    let mut leaf = Mesh::new(mac(1), &KEY, 2);
    root.host_heartbeat(0);
    root.tick(1, &mut quiet);
    let beacon = *root.next_tx().unwrap();
    leaf.receive(2, mac(0), -50, beacon.bytes(), &mut quiet);
    assert!(leaf.send_up(FrameKind::Probe, &[1, 2, 3, 4], 3, &mut quiet));
    let tx = *leaf.next_tx().unwrap();
    let mut data = [0u8; 250];
    data[..tx.bytes().len()].copy_from_slice(tx.bytes());
    (root, leaf, data, tx.bytes().len())
}

#[test]
fn any_flipped_bit_outside_ttl_and_hops_is_rejected() {
    let (mut root, _, data, length) = sealed_probe();
    for byte in 0..length {
        if byte == 2 || byte == 3 {
            continue;
        }
        for bit in 0..8 {
            let mut copy = data;
            copy[byte] ^= 1 << bit;
            root.receive(4, mac(1), -50, &copy[..length], &mut |_| {
                panic!("accepted a forged frame")
            });
        }
    }
    assert_eq!(root.rx_bad as usize, (length - 2) * 8);
}

#[test]
fn a_replay_is_dropped_right_away_and_long_after() {
    let (mut root, mut leaf, data, length) = sealed_probe();
    let mut count = 0;
    root.receive(4, mac(1), -50, &data[..length], &mut |_| count += 1);
    assert_eq!(count, 1);
    root.receive(5, mac(1), -50, &data[..length], &mut |_| count += 1);
    assert_eq!(count, 1);
    // Push the first frame out of the fingerprint ring.
    leaf.tx_done(6, true, &mut quiet);
    for _ in 0..SEEN * 2 {
        assert!(leaf.send_up(FrameKind::Probe, &[0; 4], 6, &mut quiet));
        let tx = *leaf.next_tx().unwrap();
        root.receive(6, mac(1), -50, tx.bytes(), &mut quiet);
        leaf.tx_done(6, true, &mut quiet);
    }
    let mut late = 0;
    root.receive(7, mac(1), -50, &data[..length], &mut |_| late += 1);
    assert_eq!(late, 0);
}

fn sealed(kind: FrameKind, origin: u32, seq: u32, payload: &[u8]) -> Vec<u8> {
    let head = Frame {
        kind,
        ttl: 4,
        hops: 0,
        origin,
        boot: 1 << 12,
        seq,
    };
    let mut out = [0u8; MAX_FRAME];
    let size = Sealer::new(&KEY)
        .seal(&head, payload, &mut out)
        .expect("fits");
    out[..size].to_vec()
}

const REPLAY_ORIGIN: u32 = 0x0101_0101;

/// A GATT write for `node`, recorded as `seq` from one origin.
fn recorded_write(node: &Mesh, seq: u32) -> Vec<u8> {
    let mut body = node.id.to_le_bytes().to_vec();
    body.extend_from_slice(&write_request(9, 0x21, &[1]));
    sealed(FrameKind::Gatt, REPLAY_ORIGIN, seq, &body)
}

/// Delivers `frame` and returns how many downward events it produced.
fn downs(node: &mut Mesh, frame: &[u8]) -> u32 {
    let mut count = 0;
    node.receive(1, mac(2), -50, frame, &mut |event| {
        if matches!(event, Event::Down { .. }) {
            count += 1;
        }
    });
    count
}

/// Pushes every print out of the shared ring with another origin's frames, so
/// only the per-origin bitmap can still refuse a replay.
fn turn_the_seen_ring(node: &mut Mesh) {
    for number in 0..SEEN as u32 {
        let filler = sealed(FrameKind::Command, 0x0505_0505, number + 1, &[0; 3]);
        node.receive(2, mac(3), -50, &filler, &mut quiet);
        while node.next_tx().is_some() {
            node.tx_done(2, true, &mut quiet);
        }
    }
}

#[test]
fn a_frame_replayed_after_the_seen_ring_has_turned_is_delivered_once() {
    let mut node = Mesh::new(mac(1), &KEY, 2);
    let frame = recorded_write(&node, 1);
    assert_eq!(downs(&mut node, &frame), 1);
    turn_the_seen_ring(&mut node);
    assert_eq!(downs(&mut node, &frame), 0, "the write ran twice");
}

#[test]
fn a_sequence_number_arriving_late_inside_the_window_is_accepted_once() {
    let mut node = Mesh::new(mac(1), &KEY, 2);
    let early = recorded_write(&node, 1);
    let later = recorded_write(&node, 2);
    assert_eq!(downs(&mut node, &later), 1);
    assert_eq!(downs(&mut node, &early), 1, "late, and still new");
    turn_the_seen_ring(&mut node);
    assert_eq!(downs(&mut node, &early), 0);
    assert_eq!(downs(&mut node, &later), 0);
}

#[test]
fn a_frame_seen_before_the_newest_moved_is_still_refused_after_it() {
    let mut node = Mesh::new(mac(1), &KEY, 2);
    let early = recorded_write(&node, 1);
    let later = recorded_write(&node, 2);
    assert_eq!(downs(&mut node, &early), 1);
    assert_eq!(downs(&mut node, &later), 1);
    turn_the_seen_ring(&mut node);
    assert_eq!(downs(&mut node, &early), 0);
}

#[test]
fn a_frame_a_whole_window_behind_the_newest_is_refused() {
    let mut node = Mesh::new(mac(1), &KEY, 2);
    let newest = recorded_write(&node, 100);
    let inside = recorded_write(&node, 37);
    let outside = recorded_write(&node, 36);
    assert_eq!(downs(&mut node, &newest), 1);
    assert_eq!(downs(&mut node, &inside), 1, "63 behind");
    assert_eq!(downs(&mut node, &outside), 0, "64 behind");
}

#[test]
fn garbage_off_the_air_never_panics() {
    let mut rng = Rng(99);
    let mut node = Mesh::new(mac(0), &KEY, 1);
    for length in 0..300 {
        let mut data = vec![0u8; length];
        for byte in data.iter_mut() {
            *byte = rng.next() as u8;
        }
        if length > 1 {
            data[0] = VERSION;
            data[1] = (length % 7) as u8;
        }
        node.receive(1, mac(1), -50, &data, &mut quiet);
    }
}

#[test]
fn the_clock_can_wrap() {
    let mut sim = Sim::new(3, &chain(3), 100);
    sim.now = u32::MAX - 30_000;
    sim.run_ms(60_000);
    assert_eq!(sim.nodes[2].rank, 2);
}

#[test]
fn a_lost_parent_is_reported_with_its_timeout_once_a_route_returns() {
    let mut sim = Sim::new(2, &chain(2), 100);
    sim.run_ms(20_000);
    let root_id = sim.id(0);
    assert_eq!(sim.nodes[1].parent().unwrap().id, root_id);

    // The root goes silent. Node 1 forgets it after the neighbour timeout, and
    // has no parent, so the loss waits in its log instead of going up.
    sim.alive[0] = false;
    sim.run_ms(NEIGHBOUR_TIMEOUT_MS + 2_000);
    assert!(sim.nodes[1].parent().is_none());

    sim.alive[0] = true;
    sim.run_ms(10_000);
    assert_eq!(sim.nodes[1].parent().unwrap().id, root_id);

    // (uptime_s, old, new, reason) for each Route frame node 1 sent that arrived.
    let routes: Vec<(u32, u32, u32, u8)> = sim
        .arrivals
        .iter()
        .filter(|entry| entry.origin == sim.id(1) && entry.kind == FrameKind::Route)
        .map(|entry| {
            let word =
                |at: usize| u32::from_le_bytes(entry.payload[at..at + 4].try_into().unwrap());
            (word(0), word(4), word(8), entry.payload[12])
        })
        .collect();

    let losses: Vec<_> = routes
        .iter()
        .filter(|route| route.3 == Reason::BeaconTimeout as u8)
        .collect();
    assert_eq!(losses.len(), 1, "one loss, sent once: {routes:?}");
    assert_eq!(losses[0].1, root_id, "old parent");
    assert_eq!(losses[0].2, 0, "no new parent");

    let (uptime, old, new, reason) = *routes.last().expect("the return reached the root");
    assert_eq!((old, new, reason), (0, root_id, Reason::BetterPath as u8));
    assert!(losses[0].0 <= uptime, "the loss is dated before the return");
}

#[test]
fn a_denied_root_is_never_a_neighbour_so_the_chain_is_forced() {
    // All three boards hear each other. Node 2 refuses the root, so its only way up is node 1.
    let mut sim = Sim::new(3, &[(0, 1), (1, 2), (0, 2)], 100);
    let root = sim.id(0);
    assert!(sim.nodes[2].deny(root));
    sim.run_ms(20_000);
    assert_eq!(sim.nodes[2].rank, 2);
    assert_eq!(sim.nodes[2].parent().unwrap().id, sim.id(1));
    assert!(sim.nodes[2].neighbours().all(|entry| entry.id != root));
    assert!(sim.report(2, 5));
    sim.run_ms(1_000);
    assert_eq!(sim.arrived(2), vec![5]);
}

#[test]
fn a_full_deny_list_ignores_a_new_id_and_clearing_frees_it() {
    let mut mesh = Mesh::new(mac(0), &KEY, 1);
    assert!(mesh.deny(1) && mesh.deny(2) && mesh.deny(3) && mesh.deny(4));
    assert!(mesh.deny(2), "an id already denied still reads as denied");
    assert!(!mesh.deny(5), "a full list takes no fifth id");
    assert!(
        !mesh.deny(0),
        "zero marks an empty slot and cannot be denied"
    );
    mesh.clear_deny();
    assert!(mesh.deny(5));
}

/// A budget, not a physical limit. Boards had about 106 KB of free heap in the
/// 2026-10-31 soaks, and the plan's floor is 60 KB. It went from 8 KB to 12 KB when
/// the route table arrived in T1.8b. The GATT client and the OTA receiver count too.
#[test]
fn a_node_fits_in_twelve_kilobytes() {
    let size = core::mem::size_of::<Mesh>()
        + core::mem::size_of::<Sightings<128>>()
        + core::mem::size_of::<Client>()
        + core::mem::size_of::<Receiver<SLOT, BITMAP>>();
    println!(
        "Mesh {} + Sightings<128> {} + Client {} + Receiver {} = {size} bytes",
        core::mem::size_of::<Mesh>(),
        core::mem::size_of::<Sightings<128>>(),
        core::mem::size_of::<Client>(),
        core::mem::size_of::<Receiver<SLOT, BITMAP>>()
    );
    assert!(size <= 12 * 1024, "{size} bytes");
}

/// An origin id for frames a test seals by hand. No board in these tests has it.
const FAKE_ORIGIN: u32 = 0x0102_0304;

/// A Command sealed as `origin` would send it, with this boot and sequence number.
fn sealed_command(origin: u32, boot: u32, seq: u32) -> Vec<u8> {
    let head = Frame {
        kind: FrameKind::Command,
        ttl: 16,
        hops: 0,
        origin,
        boot,
        seq,
    };
    let mut out = [0u8; MAX_FRAME];
    let length = Sealer::new(&KEY)
        .seal(&head, &COMMAND.encode(), &mut out)
        .expect("fits");
    out[..length].to_vec()
}

/// Hands `data` to `node` at `now` and returns how many Commands it applied.
fn commands_applied(node: &mut Mesh, now: u32, data: &[u8]) -> usize {
    let mut applied = 0;
    node.receive(now, mac(0), -60, data, &mut |event| {
        if matches!(event, Event::Command(_)) {
            applied += 1;
        }
    });
    applied
}

#[test]
fn a_frame_from_an_older_boot_is_refused() {
    let newer = (5 << 12) | 0x111;
    let older = (4 << 12) | 0x222;
    let mut node = Mesh::new(mac(1), &KEY, 77);
    assert_eq!(
        commands_applied(&mut node, 1_000, &sealed_command(FAKE_ORIGIN, newer, 1)),
        1
    );
    let refused = node.rx_dup;
    let data = sealed_command(FAKE_ORIGIN, older, 1);
    assert_eq!(commands_applied(&mut node, 2_000, &data), 0);
    assert_eq!(node.rx_dup, refused + 1);
}

#[test]
fn an_older_boot_does_not_reopen_the_window_for_the_current_boot() {
    let newer = (5 << 12) | 0x111;
    let older = (4 << 12) | 0x222;
    let mut node = Mesh::new(mac(1), &KEY, 77);
    let mut applied = 0;
    for seq in 1..=200 {
        applied += commands_applied(&mut node, 1_000, &sealed_command(FAKE_ORIGIN, newer, seq));
    }
    assert_eq!(applied, 200);
    // If the older boot had restarted the window, seq 1 of the newer boot would pass.
    assert_eq!(
        commands_applied(&mut node, 2_000, &sealed_command(FAKE_ORIGIN, older, 1)),
        0
    );
    assert_eq!(
        commands_applied(&mut node, 2_000, &sealed_command(FAKE_ORIGIN, newer, 1)),
        0
    );
    assert_eq!(
        commands_applied(&mut node, 2_000, &sealed_command(FAKE_ORIGIN, newer, 201)),
        1
    );
}

#[test]
fn a_recorded_command_does_not_reapply_after_the_ring_rolls() {
    let boot = (5 << 12) | 0x111;
    let mut node = Mesh::new(mac(1), &KEY, 77);
    let recorded = sealed_command(FAKE_ORIGIN, boot, 1);
    assert_eq!(commands_applied(&mut node, 1_000, &recorded), 1);
    // Enough newer frames that the recorded one has left the fingerprint ring.
    for seq in 2..=(2 * SEEN as u32 + 1) {
        assert_eq!(
            commands_applied(&mut node, 1_000, &sealed_command(FAKE_ORIGIN, boot, seq)),
            1
        );
    }
    assert_eq!(commands_applied(&mut node, 2_000, &recorded), 0);
}

#[test]
fn a_frame_from_a_newer_boot_is_accepted_and_replaces_the_old_window() {
    let older = (5 << 12) | 0x111;
    let newer = (6 << 12) | 0x222;
    let mut node = Mesh::new(mac(1), &KEY, 77);
    for seq in 1..=200 {
        commands_applied(&mut node, 1_000, &sealed_command(FAKE_ORIGIN, older, seq));
    }
    assert_eq!(
        commands_applied(&mut node, 2_000, &sealed_command(FAKE_ORIGIN, newer, 1)),
        1
    );
    assert_eq!(
        commands_applied(&mut node, 2_000, &sealed_command(FAKE_ORIGIN, older, 300)),
        0
    );
}

#[test]
fn an_origin_silent_for_ten_minutes_is_accepted_again_on_a_wrapping_clock() {
    let newer = (5 << 12) | 0x111;
    let older = (4 << 12) | 0x222;
    let start = u32::MAX - 1_000;
    let mut node = Mesh::new(mac(1), &KEY, 77);
    assert_eq!(
        commands_applied(&mut node, start, &sealed_command(FAKE_ORIGIN, newer, 1)),
        1
    );
    // Still within the ten minutes, so the older boot is refused.
    let edge = start.wrapping_add(ORIGIN_SILENCE_MS);
    assert_eq!(
        commands_applied(&mut node, edge, &sealed_command(FAKE_ORIGIN, older, 1)),
        0
    );
    // Silent past ten minutes: the documented window, where an older boot is accepted once.
    let past = edge.wrapping_add(1);
    assert_eq!(
        commands_applied(&mut node, past, &sealed_command(FAKE_ORIGIN, older, 1)),
        1
    );
}

#[test]
fn a_rebooted_board_is_heard_again_over_the_air() {
    let mut sim = Sim::new(3, &chain(3), 100);
    sim.run_ms(20_000);
    assert!(sim.report(2, 1));
    sim.run_ms(1_000);
    assert_eq!(sim.arrived(2), vec![1]);

    // Node 2 reboots. Its counter goes one past the one it had at its first boot.
    let first_boot = 1000 + 2 * 137;
    let (_, boot) = next_boot(Some(counter_of(first_boot)), 0xABC);
    sim.nodes[2] = Mesh::new(mac(2), &KEY, boot);
    sim.run_ms(20_000);
    assert_eq!(sim.nodes[2].rank, 2);
    assert!(sim.report(2, 7));
    sim.run_ms(1_000);
    assert!(sim.arrived(2).contains(&7), "{:?}", sim.arrived(2));
}

#[test]
fn a_ping_crosses_four_hops_down_by_unicast_alone() {
    let mut sim = Sim::new(5, &chain(5), 100);
    sim.run_ms(20_000);
    assert!(sim.report(4, 1));
    sim.run_ms(1_000);
    let target = sim.id(4);
    assert!(sim.send_ping(target, 7));
    sim.run_ms(1_000);
    assert_eq!(
        sim.pings_arrived(7),
        vec![3],
        "one arrival, after three forwards"
    );
    assert_eq!(sim.unicast_pings, 4, "one unicast per hop");
    assert_eq!(sim.broadcast_pings, 0, "no broadcast carried the ping");
}

#[test]
fn a_route_unused_for_a_minute_expires_and_the_root_counts_the_drop() {
    let mut sim = Sim::new(5, &chain(5), 100);
    sim.run_ms(20_000);
    assert!(sim.report(4, 1));
    sim.run_ms(1_000);
    let target = sim.id(4);
    assert!(sim.send_ping(target, 1));
    sim.run_ms(61_000);
    assert!(!sim.send_ping(target, 2));
    assert_eq!(sim.nodes[0].down_drops, 1);
    sim.run_ms(1_000);
    assert!(sim.pings_arrived(2).is_empty());
    assert_eq!(sim.pings_arrived(1).len(), 1, "the live route delivered");
}

#[test]
fn a_target_that_moved_is_reached_again_after_its_next_report() {
    // Node 4 hangs off node 1 until it moves under node 2.
    let mut sim = Sim::new(5, &[(0, 1), (1, 2), (2, 3), (1, 4)], 100);
    sim.run_ms(20_000);
    assert!(sim.report(4, 1));
    sim.run_ms(1_000);
    sim.set_link(1, 4, 0);
    sim.set_link(2, 4, 100);
    // Node 4 keeps its old parent until that parent's beacons time out, and sends
    // nothing up until then. So the root and node 1 still point the old way.
    sim.run_ms(3_000);
    let target = sim.id(4);
    assert!(sim.send_ping(target, 2));
    sim.run_ms(1_000);
    assert!(
        sim.pings_arrived(2).is_empty(),
        "a ping reached a board that moved"
    );
    // The switch to node 2 sends a Route frame up, then the board reports.
    sim.run_ms(12_000);
    assert!(sim.report(4, 3), "the moved board has no parent yet");
    sim.run_ms(1_000);
    assert!(sim.send_ping(target, 4));
    sim.run_ms(1_000);
    assert_eq!(sim.pings_arrived(4).len(), 1, "the new route delivered");
}

#[test]
fn a_failed_unicast_to_a_child_leaves_the_parent_alone() {
    let mut sim = Sim::new(3, &chain(3), 100);
    sim.run_ms(20_000);
    assert!(sim.report(2, 1));
    sim.run_ms(1_000);
    let target = sim.id(2);
    let parent = sim.id(0);
    let changes = sim.parents[1].len();
    sim.set_link(1, 2, 0);
    // Each failed frame is tried three times, and there are more of them than
    // `PARENT_FAILURES`, so a child's failures would drop the parent if they counted.
    let frames = PARENT_FAILURES as u64 + 2;
    for value in 0..frames as u32 {
        assert!(sim.send_ping(target, value));
        sim.run_ms(100);
    }
    sim.run_ms(1_000);
    let tries = TX_RETRIES as u64 + 1;
    assert_eq!(
        sim.unicast_pings,
        frames + frames * tries,
        "each frame tried three times"
    );
    assert_eq!(sim.nodes[1].parent().map(|entry| entry.id), Some(parent));
    assert_eq!(sim.parents[1].len(), changes, "node 1 changed parent");
    assert_eq!(sim.nodes[1].rank, 1);
}

#[test]
fn only_the_root_sends_downward_frames_of_a_downward_kind() {
    let mut sim = Sim::new(3, &chain(3), 100);
    sim.run_ms(20_000);
    let now = sim.now;
    let target = sim.id(2);
    assert!(!sim.nodes[1].send_down(now, target, FrameKind::Ping, &[0; 4]));
    assert!(!sim.nodes[0].send_down(now, target, FrameKind::Probe, &[0; 4]));
    assert!(!sim.nodes[0].send_down(now, target, FrameKind::Beacon, &[0; 4]));
    assert!(!sim.nodes[0].send_down(now, target, FrameKind::Ping, &[0; 215]));
    assert!(sim.nodes[0].send_down(now, target, FrameKind::Ping, &[0; 214]));
}

#[test]
fn a_downward_frame_too_short_for_a_target_is_dropped_and_counted() {
    let mut node = Mesh::new(mac(1), &KEY, 1000);
    let head = Frame {
        kind: FrameKind::Ping,
        ttl: 16,
        hops: 0,
        origin: FAKE_ORIGIN,
        boot: 5,
        seq: 1,
    };
    let mut out = [0u8; MAX_FRAME];
    let length = Sealer::new(&KEY)
        .seal(&head, &[1, 2, 3], &mut out)
        .expect("payload fits");
    let mut events = 0;
    node.receive(1000, mac(0), -50, &out[..length], &mut |_| events += 1);
    assert_eq!(node.down_drops, 1);
    assert_eq!(events, 0);
}

/// `count` distinct test addresses, starting at `first`.
fn addresses(first: usize, count: usize) -> Vec<[u8; 6]> {
    (first..first + count).map(mac).collect()
}

/// A Watch frame sealed as `origin` would send it, with `payload` as the body.
fn sealed_watch(origin: u32, boot: u32, seq: u32, payload: &[u8]) -> Vec<u8> {
    let head = Frame {
        kind: FrameKind::Watch,
        ttl: 16,
        hops: 0,
        origin,
        boot,
        seq,
    };
    let mut out = [0u8; MAX_FRAME];
    let length = Sealer::new(&KEY)
        .seal(&head, payload, &mut out)
        .expect("fits");
    out[..length].to_vec()
}

/// A Watch body with list `id` and the given addresses, as the wire carries it.
fn watch_body(id: u16, list: &[[u8; 6]]) -> Vec<u8> {
    let mut addresses = [[0u8; 6]; WATCH_MAX];
    addresses[..list.len()].copy_from_slice(list);
    let mut out = [0u8; Watch::MAX_WIRE];
    let length = Watch {
        id,
        count: list.len() as u8,
        addresses,
    }
    .encode(&mut out);
    out[..length].to_vec()
}

impl Sim {
    /// The root floods a paging list, and records it as its own board would.
    fn send_watch(&mut self, list: &[[u8; 6]]) -> bool {
        let (arrivals, commands, parents, watched) = (
            &mut self.arrivals,
            &mut self.commands[0],
            &mut self.parents[0],
            &mut self.watched[0],
        );
        let mut root_sink = sink(arrivals, commands, parents, watched);
        self.nodes[0].send_watch(list, &mut root_sink)
    }
}

#[test]
fn a_watch_list_reaches_every_board_over_hops() {
    let mut sim = Sim::new(4, &chain(4), 100);
    sim.run_ms(20_000);
    let list = addresses(50, 3);
    assert!(sim.send_watch(&list));
    sim.run_ms(3_000);
    for index in 0..4 {
        assert_eq!(
            sim.watched[index].as_deref(),
            Some(list.as_slice()),
            "board {index}"
        );
    }
}

#[test]
fn an_empty_watch_list_clears_the_one_before() {
    let mut sim = Sim::new(3, &chain(3), 100);
    sim.run_ms(20_000);
    let list = addresses(60, 2);
    assert!(sim.send_watch(&list));
    sim.run_ms(3_000);
    assert_eq!(sim.watched[2].as_deref(), Some(list.as_slice()));

    assert!(sim.send_watch(&[]));
    sim.run_ms(3_000);
    for index in 0..3 {
        assert_eq!(
            sim.watched[index],
            Some(Vec::new()),
            "board {index} cleared"
        );
    }
}

#[test]
fn a_watch_of_the_wrong_length_or_count_is_refused_and_not_forwarded() {
    let mut node = Mesh::new(mac(1), &KEY, 1000);
    let list = addresses(70, 2);

    // A count of 17, with the 102 bytes that count needs.
    let mut over = vec![9, 0, 17];
    over.extend_from_slice(&[0x77; 17 * 6]);
    // A count of 2 with only one address after it.
    let mut short = watch_body(9, &list);
    short.truncate(3 + 6);
    // A count of 1 with one address and one byte more.
    let mut long = watch_body(9, &list[..1]);
    long.push(0);

    let mut events = 0;
    for (seq, body) in [over, short, long].iter().enumerate() {
        let frame = sealed_watch(FAKE_ORIGIN, 5, seq as u32 + 1, body);
        node.receive(1000, mac(0), -50, &frame, &mut |_| events += 1);
    }
    assert_eq!(node.rx_bad, 3, "every bad list is counted");
    assert_eq!(events, 0, "none reached the board");
    assert!(node.next_tx().is_none(), "none was forwarded");
}

#[test]
fn an_older_watch_list_is_ignored_and_ids_compare_across_the_wrap() {
    let mut node = Mesh::new(mac(1), &KEY, 1000);
    let mut applied = Vec::new();
    // Each offer is a new frame from the same origin, so only its id decides.
    for (seq, id) in [0xFFFE, 0x0001, 0xFFFE].into_iter().enumerate() {
        let body = watch_body(id, &addresses(80, 1));
        let frame = sealed_watch(FAKE_ORIGIN, 5, seq as u32 + 1, &body);
        node.receive(1000, mac(0), -50, &frame, &mut |event| {
            if let Event::Watch { count, addresses } = event {
                applied.push((count, addresses[0]));
            }
        });
    }
    // 0x0001 is newer than 0xFFFE across the wrap. 0xFFFE after it is older.
    assert_eq!(applied, vec![(1, mac(80)), (1, mac(80))]);
    assert!(node.next_tx().is_some(), "the newer list was forwarded");
}

#[test]
fn the_root_refuses_more_than_sixteen_addresses() {
    let mut root = Mesh::new(mac(0), &KEY, (5 << 12) | 0x111);
    let mut events = 0;
    assert!(!root.send_watch(&addresses(0, WATCH_MAX + 1), &mut |_| events += 1));
    assert_eq!(events, 0);
    assert!(root.next_tx().is_none(), "nothing was queued");
}

#[test]
fn the_root_numbers_at_most_sixteen_lists_in_one_boot() {
    let mut root = Mesh::new(mac(0), &KEY, (5 << 12) | 0x111);
    for _ in 0..16 {
        assert!(root.send_watch(&addresses(90, 1), &mut quiet));
        while root.next_tx().is_some() {
            root.tx_done(0, true, &mut quiet);
        }
    }
    assert!(
        !root.send_watch(&addresses(90, 1), &mut quiet),
        "a seventeenth list in one boot would repeat an id, so it is refused"
    );
}

// ---- GATT client (T1.10) ----

/// The address the whitelist tests connect to, and one that is never listed.
const LISTED: [u8; 6] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x01];
const UNLISTED: [u8; 6] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x02];

/// A request as the host sends it: id, operation, then the body.
fn gatt_request(id: u16, op: u8, body: &[u8]) -> Vec<u8> {
    let mut out = id.to_le_bytes().to_vec();
    out.push(op);
    out.extend_from_slice(body);
    out
}

fn connect_request(id: u16, address: [u8; 6], kind: u8) -> Vec<u8> {
    gatt_request(id, 0, &[address.as_slice(), &[kind]].concat())
}

fn write_request(id: u16, handle: u16, value: &[u8]) -> Vec<u8> {
    gatt_request(id, 1, &[handle.to_le_bytes().as_slice(), value].concat())
}

fn handle_request(id: u16, op: u8, handle: u16) -> Vec<u8> {
    gatt_request(id, op, &handle.to_le_bytes())
}

fn whitelist_request(id: u16, list: &[[u8; 6]]) -> Vec<u8> {
    let addresses: Vec<u8> = list.iter().flatten().copied().collect();
    gatt_request(id, 5, &[&[list.len() as u8], addresses.as_slice()].concat())
}

/// The answer in `reply`, or a panic if it is not one.
fn answered(reply: Option<Reply>) -> (u16, Status) {
    match reply {
        Some(Reply::Answer { id, status, .. }) => (id, status),
        other => panic!("expected an answer, got {other:?}"),
    }
}

/// A client whose whitelist holds `LISTED`, and that is idle.
fn listed_client() -> Client {
    let mut client = Client::new();
    assert_eq!(
        answered(client.request(0, &whitelist_request(1, &[LISTED]))),
        (1, Status::Ok)
    );
    client
}

/// Connects request `id` to `LISTED` through the whole setup. Returns with the
/// link open.
fn open_link(client: &mut Client, now: u32, id: u16) {
    assert!(client
        .request(now, &connect_request(id, LISTED, 0))
        .is_none());
    assert_eq!(
        client.take_act(),
        Some(Act::Open {
            address: LISTED,
            address_kind: 0
        })
    );
    assert!(client.done(now, Done::Opened(true)).is_none());
    assert_eq!(client.take_act(), Some(Act::Mtu));
    assert!(client.done(now, Done::Mtu(true)).is_none());
    assert_eq!(client.take_act(), Some(Act::Search));
    assert_eq!(
        answered(client.done(now, Done::Searched(true))),
        (id, Status::Ok)
    );
}

#[test]
fn a_whitelist_that_drops_the_connected_device_closes_the_link() {
    let mut client = listed_client();
    open_link(&mut client, 10, 2);
    assert_eq!(
        answered(client.request(20, &whitelist_request(3, &[UNLISTED]))),
        (3, Status::Ok)
    );
    assert_eq!(client.take_act(), Some(Act::Close));
    assert!(client.done(21, Done::Closed).is_none());
    assert!(!client.link_busy());
}

#[test]
fn a_whitelist_that_keeps_the_connected_device_leaves_the_link_up() {
    let mut client = listed_client();
    open_link(&mut client, 10, 2);
    assert_eq!(
        answered(client.request(20, &whitelist_request(3, &[UNLISTED, LISTED]))),
        (3, Status::Ok)
    );
    assert!(client.take_act().is_none());
    assert!(client.link_busy());
}

#[test]
fn a_whitelist_that_drops_a_device_still_opening_cancels_it() {
    let mut client = listed_client();
    assert!(client.request(10, &connect_request(2, LISTED, 0)).is_none());
    client.take_act();
    assert_eq!(
        answered(client.request(20, &whitelist_request(3, &[]))),
        (3, Status::Ok)
    );
    assert_eq!(client.take_act(), Some(Act::Cancel));
    // The open beat the cancel, so it is closed rather than left over.
    assert!(client.done(21, Done::Opened(true)).is_none());
    assert_eq!(client.take_act(), Some(Act::Close));
    assert!(client.done(22, Done::Closed).is_none());
    assert!(!client.link_busy());
}

#[test]
fn a_whitelist_that_drops_a_device_in_setup_closes_it_and_setup_stops() {
    let mut client = listed_client();
    assert!(client.request(10, &connect_request(2, LISTED, 0)).is_none());
    client.take_act();
    assert!(client.done(11, Done::Opened(true)).is_none());
    assert_eq!(client.take_act(), Some(Act::Mtu));
    assert_eq!(
        answered(client.request(20, &whitelist_request(3, &[]))),
        (3, Status::Ok)
    );
    assert_eq!(client.take_act(), Some(Act::Close));
    assert!(client.done(21, Done::Mtu(true)).is_none());
    assert!(client.take_act().is_none(), "setup must not resume");
    assert!(client.done(22, Done::Closed).is_none());
    assert!(!client.link_busy());
}

#[test]
fn a_connect_to_a_listed_address_runs_its_setup_and_then_answers_ok() {
    let mut client = listed_client();
    open_link(&mut client, 10, 2);
    assert!(client.link_busy());
    assert!(client.take_act().is_none());
}

#[test]
fn an_empty_whitelist_refuses_every_connect() {
    let mut client = Client::new();
    assert_eq!(
        answered(client.request(0, &connect_request(2, LISTED, 0))),
        (2, Status::Refused)
    );
    assert!(client.take_act().is_none(), "no radio call was made");
    assert!(!client.link_busy());
}

#[test]
fn a_whitelist_of_none_clears_the_list_and_refuses_again() {
    let mut client = listed_client();
    assert_eq!(
        answered(client.request(0, &whitelist_request(3, &[]))),
        (3, Status::Ok)
    );
    assert_eq!(
        answered(client.request(0, &connect_request(4, LISTED, 0))),
        (4, Status::Refused)
    );
}

#[test]
fn a_connect_to_an_address_off_the_list_is_refused_and_never_opens() {
    let mut client = listed_client();
    assert_eq!(
        answered(client.request(0, &connect_request(2, UNLISTED, 0))),
        (2, Status::Refused)
    );
    assert!(client.take_act().is_none(), "the radio was never asked");
    assert!(!client.link_busy());
}

#[test]
fn a_new_whitelist_replaces_the_old_one_whole() {
    let mut client = Client::new();
    client.request(0, &whitelist_request(1, &[LISTED, UNLISTED]));
    client.request(0, &whitelist_request(2, &[UNLISTED]));
    assert_eq!(
        answered(client.request(0, &connect_request(3, LISTED, 0))),
        (3, Status::Refused),
        "an address from the old list is gone"
    );
    assert!(client
        .request(0, &connect_request(4, UNLISTED, 0))
        .is_none());
    assert!(client.take_act().is_some());
}

#[test]
fn a_second_connect_answers_busy_while_opening_and_while_open() {
    let mut client = listed_client();
    assert!(client.request(0, &connect_request(2, LISTED, 0)).is_none());
    assert_eq!(
        answered(client.request(0, &connect_request(3, LISTED, 0))),
        (3, Status::Busy),
        "busy while opening"
    );
    client.take_act();
    client.done(0, Done::Opened(true));
    client.take_act();
    client.done(0, Done::Mtu(true));
    client.take_act();
    client.done(0, Done::Searched(true));
    assert_eq!(
        answered(client.request(0, &connect_request(4, LISTED, 0))),
        (4, Status::Busy),
        "busy while open"
    );
}

#[test]
fn a_request_with_no_connection_answers_not_connected() {
    let mut client = listed_client();
    assert_eq!(
        answered(client.request(0, &write_request(5, 0x21, &[1]))),
        (5, Status::NotConnected)
    );
    assert_eq!(
        answered(client.request(0, &handle_request(6, 2, 0x21))),
        (6, Status::NotConnected),
        "a read too"
    );
    assert!(client.take_act().is_none());
}

#[test]
fn a_write_answers_once_the_radio_confirms_it() {
    let mut client = listed_client();
    open_link(&mut client, 0, 2);
    assert!(client
        .request(0, &write_request(7, 0x21, &[1, 2, 3]))
        .is_none());
    assert_eq!(
        client.take_act(),
        Some(Act::Write {
            handle: 0x21,
            len: 3,
            value: with_bytes(&[1, 2, 3]),
        })
    );
    assert_eq!(
        answered(client.done(0, Done::Written(true))),
        (7, Status::Ok)
    );
}

fn with_bytes(bytes: &[u8]) -> [u8; VALUE_MAX] {
    let mut out = [0u8; VALUE_MAX];
    out[..bytes.len()].copy_from_slice(bytes);
    out
}

#[test]
fn a_read_returns_its_bytes_exactly_and_never_as_text() {
    let mut client = listed_client();
    open_link(&mut client, 0, 2);
    assert!(client.request(0, &handle_request(8, 2, 0x30)).is_none());
    assert_eq!(client.take_act(), Some(Act::Read { handle: 0x30 }));
    let reply = client.done(
        0,
        Done::Read {
            ok: true,
            len: 3,
            value: with_bytes(&[0xFF, 0x00, 0x80]),
        },
    );
    let Some(Reply::Answer {
        id,
        status,
        len,
        value,
    }) = reply
    else {
        panic!("expected an answer, got {reply:?}");
    };
    assert_eq!((id, status, len), (8, Status::Ok, 3));
    assert_eq!(&value[..3], &[0xFF, 0x00, 0x80]);
}

#[test]
fn a_subscribe_registers_and_then_enables_the_descriptor() {
    let mut client = listed_client();
    open_link(&mut client, 0, 2);
    assert!(client.request(0, &handle_request(9, 3, 0x21)).is_none());
    assert_eq!(client.take_act(), Some(Act::Register { handle: 0x21 }));
    assert!(client.done(0, Done::Registered(true)).is_none());
    assert_eq!(client.take_act(), Some(Act::Enable { handle: 0x21 }));
    assert_eq!(
        answered(client.done(0, Done::Written(true))),
        (9, Status::Ok)
    );
}

#[test]
fn a_repeated_id_for_the_request_in_flight_is_ignored_and_not_sent_twice() {
    let mut client = listed_client();
    open_link(&mut client, 0, 2);
    assert!(client.request(0, &write_request(7, 0x21, &[1])).is_none());
    assert!(client.take_act().is_some());
    assert!(
        client.request(1, &write_request(7, 0x21, &[1])).is_none(),
        "no answer yet"
    );
    assert!(client.take_act().is_none(), "the radio was not asked twice");
    assert_eq!(
        answered(client.done(2, Done::Written(true))),
        (7, Status::Ok)
    );
}

#[test]
fn a_new_id_while_one_request_is_in_flight_answers_busy() {
    let mut client = listed_client();
    open_link(&mut client, 0, 2);
    assert!(client.request(0, &write_request(7, 0x21, &[1])).is_none());
    assert_eq!(
        answered(client.request(0, &write_request(8, 0x21, &[1]))),
        (8, Status::Busy)
    );
}

#[test]
fn a_lost_answer_is_sent_again_for_the_same_id_and_the_write_is_not_repeated() {
    let mut client = listed_client();
    open_link(&mut client, 0, 2);
    assert!(client.request(0, &write_request(7, 0x21, &[1])).is_none());
    client.take_act();
    client.done(0, Done::Written(true));
    // The host never heard the answer, so it asks again with the same id.
    assert_eq!(
        answered(client.request(1, &write_request(7, 0x21, &[1]))),
        (7, Status::Ok)
    );
    assert!(client.take_act().is_none(), "the write was not sent again");
}

#[test]
fn a_lost_connect_answer_is_sent_again_and_does_not_reconnect() {
    let mut client = listed_client();
    open_link(&mut client, 0, 2);
    assert_eq!(
        answered(client.request(1, &connect_request(2, LISTED, 0))),
        (2, Status::Ok)
    );
    assert!(
        client.take_act().is_none(),
        "no second connection was opened"
    );
}

#[test]
fn a_request_past_its_timeout_answers_timed_out_and_drops_the_link() {
    let mut client = listed_client();
    open_link(&mut client, 0, 2);
    assert!(client.request(100, &write_request(9, 0x21, &[1])).is_none());
    client.take_act();
    assert!(client.tick(100 + REQUEST_MS - 1).is_none(), "not yet");
    assert_eq!(
        answered(client.tick(100 + REQUEST_MS)),
        (9, Status::TimedOut)
    );
    assert_eq!(client.take_act(), Some(Act::Close), "the link is dropped");
    assert!(client.link_busy(), "closing until the radio confirms");
    assert_eq!(
        answered(client.request(5_000, &write_request(10, 0x21, &[1]))),
        (10, Status::NotConnected)
    );
    assert!(client.done(5_000, Done::Closed).is_none());
    assert!(!client.link_busy());
}

#[test]
fn a_connect_that_never_finishes_times_out_and_cancels_the_open() {
    let mut client = listed_client();
    assert!(client.request(0, &connect_request(2, LISTED, 0)).is_none());
    client.take_act();
    assert!(client.tick(CONNECT_MS - 1).is_none());
    assert_eq!(answered(client.tick(CONNECT_MS)), (2, Status::TimedOut));
    assert_eq!(client.take_act(), Some(Act::Cancel));
}

#[test]
fn a_late_result_for_a_timed_out_request_is_ignored() {
    let mut client = listed_client();
    open_link(&mut client, 0, 2);
    assert!(client.request(0, &write_request(9, 0x21, &[1])).is_none());
    client.take_act();
    client.tick(REQUEST_MS);
    client.take_act();
    assert!(
        client.done(REQUEST_MS + 1, Done::Written(true)).is_none(),
        "the stack finished a write nobody is waiting for"
    );
    assert_eq!(
        answered(client.request(REQUEST_MS + 2, &connect_request(11, LISTED, 0))),
        (11, Status::Busy),
        "still closing, so a connect waits"
    );
    client.done(REQUEST_MS + 3, Done::Closed);
    assert!(client
        .request(REQUEST_MS + 4, &connect_request(12, LISTED, 0))
        .is_none());
}

#[test]
fn an_open_that_lands_after_the_cancel_was_confirmed_is_closed() {
    let mut client = listed_client();
    assert!(client.request(0, &connect_request(2, LISTED, 0)).is_none());
    assert_eq!(
        client.take_act(),
        Some(Act::Open {
            address: LISTED,
            address_kind: 0
        })
    );
    // A replacement list drops the address while the open is in flight.
    assert_eq!(
        answered(client.request(1, &whitelist_request(3, &[]))),
        (3, Status::Ok)
    );
    client.take_act();
    client.done(2, Done::Closed);
    assert!(!client.link_busy(), "the cancel looked final");
    assert!(client.done(3, Done::Opened(true)).is_none());
    assert_eq!(
        client.take_act(),
        Some(Act::Close),
        "the late link is closed"
    );
    assert!(client.link_busy(), "the radio stays quiet until it closes");
    client.done(4, Done::Closed);
    assert!(!client.link_busy());
}

#[test]
fn an_open_that_lands_after_closing_timed_out_is_closed() {
    let mut client = listed_client();
    assert!(client.request(0, &connect_request(2, LISTED, 0)).is_none());
    client.take_act();
    assert_eq!(
        answered(client.request(1, &whitelist_request(3, &[]))),
        (3, Status::Ok)
    );
    client.take_act();
    assert!(client.tick(1 + REQUEST_MS).is_none());
    assert!(!client.link_busy(), "closing gave up");
    assert!(client.done(2 + REQUEST_MS, Done::Opened(true)).is_none());
    assert_eq!(client.take_act(), Some(Act::Close));
}

#[test]
fn a_closing_link_the_radio_never_confirms_goes_idle_after_a_timeout() {
    let mut client = listed_client();
    open_link(&mut client, 0, 2);
    assert!(client.request(0, &write_request(9, 0x21, &[1])).is_none());
    client.take_act();
    client.tick(REQUEST_MS);
    client.take_act();
    assert!(client.link_busy());
    client.tick(2 * REQUEST_MS);
    assert!(
        !client.link_busy(),
        "a lost confirmation cannot block for good"
    );
}

#[test]
fn a_disconnect_answers_ok_once_the_radio_confirms_the_close() {
    let mut client = listed_client();
    open_link(&mut client, 0, 2);
    assert!(client.request(0, &gatt_request(3, 4, &[])).is_none());
    assert_eq!(client.take_act(), Some(Act::Close));
    assert_eq!(answered(client.done(1, Done::Closed)), (3, Status::Ok));
    assert!(!client.link_busy());
}

#[test]
fn a_link_that_drops_on_its_own_is_reported_once_as_closed() {
    let mut client = listed_client();
    open_link(&mut client, 0, 2);
    assert_eq!(
        client.done(0, Done::Closed),
        Some(Reply::Closed { link: 2 })
    );
    assert!(client.done(1, Done::Closed).is_none(), "reported once");
}

#[test]
fn a_write_in_flight_when_the_link_drops_answers_not_connected() {
    let mut client = listed_client();
    open_link(&mut client, 0, 2);
    client.request(0, &write_request(7, 0x21, &[1]));
    client.take_act();
    assert_eq!(
        answered(client.done(1, Done::Closed)),
        (7, Status::NotConnected)
    );
}

#[test]
fn a_failed_open_answers_failed_and_the_node_goes_idle() {
    let mut client = listed_client();
    client.request(0, &connect_request(2, LISTED, 0));
    client.take_act();
    assert_eq!(
        answered(client.done(0, Done::Opened(false))),
        (2, Status::Failed)
    );
    assert!(!client.link_busy());
}

#[test]
fn a_failed_mtu_closes_the_link_it_made_and_answers_failed() {
    let mut client = listed_client();
    client.request(0, &connect_request(2, LISTED, 0));
    client.take_act();
    client.done(0, Done::Opened(true));
    assert_eq!(client.take_act(), Some(Act::Mtu));
    assert_eq!(
        answered(client.done(0, Done::Mtu(false))),
        (2, Status::Failed)
    );
    assert_eq!(client.take_act(), Some(Act::Close), "no link is left over");
}

#[test]
fn an_open_that_completes_after_the_node_gave_up_is_closed() {
    let mut client = listed_client();
    client.request(0, &connect_request(2, LISTED, 0));
    client.take_act();
    client.tick(CONNECT_MS);
    assert_eq!(client.take_act(), Some(Act::Cancel));
    assert!(client.done(CONNECT_MS, Done::Opened(true)).is_none());
    assert_eq!(
        client.take_act(),
        Some(Act::Close),
        "the late link is closed"
    );
}

#[test]
fn a_stale_result_for_another_operation_is_ignored() {
    let mut client = listed_client();
    open_link(&mut client, 0, 2);
    client.request(0, &write_request(7, 0x21, &[1]));
    client.take_act();
    let empty = Done::Read {
        ok: true,
        len: 0,
        value: [0; VALUE_MAX],
    };
    assert!(client.done(0, empty).is_none());
    assert_eq!(
        answered(client.done(0, Done::Written(true))),
        (7, Status::Ok),
        "the write is still waiting for its own result"
    );
}

#[test]
fn a_notification_goes_up_as_its_bytes_only_while_connected() {
    let mut client = listed_client();
    let mut value = [0u8; NOTIFY_MAX];
    value[..3].copy_from_slice(&[0xFF, 0x00, 0x80]);
    let note = Done::Notify {
        handle: 0x21,
        len: 3,
        value,
    };
    assert!(client.done(0, note).is_none(), "no link, no notification");
    open_link(&mut client, 0, 2);
    assert_eq!(
        client.done(1, note),
        Some(Reply::Notify {
            link: 2,
            handle: 0x21,
            len: 3,
            value,
        })
    );
}

#[test]
fn a_malformed_request_answers_malformed_when_its_id_can_be_read() {
    let mut client = listed_client();
    // A write whose body is one byte, too short for a handle.
    assert_eq!(
        answered(client.request(0, &[9, 0, 1, 0x21])),
        (9, Status::Malformed)
    );
    assert!(client.request(0, &[9]).is_none(), "no id, no answer");
}

#[test]
fn a_connect_with_an_address_type_that_is_not_public_or_random_is_malformed() {
    let mut client = listed_client();
    assert_eq!(
        answered(client.request(0, &connect_request(2, LISTED, 2))),
        (2, Status::Malformed)
    );
    assert!(client.take_act().is_none());
}

#[test]
fn a_whitelist_of_nine_addresses_is_malformed_and_changes_nothing() {
    let mut client = listed_client();
    let nine = [UNLISTED; 9];
    assert_eq!(
        answered(client.request(0, &whitelist_request(2, &nine))),
        (2, Status::Malformed)
    );
    assert!(
        client.request(0, &connect_request(3, LISTED, 0)).is_none(),
        "the old list still holds, so the connect is let through"
    );
    assert!(client.take_act().is_some());
}

#[test]
fn a_gatt_request_reaches_its_node_and_the_answer_comes_back_up() {
    let mut sim = Sim::new(5, &chain(5), 100);
    sim.run_ms(20_000);
    assert!(sim.report(4, 1));
    sim.run_ms(1_000);
    let target = sim.id(4);
    let request = gatt_request(21, 2, &0x30u16.to_le_bytes());
    let now = sim.now;
    assert!(sim.nodes[0].send_down(now, target, FrameKind::Gatt, &request));
    sim.run_ms(1_000);
    let arrived = sim
        .arrivals
        .iter()
        .find(|entry| entry.kind == FrameKind::Gatt)
        .expect("the request reached its node");
    assert_eq!(arrived.payload, request, "the bytes are the host's bytes");
    assert_eq!(arrived.hops, 3, "three forwards on the way down");

    // The node answers, and the answer goes up as bytes, not as a Ping.
    let answer = [21u8, 0, 1, 0, 0xde, 0xad];
    let now = sim.now;
    assert!(sim.nodes[4].send_up(FrameKind::GattUp, &answer, now, &mut quiet));
    sim.run_ms(1_000);
    let up = sim
        .arrivals
        .iter()
        .find(|entry| entry.kind == FrameKind::GattUp)
        .expect("the answer reached the root");
    assert_eq!(up.payload, answer);
    assert_eq!(up.origin, target);
}

#[test]
fn only_the_root_sends_gatt_requests_and_an_answer_kind_is_never_sent_down() {
    let mut sim = Sim::new(3, &chain(3), 100);
    sim.run_ms(20_000);
    assert!(sim.report(2, 1));
    sim.run_ms(1_000);
    let now = sim.now;
    let target = sim.id(2);
    assert!(!sim.nodes[1].send_down(now, target, FrameKind::Gatt, &[0; 3]));
    assert!(!sim.nodes[0].send_down(now, target, FrameKind::GattUp, &[0; 3]));
    assert!(sim.nodes[0].send_down(now, target, FrameKind::Gatt, &[0; 3]));
}

const OTA_SEED: [u8; 32] = [0x42; 32];

const OTA_VERSION: u32 = 7;

fn sign_digest(product: u8, version: u32, digest: &[u8; DIGEST]) -> [u8; 64] {
    let pair = KeyPair::from_seed(Seed::new(OTA_SEED));
    *pair
        .sk
        .sign(ota::signed_message(product, version, digest), None)
}

fn sign_image(image: &[u8]) -> ([u8; DIGEST], [u8; 64], [u8; 32]) {
    let mut hash = ota::ImageHash::new();
    hash.update(image);
    let digest = hash.finish();
    let pair = KeyPair::from_seed(Seed::new(OTA_SEED));
    let signature = sign_digest(PRODUCT_NODE, OTA_VERSION, &digest);
    (digest, signature, *pair.pk)
}

fn new_receiver() -> Receiver<SLOT, BITMAP> {
    Receiver::new(PRODUCT_NODE, 0)
}

fn image_of(size: usize) -> Vec<u8> {
    (0..size)
        .map(|index| (index.wrapping_mul(3) + 1) as u8)
        .collect()
}

fn down_bytes(message: &Down) -> Vec<u8> {
    let mut out = [0u8; 214];
    let length = ota::encode_down(message, &mut out).expect("the message fits");
    out[..length].to_vec()
}

fn chunk_at(image: &[u8], offset: usize) -> Down {
    let end = (offset + CHUNK).min(image.len());
    let mut data = [0u8; CHUNK];
    let len = end - offset;
    data[..len].copy_from_slice(&image[offset..end]);
    Down::Chunk {
        offset: offset as u32,
        len: len as u8,
        data,
    }
}

fn feed<const MAX: u32, const N: usize>(
    receiver: &mut Receiver<MAX, N>,
    payload: &[u8],
    key: &[u8; 32],
) -> (Option<Up>, Vec<Action>) {
    let up = receiver.push(payload, key);
    let mut actions = Vec::new();
    while let Some(action) = receiver.take_action() {
        actions.push(action);
    }
    (up, actions)
}

/// A chain of three with a route to the leaf, so a downward frame crosses two links.
fn routed_leaf() -> (Sim, u32) {
    let mut sim = Sim::new(3, &chain(3), 100);
    sim.run_ms(20_000);
    assert!(sim.report(2, 1));
    sim.run_ms(1_000);
    let target = sim.id(2);
    (sim, target)
}

/// Sends one OTA body down to `target` and returns the copies that reached it.
fn cross(sim: &mut Sim, target: u32, body: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let seen = sim.arrivals.len();
    let now = sim.now;
    assert!(sim.nodes[0].send_down(now, target, FrameKind::Ota, body));
    sim.run_ms(1_000);
    sim.arrivals[seen..]
        .iter()
        .filter(|entry| entry.kind == FrameKind::Ota)
        .map(|entry| (entry.hops, entry.payload.clone()))
        .collect()
}

fn begin_for(image: &[u8], digest: [u8; DIGEST], signature: [u8; 64]) -> Vec<u8> {
    down_bytes(&Down::Begin {
        size: image.len() as u32,
        product: PRODUCT_NODE,
        version: OTA_VERSION,
        digest,
        signature,
    })
}

#[test]
fn a_leaf_still_receives_an_ota_down_aimed_at_it() {
    let (mut sim, target) = routed_leaf();
    sim.nodes[2].set_leaf();
    let arrived = cross(&mut sim, target, &[1, 2, 3]);
    assert_eq!(arrived.len(), 1);
    assert_eq!(arrived[0].1, vec![1, 2, 3]);
}

#[test]
fn a_camera_receiver_accepts_its_slot_and_the_node_receiver_does_not() {
    let (digest, _, public) = sign_image(&[1]);
    let begin_of = |size: u32| {
        down_bytes(&Down::Begin {
            size,
            product: PRODUCT_CAM0,
            version: OTA_VERSION,
            digest,
            signature: sign_digest(PRODUCT_CAM0, OTA_VERSION, &digest),
        })
    };
    let mut camera = Receiver::<CAM_SLOT, CAM_BITMAP>::new(PRODUCT_CAM0, 0);
    let (up, actions) = feed(&mut camera, &begin_of(CAM_SLOT), &public);
    assert!(up.is_none());
    assert_eq!(actions, vec![Action::Begin { size: CAM_SLOT }]);
    let mut camera = Receiver::<CAM_SLOT, CAM_BITMAP>::new(PRODUCT_CAM0, 0);
    let (up, actions) = feed(&mut camera, &begin_of(CAM_SLOT + 1), &public);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::Refused
        })
    );
    assert!(actions.is_empty());
    let mut node = new_receiver();
    let (up, actions) = feed(&mut node, &begin_of(CAM_SLOT), &public);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::Refused
        })
    );
    assert!(actions.is_empty());
}

#[test]
fn a_full_image_crosses_two_hops_and_finishes() {
    let (mut sim, target) = routed_leaf();
    let now = sim.now;
    assert!(
        !sim.nodes[1].send_down(now, target, FrameKind::Ota, &[1]),
        "a node that is not the root cannot send downward"
    );
    assert!(
        !sim.nodes[0].send_down(now, target, FrameKind::OtaUp, &[2, 0]),
        "a result is not a downward kind"
    );
    let image = image_of(400);
    let (digest, signature, public) = sign_image(&image);
    let mut receiver = new_receiver();
    let arrived = cross(&mut sim, target, &begin_for(&image, digest, signature));
    assert_eq!(arrived.len(), 1);
    assert_eq!(arrived[0].0, 1, "one forward across two links");
    let (up, actions) = feed(&mut receiver, &arrived[0].1, &public);
    assert!(up.is_none());
    assert_eq!(actions, vec![Action::Begin { size: 400 }]);

    for offset in (0..image.len()).step_by(CHUNK) {
        let arrived = cross(&mut sim, target, &down_bytes(&chunk_at(&image, offset)));
        assert_eq!(arrived.len(), 1, "the chunk at {offset} arrived once");
        let (up, actions) = feed(&mut receiver, &arrived[0].1, &public);
        assert!(up.is_none());
        assert!(
            matches!(actions.as_slice(), [Action::Write { offset: at, .. }] if *at == offset as u32),
            "write the chunk at {offset}"
        );
    }
    let arrived = cross(&mut sim, target, &down_bytes(&Down::Commit));
    let (up, actions) = feed(&mut receiver, &arrived[0].1, &public);
    assert_eq!(up, Some(Up::Result { code: Code::Ok }));
    assert_eq!(
        actions,
        vec![Action::Finish {
            version: OTA_VERSION
        }]
    );

    let mut result = [0u8; 8];
    let length = ota::encode_up(&up.expect("a result"), &mut result).expect("fits");
    let now = sim.now;
    assert!(sim.nodes[2].send_up(FrameKind::OtaUp, &result[..length], now, &mut quiet));
    sim.run_ms(1_000);
    let back = sim
        .arrivals
        .iter()
        .find(|entry| entry.kind == FrameKind::OtaUp)
        .expect("the result reached the root");
    assert_eq!(back.payload, &result[..length]);
    assert_eq!(back.origin, target);
}

#[test]
fn a_lost_chunk_is_named_by_status_and_a_resend_finishes() {
    let (mut sim, target) = routed_leaf();
    let image = image_of(400);
    let (digest, signature, public) = sign_image(&image);
    let mut receiver = new_receiver();
    let arrived = cross(&mut sim, target, &begin_for(&image, digest, signature));
    feed(&mut receiver, &arrived[0].1, &public);

    let first = down_bytes(&chunk_at(&image, 0));
    let arrived = cross(&mut sim, target, &first);
    let (_, actions) = feed(&mut receiver, &arrived[0].1, &public);
    assert!(matches!(
        actions.as_slice(),
        [Action::Write { offset: 0, .. }]
    ));
    let arrived = cross(&mut sim, target, &first);
    let (_, actions) = feed(&mut receiver, &arrived[0].1, &public);
    assert!(actions.is_empty(), "a chunk already written is harmless");

    // The middle chunk never arrives. The last one is early, so it is not kept.
    let arrived = cross(&mut sim, target, &down_bytes(&chunk_at(&image, 340)));
    let (_, actions) = feed(&mut receiver, &arrived[0].1, &public);
    assert!(
        actions.is_empty(),
        "a chunk ahead of the cursor is not stored"
    );

    let arrived = cross(&mut sim, target, &down_bytes(&Down::Query { window: 0 }));
    let (up, actions) = feed(&mut receiver, &arrived[0].1, &public);
    assert!(actions.is_empty());
    let Some(Up::Status { window: 0, missing }) = up else {
        panic!("query returns a status, got {up:?}");
    };
    assert_eq!(missing[0] & 0b111, 0b110, "chunks 1 and 2 are the hole");
    assert_eq!(&missing[1..], &[0u8; WINDOW_BYTES - 1]);

    let mut body = [0u8; 40];
    let length = ota::encode_up(&up.expect("status"), &mut body).expect("fits");
    let now = sim.now;
    assert!(sim.nodes[2].send_up(FrameKind::OtaUp, &body[..length], now, &mut quiet));
    sim.run_ms(1_000);
    let back = sim
        .arrivals
        .iter()
        .rev()
        .find(|entry| entry.kind == FrameKind::OtaUp)
        .expect("the status reached the root");
    assert_eq!(
        ota::decode_up(&back.payload),
        Some(Up::Status { window: 0, missing })
    );

    for offset in [170, 340] {
        let arrived = cross(&mut sim, target, &down_bytes(&chunk_at(&image, offset)));
        let (_, actions) = feed(&mut receiver, &arrived[0].1, &public);
        assert!(
            matches!(actions.as_slice(), [Action::Write { offset: at, .. }] if *at == offset as u32)
        );
    }
    let arrived = cross(&mut sim, target, &down_bytes(&Down::Commit));
    let (up, actions) = feed(&mut receiver, &arrived[0].1, &public);
    assert_eq!(up, Some(Up::Result { code: Code::Ok }));
    assert_eq!(
        actions,
        vec![Action::Finish {
            version: OTA_VERSION
        }]
    );
}

#[test]
fn a_bad_signature_is_refused_at_begin_before_any_flash_action() {
    let image = image_of(170);
    let (digest, mut signature, public) = sign_image(&image);
    signature[0] ^= 0x01;
    let mut receiver = new_receiver();
    let (up, actions) = feed(
        &mut receiver,
        &begin_for(&image, digest, signature),
        &public,
    );
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::BadSignature
        })
    );
    assert!(actions.is_empty(), "nothing may erase flash for this image");
    let (up, actions) = feed(&mut receiver, &down_bytes(&chunk_at(&image, 0)), &public);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::Refused
        })
    );
    assert!(actions.is_empty());
    let (up, actions) = feed(&mut receiver, &down_bytes(&Down::Commit), &public);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::Refused
        })
    );
    assert!(actions.is_empty());
}

#[test]
fn a_signature_that_fails_at_commit_does_not_finish() {
    // Commit checks again, so a key that changed under the transfer cannot pass.
    let image = image_of(170);
    let (digest, signature, public) = sign_image(&image);
    let other = *KeyPair::from_seed(Seed::new([0x43; 32])).pk;
    let mut receiver = new_receiver();
    let (up, _) = feed(
        &mut receiver,
        &begin_for(&image, digest, signature),
        &public,
    );
    assert!(up.is_none());
    feed(&mut receiver, &down_bytes(&chunk_at(&image, 0)), &public);
    let (up, actions) = feed(&mut receiver, &down_bytes(&Down::Commit), &other);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::BadSignature
        })
    );
    assert_eq!(actions, vec![Action::Abort]);
}

#[test]
fn an_image_for_another_product_is_refused_with_no_flash_action() {
    let image = image_of(170);
    let (digest, _, public) = sign_image(&image);
    let mut receiver = new_receiver();
    let begin = down_bytes(&Down::Begin {
        size: 170,
        product: PRODUCT_CAM0,
        version: OTA_VERSION,
        digest,
        signature: sign_digest(PRODUCT_CAM0, OTA_VERSION, &digest),
    });
    let (up, actions) = feed(&mut receiver, &begin, &public);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::WrongProduct
        })
    );
    assert!(actions.is_empty());
}

#[test]
fn an_image_below_the_floor_is_refused_and_an_equal_one_is_a_retry() {
    let image = image_of(170);
    let (digest, _, public) = sign_image(&image);
    let begin_at = |version: u32| {
        down_bytes(&Down::Begin {
            size: 170,
            product: PRODUCT_NODE,
            version,
            digest,
            signature: sign_digest(PRODUCT_NODE, version, &digest),
        })
    };
    let mut receiver: Receiver<SLOT, BITMAP> = Receiver::new(PRODUCT_NODE, 5);
    let (up, actions) = feed(&mut receiver, &begin_at(4), &public);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::Downgrade
        })
    );
    assert!(actions.is_empty());
    let (up, actions) = feed(&mut receiver, &begin_at(5), &public);
    assert!(up.is_none());
    assert_eq!(actions, vec![Action::Begin { size: 170 }]);
    let (up, actions) = feed(&mut receiver, &begin_at(6), &public);
    assert!(up.is_none());
    assert_eq!(actions, vec![Action::Abort, Action::Begin { size: 170 }]);
}

#[test]
fn finish_carries_the_version_that_was_signed() {
    let image = image_of(170);
    let (digest, _, public) = sign_image(&image);
    let mut receiver: Receiver<SLOT, BITMAP> = Receiver::new(PRODUCT_NODE, 3);
    let begin = down_bytes(&Down::Begin {
        size: 170,
        product: PRODUCT_NODE,
        version: 9,
        digest,
        signature: sign_digest(PRODUCT_NODE, 9, &digest),
    });
    feed(&mut receiver, &begin, &public);
    feed(&mut receiver, &down_bytes(&chunk_at(&image, 0)), &public);
    let (_, actions) = feed(&mut receiver, &down_bytes(&Down::Commit), &public);
    assert_eq!(actions, vec![Action::Finish { version: 9 }]);
}

#[test]
fn a_refused_begin_leaves_the_image_in_progress_alone() {
    let image = image_of(170);
    let (digest, signature, public) = sign_image(&image);
    let mut receiver = new_receiver();
    feed(
        &mut receiver,
        &begin_for(&image, digest, signature),
        &public,
    );
    let mut bad = signature;
    bad[0] ^= 1;
    let (_, actions) = feed(&mut receiver, &begin_for(&image, digest, bad), &public);
    assert!(actions.is_empty(), "a forged begin cannot abort the write");
    feed(&mut receiver, &down_bytes(&chunk_at(&image, 0)), &public);
    let (up, actions) = feed(&mut receiver, &down_bytes(&Down::Commit), &public);
    assert_eq!(up, Some(Up::Result { code: Code::Ok }));
    assert_eq!(
        actions,
        vec![Action::Finish {
            version: OTA_VERSION
        }]
    );
}

#[test]
fn the_writes_reassemble_to_exactly_the_image() {
    let image = image_of(CHUNK * 2 + 37);
    let (digest, signature, public) = sign_image(&image);
    let mut receiver = new_receiver();
    feed(
        &mut receiver,
        &begin_for(&image, digest, signature),
        &public,
    );
    let mut flash = vec![0xffu8; image.len()];
    for offset in (0..image.len()).step_by(CHUNK) {
        let (_, actions) = feed(
            &mut receiver,
            &down_bytes(&chunk_at(&image, offset)),
            &public,
        );
        assert_eq!(actions.len(), 1);
        let Action::Write {
            offset: at,
            len,
            data,
        } = actions[0]
        else {
            panic!("a chunk writes");
        };
        let at = at as usize;
        flash[at..at + usize::from(len)].copy_from_slice(&data[..usize::from(len)]);
    }
    assert_eq!(flash, image);
}

#[test]
fn a_zero_size_begin_is_refused_with_no_flash_action() {
    let image = image_of(1);
    let (digest, _, public) = sign_image(&image);
    let mut receiver = new_receiver();
    let begin = down_bytes(&Down::Begin {
        size: 0,
        product: PRODUCT_NODE,
        version: OTA_VERSION,
        digest,
        signature: sign_digest(PRODUCT_NODE, OTA_VERSION, &digest),
    });
    let (up, actions) = feed(&mut receiver, &begin, &public);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::Refused
        })
    );
    assert!(actions.is_empty());
}

#[test]
fn a_begin_of_exactly_the_slot_is_accepted_and_one_more_byte_is_not() {
    let (digest, _, public) = sign_image(&[1]);
    let begin_of = |size: u32| {
        down_bytes(&Down::Begin {
            size,
            product: PRODUCT_NODE,
            version: OTA_VERSION,
            digest,
            signature: sign_digest(PRODUCT_NODE, OTA_VERSION, &digest),
        })
    };
    let mut receiver = new_receiver();
    let (up, actions) = feed(&mut receiver, &begin_of(SLOT), &public);
    assert!(up.is_none());
    assert_eq!(actions, vec![Action::Begin { size: SLOT }]);
    let mut receiver = new_receiver();
    let (up, actions) = feed(&mut receiver, &begin_of(SLOT + 1), &public);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::Refused
        })
    );
    assert!(actions.is_empty());
}

#[test]
fn a_full_chunk_past_the_end_of_the_image_is_not_written() {
    // 100 bytes: a whole 170-byte chunk at offset 0 ends past the image.
    let image = image_of(100);
    let (digest, signature, public) = sign_image(&image);
    let mut receiver = new_receiver();
    feed(
        &mut receiver,
        &begin_for(&image, digest, signature),
        &public,
    );
    let chunk = down_bytes(&Down::Chunk {
        offset: 0,
        len: CHUNK as u8,
        data: [9; CHUNK],
    });
    let (up, actions) = feed(&mut receiver, &chunk, &public);
    assert!(up.is_none());
    assert!(actions.is_empty());
}

#[test]
fn a_short_chunk_in_the_middle_of_the_image_is_not_written() {
    let image = image_of(CHUNK * 2);
    let (digest, signature, public) = sign_image(&image);
    let mut receiver = new_receiver();
    feed(
        &mut receiver,
        &begin_for(&image, digest, signature),
        &public,
    );
    let chunk = down_bytes(&Down::Chunk {
        offset: 0,
        len: 10,
        data: [9; CHUNK],
    });
    let (up, actions) = feed(&mut receiver, &chunk, &public);
    assert!(up.is_none());
    assert!(actions.is_empty());
}

#[test]
fn a_begin_of_the_wrong_length_does_not_decode() {
    let (digest, signature, _) = sign_image(&[1]);
    let begin = begin_for(&[1], digest, signature);
    assert_eq!(begin.len(), BEGIN);
    assert!(ota::decode_down(&begin).is_some());
    assert!(ota::decode_down(&begin[..BEGIN - 1]).is_none());
    let mut long = begin.clone();
    long.push(0);
    assert!(ota::decode_down(&long).is_none());
}

#[test]
fn a_chunk_longer_than_chunk_bytes_does_not_decode() {
    let mut body = vec![2u8, 0, 0, 0, 0];
    body.extend_from_slice(&[7; CHUNK]);
    assert!(ota::decode_down(&body).is_some());
    body.push(7);
    assert!(ota::decode_down(&body).is_none());
}

#[test]
fn a_commit_with_nothing_active_is_refused() {
    let (_, _, public) = sign_image(&[1]);
    let mut receiver = new_receiver();
    let (up, actions) = feed(&mut receiver, &down_bytes(&Down::Commit), &public);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::Refused
        })
    );
    assert!(actions.is_empty());
}

#[test]
fn a_second_commit_is_refused_and_does_not_finish_again() {
    let image = image_of(170);
    let (digest, signature, public) = sign_image(&image);
    let mut receiver = new_receiver();
    feed(
        &mut receiver,
        &begin_for(&image, digest, signature),
        &public,
    );
    feed(&mut receiver, &down_bytes(&chunk_at(&image, 0)), &public);
    let (up, actions) = feed(&mut receiver, &down_bytes(&Down::Commit), &public);
    assert_eq!(up, Some(Up::Result { code: Code::Ok }));
    assert_eq!(actions.len(), 1);
    let (up, actions) = feed(&mut receiver, &down_bytes(&Down::Commit), &public);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::Refused
        })
    );
    assert!(actions.is_empty());
}

#[test]
fn a_bad_hash_does_not_finish() {
    let image = image_of(170);
    let (digest, signature, public) = sign_image(b"not the bytes that were written");
    let mut receiver = new_receiver();
    feed(
        &mut receiver,
        &begin_for(&image, digest, signature),
        &public,
    );
    feed(&mut receiver, &down_bytes(&chunk_at(&image, 0)), &public);
    let (up, actions) = feed(&mut receiver, &down_bytes(&Down::Commit), &public);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::BadHash
        })
    );
    assert_eq!(actions, vec![Action::Abort]);
}

#[test]
fn an_oversized_image_is_refused_and_leaves_the_one_in_progress() {
    let image = image_of(170);
    let (digest, signature, public) = sign_image(&image);
    let mut receiver = new_receiver();
    let (up, actions) = feed(
        &mut receiver,
        &down_bytes(&Down::Begin {
            size: SLOT + 1,
            product: PRODUCT_NODE,
            version: OTA_VERSION,
            digest,
            signature,
        }),
        &public,
    );
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::Refused
        })
    );
    assert!(actions.is_empty());

    feed(
        &mut receiver,
        &begin_for(&image, digest, signature),
        &public,
    );
    let (up, actions) = feed(
        &mut receiver,
        &down_bytes(&Down::Begin {
            size: SLOT + 1,
            product: PRODUCT_NODE,
            version: OTA_VERSION,
            digest,
            signature,
        }),
        &public,
    );
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::Refused
        })
    );
    assert!(
        actions.is_empty(),
        "a refused begin does not abort the image already started"
    );
    feed(&mut receiver, &down_bytes(&chunk_at(&image, 0)), &public);
    let (up, actions) = feed(&mut receiver, &down_bytes(&Down::Commit), &public);
    assert_eq!(up, Some(Up::Result { code: Code::Ok }));
    assert_eq!(
        actions,
        vec![Action::Finish {
            version: OTA_VERSION
        }]
    );
}

#[test]
fn a_chunk_before_begin_is_refused() {
    let image = image_of(1);
    let (_, _, public) = sign_image(&image);
    let mut receiver = new_receiver();
    let (up, actions) = feed(&mut receiver, &down_bytes(&chunk_at(&image, 0)), &public);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::Refused
        })
    );
    assert!(actions.is_empty());
}

#[test]
fn a_commit_with_a_chunk_missing_does_not_finish() {
    let image = image_of(400);
    let (digest, signature, public) = sign_image(&image);
    let mut receiver = new_receiver();
    feed(
        &mut receiver,
        &begin_for(&image, digest, signature),
        &public,
    );
    feed(&mut receiver, &down_bytes(&chunk_at(&image, 0)), &public);
    let (up, actions) = feed(&mut receiver, &down_bytes(&Down::Commit), &public);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::Missing
        })
    );
    assert_eq!(actions, vec![Action::Abort]);
}

#[test]
fn a_chunk_off_a_boundary_or_past_the_end_is_not_written() {
    let image = image_of(170);
    let (digest, signature, public) = sign_image(&image);
    let mut receiver = new_receiver();
    feed(
        &mut receiver,
        &begin_for(&image, digest, signature),
        &public,
    );
    let mut early = [0u8; CHUNK];
    early[0] = 1;
    let off = down_bytes(&Down::Chunk {
        offset: 1,
        len: 1,
        data: early,
    });
    let (up, actions) = feed(&mut receiver, &off, &public);
    assert!(up.is_none());
    assert!(actions.is_empty());
    let past = down_bytes(&Down::Chunk {
        offset: 170,
        len: 1,
        data: early,
    });
    let (up, actions) = feed(&mut receiver, &past, &public);
    assert!(up.is_none());
    assert!(actions.is_empty());
    feed(&mut receiver, &down_bytes(&chunk_at(&image, 0)), &public);
    let (up, actions) = feed(&mut receiver, &down_bytes(&Down::Commit), &public);
    assert_eq!(up, Some(Up::Result { code: Code::Ok }));
    assert_eq!(
        actions,
        vec![Action::Finish {
            version: OTA_VERSION
        }]
    );
}

#[test]
fn a_second_begin_restarts_and_the_old_bytes_are_not_in_the_hash() {
    let first = image_of(170);
    let second = image_of(1);
    let (first_digest, first_sig, public) = sign_image(&first);
    let (second_digest, second_sig, _) = sign_image(&second);
    let mut receiver = new_receiver();
    feed(
        &mut receiver,
        &begin_for(&first, first_digest, first_sig),
        &public,
    );
    feed(&mut receiver, &down_bytes(&chunk_at(&first, 0)), &public);
    let (up, actions) = feed(
        &mut receiver,
        &begin_for(&second, second_digest, second_sig),
        &public,
    );
    assert!(up.is_none());
    assert_eq!(
        actions,
        vec![Action::Abort, Action::Begin { size: 1 }],
        "the first write is dropped before the new one starts"
    );
    feed(&mut receiver, &down_bytes(&chunk_at(&second, 0)), &public);
    let (up, actions) = feed(&mut receiver, &down_bytes(&Down::Commit), &public);
    assert_eq!(up, Some(Up::Result { code: Code::Ok }));
    assert_eq!(
        actions,
        vec![Action::Finish {
            version: OTA_VERSION
        }]
    );
}

const CLEAN: u32 = ota::HEAP_FLOOR;

/// A good Begin for `image`, and the key that verifies it.
fn signed_begin(image: &[u8]) -> (Vec<u8>, [u8; 32]) {
    let (digest, signature, public) = sign_image(image);
    let begin = down_bytes(&Down::Begin {
        size: image.len() as u32,
        product: PRODUCT_NODE,
        version: OTA_VERSION,
        digest,
        signature,
    });
    (begin, public)
}

/// A healthy leaf at `now_ms`, with `acked` frames acknowledged.
fn leaf(now_ms: u32, acked: u32) -> ota::Sample {
    ota::Sample {
        now_ms,
        rooted: false,
        has_parent: true,
        acked,
        heap_free: CLEAN,
        heap_min: CLEAN,
    }
}

/// Feeds readings every ten seconds from `start` to `end` inclusive, and returns
/// the first time one earned the mark.
fn soak(
    health: &mut ota::Health,
    start: u32,
    end: u32,
    reading: impl Fn(u32) -> ota::Sample,
) -> Option<u32> {
    let mut now = start;
    while now <= end {
        if health.sample(&reading(now)) {
            return Some(now);
        }
        now += 10_000;
    }
    None
}

#[test]
fn a_leaf_is_healthy_only_after_five_minutes_with_a_parent_and_an_ack() {
    let mut health = ota::Health::new();
    let first = soak(&mut health, 0, 400_000, |now| leaf(now, now / 10_000));
    assert_eq!(first, Some(ota::CONFIRM_AFTER_MS));
}

#[test]
fn a_leaf_with_a_parent_but_no_acks_is_never_healthy() {
    let mut health = ota::Health::new();
    assert_eq!(soak(&mut health, 0, 900_000, |now| leaf(now, 7)), None);
}

#[test]
fn losing_the_parent_once_starts_the_five_minutes_again() {
    let mut health = ota::Health::new();
    assert_eq!(soak(&mut health, 0, 200_000, |now| leaf(now, now)), None);
    let mut gone = leaf(210_000, 210_000);
    gone.has_parent = false;
    assert!(!health.sample(&gone));
    let first = soak(&mut health, 220_000, 900_000, |now| leaf(now, now));
    assert_eq!(first, Some(220_000 + ota::CONFIRM_AFTER_MS));
}

#[test]
fn a_heap_below_the_floor_is_never_healthy() {
    let mut low_free = ota::Health::new();
    let free = soak(&mut low_free, 0, 900_000, |now| ota::Sample {
        heap_free: CLEAN - 1,
        ..leaf(now, now)
    });
    assert_eq!(free, None);
    let mut low_mark = ota::Health::new();
    let mark = soak(&mut low_mark, 0, 900_000, |now| ota::Sample {
        heap_min: CLEAN - 1,
        ..leaf(now, now)
    });
    assert_eq!(mark, None);
}

#[test]
fn the_root_needs_no_ack_to_be_healthy() {
    let mut health = ota::Health::new();
    let first = soak(&mut health, 0, 400_000, |now| ota::Sample {
        rooted: true,
        has_parent: false,
        ..leaf(now, 0)
    });
    assert_eq!(first, Some(ota::CONFIRM_AFTER_MS));
}

#[test]
fn a_board_with_no_route_is_never_healthy() {
    let mut health = ota::Health::new();
    let first = soak(&mut health, 0, 900_000, |now| ota::Sample {
        has_parent: false,
        ..leaf(now, now)
    });
    assert_eq!(first, None);
}

#[test]
fn health_holds_across_the_clock_wrapping() {
    let mut health = ota::Health::new();
    let start = u32::MAX - 100_000;
    assert!(!health.sample(&leaf(start, 1)));
    let later = start.wrapping_add(ota::CONFIRM_AFTER_MS);
    assert!(health.sample(&leaf(later, 2)));
}

#[test]
fn a_silent_transfer_is_dropped_and_its_commit_refused() {
    let image = image_of(500);
    let (begin, public) = signed_begin(&image);
    let mut receiver = new_receiver();
    let (_, actions) = feed(&mut receiver, &begin, &public);
    assert_eq!(actions, vec![Action::Begin { size: 500 }]);
    assert!(receiver.in_progress());

    assert!(!receiver.expire(ota::SILENCE_MS - 1, 0), "too soon");
    assert!(receiver.in_progress());
    assert!(receiver.take_action().is_none());

    assert!(receiver.expire(ota::SILENCE_MS, 0));
    assert!(!receiver.in_progress());
    assert_eq!(receiver.take_action(), Some(Action::Abort));
    assert_eq!(receiver.take_action(), None);
    assert!(!receiver.expire(u32::MAX, 0), "nothing left to drop");

    let (up, actions) = feed(&mut receiver, &down_bytes(&Down::Commit), &public);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::Refused
        })
    );
    assert!(actions.is_empty());
}

#[test]
fn silence_is_measured_across_the_clock_wrapping() {
    let image = image_of(500);
    let (begin, public) = signed_begin(&image);
    let mut receiver = new_receiver();
    feed(&mut receiver, &begin, &public);
    let last = u32::MAX - 10;
    assert!(!receiver.expire(last.wrapping_add(100), last));
    assert!(receiver.expire(last.wrapping_add(ota::SILENCE_MS), last));
}

/// Lays the writes into a blank slot. True if a Finish came after them.
fn lay(flash: &mut [u8], actions: &[Action]) -> bool {
    let mut finished = false;
    for action in actions {
        match action {
            Action::Write { offset, len, data } => {
                let at = *offset as usize;
                let len = usize::from(*len);
                flash[at..at + len].copy_from_slice(&data[..len]);
            }
            Action::Finish { .. } => finished = true,
            _ => {}
        }
    }
    finished
}

#[test]
fn a_push_before_the_actions_are_taken_drops_no_write() {
    let image = image_of(300);
    let (begin, public) = signed_begin(&image);
    let mut receiver = new_receiver();
    feed(&mut receiver, &begin, &public);
    let mut flash = vec![0u8; image.len()];
    let mut taken = Vec::new();

    // Nothing is taken between these, so the second chunk and the Commit must
    // not move the hash past what flash will get.
    for offset in [0, CHUNK] {
        let _ = receiver.push(&down_bytes(&chunk_at(&image, offset)), &public);
    }
    let up = receiver.push(&down_bytes(&Down::Commit), &public);
    while let Some(action) = receiver.take_action() {
        taken.push(action);
    }
    let finished = lay(&mut flash, &taken);
    assert!(
        !finished || flash == image,
        "a Finish over bytes that were never written: {up:?} {taken:?}"
    );
    assert_ne!(up, Some(Up::Result { code: Code::Ok }));

    // Taken in step, the rest lands and the image finishes.
    let (_, actions) = feed(
        &mut receiver,
        &down_bytes(&chunk_at(&image, CHUNK)),
        &public,
    );
    lay(&mut flash, &actions);
    let (up, actions) = feed(&mut receiver, &down_bytes(&Down::Commit), &public);
    assert_eq!(up, Some(Up::Result { code: Code::Ok }));
    assert!(lay(&mut flash, &actions));
    assert_eq!(flash, image);
}

#[test]
fn a_begin_before_the_finish_is_taken_keeps_the_finish() {
    let image = image_of(100);
    let (begin, public) = signed_begin(&image);
    let mut receiver = new_receiver();
    feed(&mut receiver, &begin, &public);
    feed(&mut receiver, &down_bytes(&chunk_at(&image, 0)), &public);
    let up = receiver.push(&down_bytes(&Down::Commit), &public);
    assert_eq!(up, Some(Up::Result { code: Code::Ok }));

    // The host was told Ok, so the Finish must still reach flash.
    assert_eq!(receiver.push(&begin, &public), None);
    assert_eq!(
        receiver.take_action(),
        Some(Action::Finish {
            version: OTA_VERSION
        })
    );
    assert_eq!(receiver.take_action(), None);
    assert!(!receiver.in_progress());
}

#[test]
fn abandoning_with_no_transfer_queues_nothing() {
    let mut receiver = new_receiver();
    receiver.abandon();
    assert_eq!(receiver.take_action(), None);
}

#[test]
fn a_raised_floor_refuses_the_next_lower_image_and_never_goes_down() {
    let image = image_of(500);
    let (begin, public) = signed_begin(&image);
    let mut receiver: Receiver<SLOT, BITMAP> = Receiver::new(PRODUCT_NODE, 1);
    receiver.raise_floor(OTA_VERSION + 1);
    receiver.raise_floor(2);
    let (up, actions) = feed(&mut receiver, &begin, &public);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::Downgrade
        })
    );
    assert!(actions.is_empty());
}

#[test]
fn a_floor_raised_during_a_transfer_refuses_its_commit() {
    let image = image_of(170);
    let (begin, public) = signed_begin(&image);
    let mut receiver: Receiver<SLOT, BITMAP> = Receiver::new(PRODUCT_NODE, OTA_VERSION);
    let (up, _) = feed(&mut receiver, &begin, &public);
    assert_eq!(up, None, "a version equal to the floor is a retry");
    feed(&mut receiver, &down_bytes(&chunk_at(&image, 0)), &public);
    receiver.raise_floor(OTA_VERSION + 1);
    let (up, actions) = feed(&mut receiver, &down_bytes(&Down::Commit), &public);
    assert_eq!(
        up,
        Some(Up::Result {
            code: Code::Downgrade
        })
    );
    assert_eq!(actions, vec![Action::Abort], "no Finish below the floor");
    assert!(!receiver.in_progress());
}
