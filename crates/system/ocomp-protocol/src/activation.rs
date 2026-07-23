use alloy_primitives::B256;

use crate::{
    certificate::ExecutionCertificateV1,
    committee::{verify_low_s_prehash, OcompCommitteeSnapshotV1, POC_KEY_EPOCH},
    error::ProtocolError,
    hash::hash_framed,
    intent::FinalizedIntentProofV1,
    registry::HashDomain,
    result::{ActivationPayloadV1, BoundedLysisResultV1},
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
        pub result: BoundedLysisResultV1,
        pub certificate: ExecutionCertificateV1,
    }
}
impl_top_level_codec!(PoCActivationV1, PoCActivationV1);

wire_struct! {
    pub struct CandidateAnnouncementV1 {
        pub protocol_bundle_hash: B256,
        pub job_id: B256,
        pub attempt: u32,
        pub result: BoundedLysisResultV1,
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
        pub reservation_set_hash: B256,
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

impl CandidateAnnouncementV1 {
    pub fn verify(
        &self,
        committee: &OcompCommitteeSnapshotV1,
        current_height: u64,
        limits: &SchemaLimits,
    ) -> Result<(), ProtocolError> {
        require(
            self.protocol_bundle_hash == self.result.protocol_bundle_hash
                && self.job_id == self.result.job_id
                && self.attempt == self.result.attempt,
            "candidate result binding",
        )?;
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
