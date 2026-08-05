//! Independent node attestation gate for one exact OCOMP result.
//!
//! The supervisor supplies canonical `LysisResultV1` bytes, never a digest or
//! signing purpose. The gate reloads node-owned finalized/export authority,
//! validates the constant-size result bindings and equations, reconstructs the
//! `ResultDigest`, and only then enters the durable sign-once module.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use alloy_primitives::{Address, B256, U256};
use metrics::counter;
use outbe_ocomp_protocol::{
    intent::JobIntentV1, local_control::EndpointIdentity, result::LysisResultV1,
    state::RESULT_VOTE_MIN_FINALITY_DEPTH, vote::ResultVoteV1, ProtocolError, SchemaLimits,
};
use outbe_primitives::storage::{
    readonly::{ReadOnlyStorageProvider, StorageReader},
    StorageHandle,
};
use outbe_validatorset::{
    committee_snapshot_key, ocomp_binding_hash_v1, read_ocomp_snapshot_extension_for_binding,
    read_ocomp_snapshot_member_at, OcompSnapshotMemberV1,
};
use reth_provider::StateProviderFactory;
use reth_storage_api::{BlockNumReader, StateProvider};

use super::{
    retention::{CandidatePinV1, OcompRetentionCoordinator, PinStateV1, RetentionError},
    sign_once::{SignOnceError, SignOnceStore, SignOnceSubjectV1},
    signer::{OcompKeyError, OcompSigner},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportedAttestationAuthorityV1 {
    pub candidate: CandidatePinV1,
    pub job_id: B256,
    pub manifest_hash: B256,
    pub finalized_intent: JobIntentV1,
    pub finality_recorded_height: u64,
    pub open_height: u64,
    pub deadline_height: u64,
}

pub trait FinalizedAttestationAuthority: Send + Sync {
    fn reload_exported(
        &self,
        job_id: B256,
        limits: &SchemaLimits,
    ) -> Result<ExportedAttestationAuthorityV1, AttestationAuthorityError>;
}

impl FinalizedAttestationAuthority for OcompRetentionCoordinator {
    fn reload_exported(
        &self,
        requested_job_id: B256,
        limits: &SchemaLimits,
    ) -> Result<ExportedAttestationAuthorityV1, AttestationAuthorityError> {
        let record = match self.exported_job_record(requested_job_id) {
            Ok(record) => record,
            Err(RetentionError::Quarantined(reason)) => {
                return Err(AttestationAuthorityError::Unavailable(reason));
            }
            Err(RetentionError::InvalidTransition(_)) => {
                return Err(AttestationAuthorityError::NotExported(requested_job_id));
            }
            Err(error) => return Err(AttestationAuthorityError::Retention(error)),
        };
        let (
            candidate,
            job_id,
            manifest_hash,
            finality_recorded_height,
            open_height,
            deadline_height,
        ) = match record.state {
            PinStateV1::Exported {
                candidate,
                job_id,
                manifest_hash,
                finality_recorded_height,
                open_height,
                deadline_height,
                ..
            } if job_id == requested_job_id => (
                candidate,
                job_id,
                manifest_hash,
                finality_recorded_height,
                open_height,
                deadline_height,
            ),
            _ => return Err(AttestationAuthorityError::NotExported(requested_job_id)),
        };
        let proof = self.build_finalized_intent_proof(job_id)?;
        let finalized_intent = proof
            .decoded_intent(limits)
            .map_err(AttestationAuthorityError::Protocol)?;
        Ok(ExportedAttestationAuthorityV1 {
            candidate,
            job_id,
            manifest_hash,
            finalized_intent,
            finality_recorded_height,
            open_height,
            deadline_height,
        })
    }
}

pub trait CurrentHeightSource: Send + Sync {
    fn current_height(&self) -> Result<u64, HeightSourceError>;
}

/// Exact OCOMP material attached to one historical consensus ValidatorSet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoricalOcompSnapshotV1 {
    pub epoch: u64,
    pub committee_set_hash: B256,
    pub ocomp_binding_hash: B256,
    pub ordered_members: Vec<OcompSnapshotMemberV1>,
}

/// Loads one historical OCOMP extension retained in the current canonical
/// ValidatorSet ring. The three finalized-intent bindings select the exact
/// snapshot; implementations return `None` for missing, evicted or mismatched
/// state and never substitute the current membership.
pub trait HistoricalOcompSnapshotSource: Send + Sync {
    fn load_snapshot(
        &self,
        epoch: u64,
        committee_set_hash: B256,
        ocomp_binding_hash: B256,
    ) -> Result<Option<HistoricalOcompSnapshotV1>, SnapshotSourceError>;
}

#[derive(Clone, Debug)]
pub struct ProviderHistoricalOcompSnapshotSource<P> {
    provider: P,
}

impl<P> ProviderHistoricalOcompSnapshotSource<P> {
    #[must_use]
    pub const fn new(provider: P) -> Self {
        Self { provider }
    }
}

struct RethSnapshotStorageReader<'a> {
    state: &'a dyn StateProvider,
}

impl StorageReader for RethSnapshotStorageReader<'_> {
    fn read_storage(&self, address: Address, key: B256) -> outbe_primitives::error::Result<U256> {
        self.state
            .storage(address, key)
            .map(|value| value.unwrap_or(U256::ZERO))
            .map_err(|error| {
                outbe_primitives::error::PrecompileError::Storage(format!(
                    "historical OCOMP snapshot read failed: {error}"
                ))
            })
    }
}

impl<P> HistoricalOcompSnapshotSource for ProviderHistoricalOcompSnapshotSource<P>
where
    P: StateProviderFactory + Send + Sync,
{
    fn load_snapshot(
        &self,
        epoch: u64,
        committee_set_hash: B256,
        ocomp_binding_hash: B256,
    ) -> Result<Option<HistoricalOcompSnapshotV1>, SnapshotSourceError> {
        let state = self
            .provider
            .latest()
            .map_err(|error| SnapshotSourceError(error.to_string()))?;
        let reader = RethSnapshotStorageReader {
            state: state.as_ref(),
        };
        let mut provider = ReadOnlyStorageProvider::new(reader);
        let storage = StorageHandle::new(&mut provider);
        let Some(extension) = read_ocomp_snapshot_extension_for_binding(
            storage.clone(),
            epoch,
            committee_set_hash,
            ocomp_binding_hash,
        )
        .map_err(|error| SnapshotSourceError(error.to_string()))?
        else {
            return Ok(None);
        };
        let snapshot_key = committee_snapshot_key(epoch, committee_set_hash);
        let mut ordered_members = Vec::with_capacity(usize::from(extension.member_count));
        for index in 0..extension.member_count {
            let Some(member) = read_ocomp_snapshot_member_at(storage.clone(), snapshot_key, index)
                .map_err(|error| SnapshotSourceError(error.to_string()))?
            else {
                return Ok(None);
            };
            ordered_members.push(member);
        }
        if ocomp_binding_hash_v1(epoch, committee_set_hash, &ordered_members) != ocomp_binding_hash
        {
            return Ok(None);
        }
        Ok(Some(HistoricalOcompSnapshotV1 {
            epoch,
            committee_set_hash,
            ocomp_binding_hash,
            ordered_members,
        }))
    }
}

#[derive(Debug)]
pub struct AtomicHeightSource {
    height: AtomicU64,
}

impl AtomicHeightSource {
    #[must_use]
    pub const fn new(height: u64) -> Self {
        Self {
            height: AtomicU64::new(height),
        }
    }

    pub fn advance_to(&self, height: u64) {
        self.height.fetch_max(height, Ordering::SeqCst);
    }
}

impl CurrentHeightSource for AtomicHeightSource {
    fn current_height(&self) -> Result<u64, HeightSourceError> {
        Ok(self.height.load(Ordering::SeqCst))
    }
}

/// Production canonical-height source backed by the node provider.
#[derive(Clone, Debug)]
pub struct ProviderHeightSource<P> {
    provider: P,
}

impl<P> ProviderHeightSource<P> {
    #[must_use]
    pub const fn new(provider: P) -> Self {
        Self { provider }
    }
}

impl<P> CurrentHeightSource for ProviderHeightSource<P>
where
    P: BlockNumReader + Send + Sync,
{
    fn current_height(&self) -> Result<u64, HeightSourceError> {
        self.provider
            .last_block_number()
            .map_err(|error| HeightSourceError(error.to_string()))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct OcompAttestationConfig {
    pub identity: EndpointIdentity,
    pub fork_id: B256,
    pub validator_address: Address,
}

pub struct OcompAttestationGate {
    authority: Arc<dyn FinalizedAttestationAuthority>,
    height: Arc<dyn CurrentHeightSource>,
    snapshots: Arc<dyn HistoricalOcompSnapshotSource>,
    identity: EndpointIdentity,
    fork_id: B256,
    validator_address: Address,
    signer: OcompSigner,
    sign_once: SignOnceStore,
    limits: SchemaLimits,
    abstentions: AtomicU64,
}

impl OcompAttestationGate {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        authority: Arc<dyn FinalizedAttestationAuthority>,
        height: Arc<dyn CurrentHeightSource>,
        snapshots: Arc<dyn HistoricalOcompSnapshotSource>,
        config: OcompAttestationConfig,
        signer: OcompSigner,
        sign_once: SignOnceStore,
        limits: SchemaLimits,
    ) -> Result<Self, AttestationError> {
        if config.fork_id.is_zero() || config.validator_address.is_zero() {
            return Err(AttestationError::Binding("configured validator identity"));
        }
        Ok(Self {
            authority,
            height,
            snapshots,
            identity: config.identity,
            fork_id: config.fork_id,
            validator_address: config.validator_address,
            signer,
            sign_once,
            limits,
            abstentions: AtomicU64::new(0),
        })
    }

    #[cfg(test)]
    pub(crate) fn abstention_count(&self) -> u64 {
        self.abstentions.load(Ordering::Relaxed)
    }

    fn observable_abstention(
        &self,
        reason: &'static str,
        job_id: B256,
        intent: &JobIntentV1,
        error: AttestationError,
    ) -> AttestationError {
        self.abstentions.fetch_add(1, Ordering::Relaxed);
        counter!("outbe_ocomp_attestation_abstentions_total", "reason" => reason).increment(1);
        tracing::error!(
            %job_id,
            result_validator_set_epoch = intent.result_validator_set_epoch,
            result_committee_set_hash = %intent.result_committee_set_hash,
            result_ocomp_binding_hash = %intent.result_ocomp_binding_hash,
            reason,
            "OCOMP node abstains from the exact historical job snapshot"
        );
        error
    }

    pub fn attest_canonical_result(
        &self,
        canonical_result: &[u8],
    ) -> Result<ResultVoteV1, AttestationError> {
        let result = LysisResultV1::decode_canonical(canonical_result, &self.limits)?;
        if result.encode_canonical(&self.limits)? != canonical_result {
            return Err(AttestationError::Binding(
                "canonical LysisResultV1 re-encoding",
            ));
        }
        self.attest(result)
    }

    pub fn attest(&self, result: LysisResultV1) -> Result<ResultVoteV1, AttestationError> {
        result.validate_semantics(&self.limits)?;
        let authority = self
            .authority
            .reload_exported(result.job_id, &self.limits)?;
        self.validate_authority_and_result(&authority, &result)?;
        let intent = &authority.finalized_intent;
        let snapshot = match self.snapshots.load_snapshot(
            intent.result_validator_set_epoch,
            intent.result_committee_set_hash,
            intent.result_ocomp_binding_hash,
        ) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => {
                return Err(self.observable_abstention(
                    "missing_snapshot",
                    result.job_id,
                    intent,
                    AttestationError::HistoricalSnapshotUnavailable,
                ));
            }
            Err(error) => {
                return Err(self.observable_abstention(
                    "snapshot_source",
                    result.job_id,
                    intent,
                    AttestationError::Snapshot(error),
                ));
            }
        };
        if snapshot.epoch != intent.result_validator_set_epoch
            || snapshot.committee_set_hash != intent.result_committee_set_hash
            || snapshot.ocomp_binding_hash != intent.result_ocomp_binding_hash
            || snapshot.ordered_members.len() != usize::from(intent.result_member_count)
        {
            return Err(self.observable_abstention(
                "snapshot_binding",
                result.job_id,
                intent,
                AttestationError::HistoricalSnapshotUnavailable,
            ));
        }
        let Some((validator_index, member)) = snapshot
            .ordered_members
            .iter()
            .enumerate()
            .find(|(_, member)| member.validator_address == self.validator_address)
        else {
            return Err(self.observable_abstention(
                "local_member_absent",
                result.job_id,
                intent,
                AttestationError::LocalMemberAbsent,
            ));
        };
        let validator_index: u16 = validator_index
            .try_into()
            .map_err(|_| AttestationError::Binding("historical validator index"))?;
        if member.ocomp_public_key_sec1 != self.signer.public_key_sec1()
            || member.key_epoch != self.signer.key_epoch()
        {
            return Err(self.observable_abstention(
                "local_key_mismatch",
                result.job_id,
                intent,
                AttestationError::LocalKeyMismatch,
            ));
        }

        let result_digest = result.result_digest(&self.limits)?;

        // Reload immediately before the irreversible local vote. A terminal,
        // reorged or differently exported pin between validation and signing
        // therefore abstains instead of trusting the earlier snapshot.
        let fresh = self
            .authority
            .reload_exported(result.job_id, &self.limits)?;
        if fresh != authority {
            return Err(AttestationError::AuthorityChanged);
        }

        // Read the canonical height only after the final authority reload and
        // immediately before the irreversible sign-once transition. An early
        // validation snapshot must not authorize a signature after the
        // deadline or key-validity interval has advanced.
        let current_height = self.height.current_height()?;
        let intent = &fresh.finalized_intent;
        if current_height < fresh.open_height {
            return Err(AttestationError::VotingNotOpen {
                current_height,
                open_height: fresh.open_height,
            });
        }
        if current_height >= fresh.deadline_height {
            return Err(AttestationError::DeadlineReached {
                current_height,
                deadline_height: fresh.deadline_height,
            });
        }
        let record = self.sign_once.record_or_replay(
            SignOnceSubjectV1 {
                chain_id: self.identity.chain_id,
                genesis_hash: self.identity.genesis_hash,
                fork_id: intent.fork_id,
                job_id: result.job_id,
                attempt: result.attempt,
                protocol_bundle_hash: result.protocol_bundle_hash,
                result_validator_set_epoch: intent.result_validator_set_epoch,
                result_committee_set_hash: intent.result_committee_set_hash,
                result_ocomp_binding_hash: intent.result_ocomp_binding_hash,
                validator_index,
                key_epoch: self.signer.key_epoch(),
                result_digest,
            },
            |digest| {
                self.signer
                    .sign_result_digest(digest)
                    .map_err(|error| error.to_string())
            },
        )?;
        let vote = ResultVoteV1 {
            protocol_bundle_hash: result.protocol_bundle_hash,
            job_id: result.job_id,
            attempt: result.attempt,
            result_validator_set_epoch: intent.result_validator_set_epoch,
            result_committee_set_hash: intent.result_committee_set_hash,
            result_ocomp_binding_hash: intent.result_ocomp_binding_hash,
            validator_index,
            key_epoch: self.signer.key_epoch(),
            result,
            signature_rs: record.signature_rs,
        };
        vote.verify_historical_member(
            &fresh.finalized_intent,
            fresh.job_id,
            snapshot.ordered_members.len() as u16,
            member.key_epoch,
            &member.ocomp_public_key_sec1,
            current_height,
            fresh.open_height,
            fresh.deadline_height,
            &self.limits,
        )?;
        Ok(vote)
    }

    fn validate_authority_and_result(
        &self,
        authority: &ExportedAttestationAuthorityV1,
        result: &LysisResultV1,
    ) -> Result<(), AttestationError> {
        let candidate = authority.candidate;
        let intent = &authority.finalized_intent;
        if authority.job_id != result.job_id
            || candidate.protocol_bundle_hash != self.identity.protocol_bundle_hash
            || result.protocol_bundle_hash != self.identity.protocol_bundle_hash
            || result.input_manifest_hash != authority.manifest_hash
        {
            return Err(AttestationError::Binding("exported result authority"));
        }
        if intent.chain_id != self.identity.chain_id
            || intent.genesis_hash != self.identity.genesis_hash
            || intent.fork_id != self.fork_id
            || intent.protocol_bundle_hash != self.identity.protocol_bundle_hash
        {
            return Err(AttestationError::Binding("finalized intent network"));
        }
        if intent.intent_id(&self.limits)? != candidate.intent_id
            || intent.job_id(candidate.block_hash, candidate.state_root, &self.limits)?
                != authority.job_id
            || intent.wwd != candidate.wwd
            || intent.ce_sealed_root != candidate.ce_sealed_root
            || authority
                .finality_recorded_height
                .checked_add(RESULT_VOTE_MIN_FINALITY_DEPTH)
                != Some(authority.open_height)
            || authority.open_height >= authority.deadline_height
        {
            return Err(AttestationError::Binding("finalized intent pin"));
        }
        result.validate_finalized_intent(intent)?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AttestationAuthorityError {
    #[error("job {0} is not in the node-owned Exported state")]
    NotExported(B256),
    #[error("node-owned finalized/export authority is unavailable: {0}")]
    Unavailable(String),
    #[error(transparent)]
    Retention(#[from] RetentionError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}

#[derive(Debug, thiserror::Error)]
#[error("current-height authority is unavailable: {0}")]
pub struct HeightSourceError(pub String);

#[derive(Debug, thiserror::Error)]
#[error("historical OCOMP snapshot source failed: {0}")]
pub struct SnapshotSourceError(pub String);

#[derive(Debug, thiserror::Error)]
pub enum AttestationError {
    #[error("OCOMP attestation binding failed: {0}")]
    Binding(&'static str),
    #[error(
        "OCOMP attestation voting is not open: current height {current_height}, \
         open height {open_height}"
    )]
    VotingNotOpen {
        current_height: u64,
        open_height: u64,
    },
    #[error(
        "OCOMP attestation deadline reached: current height {current_height}, \
         exclusive deadline {deadline_height}"
    )]
    DeadlineReached {
        current_height: u64,
        deadline_height: u64,
    },
    #[error("the exact historical OCOMP snapshot is unavailable")]
    HistoricalSnapshotUnavailable,
    #[error("the local validator is absent from the exact historical OCOMP snapshot")]
    LocalMemberAbsent,
    #[error("the local OCOMP key does not match the exact historical validator member")]
    LocalKeyMismatch,
    #[error("node-owned finalized/export authority changed before signing")]
    AuthorityChanged,
    #[error(transparent)]
    Authority(#[from] AttestationAuthorityError),
    #[error(transparent)]
    Height(#[from] HeightSourceError),
    #[error(transparent)]
    Snapshot(#[from] SnapshotSourceError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    SignOnce(#[from] SignOnceError),
    #[error(transparent)]
    Signer(#[from] OcompKeyError),
}
