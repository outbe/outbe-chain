//! Verify exact Phase 1 parent evidence and sparse finalization anchors.
//!
//! The canonical Phase 1 decode is shared by proposal branch accounting and by
//! sparse finality catch-up. Callers verify the embedded proof kind under its
//! historical committee before relying on it.

use std::sync::Arc;

use alloy_consensus::Transaction as _;
use alloy_primitives::{Address, Bytes, B256};
use commonware_codec::{Encode as _, Read as _};
use commonware_consensus::{
    simplex::types::Finalization,
    types::{Epoch, Round, View},
};
use commonware_cryptography::{bls12381::primitives::variant::MinSig, Hasher as _, Sha256};
use commonware_parallel::Sequential;
use outbe_primitives::consensus_metadata::{
    CertifiedParentAccountingMetadata, ParentParticipationProof,
};

use crate::block::ConsensusBlock;
use crate::committee_provider::CommitteeProvider;
use crate::digest::Digest;
use crate::errors::{FinalizationError, ParentProofError};
use crate::finalization::committee_prelude::build_committee_prelude;
use crate::finalization::ingress::Finalized;
use crate::finalization::util::build_signer_bitmap_guarded;
use crate::hybrid::{bls_batch_verification_rng, HybridScheme, HybridSchemeProvider};

/// Maximum actual Marshal reads (`get_block` / `get_finalization`) per actor event.
pub(crate) const MAX_MARSHAL_READS_PER_EVENT: usize = 64;
/// Maximum BLS/VRF cryptographic verifications per actor event.
pub(crate) const MAX_VERIFY_CRYPTO_PER_EVENT: usize = 8;
/// Maximum finalized heights published per actor event (`<= MAX_MARSHAL_READS_PER_EVENT`).
pub(crate) const MAX_APPLY_STEPS_PER_EVENT: usize = 16;

pub(crate) fn certified_parent_metadata_from_phase1_system_tx(
    block: &ConsensusBlock,
) -> eyre::Result<Option<CertifiedParentAccountingMetadata>> {
    if block.number() < 2 {
        return Ok(None);
    }

    let raw_block = block.clone().into_inner().into_block();
    let layout = outbe_primitives::system_tx::split_system_layout(&raw_block.body.transactions)
        .map_err(ParentProofError::InvalidSystemTxLayout)?;
    let phase1 = *layout
        .begin
        .first()
        .ok_or_else(|| ParentProofError::MissingPhase1SystemTx(block.number()))?;
    let input = outbe_primitives::system_tx::SystemTx::decode(phase1.input().as_ref())
        .map_err(ParentProofError::DecodePhase1Input)?;
    let outbe_primitives::system_tx::SystemTx::Consensus {
        step_index: outbe_primitives::system_tx::CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
        payload,
    } = input
    else {
        return Err(ParentProofError::UnexpectedBeginOrdinal0.into());
    };

    let metadata = CertifiedParentAccountingMetadata::decode(payload.as_ref())?;
    let reencoded = metadata.encode()?;
    if reencoded.as_ref() != payload.as_ref() {
        return Err(
            outbe_primitives::system_tx::binding::ParentAccountingBindingError::NonCanonical.into(),
        );
    }
    Ok(Some(metadata))
}

/// Verified immediate-child Phase 1 parent witness for an implicit height.
///
/// Preserves the exact `proof_kind` (`Finalization` or `CertifiedNotarization`)
/// and exact `(epoch, view, parent_view)` from the child's verified Phase 1
/// system transaction. A `CertifiedNotarization` is never recast to a
/// `Finalization`.
#[derive(Clone, Debug)]
pub(crate) struct VerifiedChildParentWitness {
    pub(crate) round: Round,
    pub(crate) parent_view: u64,
    pub(crate) proof_kind: ParentParticipationProof,
    pub(crate) prev_randao: B256,
    pub(crate) metadata: CertifiedParentAccountingMetadata,
    /// Decoded genuine `Finalization` when `proof_kind == Finalization`, or
    /// `None` for `CertifiedNotarization`. Shared seam for C2 anchor recovery.
    #[allow(dead_code)]
    pub(crate) finalization: Option<Finalization<HybridScheme<MinSig>, Digest>>,
}

/// Verify `child`'s Phase 1 parent witness against `parent` under the witness
/// epoch's registered historical verifier and ordered committee.
///
/// Returns:
/// - `Ok(Some(witness))` when the child's Phase 1 proof is valid and binds
///   exactly to `parent`.
/// - `Ok(None)` when the witness epoch's verifier scheme or ordered committee
///   is temporarily unavailable (not registered yet).
/// - `Err(_)` when the parent/child link, Phase 1 payload, metadata binding,
///   committee snapshot, or cryptographic proof is invalid.
pub(crate) fn verify_child_parent_witness(
    parent: &ConsensusBlock,
    child: &ConsensusBlock,
    schemes: &HybridSchemeProvider<MinSig>,
    committees: &CommitteeProvider,
) -> eyre::Result<Option<VerifiedChildParentWitness>> {
    let parent_height = parent.number();
    let expected_child_height = parent_height.saturating_add(1);
    if child.number() != expected_child_height || child.parent_hash() != parent.digest().0 {
        return Err(evidence_error(
            parent_height,
            format!(
                "child block {} does not extend parent {} (expected parent_hash={}, actual={})",
                child.number(),
                parent_height,
                parent.digest().0,
                child.parent_hash()
            ),
        )
        .into());
    }

    let Some(metadata) = certified_parent_metadata_from_phase1_system_tx(child).map_err(|err| {
        evidence_error(
            parent_height,
            format!(
                "invalid child Phase 1 parent witness at child {}: {err}",
                child.number()
            ),
        )
    })?
    else {
        return Err(evidence_error(
            parent_height,
            format!(
                "child block {} missing Phase 1 parent witness",
                child.number()
            ),
        )
        .into());
    };

    if metadata.finalized_block_number != parent_height
        || metadata.finalized_block_hash != parent.digest().0
    {
        return Err(evidence_error(
            parent_height,
            format!(
                "child Phase 1 witness identity mismatch: expected=({}, {}), actual=({}, {})",
                parent_height,
                parent.digest().0,
                metadata.finalized_block_number,
                metadata.finalized_block_hash
            ),
        )
        .into());
    }

    let epoch = Epoch::new(metadata.finalized_epoch);
    let (Some(scheme), Some(committee)) =
        (schemes.scoped(epoch), committees.ordered_committee(epoch))
    else {
        return Ok(None);
    };

    let prelude = build_committee_prelude(
        scheme.as_ref(),
        committee.as_ref(),
        metadata.finalized_epoch,
    )
    .map_err(|err| {
        evidence_error(
            parent_height,
            format!(
                "failed to build committee prelude for witness epoch {}: {err}",
                metadata.finalized_epoch
            ),
        )
    })?;

    let verified = outbe_primitives::proof::verify_v2_proof(
        &metadata,
        &prelude.snapshot,
        metadata.proof.as_ref(),
        child.parent_hash(),
    )
    .map_err(|err| {
        evidence_error(
            parent_height,
            format!("child Phase 1 witness verification failed: {err}"),
        )
    })?;

    let finalization = match metadata.proof_kind {
        ParentParticipationProof::Finalization => {
            let mut reader = metadata.proof.as_ref();
            let member_count = committee.len();
            let decoded =
                Finalization::<HybridScheme<MinSig>, Digest>::read_cfg(&mut reader, &member_count)
                    .map_err(|err| {
                        evidence_error(
                            parent_height,
                            format!("failed to decode verified Phase 1 Finalization: {err}"),
                        )
                    })?;
            if !reader.is_empty() {
                return Err(evidence_error(
                    parent_height,
                    "trailing bytes in Phase 1 Finalization proof",
                )
                .into());
            }
            Some(decoded)
        }
        ParentParticipationProof::CertifiedNotarization => None,
    };

    let round = Round::new(epoch, View::new(metadata.finalized_view));
    Ok(Some(VerifiedChildParentWitness {
        round,
        parent_view: metadata.parent_view,
        proof_kind: metadata.proof_kind,
        prev_randao: verified.prev_randao,
        metadata,
        finalization,
    }))
}

/// Authenticated genuine `Finalization` evidence for a single height.
#[derive(Clone, Debug)]
pub(crate) struct VerifiedDirectFinalization {
    pub(crate) round: Round,
    pub(crate) parent_view: u64,
    pub(crate) digest: Digest,
    pub(crate) vrf_seed: Option<B256>,
    pub(crate) ordered_committee: Vec<Address>,
    pub(crate) signer_bitmap: Vec<u8>,
    pub(crate) encoded_certificate: Bytes,
    pub(crate) committee_set_hash: B256,
    pub(crate) vrf_material_version: u64,
    pub(crate) vrf_group_public_key_hash: B256,
}

/// Authenticate a `Finalization` certificate under its historical epoch scheme.
///
/// Returns `Ok(None)` when the epoch's verifier scheme or ordered committee is
/// not registered yet.
pub(crate) fn verify_direct_finalization(
    height: u64,
    finalization: &Finalization<HybridScheme<MinSig>, Digest>,
    schemes: &HybridSchemeProvider<MinSig>,
    committees: &CommitteeProvider,
) -> eyre::Result<Option<VerifiedDirectFinalization>> {
    let round = finalization.proposal.round;
    let epoch = round.epoch();
    let (Some(scheme), Some(committee)) =
        (schemes.scoped(epoch), committees.ordered_committee(epoch))
    else {
        return Ok(None);
    };

    let mut verification_rng = bls_batch_verification_rng();
    if !finalization.verify(&mut verification_rng, scheme.as_ref(), &Sequential) {
        return Err(evidence_error(height, "finalization certificate verification failed").into());
    }

    let bitmap = build_signer_bitmap_guarded(&finalization.certificate, committee.len());
    if bitmap.len() != committee.len() {
        return Err(evidence_error(
            height,
            "certificate signer width differs from registered committee",
        )
        .into());
    }

    let prelude = build_committee_prelude(scheme.as_ref(), committee.as_ref(), epoch.get())
        .map_err(|err| {
            evidence_error(
                height,
                format!(
                    "failed to build committee prelude for finalized epoch {}: {err}",
                    epoch.get()
                ),
            )
        })?;

    let mut vrf_rng = bls_batch_verification_rng();
    let vrf_seed = scheme
        .verified_vrf_seed_for_round(&mut vrf_rng, round, &finalization.certificate, &Sequential)
        .as_ref()
        .map(|seed_bytes| B256::from_slice(Sha256::hash(&[seed_bytes]).as_ref()));

    Ok(Some(VerifiedDirectFinalization {
        round,
        parent_view: finalization.proposal.parent.get(),
        digest: finalization.proposal.payload,
        vrf_seed,
        ordered_committee: committee.as_ref().clone(),
        signer_bitmap: bitmap,
        encoded_certificate: finalization.encode().into(),
        committee_set_hash: prelude.committee_set_hash,
        vrf_material_version: prelude.vrf_material_version,
        vrf_group_public_key_hash: prelude.vrf_group_public_key_hash,
    }))
}

/// Decode and authenticate a live [`Finalized`] event's certificate under its
/// registered epoch scheme/committee. Returns `Ok(None)` when the epoch's
/// verifier or committee is not yet registered.
pub(crate) fn verify_live_finalized_cert(
    height: u64,
    finalized: &Finalized,
    schemes: &HybridSchemeProvider<MinSig>,
    committees: &CommitteeProvider,
) -> eyre::Result<Option<VerifiedDirectFinalization>> {
    let epoch = finalized.round.epoch();
    let (Some(_scheme), Some(committee)) =
        (schemes.scoped(epoch), committees.ordered_committee(epoch))
    else {
        return Ok(None);
    };
    let encoded = finalized
        .consensus_data
        .finalized_certificate
        .encoded_certificate
        .as_ref();
    if encoded.is_empty() {
        return Err(evidence_error(
            height,
            "live descendant finalization missing encoded certificate",
        )
        .into());
    }
    let mut reader = encoded;
    let member_count = committee.len();
    let decoded =
        Finalization::<HybridScheme<MinSig>, Digest>::read_cfg(&mut reader, &member_count)
            .map_err(|err| {
                evidence_error(
                    height,
                    format!("failed to decode live finalization certificate: {err}"),
                )
            })?;
    if !reader.is_empty() {
        return Err(
            evidence_error(height, "trailing bytes in live finalization certificate").into(),
        );
    }
    if decoded.proposal.round != finalized.round
        || decoded.proposal.payload != finalized.digest
        || decoded.proposal.parent.get()
            != finalized.consensus_data.finalized_certificate.parent_view
    {
        return Err(
            evidence_error(height, "live finalization certificate proposal mismatch").into(),
        );
    }
    verify_direct_finalization(height, &decoded, schemes, committees)
}

/// Genesis / `round == None` baseline (`B1(a)`): compare an unregistered epoch
/// against `accepted_epoch.unwrap_or_else(Epoch::zero)`. `epoch > floor_epoch`
/// parks as `AwaitingFutureEpochVerifier`; `epoch <= floor_epoch` is fatal.
pub(crate) fn check_missing_verifier_epoch(
    height: u64,
    epoch: Epoch,
    accepted_epoch: Option<Epoch>,
) -> eyre::Result<PendingBlockage> {
    let floor = accepted_epoch.unwrap_or_else(Epoch::zero);
    if epoch > floor {
        return Ok(PendingBlockage::AwaitingFutureEpochVerifier { height, epoch });
    }
    Err(evidence_error(
        height,
        format!(
            "missing verifier or ordered committee for epoch {}",
            epoch.get()
        ),
    )
    .into())
}

/// Per-event work counters reported by one actor reconciliation turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EventWorkStats {
    pub(crate) marshal_reads: usize,
    pub(crate) crypto_checks: usize,
    pub(crate) applied_steps: usize,
}

/// Shared per-event budget capping actual Marshal reads (`R`), BLS/VRF
/// verifications (`C`), and published heights (`A`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct EventWorkBudget {
    max_reads: usize,
    max_crypto: usize,
    max_applies: usize,
    pub(crate) stats: EventWorkStats,
    pub(crate) marshal_timed_out: bool,
}

impl EventWorkBudget {
    pub(crate) fn new() -> Self {
        Self {
            max_reads: MAX_MARSHAL_READS_PER_EVENT,
            max_crypto: MAX_VERIFY_CRYPTO_PER_EVENT,
            max_applies: MAX_APPLY_STEPS_PER_EVENT,
            stats: EventWorkStats::default(),
            marshal_timed_out: false,
        }
    }

    pub(crate) fn can_read(&self) -> bool {
        self.can_read_count(1)
    }

    pub(crate) fn can_read_count(&self, count: usize) -> bool {
        !self.marshal_timed_out && self.stats.marshal_reads.saturating_add(count) <= self.max_reads
    }

    pub(crate) fn charge_read(&mut self) -> bool {
        if !self.can_read() {
            return false;
        }
        self.stats.marshal_reads += 1;
        true
    }

    pub(crate) fn can_crypto(&self) -> bool {
        self.can_crypto_count(1)
    }

    pub(crate) fn can_crypto_count(&self, count: usize) -> bool {
        self.stats.crypto_checks.saturating_add(count) <= self.max_crypto
    }

    pub(crate) fn charge_crypto(&mut self) -> bool {
        if !self.can_crypto() {
            return false;
        }
        self.stats.crypto_checks += 1;
        true
    }

    pub(crate) fn can_apply(&self) -> bool {
        self.stats.applied_steps < self.max_applies
    }

    pub(crate) fn charge_apply(&mut self) -> bool {
        if !self.can_apply() {
            return false;
        }
        self.stats.applied_steps += 1;
        true
    }
}

/// Why an anchor cannot advance without a registered future verifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingBlockage {
    AwaitingFutureEpochVerifier { height: u64, epoch: Epoch },
}

/// Verified proof metadata for one finalized height.
#[derive(Clone, Debug)]
pub(crate) struct VerifiedStep {
    pub(crate) number: u64,
    pub(crate) digest: Digest,
    pub(crate) proof_kind: ParentParticipationProof,
    pub(crate) round: Round,
    pub(crate) parent_view: u64,
    pub(crate) vrf_seed: Option<B256>,
    pub(crate) proof_bytes: Bytes,
    pub(crate) signer_bitmap: Vec<u8>,
    pub(crate) committee_set_hash: B256,
    pub(crate) vrf_material_version: u64,
    pub(crate) vrf_group_public_key_hash: B256,
    pub(crate) committee: Arc<Vec<Address>>,
}

impl VerifiedStep {
    pub(crate) fn from_direct_f(number: u64, verified: VerifiedDirectFinalization) -> Self {
        let committee = Arc::new(verified.ordered_committee);
        Self {
            number,
            digest: verified.digest,
            proof_kind: ParentParticipationProof::Finalization,
            round: verified.round,
            parent_view: verified.parent_view,
            vrf_seed: verified.vrf_seed,
            proof_bytes: verified.encoded_certificate,
            signer_bitmap: verified.signer_bitmap,
            committee_set_hash: verified.committee_set_hash,
            vrf_material_version: verified.vrf_material_version,
            vrf_group_public_key_hash: verified.vrf_group_public_key_hash,
            committee,
        }
    }

    pub(crate) fn from_child_witness(
        number: u64,
        digest: Digest,
        witness: VerifiedChildParentWitness,
    ) -> Self {
        let committee = Arc::new(witness.metadata.ordered_committee.clone());
        Self {
            number,
            digest,
            proof_kind: witness.proof_kind,
            round: witness.round,
            parent_view: witness.parent_view,
            vrf_seed: Some(witness.prev_randao),
            proof_bytes: witness.metadata.proof,
            signer_bitmap: witness.metadata.signer_bitmap,
            committee_set_hash: witness.metadata.committee_set_hash,
            vrf_material_version: witness.metadata.vrf_material_version,
            vrf_group_public_key_hash: witness.metadata.vrf_group_public_key_hash,
            committee,
        }
    }

    /// Rebuild a `Finalized` event byte-identically for `process_finalization`.
    pub(crate) fn to_finalized(&self) -> Finalized {
        debug_assert_eq!(self.proof_kind, ParentParticipationProof::Finalization);
        let finalized_certificate = outbe_primitives::consensus::FinalizedParentCertificateData {
            epoch: self.round.epoch().get(),
            view: self.round.view().get(),
            parent_view: self.parent_view,
            ordered_committee: self.committee.as_ref().clone(),
            signer_bitmap: self.signer_bitmap.clone(),
            encoded_certificate: self.proof_bytes.clone(),
        };
        Finalized {
            round: self.round,
            digest: self.digest,
            vrf_seed: self.vrf_seed,
            consensus_data: outbe_primitives::consensus::ConsensusData {
                finalized_block_number: self.number,
                finalized_block_hash: self.digest.0,
                finalized_certificate,
                vrf_seed: self.vrf_seed,
                missed_proposers: Vec::new(),
            },
        }
    }

    /// Rebuild a `VerifiedChildParentWitness` byte-identically for `apply_implicit_cn`.
    pub(crate) fn to_cn_witness(&self) -> VerifiedChildParentWitness {
        debug_assert_eq!(
            self.proof_kind,
            ParentParticipationProof::CertifiedNotarization
        );
        VerifiedChildParentWitness {
            round: self.round,
            parent_view: self.parent_view,
            proof_kind: ParentParticipationProof::CertifiedNotarization,
            prev_randao: self.vrf_seed.unwrap_or(B256::ZERO),
            metadata: CertifiedParentAccountingMetadata {
                finalized_block_number: self.number,
                finalized_block_hash: self.digest.0,
                finalized_epoch: self.round.epoch().get(),
                finalized_view: self.round.view().get(),
                parent_view: self.parent_view,
                ordered_committee: self.committee.as_ref().clone(),
                signer_bitmap: self.signer_bitmap.clone(),
                proof: self.proof_bytes.clone(),
                committee_set_hash: self.committee_set_hash,
                vrf_material_version: self.vrf_material_version,
                vrf_group_public_key_hash: self.vrf_group_public_key_hash,
                proof_kind: ParentParticipationProof::CertifiedNotarization,
                missed_proposers: Vec::new(),
            },
            finalization: None,
        }
    }
}

pub(crate) fn evidence_error(height: u64, reason: impl Into<String>) -> FinalizationError {
    FinalizationError::ArchiveReconciliation {
        height,
        reason: reason.into(),
    }
}
