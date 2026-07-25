use alloy_primitives::{B256, U256};

use crate::{
    abi::ACTIVATE_LYSIS_SELECTOR,
    certificate::ExecutionCertificateV1,
    committee::{verify_low_s_prehash, OcompCommitteeSnapshotV1, POC_KEY_EPOCH},
    error::ProtocolError,
    hash::hash_framed,
    intent::{FinalizedIntentProofV1, JobIntentV1},
    registry::HashDomain,
    result::{ActivationPayloadV1, LysisResultV1},
    schema::{impl_top_level_codec, require, wire_enum_u8, wire_struct, SchemaLimits},
};

wire_enum_u8! {
    pub enum SignOncePurpose {
        ResultSignature = 1,
    }
}

wire_struct! {
    pub struct PoCActivationV1 {
        pub intent_id: B256,
        pub finalized_intent_proof: FinalizedIntentProofV1,
        pub activation_payload: ActivationPayloadV1,
        pub result: LysisResultV1,
        pub certificate: ExecutionCertificateV1,
    }
}
impl_top_level_codec!(PoCActivationV1, PoCActivationV1);

wire_struct! {
    pub struct CandidateAnnouncementV1 {
        pub protocol_bundle_hash: B256,
        pub job_id: B256,
        pub attempt: u32,
        pub result: LysisResultV1,
        pub result_digest: B256,
        pub validator_index: u8,
        pub key_epoch: u64,
        pub signature_rs: [u8; 64],
    }
}
impl_top_level_codec!(CandidateAnnouncementV1, CandidateAnnouncementV1);

wire_struct! {
    pub struct SignOnceRecordV1 {
        pub chain_id: u64,
        pub purpose: SignOncePurpose,
        pub job_id: B256,
        pub attempt: u32,
        pub protocol_bundle_hash: B256,
        pub committee_snapshot_hash: B256,
        pub key_epoch: u64,
        pub result_digest: B256,
        pub signature_rs: [u8; 64],
    }
}
impl_top_level_codec!(SignOnceRecordV1, SignOnceRecordV1);

wire_struct! {
    pub struct ActivationCallCoreV1 {
        pub intent_id: B256,
        pub job_id: B256,
        pub attempt: u32,
        pub protocol_bundle_hash: B256,
        pub result_digest: B256,
        pub activation_preconditions_hash: B256,
        pub terminal_pending_nonce: u64,
    }
}
impl_top_level_codec!(ActivationCallCoreV1, ActivationCallCoreV1);

impl PoCActivationV1 {
    pub fn verify(
        &self,
        finalized_request_state_root: B256,
        committee: &OcompCommitteeSnapshotV1,
        current_height: u64,
        limits: &SchemaLimits,
    ) -> Result<(), ProtocolError> {
        let intent = self.finalized_intent_proof.decoded_intent(limits)?;
        intent.validate_semantics()?;
        require(
            self.intent_id == intent.intent_id(limits)?,
            "activation intent id",
        )?;
        let job_id = intent.job_id(
            self.finalized_intent_proof
                .parent_accounting
                .finalized_block_hash,
            finalized_request_state_root,
            limits,
        )?;
        require(
            self.result.protocol_bundle_hash == intent.protocol_bundle_hash
                && self.result.job_id == job_id
                && self.result.attempt == intent.attempt,
            "activation result intent binding",
        )?;
        self.result.validate_finalized_intent(&intent)?;
        let reconstructed_payload = self.result.activation_payload(limits)?;
        require(
            self.activation_payload == reconstructed_payload,
            "activation payload reconstruction",
        )?;
        let result_digest = reconstructed_payload.result_digest(limits)?;
        require(
            self.certificate.result_digest == result_digest,
            "activation certificate result digest",
        )?;
        require(
            intent.result_committee_snapshot_hash == committee.snapshot_hash(limits)?,
            "intent committee binding",
        )?;
        self.certificate.verify(committee, current_height, limits)
    }

    pub fn result_evidence_hash(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        hash_framed(HashDomain::ResultEvidence, &self.encode_canonical(limits)?)
    }
}

pub fn encode_activate_lysis_calldata(
    activation: &PoCActivationV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, ProtocolError> {
    let activation_bytes = activation.encode_canonical(limits)?;
    let activation_len = activation_bytes.len();
    let padded_len = activation_len
        .checked_add(31)
        .ok_or(ProtocolError::IntegerOverflow {
            what: "activation ABI padding",
        })?
        & !31;
    let calldata_len = 4_usize
        .checked_add(64)
        .and_then(|fixed| fixed.checked_add(padded_len))
        .ok_or(ProtocolError::IntegerOverflow {
            what: "activation calldata length",
        })?;
    let abi_len = u64::try_from(activation_len).map_err(|_| ProtocolError::IntegerOverflow {
        what: "activation ABI length",
    })?;
    let mut calldata = Vec::new();
    calldata
        .try_reserve_exact(calldata_len)
        .map_err(|_| ProtocolError::AllocationFailed {
            what: "activation calldata",
            bytes: calldata_len,
        })?;
    calldata.extend_from_slice(&ACTIVATE_LYSIS_SELECTOR);
    calldata.extend_from_slice(&U256::from(32).to_be_bytes::<32>());
    calldata.extend_from_slice(&U256::from(abi_len).to_be_bytes::<32>());
    calldata.extend_from_slice(&activation_bytes);
    calldata.resize(calldata_len, 0);
    Ok(calldata)
}

impl CandidateAnnouncementV1 {
    pub fn verify(
        &self,
        finalized_intent: &JobIntentV1,
        expected_job_id: B256,
        committee: &OcompCommitteeSnapshotV1,
        current_height: u64,
        limits: &SchemaLimits,
    ) -> Result<(), ProtocolError> {
        finalized_intent.validate_semantics()?;
        require(
            self.protocol_bundle_hash == self.result.protocol_bundle_hash
                && self.job_id == self.result.job_id
                && self.attempt == self.result.attempt,
            "candidate result binding",
        )?;
        require(
            self.protocol_bundle_hash == finalized_intent.protocol_bundle_hash
                && self.job_id == expected_job_id
                && self.attempt == finalized_intent.attempt,
            "candidate finalized job binding",
        )?;
        self.result.validate_finalized_intent(finalized_intent)?;
        require(self.key_epoch == POC_KEY_EPOCH, "candidate key epoch")?;
        let payload = self.result.activation_payload(limits)?;
        require(
            self.result_digest == payload.result_digest(limits)?,
            "candidate result digest",
        )?;
        let member = committee
            .ordered_members
            .get(usize::from(self.validator_index))
            .ok_or(ProtocolError::InvalidInvariant("candidate validator index"))?;
        require(
            member.key_epoch == self.key_epoch
                && member.valid_from_height <= current_height
                && current_height < member.valid_until_height_exclusive,
            "candidate member validity",
        )?;
        require(
            finalized_intent.result_committee_snapshot_hash == committee.snapshot_hash(limits)?,
            "candidate intent committee binding",
        )?;
        verify_low_s_prehash(
            &member.ocomp_public_key_sec1,
            self.result_digest,
            &self.signature_rs,
        )
    }
}

impl SignOnceRecordV1 {
    pub fn slot_id(&self) -> Result<B256, ProtocolError> {
        require(self.key_epoch == POC_KEY_EPOCH, "sign-once key epoch")?;
        let mut payload = Vec::with_capacity(45);
        payload.extend_from_slice(&self.chain_id.to_be_bytes());
        payload.push(self.purpose as u8);
        payload.extend_from_slice(self.job_id.as_slice());
        payload.extend_from_slice(&self.attempt.to_be_bytes());
        hash_framed(HashDomain::SignOnceSlot, &payload)
    }
}

impl ActivationCallCoreV1 {
    pub fn activation_call_id(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        hash_framed(HashDomain::ActivationCall, &self.encode_canonical(limits)?)
    }
}
