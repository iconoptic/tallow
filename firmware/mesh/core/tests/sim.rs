//! The mesh on a simulated radio: virtual time, lossy links, boards that die.

use tallow_mesh_core::mesh::{BEACON_MS, MAX_RANK, SEEN};
use tallow_mesh_core::wire::Command;
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
    airtime: u64,
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
            airtime: 0,
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
            let (arrivals, commands, parents) = (
                &mut self.arrivals,
                &mut self.commands[index],
                &mut self.parents[index],
            );
            let mut sink = sink(arrivals, commands, parents);
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
                let (arrivals, commands, parents) = (
                    &mut self.arrivals,
                    &mut self.commands[other],
                    &mut self.parents[other],
                );
                let mut sink = sink(arrivals, commands, parents);
                self.nodes[other].receive(now, mac(index), -60, tx.bytes(), &mut sink);
            }
            let (arrivals, commands, parents) = (
                &mut self.arrivals,
                &mut self.commands[index],
                &mut self.parents[index],
            );
            let mut sink = sink(arrivals, commands, parents);
            self.nodes[index].tx_done(acked || tx.dest == Dest::Broadcast, &mut sink);
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
        let (arrivals, commands, parents) = (
            &mut self.arrivals,
            &mut self.commands[index],
            &mut self.parents[index],
        );
        let mut sink = sink(arrivals, commands, parents);
        self.nodes[index].send_up(FrameKind::Probe, &value.to_le_bytes(), now, &mut sink)
    }

    fn arrived(&self, index: usize) -> Vec<u32> {
        let id = self.id(index);
        self.arrivals
            .iter()
            .filter(|entry| entry.origin == id)
            .map(|entry| entry.seq_value)
            .collect()
    }
}

fn sink<'a>(
    arrivals: &'a mut Vec<Arrival>,
    commands: &'a mut Vec<u16>,
    parents: &'a mut Vec<Option<u32>>,
) -> impl FnMut(Event) + 'a {
    move |event| match event {
        Event::Up { frame, payload, .. } => {
            let seq_value = u32::from_le_bytes(payload[..4].try_into().unwrap());
            arrivals.push(Arrival {
                origin: frame.origin,
                hops: frame.hops,
                seq_value,
            });
        }
        Event::Command(command) => commands.push(command.id),
        Event::Parent(parent) => parents.push(parent),
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
        .filter(|entry| entry.origin == sim.id(4))
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
    let (arrivals, commands, parents) =
        (&mut sim.arrivals, &mut sim.commands[0], &mut sim.parents[0]);
    let mut root_sink = sink(arrivals, commands, parents);
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
    leaf.tx_done(true, &mut quiet);
    for _ in 0..SEEN * 2 {
        assert!(leaf.send_up(FrameKind::Probe, &[0; 4], 6, &mut quiet));
        let tx = *leaf.next_tx().unwrap();
        root.receive(6, mac(1), -50, tx.bytes(), &mut quiet);
        leaf.tx_done(true, &mut quiet);
    }
    let mut late = 0;
    root.receive(7, mac(1), -50, &data[..length], &mut |_| late += 1);
    assert_eq!(late, 0);
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
            data[0] = 0xA1;
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
fn a_node_fits_in_eight_kilobytes() {
    let size = core::mem::size_of::<Mesh>() + core::mem::size_of::<Sightings<128>>();
    println!(
        "Mesh {} + Sightings<128> {} = {size} bytes",
        core::mem::size_of::<Mesh>(),
        core::mem::size_of::<Sightings<128>>()
    );
    assert!(size <= 8192, "{size} bytes");
}
