//! Wire-codec types for the Outbe Hybrid certificate.
//!
//! These types are byte-identical to the legacy declarations that previously
//! lived in `outbe-consensus::hybrid`. The codec format is part of the V2
//! consensus protocol surface: every byte is observed by gossip, block header
//! `extra_data` (via `OutbeBlockArtifacts`), and the marshal archive. Any
//! drift here breaks block-hash determinism and replay.
//!
//! Layout (encoded with `commonware-codec`):
//!
//! * [`VrfProof<V>`] — `material_version: u64` (big-endian) || `V::Signature`.
//! * [`HybridCertificate<V>`] — `Signers` bitmap || aggregated BLS MinPk
//!   signature (96 bytes) || `u8` VRF presence flag (`0` or `1`) ||
//!   optional `VrfProof<V>`.
//!
//! The decoder rejects an empty signer set and any presence flag outside
//! `{0, 1}`.

use bytes::{Buf, BufMut};
use commonware_codec::{Encode, EncodeSize, Error, FixedSize, Read, ReadExt, Write};
use commonware_consensus::{simplex::scheme::bls12381_threshold::vrf::Seed, types::Round};
use commonware_cryptography::bls12381::{
    self,
    primitives::{
        ops::aggregate,
        variant::{MinPk, Variant},
    },
};
use commonware_cryptography::certificate::Signers;

/// Verified threshold VRF proof sidecar for a consensus certificate.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VrfProof<V: Variant> {
    pub material_version: u64,
    pub threshold_signature: V::Signature,
}

impl<V: Variant> Write for VrfProof<V> {
    fn write(&self, writer: &mut impl BufMut) {
        writer.put_u64(self.material_version);
        self.threshold_signature.write(writer);
    }
}

impl<V: Variant> Read for VrfProof<V> {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &()) -> Result<Self, Error> {
        if reader.remaining() < 8 {
            return Err(Error::Invalid("VrfProof", "missing material version"));
        }
        let material_version = reader.get_u64();
        let threshold_signature = V::Signature::read(reader)?;
        Ok(Self {
            material_version,
            threshold_signature,
        })
    }
}

impl<V: Variant> EncodeSize for VrfProof<V> {
    fn encode_size(&self) -> usize {
        8 + V::Signature::SIZE
    }
}

/// Certificate assembled from a quorum of hybrid attestations.
///
/// Contains:
/// - Signer bitmap (who voted)
/// - Single aggregated BLS MinPk vote signature (96 bytes)
/// - Optional recovered BLS MinSig threshold VRF proof
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HybridCertificate<V: Variant> {
    /// Bitmap of participants that signed.
    pub signers: Signers,
    /// Aggregated BLS vote signature from individual MinPk signatures.
    pub bls_aggregated_vote: aggregate::Signature<MinPk>,
    /// Optional recovered and self-verified threshold VRF proof.
    pub vrf_proof: Option<VrfProof<V>>,
}

impl<V: Variant> Write for HybridCertificate<V> {
    fn write(&self, writer: &mut impl BufMut) {
        self.signers.write(writer);
        self.bls_aggregated_vote.write(writer);
        match &self.vrf_proof {
            Some(proof) => {
                writer.put_u8(1);
                proof.write(writer);
            }
            None => writer.put_u8(0),
        }
    }
}

impl<V: Variant> EncodeSize for HybridCertificate<V> {
    fn encode_size(&self) -> usize {
        self.signers.encode_size()
            + aggregate::Signature::<MinPk>::SIZE
            + 1
            + self
                .vrf_proof
                .as_ref()
                .map(EncodeSize::encode_size)
                .unwrap_or(0)
    }
}

impl<V: Variant> Read for HybridCertificate<V> {
    type Cfg = usize;

    fn read_cfg(reader: &mut impl Buf, max_participants: &usize) -> Result<Self, Error> {
        let signers = Signers::read_cfg(reader, max_participants)?;
        if signers.count() == 0 {
            return Err(Error::Invalid(
                "HybridCertificate",
                "certificate contains no signers",
            ));
        }
        let bls_aggregated_vote = aggregate::Signature::<MinPk>::read(reader)?;
        if !reader.has_remaining() {
            return Err(Error::Invalid(
                "HybridCertificate",
                "missing VRF proof presence flag",
            ));
        }
        let vrf_proof = match reader.get_u8() {
            0 => None,
            1 => Some(VrfProof::<V>::read(reader)?),
            _ => {
                return Err(Error::Invalid(
                    "HybridCertificate",
                    "invalid VRF proof presence flag",
                ));
            }
        };

        Ok(Self {
            signers,
            bls_aggregated_vote,
            vrf_proof,
        })
    }
}

impl<V: Variant> HybridCertificate<V> {
    /// Extract the VRF seed from this certificate for a given round.
    pub fn seed(&self, round: Round) -> Option<Seed<V>> {
        self.vrf_proof
            .as_ref()
            .map(|proof| Seed::new(round, proof.threshold_signature))
    }

    /// Encoded raw bytes of the threshold VRF signature for downstream
    /// fingerprinting and degraded leader-selection fallbacks.
    pub fn raw_vrf_seed_bytes(&self) -> Option<Vec<u8>> {
        self.vrf_proof
            .as_ref()
            .map(|proof| proof.threshold_signature.encode().to_vec())
    }
}

// Suppress an unused-import false positive when only the trait method
// `bls12381::Signature::SIZE` is needed transitively for `FixedSize`.
const _: fn() = || {
    let _ = <bls12381::Signature as FixedSize>::SIZE;
};
