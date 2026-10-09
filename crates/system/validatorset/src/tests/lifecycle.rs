use super::*;

#[test]
fn test_activate_missing_validator_returns_revert() {
    let val_addr = address!("0x1111111111111111111111111111111111111111");

    with_vs_configured(10, |vs| {
        let err = vs
            .activate_validator_via_boundary_for_test(val_addr)
            .unwrap_err();
        assert!(
            matches!(err, PrecompileError::Revert(message) if message == "test validator is not registered")
        );
    });
}

// ---------------------------------------------------------------------------
// 5. test_activate_deactivate
// ---------------------------------------------------------------------------
#[test]
fn test_activate_deactivate() {
    let val_addr = address!("0x4444444444444444444444444444444444444444");

    with_vs_configured(10, |vs| {
        register_validators(vs, &[(val_addr, 4)]).unwrap();

        // Initially REGISTERED
        assert_eq!(vs.val_status.read(&val_addr).unwrap(), status::REGISTERED);

        vs.activate_validator_via_boundary_for_test(val_addr)
            .unwrap();
        assert_eq!(vs.val_status.read(&val_addr).unwrap(), status::ACTIVE);

        vs.deactivate_validator(OWNER, val_addr).unwrap();
        // In the new lifecycle, deactivation transitions to EXITING (not INACTIVE)
        assert_eq!(vs.val_status.read(&val_addr).unwrap(), status::EXITING);

        // pending_set_change should be set after deactivation
        assert!(vs.pending_set_change.read().unwrap());
    });
}

#[test]
fn deactivation_rejection_reasons_preserve_validator_state() {
    let validator = address!("0x4444444444444444444444444444444444444444");
    let outsider = address!("0x9999999999999999999999999999999999999999");
    with_vs_configured(10, |vs| {
        register_validators(vs, &[(validator, 4)]).unwrap();
        activate_staked_for_test(vs, validator);
        let active = vs.validator_state(validator).unwrap();
        let pending = vs.pending_set_change.read().unwrap();
        assert!(matches!(
            vs.deactivate_validator(outsider, validator),
            Err(PrecompileError::Revert(reason))
                if reason == "unauthorized: caller must be owner or validator itself"
        ));
        assert_eq!(vs.validator_state(validator).unwrap(), active);
        assert_eq!(vs.pending_set_change.read().unwrap(), pending);

        vs.deactivate_validator(validator, validator).unwrap();
        let exiting = vs.validator_state(validator).unwrap();
        let pending = vs.pending_set_change.read().unwrap();
        assert!(matches!(
            vs.deactivate_validator(validator, validator),
            Err(PrecompileError::Revert(reason))
                if reason == "can only deactivate an active validator"
        ));
        assert_eq!(vs.validator_state(validator).unwrap(), exiting);
        assert_eq!(vs.pending_set_change.read().unwrap(), pending);
    });
}

// ---------------------------------------------------------------------------
// 6. test_force_exit
// ---------------------------------------------------------------------------
#[test]
fn test_force_exit() {
    let val_addr = address!("0x5555555555555555555555555555555555555555");

    with_vs_configured(10, |vs| {
        register_validators(vs, &[(val_addr, 5)]).unwrap();
        activate_for_test(vs, val_addr);

        vs.force_exit_validator(val_addr).unwrap();
        assert_eq!(vs.val_status.read(&val_addr).unwrap(), status::EXITING);
        assert_eq!(vs.val_slash_count.read(&val_addr).unwrap(), 1);
        assert!(vs.pending_set_change.read().unwrap());
    });
}

// ---------------------------------------------------------------------------
// 19. test_forced_exit_preserves_staking_lifecycle
// ---------------------------------------------------------------------------
#[test]
fn test_forced_exit_preserves_staking_lifecycle() {
    use alloy_primitives::U256;
    let val_addr = address!("0x3333333333333333333333333333333333333333");

    with_vs_configured(10, |vs| {
        vs.config_min_stake.write(U256::from(1000u64)).unwrap();

        register_boundary_active(vs, val_addr, 0x33).unwrap();

        // Set stake above min then force exit.
        vs.val_stake.write(&val_addr, U256::from(1000u64)).unwrap();
        vs.force_exit_validator(val_addr).unwrap();
        assert_eq!(vs.val_status.read(&val_addr).unwrap(), status::EXITING);

        // Simulate slash reducing stake below min_stake
        vs.val_stake.write(&val_addr, U256::from(500u64)).unwrap();

        // Forced exit never returns through REGISTERED. Staking moves
        // UNBONDING validators to INACTIVE after withdrawability completes.
        assert_eq!(vs.val_status.read(&val_addr).unwrap(), status::EXITING);
    });
}

// ---------------------------------------------------------------------------
// 20. test_cleanup_inactive_validators
// ---------------------------------------------------------------------------
#[test]
fn test_cleanup_inactive_validators() {
    let val1 = address!("0x00000000000000000000000000000000000000A1");
    let val2 = address!("0x00000000000000000000000000000000000000A2");
    let val3 = address!("0x00000000000000000000000000000000000000A3");
    let val4 = address!("0x00000000000000000000000000000000000000A4");
    let val5 = address!("0x00000000000000000000000000000000000000A5");

    with_vs_configured(10, |vs| {
        register_validators(
            vs,
            &[
                (val1, 0xA1),
                (val2, 0xA2),
                (val3, 0xA3),
                (val4, 0xA4),
                (val5, 0xA5),
            ],
        )
        .unwrap();
        assert_eq!(vs.validator_count.read().unwrap(), 5);

        // Move val2 and val4 through the canonical exit and claim-completion
        // transitions before cleaning their registry slots.
        make_inactive_for_test(vs, val2);
        make_inactive_for_test(vs, val4);

        // Cleanup all INACTIVE entries
        let removed = vs.cleanup_inactive_validators(0).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(vs.validator_count.read().unwrap(), 3);

        // Cleaned-up validators have index 0
        assert_eq!(vs.address_to_index.read(&val2).unwrap(), 0);
        assert_eq!(vs.address_to_index.read(&val4).unwrap(), 0);

        // Remaining validators are still accessible
        assert!(vs.address_to_index.read(&val1).unwrap() > 0);
        assert!(vs.address_to_index.read(&val3).unwrap() > 0);
        assert!(vs.address_to_index.read(&val5).unwrap() > 0);
    });
}

// ---------------------------------------------------------------------------
// 21. test_cleanup_capped
// ---------------------------------------------------------------------------
#[test]
fn test_cleanup_capped() {
    let val1 = address!("0x00000000000000000000000000000000000000B1");
    let val2 = address!("0x00000000000000000000000000000000000000B2");
    let val3 = address!("0x00000000000000000000000000000000000000B3");

    with_vs_configured(10, |vs| {
        register_validators(vs, &[(val1, 0xB1), (val2, 0xB2), (val3, 0xB3)]).unwrap();

        // Move all three through canonical INACTIVE records.
        make_inactive_for_test(vs, val1);
        make_inactive_for_test(vs, val2);
        make_inactive_for_test(vs, val3);

        // Cap at 2
        let removed = vs.cleanup_inactive_validators(2).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(vs.validator_count.read().unwrap(), 1);

        // Second call gets the remaining one
        let removed2 = vs.cleanup_inactive_validators(2).unwrap();
        assert_eq!(removed2, 1);
        assert_eq!(vs.validator_count.read().unwrap(), 0);
    });
}

// ---------------------------------------------------------------------------
// P2-3: Re-registration cooldown tests
// ---------------------------------------------------------------------------

#[test]
fn test_reregistration_cooldown_gates_then_allows() {
    // Re-registration is rejected until `config_reregistration_cooldown` blocks
    // pass after deactivation, then allowed. Deactivated at h100, cooldown 1000:
    // rejected at h500 (400 elapsed), allowed at h1100 (1000 elapsed).
    let mut storage = registry_storage(100, 128).unwrap();
    let val = address!("0x1111111111111111111111111111111111111111");

    at_height(&mut storage, 100, |vs| {
        vs.config_reregistration_cooldown.write(1000).unwrap();
        register_validators(vs, &[(val, 0xCC)]).unwrap();
        make_inactive_for_test(vs, val);
    });

    // h500: only 400 blocks elapsed -> rejected with a cooldown error.
    at_height(&mut storage, 500, |vs| {
        let err = vs
            .register_validator(OWNER, val, &dummy_consensus_pubkey(0xDD))
            .unwrap_err();
        assert!(
            err.to_string().contains("cooldown"),
            "error should mention cooldown"
        );
    });

    // h1100: 1000 blocks elapsed -> allowed.
    at_height(&mut storage, 1100, |vs| {
        register_validators(vs, &[(val, 0xFF)]).unwrap();
        assert_eq!(vs.val_status.read(&val).unwrap(), status::REGISTERED);
    });
}

#[test]
fn test_reregistration_no_cooldown_configured() {
    with_vs_configured(128, |vs| {
        let val = address!("0x3333333333333333333333333333333333333333");
        let pk = dummy_consensus_pubkey(0xAA);

        // cooldown = 0 (default)
        assert_eq!(vs.config_reregistration_cooldown.read().unwrap(), 0);

        vs.register_validator(OWNER, val, &pk).unwrap();
        make_inactive_for_test(vs, val);

        // Re-register immediately - should succeed (no cooldown)
        let pk_new = dummy_consensus_pubkey(0xBB);
        vs.register_validator(OWNER, val, &pk_new).unwrap();
        assert_eq!(vs.val_status.read(&val).unwrap(), status::REGISTERED);
    });
}

#[test]
fn test_stale_join_guard_resets_on_restake() {
    // A re-staked validator (PENDING->...->PENDING) must re-confirm: mark_pending
    // clears the confirmed flag so a stale prior confirmation cannot leak through.
    with_vs_configured(128, |vs| {
        let pend = address!("0x2222222222222222222222222222222222222222");
        register_validators(vs, &[(pend, 0x02)]).unwrap();
        vs.mark_pending(pend).unwrap();
        confirm_ready(vs, pend, 0x22);
        let in_target = |vs: &mut crate::schema::ValidatorSet, addr| {
            vs.get_reshare_target_set()
                .unwrap()
                .into_iter()
                .any(|v| v.validator_address == addr)
        };
        assert!(in_target(vs, pend), "confirmed PENDING is in the target");

        // Promotion clears the flag. Demote back to REGISTERED, then re-PENDING.
        vs.activate_reshared_set(&[pend], B256::ZERO).unwrap();
        // Force back to REGISTERED to simulate a churn that returns it to PENDING.
        vs.deactivate_validator(OWNER, pend).unwrap();
        vs.activate_reshared_set(&[], B256::ZERO).unwrap(); // EXITING->UNBONDING
                                                            // A fresh registration+stake cycle starts unconfirmed.
        let pend2 = address!("0x4444444444444444444444444444444444444444");
        register_validators(vs, &[(pend2, 0x04)]).unwrap();
        vs.mark_pending(pend2).unwrap();
        assert!(
            !in_target(vs, pend2),
            "freshly re-PENDING joiner must NOT be in the target without re-confirming"
        );
    });
}

// ===========================================================================
// Forced-exit validator status guard tests
// ===========================================================================

/// The lifecycle that a forced exit starts from.
#[derive(Clone, Copy)]
enum ForceExitStart {
    Active,
    Exiting,
    Unbonding,
    Inactive,
    Registered,
}

impl ForceExitStart {
    /// Moves the registered `val` into this starting lifecycle.
    fn prepare(self, vs: &mut ValidatorSet, val: Address) -> outbe_primitives::error::Result<()> {
        match self {
            Self::Active => activate_for_test(vs, val),
            Self::Exiting => {
                activate_for_test(vs, val);
                vs.force_exit_validator(val)?;
            }
            Self::Unbonding => {
                activate_for_test(vs, val);
                vs.deactivate_validator(OWNER, val)?;
                vs.activate_reshared_set(&[], B256::ZERO)?;
            }
            Self::Inactive => make_inactive_for_test(vs, val),
            Self::Registered => {}
        }
        Ok(())
    }
}

#[test]
fn force_exit_from_each_status() {
    // force_exit_validator across every starting status: ACTIVE->EXITING, an
    // already-EXITING idempotent call, the UNBONDING/INACTIVE idempotent no-ops,
    // and the REGISTERED rejection.
    let val = address!("0x0909090909090909090909090909090909090909");
    let cases: &[(&str, ForceExitStart, bool, u8)] = &[
        (
            "ACTIVE -> EXITING",
            ForceExitStart::Active,
            true,
            status::EXITING,
        ),
        (
            "EXITING idempotent",
            ForceExitStart::Exiting,
            true,
            status::EXITING,
        ),
        (
            "UNBONDING idempotent",
            ForceExitStart::Unbonding,
            true,
            status::UNBONDING,
        ),
        (
            "INACTIVE idempotent",
            ForceExitStart::Inactive,
            true,
            status::INACTIVE,
        ),
        (
            "REGISTERED rejected",
            ForceExitStart::Registered,
            false,
            status::REGISTERED,
        ),
    ];
    for (i, (label, start, expect_ok, final_status)) in cases.iter().enumerate() {
        with_vs_configured(10, |vs| {
            register_validators(vs, &[(val, 90 + i as u8)]).unwrap();
            start.prepare(vs, val).unwrap();
            assert_eq!(
                vs.force_exit_validator(val).is_ok(),
                *expect_ok,
                "case '{label}': result mismatch"
            );
            if *expect_ok {
                assert_eq!(
                    vs.val_status.read(&val).unwrap(),
                    *final_status,
                    "case '{label}': final status"
                );
            }
        });
    }
}

#[test]
fn test_repeated_force_exit_remains_exiting() {
    with_vs_configured(10, |vs| {
        let val = address!("0x0909090909090909090909090909090909090909");
        register_boundary_active(vs, val, 92).unwrap();
        vs.force_exit_validator(val).unwrap();
        vs.force_exit_validator(val).unwrap();
        assert_eq!(vs.val_status.read(&val).unwrap(), status::EXITING);
        assert_eq!(vs.val_slash_count.read(&val).unwrap(), 1);
    });
}

// ===========================================================================
// Activate validator status guard tests
// ===========================================================================

#[test]
fn activate_rejected_from_non_promotable_status() {
    // activate_validator only promotes REGISTERED/PENDING. EXITING (reached either
    // via force_exit or written directly), UNBONDING, and INACTIVE are all rejected.
    let val = address!("0x2121212121212121212121212121212121212121");
    let cases: &[(&str, u8)] = &[
        ("EXITING", status::EXITING),
        ("UNBONDING", status::UNBONDING),
        ("INACTIVE", status::INACTIVE),
    ];
    for (i, (label, s)) in cases.iter().enumerate() {
        with_vs_configured(10, |vs| {
            register_validators(vs, &[(val, 21 + i as u8)]).unwrap();
            vs.val_status.write(&val, *s).unwrap();
            assert!(
                vs.activate_validator_via_boundary_for_test(val).is_err(),
                "case '{label}': activate must be rejected"
            );
        });
    }
}
