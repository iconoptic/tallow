//! Parent changes and who a beacon may come from, one node at a time.

use tallow_mesh_core::mesh::{
    Reason, BEACON_MS, NEIGHBOUR_TIMEOUT_MS, PARENT_FAILURES, ROOT_TIMEOUT_MS, TX_RETRIES,
};
use tallow_mesh_core::wire::{Frame, FrameKind, Sealer};
use tallow_mesh_core::{Event, Mesh, MAX_FRAME, NO_RANK};

const KEY: [u8; 32] = [7; 32];

fn mac(index: u8) -> [u8; 6] {
    [0x70, 0x4B, 0xCA, 0x10, 0x20, index]
}

fn board(index: u8) -> Mesh {
    Mesh::new(mac(index), &KEY, 1000 + index as u32)
}

fn quiet(_: Event) {}

/// A parent change as owned values, for comparing after the callback returns.
fn owned(event: Event) -> (Option<u32>, Option<u32>, Reason) {
    match event {
        Event::Parent { old, new, reason } => (old, new, reason),
        other => panic!("not a parent change: {other:?}"),
    }
}

/// Runs `sender` long enough to beacon and hands the frame to `node`.
fn beacon_into(sender: &mut Mesh, node: &mut Mesh, now: u32, parents: &mut Vec<Option<u32>>) {
    sender.tick(now.wrapping_add(BEACON_MS), &mut quiet);
    let Some(tx) = sender.next_tx() else { return };
    let bytes = tx.bytes().to_vec();
    sender.tx_done(now, true, &mut quiet);
    node.receive(now, sender.mac, -50, &bytes, &mut |event| {
        if let Event::Parent { new, .. } = event {
            parents.push(new);
        }
    });
}

fn root(index: u8, now: u32) -> Mesh {
    let mut mesh = board(index);
    mesh.host_heartbeat(now);
    mesh
}

#[test]
fn a_neighbour_that_is_not_the_parent_going_quiet_changes_nothing() {
    let mut node = board(1);
    let mut host = root(2, 0);
    let mut leaf = board(3);
    let mut parents = Vec::new();
    beacon_into(&mut host, &mut node, 0, &mut parents);
    beacon_into(&mut leaf, &mut node, 0, &mut parents);
    assert_eq!(parents, [Some(host.id)]);

    let later = NEIGHBOUR_TIMEOUT_MS + 1;
    host.host_heartbeat(later);
    beacon_into(&mut host, &mut node, later, &mut parents);
    node.tick(later, &mut |event| {
        if let Event::Parent { new, .. } = event {
            parents.push(new);
        }
    });
    assert_eq!(parents, [Some(host.id)]);
    assert_eq!(node.parent_changes, 1);
    assert_eq!(node.parent().map(|parent| parent.id), Some(host.id));
}

#[test]
fn a_parent_that_goes_quiet_is_reported_once() {
    let mut node = board(1);
    let mut host = root(2, 0);
    let mut parents = Vec::new();
    beacon_into(&mut host, &mut node, 0, &mut parents);
    node.tick(NEIGHBOUR_TIMEOUT_MS + 1, &mut |event| {
        if let Event::Parent { new, .. } = event {
            parents.push(new);
        }
    });
    assert_eq!(parents, [Some(host.id), None]);
    assert_eq!(node.parent_changes, 2);
    assert_eq!(node.rank, NO_RANK);
}

#[test]
fn a_lost_parent_is_reported_with_its_old_id_and_the_timeout_reason() {
    let mut node = board(1);
    let mut host = root(2, 0);
    let mut parents = Vec::new();
    beacon_into(&mut host, &mut node, 0, &mut parents);
    let mut changes = Vec::new();
    node.tick(NEIGHBOUR_TIMEOUT_MS + 1, &mut |event| {
        changes.push(owned(event))
    });
    assert_eq!(changes, [(Some(host.id), None, Reason::BeaconTimeout)]);
}

#[test]
fn a_board_that_stops_being_the_root_joins_with_no_timeout_reported() {
    let mut node = root(1, 0);
    let mut other = root(2, 0);
    let mut parents = Vec::new();
    beacon_into(&mut other, &mut node, 0, &mut parents);
    assert!(parents.is_empty(), "a root takes no parent");
    let mut changes = Vec::new();
    node.tick(ROOT_TIMEOUT_MS + 1, &mut |event| changes.push(owned(event)));
    assert_eq!(changes, [(None, Some(other.id), Reason::BetterPath)]);
}

#[test]
fn a_first_parent_is_reported_as_a_better_path_from_none() {
    let mut node = board(1);
    let mut host = root(2, 0);
    host.tick(BEACON_MS, &mut quiet);
    let bytes = host.next_tx().expect("a beacon").bytes().to_vec();
    let mut changes = Vec::new();
    node.receive(0, host.mac, -50, &bytes, &mut |event| {
        changes.push(owned(event))
    });
    assert_eq!(changes, [(None, Some(host.id), Reason::BetterPath)]);
}

#[test]
fn a_parent_that_stops_acknowledging_is_reported_gone() {
    let mut node = board(1);
    let mut host = root(2, 0);
    let mut parents = Vec::new();
    beacon_into(&mut host, &mut node, 0, &mut parents);

    let mut changes = Vec::new();
    for _ in 0..PARENT_FAILURES {
        assert!(node.send_up(FrameKind::Report, &[0; 10], 0, &mut quiet));
        for _ in 0..=TX_RETRIES {
            node.tx_done(0, false, &mut |event| {
                if let Event::Parent { new, .. } = event {
                    parents.push(new);
                }
                changes.push(owned(event));
            });
        }
    }
    assert_eq!(parents, [Some(host.id), None]);
    assert_eq!(node.parent_changes, 2);
    assert_eq!(node.rank, NO_RANK);
    assert_eq!(
        changes.last(),
        Some(&(Some(host.id), None, Reason::FailedAcks))
    );
}

#[test]
fn losing_a_parent_with_a_spare_is_one_change() {
    let mut node = board(1);
    let mut first = root(2, 0);
    let mut spare = root(3, 0);
    let mut parents = Vec::new();
    beacon_into(&mut first, &mut node, 0, &mut parents);
    beacon_into(&mut spare, &mut node, 0, &mut parents);

    let later = NEIGHBOUR_TIMEOUT_MS + 1;
    spare.host_heartbeat(later);
    beacon_into(&mut spare, &mut node, later, &mut parents);
    assert_eq!(parents.first(), Some(&Some(first.id)));
    node.tick(later, &mut |event| {
        if let Event::Parent { new, .. } = event {
            parents.push(new);
        }
    });
    assert_eq!(parents, [Some(first.id), Some(spare.id)]);
    assert_eq!(node.parent_changes, 2);
}

#[test]
fn a_beacon_from_another_address_is_dropped() {
    let mut node = board(1);
    let mut host = root(2, 0);
    host.tick(BEACON_MS, &mut quiet);
    let bytes = host.next_tx().expect("a beacon").bytes().to_vec();

    node.receive(0, mac(9), -50, &bytes, &mut quiet);
    assert_eq!(node.neighbours().count(), 0);
    assert_eq!(node.rx_bad, 1);

    node.receive(0, host.mac, -50, &bytes, &mut quiet);
    assert_eq!(
        node.neighbours().next().map(|known| known.mac),
        Some(host.mac)
    );
}

#[test]
fn a_recorded_beacon_cannot_repoint_a_parent() {
    let mut node = board(1);
    // Counter 2, so an earlier boot can have a lower counter.
    let mut host = Mesh::new(mac(2), &KEY, (2 << 12) | 5);
    host.host_heartbeat(0);
    let mut parents = Vec::new();
    beacon_into(&mut host, &mut node, 0, &mut parents);

    // An earlier boot's beacon, as a radio with a recording would send it. Counter 1,
    // so it is refused as an older boot, as well as for its source address.
    let sealer = Sealer::new(&KEY);
    let head = Frame {
        kind: FrameKind::Beacon,
        ttl: 1,
        hops: 0,
        origin: host.id,
        boot: (1 << 12) | 9,
        seq: 1,
    };
    let mut out = [0u8; MAX_FRAME];
    let length = sealer.seal(&head, &[0; 11], &mut out).expect("fits");
    node.receive(1, mac(9), -50, &out[..length], &mut quiet);
    assert_eq!(node.parent().map(|parent| parent.mac), Some(host.mac));

    // The same older boot from the host's own address, with a rank the host never sent.
    let mut payload = [0u8; 11];
    payload[0] = 7;
    let length = sealer.seal(&head, &payload, &mut out).expect("fits");
    node.receive(2, host.mac, -50, &out[..length], &mut quiet);
    let rank = node
        .neighbours()
        .find(|known| known.id == host.id)
        .map(|known| known.rank);
    assert_eq!(rank, Some(0), "an older boot changed the host's entry");
}

#[test]
fn a_report_arrives_from_any_forwarder() {
    // Only beacons name their sender. A forwarded report's source is the last hop.
    let mut host = root(2, 0);
    let sealer = Sealer::new(&KEY);
    let head = Frame {
        kind: FrameKind::Report,
        ttl: 16,
        hops: 1,
        origin: 0x0102_0304,
        boot: 5,
        seq: 1,
    };
    let mut out = [0u8; MAX_FRAME];
    let length = sealer.seal(&head, &[0; 10], &mut out).expect("fits");
    let mut arrived = 0;
    host.receive(1, mac(9), -50, &out[..length], &mut |event| {
        if matches!(event, Event::Up { .. }) {
            arrived += 1;
        }
    });
    assert_eq!(arrived, 1);
}
