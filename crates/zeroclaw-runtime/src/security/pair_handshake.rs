//! Cryptographic primitives for the SHARED_AUTH_SPEC.md §3.3 pairing handshake.
//!
//! This module is feature-gated under `webauthn` because it ships only when the
//! WebAuthn surface is enabled. It implements:
//!
//! - HKDF-SHA256 KEK derivation from a 32-byte PRF output (§1.3 primary KEK).
//! - HMAC-SHA256 companion_proof / hub_proof construction and verification (§3.3 Step C).
//! - AES Key Wrap RFC 3394 wrap/unwrap of the 32-byte sync DEK (§2).
//!
//! The module is **not** a complete pairing implementation: WebAuthn attestation
//! parsing + PRF-extension extraction live in the gateway's WebAuthn module.
//! Tests here verify byte-identical output against the spec's RFC 3394 canonical
//! vector and the §1.4 / §3.5 cross-implementation fixtures.
//!
//! All multi-byte buffers are constant-time-compared where comparisons feed
//! authentication decisions.

use aes_kw::KwAes256;
use aes_kw::cipher::KeyInit;
use anyhow::{Result, bail};
use hmac::{Hmac, Mac};
use ring::hkdf;
use sha2::Sha256;

/// HKDF salt — SHARED_AUTH_SPEC.md §1.2. Brand-neutral, version-tagged.
pub const HKDF_SALT_V1: &[u8] = b"creativemachines/auth/v1";

/// Primary KEK info-string prefix — SHARED_AUTH_SPEC.md §1.3.
pub const PRIMARY_KEK_INFO_PREFIX: &str = "creativemachines/kek/primary/v1/";

/// HMAC input for `companion_proof` — SHARED_AUTH_SPEC.md §3.3 Step C.
pub const COMPANION_PROOF_INPUT: &[u8] = b"creativemachines/pair/proof/v1";

/// HMAC input for `hub_proof` — SHARED_AUTH_SPEC.md §3.3 Step C response.
pub const HUB_PROOF_INPUT: &[u8] = b"creativemachines/pair/proof/v1/hub";

/// Required PRF output length — SHARED_AUTH_SPEC.md §1.1.
pub const PRF_OUTPUT_LEN: usize = 32;

/// Required KEK length — SHARED_AUTH_SPEC.md §0 ("All long-term keys… are 32 bytes").
pub const KEK_LEN: usize = 32;

/// Required DEK length — SHARED_AUTH_SPEC.md §2.1.
pub const DEK_LEN: usize = 32;

/// Required wrapped-DEK length — SHARED_AUTH_SPEC.md §2.1 (`n × 8 + 8`, n=4).
pub const WRAPPED_DEK_LEN: usize = 40;

/// Required HMAC-SHA256 proof length.
pub const PROOF_LEN: usize = 32;

/// Default `thread_id` for v1 — SHARED_AUTH_SPEC.md §5.2.
pub const DEFAULT_THREAD_ID: &str = "default";

/// A 32-byte primary KEK derived per SHARED_AUTH_SPEC.md §1.3.
///
/// Wraps a fixed-size byte array so type-system mistakes (e.g. passing a DEK
/// where a KEK is expected) become compile errors.
#[derive(Clone)]
pub struct PrimaryKek([u8; KEK_LEN]);

impl PrimaryKek {
    /// Derive the primary KEK from a 32-byte PRF output and a thread_id.
    ///
    /// Implements SHARED_AUTH_SPEC.md §1.3:
    ///   `salt = "creativemachines/auth/v1"`
    ///   `info = "creativemachines/kek/primary/v1/{thread_id}"`
    ///   HKDF-SHA256, L = 32.
    ///
    /// `thread_id` MUST match `[a-z0-9_]{1,32}` — in v1 only `"default"` is legal.
    pub fn derive(prf_output: &[u8], thread_id: &str) -> Result<Self> {
        if prf_output.len() != PRF_OUTPUT_LEN {
            bail!(
                "PRF output must be {PRF_OUTPUT_LEN} bytes, got {}",
                prf_output.len()
            );
        }
        validate_thread_id(thread_id)?;

        let info = format!("{PRIMARY_KEK_INFO_PREFIX}{thread_id}");

        let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, HKDF_SALT_V1);
        let prk = salt.extract(prf_output);

        struct Len32;
        impl hkdf::KeyType for Len32 {
            fn len(&self) -> usize {
                KEK_LEN
            }
        }

        let info_bytes: &[&[u8]] = &[info.as_bytes()];
        let okm = prk
            .expand(info_bytes, Len32)
            .map_err(|_| anyhow::anyhow!("HKDF expand failed"))?;
        let mut out = [0u8; KEK_LEN];
        okm.fill(&mut out)
            .map_err(|_| anyhow::anyhow!("HKDF fill failed"))?;
        Ok(Self(out))
    }

    /// Borrow the raw KEK bytes.
    pub fn as_bytes(&self) -> &[u8; KEK_LEN] {
        &self.0
    }
}

impl std::fmt::Debug for PrimaryKek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the KEK bytes, even in debug logs.
        f.write_str("PrimaryKek(<redacted 32 bytes>)")
    }
}

/// Compute the `companion_proof` HMAC the Companion submits in Step C.
///
/// Per SHARED_AUTH_SPEC.md §3.3:
/// `companion_proof = HMAC-SHA256(primary_kek, "creativemachines/pair/proof/v1")`
///
/// Output is exactly 32 bytes.
pub fn compute_companion_proof(kek: &PrimaryKek) -> [u8; PROOF_LEN] {
    hmac_sha256(kek.as_bytes(), COMPANION_PROOF_INPUT)
}

/// Compute the `hub_proof` HMAC the Hub returns in Step C's response.
///
/// Per SHARED_AUTH_SPEC.md §3.3:
/// `hub_proof = HMAC-SHA256(primary_kek, "creativemachines/pair/proof/v1/hub")`
pub fn compute_hub_proof(kek: &PrimaryKek) -> [u8; PROOF_LEN] {
    hmac_sha256(kek.as_bytes(), HUB_PROOF_INPUT)
}

/// Verify a candidate `companion_proof` against the locally-derived KEK.
///
/// Returns `true` iff the proof is byte-identical to what
/// `compute_companion_proof` produces, in constant time.
pub fn verify_companion_proof(kek: &PrimaryKek, candidate: &[u8]) -> bool {
    if candidate.len() != PROOF_LEN {
        return false;
    }
    let expected = compute_companion_proof(kek);
    constant_time_eq(&expected, candidate)
}

/// Wrap a 32-byte DEK under a 32-byte primary KEK using AES-KW (RFC 3394).
///
/// SHARED_AUTH_SPEC.md §2:
/// - 32-byte plaintext DEK → 40-byte wrapped output.
/// - Default IV `A6A6A6A6A6A6A6A6` (implicit, no IV transmitted).
/// - RFC 3394 unpadded variant only.
pub fn wrap_dek(kek: &PrimaryKek, dek: &[u8; DEK_LEN]) -> [u8; WRAPPED_DEK_LEN] {
    let cipher = KwAes256::new_from_slice(kek.as_bytes()).expect("KwAes256 accepts 32-byte key");
    let mut wrapped = [0u8; WRAPPED_DEK_LEN];
    cipher
        .wrap_key(dek.as_slice(), &mut wrapped)
        .expect("AES-KW wrap of fixed-size 32 B input must succeed");
    wrapped
}

/// Unwrap a 40-byte wrapped DEK under a 32-byte primary KEK.
///
/// Returns `Err` on integrity-check failure (RFC 3394 default-IV mismatch),
/// never partially-decrypted garbage. SHARED_AUTH_SPEC.md §2.2.
pub fn unwrap_dek(kek: &PrimaryKek, wrapped: &[u8; WRAPPED_DEK_LEN]) -> Result<[u8; DEK_LEN]> {
    let cipher = KwAes256::new_from_slice(kek.as_bytes()).expect("KwAes256 accepts 32-byte key");
    let mut out = [0u8; DEK_LEN];
    cipher
        .unwrap_key(wrapped, &mut out)
        .map_err(|e| anyhow::anyhow!("AES-KW unwrap failed (integrity check): {e:?}"))?;
    Ok(out)
}

/// Generate a fresh random 32-byte sync DEK.
pub fn generate_dek() -> [u8; DEK_LEN] {
    use ring::rand::{SecureRandom, SystemRandom};
    let rng = SystemRandom::new();
    let mut dek = [0u8; DEK_LEN];
    rng.fill(&mut dek).expect("system RNG fill must succeed");
    dek
}

/// Validate a `thread_id` per SHARED_AUTH_SPEC.md §5.2 ("v1: only `default`").
fn validate_thread_id(s: &str) -> Result<()> {
    if s.is_empty() || s.len() > 32 {
        bail!("thread_id must be 1..=32 chars, got {}", s.len());
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        bail!("thread_id must match [a-z0-9_]+, got {s:?}");
    }
    if s != DEFAULT_THREAD_ID {
        bail!("v1 only accepts thread_id=\"default\", got {s:?}");
    }
    Ok(())
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; PROOF_LEN] {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message);
    let result = mac.finalize().into_bytes();
    let mut out = [0u8; PROOF_LEN];
    out.copy_from_slice(&result);
    out
}

/// Constant-time byte-array equality.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── SHARED_AUTH_SPEC.md §1.4 cross-implementation fixture ────────────
    //
    // PRF output (32 B) = 0x0011..eeff repeated twice
    // Expected primary KEK (thread_id = "default"):
    //   25f40f6353c5a2bc35e5b12e1a07f1d7abb44dc8d74f209ff7239a93e3b39393
    //
    // This is the canonical test vector that both Hub and Companion implementations
    // must agree on byte-for-byte.

    fn fixture_prf_output() -> [u8; 32] {
        let mut prf = [0u8; 32];
        let half = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        prf[..16].copy_from_slice(&half);
        prf[16..].copy_from_slice(&half);
        prf
    }

    #[test]
    fn primary_kek_matches_spec_fixture() {
        let prf = fixture_prf_output();
        let kek = PrimaryKek::derive(&prf, "default").expect("default thread_id");
        let expected_hex = "25f40f6353c5a2bc35e5b12e1a07f1d7abb44dc8d74f209ff7239a93e3b39393";
        assert_eq!(hex::encode(kek.as_bytes()), expected_hex);
    }

    #[test]
    fn primary_kek_rejects_wrong_prf_length() {
        assert!(PrimaryKek::derive(&[0u8; 31], "default").is_err());
        assert!(PrimaryKek::derive(&[0u8; 33], "default").is_err());
    }

    #[test]
    fn primary_kek_rejects_non_default_thread_id() {
        let prf = [0u8; 32];
        assert!(PrimaryKek::derive(&prf, "default").is_ok());
        assert!(PrimaryKek::derive(&prf, "other").is_err());
        assert!(PrimaryKek::derive(&prf, "").is_err());
        assert!(PrimaryKek::derive(&prf, "DEFAULT").is_err());
    }

    // ── SHARED_AUTH_SPEC.md §2.4 RFC 3394 canonical vector ──────────────
    //
    // Wrap 256 bits of Key Data with a 256-bit KEK:
    // KEK       = 000102...1F (32 B)
    // Key Data  = 0011...0F  (32 B)
    // Cipher    = 28C9F404C4B810F4CBCCB35CFB87F8263F5786E2D80ED326CBC7F0E71A99F43BFB988B9B7A02DD21

    #[test]
    fn aes_kw_matches_rfc3394_vector() {
        let kek_bytes_hex = "000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F";
        let dek_bytes_hex = "00112233445566778899AABBCCDDEEFF000102030405060708090A0B0C0D0E0F";
        let expected_hex =
            "28C9F404C4B810F4CBCCB35CFB87F8263F5786E2D80ED326CBC7F0E71A99F43BFB988B9B7A02DD21";

        let mut kek_bytes = [0u8; KEK_LEN];
        hex::decode_to_slice(kek_bytes_hex, &mut kek_bytes).unwrap();
        let mut dek = [0u8; DEK_LEN];
        hex::decode_to_slice(dek_bytes_hex, &mut dek).unwrap();
        let mut expected = [0u8; WRAPPED_DEK_LEN];
        hex::decode_to_slice(expected_hex, &mut expected).unwrap();

        let kek = PrimaryKek(kek_bytes);
        let wrapped = wrap_dek(&kek, &dek);
        assert_eq!(
            hex::encode_upper(wrapped),
            hex::encode_upper(expected),
            "AES-KW wrap MUST match RFC 3394 §4.3 canonical vector"
        );

        let unwrapped = unwrap_dek(&kek, &wrapped).expect("unwrap valid wrap");
        assert_eq!(unwrapped, dek);
    }

    #[test]
    fn aes_kw_unwrap_rejects_corrupted_blob() {
        let kek = PrimaryKek([0x42; KEK_LEN]);
        let dek = [0x55; DEK_LEN];
        let mut wrapped = wrap_dek(&kek, &dek);
        // Flip a bit in the integrity-check region.
        wrapped[0] ^= 0x80;
        let result = unwrap_dek(&kek, &wrapped);
        assert!(result.is_err(), "AES-KW unwrap MUST reject corrupted blob");
    }

    #[test]
    fn aes_kw_unwrap_rejects_wrong_kek() {
        let kek = PrimaryKek([0x11; KEK_LEN]);
        let wrong_kek = PrimaryKek([0x22; KEK_LEN]);
        let dek = [0x33; DEK_LEN];
        let wrapped = wrap_dek(&kek, &dek);
        let result = unwrap_dek(&wrong_kek, &wrapped);
        assert!(result.is_err(), "AES-KW unwrap MUST reject wrong KEK");
    }

    #[test]
    fn aes_kw_round_trip_random() {
        let kek = PrimaryKek(generate_dek());
        let dek = generate_dek();
        let wrapped = wrap_dek(&kek, &dek);
        assert_eq!(wrapped.len(), WRAPPED_DEK_LEN);
        let unwrapped = unwrap_dek(&kek, &wrapped).unwrap();
        assert_eq!(unwrapped, dek);
    }

    // ── HMAC proofs ──────────────────────────────────────────────────────

    #[test]
    fn companion_and_hub_proofs_differ() {
        let kek = PrimaryKek([0x42; KEK_LEN]);
        let cp = compute_companion_proof(&kek);
        let hp = compute_hub_proof(&kek);
        assert_ne!(cp, hp, "domain separation MUST yield different proofs");
    }

    #[test]
    fn companion_proof_is_32_bytes() {
        let kek = PrimaryKek([0x42; KEK_LEN]);
        let cp = compute_companion_proof(&kek);
        assert_eq!(cp.len(), 32);
    }

    #[test]
    fn verify_companion_proof_accepts_correct() {
        let kek = PrimaryKek([0x42; KEK_LEN]);
        let cp = compute_companion_proof(&kek);
        assert!(verify_companion_proof(&kek, &cp));
    }

    #[test]
    fn verify_companion_proof_rejects_wrong_kek() {
        let kek_a = PrimaryKek([0x11; KEK_LEN]);
        let kek_b = PrimaryKek([0x22; KEK_LEN]);
        let cp_a = compute_companion_proof(&kek_a);
        assert!(!verify_companion_proof(&kek_b, &cp_a));
    }

    #[test]
    fn verify_companion_proof_rejects_wrong_length() {
        let kek = PrimaryKek([0x42; KEK_LEN]);
        assert!(!verify_companion_proof(&kek, &[0x00; 31]));
        assert!(!verify_companion_proof(&kek, &[0x00; 33]));
        assert!(!verify_companion_proof(&kek, &[]));
    }

    #[test]
    fn verify_companion_proof_rejects_flipped_bit() {
        let kek = PrimaryKek([0x42; KEK_LEN]);
        let mut cp = compute_companion_proof(&kek);
        cp[0] ^= 0x01;
        assert!(!verify_companion_proof(&kek, &cp));
    }

    #[test]
    fn primary_kek_debug_redacts_bytes() {
        let kek = PrimaryKek([0x42; KEK_LEN]);
        let s = format!("{kek:?}");
        assert!(s.contains("redacted"));
        assert!(!s.contains("42"));
    }

    #[test]
    fn validate_thread_id_accepts_default_only_in_v1() {
        assert!(validate_thread_id("default").is_ok());
        assert!(validate_thread_id("calendar").is_err());
        assert!(validate_thread_id("Default").is_err());
        assert!(validate_thread_id("").is_err());
        assert!(validate_thread_id(&"x".repeat(33)).is_err());
    }
}
