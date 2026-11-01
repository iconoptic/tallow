//! One sealed frame of each kind, with a fixed key, origin, boot, and sequence.
//!
//! The nonce is those three words, so the bytes do not change between runs.
//! These tests fail if `seal` moves any byte. The tests further down build their
//! payloads from the code that sends them, and check that `open` refuses a frame
//! with a good tag and a header it should not accept.

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use tallow_mesh_core::gatt::{parse, Reply, Request, Status, NOTIFY_MAX, VALUE_MAX};
use tallow_mesh_core::mesh::{is_down, BEACON_MS};
use tallow_mesh_core::sightings::RECORD;
use tallow_mesh_core::wire::{
    Command, Frame, FrameKind, Sealer, Watch, HEADER, MAX_FRAME, MAX_PAYLOAD, TAG, VERSION,
    WATCH_MAX,
};
use tallow_mesh_core::{Event, Kind, Mesh, Names, Sightings, MAX_FRAME as FRAME_LIMIT};

const KEY: [u8; 32] = [7; 32];
const ORIGIN: u32 = 0x0102_0304;
const BOOT: u32 = 0x0A0B_0C0D;
const SEQ: u32 = 0x1112_1314;

fn seal(kind: FrameKind, payload: &[u8]) -> Vec<u8> {
    let sealer = Sealer::new(&KEY);
    let head = Frame {
        kind,
        ttl: 16,
        hops: 0,
        origin: ORIGIN,
        boot: BOOT,
        seq: SEQ,
    };
    let mut out = [0u8; MAX_FRAME];
    let length = sealer.seal(&head, payload, &mut out).expect("payload fits");
    out[..length].to_vec()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn assert_golden(kind: FrameKind, payload: &[u8], golden: &str) {
    let first = seal(kind, payload);
    let second = seal(kind, payload);
    assert_eq!(first, second, "the same inputs sealed to different bytes");
    assert_eq!(hex(&first), golden);
    let mut plain = [0u8; MAX_PAYLOAD];
    let (frame, length) = Sealer::new(&KEY)
        .open(&first, &mut plain)
        .expect("golden frame opens");
    assert_eq!(frame.kind, kind);
    assert_eq!(frame.ttl, 16);
    assert_eq!(frame.hops, 0);
    assert_eq!(frame.origin, ORIGIN);
    assert_eq!(frame.boot, BOOT);
    assert_eq!(frame.seq, SEQ);
    assert_eq!(&plain[..length], payload);
}

fn command_with(set: impl FnOnce(&mut Command)) -> [u8; Command::WIRE] {
    let mut command = Command {
        id: 0,
        ble_window_ms: 0,
        ble_interval_ms: 0,
        inquiry_period_s: 0,
        inquiry_length: 0,
        probe_hz: 0,
        probe_size: 0,
    };
    set(&mut command);
    command.encode()
}

#[test]
fn a_sealed_beacon_is_these_bytes() {
    // Rank 1, parent 0x11223344, beacon 0x5566, four unread zeros.
    let payload = [1, 0x44, 0x33, 0x22, 0x11, 0x66, 0x55, 0, 0, 0, 0];
    assert_golden(
        FrameKind::Beacon,
        &payload,
        "a6011000040302010d0c0b0a141312113552a8ec7193390a6490d475728525c311e60edf147dc879276fd4",
    );
}

#[test]
fn a_sealed_report_is_these_bytes() {
    // One sighting: address, BLE public, RSSI -60, count 3, age 2 seconds.
    let payload = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0, 0xc4, 3, 2];
    assert_golden(
        FrameKind::Report,
        &payload,
        "a6021000040302010d0c0b0a141312119ead57138e0a6cce679236bb89f8bada65b4e37f47ff43e4f1f1",
    );
}

#[test]
fn a_sealed_name_is_these_bytes() {
    // Address, Classic, company 0x004c, class 1.2.3, name "Flip 5".
    let payload = [
        0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 2, 0x4c, 0x00, 1, 2, 3, 6, b'F', b'l', b'i', b'p',
        b' ', b'5',
    ];
    assert_golden(
        FrameKind::Name,
        &payload,
        "a6031000040302010d0c0b0a141312119ead57138e0a6e466491d673a050b2f7672fdf132d589ecd2e259112fd69dfa99e19e2",
    );
}

#[test]
fn a_sealed_stats_frame_is_these_bytes() {
    // The 21 counters are 1 through 21. Then rank 2, 3 neighbours, RSSI -40, quality 80.
    let mut payload = [0u8; 88];
    for index in 0..21 {
        let value = (index as u32).wrapping_add(1);
        payload[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    payload[84] = 2;
    payload[85] = 3;
    payload[86] = (-40i8) as u8;
    payload[87] = 80;
    assert_golden(
        FrameKind::Stats,
        &payload,
        "a6041000040302010d0c0b0a1413121135169bce62f56c0a6790d470a216de9e120feaa0701a72be6f2b769df6f1c447d945377a79b0e752af465d7f1dab8367ee011fe4177e2ca55e699a0ba51caa3c0b7b1b957db1f1a1870147bf4895a53e2fbdc0ab857352c319f715b26b4a67b2db27696edaeb535e",
    );
}

#[test]
fn a_sealed_probe_is_these_bytes() {
    // Probe sequence 0x12345678, then four zeros of padding.
    let payload = [0x78, 0x56, 0x34, 0x12, 0, 0, 0, 0];
    assert_golden(
        FrameKind::Probe,
        &payload,
        "a6051000040302010d0c0b0a141312114c40afdc60f56c0af3075305dc55420794556a77177328bc",
    );
}

#[test]
fn a_sealed_route_is_these_bytes() {
    // Uptime 1234 seconds, old parent 0x11223344, new parent 0x55667788, reason 1.
    let mut payload = [0u8; 13];
    payload[0..4].copy_from_slice(&1234u32.to_le_bytes());
    payload[4..8].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    payload[8..12].copy_from_slice(&0x5566_7788u32.to_le_bytes());
    payload[12] = 1;
    assert_golden(
        FrameKind::Route,
        &payload,
        "a6071000040302010d0c0b0a14131211e6129bce24c64e1bece7b225a77ea90ef416171b09bc8d189f9f73f614",
    );
}

#[test]
fn a_sealed_watch_is_these_bytes() {
    // List id 0x1234, two addresses: aa:bb:cc:dd:ee:ff and 11:22:33:44:55:66.
    let mut addresses = [[0u8; 6]; WATCH_MAX];
    addresses[0] = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    addresses[1] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
    let mut wire = [0u8; Watch::MAX_WIRE];
    let length = Watch {
        id: 0x1234,
        count: 2,
        addresses,
    }
    .encode(&mut wire);
    assert_golden(FrameKind::Watch, &wire[..length], "a6081000040302010d0c0b0a1413121100049964db39b1e49b81f643e243b8209d760dfbc81549650284514a0ab39c");
}

#[test]
fn a_real_watch_matches_the_documented_layout() {
    // Boot counter 5, so the list id is 5 << 4 for the first list of this boot.
    let mut root = Mesh::new([1, 2, 3, 4, 5, 6], &KEY, (5 << 12) | 0x111);
    let addresses = [[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff], [1, 2, 3, 4, 5, 6]];
    let mut applied = None;
    let sent = root.send_watch(&addresses, &mut |event| {
        if let Event::Watch { count, addresses } = event {
            applied = Some((count, addresses));
        }
    });
    assert!(sent);
    let mut expected = [[0u8; 6]; WATCH_MAX];
    expected[0] = addresses[0];
    expected[1] = addresses[1];
    assert_eq!(
        applied,
        Some((2, expected)),
        "the root applies its own list"
    );
    let bytes = root.next_tx().expect("a watch").bytes().to_vec();
    let mut plain = [0u8; MAX_PAYLOAD];
    let (frame, length) = Sealer::new(&KEY).open(&bytes, &mut plain).expect("opens");
    assert_eq!(frame.kind, FrameKind::Watch);
    assert_eq!(frame.ttl, 16);
    assert_eq!(length, 3 + 6 * 2);
    assert_eq!(plain[0..2], (5u16 << 4).to_le_bytes(), "list id");
    assert_eq!(plain[2], 2, "count");
    assert_eq!(plain[3..9], addresses[0], "first address");
    assert_eq!(plain[9..15], addresses[1], "second address");
}

#[test]
fn every_watch_field_decodes_from_its_own_bytes() {
    let wire = [0xEF, 0xBE, 1, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    let watch = Watch::decode(&wire).expect("one address");
    assert_eq!(watch.id, 0xBEEF);
    assert_eq!(watch.count, 1);
    assert_eq!(watch.addresses[0], [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
    assert_eq!(watch.addresses[1], [0; 6], "unused slots are zero");
}

#[test]
fn a_watch_with_no_addresses_is_a_clear() {
    let watch = Watch::decode(&[7, 0, 0]).expect("a clear");
    assert_eq!((watch.id, watch.count), (7, 0));
}

#[test]
fn a_watch_of_the_wrong_length_or_count_is_refused_not_cut_short() {
    assert_eq!(Watch::decode(&[]), None);
    assert_eq!(Watch::decode(&[1, 0]), None, "no count byte");
    assert_eq!(
        Watch::decode(&[1, 0, 2, 0, 0, 0, 0, 0, 0]),
        None,
        "one address short"
    );
    assert_eq!(
        Watch::decode(&[1, 0, 1, 0, 0, 0, 0, 0, 0, 0]),
        None,
        "one byte over"
    );
    let mut seventeen = vec![1, 0, 17];
    seventeen.extend_from_slice(&[0; 17 * 6]);
    assert_eq!(Watch::decode(&seventeen), None, "more than sixteen");
}

#[test]
fn a_sealed_command_is_these_bytes() {
    let payload = Command {
        id: 7,
        ble_window_ms: 30,
        ble_interval_ms: 100,
        inquiry_period_s: 60,
        inquiry_length: 4,
        probe_hz: 2,
        probe_size: 32,
    }
    .encode();
    assert_golden(
        FrameKind::Command,
        &payload,
        "a6061000040302010d0c0b0a14131211331685ce04f5500a6092f497fecc687ffc3a7570917f6448b6cb17",
    );
}

#[test]
fn command_id_occupies_the_first_two_bytes() {
    assert_eq!(
        command_with(|command| command.id = 0xBEEF),
        [0xEF, 0xBE, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    );
}

#[test]
fn command_ble_window_occupies_bytes_two_and_three() {
    assert_eq!(
        command_with(|command| command.ble_window_ms = 0x1234),
        [0, 0, 0x34, 0x12, 0, 0, 0, 0, 0, 0, 0]
    );
}

#[test]
fn command_ble_interval_occupies_bytes_four_and_five() {
    assert_eq!(
        command_with(|command| command.ble_interval_ms = 0x0100),
        [0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0]
    );
}

#[test]
fn command_inquiry_period_occupies_bytes_six_and_seven() {
    assert_eq!(
        command_with(|command| command.inquiry_period_s = 60),
        [0, 0, 0, 0, 0, 0, 60, 0, 0, 0, 0]
    );
}

#[test]
fn command_inquiry_length_occupies_byte_eight() {
    assert_eq!(
        command_with(|command| command.inquiry_length = 4),
        [0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0]
    );
}

#[test]
fn command_probe_rate_occupies_byte_nine() {
    assert_eq!(
        command_with(|command| command.probe_hz = 2),
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0]
    );
}

#[test]
fn command_probe_size_occupies_byte_ten() {
    assert_eq!(
        command_with(|command| command.probe_size = 32),
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 32]
    );
}

#[test]
fn every_command_field_decodes_from_its_own_bytes() {
    let wire = [0xEF, 0xBE, 0x34, 0x12, 0x78, 0x56, 0xBC, 0x9A, 4, 2, 32];
    assert_eq!(
        Command::decode(&wire),
        Some(Command {
            id: 0xBEEF,
            ble_window_ms: 0x1234,
            ble_interval_ms: 0x5678,
            inquiry_period_s: 0x9ABC,
            inquiry_length: 4,
            probe_hz: 2,
            probe_size: 32,
        })
    );
}

#[test]
fn a_command_of_any_other_length_is_refused() {
    let wire = [0u8; Command::WIRE + 1];
    assert_eq!(Command::decode(&wire[..Command::WIRE - 1]), None);
    assert_eq!(Command::decode(&wire), None);
}

/// A frame with a valid tag over exactly these first two bytes.
fn sealed_with_header(version: u8, kind: u8) -> Vec<u8> {
    let mut frame = vec![version, kind, 16, 0];
    frame.extend_from_slice(&ORIGIN.to_le_bytes());
    frame.extend_from_slice(&BOOT.to_le_bytes());
    frame.extend_from_slice(&SEQ.to_le_bytes());
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&[&frame[4..8], &frame[8..12], &frame[12..16]].concat());
    let extra = [&frame[..2], &frame[4..16]].concat();
    let mut body = [1u8, 2, 3, 4];
    let tag = ChaCha20Poly1305::new(Key::from_slice(&KEY))
        .encrypt_in_place_detached(Nonce::from_slice(&nonce), &extra, &mut body)
        .expect("seals");
    frame.extend_from_slice(&body);
    frame.extend_from_slice(&tag);
    frame
}

fn opens(frame: &[u8]) -> bool {
    let mut plain = [0u8; MAX_PAYLOAD];
    Sealer::new(&KEY).open(frame, &mut plain).is_some()
}

#[test]
fn the_helper_builds_a_frame_that_opens() {
    assert!(opens(&sealed_with_header(VERSION, FrameKind::Probe as u8)));
}

#[test]
fn another_frame_version_is_refused_even_with_a_good_tag() {
    let other = VERSION.wrapping_add(1);
    assert!(!opens(&sealed_with_header(other, FrameKind::Probe as u8)));
}

#[test]
fn an_unknown_frame_kind_is_refused_even_with_a_good_tag() {
    assert!(!opens(&sealed_with_header(VERSION, 200)));
}

#[test]
fn a_tampered_or_wrongly_keyed_frame_is_refused() {
    let good = seal(FrameKind::Probe, &[1, 2, 3, 4]);
    let mut plain = [0u8; MAX_PAYLOAD];
    assert!(Sealer::new(&[8; 32]).open(&good, &mut plain).is_none());
    for at in [0, 1, 4, 8, 12, 16, good.len() - 1] {
        let mut bad = good.clone();
        bad[at] ^= 1;
        assert!(!opens(&bad), "byte {at} is not covered");
    }
}

#[test]
fn ttl_and_hops_may_change_without_breaking_the_seal() {
    let mut forwarded = seal(FrameKind::Report, &[0; 10]);
    forwarded[2] -= 1;
    forwarded[3] += 1;
    let mut plain = [0u8; MAX_PAYLOAD];
    let (frame, _) = Sealer::new(&KEY)
        .open(&forwarded, &mut plain)
        .expect("opens");
    assert_eq!((frame.ttl, frame.hops), (15, 1));
}

#[test]
fn the_largest_payload_fits_one_esp_now_frame() {
    assert_eq!(FRAME_LIMIT, 250);
    assert_eq!(MAX_PAYLOAD, 218);
    let frame = seal(FrameKind::Probe, &[9; MAX_PAYLOAD]);
    assert_eq!(frame.len(), 250);
    assert!(opens(&frame));
}

#[test]
fn a_payload_past_the_limit_is_not_sealed() {
    let head = Frame {
        kind: FrameKind::Probe,
        ttl: 16,
        hops: 0,
        origin: ORIGIN,
        boot: BOOT,
        seq: SEQ,
    };
    let mut out = [0u8; MAX_FRAME];
    let sealer = Sealer::new(&KEY);
    assert_eq!(sealer.seal(&head, &[9; MAX_PAYLOAD + 1], &mut out), None);
    assert_eq!(HEADER + MAX_PAYLOAD + TAG, MAX_FRAME);
}

#[test]
fn a_real_beacon_matches_the_documented_layout() {
    let mut root = Mesh::new([1, 2, 3, 4, 5, 6], &KEY, 1);
    root.host_heartbeat(0);
    let mut node = Mesh::new([1, 2, 0x10, 0x20, 0x30, 0x40], &KEY, 2);
    root.tick(BEACON_MS, &mut |_| {});
    let sent = root.next_tx().expect("a beacon").bytes().to_vec();
    node.receive(0, root.mac, -50, &sent, &mut |_| {});
    node.tick(BEACON_MS, &mut |_: Event| {});

    // Joining queued a Route frame ahead of the beacon. Send it, then read the beacon.
    let mut plain = [0u8; MAX_PAYLOAD];
    let route = node.next_tx().expect("a route").bytes().to_vec();
    let (route_frame, _) = Sealer::new(&KEY).open(&route, &mut plain).expect("opens");
    assert_eq!(route_frame.kind, FrameKind::Route);
    node.tx_done(BEACON_MS, true, &mut |_: Event| {});
    let bytes = node.next_tx().expect("a beacon").bytes().to_vec();
    let (frame, length) = Sealer::new(&KEY).open(&bytes, &mut plain).expect("opens");
    assert_eq!(frame.kind, FrameKind::Beacon);
    assert_eq!(frame.ttl, 1);
    assert_eq!(length, 11);
    assert_eq!(plain[0], 1, "rank");
    assert_eq!(plain[1..5], root.id.to_le_bytes(), "parent");
    assert_eq!(plain[5..7], 1u16.to_le_bytes(), "first beacon number");
    assert_eq!(plain[7..11], [0; 4], "sent as zero");
}

#[test]
fn a_real_route_matches_the_documented_layout() {
    let mut root = Mesh::new([1, 2, 3, 4, 5, 6], &KEY, 1);
    root.host_heartbeat(0);
    let mut node = Mesh::new([1, 2, 0x10, 0x20, 0x30, 0x40], &KEY, 2);
    root.tick(BEACON_MS, &mut |_| {});
    let sent = root.next_tx().expect("a beacon").bytes().to_vec();
    // Joining at 7.25 s since boot. The join is the parent change to report.
    node.receive(7_250, root.mac, -50, &sent, &mut |_| {});

    let mut plain = [0u8; MAX_PAYLOAD];
    let bytes = node.next_tx().expect("a route").bytes().to_vec();
    let (frame, length) = Sealer::new(&KEY).open(&bytes, &mut plain).expect("opens");
    assert_eq!(frame.kind, FrameKind::Route);
    assert_eq!(frame.origin, node.id);
    assert_eq!(length, 13);
    assert_eq!(plain[0..4], 7u32.to_le_bytes(), "uptime seconds");
    assert_eq!(plain[4..8], [0; 4], "no old parent");
    assert_eq!(plain[8..12], root.id.to_le_bytes(), "new parent");
    assert_eq!(plain[12], 2, "reason: better path");
}

#[test]
fn a_real_report_matches_the_documented_layout() {
    let address = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    let mut table = Sightings::<4>::new();
    table.observe(1000, address, Kind::Classic, -60);
    table.observe(1500, address, Kind::Classic, -70);
    let mut out = [0u8; RECORD];
    assert_eq!(table.drain(3500, &mut out), RECORD);
    assert_eq!(out, [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 2, 0xc4, 2, 2]);

    table.observe(0, address, Kind::BleRandom, -60);
    table.drain(255_000 + 5_000, &mut out);
    assert_eq!(out[6], 1);
    assert_eq!(out[9], 255, "age saturates");
}

#[test]
fn a_real_name_matches_the_documented_layout() {
    let address = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    let mut names = Names::<8, 2>::new();
    names.offer(
        0,
        address,
        Kind::Classic,
        Some(0x004c),
        Some([1, 2, 3]),
        Some(b"Flip 5"),
    );
    let mut out = [0u8; 64];
    let length = names.drain(&mut out);
    assert_eq!(
        out[..length],
        [
            0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 2, 0x4c, 0x00, 1, 2, 3, 6, b'F', b'l', b'i', b'p',
            b' ', b'5',
        ]
    );
}

#[test]
fn a_name_with_no_maker_marks_the_company_unknown() {
    let mut names = Names::<8, 2>::new();
    names.offer(0, [1; 6], Kind::BlePublic, None, None, Some(b"x"));
    let mut out = [0u8; 64];
    names.drain(&mut out);
    assert_eq!(out[6], 0);
    assert_eq!(out[7..9], [0xff, 0xff]);
    assert_eq!(out[9..12], [0; 3]);
}

#[test]
fn a_sealed_gatt_connect_is_these_bytes() {
    // Request 0x0102, Connect (0), aa:bb:cc:dd:ee:ff, random address (1).
    let payload = [0x02, 0x01, 0x00, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x01];
    assert_golden(
        FrameKind::Gatt,
        &payload,
        "a60a1000040302010d0c0b0a1413121136179b64db39b1e49b91c121b7e637702fd63c28da06c6c13d3e",
    );
}

#[test]
fn a_sealed_gatt_write_is_these_bytes() {
    // Request 3, Write (1), handle 0x0021, value 01 02 03.
    let payload = [0x03, 0x00, 0x01, 0x21, 0x00, 0x01, 0x02, 0x03];
    assert_golden(
        FrameKind::Gatt,
        &payload,
        "a60a1000040302010d0c0b0a1413121137169aef60f46e095afd40ca78c0e8859034c8f3f10199ba",
    );
}

#[test]
fn a_sealed_gatt_answer_is_these_bytes() {
    // Answer to request 3, kind 1, status Ok, value de ad.
    let payload = [0x03, 0x00, 0x01, 0x00, 0xde, 0xad];
    assert_golden(
        FrameKind::GattUp,
        &payload,
        "a60c1000040302010d0c0b0a1413121137169acebe5842899ec54ed58fc7743588ceb60adad4",
    );
}

#[test]
fn a_sealed_gatt_notification_is_these_bytes() {
    // Link 2, kind 2, handle 0x0021, value ff 00 80, which is not UTF-8.
    let payload = [0x02, 0x00, 0x02, 0x21, 0x00, 0xff, 0x00, 0x80];
    assert_golden(
        FrameKind::GattUp,
        &payload,
        "a60c1000040302010d0c0b0a14131211361699ef600a6c8a929474a2d916757302becf069d1b32eb",
    );
}

#[test]
fn the_gatt_kinds_go_down_and_up_and_decode_from_their_bytes() {
    assert_eq!(FrameKind::from_u8(10), Some(FrameKind::Gatt));
    assert_eq!(FrameKind::from_u8(12), Some(FrameKind::GattUp));
    assert_eq!(FrameKind::from_u8(9), None, "nine is still unused");
    assert!(is_down(FrameKind::Gatt));
    assert!(!is_down(FrameKind::GattUp), "answers travel up, not down");
}

#[test]
fn a_real_connect_request_parses_to_the_documented_fields() {
    let bytes = [0x34, 0x12, 0x00, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00];
    let (id, request) = parse(&bytes).expect("a connect");
    assert_eq!(id, 0x1234);
    assert_eq!(
        request,
        Request::Connect {
            address: [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
            address_kind: 0,
        }
    );
    assert!(parse(&bytes[..9]).is_none(), "one byte short");
    assert!(
        parse(&[0x34, 0x12, 0x00, 0, 0, 0, 0, 0, 0, 2]).is_none(),
        "address type 2 is not public or random"
    );
}

#[test]
fn a_real_answer_carries_its_value_after_the_status() {
    let mut value = [0u8; VALUE_MAX];
    value[..2].copy_from_slice(&[0xde, 0xad]);
    let mut out = [0u8; MAX_PAYLOAD];
    let length = Reply::Answer {
        id: 0x0203,
        status: Status::Ok,
        len: 2,
        value,
    }
    .encode(&mut out);
    assert_eq!(&out[..length], &[0x03, 0x02, 0x01, 0x00, 0xde, 0xad]);
}

#[test]
fn a_notification_is_its_bytes_and_never_text() {
    // Bytes that are not UTF-8 go up unchanged, and a NUL is not a terminator.
    let mut value = [0u8; NOTIFY_MAX];
    value[..4].copy_from_slice(&[0xff, 0x00, b'o', 0x80]);
    let mut out = [0u8; MAX_PAYLOAD];
    let length = Reply::Notify {
        link: 2,
        handle: 0x21,
        len: 4,
        value,
    }
    .encode(&mut out);
    assert_eq!(
        &out[..length],
        &[0x02, 0x00, 0x02, 0x21, 0x00, 0xff, 0x00, b'o', 0x80]
    );
}
