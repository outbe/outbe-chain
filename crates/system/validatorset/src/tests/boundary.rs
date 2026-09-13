use super::*;

fn admit_pending(vs: &mut ValidatorSet<'_>, validator: Address, key_seed: u8) {
    vs.mark_pending(validator).unwrap();
    confirm_ready(vs, validator, key_seed);
}

// ---------------------------------------------------------------------------
// 9. test_update_epoch
// ---------------------------------------------------------------------------
#[test]
fn test_update_epoch() {
    let val_addr = address!("0x0000000000000000000000000000000000000091");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val_addr, &dummy_consensus_pubkey(91))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(val_addr)
            .unwrap();
        vs.val_has_bls_share.write(&val_addr, true).unwrap();

        // Accumulate some stats
        vs.record_proposer(val_addr).unwrap();
        vs.record_missed_block(val_addr).unwrap();
        vs.record_participation(&[], &[val_addr]).unwrap();

        assert_eq!(vs.val_blocks_proposed.read(&val_addr).unwrap(), 1);
        assert_eq!(vs.val_missed_blocks.read(&val_addr).unwrap(), 1);
        assert_eq!(vs.val_missed_votes.read(&val_addr).unwrap(), 1);
        assert_eq!(vs.epoch_start_block.read().unwrap(), 0);
        assert_eq!(vs.epoch_number.read().unwrap(), 0);

        vs.update_epoch(5000, 77).unwrap();

        // Counters reset
        assert_eq!(vs.val_blocks_proposed.read(&val_addr).unwrap(), 0);
        assert_eq!(vs.val_missed_blocks.read(&val_addr).unwrap(), 0);
        assert_eq!(vs.val_missed_votes.read(&val_addr).unwrap(), 0);
        assert_eq!(vs.epoch_start_block.read().unwrap(), 77);

        // Epoch number incremented, timestamp and start block updated.
        assert_eq!(vs.epoch_number.read().unwrap(), 1);
        assert_eq!(vs.epoch_start_timestamp.read().unwrap(), 5000);
    });
}

#[test]
fn test_epoch_boundary_uses_block_height_not_timestamp() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let vs = ValidatorSet::new(storage.clone());
        vs.config_epoch_length_blocks.write(100).unwrap();
        vs.epoch_start_block.write(25).unwrap();
        vs.epoch_start_timestamp.write(1_000).unwrap();

        assert!(
            !crate::hooks::is_epoch_boundary(storage.clone(), 124).unwrap(),
            "block before start+length must not transition even if wall-clock advanced"
        );
        assert!(
            crate::hooks::is_epoch_boundary(storage.clone(), 125).unwrap(),
            "block at start+length must transition"
        );
    });
}

#[test]
fn test_transition_epoch_updates_start_block_and_timestamp() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let vs = ValidatorSet::new(storage.clone());
        vs.config_epoch_length_blocks.write(100).unwrap();

        crate::hooks::transition_epoch(storage.clone(), 1_234, 456).unwrap();

        let vs = ValidatorSet::new(storage);
        assert_eq!(vs.epoch_number.read().unwrap(), 1);
        assert_eq!(vs.epoch_start_timestamp.read().unwrap(), 1_234);
        assert_eq!(vs.epoch_start_block.read().unwrap(), 456);
    });
}

// ---------------------------------------------------------------------------
// 12. test_consensus_set
// ---------------------------------------------------------------------------
#[test]
fn test_consensus_set() {
    let val1 = address!("0x00000000000000000000000000000000000000C1");
    let val2 = address!("0x00000000000000000000000000000000000000C2");
    let val3 = address!("0x00000000000000000000000000000000000000C3");
    let group_key = B256::with_last_byte(0xFF);

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val1, &dummy_consensus_pubkey(0xC1))
            .unwrap();
        vs.register_validator(OWNER, val2, &dummy_consensus_pubkey(0xC2))
            .unwrap();
        vs.register_validator(OWNER, val3, &dummy_consensus_pubkey(0xC3))
            .unwrap();

        // All start as REGISTERED
        assert_eq!(vs.val_status.read(&val1).unwrap(), status::REGISTERED);
        assert_eq!(vs.val_status.read(&val2).unwrap(), status::REGISTERED);

        // Stake/readiness fixtures make val1 and val2 eligible for boundary
        // inclusion. val3 deliberately remains WaitingForStake.
        activate_for_test(vs, val1);
        activate_for_test(vs, val2);

        // Commit a same-member reshared set with val1 and val2 (not val3).
        vs.activate_reshared_set(&[val1, val2], group_key).unwrap();

        // val1 and val2 should be ACTIVE with has_bls_share
        assert_eq!(vs.val_status.read(&val1).unwrap(), status::ACTIVE);
        assert_eq!(vs.val_status.read(&val2).unwrap(), status::ACTIVE);
        assert!(vs.val_has_bls_share.read(&val1).unwrap());
        assert!(vs.val_has_bls_share.read(&val2).unwrap());

        // val3 remains REGISTERED, no BLS share
        assert_eq!(vs.val_status.read(&val3).unwrap(), status::REGISTERED);
        assert!(!vs.val_has_bls_share.read(&val3).unwrap());

        // Consensus set contains only val1 and val2
        let consensus_set = vs.get_active_consensus_set().unwrap();
        assert_eq!(consensus_set.len(), 2);
        assert_eq!(vs.active_consensus_count().unwrap(), 2);

        // pending_set_change should be cleared
        assert!(!vs.pending_set_change.read().unwrap());

        // is_consensus_participant checks
        assert!(vs.is_consensus_participant(val1).unwrap());
        assert!(vs.is_consensus_participant(val2).unwrap());
        assert!(!vs.is_consensus_participant(val3).unwrap());
    });
}

// ---------------------------------------------------------------------------
// 13. test_exiting_to_unbonding_via_reshare
// ---------------------------------------------------------------------------
#[test]
fn test_exiting_to_unbonding_via_reshare() {
    let val1 = address!("0x00000000000000000000000000000000000000D1");
    let val2 = address!("0x00000000000000000000000000000000000000D2");
    let group_key = B256::with_last_byte(0xFE);

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val1, &dummy_consensus_pubkey(0xD1))
            .unwrap();
        vs.register_validator(OWNER, val2, &dummy_consensus_pubkey(0xD2))
            .unwrap();
        admit_pending(vs, val1, 0xD1);
        admit_pending(vs, val2, 0xD2);

        activate_for_test(vs, val1);
        activate_for_test(vs, val2);

        // First reshare: both remain active.
        vs.activate_reshared_set(&[val1, val2], group_key).unwrap();
        assert_eq!(vs.val_status.read(&val1).unwrap(), status::ACTIVE);
        assert_eq!(vs.val_status.read(&val2).unwrap(), status::ACTIVE);

        // val2 requests deactivation -> EXITING
        vs.deactivate_validator(OWNER, val2).unwrap();
        assert_eq!(vs.val_status.read(&val2).unwrap(), status::EXITING);

        // Second reshare: only val1 in new set
        let group_key2 = B256::with_last_byte(0xFD);
        vs.activate_reshared_set(&[val1], group_key2).unwrap();

        // val1 still ACTIVE with BLS share
        assert_eq!(vs.val_status.read(&val1).unwrap(), status::ACTIVE);
        assert!(vs.val_has_bls_share.read(&val1).unwrap());

        // val2 transitioned from EXITING -> UNBONDING, no BLS share
        assert_eq!(vs.val_status.read(&val2).unwrap(), status::UNBONDING);
        assert!(!vs.val_has_bls_share.read(&val2).unwrap());
    });
}

#[test]
fn test_deactivated_validator_stays_current_consensus_participant_until_reshare() {
    let val1 = address!("0x0000000000000000000000000000000000000CD1");
    let val2 = address!("0x0000000000000000000000000000000000000CD2");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val1, &dummy_consensus_pubkey(0xD1))
            .unwrap();
        vs.register_validator(OWNER, val2, &dummy_consensus_pubkey(0xD2))
            .unwrap();
        activate_for_test(vs, val1);
        activate_for_test(vs, val2);
        vs.activate_reshared_set(&[val1, val2], B256::with_last_byte(0xD1))
            .unwrap();

        vs.deactivate_validator(OWNER, val2).unwrap();

        assert_eq!(vs.val_status.read(&val2).unwrap(), status::EXITING);
        assert!(vs.val_has_bls_share.read(&val2).unwrap());
        assert!(vs.is_consensus_participant(val2).unwrap());
        assert_eq!(vs.active_consensus_count().unwrap(), 2);

        let current_set = vs.get_active_consensus_set().unwrap();
        let current_addrs: Vec<_> = current_set.iter().map(|v| v.validator_address).collect();
        assert!(current_addrs.contains(&val1));
        assert!(current_addrs.contains(&val2));

        vs.record_proposer(val2).unwrap();
        vs.record_participation(&[val1], &[val2]).unwrap();
        assert_eq!(vs.val_blocks_proposed.read(&val2).unwrap(), 1);
        assert_eq!(vs.val_missed_votes.read(&val2).unwrap(), 1);

        vs.activate_reshared_set(&[val1], B256::with_last_byte(0xD2))
            .unwrap();
        assert_eq!(vs.val_status.read(&val2).unwrap(), status::UNBONDING);
        assert!(!vs.val_has_bls_share.read(&val2).unwrap());
        assert!(!vs.is_consensus_participant(val2).unwrap());
    });
}

#[test]
fn test_force_exited_validator_stays_current_consensus_participant_until_reshare() {
    let val1 = address!("0x0000000000000000000000000000000000000CF1");
    let val2 = address!("0x0000000000000000000000000000000000000CF2");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val1, &dummy_consensus_pubkey(0xF1))
            .unwrap();
        vs.register_validator(OWNER, val2, &dummy_consensus_pubkey(0xF2))
            .unwrap();
        activate_for_test(vs, val1);
        activate_for_test(vs, val2);
        vs.activate_reshared_set(&[val1, val2], B256::with_last_byte(0xF1))
            .unwrap();

        vs.force_exit_validator(val2).unwrap();

        assert_eq!(vs.val_status.read(&val2).unwrap(), status::EXITING);
        assert!(vs.val_has_bls_share.read(&val2).unwrap());
        assert!(vs.is_consensus_participant(val2).unwrap());
        assert_eq!(vs.val_slash_count.read(&val2).unwrap(), 1);

        vs.record_proposer(val2).unwrap();
        vs.record_participation(&[val1], &[val2]).unwrap();

        vs.force_exit_validator(val2).unwrap();
        assert!(vs.val_has_bls_share.read(&val2).unwrap());
        assert!(vs.is_consensus_participant(val2).unwrap());
        assert_eq!(vs.val_slash_count.read(&val2).unwrap(), 1);

        vs.activate_reshared_set(&[val1], B256::with_last_byte(0xF2))
            .unwrap();
        assert_eq!(vs.val_status.read(&val2).unwrap(), status::UNBONDING);
        assert!(!vs.is_consensus_participant(val2).unwrap());
    });
}

#[test]
fn post_freeze_exit_is_retained_then_excluded_at_a_later_boundary() {
    let survivor = address!("0x0000000000000000000000000000000000000E11");
    let exiting = address!("0x0000000000000000000000000000000000000E12");
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    // The target was frozen at height 10; the exit request lands afterwards.
    storage.set_block_number(11);

    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = ValidatorSet::new(storage);
        vs.config_owner.write(OWNER).unwrap();
        vs.config_max_validators.write(10).unwrap();
        vs.register_validator(OWNER, survivor, &dummy_consensus_pubkey(0xE1))
            .unwrap();
        vs.register_validator(OWNER, exiting, &dummy_consensus_pubkey(0xE2))
            .unwrap();
        activate_staked_for_test(&mut vs, survivor);
        activate_staked_for_test(&mut vs, exiting);

        vs.deactivate_validator(OWNER, exiting).unwrap();
        assert!(matches!(
            vs.validator_lifecycle(exiting).unwrap(),
            ValidatorLifecycle::Exiting(_)
        ));

        // The height-10 frozen target still contains the validator. Retaining it
        // is required: excluding it here would apply post-freeze state to a
        // historical target and make honest nodes disagree about the boundary.
        let retained_hash = B256::with_last_byte(0xE1);
        vs.test_activate_validated_boundary_set(&[survivor, exiting], retained_hash, 10)
            .unwrap();
        assert!(matches!(
            vs.validator_lifecycle(exiting).unwrap(),
            ValidatorLifecycle::Exiting(_)
        ));
        assert!(vs.is_consensus_participant(exiting).unwrap());
        assert!(vs.has_pending_set_change().unwrap());

        // Once the exit is visible at the freeze height, retaining it is an
        // invalid artifact. Verify the failed plan leaves every observed field
        // unchanged before applying the correct exclusion boundary.
        let survivor_before = vs.validator_lifecycle(survivor).unwrap();
        let exiting_before = vs.validator_lifecycle(exiting).unwrap();
        let pending_before = vs.has_pending_set_change().unwrap();
        let hash_before = vs.active_consensus_set_hash().unwrap();
        let err = vs
            .test_activate_validated_boundary_set(
                &[survivor, exiting],
                B256::with_last_byte(0xEE),
                11,
            )
            .unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::Fatal(message)
                if message.contains("retained validator")
                    && message.contains("exited at 11 before freeze 11")
        ));
        assert_eq!(vs.validator_lifecycle(survivor).unwrap(), survivor_before);
        assert_eq!(vs.validator_lifecycle(exiting).unwrap(), exiting_before);
        assert_eq!(vs.has_pending_set_change().unwrap(), pending_before);
        assert_eq!(vs.active_consensus_set_hash().unwrap(), hash_before);

        let excluded_hash = B256::with_last_byte(0xE2);
        vs.test_activate_validated_boundary_set(&[survivor], excluded_hash, 12)
            .unwrap();
        assert!(matches!(
            vs.validator_lifecycle(exiting).unwrap(),
            ValidatorLifecycle::Unbonding(_)
        ));
        assert!(!vs.is_consensus_participant(exiting).unwrap());
        assert!(!vs.has_pending_set_change().unwrap());
        assert_eq!(vs.active_consensus_set_hash().unwrap(), excluded_hash);
    });
}

#[test]
fn post_freeze_jail_is_retained_then_excluded_at_a_later_boundary() {
    let survivor = address!("0x0000000000000000000000000000000000000A11");
    let jailed = address!("0x0000000000000000000000000000000000000A12");
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    // The target was frozen at height 10; punishment lands afterwards.
    storage.set_block_number(11);

    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = ValidatorSet::new(storage);
        vs.config_owner.write(OWNER).unwrap();
        vs.config_max_validators.write(10).unwrap();
        vs.register_validator(OWNER, survivor, &dummy_consensus_pubkey(0xA1))
            .unwrap();
        vs.register_validator(OWNER, jailed, &dummy_consensus_pubkey(0xA2))
            .unwrap();
        activate_staked_for_test(&mut vs, survivor);
        activate_staked_for_test(&mut vs, jailed);

        vs.jail_validator(jailed).unwrap();
        assert!(matches!(
            vs.validator_lifecycle(jailed).unwrap(),
            ValidatorLifecycle::JailRetained(_)
        ));
        assert_eq!(vs.val_jailed_at_height.read(&jailed).unwrap(), 11);

        // The historical height-10 target still contains the newly jailed
        // validator, so this boundary retains its live-share accountability.
        let retained_hash = B256::with_last_byte(0xA1);
        vs.test_activate_validated_boundary_set(&[survivor, jailed], retained_hash, 10)
            .unwrap();
        assert!(matches!(
            vs.validator_lifecycle(jailed).unwrap(),
            ValidatorLifecycle::JailRetained(_)
        ));
        assert!(vs.is_consensus_participant(jailed).unwrap());
        assert!(vs.has_pending_set_change().unwrap());

        // At a freeze that can see the jail, retaining it must fail without
        // committing lifecycle/hash/signal changes.
        let survivor_before = vs.validator_lifecycle(survivor).unwrap();
        let jailed_before = vs.validator_lifecycle(jailed).unwrap();
        let pending_before = vs.has_pending_set_change().unwrap();
        let hash_before = vs.active_consensus_set_hash().unwrap();
        let err = vs
            .test_activate_validated_boundary_set(
                &[survivor, jailed],
                B256::with_last_byte(0xAA),
                11,
            )
            .unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::Fatal(message)
                if message.contains("retained validator")
                    && message.contains("jailed at 11 before freeze 11")
        ));
        assert_eq!(vs.validator_lifecycle(survivor).unwrap(), survivor_before);
        assert_eq!(vs.validator_lifecycle(jailed).unwrap(), jailed_before);
        assert_eq!(vs.has_pending_set_change().unwrap(), pending_before);
        assert_eq!(vs.active_consensus_set_hash().unwrap(), hash_before);

        let excluded_hash = B256::with_last_byte(0xA2);
        vs.test_activate_validated_boundary_set(&[survivor], excluded_hash, 12)
            .unwrap();
        assert!(matches!(
            vs.validator_lifecycle(jailed).unwrap(),
            ValidatorLifecycle::Jail(_)
        ));
        assert!(!vs.is_consensus_participant(jailed).unwrap());
        assert!(!vs.has_pending_set_change().unwrap());
        assert_eq!(vs.active_consensus_set_hash().unwrap(), excluded_hash);
    });
}

// ---------------------------------------------------------------------------
// 14. test_pending_set_change
// ---------------------------------------------------------------------------
#[test]
fn test_pending_set_change() {
    let val_addr = address!("0x00000000000000000000000000000000000000E1");

    with_vs_configured(10, |vs| {
        // Initially no pending change
        assert!(!vs.has_pending_set_change().unwrap());

        // Registration triggers pending_set_change
        vs.register_validator(OWNER, val_addr, &dummy_consensus_pubkey(0xE1))
            .unwrap();
        assert!(vs.has_pending_set_change().unwrap());

        activate_for_test(vs, val_addr);

        // A valid same-member boundary clears it.
        let group_key = B256::with_last_byte(0xEE);
        vs.activate_reshared_set(&[val_addr], group_key).unwrap();
        assert!(!vs.has_pending_set_change().unwrap());

        // Forced exit triggers pending_set_change
        vs.force_exit_validator(val_addr).unwrap();
        assert!(vs.has_pending_set_change().unwrap());
    });
}

// ---------------------------------------------------------------------------
// 14b. test_pending_set_change_missed_validator
// ---------------------------------------------------------------------------
#[test]
fn test_boundary_rejects_missed_active_validator_atomically() {
    let val1 = address!("0x00000000000000000000000000000000000000A1");
    let val2 = address!("0x00000000000000000000000000000000000000A2");
    let val3 = address!("0x00000000000000000000000000000000000000A3");
    let group_key1 = B256::with_last_byte(0xAA);
    let group_key2 = B256::with_last_byte(0xBB);

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val1, &dummy_consensus_pubkey(0xA1))
            .unwrap();
        vs.register_validator(OWNER, val2, &dummy_consensus_pubkey(0xA2))
            .unwrap();
        vs.register_validator(OWNER, val3, &dummy_consensus_pubkey(0xA3))
            .unwrap();
        admit_pending(vs, val1, 0xA1);
        admit_pending(vs, val2, 0xA2);
        admit_pending(vs, val3, 0xA3);

        for val in [val1, val2, val3] {
            activate_for_test(vs, val);
        }

        // First reshare: all 3 validators participate -> all ACTIVE.
        vs.activate_reshared_set(&[val1, val2, val3], group_key1)
            .unwrap();
        assert_eq!(vs.val_status.read(&val1).unwrap(), status::ACTIVE);
        assert_eq!(vs.val_status.read(&val2).unwrap(), status::ACTIVE);
        assert_eq!(vs.val_status.read(&val3).unwrap(), status::ACTIVE);
        // All covered -> pending cleared
        assert!(!vs.has_pending_set_change().unwrap());

        // A purported validated boundary may not silently omit an existing
        // ACTIVE participant. The planner rejects it before writing anything.
        assert!(matches!(
            vs.activate_reshared_set(&[val1, val2], group_key2),
            Err(PrecompileError::Fatal(_))
        ));

        // The complete prior committee, hash, and repair signal are unchanged.
        assert!(vs.val_has_bls_share.read(&val1).unwrap());
        assert!(vs.val_has_bls_share.read(&val2).unwrap());
        assert_eq!(vs.val_status.read(&val3).unwrap(), status::ACTIVE);
        assert!(vs.val_has_bls_share.read(&val3).unwrap());
        assert_eq!(vs.active_consensus_set_hash.read().unwrap(), group_key1);
        assert!(!vs.has_pending_set_change().unwrap());
    });
}

#[test]
fn certified_tee_expiry_demotes_active_and_clears_pending_readiness() {
    with_vs_configured(128, |vs| {
        let active = address!("0x1111111111111111111111111111111111111111");
        let pending = address!("0x2222222222222222222222222222222222222222");
        vs.register_validator(OWNER, active, &dummy_consensus_pubkey(0x01))
            .unwrap();
        vs.activate_validator(active).unwrap();
        vs.register_validator(OWNER, pending, &dummy_consensus_pubkey(0x02))
            .unwrap();
        vs.mark_pending(pending).unwrap();
        confirm_ready(vs, pending, 0x32);

        vs.test_activate_validated_boundary_set_with_expiry_exclusions(
            &[],
            B256::with_last_byte(0xA2),
            u64::MAX,
            &[active, pending],
        )
        .unwrap();

        for validator in [active, pending] {
            assert_eq!(vs.val_status.read(&validator).unwrap(), status::PENDING);
            assert!(!vs.val_has_bls_share.read(&validator).unwrap());
            assert!(!vs.val_join_confirmed.read(&validator).unwrap());
        }
        assert!(vs.get_reshare_target_set().unwrap().is_empty());

        // Renewal alone does not touch ValidatorSet readiness. Explicit operator
        // confirmation is required before either validator can return to target.
        confirm_ready(vs, active, 0x31);
        confirm_ready(vs, pending, 0x32);
        let target: Vec<_> = vs
            .get_reshare_target_set()
            .unwrap()
            .into_iter()
            .map(|record| record.validator_address)
            .collect();
        assert_eq!(target, vec![active, pending]);
    });
}

#[test]
fn ordinary_dkg_omission_without_expiry_proof_is_rejected_atomically() {
    with_vs_configured(128, |vs| {
        let active = address!("0x1111111111111111111111111111111111111111");
        vs.register_validator(OWNER, active, &dummy_consensus_pubkey(0x01))
            .unwrap();
        vs.activate_validator(active).unwrap();
        let hash_before = vs.active_consensus_set_hash().unwrap();
        assert!(vs
            .test_activate_validated_boundary_set_with_expiry_exclusions(
                &[],
                B256::with_last_byte(0xB2),
                u64::MAX,
                &[],
            )
            .is_err());

        assert_eq!(vs.val_status.read(&active).unwrap(), status::ACTIVE);
        assert!(vs.val_has_bls_share.read(&active).unwrap());
        assert_eq!(vs.active_consensus_set_hash().unwrap(), hash_before);
    });
}

#[test]
fn expiry_branch_rejects_contradictory_duplicate_and_unknown_authority() {
    with_vs_configured(128, |vs| {
        let active = address!("0x1111111111111111111111111111111111111111");
        let unknown = address!("0x9999999999999999999999999999999999999999");
        vs.register_validator(OWNER, active, &dummy_consensus_pubkey(0x01))
            .unwrap();
        vs.activate_validator(active).unwrap();

        assert!(vs
            .test_activate_validated_boundary_set_with_expiry_exclusions(
                &[active],
                B256::with_last_byte(0xC2),
                u64::MAX,
                &[active],
            )
            .is_err());
        assert!(vs
            .test_activate_validated_boundary_set_with_expiry_exclusions(
                &[],
                B256::with_last_byte(0xC2),
                u64::MAX,
                &[active, active],
            )
            .is_err());
        assert!(vs
            .test_activate_validated_boundary_set_with_expiry_exclusions(
                &[],
                B256::with_last_byte(0xC2),
                u64::MAX,
                &[unknown],
            )
            .is_err());

        assert_eq!(vs.val_status.read(&active).unwrap(), status::ACTIVE);
        assert!(vs.val_has_bls_share.read(&active).unwrap());
    });
}
