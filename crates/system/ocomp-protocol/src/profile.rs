use alloy_primitives::B256;

use crate::{
    codec::CodecLimits,
    error::ProtocolError,
    generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1,
    hash::{framed_identity_hash, hash_framed},
    registry::HashDomain,
    schema::{impl_top_level_codec, wire_enum_u8, wire_struct, SchemaLimits},
};

/// Generated measurement ceilings shared by every OCOMP PoC process.
///
/// These compile ceilings do not arm a network or provide a bundle hash.
#[must_use]
pub fn poc_schema_limits() -> SchemaLimits {
    POC_SCHEMA_LIMITS
}

const POC_SCHEMA_LIMITS: SchemaLimits = {
    let candidate = OCOMP_POC_CANDIDATE_LIMITS_V1;
    let max_action_items = if candidate.max_nod_actions_per_result_chunk
        <= candidate.max_contributor_actions_per_result_chunk
    {
        candidate.max_nod_actions_per_result_chunk
    } else {
        candidate.max_contributor_actions_per_result_chunk
    };
    assert!(
        candidate.max_activation_ocb1_bytes <= usize::MAX as u64,
        "generated body cap fits usize"
    );
    assert!(
        candidate.max_protocol_collection_items <= usize::MAX as u64,
        "generated item cap fits usize"
    );
    assert!(
        candidate.max_transaction_rlp_bytes <= usize::MAX as u64,
        "generated allocation cap fits usize"
    );
    assert!(
        candidate.max_finalized_intent_proof_bytes <= usize::MAX as u64,
        "generated proof cap fits usize"
    );
    assert!(
        candidate.max_opening_bytes <= usize::MAX as u64,
        "generated opening cap fits usize"
    );
    assert!(
        max_action_items <= usize::MAX as u64,
        "generated per-result-chunk action cap fits usize"
    );
    assert!(
        candidate.max_records_per_input_chunk <= usize::MAX as u64,
        "generated per-chunk record cap fits usize"
    );
    assert!(
        candidate.max_inputs_per_work_unit <= usize::MAX as u64,
        "generated per-unit input cap fits usize"
    );
    let max_body_bytes = candidate.max_activation_ocb1_bytes as usize;
    let max_collection_items = candidate.max_protocol_collection_items as usize;
    SchemaLimits {
        codec: CodecLimits::new(
            max_body_bytes,
            max_collection_items,
            candidate.max_transaction_rlp_bytes as usize,
        ),
        max_bounded_bytes: max_body_bytes,
        max_proof_bytes: candidate.max_finalized_intent_proof_bytes as usize,
        max_opening_bytes: candidate.max_opening_bytes as usize,
        max_collection_items,
        max_action_items: max_action_items as usize,
        max_chunk_items: candidate.max_records_per_input_chunk as usize,
        max_unit_inputs: candidate.max_inputs_per_work_unit as usize,
        max_result_chunk_bytes: candidate.max_result_chunk_bytes,
        // Local control transports typed off-chain proofs and openings as well
        // as compact activation data. Method codecs enforce their narrower
        // limits. The frame ceiling must not reject a body that the shared
        // canonical codec accepts.
        max_control_body_bytes: max_body_bytes,
    }
};

wire_enum_u8! {
    /// Closed program registry for the PoC.
    pub enum ProgramId {
        LysisV1 = 1,
    }
}

wire_struct! {
    pub struct CorrectnessProfileV1 {
        pub profile_id: B256,
        pub program: ProgramId,
        pub arithmetic_profile_id: B256,
        pub object_codec_registry_hash: B256,
        pub list_root_scheme_id: B256,
        pub result_signature_profile_id: B256,
        pub finality_verifier_profile_id: B256,
    }
}
impl_top_level_codec!(CorrectnessProfileV1, CorrectnessProfileV1);

wire_struct! {
    pub struct CapacityProfileV1 {
        pub profile_id: B256,
        pub max_tributes_per_work_shard: u32,
        pub max_workers_per_domain: u8,
        pub max_intents_per_block: u8,
        pub max_activations_per_block: u8,
        pub max_ready_inspections_per_block: u8,
        pub max_expirations_per_block: u8,
        pub ready_backoff_blocks: u64,
        pub max_reference_currencies: u16,
        pub max_oracle_wwd_pair_entries: u32,
        pub max_active_scurve_entries: u32,
        pub result_deadline_blocks: u64,
        pub source_retention_after_terminal_blocks: u64,
        pub generated_limits_manifest_hash: B256,
    }
}
impl_top_level_codec!(CapacityProfileV1, CapacityProfileV1);

wire_struct! {
    pub struct ProtocolBundleV1 {
        pub protocol_version: u16,
        pub fork_id: B256,
        pub intent_codec_id: B256,
        pub finalized_intent_proof_codec_id: B256,
        pub tribute_body_codec_id: B256,
        pub fidelity_opening_codec_id: B256,
        pub oracle_opening_codec_id: B256,
        pub result_codec_id: B256,
        pub action_codec_id: B256,
        pub activation_codec_id: B256,
        pub evidence_codec_id: B256,
        pub request_semantics_version: u16,
        pub lysis_program_semantics_hash: B256,
        pub planner_spec_version: u16,
        pub reducer_spec_version: u16,
        pub activation_apply_semantics_hash: B256,
        pub effect_contract_registry_hash: B256,
        pub object_codec_registry_hash: B256,
        pub correctness_profile_id: B256,
        pub capacity_profile_id: B256,
        pub result_signature_profile_id: B256,
        pub finality_verifier_and_vote_domain_id: B256,
        pub consensus_committee_history_schema_version: u16,
        pub ocomp_committee_schema_version: u16,
        pub proof_system_and_verifier_key_id: Option<B256>,
        pub da_codec_and_binding_verifier_id: Option<B256>,
        pub anti_equivocation_journal_schema_hash: B256,
        pub mode_pause_revocation_semantics_hash: B256,
        pub upgrade_fsm_semantics_hash: B256,
        pub release_requirement_catalog_sequence: u64,
        pub release_requirement_catalog_hash: B256,
        pub release_requirement_catalog_parent_hash: B256,
        pub release_gate_authority_envelope_hash: B256,
        pub release_approval_policy_hash: B256,
        pub release_validator_command_artifact_hash: B256,
        pub consensus_state_schema_version: u16,
        pub migration_manifest_hash: B256,
        pub required_upgrade_handler_set_hash: B256,
    }
}
impl_top_level_codec!(ProtocolBundleV1, ProtocolBundleV1);

impl ProtocolBundleV1 {
    framed_identity_hash!(protocol_bundle_hash, ProtocolBundle);

    pub fn opening_codec_registry_hash(&self) -> Result<B256, ProtocolError> {
        let mut payload = Vec::with_capacity(68);
        payload.extend_from_slice(&2_u16.to_be_bytes());
        payload.push(1);
        payload.extend_from_slice(self.fidelity_opening_codec_id.as_slice());
        payload.push(2);
        payload.extend_from_slice(self.oracle_opening_codec_id.as_slice());
        hash_framed(HashDomain::OpeningCodecRegistry, &payload)
    }

    pub fn validate_lysis_v1_input_codecs(&self) -> Result<(), ProtocolError> {
        crate::schema::require(
            self.tribute_body_codec_id == crate::registry::TRIBUTE_BODY_CODEC_ID
                && self.fidelity_opening_codec_id == crate::registry::FIDELITY_OPENING_CODEC_ID
                && self.oracle_opening_codec_id == crate::registry::ORACLE_OPENING_CODEC_ID,
            "unsupported Lysis V1 input codec bundle",
        )
    }
}

/// Protocol bundle of the measurement-classification OCOMP fork install.
#[must_use]
pub fn measurement_protocol_bundle_v1() -> ProtocolBundleV1 {
    let hash = B256::repeat_byte;
    ProtocolBundleV1 {
        protocol_version: 1,
        fork_id: hash(1),
        intent_codec_id: hash(2),
        finalized_intent_proof_codec_id: hash(3),
        tribute_body_codec_id: crate::registry::TRIBUTE_BODY_CODEC_ID,
        fidelity_opening_codec_id: crate::registry::FIDELITY_OPENING_CODEC_ID,
        oracle_opening_codec_id: crate::registry::ORACLE_OPENING_CODEC_ID,
        result_codec_id: hash(4),
        action_codec_id: hash(5),
        activation_codec_id: hash(6),
        evidence_codec_id: hash(7),
        request_semantics_version: 1,
        lysis_program_semantics_hash: hash(8),
        planner_spec_version: 1,
        reducer_spec_version: 1,
        activation_apply_semantics_hash: hash(9),
        effect_contract_registry_hash: hash(10),
        object_codec_registry_hash: hash(11),
        correctness_profile_id: hash(12),
        capacity_profile_id: hash(13),
        result_signature_profile_id: hash(14),
        finality_verifier_and_vote_domain_id: hash(15),
        consensus_committee_history_schema_version: 1,
        ocomp_committee_schema_version: 1,
        proof_system_and_verifier_key_id: None,
        da_codec_and_binding_verifier_id: None,
        anti_equivocation_journal_schema_hash: hash(16),
        mode_pause_revocation_semantics_hash: hash(17),
        upgrade_fsm_semantics_hash: hash(18),
        release_requirement_catalog_sequence: 1,
        release_requirement_catalog_hash: hash(19),
        release_requirement_catalog_parent_hash: hash(20),
        release_gate_authority_envelope_hash: hash(21),
        release_approval_policy_hash: hash(22),
        release_validator_command_artifact_hash: hash(23),
        consensus_state_schema_version: 1,
        migration_manifest_hash: hash(24),
        required_upgrade_handler_set_hash: hash(25),
    }
}
