use super::*;

#[test]
fn tee_expiry_jail_is_non_slashing_and_idempotent() {
    let validator = address!("0x0000000000000000000000000000000000000A13");
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_block_number(17);

    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = ValidatorSet::new(storage);
        vs.config_owner.write(OWNER).unwrap();
        vs.config_max_validators.write(10).unwrap();
        vs.register_validator(OWNER, validator, &dummy_consensus_pubkey(0xA3))
            .unwrap();
        activate_staked_for_test(&mut vs, validator);

        let before = vs.get_validator(validator).unwrap().unwrap();
        let before_stake = before.stake;
        let before_slash_count = before.slash_count;

        assert!(vs.jail_validator_for_tee_expiry(validator).unwrap());
        let jailed = vs.get_validator(validator).unwrap().unwrap();
        assert_eq!(jailed.status, status::JAILED);
        assert_eq!(jailed.stake, before_stake);
        assert_eq!(jailed.slash_count, before_slash_count);
        assert_eq!(vs.val_jailed_at_height.read(&validator).unwrap(), 17);
        assert!(vs.has_pending_set_change().unwrap());

        assert!(!vs.jail_validator_for_tee_expiry(validator).unwrap());
        let replayed = vs.get_validator(validator).unwrap().unwrap();
        assert_eq!(replayed, jailed);
        assert_eq!(replayed.stake, before_stake);
        assert_eq!(replayed.slash_count, before_slash_count);

        vs.test_activate_validated_boundary_set(&[], B256::ZERO, 17)
            .unwrap();
        let excluded = vs.get_validator(validator).unwrap().unwrap();
        assert_eq!(excluded.status, status::JAILED);
        assert!(!excluded.has_bls_share);
        assert_eq!(excluded.stake, before_stake);
        assert_eq!(excluded.slash_count, before_slash_count);
        assert!(!vs.is_consensus_participant(validator).unwrap());
    });
}

#[test]
fn test_jail_validator_from_active() {
    with_vs_configured(128, |vs| {
        let v = address!("0x1111111111111111111111111111111111111111");
        vs.register_validator(OWNER, v, &dummy_consensus_pubkey(0x01))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(v).unwrap();
        vs.val_has_bls_share.write(&v, true).unwrap();
        assert!(vs.is_consensus_participant(v).unwrap());

        vs.jail_validator(v).unwrap();
        assert_eq!(vs.val_status.read(&v).unwrap(), status::JAILED);
        assert_eq!(vs.val_slash_count.read(&v).unwrap(), 1);
        assert!(vs.has_pending_set_change().unwrap());
        // Still accountable in the live committee until the next reshare clears the
        // share (same as EXITING) - so current-epoch metadata does not Fatal.
        assert!(vs.is_consensus_participant(v).unwrap());
        // Excluded from the NEXT reshare target.
        assert!(!vs
            .get_reshare_target_set()
            .unwrap()
            .iter()
            .any(|r| r.validator_address == v));
        // Still admitted to P2P as a non-voting follower so it keeps syncing.
        assert!(vs
            .get_admitted_non_consensus_validators()
            .unwrap()
            .iter()
            .any(|r| r.validator_address == v));
    });
}

#[test]
fn test_jailed_loses_share_at_reshare() {
    with_vs_configured(128, |vs| {
        let v = address!("0x1111111111111111111111111111111111111111");
        vs.register_validator(OWNER, v, &dummy_consensus_pubkey(0x01))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(v).unwrap();
        vs.val_has_bls_share.write(&v, true).unwrap();
        vs.jail_validator(v).unwrap();

        // A reshare that does not include the jailed validator clears its share
        // (clear-all loop) and it stops being a participant - but stays JAILED.
        vs.activate_reshared_set(&[], B256::ZERO).unwrap();
        assert!(!vs.val_has_bls_share.read(&v).unwrap());
        assert!(!vs.is_consensus_participant(v).unwrap());
        assert_eq!(vs.val_status.read(&v).unwrap(), status::JAILED);
    });
}

#[test]
fn test_unjail_after_exclusion_returns_to_unconfirmed_pending() {
    with_vs_configured(128, |vs| {
        let v = address!("0x1111111111111111111111111111111111111111");
        vs.register_validator(OWNER, v, &dummy_consensus_pubkey(0x01))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(v).unwrap();
        vs.val_has_bls_share.write(&v, true).unwrap();
        vs.val_missed_blocks.write(&v, 7).unwrap();
        vs.val_missed_votes.write(&v, 9).unwrap();
        vs.jail_validator(v).unwrap();

        // A retained jailed member cannot be unjailed before the committee
        // boundary has removed its old share.
        assert!(vs.unjail_to_pending(v).is_err());
        vs.activate_reshared_set(&[], B256::ZERO).unwrap();
        vs.unjail_to_pending(v).unwrap();
        assert_eq!(vs.val_status.read(&v).unwrap(), status::PENDING);
        assert_eq!(vs.val_jailed_at_height.read(&v).unwrap(), 0);
        assert_eq!(vs.val_missed_blocks.read(&v).unwrap(), 0);
        assert_eq!(vs.val_missed_votes.read(&v).unwrap(), 0);
        // Must re-confirm readiness before re-entering the reshare target.
        assert!(!vs.val_join_confirmed.read(&v).unwrap());
        assert!(!vs
            .get_reshare_target_set()
            .unwrap()
            .iter()
            .any(|r| r.validator_address == v));
        vs.admit_validator_for_boundary_for_test(v)
            .expect("unjail replays the pinned OCOMP registration");
        assert!(vs
            .get_reshare_target_set()
            .unwrap()
            .iter()
            .any(|r| r.validator_address == v));
    });
}

#[test]
fn excluded_jail_accepts_late_finalized_participation_then_unjail_clears_it() {
    with_vs_configured(128, |vs| {
        let v = address!("0x1111111111111111111111111111111111111111");
        vs.register_validator(OWNER, v, &dummy_consensus_pubkey(0x01))
            .unwrap();
        vs.activate_validator(v).unwrap();
        vs.val_has_bls_share.write(&v, true).unwrap();
        vs.val_missed_blocks.write(&v, 7).unwrap();
        vs.val_missed_votes.write(&v, 9).unwrap();
        vs.jail_validator(v).unwrap();

        vs.activate_reshared_set(&[], B256::ZERO).unwrap();
        assert_eq!(vs.val_missed_blocks.read(&v).unwrap(), 0);
        assert_eq!(vs.val_missed_votes.read(&v).unwrap(), 0);

        // A late certificate for the historical committee may arrive after the
        // exclusion boundary. The excluded Jail state must remain decodable.
        vs.record_finalized_participation(&[], &[v]).unwrap();
        assert_eq!(vs.val_missed_votes.read(&v).unwrap(), 1);
        let jailed = vs.validator_state(v).unwrap();
        assert!(matches!(jailed.lifecycle(), ValidatorLifecycle::Jail(_)));
        assert_eq!(jailed.history().unwrap().missed_votes(), 1);

        // Rejoining always starts from a clean per-epoch miss slate, including
        // late historical misses recorded after exclusion.
        vs.unjail_to_pending(v).unwrap();
        assert_eq!(vs.val_missed_blocks.read(&v).unwrap(), 0);
        assert_eq!(vs.val_missed_votes.read(&v).unwrap(), 0);
    });
}

#[test]
fn test_unjail_requires_jailed_status() {
    with_vs_configured(128, |vs| {
        let active = address!("0x1111111111111111111111111111111111111111");
        vs.register_validator(OWNER, active, &dummy_consensus_pubkey(0x01))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(active).unwrap();
        assert!(
            vs.unjail_to_pending(active).is_err(),
            "cannot unjail an ACTIVE validator"
        );
        let reg = address!("0x2222222222222222222222222222222222222222");
        vs.register_validator(OWNER, reg, &dummy_consensus_pubkey(0x02))
            .unwrap();
        assert!(
            vs.unjail_to_pending(reg).is_err(),
            "cannot unjail a REGISTERED validator"
        );
    });
}

#[test]
fn test_unjail_cooldown_blocks() {
    use outbe_primitives::storage::StorageHandle;
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let v = address!("0x1111111111111111111111111111111111111111");
    storage.set_block_number(100);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_unjail_cooldown_blocks.write(50).unwrap();
        vs.register_validator(OWNER, v, &dummy_consensus_pubkey(0x01))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(v).unwrap();
        vs.val_has_bls_share.write(&v, true).unwrap();
        vs.jail_validator(v).unwrap();
        assert_eq!(vs.val_jailed_at_height.read(&v).unwrap(), 100);
        vs.activate_reshared_set(&[], B256::ZERO).unwrap();
        // 100 < 100 + 50 -> still in cooldown.
        assert!(
            vs.unjail_to_pending(v).is_err(),
            "unjail must fail before the cooldown elapses"
        );
    });
    storage.set_block_number(150);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = ValidatorSet::new(storage.clone());
        // 150 >= 100 + 50 -> cooldown elapsed.
        vs.unjail_to_pending(v).unwrap();
        assert_eq!(vs.val_status.read(&v).unwrap(), status::PENDING);
    });
}
