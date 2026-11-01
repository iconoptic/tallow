//! Checks a signed OTA image. Nothing here holds a key: the caller passes the
//! public key in, so this builds and tests with no key file.
//!
//! The image is hashed with SHA-256 as its chunks arrive. The signature is
//! Ed25519 over that 32-byte digest. `ed25519-compact` and `sha2` do the
//! arithmetic and the hashing. This file only feeds them and reads the answer.
//!
//! The mesh key does not take part. A frame that passes the mesh check is not
//! enough to change a node's firmware.

use ed25519_compact::{PublicKey, Signature};
use sha2::{Digest, Sha256};

/// Length of the digest that the signature covers.
pub const DIGEST: usize = 32;

/// SHA-256 of an image, fed in chunks as they arrive. Any split of the same
/// bytes gives the same digest.
#[derive(Clone)]
pub struct ImageHash {
    state: Sha256,
}

impl Default for ImageHash {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageHash {
    pub fn new() -> Self {
        Self {
            state: Sha256::new(),
        }
    }

    pub fn update(&mut self, chunk: &[u8]) {
        self.state.update(chunk);
    }

    pub fn finish(self) -> [u8; DIGEST] {
        let mut out = [0u8; DIGEST];
        out.copy_from_slice(&self.state.finalize());
        out
    }
}

/// True when `signature` is an Ed25519 signature by `public_key` over `digest`.
///
/// A malformed key, a small-order key, a non-canonical signature, or a wrong
/// signature all return false. None of them panic.
pub fn verify(public_key: &[u8; 32], digest: &[u8; DIGEST], signature: &[u8; 64]) -> bool {
    check(public_key, digest, signature)
}

/// The same check over any message. `verify` is this over a digest. The RFC
/// 8032 vectors sign whole messages, so the tests use this to reach them.
fn check(public_key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    let public_key = PublicKey::new(*public_key);
    let signature = Signature::new(*signature);
    public_key.verify(message, &signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_compact::{KeyPair, Seed};

    /// Fixed seeds so every test key is the same on every run.
    const SEED: [u8; 32] = [0x42; 32];
    const OTHER_SEED: [u8; 32] = [0x43; 32];

    fn unhex<const N: usize>(text: &str) -> [u8; N] {
        let mut out = [0u8; N];
        for (index, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap();
        }
        out
    }

    fn digest_of(image: &[u8]) -> [u8; DIGEST] {
        let mut hash = ImageHash::new();
        hash.update(image);
        hash.finish()
    }

    fn keypair(seed: [u8; 32]) -> KeyPair {
        KeyPair::from_seed(Seed::new(seed))
    }

    fn sign(seed: [u8; 32], digest: &[u8; DIGEST]) -> [u8; 64] {
        *keypair(seed).sk.sign(digest, None)
    }

    const IMAGE: &[u8] = b"tallow node image, version 1";

    #[test]
    fn a_good_signature_over_the_digest_verifies() {
        let digest = digest_of(IMAGE);
        let public = *keypair(SEED).pk;
        assert!(verify(&public, &digest, &sign(SEED, &digest)));
    }

    #[test]
    fn a_bad_signature_is_refused() {
        let digest = digest_of(IMAGE);
        let public = *keypair(SEED).pk;
        let mut signature = sign(SEED, &digest);
        signature[10] ^= 0x01;
        assert!(!verify(&public, &digest, &signature));
    }

    #[test]
    fn a_flipped_hash_byte_is_refused() {
        let public = *keypair(SEED).pk;
        let digest = digest_of(IMAGE);
        let signature = sign(SEED, &digest);
        let mut tampered = digest;
        tampered[0] ^= 0x80;
        assert!(!verify(&public, &tampered, &signature));
    }

    #[test]
    fn a_signature_from_another_key_is_refused() {
        let digest = digest_of(IMAGE);
        let other_public = *keypair(OTHER_SEED).pk;
        assert!(!verify(&other_public, &digest, &sign(SEED, &digest)));
    }

    #[test]
    fn a_changed_image_with_the_old_signature_is_refused() {
        let public = *keypair(SEED).pk;
        let signature = sign(SEED, &digest_of(IMAGE));
        let changed = digest_of(b"tallow node image, version 2");
        assert!(!verify(&public, &changed, &signature));
    }

    #[test]
    fn malformed_keys_and_signatures_are_refused_without_a_panic() {
        let digest = digest_of(IMAGE);
        let signature = sign(SEED, &digest);
        let public = *keypair(SEED).pk;
        // All zeros is the identity point, which has small order.
        assert!(!verify(&[0u8; 32], &digest, &signature));
        // A y coordinate above the field prime is not a canonical point.
        assert!(!verify(&[0xff; 32], &digest, &signature));
        // All zeros is not a signature by any key.
        assert!(!verify(&public, &digest, &[0u8; 64]));
        // S at or above the group order is not canonical, so it is refused.
        let mut high_s = signature;
        high_s[32..].copy_from_slice(&[0xff; 32]);
        assert!(!verify(&public, &digest, &high_s));
    }

    #[test]
    fn sha256_of_abc_matches_the_fips_vector() {
        assert_eq!(
            digest_of(b"abc"),
            unhex::<32>("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn the_digest_is_the_same_however_the_chunks_fall() {
        let image: Vec<u8> = (0..300u32).map(|value| (value * 7) as u8).collect();
        let whole = digest_of(&image);
        for split in 0..=image.len() {
            let mut hash = ImageHash::new();
            hash.update(&image[..split]);
            hash.update(&image[split..]);
            assert_eq!(hash.finish(), whole, "split at {split}");
        }
        let mut one_byte_at_a_time = ImageHash::default();
        for byte in &image {
            one_byte_at_a_time.update(std::slice::from_ref(byte));
        }
        assert_eq!(one_byte_at_a_time.finish(), whole);
    }

    #[test]
    fn rfc8032_test_1_verifies_over_the_empty_message() {
        let public =
            unhex::<32>("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
        let signature = unhex::<64>(concat!(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155",
            "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
        ));
        assert!(check(&public, &[], &signature));
    }

    #[test]
    fn rfc8032_test_2_verifies_over_the_one_byte_message() {
        let public =
            unhex::<32>("3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c");
        let signature = unhex::<64>(concat!(
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da",
            "085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00"
        ));
        assert!(check(&public, &[0x72], &signature));
        assert!(!check(&public, &[0x73], &signature));
    }

    #[test]
    fn an_openssl_signature_from_a_key_made_by_ota_key_verifies() {
        // Made by `make_ota_key` in tools/mesh.sh, in a scratch directory, and
        // signed with OpenSSL over the digest of MESSAGE. This is a throwaway
        // test key, not the image key.
        const MESSAGE: &[u8] = b"tallow ota-key check";
        let public =
            unhex::<32>("2cb88b8deb50744a541ea422bdf99056f68bce996dbcd2f814275b6063587468");
        let signature = unhex::<64>(concat!(
            "82663e8858c03ec1892ffffa646458d5653c37f2d3d6ce1d139c9261db7c9744",
            "fb9acea581539e312e8229e840a80a1edc0865b195d16cc349c8b0524307ab05"
        ));
        assert!(verify(&public, &digest_of(MESSAGE), &signature));
    }
}
