use alloy_primitives::{B256, U256};
use outbe_node::ocomp::retention::RetentionError;
use outbe_ocomp_protocol::input::{CheckpointIdentityV1, Compression, InputManifestV1};
use outbe_offchain_data::ProjectionState;
use outbe_primitives::projection::ProjectionCheckpoint;

use super::{
    committed_pin_generation, is_lysis_opening_capacity_error, require_projection_checkpoint,
    require_replayed_input_authority, ExpectedInputAuthorityV1,
};

#[test]
fn reader_only_projection_checkpoint_must_cover_the_finalized_request() {
    let request = super::FinalizedRequestBindingV1 {
        block_number: 42,
        block_hash: B256::repeat_byte(0x42),
        state_root: B256::repeat_byte(0x24),
    };
    let state = |block_number, block_hash| ProjectionState {
        chain_id: 7,
        genesis_hash: B256::repeat_byte(0x77),
        storage_schema_version: 1,
        start_block: 1,
        checkpoint: Some(ProjectionCheckpoint {
            block_number,
            block_hash,
        }),
    };

    assert!(require_projection_checkpoint(None, &request).is_err());
    assert!(
        require_projection_checkpoint(Some(&state(41, B256::repeat_byte(0x41))), &request).is_err()
    );
    assert!(
        require_projection_checkpoint(Some(&state(42, B256::repeat_byte(0x99))), &request).is_err()
    );
    assert!(require_projection_checkpoint(Some(&state(42, request.block_hash)), &request).is_ok());
    assert!(
        require_projection_checkpoint(Some(&state(43, B256::repeat_byte(0x43))), &request).is_ok()
    );
}

#[test]
fn committed_generation_is_the_offer_generation_successor() {
    assert_eq!(committed_pin_generation(7).unwrap(), 8);
    assert!(committed_pin_generation(u64::MAX).is_err());
}

#[test]
fn only_lysis_opening_byte_capacity_triggers_bisection() {
    assert!(is_lysis_opening_capacity_error(&RetentionError::Source(
        "Lysis opening bytes exceeds cap: 317499 > 262144".to_owned(),
    )));
    assert!(!is_lysis_opening_capacity_error(&RetentionError::Source(
        "raw contract opening bytes exceeds cap: 317499 > 262144".to_owned(),
    )));
    assert!(!is_lysis_opening_capacity_error(
        &RetentionError::RetainedTributeStorageUnavailable,
    ));
}

#[test]
fn replayed_manifest_requires_every_finalized_authority_field() {
    let expected = expected_input_authority();
    let manifest = matching_manifest(&expected);
    assert!(require_replayed_input_authority(&expected, &expected.checkpoint, &manifest,).is_ok());

    let mut substitutions = Vec::new();
    let mut changed = manifest.clone();
    changed.protocol_bundle_hash = B256::repeat_byte(21);
    substitutions.push(changed);
    let mut changed = manifest.clone();
    changed.job_id = B256::repeat_byte(22);
    substitutions.push(changed);
    let mut changed = manifest.clone();
    changed.attempt += 1;
    substitutions.push(changed);
    let mut changed = manifest.clone();
    changed.checkpoint.finalized_block_hash = B256::repeat_byte(23);
    substitutions.push(changed);
    let mut changed = manifest.clone();
    changed.wwd += 1;
    substitutions.push(changed);
    let mut changed = manifest.clone();
    changed.sealed_tribute_collection_key = B256::repeat_byte(24);
    substitutions.push(changed);
    let mut changed = manifest.clone();
    changed.sealed_tribute_collection_root = B256::repeat_byte(25);
    substitutions.push(changed);
    let mut changed = manifest.clone();
    changed.tribute_count += 1;
    substitutions.push(changed);
    let mut changed = manifest.clone();
    changed.tribute_nominal_total += U256::from(1);
    substitutions.push(changed);
    let mut changed = manifest.clone();
    changed.body_codec_id = B256::repeat_byte(26);
    substitutions.push(changed);
    let mut changed = manifest;
    changed.opening_codec_registry_hash = B256::repeat_byte(27);
    substitutions.push(changed);

    for substituted in substitutions {
        assert!(
            require_replayed_input_authority(&expected, &expected.checkpoint, &substituted,)
                .is_err()
        );
    }

    let mut receipt_checkpoint = expected.checkpoint.clone();
    receipt_checkpoint.finalized_block_number += 1;
    assert!(require_replayed_input_authority(
        &expected,
        &receipt_checkpoint,
        &matching_manifest(&expected),
    )
    .is_err());
}

fn expected_input_authority() -> ExpectedInputAuthorityV1 {
    ExpectedInputAuthorityV1 {
        protocol_bundle_hash: B256::repeat_byte(1),
        job_id: B256::repeat_byte(2),
        attempt: 0,
        checkpoint: CheckpointIdentityV1 {
            finalized_block_number: 4,
            finalized_block_hash: B256::repeat_byte(5),
            finalized_state_root: B256::repeat_byte(6),
            finalized_ce_root: B256::repeat_byte(7),
            ce_schema_version: 8,
        },
        wwd: 20_260_901,
        sealed_tribute_collection_key: B256::repeat_byte(9),
        sealed_tribute_collection_root: B256::repeat_byte(10),
        tribute_count: 11,
        tribute_nominal_total: U256::from(12),
        body_codec_id: B256::repeat_byte(13),
        opening_codec_registry_hash: B256::repeat_byte(14),
    }
}

fn matching_manifest(expected: &ExpectedInputAuthorityV1) -> InputManifestV1 {
    InputManifestV1 {
        protocol_bundle_hash: expected.protocol_bundle_hash,
        job_id: expected.job_id,
        attempt: expected.attempt,
        checkpoint: expected.checkpoint.clone(),
        wwd: expected.wwd,
        sealed_tribute_collection_key: expected.sealed_tribute_collection_key,
        sealed_tribute_collection_root: expected.sealed_tribute_collection_root,
        tribute_count: expected.tribute_count,
        tribute_nominal_total: expected.tribute_nominal_total,
        input_chunk_count: 1,
        input_chunk_list_root: B256::repeat_byte(15),
        fidelity_opening_root: B256::repeat_byte(16),
        oracle_opening_root: B256::repeat_byte(17),
        exact_encoded_bytes: 18,
        exact_record_count: 19,
        body_codec_id: expected.body_codec_id,
        opening_codec_registry_hash: expected.opening_codec_registry_hash,
        compression: Compression::None,
    }
}
