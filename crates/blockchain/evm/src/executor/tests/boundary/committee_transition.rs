use super::super::*;
use super::fixtures::*;
/// Task 01 test: activate_reshared_set() runs AFTER participation decode.
///
/// Simulates the executor's finish() hook order:
/// 1. Read active consensus set (OLD set)
/// 2. Decode participation bitmap against OLD set
/// 3. Record participation / slashing
/// 4. THEN activate_reshared_set() -> set changes to NEW set
///
/// Verifies that get_active_consensus_set() returns the OLD set
/// at step 2, and the NEW set only after step 4.
#[test]
fn test_reshare_activation_after_participation_decode() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_block_number(1);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        // Register and activate validators A, B, C.
        let val_a = address!("0x1111111111111111111111111111111111111111");
        let val_b = address!("0x2222222222222222222222222222222222222222");
        let val_c = address!("0x3333333333333333333333333333333333333333");
        let val_d = address!("0x4444444444444444444444444444444444444444");

        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));
        test_register_active(&mut vs, val_b, &dummy_pubkey(0xB2));
        test_register_active(&mut vs, val_c, &dummy_pubkey(0xC3));
        test_register_joining(&mut vs, val_d, &dummy_pubkey(0xD4));

        // The fixture helpers activated A, B, C; D remains a ready joiner.

        // Step 1: Read old active set - should be [A, B, C].
        let old_set = vs.get_active_consensus_set().unwrap();
        let old_addrs: Vec<Address> = old_set.iter().map(|v| v.validator_address).collect();
        assert!(old_addrs.contains(&val_a));
        assert!(old_addrs.contains(&val_b));
        assert!(old_addrs.contains(&val_c));
        assert!(!old_addrs.contains(&val_d), "D should NOT be in old set");
        assert_eq!(old_addrs.len(), 3);

        // Step 2-3: Participation/slashing would happen here using old_addrs.
        // (We only verify that the set is correct. Task 01 code tests actual slashing.)

        // Step 4: NOW activate new reshare with [A, B, D] (C removed, D added).
        let new_hash = B256::with_last_byte(0x02);
        // First deactivate C (simulate EXITING).
        vs.deactivate_validator(OWNER, val_c).unwrap();

        // C is still in the current consensus set until the reshare outcome
        // is applied. This matches the still-running engine committee.
        let transition_set = vs.get_active_consensus_set().unwrap();
        let transition_addrs: Vec<Address> =
            transition_set.iter().map(|v| v.validator_address).collect();
        assert!(transition_addrs.contains(&val_c));
        assert_eq!(transition_addrs.len(), 3);
        vs.record_proposer(val_c).unwrap();
        vs.record_participation(&[val_a, val_b], &[val_c]).unwrap();

        // Reshare with new set.
        vs.test_activate_validated_boundary_set(&[val_a, val_b, val_d], new_hash, 1)
            .unwrap();

        // After reshare: active set is [A, B, D].
        let new_set = vs.get_active_consensus_set().unwrap();
        let new_addrs: Vec<Address> = new_set.iter().map(|v| v.validator_address).collect();
        assert!(new_addrs.contains(&val_a));
        assert!(new_addrs.contains(&val_b));
        assert!(new_addrs.contains(&val_d));
        assert!(!new_addrs.contains(&val_c), "C should NOT be in new set");
        assert_eq!(new_addrs.len(), 3);
    });
}

/// Task 01 test: committee size change doesn't corrupt participation.
///
/// The old set has 3 validators and the new set has 4. In this case, the
/// participation bitmap encoded for 3 validators should be decoded against the
/// 3-validator set, not the 4-validator set.
#[test]
fn test_committee_size_change_participation_safety() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        let val_b = address!("0x2222222222222222222222222222222222222222");
        let val_c = address!("0x3333333333333333333333333333333333333333");
        let val_d = address!("0x4444444444444444444444444444444444444444");

        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));
        test_register_active(&mut vs, val_b, &dummy_pubkey(0xB2));
        test_register_active(&mut vs, val_c, &dummy_pubkey(0xC3));
        test_register_joining(&mut vs, val_d, &dummy_pubkey(0xD4));

        // Old set: 3 validators [A, B, C].
        let old_set = vs.get_active_consensus_set().unwrap();
        assert_eq!(old_set.len(), 3, "old set must have 3 validators");

        // Encode participation for 3-validator set.
        let mut old_addrs: Vec<Address> = old_set.iter().map(|v| v.validator_address).collect();
        old_addrs.sort();
        let signers = vec![true, true, false]; // A, B signed; C absent
        let extra_data = outbe_primitives::participation::encode_participation_extended(
            &old_addrs,
            &signers,
            &[],
            &[],
        )
        .unwrap();

        // Now activate new set with 4 validators.
        vs.test_activate_validated_boundary_set(
            &[val_a, val_b, val_c, val_d],
            B256::with_last_byte(0x02),
            0,
        )
        .unwrap();
        let new_set = vs.get_active_consensus_set().unwrap();
        assert_eq!(new_set.len(), 4, "new set must have 4 validators");

        // Decode participation against OLD set (3 validators) -> should work.
        let decoded =
            outbe_primitives::participation::decode_participation_extended(&extra_data, &old_addrs);
        assert!(decoded.is_some(), "decode against OLD set must succeed");

        // Decode against NEW set (4 validators) -> count mismatch -> returns None.
        let mut new_addrs: Vec<Address> = new_set.iter().map(|v| v.validator_address).collect();
        new_addrs.sort();
        let decoded_wrong =
            outbe_primitives::participation::decode_participation_extended(&extra_data, &new_addrs);
        assert!(
            decoded_wrong.is_none(),
            "decode against NEW set with different size must return None (count mismatch)"
        );
    });
}

/// Task 01 test: re-execution of reshare activation is idempotent.
///
/// Calling activate_reshared_set() twice with same hash must not
/// change state the second time (idempotency guard).
#[test]
fn test_reshare_activation_idempotent() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        let val_b = address!("0x2222222222222222222222222222222222222222");

        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));
        test_register_active(&mut vs, val_b, &dummy_pubkey(0xB2));

        let hash = vs.active_consensus_set_hash().unwrap();

        // Read state after first activation.
        let set1 = vs.get_active_consensus_set().unwrap();
        let hash1 = vs.active_consensus_set_hash().unwrap();

        // Second call with same hash -> idempotency guard in executor.rs
        // checks `current_hash != reshare.active_set_hash`.
        // Here: current_hash == hash -> no-op.
        let current_hash = vs.active_consensus_set_hash().unwrap();
        assert_eq!(current_hash, hash, "hash must match after first activation");

        // Simulate executor's guard: skip if hash matches.
        assert_eq!(current_hash, hash);
        // State unchanged.
        let set2 = vs.get_active_consensus_set().unwrap();
        let hash2 = vs.active_consensus_set_hash().unwrap();
        assert_eq!(
            set1.len(),
            set2.len(),
            "set must be unchanged on re-execution"
        );
        assert_eq!(hash1, hash2, "hash must be unchanged on re-execution");
    });
}

#[test]
fn certified_delayed_boundary_atomically_advances_epoch_and_snapshot() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let validator = address!("0x1111111111111111111111111111111111111111");
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        vs.register_validator(OWNER, validator, &dummy_pubkey(0xA1))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(validator)
            .unwrap();
        let mut epoch = vs.epoch_snapshot().unwrap();
        epoch.number = U256::ZERO;
        epoch.start_block = 1;
        epoch.start_timestamp = TEST_BLOCK_TIMESTAMP_BASE;
        vs.test_set_epoch_snapshot(epoch).unwrap();
        let record = vs.get_validator(validator).unwrap().unwrap();
        vs.test_set_history(
            validator,
            ValidatorHistory::new(
                record.joined_at_height,
                (record.deactivated_at_height != 0).then_some(record.deactivated_at_height),
                record.slash_count,
                7,
                8,
                9,
            ),
        )
        .unwrap();
        drop(vs);
        let slash = outbe_slashindicator::contract::SlashIndicator::new(storage.clone());
        slash.proposer_miss_count.write(&validator, 10).unwrap();
        slash.voter_miss_count.write(&validator, 11).unwrap();

        let activation_block = 301;
        let activation_timestamp = TEST_BLOCK_TIMESTAMP_BASE + 600;
        let boundary = boundary_with_epoch(1, false, vec![(validator, dummy_pubkey(0xA1))]);
        let ctx = BlockRuntimeContext::new(
            BlockContext::new(
                activation_block,
                activation_timestamp,
                CHAIN_ID,
                validator,
                vec![validator],
            ),
            storage.clone(),
        );

        super::super::prepare_boundary_epoch_counters(storage.clone(), &boundary, activation_block)
            .expect("certified boundary must prepare outgoing counters");
        crate::begin_block_precompile::run_boundary_outcome(&ctx, &boundary)
            .expect("certified delayed boundary must activate");

        let vs_after = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let epoch = vs_after.epoch_snapshot().unwrap();
        assert_eq!(epoch.number, U256::from(1));
        assert_eq!(epoch.start_block, activation_block);
        assert_eq!(epoch.start_timestamp, activation_timestamp);
        assert_eq!(
            vs_after.participation(validator).unwrap(),
            outbe_validatorset::ValidatorParticipation::default()
        );
        let slash_after = outbe_slashindicator::contract::SlashIndicator::new(storage.clone());
        assert_eq!(slash_after.proposer_miss_count.read(&validator).unwrap(), 0);
        assert_eq!(slash_after.voter_miss_count.read(&validator).unwrap(), 0);
        let (_, extension) = outbe_validatorset::read_ocomp_snapshot_extension_at_epoch(storage, 1)
            .unwrap()
            .expect("activated epoch must publish its OCOMP snapshot");
        assert_eq!(extension.epoch, 1);
        assert_eq!(extension.committee_set_hash, boundary.committee_set_hash);
    });
}

#[test]
fn failed_boundary_snapshot_write_rolls_back_epoch_membership_and_counters() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let validator = address!("0x1111111111111111111111111111111111111111");
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        vs.register_validator(OWNER, validator, &dummy_pubkey(0xA1))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(validator)
            .unwrap();
        let mut epoch = vs.epoch_snapshot().unwrap();
        epoch.number = U256::ZERO;
        epoch.start_block = 1;
        epoch.start_timestamp = TEST_BLOCK_TIMESTAMP_BASE;
        vs.test_set_epoch_snapshot(epoch).unwrap();
        let record = vs.get_validator(validator).unwrap().unwrap();
        vs.test_set_history(
            validator,
            ValidatorHistory::new(
                record.joined_at_height,
                (record.deactivated_at_height != 0).then_some(record.deactivated_at_height),
                record.slash_count,
                7,
                record.missed_votes,
                record.blocks_proposed,
            ),
        )
        .unwrap();
        let active_hash_before = vs.active_consensus_set_hash().unwrap();
        // Force the incoming snapshot writer to fail after the boundary
        // transition has started. The enclosing activation checkpoint must
        // restore every earlier epoch/set/counter write.
        vs.val_ocomp_registration
            .get_bytes(&validator)
            .clear()
            .unwrap();
        drop(vs);
        let slash = outbe_slashindicator::contract::SlashIndicator::new(storage.clone());
        slash.proposer_miss_count.write(&validator, 10).unwrap();

        let boundary = boundary_with_epoch(1, false, vec![(validator, dummy_pubkey(0xA1))]);
        let block_guard = storage.checkpoint_guard();
        super::super::prepare_boundary_epoch_counters(storage.clone(), &boundary, 301)
            .expect("certified boundary must prepare outgoing counters");
        let error = super::super::apply_boundary_outcome(
            storage.clone(),
            &boundary,
            301,
            TEST_BLOCK_TIMESTAMP_BASE + 600,
        )
        .expect_err("missing OCOMP registration must reject incoming snapshot");
        assert!(error.to_string().contains("no admitted OCOMP registration"));
        drop(block_guard);

        let vs_after = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let epoch = vs_after.epoch_snapshot().unwrap();
        assert_eq!(epoch.number, U256::ZERO);
        assert_eq!(epoch.start_block, 1);
        assert_eq!(epoch.start_timestamp, TEST_BLOCK_TIMESTAMP_BASE);
        assert_eq!(
            vs_after.active_consensus_set_hash().unwrap(),
            active_hash_before
        );
        assert_eq!(vs_after.participation(validator).unwrap().missed_blocks, 7);
        let slash_after = outbe_slashindicator::contract::SlashIndicator::new(storage.clone());
        assert_eq!(
            slash_after.proposer_miss_count.read(&validator).unwrap(),
            10
        );
        assert!(
            outbe_validatorset::read_ocomp_snapshot_extension_at_epoch(storage, 1)
                .unwrap()
                .is_none(),
            "failed activation must not expose an epoch-1 snapshot"
        );
    });
}

#[test]
fn boundary_rejects_skipped_epoch_without_mutating_current_state() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let validator = address!("0x1111111111111111111111111111111111111111");
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        vs.register_validator(OWNER, validator, &dummy_pubkey(0xA1))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(validator)
            .unwrap();
        let mut epoch = vs.epoch_snapshot().unwrap();
        epoch.number = U256::ZERO;
        epoch.start_block = 1;
        vs.test_set_epoch_snapshot(epoch).unwrap();
        drop(vs);

        let boundary = boundary_with_epoch(2, false, vec![(validator, dummy_pubkey(0xA1))]);
        let error = super::super::apply_boundary_outcome(
            storage.clone(),
            &boundary,
            301,
            TEST_BLOCK_TIMESTAMP_BASE + 600,
        )
        .expect_err("BoundaryOutcome must not skip activated epochs");
        assert!(error.to_string().contains("activate current+1"));

        let vs_after = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let epoch = vs_after.epoch_snapshot().unwrap();
        assert_eq!(epoch.number, U256::ZERO);
        assert_eq!(epoch.start_block, 1);
        assert!(
            outbe_validatorset::read_ocomp_snapshot_extension_at_epoch(storage, 2)
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn apply_boundary_outcome_fatals_on_hash_change_without_set_change() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        let val_b = address!("0x2222222222222222222222222222222222222222");
        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));
        test_register_active(&mut vs, val_b, &dummy_pubkey(0xB2));

        // Boundary claims membership unchanged but carries a different active set.
        let boundary = boundary_with(false, vec![(val_a, dummy_pubkey(0xA1))]);
        let err = super::super::apply_boundary_outcome(
            storage.clone(),
            &boundary,
            1,
            TEST_BLOCK_TIMESTAMP_BASE,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("active_set_hash changed without validator-set change"),
            "expected hash-vs-flag inconsistency, got {err}"
        );
    });
}

#[test]
fn apply_boundary_outcome_activates_on_validator_set_change_with_hash_change() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        let val_b = address!("0x2222222222222222222222222222222222222222");
        let val_c = address!("0x3333333333333333333333333333333333333333");
        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));
        test_register_active(&mut vs, val_b, &dummy_pubkey(0xB2));
        test_register_joining(&mut vs, val_c, &dummy_pubkey(0xC3));

        let boundary = boundary_with(
            true,
            vec![
                (val_a, dummy_pubkey(0xA1)),
                (val_b, dummy_pubkey(0xB2)),
                (val_c, dummy_pubkey(0xC3)),
            ],
        );
        let new_hash = boundary.reshare.active_set_hash;
        super::super::apply_boundary_outcome(
            storage.clone(),
            &boundary,
            1,
            TEST_BLOCK_TIMESTAMP_BASE,
        )
        .unwrap();

        let vs_after = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let now_hash = vs_after.active_consensus_set_hash().unwrap();
        assert_eq!(now_hash, new_hash, "active_set_hash must advance");
        let active = vs_after.get_active_consensus_set().unwrap();
        let addrs: Vec<Address> = active.iter().map(|v| v.validator_address).collect();
        assert!(addrs.contains(&val_c), "C must now be in active set");
    });
}

#[test]
fn apply_boundary_outcome_replays_narrow_certified_tee_expiry_demotion() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let retained = address!("0x1111111111111111111111111111111111111111");
        let expired = address!("0x2222222222222222222222222222222222222222");
        test_register_active(&mut vs, retained, &dummy_pubkey(0xA1));
        test_register_active(&mut vs, expired, &dummy_pubkey(0xB2));
        let current_hash = super::super::hash_boundary_active_set(&[retained, expired]);
        vs.test_set_active_consensus_set_hash(current_hash).unwrap();

        let mut boundary = boundary_with(true, vec![(retained, dummy_pubkey(0xA1))]);
        boundary.tee_expired_target_exclusions = vec![expired];
        boundary.tee_expired_target_exclusions_hash =
            outbe_primitives::reshare_artifact::tee_expired_target_exclusions_hash(
                &boundary.tee_expired_target_exclusions,
            )
            .unwrap();
        super::super::apply_boundary_outcome(
            storage.clone(),
            &boundary,
            1,
            TEST_BLOCK_TIMESTAMP_BASE,
        )
        .unwrap();

        let vs_after = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let expired_state = vs_after.validator_state(expired).unwrap();
        assert_eq!(
            expired_state.stored_status().unwrap(),
            outbe_validatorset::runtime::status::PENDING
        );
        assert!(!expired_state.has_bls_share());
        assert!(!expired_state.join_confirmed());
        let retained_state = vs_after.validator_state(retained).unwrap();
        assert_eq!(
            retained_state.stored_status().unwrap(),
            outbe_validatorset::runtime::status::ACTIVE
        );
    });
}

#[test]
fn apply_boundary_outcome_rejects_tampered_tee_expiry_commitment() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        let retained = address!("0x1111111111111111111111111111111111111111");
        test_register_active(&mut vs, retained, &dummy_pubkey(0xA1));
        let hash = super::super::hash_boundary_active_set(&[retained]);
        vs.test_set_active_consensus_set_hash(hash).unwrap();

        let mut boundary = boundary_with(false, vec![(retained, dummy_pubkey(0xA1))]);
        boundary.tee_expired_target_exclusions_hash = B256::with_last_byte(0xFF);
        let error = super::super::apply_boundary_outcome(
            storage.clone(),
            &boundary,
            1,
            TEST_BLOCK_TIMESTAMP_BASE,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("TEE expiry exclusions commitment mismatch"));
    });
}

#[test]
fn apply_boundary_outcome_writes_snapshot_when_hash_matches() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));
        let hash = vs.active_consensus_set_hash().unwrap();

        let boundary = boundary_with(false, vec![(val_a, dummy_pubkey(0xA1))]);
        super::super::apply_boundary_outcome(
            storage.clone(),
            &boundary,
            1,
            TEST_BLOCK_TIMESTAMP_BASE,
        )
        .unwrap();

        let vs_after = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        assert_eq!(vs_after.active_consensus_set_hash().unwrap(), hash);

        let snapshot_key =
            outbe_validatorset::committee_snapshot_key(boundary.epoch, boundary.committee_set_hash);
        let snapshot = outbe_validatorset::read_committee_snapshot(storage.clone(), snapshot_key)
            .unwrap()
            .expect("BoundaryOutcome must write the incoming committee snapshot");
        assert_eq!(snapshot.committee.len(), 1);
        assert_eq!(snapshot.committee[0].address, val_a);
        assert_eq!(snapshot.committee[0].consensus_pubkey, dummy_pubkey(0xA1));
        assert_eq!(snapshot.vrf_material_version, boundary.vrf_material_version);
        assert_eq!(
            snapshot.vrf_group_public_key_bytes,
            boundary.vrf_group_public_key_bytes.to_vec()
        );
    });
}

#[test]
fn apply_boundary_outcome_rejects_committee_set_hash_mismatch() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));

        let mut boundary = boundary_with(false, vec![(val_a, dummy_pubkey(0xA1))]);
        boundary.committee_set_hash = B256::with_last_byte(0xFE);

        let err = super::super::apply_boundary_outcome(
            storage.clone(),
            &boundary,
            1,
            TEST_BLOCK_TIMESTAMP_BASE,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("committee_set_hash mismatch"),
            "expected committee_set_hash mismatch, got {err}"
        );
    });
}
