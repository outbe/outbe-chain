use super::*;

// ---------------------------------------------------------------------------
// Task 04: validator join race tests
// ---------------------------------------------------------------------------

#[test]
fn test_confirm_ready_signals_pending_until_certified_boundary() {
    with_vs_configured(128, |vs| {
        let val = address!("0x1111111111111111111111111111111111111111");
        let pk = dummy_consensus_pubkey(0x01);

        // Register -> WaitingForReadiness (PENDING), pending_set_change = true
        vs.register_validator(OWNER, val, &pk).unwrap();
        assert!(vs.has_pending_set_change().unwrap());

        // An empty boundary while val is WaitingForReadiness retains pending_set_change = true
        // until val confirms readiness and enters the active set at a certified boundary.
        vs.activate_reshared_set(&[], B256::with_last_byte(0x01))
            .unwrap();
        assert!(vs.has_pending_set_change().unwrap());

        // Even if a prior step clears pending_set_change, confirm-ready sets it.
        vs.test_set_pending_set_change(false).unwrap();
        assert!(!vs.has_pending_set_change().unwrap());
        confirm_ready(vs, val, 0x01);
        assert!(vs.has_pending_set_change().unwrap());

        // The shared helper drives the certified boundary and clears the signal
        // once the admitted validator is part of the complete ACTIVE set.
        vs.activate_validator_via_boundary_for_test(val).unwrap();
        assert_eq!(vs.val_status.read(&val).unwrap(), status::ACTIVE);
        assert!(
            !vs.has_pending_set_change().unwrap(),
            "certified boundary must clear the covered set-change signal"
        );
    });
}

#[test]
fn test_activate_reshared_set_clears_pending_after_join() {
    // After a new validator is activated and reshare includes them,
    // pending_set_change should be cleared.
    with_vs_configured(128, |vs| {
        let val = address!("0x1111111111111111111111111111111111111111");
        let pk = dummy_consensus_pubkey(0x01);

        vs.register_validator(OWNER, val, &pk).unwrap();
        vs.mark_pending(val).unwrap();
        confirm_ready(vs, val, 0x02);
        assert!(vs.has_pending_set_change().unwrap());

        // The production boundary path now includes the admitted validator.
        vs.activate_validator_via_boundary_for_test(val).unwrap();
        assert!(
            !vs.has_pending_set_change().unwrap(),
            "pending should be cleared after reshare includes all active validators"
        );
    });
}

#[test]
fn test_admitted_non_consensus_includes_registered_and_pending_not_active() {
    // TEE full-node admission: the secondary-tier P2P set must
    // contain PENDING (unconfirmed and confirmed staked joiner), but NOT
    // ACTIVE (already a primary peer). The reshare target is {ACTIVE, confirmed PENDING}.
    with_vs_configured(128, |vs| {
        let unconfirmed = address!("0x1111111111111111111111111111111111111111");
        let pend = address!("0x2222222222222222222222222222222222222222");
        let act = address!("0x3333333333333333333333333333333333333333");

        register_validators(vs, &[(unconfirmed, 0x01), (pend, 0x02)]).unwrap();
        vs.mark_pending(pend).unwrap();
        register_boundary_active(vs, act, 0x03).unwrap();

        let admitted: Vec<_> = vs
            .get_admitted_non_consensus_validators()
            .unwrap()
            .into_iter()
            .map(|v| v.validator_address)
            .collect();
        assert!(
            admitted.contains(&unconfirmed),
            "unconfirmed PENDING joiner must be admitted"
        );
        assert!(admitted.contains(&pend), "PENDING joiner must be admitted");
        assert!(
            !admitted.contains(&act),
            "ACTIVE validator is a primary peer, not secondary"
        );

        // Stale-join guard: a freshly-PENDING joiner is NOT yet in the reshare
        // target until it confirms readiness. ACTIVE is always in.
        let reshare_before: Vec<_> = vs
            .get_reshare_target_set()
            .unwrap()
            .into_iter()
            .map(|v| v.validator_address)
            .collect();
        assert!(
            !reshare_before.contains(&unconfirmed),
            "unconfirmed PENDING joiner must NOT be a reshare player"
        );
        assert!(
            !reshare_before.contains(&pend),
            "unconfirmed PENDING joiner must NOT be a reshare player (stale-join guard)"
        );
        assert!(reshare_before.contains(&act));

        // After confirming readiness the PENDING joiner enters the target.
        confirm_ready(vs, pend, 0x12);
        let reshare_after: Vec<_> = vs
            .get_reshare_target_set()
            .unwrap()
            .into_iter()
            .map(|v| v.validator_address)
            .collect();
        assert!(
            reshare_after.contains(&pend) && reshare_after.contains(&act),
            "confirmed PENDING joiner + ACTIVE must both be reshare players"
        );
    });
}

#[test]
fn test_confirm_validator_ready_requires_pending() {
    // confirmValidatorReady accepts only PENDING validators. ACTIVE and unregistered callers revert.
    with_vs_configured(128, |vs| {
        let pend = address!("0x1111111111111111111111111111111111111111");
        let act = address!("0x3333333333333333333333333333333333333333");
        register_validators(vs, &[(pend, 0x01), (act, 0x03)]).unwrap();
        vs.activate_validator_via_boundary_for_test(act).unwrap();

        vs.confirm_validator_ready(pend).unwrap();
        assert!(
            vs.confirm_validator_ready(act).is_err(),
            "ACTIVE cannot confirm readiness"
        );
        let unregistered = address!("0x9999999999999999999999999999999999999999");
        assert!(
            vs.confirm_validator_ready(unregistered).is_err(),
            "unregistered address cannot confirm readiness"
        );
    });
}

#[test]
fn certified_activation_rejects_registered_and_unconfirmed_pending_members() {
    let pending = Address::repeat_byte(0x5B);
    with_vs_configured(128, |vs| {
        register_validators(vs, &[(pending, 0x5B)]).unwrap();
        vs.mark_pending(pending).unwrap();
        let error = vs
            .activate_reshared_set(&[pending], B256::repeat_byte(0xA2))
            .unwrap_err();
        assert!(matches!(error, PrecompileError::RevertBytes(_)));
        assert_eq!(vs.val_status.read(&pending).unwrap(), status::PENDING);
        assert!(!vs.val_has_bls_share.read(&pending).unwrap());
    });
}

#[test]
fn inactive_reentry_with_same_bls_requires_fresh_readiness() {
    with_vs_configured(128, |vs| {
        let validator = address!("0x5555555555555555555555555555555555555555");
        let consensus_pubkey = dummy_consensus_pubkey(0x55);

        vs.register_validator(OWNER, validator, &consensus_pubkey)
            .unwrap();
        vs.mark_pending(validator).unwrap();
        vs.confirm_validator_ready(validator).unwrap();
        vs.activate_validator_via_boundary_for_test(validator)
            .unwrap();

        make_inactive_for_test(vs, validator);
        vs.register_validator(OWNER, validator, &consensus_pubkey)
            .unwrap();
        assert_eq!(vs.val_status.read(&validator).unwrap(), status::PENDING);
        assert!(!vs.val_join_confirmed.read(&validator).unwrap());

        vs.mark_pending(validator).unwrap();
        assert!(
            vs.get_reshare_target_set().unwrap().is_empty(),
            "re-entry must remain outside DKG until readiness is confirmed again"
        );

        vs.confirm_validator_ready(validator).unwrap();
        assert_eq!(
            vs.get_reshare_target_set()
                .unwrap()
                .into_iter()
                .map(|record| record.validator_address)
                .collect::<Vec<_>>(),
            vec![validator]
        );
    });
}

#[test]
fn test_already_active_validator_does_not_raise_pending() {
    // Calling activate_validator on an already-ACTIVE validator is a no-op.
    with_vs_configured(128, |vs| {
        let val = address!("0x1111111111111111111111111111111111111111");
        let pk = dummy_consensus_pubkey(0x01);

        vs.register_validator(OWNER, val, &pk).unwrap();
        vs.activate_validator_via_boundary_for_test(val).unwrap();

        // Clear pending by completing reshare.
        vs.activate_reshared_set(&[val], B256::with_last_byte(0x01))
            .unwrap();
        assert!(!vs.has_pending_set_change().unwrap());

        // Calling activate_validator again should NOT re-raise pending.
        vs.activate_validator_via_boundary_for_test(val).unwrap();
        assert!(
            !vs.has_pending_set_change().unwrap(),
            "already-active validator should not trigger spurious pending_set_change"
        );
    });
}

#[test]
fn confirm_validator_ready_requires_minimum_bond_in_waiting_and_joining() {
    use crate::precompile::IValidatorSet as I;
    use alloy_sol_types::SolError;

    with_vs_configured(128, |vs| {
        let validator = address!("0x6666666666666666666666666666666666666666");
        let pk = dummy_consensus_pubkey(0x66);
        let minimum = U256::from(1_000u64);
        let sub_min = U256::from(999u64);

        vs.register_validator(OWNER, validator, &pk).unwrap();
        vs.test_seed_authoritative_min_stake(minimum).unwrap();
        vs.test_seed_authoritative_bonded(validator, sub_min)
            .unwrap();

        // WaitingForReadiness below minimum stake must revert with BondedStakeBelowMinimum.
        let err = vs.confirm_validator_ready(validator).unwrap_err();
        assert_eq!(
            super::typed_revert(
                I::BondedStakeBelowMinimum {
                    bonded: sub_min,
                    minimum,
                }
                .abi_encode()
            ),
            match err {
                PrecompileError::RevertBytes(bytes) => super::typed_revert(bytes),
                other => panic!("expected typed revert, got {other:?}"),
            }
        );
        assert!(!vs.val_join_confirmed.read(&validator).unwrap());

        // Restore minimum stake -> confirmValidatorReady succeeds.
        vs.test_seed_authoritative_bonded(validator, minimum)
            .unwrap();
        vs.confirm_validator_ready(validator).unwrap();
        assert!(vs.val_join_confirmed.read(&validator).unwrap());

        // If bonded stake drops below minimum while Joining, idempotent confirm_validator_ready
        // must also reject with BondedStakeBelowMinimum.
        vs.test_seed_authoritative_bonded(validator, sub_min)
            .unwrap();
        let err = vs.confirm_validator_ready(validator).unwrap_err();
        assert_eq!(
            super::typed_revert(
                I::BondedStakeBelowMinimum {
                    bonded: sub_min,
                    minimum,
                }
                .abi_encode()
            ),
            match err {
                PrecompileError::RevertBytes(bytes) => super::typed_revert(bytes),
                other => panic!("expected typed revert, got {other:?}"),
            }
        );
    });
}
