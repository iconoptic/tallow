//! One sealed frame of each kind, with a fixed key, origin, boot, and sequence.
//!
//! The nonce is those three words, so the bytes do not change between runs.
//! These tests fail if `seal` moves any byte.

use tallow_mesh_core::wire::{Command, Frame, FrameKind, Sealer, MAX_FRAME, MAX_PAYLOAD};

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
        "a1011000040302010d0c0b0a141312113552a8ec7193390a6490d4368061a4bdf8b14837c90c36ad3f9358",
    );
}

#[test]
fn a_sealed_report_is_these_bytes() {
    // One sighting: address, BLE public, RSSI -60, count 3, age 2 seconds.
    let payload = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0, 0xc4, 3, 2];
    assert_golden(
        FrameKind::Report,
        &payload,
        "a1021000040302010d0c0b0a141312119ead57138e0a6cce6792f7c86577b5c131ee3b34d76c77fc1576",
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
        "a1031000040302010d0c0b0a141312119ead57138e0a6e466491d673a050b2f7672fdfbd402a4efb6e2653fb94f8d096c0e0be",
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
        "a1041000040302010d0c0b0a1413121135169bce62f56c0a6790d470a216de9e120feaa0701a72be6f2b769df6f1c447d945377a79b0e752af465d7f1dab8367ee011fe4177e2ca55e699a0ba51caa3c0b7b1b957db1f1a1870147bf4895a53e2fbdc0ab857352c357d101a1dd7e07b29a2fbd5ad6589c6d",
    );
}

#[test]
fn a_sealed_probe_is_these_bytes() {
    // Probe sequence 0x12345678, then four zeros of padding.
    let payload = [0x78, 0x56, 0x34, 0x12, 0, 0, 0, 0];
    assert_golden(
        FrameKind::Probe,
        &payload,
        "a1051000040302010d0c0b0a141312114c40afdc60f56c0ab4152f84d63c0e41ec09fae44a8b4c40",
    );
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
        "a1061000040302010d0c0b0a14131211331685ce04f5500a6092f4530ca9e779e306afc8450fd27bceef9b",
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
