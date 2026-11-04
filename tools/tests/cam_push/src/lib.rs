//! Host tests for `firmware/cam/src/push.rs` and the deadlines in `serve.rs`.
//!
//! `tools/mesh.sh` copies those files and `stream.rs` to `src/` before
//! `cargo test`. The copies are not committed. Helpers exist for the tests, so the plain lib
//! build has nothing else to call.
#![allow(dead_code)]

#[path = "push.rs"]
mod push;
#[path = "serve.rs"]
mod serve;
#[path = "stream.rs"]
mod stream;

use ed25519_compact::{KeyPair, Seed};
#[allow(unused_imports)]
use tallow_mesh_core::ota::{signed_message, Code, PRODUCT_CAM0, PRODUCT_NODE};

#[cfg(test)]
use std::io::{Read, Write};
#[cfg(test)]
use std::net::{TcpListener, TcpStream};
#[cfg(test)]
use std::time::{Duration, Instant};

const SEED: [u8; 32] = [0x42; 32];
const VERSION: u32 = 7;
const FLOOR: u32 = 3;

fn keypair(seed: [u8; 32]) -> KeyPair {
    KeyPair::from_seed(Seed::new(seed))
}

fn sign(seed: [u8; 32], product: u8, version: u32, digest: &[u8; 32]) -> [u8; 64] {
    *keypair(seed)
        .sk
        .sign(signed_message(product, version, digest), None)
}

fn digest_of(image: &[u8]) -> [u8; 32] {
    let mut hash = tallow_mesh_core::ota::ImageHash::new();
    hash.update(image);
    hash.finish()
}

fn header(
    size: u32,
    product: u8,
    version: u32,
    digest: &[u8; 32],
    signature: &[u8; 64],
) -> [u8; push::HEADER_LEN] {
    let mut out = [0u8; push::HEADER_LEN];
    out[..4].copy_from_slice(&size.to_be_bytes());
    out[4] = product;
    out[5..9].copy_from_slice(&version.to_le_bytes());
    out[9..41].copy_from_slice(digest);
    out[41..].copy_from_slice(signature);
    out
}

/// A header signed for the cam at [`VERSION`].
fn good(image: &[u8]) -> [u8; push::HEADER_LEN] {
    let digest = digest_of(image);
    let signature = sign(SEED, PRODUCT_CAM0, VERSION, &digest);
    header(image.len() as u32, PRODUCT_CAM0, VERSION, &digest, &signature)
}

fn public() -> [u8; 32] {
    *keypair(SEED).pk
}

#[test]
fn a_bad_signature_is_refused_before_any_image_byte_is_taken() {
    let image = b"jpeg-bytes-that-must-not-be-taken";
    let digest = digest_of(image);
    let mut signature = sign(SEED, PRODUCT_CAM0, VERSION, &digest);
    signature[10] ^= 0x01;
    let mut offer = push::Offer::new();
    let refused = header(image.len() as u32, PRODUCT_CAM0, VERSION, &digest, &signature);
    assert_eq!(offer.admit(&refused, &public(), FLOOR), Code::BadSignature);
    assert_eq!(offer.taken(), 0);
    assert!(!offer.take(image));
    assert_eq!(offer.taken(), 0);
    assert!(!offer.activate());
}

#[test]
fn a_good_signature_with_a_different_body_does_not_activate() {
    let image = b"the body the signature names.";
    let mut other = *image;
    other[0] ^= 0xff;
    let mut offer = push::Offer::new();
    assert_eq!(offer.admit(&good(image), &public(), FLOOR), Code::Ok);
    assert!(offer.take(&other));
    assert!(!offer.activate());
}

#[test]
fn a_matching_body_activates() {
    let image = b"the body the signature names.";
    let mut offer = push::Offer::new();
    assert_eq!(offer.admit(&good(image), &public(), FLOOR), Code::Ok);
    assert_eq!(offer.remaining(), image.len() as u32);
    assert_eq!(offer.version(), VERSION);
    assert!(offer.take(image));
    assert_eq!(offer.remaining(), 0);
    assert!(offer.activate());
}

#[test]
fn a_body_past_the_signed_size_is_refused_and_not_counted() {
    let image = b"signed image";
    let mut offer = push::Offer::new();
    assert_eq!(offer.admit(&good(image), &public(), FLOOR), Code::Ok);
    assert!(!offer.take(&[0u8; 13]));
    assert_eq!(offer.taken(), 0);
    assert_eq!(offer.remaining(), image.len() as u32);
    assert!(offer.take(image));
    assert!(!offer.take(&[0u8; 1]));
    assert!(offer.activate());
}

#[test]
fn a_zero_size_is_refused_before_any_image_byte_is_taken() {
    let digest = digest_of(b"");
    let signature = sign(SEED, PRODUCT_CAM0, VERSION, &digest);
    let mut offer = push::Offer::new();
    let refused = header(0, PRODUCT_CAM0, VERSION, &digest, &signature);
    assert_eq!(offer.admit(&refused, &public(), FLOOR), Code::Refused);
    assert_eq!(offer.taken(), 0);
    assert!(!offer.take(&[0]));
}

#[test]
fn a_size_past_the_slot_is_refused_before_any_image_byte_is_taken() {
    let digest = digest_of(b"x");
    let signature = sign(SEED, PRODUCT_CAM0, VERSION, &digest);
    let mut offer = push::Offer::new();
    let refused = header(push::SLOT + 1, PRODUCT_CAM0, VERSION, &digest, &signature);
    assert_eq!(offer.admit(&refused, &public(), FLOOR), Code::Refused);
    assert_eq!(offer.taken(), 0);
    assert!(!offer.take(b"x"));
    assert_eq!(push::SLOT, 0x7C_0000);
}

#[test]
fn a_short_header_is_refused() {
    let image = b"x";
    let whole = good(image);
    let mut offer = push::Offer::new();
    assert_eq!(
        offer.admit(&whole[..push::HEADER_LEN - 1], &public(), FLOOR),
        Code::Refused
    );
    assert!(!offer.take(image));
}

#[test]
fn a_node_image_is_refused_as_the_wrong_product() {
    let image = b"an image signed for a mesh node.";
    let digest = digest_of(image);
    let signature = sign(SEED, PRODUCT_NODE, VERSION, &digest);
    let mut offer = push::Offer::new();
    let refused = header(image.len() as u32, PRODUCT_NODE, VERSION, &digest, &signature);
    assert_eq!(offer.admit(&refused, &public(), FLOOR), Code::WrongProduct);
    assert!(!offer.take(image));
}

#[test]
fn a_node_signature_relabelled_for_the_cam_is_a_bad_signature() {
    let image = b"an image signed for a mesh node.";
    let digest = digest_of(image);
    let signature = sign(SEED, PRODUCT_NODE, VERSION, &digest);
    let mut offer = push::Offer::new();
    let refused = header(image.len() as u32, PRODUCT_CAM0, VERSION, &digest, &signature);
    assert_eq!(offer.admit(&refused, &public(), FLOOR), Code::BadSignature);
}

#[test]
fn a_raised_version_breaks_the_signature() {
    let image = b"an old image with a new number.";
    let digest = digest_of(image);
    let signature = sign(SEED, PRODUCT_CAM0, FLOOR - 1, &digest);
    let mut offer = push::Offer::new();
    let refused = header(image.len() as u32, PRODUCT_CAM0, FLOOR, &digest, &signature);
    assert_eq!(offer.admit(&refused, &public(), FLOOR), Code::BadSignature);
}

#[test]
fn a_version_below_the_floor_is_a_downgrade() {
    let image = b"an older image, correctly signed.";
    let digest = digest_of(image);
    let signature = sign(SEED, PRODUCT_CAM0, FLOOR - 1, &digest);
    let mut offer = push::Offer::new();
    let refused = header(image.len() as u32, PRODUCT_CAM0, FLOOR - 1, &digest, &signature);
    assert_eq!(offer.admit(&refused, &public(), FLOOR), Code::Downgrade);
    assert!(!offer.take(image));
    assert!(!offer.activate());
}

#[test]
fn the_floor_itself_is_allowed_so_a_push_can_be_retried() {
    let image = b"the same image again.";
    let digest = digest_of(image);
    let signature = sign(SEED, PRODUCT_CAM0, FLOOR, &digest);
    let mut offer = push::Offer::new();
    let again = header(image.len() as u32, PRODUCT_CAM0, FLOOR, &digest, &signature);
    assert_eq!(offer.admit(&again, &public(), FLOOR), Code::Ok);
}

#[test]
fn an_unreadable_floor_refuses_every_version() {
    let image = b"any image.";
    let floor = push::floor_from::<()>(Err(()));
    let mut offer = push::Offer::new();
    assert_eq!(offer.admit(&good(image), &public(), floor), Code::Downgrade);
}

#[test]
fn a_missing_floor_is_zero_and_a_stored_one_is_read_back() {
    assert_eq!(push::floor_from::<()>(Ok(None)), 0);
    assert_eq!(push::floor_from::<()>(Ok(Some(9))), 9);
}

#[test]
fn only_a_higher_pending_version_raises_the_floor() {
    assert_eq!(push::promoted(3, Some(4)), Some(4));
    assert_eq!(push::promoted(3, Some(3)), None);
    assert_eq!(push::promoted(3, Some(2)), None);
    assert_eq!(push::promoted(3, None), None);
}

#[test]
fn a_job_with_no_progress_expires_after_thirty_seconds() {
    let mut idle = push::Idle::new(1_000);
    assert!(!idle.expired(1_000 + push::IDLE_MS - 1));
    assert!(idle.expired(1_000 + push::IDLE_MS));
    idle.note(1_000 + push::IDLE_MS);
    assert!(!idle.expired(1_000 + push::IDLE_MS * 2 - 1));
    assert!(idle.expired(1_000 + push::IDLE_MS * 2));
    assert_eq!(push::IDLE_MS, 30_000);
}

#[test]
fn the_idle_limit_survives_the_clock_wrapping() {
    let start = u32::MAX - 1_000;
    let idle = push::Idle::new(start);
    assert!(!idle.expired(start.wrapping_add(push::IDLE_MS - 1)));
    assert!(idle.expired(start.wrapping_add(push::IDLE_MS)));
}

#[test]
fn wifi_down_clears_the_five_minute_timer() {
    let mut steady = push::Steady::new();
    steady.note_link(true, 0);
    steady.note_frame(push::FIVE_MIN_MS - 1);
    assert!(!steady.due(push::FIVE_MIN_MS - 1));
    steady.note_link(false, 1_000);
    steady.note_link(true, 1_000);
    steady.note_frame(1_000 + push::FIVE_MIN_MS - 1);
    assert!(!steady.due(1_000 + push::FIVE_MIN_MS - 1));
    assert!(steady.due(1_000 + push::FIVE_MIN_MS));
}

#[test]
fn the_mark_uses_wrapping_subtraction() {
    let mut steady = push::Steady::new();
    let start = u32::MAX - 1_000;
    steady.note_link(true, start);
    steady.note_frame(start.wrapping_add(push::FIVE_MIN_MS - 1));
    assert!(!steady.due(start.wrapping_add(push::FIVE_MIN_MS - 1)));
    assert!(steady.due(start.wrapping_add(push::FIVE_MIN_MS)));
}

#[test]
fn a_frame_is_required_before_the_mark() {
    let mut steady = push::Steady::new();
    steady.note_link(true, 0);
    assert!(!steady.due(push::FIVE_MIN_MS));
    steady.note_frame(push::FIVE_MIN_MS);
    assert!(steady.due(push::FIVE_MIN_MS));
    steady.mark();
    assert!(!steady.due(push::FIVE_MIN_MS));
}

#[test]
fn one_frame_at_boot_does_not_carry_a_dead_sensor_to_the_mark() {
    let mut steady = push::Steady::new();
    steady.note_link(true, 0);
    steady.note_frame(1_000);
    assert!(!steady.due(push::FIVE_MIN_MS));
    steady.note_frame(push::FIVE_MIN_MS - push::FRAME_FRESH_MS);
    assert!(steady.due(push::FIVE_MIN_MS));
    assert!(!steady.due(push::FIVE_MIN_MS + 1));
}

#[test]
fn rollback_waits_past_the_five_minute_mark() {
    assert_eq!(push::ROLLBACK_AFTER_MS, 15 * 60 * 1000);
    assert!(push::ROLLBACK_AFTER_MS >= push::FIVE_MIN_MS * 2);
}

#[cfg(test)]
fn pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (near, _) = listener.accept().unwrap();
    near.set_read_timeout(Some(serve::POLL)).unwrap();
    (near, peer)
}

#[test]
fn a_whole_get_passes_the_request_check() {
    let (mut near, mut peer) = pair();
    peer.write_all(b"GET /stream HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    assert!(serve::request_ok(&mut near, deadline));
}

#[test]
fn only_a_request_line_that_starts_with_get_passes() {
    let (mut near, mut peer) = pair();
    peer.write_all(b"POST / HTTP/1.1\r\nX-Note: GET \r\n\r\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    assert!(!serve::request_ok(&mut near, deadline));
}

#[test]
fn a_silent_client_is_dropped_at_the_request_deadline() {
    let (mut near, _peer) = pair();
    let start = Instant::now();
    assert!(!serve::request_ok(
        &mut near,
        start + Duration::from_millis(600)
    ));
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_dripping_client_does_not_reset_the_request_deadline() {
    let (mut near, mut peer) = pair();
    let drip = std::thread::spawn(move || {
        for _ in 0..40 {
            if peer.write_all(b"x").is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    let start = Instant::now();
    assert!(!serve::request_ok(
        &mut near,
        start + Duration::from_millis(600)
    ));
    assert!(start.elapsed() < Duration::from_millis(1500));
    drop(near);
    drip.join().unwrap();
}

#[test]
fn a_write_to_a_reader_finishes_inside_the_deadline() {
    let (mut near, mut peer) = pair();
    let reader = std::thread::spawn(move || {
        let mut sink = Vec::new();
        peer.read_to_end(&mut sink).unwrap();
        sink.len()
    });
    let body = vec![7u8; 256 * 1024];
    let deadline = Instant::now() + Duration::from_secs(5);
    assert!(serve::write_by(&mut near, &body, deadline));
    drop(near);
    assert_eq!(reader.join().unwrap(), body.len());
}

#[test]
fn two_missed_records_do_not_reconnect_and_three_do() {
    let (again, misses) = stream::reconnect_now(true, false, 0);
    assert!(!again);
    assert_eq!(misses, 1);
    let (again, misses) = stream::reconnect_now(true, false, misses);
    assert!(!again);
    assert_eq!(misses, 2);
    let (again, misses) = stream::reconnect_now(true, false, misses);
    assert!(again);
    assert_eq!(misses, 0);
    let (again, misses) = stream::reconnect_now(true, true, 2);
    assert!(!again);
    assert_eq!(misses, 0);
}

#[test]
fn a_down_netif_reconnects_at_once() {
    let (again, misses) = stream::reconnect_now(false, true, 2);
    assert!(again);
    assert_eq!(misses, 0);
    let (again, misses) = stream::reconnect_now(false, false, 0);
    assert!(again);
    assert_eq!(misses, 0);
}

#[test]
fn a_client_that_stops_reading_is_dropped_at_the_frame_deadline() {
    let (mut near, _peer) = pair();
    let body = vec![7u8; 64 * 1024 * 1024];
    let start = Instant::now();
    assert!(!serve::write_by(
        &mut near,
        &body,
        start + Duration::from_millis(600)
    ));
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[test]
fn a_past_deadline_writes_nothing() {
    let (mut near, _peer) = pair();
    assert!(!serve::write_by(&mut near, b"x", Instant::now()));
}
