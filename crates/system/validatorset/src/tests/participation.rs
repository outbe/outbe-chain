use super::*;

// ---------------------------------------------------------------------------
// 7. test_record_proposer
// ---------------------------------------------------------------------------
#[test]
fn test_record_proposer() {
    let val_addr = address!("0x6666666666666666666666666666666666666666");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val_addr, &dummy_consensus_pubkey(6))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(val_addr)
            .unwrap();
        vs.val_has_bls_share.write(&val_addr, true).unwrap();

        assert_eq!(vs.val_blocks_proposed.read(&val_addr).unwrap(), 0);
        assert_eq!(vs.epoch_start_block.read().unwrap(), 0);

        vs.record_proposer(val_addr).unwrap();
        assert_eq!(vs.val_blocks_proposed.read(&val_addr).unwrap(), 1);
        assert_eq!(vs.epoch_start_block.read().unwrap(), 0);

        vs.record_proposer(val_addr).unwrap();
        assert_eq!(vs.val_blocks_proposed.read(&val_addr).unwrap(), 2);
        assert_eq!(vs.epoch_start_block.read().unwrap(), 0);
    });
}

// ---------------------------------------------------------------------------
// 8. test_record_participation
// ---------------------------------------------------------------------------
#[test]
fn test_record_participation() {
    let val1 = address!("0x0000000000000000000000000000000000000071");
    let val2 = address!("0x0000000000000000000000000000000000000072");
    let val3 = address!("0x0000000000000000000000000000000000000073");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val1, &dummy_consensus_pubkey(71))
            .unwrap();
        vs.register_validator(OWNER, val2, &dummy_consensus_pubkey(72))
            .unwrap();
        vs.register_validator(OWNER, val3, &dummy_consensus_pubkey(73))
            .unwrap();
        for val in [val1, val2, val3] {
            vs.activate_validator_via_boundary_for_test(val).unwrap();
            vs.val_has_bls_share.write(&val, true).unwrap();
        }

        // val3 is absent
        let voters = vec![val1, val2];
        let absent = vec![val3];
        vs.record_participation(&voters, &absent).unwrap();

        assert_eq!(vs.val_missed_votes.read(&val1).unwrap(), 0);
        assert_eq!(vs.val_missed_votes.read(&val2).unwrap(), 0);
        assert_eq!(vs.val_missed_votes.read(&val3).unwrap(), 1);

        // Record again - val2 also absent this time
        let voters2 = vec![val1];
        let absent2 = vec![val2, val3];
        vs.record_participation(&voters2, &absent2).unwrap();

        assert_eq!(vs.val_missed_votes.read(&val2).unwrap(), 1);
        assert_eq!(vs.val_missed_votes.read(&val3).unwrap(), 2);
    });
}

// ---------------------------------------------------------------------------
// 8b. test_record_finalized_participation
// ---------------------------------------------------------------------------
#[test]
fn test_record_finalized_participation_accepts_historical_validators() {
    let val_active = address!("0x0000000000000000000000000000000000000081");
    let val_unbonding = address!("0x0000000000000000000000000000000000000082");

    with_vs_configured(10, |vs| {
        // Active current participant.
        vs.register_validator(OWNER, val_active, &dummy_consensus_pubkey(81))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(val_active)
            .unwrap();
        vs.val_has_bls_share.write(&val_active, true).unwrap();

        // Registered historical participant: canonically exit the live set to
        // UNBONDING. record_participation rejects it, while finalized-parent
        // accounting still accepts its retained registry/history record.
        vs.register_validator(OWNER, val_unbonding, &dummy_consensus_pubkey(82))
            .unwrap();
        activate_for_test(vs, val_unbonding);
        vs.deactivate_validator(OWNER, val_unbonding).unwrap();
        vs.activate_reshared_set(&[val_active], B256::ZERO).unwrap();

        // Sanity: record_participation rejects historical val_unbonding.
        // Participation/registration checks revert (not Fatal) so the error
        // message propagates instead of being masked as OutOfGas (see commit
        // c879d4e: Fatal -> Revert for system/core checks).
        let err = vs
            .record_participation(&[val_active], &[val_unbonding])
            .unwrap_err();
        assert!(matches!(err, PrecompileError::Revert(_)));

        // record_finalized_participation accepts both, increments missed_votes for absent.
        vs.record_finalized_participation(&[val_active], &[val_unbonding])
            .unwrap();
        assert_eq!(vs.val_missed_votes.read(&val_active).unwrap(), 0);
        assert_eq!(vs.val_missed_votes.read(&val_unbonding).unwrap(), 1);
    });
}

#[test]
fn test_record_finalized_participation_rejects_unregistered() {
    let val = address!("0x0000000000000000000000000000000000000091");
    let stranger = address!("0x9999999999999999999999999999999999999999");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val, &dummy_consensus_pubkey(91))
            .unwrap();

        let err = vs
            .record_finalized_participation(&[val], &[stranger])
            .unwrap_err();
        // Registration check reverts (not Fatal) so the message propagates
        // cleanly instead of being masked as OutOfGas (see commit c879d4e).
        match err {
            PrecompileError::Revert(msg) => {
                assert!(
                    msg.contains("not a registered validator"),
                    "unexpected error: {msg}"
                );
            }
            other => panic!("expected Revert, got {other:?}"),
        }
    });
}

// ===========================================================================
// EXITING validators get per-epoch counters reset
// ===========================================================================

#[test]
fn test_epoch_reset_includes_exiting() {
    with_vs_configured(10, |vs| {
        let val = address!("0x4444444444444444444444444444444444444444");
        vs.register_validator(OWNER, val, &dummy_consensus_pubkey(44))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(val).unwrap();

        // Accumulate counters then transition to EXITING
        vs.val_missed_blocks.write(&val, 10).unwrap();
        vs.val_missed_votes.write(&val, 5).unwrap();
        vs.val_blocks_proposed.write(&val, 3).unwrap();
        vs.deactivate_validator(OWNER, val).unwrap();

        // Epoch transition should reset counters even for EXITING
        vs.update_epoch(1000, 42).unwrap();

        assert_eq!(vs.val_missed_blocks.read(&val).unwrap(), 0);
        assert_eq!(vs.val_missed_votes.read(&val).unwrap(), 0);
        assert_eq!(vs.val_blocks_proposed.read(&val).unwrap(), 0);
    });
}

#[test]
fn finalized_participation_guard_prune_ring_bounds_growth() {
    use outbe_primitives::storage::StorageHandle;
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        let v = address!("0x0101010101010101010101010101010101010101");
        vs.register_validator(OWNER, v, &dummy_consensus_pubkey(0x01))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(v).unwrap();

        let retain = crate::hooks::FINALIZED_PARTICIPATION_RETAIN;
        let total = retain + 3;
        let hashes: Vec<B256> = (0..total)
            .map(|i| B256::with_last_byte((i + 1) as u8))
            .collect();
        for h in &hashes {
            crate::hooks::record_finalized_participation(storage.clone(), *h, &[v], &[]).unwrap();
        }
        // The oldest (total - retain) guard flags are evicted (slots reclaimed);
        // the last `retain` finalized blocks are still guarded against replay.
        for i in 0..(total - retain) {
            assert!(
                !vs.finalized_participation_recorded
                    .read(&hashes[i as usize])
                    .unwrap(),
                "guard entry {i} must be pruned"
            );
        }
        for i in (total - retain)..total {
            assert!(
                vs.finalized_participation_recorded
                    .read(&hashes[i as usize])
                    .unwrap(),
                "guard entry {i} must be retained"
            );
        }
    });
}
