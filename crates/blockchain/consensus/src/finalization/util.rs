//! Pure helpers shared between the application verify path and the
//! finalization actor.
//!
//! These functions used to live in `crate::application::handler` and
//! moved here in step 17 of the rewards-economy-mailbox migration so
//! the finalization actor can call them without crossing module
//! boundaries the wrong way. The application handler now imports them
//! back from this module.
//!

use std::{collections::BTreeSet, time::Duration};

use alloy_primitives::{Address, B256};
use commonware_codec::Read as _;
use commonware_consensus::{
    simplex::{elector::Config as _, types::Finalization},
    types::{Epoch, Height},
};
use commonware_cryptography::{
    bls12381::primitives::variant::MinSig,
    certificate::{Provider as _, Scheme as _},
};
use commonware_parallel::Sequential;
use outbe_primitives::{
    consensus_metadata::CertifiedParentAccountingMetadata,
    reshare_artifact::{decode_consensus_header_artifact, ConsensusHeaderArtifact},
};

use crate::{
    block::ConsensusBlock,
    committee_provider::CommitteeProvider,
    digest::Digest,
    hybrid::{
        bls_batch_verification_rng, election::HybridElectorConfigProvider, HybridCertificate,
        HybridScheme, HybridSchemeProvider,
    },
};

/// Time budget for finalized-history metadata checks during verify and
/// proposer-side validate-before-include.
pub(crate) const METADATA_CANONICAL_LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);
/// Reporter caps missed-proposer attribution to one byte worth of entries.
const MAX_MISSED_PROPOSERS_IN_METADATA: usize = u8::MAX as usize;

/// Single shared verdict enum for builder-side and verifier-side
/// finalized-parent attestation validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttestationVerdict {
    /// Attestation is absent — valid block, no settlement.
    AcceptNone,
    /// Attestation present and valid; embed (builder) / accept (verifier).
    AcceptValid,
    /// Bitmap / committee / missed-proposers structurally bad.
    RejectStructural,
    /// BLS certificate verification failed under the scoped scheme.
    RejectCertificate,
    /// Canonical identity or canonical missed-proposer calculation failed.
    RejectCanonicalIdentity,
    /// Local canonical information is not available yet.
    TransientUnavailable,
}

impl AttestationVerdict {
    pub fn is_accept(self) -> bool {
        matches!(self, AttestationVerdict::AcceptValid)
    }

    pub fn is_drain(self) -> bool {
        matches!(
            self,
            AttestationVerdict::RejectStructural
                | AttestationVerdict::RejectCertificate
                | AttestationVerdict::RejectCanonicalIdentity
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            AttestationVerdict::AcceptNone => "accept_none",
            AttestationVerdict::AcceptValid => "accept_valid",
            AttestationVerdict::RejectStructural => "reject_structural",
            AttestationVerdict::RejectCertificate => "reject_certificate",
            AttestationVerdict::RejectCanonicalIdentity => "reject_canonical_identity",
            AttestationVerdict::TransientUnavailable => "transient_unavailable",
        }
    }
}

pub struct AttestationValidationContext<'a> {
    pub certificate_scheme_provider: &'a HybridSchemeProvider<MinSig>,
    pub elector_config_provider: &'a HybridElectorConfigProvider<MinSig>,
    pub committee_provider: &'a CommitteeProvider,
    pub marshal_mailbox: &'a crate::marshal_types::MarshalMailbox,
    pub proposed_block_number: u64,
}

/// Result of replay classification for a finalization event.
#[derive(Debug, PartialEq)]
pub enum ReplayClassification {
    /// Block number > last finalized — genuinely new finalization.
    New,
    /// Block number < last finalized — historical journal replay, drop.
    HistoricalReplay,
    /// Same (number, hash) as current finalized — duplicate, drop.
    DuplicateReplay,
    /// Same number but different hash — fatal chain inconsistency.
    FatalInconsistency,
}

/// Classify a finalization event as new, replayed, or inconsistent.
pub fn classify_finalization(
    block_number: u64,
    digest: B256,
    last_finalized_number: u64,
    finalized_head_hash: B256,
) -> ReplayClassification {
    if block_number < last_finalized_number {
        ReplayClassification::HistoricalReplay
    } else if block_number == last_finalized_number && digest == finalized_head_hash {
        ReplayClassification::DuplicateReplay
    } else if block_number == last_finalized_number && digest != finalized_head_hash {
        ReplayClassification::FatalInconsistency
    } else {
        ReplayClassification::New
    }
}

/// Reason a `retry_with_backoff` attempt failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryFailureKind {
    /// Resolver returned `Err(())`.
    Unavailable,
    /// Resolver future did not complete within the per-attempt timeout.
    Timeout,
}

/// Outcome of `retry_with_backoff` after exhaustion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryFailure {
    pub attempts: u32,
    pub last_kind: RetryFailureKind,
}

/// Retry an async fallible operation with bounded attempts and a per-attempt timeout.
///
/// Returns `Ok(T)` on the first successful attempt. After `max_retries` failures
/// (`Err` or attempt-timeout), returns `Err(RetryFailure { attempts, last_kind })`.
/// A never-completing future counts as one failed attempt — it cannot wedge the caller.
pub async fn retry_with_backoff<T, F, Fut>(
    clock: &impl commonware_runtime::Clock,
    mut resolve: F,
    max_retries: u32,
    delay: Duration,
    per_attempt_timeout: Duration,
) -> Result<T, RetryFailure>
where
    F: FnMut() -> Fut,
    // `Clock::timeout` requires a `Send + 'static` future; every production and
    // test caller supplies an owned `async move` resolver, so the bound holds.
    Fut: std::future::Future<Output = Result<T, ()>> + Send + 'static,
    T: Send + 'static,
{
    let mut attempts = 0u32;
    loop {
        // `Clock::timeout` returns `Err(commonware_runtime::Error::Timeout)` on
        // expiry; the inner `Ok`/`Err(())` is the resolver's own output, unchanged
        // from the previous timeout-based implementation.
        let last_kind = match clock.timeout(per_attempt_timeout, resolve()).await {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(())) => RetryFailureKind::Unavailable,
            Err(_) => RetryFailureKind::Timeout,
        };
        attempts += 1;
        if attempts >= max_retries {
            return Err(RetryFailure {
                attempts,
                last_kind,
            });
        }
        clock.sleep(delay).await;
    }
}

/// Extract the (optional) consensus header artifact from a block's
/// `extra_data`. Used by both the verify path and the finalization
/// actor to surface DKG boundary / dealer-log payloads.
pub fn extract_header_artifact_from_block(
    block: &ConsensusBlock,
) -> Result<Option<ConsensusHeaderArtifact>, String> {
    let raw_block = block.clone().into_inner().into_block();
    decode_consensus_header_artifact(raw_block.header.inner.extra_data.as_ref())
        .map_err(|error| error.to_string())
}

// `validate_finalized_parent_attestation` was the V1
// async certificate-validation predicate used by the proposer-side
// exact-parent wait and by `handle_verify`. Both call sites are removed
// (the proposer reads the proof store directly; `handle_verify`
// is narrowed to structural checks only per). The function is
// deleted to prevent accidental reintroduction of the BLS-on-verify path.
//
// `validate_consensus_metadata_for_verify` below is retained ONLY as a
// test fixture for legacy `handler_tests.rs` cases that pre-date
// — it MUST NOT be called from production runtime paths. 's
// V2 verifier reads `outbe-consensus-proof::verify_v2_proof` instead.

pub async fn validate_consensus_metadata_for_verify(
    clock: &impl commonware_runtime::Clock,
    actual: Option<&CertifiedParentAccountingMetadata>,
    ctx: &AttestationValidationContext<'_>,
) -> AttestationVerdict {
    let Some(actual) = actual else {
        return AttestationVerdict::AcceptNone;
    };

    let certificate_verdict = validate_consensus_metadata(
        Some(actual),
        ctx.certificate_scheme_provider,
        ctx.committee_provider,
    );
    if certificate_verdict != AttestationVerdict::AcceptValid {
        return certificate_verdict;
    }

    if actual.finalized_block_number >= ctx.proposed_block_number {
        return AttestationVerdict::RejectStructural;
    }

    let epoch = Epoch::new(actual.finalized_epoch);
    let Some(expected_committee) = ctx.committee_provider.ordered_committee(epoch) else {
        return AttestationVerdict::RejectStructural;
    };
    let Some(scheme) = ctx.certificate_scheme_provider.scoped(epoch) else {
        return AttestationVerdict::RejectCertificate;
    };

    let digest = Digest(actual.finalized_block_hash);
    // The marshal lookup future borrows `&digest`, so it is not `'static` and
    // cannot use `Clock::timeout`. Inline the same race `Clock::timeout` uses:
    // a biased select between the lookup and a runtime-agnostic sleep.
    let info_lookup = ctx.marshal_mailbox.get_info(&digest);
    let timeout = clock.sleep(METADATA_CANONICAL_LOOKUP_TIMEOUT);
    let mut info_lookup = std::pin::pin!(info_lookup);
    let mut timeout = std::pin::pin!(timeout);
    let lookup = commonware_macros::select! {
        result = &mut info_lookup => Some(result),
        _ = &mut timeout => None,
    };
    let canonical_identity = match lookup {
        Some(Some((height, canonical_digest))) => {
            height == Height::new(actual.finalized_block_number) && canonical_digest == digest
        }
        Some(None) | None => return AttestationVerdict::TransientUnavailable,
    };
    if !canonical_identity {
        return AttestationVerdict::RejectCanonicalIdentity;
    }

    match validate_canonical_missed_proposers(
        clock,
        actual,
        scheme.as_ref(),
        ctx.elector_config_provider,
        expected_committee.as_ref(),
        ctx.marshal_mailbox,
    )
    .await
    {
        Ok(true) => AttestationVerdict::AcceptValid,
        Ok(false) => AttestationVerdict::RejectCanonicalIdentity,
        Err(verdict) => verdict,
    }
}

pub(crate) fn validate_consensus_metadata(
    actual: Option<&CertifiedParentAccountingMetadata>,
    certificate_scheme_provider: &HybridSchemeProvider<MinSig>,
    committee_provider: &CommitteeProvider,
) -> AttestationVerdict {
    let Some(actual) = actual else {
        return AttestationVerdict::AcceptNone;
    };

    if actual.finalized_block_number == 0 || actual.finalized_block_hash == B256::ZERO {
        return AttestationVerdict::RejectStructural;
    }

    let epoch = Epoch::new(actual.finalized_epoch);
    let Some(expected_committee) = committee_provider.ordered_committee(epoch) else {
        return AttestationVerdict::RejectStructural;
    };
    if expected_committee.as_ref() != &actual.ordered_committee {
        return AttestationVerdict::RejectStructural;
    }
    if actual.signer_bitmap.len() != expected_committee.len() {
        return AttestationVerdict::RejectStructural;
    }
    if actual.signer_bitmap.iter().any(|byte| *byte > 1) {
        return AttestationVerdict::RejectStructural;
    }
    let committee_set: BTreeSet<_> = expected_committee.iter().copied().collect();
    // V2 contract requires `missed_proposers` to be empty; if any
    // event is present, it must reference a committee member (defensive
    // structural check — the V2 verifier enforces emptiness upstream).
    if actual
        .missed_proposers
        .iter()
        .any(|ev| !committee_set.contains(&ev.validator))
    {
        return AttestationVerdict::RejectStructural;
    }

    let mut proof_reader = actual.proof.as_ref();
    let Ok(finalization) = Finalization::<HybridScheme<MinSig>, Digest>::read_cfg(
        &mut proof_reader,
        &expected_committee.len(),
    ) else {
        return AttestationVerdict::RejectCertificate;
    };
    if !proof_reader.is_empty() {
        return AttestationVerdict::RejectCertificate;
    }

    let Some(scheme) = certificate_scheme_provider.scoped(epoch) else {
        return AttestationVerdict::RejectCertificate;
    };

    let proposal = &finalization.proposal;
    if proposal.round.epoch() != epoch
        || proposal.round.view().get() != actual.finalized_view
        || proposal.parent.get() != actual.parent_view
        || proposal.payload.0 != actual.finalized_block_hash
    {
        return AttestationVerdict::RejectStructural;
    }

    let mut rng = bls_batch_verification_rng();
    if !finalization.verify(&mut rng, scheme.as_ref(), &Sequential) {
        return AttestationVerdict::RejectCertificate;
    }

    // V2 signer bitmap is the certificate's own bitmap — no
    // supplemental finalize-vote reconciliation. The V1
    // `build_signer_bitmap_with_finalize_votes` helper is dropped.
    let expected_bitmap = build_signer_bitmap(&finalization.certificate, expected_committee.len());
    if expected_bitmap == actual.signer_bitmap {
        AttestationVerdict::AcceptValid
    } else {
        AttestationVerdict::RejectCertificate
    }
}

async fn validate_canonical_missed_proposers(
    clock: &impl commonware_runtime::Clock,
    actual: &CertifiedParentAccountingMetadata,
    scheme: &HybridScheme<MinSig>,
    elector_config_provider: &HybridElectorConfigProvider<MinSig>,
    expected_committee: &[Address],
    marshal_mailbox: &crate::marshal_types::MarshalMailbox,
) -> Result<bool, AttestationVerdict> {
    if actual.finalized_view <= actual.parent_view.saturating_add(1) || actual.parent_view == 0 {
        return Ok(actual.missed_proposers.is_empty());
    }

    let previous_finalization = if actual.finalized_block_number <= 1 {
        None
    } else {
        // Borrowing future ⇒ biased select instead of `Clock::timeout`.
        let lookup =
            marshal_mailbox.get_finalization(Height::new(actual.finalized_block_number - 1));
        let timeout = clock.sleep(METADATA_CANONICAL_LOOKUP_TIMEOUT);
        let mut lookup = std::pin::pin!(lookup);
        let mut timeout = std::pin::pin!(timeout);
        let result = commonware_macros::select! {
            result = &mut lookup => Some(result),
            _ = &mut timeout => None,
        };
        match result {
            Some(Some(finalization)) => Some(finalization),
            Some(None) | None => return Err(AttestationVerdict::TransientUnavailable),
        }
    };

    let Some(expected) = canonical_missed_proposers(
        actual,
        previous_finalization.as_ref(),
        scheme,
        elector_config_provider,
        expected_committee,
    ) else {
        return Ok(false);
    };

    // compare V2 event list (`Vec<MissedProposerEvent>`) against
    // the canonical-derivation `Vec<Address>` — equality holds when (a) both
    // are empty (the V2 contract) or (b) the event sequence's `.validator`
    // chain matches the expected address sequence.
    let actual_addrs: Vec<Address> = actual
        .missed_proposers
        .iter()
        .map(|ev| ev.validator)
        .collect();
    Ok(actual_addrs == expected)
}

fn canonical_missed_proposers(
    actual: &CertifiedParentAccountingMetadata,
    previous_finalization: Option<&crate::marshal_types::Finalization>,
    scheme: &HybridScheme<MinSig>,
    elector_config_provider: &HybridElectorConfigProvider<MinSig>,
    expected_committee: &[Address],
) -> Option<Vec<Address>> {
    let epoch = Epoch::new(actual.finalized_epoch);
    let parent_view = actual.parent_view;
    let current_view = actual.finalized_view;

    if current_view <= parent_view.saturating_add(1) || parent_view == 0 {
        return Some(Vec::new());
    }

    let previous = previous_finalization?;
    let previous_round = previous.proposal.round;
    if previous_round.epoch() > epoch {
        return None;
    }

    if previous_round.epoch() < epoch {
        return Some(Vec::new());
    }

    if previous.proposal.round.view().get() != parent_view {
        return None;
    }

    let participants = scheme.participants();
    if participants.is_empty() || participants.len() != expected_committee.len() {
        return None;
    }
    let elector_config = elector_config_provider.scoped(epoch)?;
    let elector = elector_config.as_ref().clone().build(participants);

    // Shared single source of truth with the proposer-side reporter path: the
    // election sequence must match exactly or this recompute would reject a
    // valid proposer's `missed_proposers` list.
    let leaders = crate::missed_proposers::elected_leaders_for_gap(
        epoch,
        &elector,
        Some(&previous.certificate),
        parent_view,
        current_view,
        MAX_MISSED_PROPOSERS_IN_METADATA,
    );
    let mut missed = Vec::with_capacity(leaders.len());
    for leader in &leaders {
        let leader_idx = leader.get() as usize;
        let address = expected_committee.get(leader_idx)?;
        missed.push(*address);
    }

    Some(missed)
}

pub(crate) fn build_signer_bitmap(
    certificate: &HybridCertificate<MinSig>,
    committee_len: usize,
) -> Vec<u8> {
    let mut bitmap = vec![0u8; committee_len];
    for signer in certificate.signers.iter() {
        let idx = signer.get() as usize;
        if idx < committee_len {
            bitmap[idx] = 1;
        }
    }
    bitmap
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_finalization_new() {
        assert_eq!(
            classify_finalization(101, B256::repeat_byte(0xBB), 100, B256::repeat_byte(0xAA)),
            ReplayClassification::New,
        );
    }

    #[test]
    fn classify_finalization_historical_replay() {
        assert_eq!(
            classify_finalization(50, B256::repeat_byte(0xBB), 100, B256::repeat_byte(0xAA)),
            ReplayClassification::HistoricalReplay,
        );
    }

    #[test]
    fn classify_finalization_duplicate_replay() {
        let hash = B256::repeat_byte(0xAA);
        assert_eq!(
            classify_finalization(100, hash, 100, hash),
            ReplayClassification::DuplicateReplay,
        );
    }

    #[test]
    fn classify_finalization_fatal_inconsistency() {
        assert_eq!(
            classify_finalization(100, B256::repeat_byte(0xBB), 100, B256::repeat_byte(0xAA)),
            ReplayClassification::FatalInconsistency,
        );
    }

    // the finalization actor's recovery loop calls `retry_with_backoff` per
    // cycle and, on exhaustion, records the stall metric and retries the NEXT
    // cycle instead of returning a node-fatal error (actor.rs `handle_finalized`).
    // The loop can only EXIT via `Ok(..) => process_finalization(..)`, so a
    // persistent failure can never return the fatal error — it parks and retries.
    // These tests pin the building block the loop depends on: exhaustion after
    // exactly `max_retries` and recovery when the resolver clears mid-cycle.

    #[test]
    fn retry_with_backoff_exhausts_after_max_retries_on_persistent_failure() {
        use commonware_runtime::Runner as _;
        use std::sync::{
            atomic::{AtomicU32, Ordering},
            Arc,
        };
        commonware_runtime::deterministic::Runner::timed(Duration::from_secs(600)).start(
            |context| async move {
                let calls = Arc::new(AtomicU32::new(0));
                let calls_resolver = calls.clone();
                let resolve = move || {
                    let calls = calls_resolver.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Err::<(), ()>(())
                    }
                };
                let failure = retry_with_backoff(
                    &context,
                    resolve,
                    3,
                    Duration::from_millis(50),
                    Duration::from_millis(100),
                )
                .await
                .expect_err("persistent failure must exhaust the retry budget");
                assert_eq!(failure.attempts, 3, "exactly max_retries attempts");
                assert_eq!(failure.last_kind, RetryFailureKind::Unavailable);
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    3,
                    "resolver invoked exactly max_retries times before Err"
                );
            },
        );
    }

    #[test]
    fn retry_with_backoff_recovers_when_resolver_succeeds_before_exhaustion() {
        use commonware_runtime::Runner as _;
        use std::sync::{
            atomic::{AtomicU32, Ordering},
            Arc,
        };
        commonware_runtime::deterministic::Runner::timed(Duration::from_secs(600)).start(
            |context| async move {
                // Fail the first two attempts, succeed on the third — models a
                // transient all-peers stall that clears mid-cycle: the loop
                // resolves and advances rather than staying parked.
                let calls = Arc::new(AtomicU32::new(0));
                let calls_resolver = calls.clone();
                let resolve = move || {
                    let calls = calls_resolver.clone();
                    async move {
                        let n = calls.fetch_add(1, Ordering::SeqCst);
                        if n < 2 {
                            Err::<u64, ()>(())
                        } else {
                            Ok(42)
                        }
                    }
                };
                let value = retry_with_backoff(
                    &context,
                    resolve,
                    5,
                    Duration::from_millis(50),
                    Duration::from_millis(100),
                )
                .await
                .expect("resolver recovering before the budget must yield Ok");
                assert_eq!(value, 42);
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    3,
                    "stopped retrying as soon as the resolver succeeded"
                );
            },
        );
    }
}
