//! Canonical direct on-chain result votes and bounded accountability state.

use alloy_primitives::B256;

use crate::{
    committee::{
        verify_low_s_prehash, OcompCommitteeSnapshotV1, POC_COMMITTEE_SIZE,
        POC_COMMITTEE_THRESHOLD, POC_KEY_EPOCH,
    },
    error::ProtocolError,
    hash::hash_framed,
    intent::JobIntentV1,
    registry::HashDomain,
    result::LysisResultV1,
    schema::{impl_top_level_codec, require, wire_struct, SchemaLimits},
};

wire_struct! {
    pub struct ResultVoteV1 {
        pub protocol_bundle_hash: B256,
        pub job_id: B256,
        pub attempt: u32,
        pub result_committee_snapshot_hash: B256,
        pub validator_index: u8,
        pub key_epoch: u64,
        pub result: LysisResultV1,
        pub signature_rs: [u8; 64],
    }
}
impl_top_level_codec!(ResultVoteV1, ResultVoteV1);

wire_struct! {
    pub struct EquivocationEvidenceV1 {
        pub conflicting_result_digest: B256,
        pub conflicting_key_epoch: u64,
        pub conflicting_signature_rs: [u8; 64],
        pub submitted_height: u64,
    }
}
impl_top_level_codec!(EquivocationEvidenceV1, EquivocationEvidenceV1);

wire_struct! {
    pub struct ResultVoteSlotV1 {
        pub validator_index: u8,
        pub first_result_digest: B256,
        pub key_epoch: u64,
        pub first_signature_rs: [u8; 64],
        pub submitted_height: u64,
        pub equivocation: Option<EquivocationEvidenceV1>,
    }
    validate = validate_vote_slot;
}
impl_top_level_codec!(ResultVoteSlotV1, ResultVoteSlotV1);

wire_struct! {
    pub struct OcompQuorumV1 {
        pub result_digest: B256,
        pub quorum_height: u64,
        pub signer_bitmap: u8,
        pub evidence_hash: B256,
    }
    validate = validate_quorum;
}
impl_top_level_codec!(OcompQuorumV1, OcompQuorumV1);

wire_struct! {
    pub struct OcompAccountabilitySummaryV1 {
        pub closed_height: u64,
        pub timely_bitmap: u8,
        pub matching_bitmap: u8,
        pub divergent_bitmap: u8,
        pub missing_bitmap: u8,
        pub equivocation_bitmap: u8,
    }
    validate = validate_accountability_summary;
}
impl_top_level_codec!(OcompAccountabilitySummaryV1, OcompAccountabilitySummaryV1);

wire_struct! {
    pub struct OcompVoteAccountabilityV1 {
        pub job_id: B256,
        pub result_committee_snapshot_hash: B256,
        pub slots: Vec<Option<ResultVoteSlotV1>>,
        pub quorum: Option<OcompQuorumV1>,
        pub closed_summary: Option<OcompAccountabilitySummaryV1>,
    }
    validate = validate_vote_accountability;
}
impl_top_level_codec!(OcompVoteAccountabilityV1, OcompVoteAccountabilityV1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordVoteOutcomeV1 {
    FirstVote,
    ExactRetry,
    SameDigestRetry,
    EquivocationRecorded,
    EquivocationAlreadyRecorded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResultVoteSigningSubjectV1 {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub fork_id: B256,
    pub protocol_bundle_hash: B256,
    pub job_id: B256,
    pub attempt: u32,
    pub result_committee_snapshot_hash: B256,
    pub validator_index: u8,
    pub key_epoch: u64,
    pub purpose: u8,
    pub result_digest: B256,
}

impl ResultVoteSigningSubjectV1 {
    pub fn signing_digest(self) -> Result<B256, ProtocolError> {
        let mut payload = Vec::with_capacity(8 + 32 * 6 + 4 + 1 + 8 + 1);
        payload.extend_from_slice(&self.chain_id.to_be_bytes());
        payload.extend_from_slice(self.genesis_hash.as_slice());
        payload.extend_from_slice(self.fork_id.as_slice());
        payload.extend_from_slice(self.protocol_bundle_hash.as_slice());
        payload.extend_from_slice(self.job_id.as_slice());
        payload.extend_from_slice(&self.attempt.to_be_bytes());
        payload.extend_from_slice(self.result_committee_snapshot_hash.as_slice());
        payload.push(self.validator_index);
        payload.extend_from_slice(&self.key_epoch.to_be_bytes());
        payload.push(self.purpose);
        payload.extend_from_slice(self.result_digest.as_slice());
        hash_framed(HashDomain::ResultVoteSubject, &payload)
    }
}

impl ResultVoteV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn verify(
        &self,
        finalized_intent: &JobIntentV1,
        expected_job_id: B256,
        committee: &OcompCommitteeSnapshotV1,
        inclusion_height: u64,
        open_height: u64,
        deadline_height: u64,
        limits: &SchemaLimits,
    ) -> Result<(), ProtocolError> {
        finalized_intent.validate_semantics()?;
        self.result.validate_semantics(limits)?;
        self.result.validate_finalized_intent(finalized_intent)?;
        require(open_height < deadline_height, "vote window ordering")?;
        require(
            open_height <= inclusion_height && inclusion_height < deadline_height,
            "vote inclusion height",
        )?;
        require(
            self.protocol_bundle_hash == finalized_intent.protocol_bundle_hash
                && self.job_id == expected_job_id
                && self.attempt == finalized_intent.attempt,
            "vote finalized job binding",
        )?;
        require(
            self.result.protocol_bundle_hash == self.protocol_bundle_hash
                && self.result.job_id == self.job_id
                && self.result.attempt == self.attempt,
            "vote full result binding",
        )?;
        let committee_hash = committee.snapshot_hash(limits)?;
        require(
            self.result_committee_snapshot_hash == finalized_intent.result_committee_snapshot_hash
                && self.result_committee_snapshot_hash == committee_hash,
            "vote committee binding",
        )?;
        require(self.key_epoch == POC_KEY_EPOCH, "vote key epoch")?;
        let member = committee
            .ordered_members
            .get(usize::from(self.validator_index))
            .ok_or(ProtocolError::InvalidInvariant("vote validator index"))?;
        require(
            member.key_epoch == self.key_epoch
                && member.valid_from_height <= inclusion_height
                && inclusion_height < member.valid_until_height_exclusive,
            "vote member validity",
        )?;
        let signing_digest = self.signing_digest(finalized_intent, limits)?;
        verify_low_s_prehash(
            &member.ocomp_public_key_sec1,
            signing_digest,
            &self.signature_rs,
        )
    }

    pub fn result_digest(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        self.result.result_digest(limits)
    }

    pub fn signing_digest(
        &self,
        finalized_intent: &JobIntentV1,
        limits: &SchemaLimits,
    ) -> Result<B256, ProtocolError> {
        require(
            self.protocol_bundle_hash == finalized_intent.protocol_bundle_hash
                && self.attempt == finalized_intent.attempt
                && self.result_committee_snapshot_hash
                    == finalized_intent.result_committee_snapshot_hash,
            "vote signing subject intent binding",
        )?;
        ResultVoteSigningSubjectV1 {
            chain_id: finalized_intent.chain_id,
            genesis_hash: finalized_intent.genesis_hash,
            fork_id: finalized_intent.fork_id,
            protocol_bundle_hash: self.protocol_bundle_hash,
            job_id: self.job_id,
            attempt: self.attempt,
            result_committee_snapshot_hash: self.result_committee_snapshot_hash,
            validator_index: self.validator_index,
            key_epoch: self.key_epoch,
            purpose: 1, // SignOncePurpose::ResultSignature
            result_digest: self.result_digest(limits)?,
        }
        .signing_digest()
    }
}

impl ResultVoteSlotV1 {
    pub fn from_vote(
        vote: &ResultVoteV1,
        submitted_height: u64,
        limits: &SchemaLimits,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            validator_index: vote.validator_index,
            first_result_digest: vote.result_digest(limits)?,
            key_epoch: vote.key_epoch,
            first_signature_rs: vote.signature_rs,
            submitted_height,
            equivocation: None,
        })
    }

    fn exact_vote(&self, vote: &ResultVoteV1, vote_digest: B256, _submitted_height: u64) -> bool {
        self.validator_index == vote.validator_index
            && self.first_result_digest == vote_digest
            && self.key_epoch == vote.key_epoch
            && self.first_signature_rs == vote.signature_rs
    }
}

impl OcompVoteAccountabilityV1 {
    pub fn empty(
        job_id: B256,
        result_committee_snapshot_hash: B256,
    ) -> Result<Self, ProtocolError> {
        let slots = (0..POC_COMMITTEE_SIZE).map(|_| None).collect();
        Ok(Self {
            job_id,
            result_committee_snapshot_hash,
            slots,
            quorum: None,
            closed_summary: None,
        })
    }

    pub fn record_verified_vote(
        &mut self,
        vote: &ResultVoteV1,
        submitted_height: u64,
        limits: &SchemaLimits,
    ) -> Result<RecordVoteOutcomeV1, ProtocolError> {
        self.validate_semantics(limits)?;
        require(self.closed_summary.is_none(), "vote window already closed")?;
        require(
            vote.job_id == self.job_id
                && vote.result_committee_snapshot_hash == self.result_committee_snapshot_hash,
            "vote accountability binding",
        )?;
        let vote_digest = vote.result_digest(limits)?;
        let slot = self
            .slots
            .get_mut(usize::from(vote.validator_index))
            .ok_or(ProtocolError::InvalidInvariant("vote slot index"))?;
        let outcome = match slot {
            None => {
                *slot = Some(ResultVoteSlotV1::from_vote(vote, submitted_height, limits)?);
                RecordVoteOutcomeV1::FirstVote
            }
            Some(existing) if existing.exact_vote(vote, vote_digest, submitted_height) => {
                RecordVoteOutcomeV1::ExactRetry
            }
            Some(existing) if existing.first_result_digest == vote_digest => {
                RecordVoteOutcomeV1::SameDigestRetry
            }
            Some(existing) if existing.equivocation.is_none() => {
                existing.equivocation = Some(EquivocationEvidenceV1 {
                    conflicting_result_digest: vote_digest,
                    conflicting_key_epoch: vote.key_epoch,
                    conflicting_signature_rs: vote.signature_rs,
                    submitted_height,
                });
                RecordVoteOutcomeV1::EquivocationRecorded
            }
            Some(_) => RecordVoteOutcomeV1::EquivocationAlreadyRecorded,
        };
        if self.quorum.is_none() && matches!(outcome, RecordVoteOutcomeV1::FirstVote) {
            self.quorum = self.derive_quorum(submitted_height, limits)?;
        }
        self.validate_semantics(limits)?;
        Ok(outcome)
    }

    pub fn close(
        &mut self,
        closed_height: u64,
        limits: &SchemaLimits,
    ) -> Result<OcompAccountabilitySummaryV1, ProtocolError> {
        self.validate_semantics(limits)?;
        if let Some(summary) = &self.closed_summary {
            require(
                summary.closed_height == closed_height,
                "accountability close exact retry",
            )?;
            return Ok(summary.clone());
        }
        let mut timely_bitmap = 0_u8;
        let mut matching_bitmap = 0_u8;
        let mut divergent_bitmap = 0_u8;
        let mut equivocation_bitmap = 0_u8;
        for (index, slot) in self.slots.iter().enumerate() {
            let Some(slot) = slot else {
                continue;
            };
            let bit = 1_u8 << index;
            timely_bitmap |= bit;
            if self
                .quorum
                .as_ref()
                .is_some_and(|quorum| quorum.result_digest == slot.first_result_digest)
            {
                matching_bitmap |= bit;
            } else {
                divergent_bitmap |= bit;
            }
            if slot.equivocation.is_some() {
                equivocation_bitmap |= bit;
            }
        }
        let committee_mask = committee_mask();
        let summary = OcompAccountabilitySummaryV1 {
            closed_height,
            timely_bitmap,
            matching_bitmap,
            divergent_bitmap,
            missing_bitmap: committee_mask & !timely_bitmap,
            equivocation_bitmap,
        };
        summary.validate_semantics()?;
        self.closed_summary = Some(summary.clone());
        self.validate_semantics(limits)?;
        Ok(summary)
    }

    pub fn accountability_hash(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        self.validate_semantics(limits)?;
        hash_framed(
            HashDomain::VoteAccountability,
            &self.encode_canonical(limits)?,
        )
    }

    fn derive_quorum(
        &self,
        quorum_height: u64,
        limits: &SchemaLimits,
    ) -> Result<Option<OcompQuorumV1>, ProtocolError> {
        for candidate in self.slots.iter().flatten() {
            let mut signer_bitmap = 0_u8;
            for (index, slot) in self.slots.iter().enumerate() {
                if slot
                    .as_ref()
                    .is_some_and(|slot| slot.first_result_digest == candidate.first_result_digest)
                {
                    signer_bitmap |= 1_u8 << index;
                }
            }
            if signer_bitmap.count_ones() < u32::from(POC_COMMITTEE_THRESHOLD) {
                continue;
            }
            let evidence_hash = self.quorum_evidence_hash(
                candidate.first_result_digest,
                quorum_height,
                signer_bitmap,
                limits,
            )?;
            return Ok(Some(OcompQuorumV1 {
                result_digest: candidate.first_result_digest,
                quorum_height,
                signer_bitmap,
                evidence_hash,
            }));
        }
        Ok(None)
    }

    fn quorum_evidence_hash(
        &self,
        result_digest: B256,
        quorum_height: u64,
        signer_bitmap: u8,
        _limits: &SchemaLimits,
    ) -> Result<B256, ProtocolError> {
        let mut payload = Vec::new();
        payload.extend_from_slice(self.job_id.as_slice());
        payload.extend_from_slice(self.result_committee_snapshot_hash.as_slice());
        payload.extend_from_slice(result_digest.as_slice());
        payload.extend_from_slice(&quorum_height.to_be_bytes());
        payload.push(signer_bitmap);
        for (index, slot) in self.slots.iter().enumerate() {
            if signer_bitmap & (1_u8 << index) == 0 {
                continue;
            }
            let slot = slot
                .as_ref()
                .ok_or(ProtocolError::InvalidInvariant("quorum slot present"))?;
            require(
                slot.first_result_digest == result_digest,
                "quorum slot result digest",
            )?;
            payload.push(slot.validator_index);
            payload.extend_from_slice(slot.first_result_digest.as_slice());
            payload.extend_from_slice(&slot.key_epoch.to_be_bytes());
            payload.extend_from_slice(&slot.first_signature_rs);
            payload.extend_from_slice(&slot.submitted_height.to_be_bytes());
        }
        hash_framed(HashDomain::QuorumEvidence, &payload)
    }

    pub fn validate_semantics(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
        require(
            self.slots.len() == POC_COMMITTEE_SIZE,
            "accountability slot count",
        )?;
        for (index, slot) in self.slots.iter().enumerate() {
            let Some(slot) = slot else {
                continue;
            };
            slot.validate_semantics()?;
            require(
                usize::from(slot.validator_index) == index,
                "accountability slot position",
            )?;
        }
        if let Some(quorum) = &self.quorum {
            quorum.validate_semantics()?;
            let matching = self
                .slots
                .iter()
                .enumerate()
                .filter(|(index, slot)| {
                    quorum.signer_bitmap & (1_u8 << index) != 0
                        && slot
                            .as_ref()
                            .is_some_and(|slot| slot.first_result_digest == quorum.result_digest)
                })
                .count();
            require(
                matching >= usize::from(POC_COMMITTEE_THRESHOLD),
                "quorum matching slots",
            )?;
            require(
                quorum.evidence_hash
                    == self.quorum_evidence_hash(
                        quorum.result_digest,
                        quorum.quorum_height,
                        quorum.signer_bitmap,
                        limits,
                    )?,
                "quorum evidence hash",
            )?;
        }
        if let Some(summary) = &self.closed_summary {
            summary.validate_semantics()?;
        }
        Ok(())
    }
}

impl ResultVoteSlotV1 {
    pub fn validate_semantics(&self) -> Result<(), ProtocolError> {
        require(
            usize::from(self.validator_index) < POC_COMMITTEE_SIZE,
            "vote slot validator index",
        )?;
        require(self.key_epoch == POC_KEY_EPOCH, "vote slot key epoch")?;
        if let Some(equivocation) = &self.equivocation {
            require(
                equivocation.conflicting_key_epoch == self.key_epoch
                    && equivocation.conflicting_result_digest != self.first_result_digest,
                "equivocation shape",
            )?;
        }
        Ok(())
    }
}

impl OcompQuorumV1 {
    pub fn validate_semantics(&self) -> Result<(), ProtocolError> {
        require(
            self.signer_bitmap & !committee_mask() == 0
                && self.signer_bitmap.count_ones() >= u32::from(POC_COMMITTEE_THRESHOLD),
            "quorum signer bitmap",
        )
    }
}

impl OcompAccountabilitySummaryV1 {
    pub fn validate_semantics(&self) -> Result<(), ProtocolError> {
        let mask = committee_mask();
        for bitmap in [
            self.timely_bitmap,
            self.matching_bitmap,
            self.divergent_bitmap,
            self.missing_bitmap,
            self.equivocation_bitmap,
        ] {
            require(bitmap & !mask == 0, "accountability bitmap high bits")?;
        }
        require(
            self.matching_bitmap & self.divergent_bitmap == 0
                && self.matching_bitmap | self.divergent_bitmap == self.timely_bitmap
                && self.missing_bitmap == mask & !self.timely_bitmap
                && self.equivocation_bitmap & !self.timely_bitmap == 0,
            "accountability bitmap partition",
        )
    }
}

fn validate_vote_slot(
    slot: &ResultVoteSlotV1,
    _limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    slot.validate_semantics()
}

fn validate_quorum(quorum: &OcompQuorumV1, _limits: &SchemaLimits) -> Result<(), ProtocolError> {
    quorum.validate_semantics()
}

fn validate_accountability_summary(
    summary: &OcompAccountabilitySummaryV1,
    _limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    summary.validate_semantics()
}

fn validate_vote_accountability(
    accountability: &OcompVoteAccountabilityV1,
    limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    accountability.validate_semantics(limits)
}

const fn committee_mask() -> u8 {
    (1_u8 << POC_COMMITTEE_SIZE) - 1
}
