//! V2 self-contained Hybrid certificate verifier.
//!
//! Pure metadata, committee and certificate verification for executor and import callers.
//! No actor mailbox, DKG runtime or clock is needed.
//!
//! The metadata-bound entry point rejects missed proposers first, binds the exact
//! parent and ordered committee, checks VRF material and proof identity, verifies
//! the aggregate vote and threshold VRF, and reconciles the reconstructed bitmap.
//! The low-level entry point verifies a decoded Hybrid certificate independently
//! of the proposer metadata envelope. Both share the same cryptographic checker.

use super::error::V2VerifyError;
use super::hybrid_wire::{HybridCertificate, VrfProof};
use crate::proof::committee_keys::decode_committee_participants;
use crate::proof::CommitteeSnapshot;
use alloy_primitives::{keccak256, B256};
use bytes::Bytes;
use commonware_codec::{Decode, DecodeExt};
use commonware_cryptography::bls12381::{
    self,
    primitives::variant::{MinSig, Variant},
};
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;

mod bindings;
mod certificate;
mod crypto;
use crypto::verify_v2_certificate_low_level;

// =============================================================================
// Public API surface
// =============================================================================

/// Outcome of a successful V2 proof verification.
///
/// All fields are derived from the decoded certificate; none are read from a
/// `Mutex` or background channel. Callers can copy or persist this without
/// holding any lock.
#[derive(Debug, Clone)]
pub struct VerifiedProof {
    /// Encoded signer bitmap (`1` = signed, `0` = absent), one byte per
    /// participant, in the same order as `snapshot.ordered_committee`.
    pub signer_bitmap: Vec<u8>,
    /// `keccak256(VrfProof::encode())` - canonical fingerprint of the VRF
    /// proof carried in this certificate. See
    /// [`crate::canonical_vrf_proof_hash_v2`].
    pub vrf_proof_hash: B256,
    /// Material version of the verified VRF proof. Used by Rewards/Slash V2
    /// settlement to bind to the active VRF material.
    pub vrf_material_version: u64,
}

// `V2VerifyError` lives in [`crate::error`]. The enum
// was split out so the variant taxonomy is the single source
// of truth for both the verifier and the downstream evidence wrappers.

/// Reason why a vote subject is required by the verifier.
///
/// V2 finalization-bound proofs use [`VoteSubject::Finalize`]; notarization
/// fallback proofs (Activity::Certification) use [`VoteSubject::Notarize`].
#[derive(Debug, Clone, Copy)]
pub enum VoteSubject {
    Notarize,
    Finalize,
}

/// Borrowed view of the active committee snapshot at the proof's epoch.
///
/// Field shapes are minimal on purpose: the verifier is self-contained and
/// must not need anything beyond what is required to verify BLS aggregate
/// vote + threshold VRF. The on-chain persistence lives in
/// `CommitteeSnapshotStore` slots 31..40 in `ValidatorSet`; the verifier
/// only requires this borrowed view.
#[derive(Debug, Clone, Copy)]
pub struct CommitteeSnapshotView<'a> {
    /// Per-participant BLS MinPk identity keys, in the same order as the
    /// committee bitmap (`signer_bitmap[i] == 1` <-> `participants[i]` signed).
    pub participants: &'a [bls12381::PublicKey],
    /// Active VRF group public key (BLS MinSig variant). Threshold partial
    /// signatures recover to a signature under this key.
    pub vrf_group_public_key: <MinSig as Variant>::Public,
    /// Active VRF material version. Used downstream (Rewards/Slash) to bind to
    /// a specific DKG/reshare epoch.
    pub vrf_material_version: u64,
}

/// Vote message bytes required to verify the BLS aggregate.
///
/// In Simplex, the BLS aggregate signs `subject.namespace || subject.message()`.
/// The message is the canonical encoded proposal. Its namespace is derived
/// from the canonical ordered committee using `notarize_namespace` or
/// `finalize_namespace`, depending on `subject`. Low-level callers provide
/// these exact bytes; the metadata-bound entry point derives them internally.
#[derive(Debug, Clone, Copy)]
pub struct VoteBinding<'a> {
    pub subject: VoteSubject,
    /// Domain-separated namespace bytes for this subject and canonical committee.
    pub namespace: &'a [u8],
    /// Canonical encoded vote message (e.g. `Proposal::encode()` bytes).
    pub message: &'a [u8],
    /// Canonical encoded seed message for the threshold-VRF check.
    pub seed_message: &'a [u8],
}

/// Low-level Hybrid certificate verifier.
///
/// Used internally by [`verify_v2_proof`] and retained for the
/// smoke-test fixture that drives the BLS+VRF rules in isolation. Callers
/// implementing the V2 protocol should use the metadata-bound
/// [`verify_v2_proof`] instead - it adds the A4 binding rules
/// (missed_proposers, exact-parent, committee_set_hash, signer-bitmap
/// reconciliation, VRF material/group-key binding) on top of the structural
/// + crypto checks performed here.
///
/// ## Rules verified here
///
/// 1. `proof_bytes` decodes into a `HybridCertificate<MinSig>` against
///    `snapshot.participants.len()` as the upper bound.
/// 2. Signer count meets the simplex `N3f1` quorum.
/// 3. Signer bitmap shape (length, indices `< participants.len()`).
/// 4. BLS MinPk aggregate vote verifies under `binding.namespace`/`binding.message`.
/// 5. A threshold-VRF proof is present and verifies against
///    `snapshot.vrf_group_public_key` and the canonical seed namespace.
pub fn verify_v2_proof_low_level(
    snapshot: &CommitteeSnapshotView<'_>,
    binding: &VoteBinding<'_>,
    proof_bytes: &[u8],
) -> Result<VerifiedProof, V2VerifyError> {
    let cert = HybridCertificate::<MinSig>::decode_cfg(
        Bytes::copy_from_slice(proof_bytes),
        &snapshot.participants.len(),
    )
    .map_err(V2VerifyError::Decode)?;
    // The structural + crypto checks are shared with the metadata-bound path
    // through the private checker; this public entry adds only the wire decode.
    verify_v2_certificate_low_level(snapshot, binding, &cert)
}

/// `N3f1` quorum threshold: `floor(2n/3) + 1`. Matches
/// `commonware_utils::N3f1::quorum(n)` for any `n >= 1`.
pub const fn simplex_n3f1_quorum(n: usize) -> usize {
    (2 * n) / 3 + 1
}

// =============================================================================
// Metadata-bound public verifier
// =============================================================================

/// self-contained V2 verifier. Verifies a Hybrid finalization /
/// certified-notarization certificate against the proposer-claimed
/// [`CertifiedParentAccountingMetadata`], the active [`CommitteeSnapshot`],
/// and the block-header parent hash. Returns the canonical
/// [`VerifiedProof`] on success; otherwise a precise [`V2VerifyError`]
/// matching the violated rule.
///
/// ## Rules verified
///
/// 1. `metadata.missed_proposers` is empty
///    ([`V2VerifyError::NonEmptyMissedProposers`]).
/// 2. Exact-parent binding: `metadata.finalized_block_hash == header_parent_hash`
///    ([`V2VerifyError::WrongAccountedHash`]).
/// 3. Committee shape: `metadata.ordered_committee.len() == snapshot.committee.len()`
///    and per-position `address` matches
///    ([`V2VerifyError::BitmapMismatch`]).
/// 4. Bitmap shape: `metadata.signer_bitmap.len() == metadata.ordered_committee.len()`
///    ([`V2VerifyError::BitmapMismatch`]).
/// 5. VRF material version binding: metadata == snapshot
///    ([`V2VerifyError::WrongVrfMaterialVersion`]).
/// 6. VRF group public key hash: `metadata.vrf_group_public_key_hash ==
///    keccak256(snapshot.vrf_group_public_key_bytes)`
///    ([`V2VerifyError::WrongVrfGroupKeyHash`]).
/// 7. Committee fingerprint: `metadata.committee_set_hash ==
///    committee_set_hash_v2(metadata.finalized_epoch, snapshot)`
///    ([`V2VerifyError::CommitteeSetHashMismatch`]).
/// 8. Proof bytes equal `metadata.proof` byte-identically
///    ([`V2VerifyError::WrongProofDomain`] with the embedded payload hash if
///    the inner proposal differs from `metadata.finalized_block_hash`).
/// 9. Certificate decodes into `HybridCertificate<MinSig>` and passes the
///    BLS aggregate + threshold VRF rules from [`verify_v2_proof_low_level`]
///    under the canonical namespace + seed for
///    `Round(metadata.finalized_epoch, metadata.finalized_view)` and the
///    canonical Simplex `Proposal::encode()` vote message.
///
/// ## Determinism
///
/// `verify_v2_proof` is a synchronous pure function. It does not read
/// wall-clock time, OS entropy, network state, or any process-local mutable
/// state - same inputs produce the same `Result`, byte-deterministically
/// within the installed chain namespace (proptest
/// `verifier_outcome_deterministic_from_parent_state_and_body`).
pub fn verify_v2_proof(
    metadata: &CertifiedParentAccountingMetadata,
    snapshot: &CommitteeSnapshot,
    proof_bytes: &[u8],
    header_parent_hash: B256,
) -> Result<VerifiedProof, V2VerifyError> {
    bindings::validate_metadata(metadata, snapshot, proof_bytes, header_parent_hash)?;
    // Build the snapshot view + vote binding from metadata.
    let participants = decode_committee_participants(snapshot)?;
    let vrf_group_public_key = decode_min_sig_public(&snapshot.vrf_group_public_key_bytes)?;
    let view = CommitteeSnapshotView {
        participants: &participants,
        vrf_group_public_key,
        vrf_material_version: snapshot.vrf_material_version,
    };

    let inner = certificate::verify(&view, metadata, snapshot.committee.len())?;
    bindings::validate_result(metadata, &inner)?;
    Ok(inner)
}

/// Decode a `MinSig` BLS group public key (G2 element) from raw bytes.
fn decode_min_sig_public(bytes: &[u8]) -> Result<<MinSig as Variant>::Public, V2VerifyError> {
    use commonware_codec::FixedSize;
    if bytes.len() != <MinSig as Variant>::Public::SIZE {
        return Err(V2VerifyError::WrongVrfGroupKeyHash {
            expected: B256::ZERO,
            actual: keccak256(bytes),
        });
    }
    <MinSig as Variant>::Public::decode(Bytes::copy_from_slice(bytes))
        .map_err(V2VerifyError::Decode)
}
