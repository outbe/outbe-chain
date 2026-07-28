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

use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    committee::{OcompCommitteeSnapshotV1, RESULT_SIGNATURE_PURPOSE_BITMAP},
    intent::JobIntentV1,
    local_control::EndpointIdentity,
    result::LysisResultV1,
    state::RESULT_VOTE_MIN_FINALITY_DEPTH,
    vote::ResultVoteV1,
    ProtocolError, SchemaLimits,
};
use reth_storage_api::BlockNumReader;

use super::{
    retention::{
        CandidatePinV1, OcompRetentionCoordinator, PinStateV1, RetentionError, RetentionStatus,
    },
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
        let (
            candidate,
            job_id,
            manifest_hash,
            finality_recorded_height,
            open_height,
            deadline_height,
        ) = match self.status() {
            RetentionStatus::Ready(record) => match record.state {
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
            },
            RetentionStatus::Empty => {
                return Err(AttestationAuthorityError::NotExported(requested_job_id));
            }
            RetentionStatus::Quarantined { reason } => {
                return Err(AttestationAuthorityError::Unavailable(reason));
            }
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
    pub validator_index: u8,
}

pub struct OcompAttestationGate {
    authority: Arc<dyn FinalizedAttestationAuthority>,
    height: Arc<dyn CurrentHeightSource>,
    identity: EndpointIdentity,
    validator_index: u8,
    committee: OcompCommitteeSnapshotV1,
    committee_snapshot_hash: B256,
    signer: OcompSigner,
    sign_once: SignOnceStore,
    limits: SchemaLimits,
}

impl OcompAttestationGate {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        authority: Arc<dyn FinalizedAttestationAuthority>,
        height: Arc<dyn CurrentHeightSource>,
        config: OcompAttestationConfig,
        committee: OcompCommitteeSnapshotV1,
        signer: OcompSigner,
        sign_once: SignOnceStore,
        limits: SchemaLimits,
    ) -> Result<Self, AttestationError> {
        committee.validate_semantics(&limits)?;
        if committee.chain_id != config.identity.chain_id
            || committee.genesis_hash != config.identity.genesis_hash
            || committee.protocol_bundle_hash != config.identity.protocol_bundle_hash
        {
            return Err(AttestationError::Binding("configured committee network"));
        }
        let member = committee
            .ordered_members
            .get(usize::from(config.validator_index))
            .ok_or(AttestationError::Binding("configured validator index"))?;
        if member.validator_index != config.validator_index
            || member.ocomp_public_key_sec1 != signer.public_key_sec1()
            || member.key_epoch != signer.key_epoch()
            || member.allowed_purpose_bitmap != RESULT_SIGNATURE_PURPOSE_BITMAP
        {
            return Err(AttestationError::Binding("configured local OCOMP member"));
        }
        let committee_snapshot_hash = committee.snapshot_hash(&limits)?;
        Ok(Self {
            authority,
            height,
            identity: config.identity,
            validator_index: config.validator_index,
            committee,
            committee_snapshot_hash,
            signer,
            sign_once,
            limits,
        })
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
        let member = &self.committee.ordered_members[usize::from(self.validator_index)];
        if member.valid_from_height > current_height
            || current_height >= member.valid_until_height_exclusive
        {
            return Err(AttestationError::LocalMemberInactive { current_height });
        }

        let record = self.sign_once.record_or_replay(
            SignOnceSubjectV1 {
                chain_id: self.identity.chain_id,
                genesis_hash: self.identity.genesis_hash,
                fork_id: intent.fork_id,
                job_id: result.job_id,
                attempt: result.attempt,
                protocol_bundle_hash: result.protocol_bundle_hash,
                committee_snapshot_hash: self.committee_snapshot_hash,
                validator_index: self.validator_index,
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
            result_committee_snapshot_hash: self.committee_snapshot_hash,
            validator_index: self.validator_index,
            key_epoch: self.signer.key_epoch(),
            result,
            signature_rs: record.signature_rs,
        };
        vote.verify(
            &fresh.finalized_intent,
            fresh.job_id,
            &self.committee,
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
            || intent.fork_id != self.committee.fork_id
            || intent.protocol_bundle_hash != self.identity.protocol_bundle_hash
            || intent.result_committee_snapshot_hash != self.committee_snapshot_hash
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
    #[error("local OCOMP committee member is inactive at height {current_height}")]
    LocalMemberInactive { current_height: u64 },
    #[error("node-owned finalized/export authority changed before signing")]
    AuthorityChanged,
    #[error(transparent)]
    Authority(#[from] AttestationAuthorityError),
    #[error(transparent)]
    Height(#[from] HeightSourceError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    SignOnce(#[from] SignOnceError),
    #[error(transparent)]
    Signer(#[from] OcompKeyError),
}
