//! Bounded-budget remote fetch for certified-parent notarization proofs.
//!
//! `fetch_parent_proof(key, targets, schedule)` uses
//! [`OutbeProtocolSchedule`] budgets for timeout/attempts/byte cap and only
//! ingests bytes via the `Request::Notarized` byte-recovery channel
//! (monorepo `consensus/src/marshal/core/actor.rs:847`, returns
//! `(notarization, block).encode()`).
//!
//! Marshal-wire integration is 's responsibility. ships:
//!
//! 1. The [`ParentProofTransport`] trait the resolver depends on (concrete
//!    marshal transport plugs in by implementing it).
//! 2. The [`ParentProofResolver`] policy layer: enforces per-attempt timeout,
//!    max attempts, max bytes; rejects hash-mismatch responses
//!    ([`ProofFetchOutcome::NoProofForExactParent`]); gates persistence
//!    on a local certification witness already being present
//! ([`ProofFetchOutcome::NoLocalCertificationWitness`]).
//! 3. The structured outcome enum [`ProofFetchOutcome`] consumed by
//!    's V2 proposer selector.
//!
//! binding: a remote `Request::Notarized` response NEVER produces a
//! `CertifiedParentProofRecord` write unless this node has already locally
//! observed `Activity::Certification` for the same `(epoch, view, block_hash)`
//! (verified by [`CertifiedParentProofStore::get_certified_notarization`]
//! being non-empty for the requested parent hash).

use std::{future::Future, time::Duration};

use crate::proof::{committee_set_hash_v2, CommitteeEntry, CommitteeSnapshot};
use alloy_primitives::{Address, Bytes, B256};
use commonware_codec::Encode;
use commonware_consensus::{
    simplex::types::Notarization, types::Round, Epochable as _, Viewable as _,
};
use commonware_cryptography::{
    bls12381::{self, primitives::variant::MinSig},
    certificate::Scheme as _,
};
use commonware_parallel::Sequential;
use commonware_utils::ordered::Set as OrderedSet;
use outbe_primitives::{
    consensus_metadata::ParentParticipationProof, protocol_schedule::OutbeProtocolSchedule,
};

use crate::{
    digest::Digest,
    finalization::parent_cert_store::{
        CertifiedParentProofKey, CertifiedParentProofRecord, CertifiedParentProofStore,
        FinalizedParentCertStore, ParentProofStoreError,
        CERTIFIED_PARENT_PROOF_RECORD_FORMAT_VERSION,
    },
    hybrid::{bls_batch_verification_rng, HybridScheme},
};

/// Lookup key for a bounded parent-proof fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProofFetchKey {
    pub round: Round,
    pub parent_hash: B256,
}

/// Outcome of a [`ParentProofResolver::fetch_parent_proof`] call.
///
/// Not `PartialEq` because [`ParentProofStoreError`] does not implement it.
/// Tests should use `matches!` against the desired variant.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum ProofFetchOutcome {
    /// Remote response decoded, signature verified, hash matched, and local
    /// certification witness exists. The record has been persisted to the
    /// certified-notarization slot.
    Hit(CertifiedParentProofRecord),
    /// The remote response payload hash did not match the requested
    /// `parent_hash`. exact-key only; the resolver
    /// fails the request without ever writing under the mismatched key.
    NoProofForExactParent,
    /// No local witness for the requested `(epoch, view, parent_hash)` —
    /// forbids producing a record from remote bytes alone.
    NoLocalCertificationWitness,
    /// All budget exhausted (no successful response within
    /// `max_attempts` × per-attempt `timeout_ms`).
    BudgetExhausted,
    /// Notarization signature verification failed on the decoded remote
    /// response.
    VerifyFailed,
    /// Persistence to the certified-parent proof store failed; the underlying
    /// error is reported for caller diagnostics.
    StoreError(ParentProofStoreError),
}

/// Errors that a [`ParentProofTransport`] implementation may surface.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransportError {
    /// Remote peer returned no notarization for the requested round.
    #[error("remote peer returned no notarization for the requested round")]
    NotFound,
    /// Remote response exceeded the byte cap before being fully decoded.
    #[error("remote response exceeded byte cap: {actual} > {cap}")]
    ResponseTooLarge { actual: usize, cap: usize },
    /// Generic transport failure (network, decode, peer-blocked).
    #[error("transport error: {0}")]
    Other(String),
}

/// Transport contract for the bounded fetch path.
///
/// The marshal-backed impl wraps marshal's `Request::Notarized`
/// resolver subscription and decodes the leading `Notarization<S, D>` from the
/// `(notarization, block).encode` wire payload. ships the trait
/// surface and the mock-driven test suite.
///
/// Implementors MUST enforce `byte_cap` on the wire (refuse oversize bytes
/// without buffering); the resolver passes the protocol schedule's
/// `parent_proof_fetch_max_bytes` so a hostile peer cannot consume node
/// resources before the cap is enforced.
pub trait ParentProofTransport: Send + Sync + 'static {
    /// Opaque target identifier (peer id, address, etc.). Resolver does not
    /// interpret it; it only forwards it to `request_notarized`.
    type Target: Clone + Send + Sync + 'static;

    /// Request the canonical notarization for `round` from `target`,
    /// returning the decoded `Notarization` or a [`TransportError`].
    fn request_notarized(
        &self,
        round: Round,
        target: Self::Target,
        byte_cap: usize,
        attempt_timeout: Duration,
    ) -> impl Future<Output = Result<Notarization<HybridScheme<MinSig>, Digest>, TransportError>> + Send;
}

/// Policy layer over a [`ParentProofTransport`] enforcing 's
/// schedule budgets, hash-exact-only contract, and local-witness gate.
pub struct ParentProofResolver<T: ParentProofTransport> {
    transport: T,
    schedule: OutbeProtocolSchedule,
    proof_store: FinalizedParentCertStore,
    verifier_scheme: HybridScheme<MinSig>,
    validator_addresses: Vec<Address>,
}

impl<T: ParentProofTransport> ParentProofResolver<T> {
    pub fn new(
        transport: T,
        schedule: OutbeProtocolSchedule,
        proof_store: FinalizedParentCertStore,
        verifier_scheme: HybridScheme<MinSig>,
        validator_addresses: Vec<Address>,
    ) -> Self {
        Self {
            transport,
            schedule,
            proof_store,
            verifier_scheme,
            validator_addresses,
        }
    }

    /// Try up to `schedule.parent_proof_fetch_max_attempts` targets, applying
    /// the schedule's per-attempt timeout and byte cap. Returns the first
    /// successful, hash-matching, locally-witnessed response — or one of the
    /// structured failure variants on [`ProofFetchOutcome`].
    ///
    /// The resolver is hash-strict: any decoded notarization whose
    /// payload hash differs from `key.parent_hash` aborts the request with
    /// [`ProofFetchOutcome::NoProofForExactParent`] rather than continuing to
    /// the next target, because a peer that supplied wrong bytes for this
    /// round is not going to redeem itself on retry.
    pub async fn fetch_parent_proof(
        &self,
        clock: &impl commonware_runtime::Clock,
        key: ProofFetchKey,
        targets: &[T::Target],
    ) -> ProofFetchOutcome {
        let max_attempts = self.schedule.parent_proof_fetch_max_attempts as usize;
        let attempt_timeout = Duration::from_millis(self.schedule.parent_proof_fetch_timeout_ms);
        let byte_cap = self.schedule.parent_proof_fetch_max_bytes;
        let attempt_budget = max_attempts.min(targets.len());

        for target in targets.iter().take(attempt_budget).cloned() {
            // The transport future borrows `self`/`target`, so it is not `'static`
            // and cannot use `Clock::timeout` (which requires `Send + 'static`).
            // Inline the same race `Clock::timeout`'s default impl uses: a biased
            // select between the request and a runtime-agnostic sleep, preferring
            // the response.
            let request =
                self.transport
                    .request_notarized(key.round, target, byte_cap, attempt_timeout);
            let sleep = clock.sleep(attempt_timeout);
            let mut request = std::pin::pin!(request);
            let mut sleep = std::pin::pin!(sleep);
            let attempt: Result<_, ()> = commonware_macros::select! {
                result = &mut request => Ok(result),
                _ = &mut sleep => Err(()),
            };

            let notarization = match attempt {
                Ok(Ok(n)) => n,
                Ok(Err(_transport_err)) => continue,
                Err(()) => continue,
            };

            // Strict hash check. A peer that returned a
            // different parent for this round has nothing useful to offer on
            // retry; fail immediately so the caller can fall back to
            // proposer-forfeit.
            if notarization.proposal.payload.0 != key.parent_hash {
                return ProofFetchOutcome::NoProofForExactParent;
            }
            // The notarization must be for the requested round. Defence in
            // depth — a transport implementation that did not enforce this is
            // a bug, but we still fail fast rather than persist mismatched
            // round bookkeeping.
            if notarization.proposal.round != key.round {
                return ProofFetchOutcome::NoProofForExactParent;
            }

            // Signature verification — same trust boundary the reporter uses
            // for Activity::Certification (see `reporter.rs::handle_certification`).
            let mut rng = bls_batch_verification_rng();
            if !notarization.verify(&mut rng, &self.verifier_scheme, &Sequential) {
                return ProofFetchOutcome::VerifyFailed;
            }

            // — local witness gate. A record produced from purely
            // remote bytes is never written; the local node must have already
            // observed `Activity::Certification` for the same key. The gate is
            // the in-memory witness index, not the persistent CN record slot:
            // CN rows may be pruned or withheld from proposer selection, while
            // the witness fact remains a separate local observation.
            let proof_key = CertifiedParentProofKey::new(
                key.round.epoch().get(),
                key.round.view().get(),
                key.parent_hash,
            );
            if !self.proof_store.has_local_certification_witness(proof_key) {
                return ProofFetchOutcome::NoLocalCertificationWitness;
            }

            // Build the record and persist. Witness flag is true by the gate
            // above. `committee_set_hash_v2` matches the reporter's canonical
            // fingerprint so 's V2 selector sees a consistent value.
            //
            // PLAN A4 requires the full canonical snapshot (address +
            // 48-byte MinPk pubkey per validator + raw encoded VRF group pk
            // bytes), so the resolver assembles the same snapshot shape the
            // boundary writer (`apply_boundary_outcome`) and Phase 1 verifier
            // recompute.
            let vrf_material_version = self.verifier_scheme.active_vrf_material_version();
            let vrf_group_public_key_bytes: Vec<u8> = self
                .verifier_scheme
                .identity()
                .map(|pk| pk.encode().as_ref().to_vec())
                .unwrap_or_default();
            let snapshot = build_committee_snapshot_from_scheme(
                &self.validator_addresses,
                self.verifier_scheme.participants(),
                vrf_material_version,
                vrf_group_public_key_bytes,
            );
            let committee_set_hash = committee_set_hash_v2(notarization.epoch().get(), &snapshot);
            let encoded_proof: Bytes = notarization.encode().into();
            let view = notarization.view().get();
            // The notarization carries no block-number context. Store `0` so
            // this record can serve as the exact-key local witness, but the
            // Phase 1 selector will not promote it without a real block number.
            let record = CertifiedParentProofRecord {
                format_version: CERTIFIED_PARENT_PROOF_RECORD_FORMAT_VERSION,
                proof_type: ParentParticipationProof::CertifiedNotarization,
                finalized_epoch: notarization.epoch().get(),
                finalized_view: view,
                parent_view: notarization.proposal.parent.get(),
                finalized_block_number: 0,
                finalized_block_hash: notarization.proposal.payload.0,
                committee_set_hash,
                vrf_material_version,
                ordered_committee: self.validator_addresses.clone(),
                signer_bitmap: build_signer_bitmap(
                    &notarization.certificate,
                    self.validator_addresses.len(),
                ),
                certificate: encoded_proof.clone(),
                encoded_proof,
                local_certification_witness: true,
                stored_at_height: view,
                ..CertifiedParentProofRecord::default()
            };

            if let Err(error) = self.proof_store.put_certified_notarization(record.clone()) {
                return ProofFetchOutcome::StoreError(error);
            }
            return ProofFetchOutcome::Hit(record);
        }

        ProofFetchOutcome::BudgetExhausted
    }
}

/// Mirror of `OutbeReporter::build_signer_bitmap` for resolver use. Held
/// locally to avoid making the reporter helper part of the public crate
/// surface; the input contract is identical (1 byte per participant, ones
/// where the certificate signers set carries the participant index).
fn build_signer_bitmap(
    certificate: &crate::hybrid::HybridCertificate<MinSig>,
    committee_size: usize,
) -> Vec<u8> {
    if certificate.signers.len() != committee_size {
        return Vec::new();
    }
    let mut signed = vec![0u8; committee_size];
    for signer in certificate.signers.iter() {
        let idx = signer.get() as usize;
        if idx < committee_size {
            signed[idx] = 1;
        }
    }
    signed
}

/// Same as `reporter::build_committee_snapshot`: zips an ordered address list
/// with the scheme's Commonware-ordered participant pubkeys into a canonical
/// snapshot for [`committee_set_hash_v2`].
///
/// Duplicated here (rather than re-exported from the reporter module) to keep
/// `resolver.rs` self-contained inside `finalization/`; the implementation is
/// trivial and the byte-layout contract lives in
/// `outbe-consensus-proof::committee`.
fn build_committee_snapshot_from_scheme(
    addresses: &[Address],
    participants: &OrderedSet<bls12381::PublicKey>,
    vrf_material_version: u64,
    vrf_group_public_key_bytes: Vec<u8>,
) -> CommitteeSnapshot {
    let committee = addresses
        .iter()
        .zip(participants.iter())
        .map(|(address, pubkey)| {
            let bytes = pubkey.encode();
            let mut consensus_pubkey = [0u8; 48];
            let len = bytes.as_ref().len().min(48);
            consensus_pubkey[..len].copy_from_slice(&bytes.as_ref()[..len]);
            CommitteeEntry {
                address: *address,
                consensus_pubkey,
            }
        })
        .collect();
    CommitteeSnapshot {
        committee,
        vrf_material_version,
        vrf_group_public_key_bytes,
    }
}
