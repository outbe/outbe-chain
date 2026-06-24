//! Enclave key material + quote assembly (secret-bearing — enclave only).
//!
//! Holds the Noise static keypair and the offer X25519 keypair (the tribute
//! offer key that clients encrypt to), and builds the SGX quote whose
//! `report_data` binds those public keys.
//!
//! The quote is **real**: under `gramine-sgx` it is produced by the hardware via
//! [`crate::gramine::dcap_quote`] and the measurements (MRENCLAVE/MRSIGNER/ISVSVN)
//! are parsed out of it. Under `gramine-direct`/bare there is no SGX hardware, so
//! the quote is empty and measurements are zero — the enclave runs in an
//! explicitly **unattested** mode (the host must use a dev policy to accept it).
//! The attestation key is a real Ed25519 keypair generated fresh each boot
//! (ephemeral, never sealed); its public key is bound into `report_data` and the
//! host verifies per-offer attestation tags against it.

use alloy_primitives::{keccak256, B256};
use commonware_codec::Encode as _;
use commonware_cryptography::Signer as _;
use x25519_dalek::{PublicKey, StaticSecret};

use outbe_tee::protocol::EnclaveResponse;
use outbe_tee::NOISE_PARAMS;

use crate::crypto::{hkdf_sha256, x25519_public};
use crate::dkg::PrivKey;
use crate::gramine::{self, AttestationType};
use crate::process::TributeOfferKeyMaterial;

/// Measurement value used when no SGX hardware quote is available
/// (`gramine-direct`/bare). Zero is not a valid SGX measurement, so it cannot be
/// mistaken for an attested enclave — a strict host policy rejects it.
pub const UNATTESTED_MEASUREMENT: B256 = B256::ZERO;

/// Enclave-resident key material.
pub struct EnclaveKeys {
    noise_private: Vec<u8>,
    noise_public: [u8; 32],
    /// X25519 offer secret (the decrypt key) + its public (clients encrypt to it).
    tribute_offer_secret: [u8; 32],
    tribute_offer_public: [u8; 32],
    /// Per-enclave Ed25519 attestation signing key, generated fresh each boot
    /// (ephemeral, never sealed). The host pins its public key from the quote this
    /// session and verifies per-offer attestation tags against it.
    attestation_signing: ed25519_dalek::SigningKey,
    /// Cached attestation public key bytes (the verifying key). Bound into
    /// `report_data` so the host can pin it from the quote.
    attestation_pub: [u8; 32],
    /// TEE threshold-BLS signing key (this enclave's DKG participant identity).
    tee_bls_key: PrivKey,
    /// X25519 secret used to open DKG shares sealed to this enclave.
    dkg_enc_secret: [u8; 32],
    mrenclave: B256,
    mrsigner: B256,
    isv_svn: u16,
    /// The real attestation environment detected at startup.
    attest_type: AttestationType,
    /// Real DCAP quote bytes (empty when unattested). Generated once at startup;
    /// the embedded report body carries the measurements + report_data.
    quote_body: Vec<u8>,
}

impl EnclaveKeys {
    /// Build enclave keys from an explicit offer secret. Generates a fresh Noise
    /// static key and uses mock measurements. The offer encryption salt is the
    /// fixed protocol constant [`outbe_tee::OFFER_HKDF_SALT`], not per-enclave.
    ///
    /// `dkg_seed_override` decouples this enclave's **DKG participant identity**
    /// (its threshold-BLS signing key + X25519 share-decryption key) from the
    /// offer secret: every validator's enclave shares the same dev offer secret,
    /// so without a distinct seed all `n` enclaves would be the *same* DKG
    /// participant (a degenerate ceremony). Each validator passes a distinct
    /// `--dkg-seed`; tests that already give each enclave a distinct offer secret
    /// pass `None` and fall back to it. The offer *key* is no longer derived from
    /// `tribute_offer_secret` in production — it comes from the DKG group signature
    /// (Seam F) at runtime — but the dev `tribute_offer_secret` remains the pre-DKG
    /// fallback decrypt key.
    pub fn new(
        tribute_offer_secret: [u8; 32],
        dkg_seed_override: Option<[u8; 32]>,
    ) -> Result<Self, String> {
        let params = NOISE_PARAMS
            .parse()
            .map_err(|e| format!("noise params: {e:?}"))?;
        let keypair = snow::Builder::new(params)
            .generate_keypair()
            .map_err(|e| format!("noise keygen: {e}"))?;
        if keypair.public.len() != 32 {
            return Err("noise public key is not 32 bytes".to_string());
        }
        let mut noise_public = [0u8; 32];
        noise_public.copy_from_slice(&keypair.public);

        let tribute_offer_public =
            PublicKey::from(&StaticSecret::from(tribute_offer_secret)).to_bytes();
        // Real Ed25519 attestation keypair, generated fresh per boot. `OsRng` is
        // used here only for protocol-required cryptographic secret material (the
        // attestation signing key); it never feeds VRF/leader-election/consensus
        // determinism. The key is ephemeral (not sealed): the host re-pins the
        // public key from each session's quote.
        let attestation_signing = ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng);
        let attestation_pub = attestation_signing.verifying_key().to_bytes();

        // DKG identity. Seeded from `dkg_seed_override` when provided (each
        // validator passes a distinct `--dkg-seed`, so the n enclaves are
        // distinct DKG participants even though they share the dev offer secret);
        // otherwise falls back to `tribute_offer_secret` (tests give each enclave a
        // distinct offer secret). Deterministic and stable across restart.
        let dkg_seed_input = dkg_seed_override.unwrap_or(tribute_offer_secret);
        let dkg_enc_secret = hkdf_sha256(&dkg_seed_input, b"", b"outbe/tee/dkg-enc/v1")
            .map_err(|e| e.to_string())?;
        let bls_seed_bytes = hkdf_sha256(&dkg_seed_input, b"", b"outbe/tee/dkg-bls-seed/v1")
            .map_err(|e| e.to_string())?;
        let bls_seed = u64::from_le_bytes(
            bls_seed_bytes[..8]
                .try_into()
                .map_err(|_| "bls seed slice".to_string())?,
        );
        let tee_bls_key = PrivKey::from_seed(bls_seed);

        // Real SGX attestation. Bind the cleartext public keys into the SGX
        // report_data (first 32 bytes = keccak binding) and ask the hardware for
        // a DCAP quote; the measurements are then parsed out of that real quote.
        // Under gramine-direct/bare there is no SGX hardware (observed: the quote
        // pseudo-file is unwritable and keys/ is empty), so the quote is empty and
        // the measurements are zero — explicitly unattested, never fabricated.
        let attest_type = gramine::attestation_type();
        let report_data_b256 =
            Self::report_data_binding(&noise_public, &tribute_offer_public, &attestation_pub);
        let mut report_data_64 = [0u8; 64];
        report_data_64[..32].copy_from_slice(report_data_b256.as_slice());
        let (mrenclave, mrsigner, isv_svn, quote_body) = match gramine::dcap_quote(&report_data_64)
        {
            Ok(quote) => {
                let m = gramine::parse_quote_measurements(&quote)
                    .map_err(|e| format!("parse own quote: {e}"))?;
                (
                    B256::from(m.mrenclave),
                    B256::from(m.mrsigner),
                    m.isv_svn,
                    quote,
                )
            }
            // No DCAP quote. Under gramine-sgx with remote attestation disabled
            // (manifest "none") the hardware is still present, so read the REAL
            // MRENCLAVE/MRSIGNER from a local SGX report — measured + confidential,
            // but NOT remote-attested (quote_body stays empty). Under
            // gramine-direct/bare there is no SGX at all, so this also fails and we
            // report zero measurements, never fabricated.
            Err(_) => match gramine::local_report_measurements(&report_data_64) {
                Ok(m) => (
                    B256::from(m.mrenclave),
                    B256::from(m.mrsigner),
                    m.isv_svn,
                    Vec::new(),
                ),
                Err(_) => (
                    UNATTESTED_MEASUREMENT,
                    UNATTESTED_MEASUREMENT,
                    0u16,
                    Vec::new(),
                ),
            },
        };

        Ok(Self {
            noise_private: keypair.private,
            noise_public,
            tribute_offer_secret,
            tribute_offer_public,
            attestation_signing,
            attestation_pub,
            tee_bls_key,
            dkg_enc_secret,
            mrenclave,
            mrsigner,
            isv_svn,
            attest_type,
            quote_body,
        })
    }

    /// This enclave's TEE threshold-BLS signing key (DKG participant identity).
    pub fn tee_bls_key(&self) -> &PrivKey {
        &self.tee_bls_key
    }

    /// Encoded TEE-BLS public key (the enclave's DKG participant identity bytes).
    pub fn tee_bls_public_bytes(&self) -> Vec<u8> {
        self.tee_bls_key.public_key().encode().to_vec()
    }

    /// This enclave's X25519 share-decryption secret.
    pub fn dkg_enc_secret(&self) -> [u8; 32] {
        self.dkg_enc_secret
    }

    /// This enclave's X25519 share-encryption public key (dealers seal to it).
    pub fn dkg_enc_public(&self) -> [u8; 32] {
        x25519_public(&self.dkg_enc_secret)
    }

    pub fn noise_private(&self) -> &[u8] {
        &self.noise_private
    }
    pub fn noise_public(&self) -> [u8; 32] {
        self.noise_public
    }
    pub fn tribute_offer_public(&self) -> [u8; 32] {
        self.tribute_offer_public
    }
    /// The X25519 secret behind `tribute_offer_public` — the key a keyless enclave
    /// advertises as its `recipient_x25519` (REPORT_DATA-bound, per-enclave). A
    /// key-handoff is sealed to that public, so the newcomer decrypts the handed-off
    /// group signature with this secret.
    pub fn tribute_offer_x25519_secret(&self) -> [u8; 32] {
        self.tribute_offer_secret
    }
    pub fn attestation_pub(&self) -> [u8; 32] {
        self.attestation_pub
    }

    /// Sign `msg` with this enclave's Ed25519 attestation key. Used to
    /// produce the per-offer attestation tag over the offer-attestation preimage;
    /// the host verifies it against [`EnclaveKeys::attestation_pub`].
    pub fn sign_attestation(&self, msg: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer as _;
        self.attestation_signing.sign(msg).to_bytes()
    }
    /// The running enclave's ISV SVN (0 when unattested). Consumed by the
    /// seal/unseal boot path for the anti-rollback floor (plan §"Local
    /// Persistence").
    pub fn isv_svn(&self) -> u16 {
        self.isv_svn
    }

    /// Borrow the (dev) offer decrypt key material for a batch call. The salt is
    /// the fixed protocol constant [`outbe_tee::OFFER_HKDF_SALT`] (clients use the
    /// same value), so the derived key is identical on every validator.
    pub fn tribute_offer_key_material(&self) -> TributeOfferKeyMaterial<'_> {
        TributeOfferKeyMaterial {
            tribute_offer_private_key: &self.tribute_offer_secret,
            salt: &outbe_tee::OFFER_HKDF_SALT,
        }
    }

    /// Offer decrypt key material using an externally-supplied secret (the
    /// DKG-derived offer secret) with the fixed protocol salt
    /// [`outbe_tee::OFFER_HKDF_SALT`] — the same non-secret domain value clients
    /// use, shared by the dev and DKG-derived offer keys alike.
    pub fn tribute_offer_key_material_with<'a>(
        &'a self,
        secret: &'a [u8; 32],
    ) -> TributeOfferKeyMaterial<'a> {
        TributeOfferKeyMaterial {
            tribute_offer_private_key: secret,
            salt: &outbe_tee::OFFER_HKDF_SALT,
        }
    }

    /// `report_data = keccak256(noise_static_pub || recipient_x25519_pub ||
    /// attestation_pub)` — binds the cleartext quote keys to the attestation. The
    /// first 32 bytes of the SGX 64-byte report_data carry this value, so the
    /// host can verify the binding against the value embedded in the real quote.
    pub fn report_data_binding(
        noise_public: &[u8; 32],
        tribute_offer_public: &[u8; 32],
        attestation_pub: &[u8; 32],
    ) -> B256 {
        let mut preimage = Vec::with_capacity(96);
        preimage.extend_from_slice(noise_public);
        preimage.extend_from_slice(tribute_offer_public);
        preimage.extend_from_slice(attestation_pub);
        keccak256(&preimage)
    }

    fn report_data(&self) -> B256 {
        Self::report_data_binding(
            &self.noise_public,
            &self.tribute_offer_public,
            &self.attestation_pub,
        )
    }

    /// The detected attestation environment (hardware vs unattested).
    pub fn attestation_type(&self) -> &AttestationType {
        &self.attest_type
    }

    /// True only when this enclave produced a real SGX hardware quote.
    pub fn is_attested(&self) -> bool {
        !self.quote_body.is_empty()
    }

    /// Build the SGX quote response. The `quote_body` is the real DCAP quote
    /// generated at startup (empty when unattested). `nonce` is unused for
    /// freshness here — the channel's freshness comes from the Noise-IK handshake
    /// that pins the attested static key.
    pub fn quote(&self, _nonce: [u8; 32]) -> EnclaveResponse {
        EnclaveResponse::Quote {
            mrenclave: self.mrenclave,
            mrsigner: self.mrsigner,
            isv_svn: self.isv_svn,
            report_data: self.report_data(),
            recipient_x25519_pub: self.tribute_offer_public,
            attestation_pub: self.attestation_pub,
            noise_static_pub: self.noise_public,
            quote_body: self.quote_body.clone(),
            attestation: self.attest_type.label(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Off SGX hardware (CI / gramine-direct / bare) the enclave MUST run
    /// unattested: empty quote, zero measurements, `is_attested() == false`. It
    /// must never fabricate a quote or measurements (the old mock did exactly
    /// that — `mock-gramine-direct-quote` + `[0xE1;32]`/`[0x51;32]`).
    #[test]
    fn unattested_when_no_sgx_hardware() {
        let keys = EnclaveKeys::new([0x07; 32], Some([0x01; 32])).expect("key init off-hardware");
        assert!(
            !keys.is_attested(),
            "must be unattested without SGX hardware"
        );
        assert_eq!(keys.mrenclave, UNATTESTED_MEASUREMENT);
        assert_eq!(keys.mrsigner, UNATTESTED_MEASUREMENT);
        assert_eq!(keys.isv_svn, 0);
        match keys.quote([0u8; 32]) {
            EnclaveResponse::Quote {
                quote_body,
                mrenclave,
                ..
            } => {
                assert!(quote_body.is_empty(), "no fabricated quote bytes");
                assert_eq!(mrenclave, UNATTESTED_MEASUREMENT);
            }
            other => panic!("expected Quote, got {other:?}"),
        }
    }

    /// `report_data` binds the cleartext public keys regardless of attestation.
    #[test]
    fn report_data_binds_public_keys() {
        let keys = EnclaveKeys::new([0x09; 32], Some([0x02; 32])).expect("key init off-hardware");
        let expect = EnclaveKeys::report_data_binding(
            &keys.noise_public(),
            &keys.tribute_offer_public(),
            &keys.attestation_pub(),
        );
        match keys.quote([0u8; 32]) {
            EnclaveResponse::Quote { report_data, .. } => assert_eq!(report_data, expect),
            other => panic!("expected Quote, got {other:?}"),
        }
    }

    /// Pin the REPORT_DATA preimage byte order. The canonical layout is
    /// `keccak256(noise_static ‖ recipient_x25519 ‖ attestation)`; the host
    /// (`outbe-tee::client::verify_quote`) recomputes it in the SAME order, so a
    /// drift on either side breaks the channel — this test freezes it.
    #[test]
    fn report_data_preimage_order_is_pinned() {
        let noise = [1u8; 32];
        let offer = [2u8; 32];
        let attest = [3u8; 32];
        let got = EnclaveKeys::report_data_binding(&noise, &offer, &attest);

        let mut canonical = Vec::new();
        canonical.extend_from_slice(&noise);
        canonical.extend_from_slice(&offer);
        canonical.extend_from_slice(&attest);
        assert_eq!(
            got,
            keccak256(&canonical),
            "canonical order is noise‖offer‖attest"
        );

        // Any other field order yields a different binding.
        let mut swapped = Vec::new();
        swapped.extend_from_slice(&noise);
        swapped.extend_from_slice(&attest);
        swapped.extend_from_slice(&offer);
        assert_ne!(got, keccak256(&swapped));
    }

    /// The attestation key is a real Ed25519 key, generated fresh per boot
    /// (two enclaves with identical seeds still get different attestation keys),
    /// and its signatures verify against the advertised public key.
    #[test]
    fn attestation_key_is_real_ed25519_and_ephemeral() {
        let k1 = EnclaveKeys::new([0x07; 32], Some([0x01; 32])).expect("k1");
        let k2 = EnclaveKeys::new([0x07; 32], Some([0x01; 32])).expect("k2");
        // Ephemeral per boot: same seeds, different attestation keys.
        assert_ne!(k1.attestation_pub(), k2.attestation_pub());
        assert_ne!(k1.attestation_pub(), [0u8; 32]);

        // A signature verifies against the advertised public key.
        let msg = b"outbe/tee/test-attestation-msg";
        let tag = k1.sign_attestation(msg);
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&k1.attestation_pub()).expect("vk");
        let sig = ed25519_dalek::Signature::from_bytes(&tag);
        vk.verify_strict(msg, &sig)
            .expect("attestation signature verifies");
        // Wrong key must not verify.
        let vk2 = ed25519_dalek::VerifyingKey::from_bytes(&k2.attestation_pub()).expect("vk2");
        assert!(vk2.verify_strict(msg, &sig).is_err());
    }
}
