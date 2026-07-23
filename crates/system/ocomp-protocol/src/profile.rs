use alloy_primitives::B256;

use crate::{
    error::ProtocolError,
    hash::hash_framed,
    registry::HashDomain,
    schema::{impl_top_level_codec, wire_enum_u8, wire_struct, SchemaLimits},
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
        pub max_poc_tributes: u32,
        pub unit_tributes: u32,
        pub max_workers_per_domain: u8,
        pub max_pending_jobs: u8,
        pub max_intents_per_block: u8,
        pub max_activations_per_block: u8,
        pub max_ready_inspections_per_block: u8,
        pub max_expirations_per_block: u8,
        pub retry_backoff_blocks: u64,
        pub max_terminal_job_records: u16,
        pub max_reference_currencies: u16,
        pub max_fidelity_cohorts_per_owner: u16,
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
    pub fn protocol_bundle_hash(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        hash_framed(HashDomain::ProtocolBundle, &self.encode_canonical(limits)?)
    }
}
