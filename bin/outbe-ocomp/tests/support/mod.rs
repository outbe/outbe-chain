use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    profile::ProtocolBundleV1,
    registry::{FIDELITY_OPENING_CODEC_ID, ORACLE_OPENING_CODEC_ID, TRIBUTE_BODY_CODEC_ID},
};

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

pub fn protocol_bundle() -> ProtocolBundleV1 {
    ProtocolBundleV1 {
        protocol_version: 1,
        fork_id: hash(1),
        intent_codec_id: hash(2),
        finalized_intent_proof_codec_id: hash(3),
        tribute_body_codec_id: TRIBUTE_BODY_CODEC_ID,
        fidelity_opening_codec_id: FIDELITY_OPENING_CODEC_ID,
        oracle_opening_codec_id: ORACLE_OPENING_CODEC_ID,
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
