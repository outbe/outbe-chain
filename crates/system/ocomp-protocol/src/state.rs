use alloy_primitives::B256;

use crate::{
    error::ProtocolError,
    hash::hash_framed,
    intent::JobIntentV1,
    receipts::{ActivationOutcome, AggregateActivationReceiptV1},
    registry::HashDomain,
    result::ExactCountsV1,
    schema::{impl_top_level_codec, require, wire_enum_u8, wire_struct, SchemaLimits},
};

wire_struct! {
    pub struct ActiveGenerationV1 {
        pub job_id: B256,
        pub program_semantics_hash: B256,
        pub nod_root: B256,
        pub bucket_root: B256,
        pub contributor_root: B256,
        pub output_manifest_root: B256,
        pub exact_counts: ExactCountsV1,
        pub result_evidence_hash: B256,
        pub availability_certificate_hash: Option<B256>,
    }
    validate = validate_active_generation;
}
impl_top_level_codec!(ActiveGenerationV1, ActiveGenerationV1);

wire_struct! {
    pub struct OcompCompletedBindingV1 {
        pub job_id: B256,
        pub activation_call_id: B256,
        pub result_digest: B256,
        pub result_evidence_hash: B256,
        pub terminal_receipt_hash: B256,
        pub terminal_receipt: AggregateActivationReceiptV1,
    }
}

wire_enum_u8! {
    pub enum OcompTerminalOutcome {
        Completed = 1,
        Expired = 2,
        Conflicted = 3,
        Canceled = 4,
    }
}

wire_struct! {
    pub struct OcompJobTerminalV1 {
        pub outcome: OcompTerminalOutcome,
        pub terminal_height: u64,
        pub terminal_time: u64,
        pub next_pending_nonce: Option<u64>,
        pub completed_binding: Option<OcompCompletedBindingV1>,
    }
}

wire_enum_u8! {
    pub enum OcompJobStatus {
        OffchainPending = 1,
        Completed = 2,
        Expired = 3,
        Conflicted = 4,
        Canceled = 5,
    }
}

wire_struct! {
    pub struct OcompJobRecordV1 {
        pub intent: JobIntentV1,
        pub status: OcompJobStatus,
        pub terminal: Option<OcompJobTerminalV1>,
    }
    validate = validate_job_record;
}
impl_top_level_codec!(OcompJobRecordV1, OcompJobRecordV1);

impl ActiveGenerationV1 {
    pub fn active_generation_hash(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        require(
            self.availability_certificate_hash.is_none(),
            "PoC availability certificate absent",
        )?;
        hash_framed(
            HashDomain::ActiveGeneration,
            &self.encode_canonical(limits)?,
        )
    }
}

impl OcompCompletedBindingV1 {
    pub fn validate_semantics(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
        require(
            self.terminal_receipt.outcome == ActivationOutcome::Applied
                || self.terminal_receipt.outcome == ActivationOutcome::ConflictResolved,
            "completed binding receipt outcome",
        )?;
        require(
            self.job_id == self.terminal_receipt.binding.job_id
                && self.activation_call_id == self.terminal_receipt.binding.activation_call_id
                && self.result_digest == self.terminal_receipt.binding.result_digest,
            "completed binding receipt binding",
        )?;
        require(
            self.terminal_receipt_hash == self.terminal_receipt.terminal_receipt_hash(limits)?,
            "completed binding terminal receipt hash",
        )
    }
}

impl OcompJobRecordV1 {
    pub fn validate_semantics(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
        self.intent.validate_semantics()?;
        match (&self.status, &self.terminal) {
            (OcompJobStatus::OffchainPending, None) => Ok(()),
            (OcompJobStatus::Completed, Some(terminal)) => {
                require(
                    terminal.outcome == OcompTerminalOutcome::Completed
                        && terminal.next_pending_nonce.is_none(),
                    "completed terminal shape",
                )?;
                let binding = terminal
                    .completed_binding
                    .as_ref()
                    .ok_or(ProtocolError::InvalidInvariant("completed binding present"))?;
                binding.validate_semantics(limits)?;
                require(
                    binding.terminal_receipt.outcome == ActivationOutcome::Applied,
                    "completed applied receipt",
                )
            }
            (OcompJobStatus::Expired, Some(terminal)) => require(
                terminal.outcome == OcompTerminalOutcome::Expired
                    && terminal.next_pending_nonce.is_some()
                    && terminal.completed_binding.is_none(),
                "expired terminal shape",
            ),
            (OcompJobStatus::Conflicted, Some(terminal)) => {
                require(
                    terminal.outcome == OcompTerminalOutcome::Conflicted
                        && terminal.next_pending_nonce.is_some(),
                    "conflicted terminal shape",
                )?;
                let binding = terminal
                    .completed_binding
                    .as_ref()
                    .ok_or(ProtocolError::InvalidInvariant("conflict binding present"))?;
                binding.validate_semantics(limits)?;
                require(
                    binding.terminal_receipt.outcome == ActivationOutcome::ConflictResolved,
                    "conflict resolved receipt",
                )
            }
            (OcompJobStatus::Canceled, Some(terminal)) => require(
                terminal.outcome == OcompTerminalOutcome::Canceled
                    && terminal.completed_binding.is_none(),
                "canceled terminal shape",
            ),
            _ => Err(ProtocolError::InvalidInvariant("job status terminal shape")),
        }
    }

    pub fn job_record_hash(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        self.validate_semantics(limits)?;
        hash_framed(HashDomain::JobRecord, &self.encode_canonical(limits)?)
    }
}

fn validate_active_generation(
    generation: &ActiveGenerationV1,
    _limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    require(
        generation.availability_certificate_hash.is_none(),
        "PoC availability certificate absent",
    )
}

fn validate_job_record(
    record: &OcompJobRecordV1,
    limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    record.validate_semantics(limits)
}
