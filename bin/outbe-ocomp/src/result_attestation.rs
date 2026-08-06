//! Local construction of the signed OCOMP vote carried by the public transaction.
//!
//! Finalized job authority comes from the Supervisor's verified public-RPC
//! discovery record. The dedicated OCOMP key signs the inner `ResultVoteV1`;
//! the EVM key signs only the outer transaction.

use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    committee::{
        verify_low_s_prehash, OcompCommitteeSnapshotV1, POC_COMMITTEE_SIZE,
        RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    control::FinalizedJobSpecV1,
    intent::JobIntentV1,
    local_control::EndpointIdentity,
    result::LysisResultV1,
    vote::ResultVoteV1,
    ProtocolError, SchemaLimits,
};
use thiserror::Error;

use crate::{
    result_signer::{OcompKeyError, OcompSigner},
    sign_once::{SignOnceError, SignOnceStore, SignOnceSubjectV1},
};

pub struct LocalResultVoteAttesterV1 {
    identity: EndpointIdentity,
    fork_id: B256,
    validator_index: u8,
    committee: OcompCommitteeSnapshotV1,
    signer: OcompSigner,
    sign_once: SignOnceStore,
    limits: SchemaLimits,
}

impl LocalResultVoteAttesterV1 {
    pub fn new(
        identity: EndpointIdentity,
        fork_id: B256,
        validator_index: u8,
        committee: OcompCommitteeSnapshotV1,
        signer: OcompSigner,
        sign_once: SignOnceStore,
        limits: SchemaLimits,
    ) -> Result<Self, LocalResultAttestationErrorV1> {
        if usize::from(validator_index) >= POC_COMMITTEE_SIZE {
            return Err(LocalResultAttestationErrorV1::Binding(
                "configured validator index",
            ));
        }
        let member = committee
            .ordered_members
            .get(usize::from(validator_index))
            .ok_or(LocalResultAttestationErrorV1::Binding(
                "configured validator committee slot",
            ))?;
        if committee.chain_id != identity.chain_id
            || committee.genesis_hash != identity.genesis_hash
            || committee.fork_id != fork_id
            || committee.protocol_bundle_hash != identity.protocol_bundle_hash
            || member.validator_index != validator_index
            || member.ocomp_public_key_sec1 != signer.public_key_sec1()
            || member.key_epoch != signer.key_epoch()
            || member.allowed_purpose_bitmap & RESULT_SIGNATURE_PURPOSE_BITMAP == 0
        {
            return Err(LocalResultAttestationErrorV1::Binding(
                "configured OCOMP key and installed committee member",
            ));
        }
        Ok(Self {
            identity,
            fork_id,
            validator_index,
            committee,
            signer,
            sign_once,
            limits,
        })
    }

    pub fn attest(
        &self,
        canonical_result: &[u8],
        finalized: &FinalizedJobSpecV1,
        canonical_height: u64,
    ) -> Result<ResultVoteV1, LocalResultAttestationErrorV1> {
        let intent =
            JobIntentV1::decode_canonical(&finalized.canonical_job_intent.0, &self.limits)?;
        let result = LysisResultV1::decode_canonical(canonical_result, &self.limits)?;
        if result.encode_canonical(&self.limits)? != canonical_result {
            return Err(LocalResultAttestationErrorV1::Binding(
                "canonical LysisResultV1 re-encoding",
            ));
        }
        self.validate_finalized_binding(finalized, &intent, &result)?;
        let member = &self.committee.ordered_members[usize::from(self.validator_index)];
        if finalized.summary.open_height >= finalized.summary.deadline_height
            || canonical_height < finalized.summary.open_height
            || canonical_height >= finalized.summary.deadline_height
            || canonical_height < member.valid_from_height
            || canonical_height >= member.valid_until_height_exclusive
        {
            return Err(LocalResultAttestationErrorV1::Binding(
                "canonical voting and committee-member validity window",
            ));
        }
        if self.committee.snapshot_hash(&self.limits)? != intent.result_committee_snapshot_hash {
            return Err(LocalResultAttestationErrorV1::Binding(
                "finalized intent committee snapshot",
            ));
        }

        let result_digest = result.result_digest(&self.limits)?;
        let subject = SignOnceSubjectV1 {
            chain_id: self.identity.chain_id,
            genesis_hash: self.identity.genesis_hash,
            fork_id: self.fork_id,
            job_id: result.job_id,
            attempt: result.attempt,
            protocol_bundle_hash: result.protocol_bundle_hash,
            committee_snapshot_hash: intent.result_committee_snapshot_hash,
            validator_index: self.validator_index,
            key_epoch: self.signer.key_epoch(),
            result_digest,
        };
        let expected_signing_digest = subject.signing_digest()?;
        let record = self.sign_once.record_or_replay(subject, |digest| {
            self.signer
                .sign_result_digest(digest)
                .map_err(|error| error.to_string())
        })?;
        let vote = ResultVoteV1 {
            protocol_bundle_hash: result.protocol_bundle_hash,
            job_id: result.job_id,
            attempt: result.attempt,
            result_committee_snapshot_hash: intent.result_committee_snapshot_hash,
            validator_index: self.validator_index,
            key_epoch: self.signer.key_epoch(),
            result,
            signature_rs: record.signature_rs,
        };
        if vote.signing_digest(&intent, &self.limits)? != expected_signing_digest {
            return Err(LocalResultAttestationErrorV1::Binding(
                "sign-once and ResultVote signing subjects",
            ));
        }
        verify_low_s_prehash(
            &self.signer.public_key_sec1(),
            expected_signing_digest,
            &vote.signature_rs,
        )?;
        Ok(vote)
    }

    fn validate_finalized_binding(
        &self,
        finalized: &FinalizedJobSpecV1,
        intent: &JobIntentV1,
        result: &LysisResultV1,
    ) -> Result<(), LocalResultAttestationErrorV1> {
        intent.validate_semantics()?;
        result.validate_semantics(&self.limits)?;
        result.validate_finalized_intent(intent)?;
        let summary = &finalized.summary;
        if intent.chain_id != self.identity.chain_id
            || intent.genesis_hash != self.identity.genesis_hash
            || intent.fork_id != self.fork_id
            || intent.protocol_bundle_hash != self.identity.protocol_bundle_hash
            || summary.protocol_bundle_hash != self.identity.protocol_bundle_hash
            || summary.intent_id != intent.intent_id(&self.limits)?
            || summary.job_id
                != intent.job_id(
                    summary.finalized_block_hash,
                    summary.finalized_state_root,
                    &self.limits,
                )?
            || result.job_id != summary.job_id
        {
            return Err(LocalResultAttestationErrorV1::Binding(
                "finalized job, network, and result",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum LocalResultAttestationErrorV1 {
    #[error("OCOMP result attestation binding failed: {0}")]
    Binding(&'static str),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    SignOnce(#[from] SignOnceError),
    #[error(transparent)]
    Signer(#[from] OcompKeyError),
}
