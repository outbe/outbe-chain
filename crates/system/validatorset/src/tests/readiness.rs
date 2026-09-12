use super::*;

#[test]
fn founder_key_bootstrap_imports_exact_active_order_and_replays_idempotently() {
    let validators = [Address::repeat_byte(0x61), Address::repeat_byte(0x62)];
    let consensus_keys = [dummy_consensus_pubkey(0x31), dummy_consensus_pubkey(0x32)];

    with_vs_configured(2, |vs| {
        let mut registrations = Vec::new();
        for (index, (validator, consensus_key)) in
            validators.into_iter().zip(consensus_keys).enumerate()
        {
            vs.register_validator(OWNER, validator, &consensus_key)
                .unwrap();
            vs.mark_pending(validator).unwrap();
            let (registration, encoded) = ocomp_registration(
                validator,
                &consensus_key,
                u8::try_from(index).unwrap().saturating_add(0x71),
            );
            vs.confirm_validator_ready(validator, &encoded).unwrap();
            vs.activate_validator_via_boundary_for_test(validator)
                .unwrap();
            registrations.push(registration);
        }

        for (validator, registration) in validators.into_iter().zip(&registrations) {
            let key_hash = keccak256(registration.core.ocomp_public_key_sec1);
            vs.val_ocomp_registration
                .get_bytes(&validator)
                .clear()
                .unwrap();
            vs.ocomp_key_hash_to_validator
                .write(&key_hash, Address::ZERO)
                .unwrap();
            vs.val_join_confirmed.write(&validator, false).unwrap();
        }

        vs.initialize_founder_ocomp_registrations(&registrations)
            .unwrap();
        for (validator, registration) in validators.into_iter().zip(&registrations) {
            assert_eq!(
                vs.ocomp_registration(validator).unwrap().as_ref(),
                Some(registration)
            );
            assert_eq!(
                vs.ocomp_key_hash_to_validator
                    .read(&keccak256(registration.core.ocomp_public_key_sec1))
                    .unwrap(),
                validator
            );
            assert!(
                !vs.val_join_confirmed.read(&validator).unwrap(),
                "founder key bootstrap must not mutate ACTIVE admission readiness"
            );
        }

        vs.initialize_founder_ocomp_registrations(&registrations)
            .unwrap();
        let mut reordered = registrations;
        reordered.swap(0, 1);
        assert!(matches!(
            vs.initialize_founder_ocomp_registrations(&reordered),
            Err(PrecompileError::Fatal(_))
        ));
    });
}

// ---------------------------------------------------------------------------
// Task 04: validator join race tests
// ---------------------------------------------------------------------------

#[test]
fn test_confirm_ready_signals_pending_until_certified_boundary() {
    with_vs_configured(128, |vs| {
        let val = address!("0x1111111111111111111111111111111111111111");
        let pk = dummy_consensus_pubkey(0x01);

        // Register -> REGISTERED, pending_set_change = true
        vs.register_validator(OWNER, val, &pk).unwrap();
        assert!(vs.has_pending_set_change().unwrap());

        // Simulate reshare completed (without this validator, they had no stake).
        // activate_reshared_set with empty set -> clears pending.
        vs.activate_reshared_set(&[], B256::with_last_byte(0x01))
            .unwrap();
        assert!(!vs.has_pending_set_change().unwrap());

        // Staking and confirm-ready signal that a new certified boundary is due.
        vs.mark_pending(val).unwrap();
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
    // contain REGISTERED (full-node, not staked) + PENDING (staked joiner), but NOT
    // ACTIVE (already a primary peer). The reshare target is the mirror image
    // ({ACTIVE, PENDING}) - REGISTERED must never be a reshare player (no stake).
    with_vs_configured(128, |vs| {
        let reg = address!("0x1111111111111111111111111111111111111111");
        let pend = address!("0x2222222222222222222222222222222222222222");
        let act = address!("0x3333333333333333333333333333333333333333");

        vs.register_validator(OWNER, reg, &dummy_consensus_pubkey(0x01))
            .unwrap();
        vs.register_validator(OWNER, pend, &dummy_consensus_pubkey(0x02))
            .unwrap();
        vs.mark_pending(pend).unwrap();
        vs.register_validator(OWNER, act, &dummy_consensus_pubkey(0x03))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(act).unwrap();

        let admitted: Vec<_> = vs
            .get_admitted_non_consensus_validators()
            .unwrap()
            .into_iter()
            .map(|v| v.validator_address)
            .collect();
        assert!(
            admitted.contains(&reg),
            "REGISTERED full-node must be admitted"
        );
        assert!(admitted.contains(&pend), "PENDING joiner must be admitted");
        assert!(
            !admitted.contains(&act),
            "ACTIVE validator is a primary peer, not secondary"
        );

        // Stale-join guard: a freshly-PENDING joiner is NOT yet in the reshare
        // target until it confirms readiness; ACTIVE is always in.
        let reshare_before: Vec<_> = vs
            .get_reshare_target_set()
            .unwrap()
            .into_iter()
            .map(|v| v.validator_address)
            .collect();
        assert!(
            !reshare_before.contains(&reg),
            "REGISTERED (unstaked) must NOT be a reshare player"
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
    // confirmValidatorReady is only valid from PENDING; REGISTERED and ACTIVE revert.
    with_vs_configured(128, |vs| {
        let reg = address!("0x1111111111111111111111111111111111111111");
        let act = address!("0x3333333333333333333333333333333333333333");
        vs.register_validator(OWNER, reg, &dummy_consensus_pubkey(0x01))
            .unwrap();
        vs.register_validator(OWNER, act, &dummy_consensus_pubkey(0x03))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(act).unwrap();

        assert!(
            vs.confirm_validator_ready(reg, &[]).is_err(),
            "REGISTERED cannot confirm readiness"
        );
        assert!(
            vs.confirm_validator_ready(act, &[]).is_err(),
            "ACTIVE cannot confirm readiness"
        );
        let unregistered = address!("0x9999999999999999999999999999999999999999");
        assert!(
            vs.confirm_validator_ready(unregistered, &[]).is_err(),
            "unregistered address cannot confirm readiness"
        );
    });
}

#[test]
fn confirm_ready_persists_valid_ocomp_registration_before_readiness() {
    with_vs_configured(128, |vs| {
        let validator = address!("0x5555555555555555555555555555555555555555");
        let consensus_pubkey = dummy_consensus_pubkey(0x55);
        vs.register_validator(OWNER, validator, &consensus_pubkey)
            .unwrap();
        vs.mark_pending(validator).unwrap();
        let (registration, encoded) = ocomp_registration(validator, &consensus_pubkey, 0x56);

        vs.confirm_validator_ready(validator, &encoded).unwrap();

        assert_eq!(
            vs.ocomp_registration(validator).unwrap(),
            Some(registration.clone())
        );
        let key_hash = keccak256(registration.core.ocomp_public_key_sec1);
        assert_eq!(
            vs.ocomp_key_hash_to_validator.read(&key_hash).unwrap(),
            validator
        );
        assert!(vs.val_join_confirmed.read(&validator).unwrap());
    });
}

#[test]
fn confirm_ready_exact_replay_rejects_ocomp_key_replacement_atomically() {
    with_vs_configured(128, |vs| {
        let validator = address!("0x5656565656565656565656565656565656565656");
        let consensus_pubkey = dummy_consensus_pubkey(0x56);
        vs.register_validator(OWNER, validator, &consensus_pubkey)
            .unwrap();
        vs.mark_pending(validator).unwrap();
        let (first, first_encoded) = ocomp_registration(validator, &consensus_pubkey, 0x57);
        vs.confirm_validator_ready(validator, &first_encoded)
            .unwrap();

        vs.confirm_validator_ready(validator, &first_encoded)
            .expect("byte-identical registration replay");

        let (replacement, replacement_encoded) =
            ocomp_registration(validator, &consensus_pubkey, 0x58);
        let error = vs
            .confirm_validator_ready(validator, &replacement_encoded)
            .expect_err("V1 OCOMP key is immutable after first admission");
        assert!(error.to_string().contains("OCOMP public key is immutable"));

        assert_eq!(
            vs.ocomp_registration(validator).unwrap(),
            Some(first.clone()),
            "failed replacement preserves the original registration"
        );
        assert_eq!(
            vs.ocomp_key_hash_to_validator
                .read(&keccak256(first.core.ocomp_public_key_sec1))
                .unwrap(),
            validator
        );
        assert_eq!(
            vs.ocomp_key_hash_to_validator
                .read(&keccak256(replacement.core.ocomp_public_key_sec1))
                .unwrap(),
            Address::ZERO
        );
        assert!(vs.val_join_confirmed.read(&validator).unwrap());
    });
}

#[test]
fn reshare_target_requires_registration_even_if_readiness_flag_is_set() {
    with_vs_configured(128, |vs| {
        let validator = address!("0x5757575757575757575757575757575757575757");
        vs.register_validator(OWNER, validator, &dummy_consensus_pubkey(0x57))
            .unwrap();
        vs.mark_pending(validator).unwrap();

        // Model a stale/legacy/corrupt flag without the registration whose
        // admission must now be authoritative.
        vs.val_join_confirmed.write(&validator, true).unwrap();

        assert!(
            !vs.get_reshare_target_set()
                .unwrap()
                .iter()
                .any(|record| record.validator_address == validator),
            "readiness without a canonical OCOMP registration must fail closed"
        );
    });
}

#[test]
fn bls_key_change_preserves_ocomp_pin_and_requires_same_key_identity_refresh() {
    with_vs_configured(128, |vs| {
        let validator = address!("0x5858585858585858585858585858585858585858");
        let first_consensus_pubkey = dummy_consensus_pubkey(0x58);
        vs.register_validator(OWNER, validator, &first_consensus_pubkey)
            .unwrap();
        vs.mark_pending(validator).unwrap();
        let (registration, encoded) = ocomp_registration(validator, &first_consensus_pubkey, 0x59);
        vs.confirm_validator_ready(validator, &encoded).unwrap();
        let old_ocomp_key_hash = keccak256(registration.core.ocomp_public_key_sec1);

        make_inactive_for_test(vs, validator);
        let replacement_consensus_pubkey = dummy_consensus_pubkey(0x5A);
        vs.register_validator(OWNER, validator, &replacement_consensus_pubkey)
            .unwrap();

        assert_eq!(
            vs.ocomp_registration(validator).unwrap(),
            Some(registration.clone())
        );
        assert_eq!(
            vs.ocomp_key_hash_to_validator
                .read(&old_ocomp_key_hash)
                .unwrap(),
            validator
        );
        assert!(!vs.val_join_confirmed.read(&validator).unwrap());
        assert_eq!(vs.val_status.read(&validator).unwrap(), status::REGISTERED);

        vs.mark_pending(validator).unwrap();
        let (refreshed, refreshed_encoded) =
            ocomp_registration(validator, &replacement_consensus_pubkey, 0x59);
        vs.confirm_validator_ready(validator, &refreshed_encoded)
            .expect("new BLS identity may refresh PoP only with the pinned OCOMP key");
        assert_eq!(
            refreshed.core.ocomp_public_key_sec1,
            registration.core.ocomp_public_key_sec1
        );
        assert_eq!(vs.ocomp_registration(validator).unwrap(), Some(refreshed));
        assert_eq!(
            vs.ocomp_key_hash_to_validator
                .read(&old_ocomp_key_hash)
                .unwrap(),
            validator
        );
        assert!(vs.val_join_confirmed.read(&validator).unwrap());
    });
}

#[test]
fn full_validator_cleanup_preserves_immutable_ocomp_registration_and_key_pin() {
    with_vs_configured(128, |vs| {
        let validator = address!("0x5959595959595959595959595959595959595959");
        let consensus_pubkey = dummy_consensus_pubkey(0x59);
        vs.register_validator(OWNER, validator, &consensus_pubkey)
            .unwrap();
        vs.mark_pending(validator).unwrap();
        let (registration, encoded) = ocomp_registration(validator, &consensus_pubkey, 0x5A);
        vs.confirm_validator_ready(validator, &encoded).unwrap();
        let ocomp_key_hash = keccak256(registration.core.ocomp_public_key_sec1);
        make_inactive_for_test(vs, validator);

        assert_eq!(vs.cleanup_inactive_validators(1).unwrap(), 1);

        assert_eq!(
            vs.ocomp_registration(validator).unwrap(),
            Some(registration)
        );
        assert_eq!(
            vs.ocomp_key_hash_to_validator
                .read(&ocomp_key_hash)
                .unwrap(),
            validator
        );
        assert_eq!(vs.address_to_index.read(&validator).unwrap(), 0);

        let squatter = Address::repeat_byte(0x5A);
        let squatter_consensus_pubkey = dummy_consensus_pubkey(0x5B);
        vs.register_validator(OWNER, squatter, &squatter_consensus_pubkey)
            .unwrap();
        vs.mark_pending(squatter).unwrap();
        let (_, squatter_registration) =
            ocomp_registration(squatter, &squatter_consensus_pubkey, 0x5A);
        let error = vs
            .confirm_validator_ready(squatter, &squatter_registration)
            .expect_err("cleanup must not make the pinned OCOMP key squattable");
        assert!(error
            .to_string()
            .contains("already registered by another validator"));
        assert_eq!(
            vs.ocomp_key_hash_to_validator
                .read(&ocomp_key_hash)
                .unwrap(),
            validator
        );
    });
}

#[test]
fn certified_activation_rejects_registered_and_keyless_pending_members() {
    let registered = Address::repeat_byte(0x5A);
    with_vs_configured(128, |vs| {
        vs.register_validator(OWNER, registered, &dummy_consensus_pubkey(0x5A))
            .unwrap();
        let error = vs
            .activate_reshared_set(&[registered], B256::repeat_byte(0xA1))
            .unwrap_err();
        assert!(matches!(error, PrecompileError::Fatal(_)));
        assert_eq!(vs.val_status.read(&registered).unwrap(), status::REGISTERED);
    });

    let pending = Address::repeat_byte(0x5B);
    with_vs_configured(128, |vs| {
        vs.register_validator(OWNER, pending, &dummy_consensus_pubkey(0x5B))
            .unwrap();
        vs.mark_pending(pending).unwrap();
        vs.val_join_confirmed.write(&pending, true).unwrap();
        let error = vs
            .activate_reshared_set(&[pending], B256::repeat_byte(0xA2))
            .unwrap_err();
        assert!(matches!(error, PrecompileError::Fatal(_)));
        assert_eq!(vs.val_status.read(&pending).unwrap(), status::PENDING);
        assert!(!vs.val_has_bls_share.read(&pending).unwrap());
    });
}

#[test]
fn inactive_reentry_with_same_bls_requires_fresh_ocomp_confirmation() {
    with_vs_configured(128, |vs| {
        let validator = address!("0x5555555555555555555555555555555555555555");
        let consensus_pubkey = dummy_consensus_pubkey(0x55);
        let (_, encoded_registration) = ocomp_registration(validator, &consensus_pubkey, 0x55);

        vs.register_validator(OWNER, validator, &consensus_pubkey)
            .unwrap();
        vs.mark_pending(validator).unwrap();
        vs.confirm_validator_ready(validator, &encoded_registration)
            .unwrap();
        vs.activate_validator_via_boundary_for_test(validator)
            .unwrap();

        make_inactive_for_test(vs, validator);
        vs.register_validator(OWNER, validator, &consensus_pubkey)
            .unwrap();
        assert_eq!(vs.val_status.read(&validator).unwrap(), status::REGISTERED);
        assert!(!vs.val_join_confirmed.read(&validator).unwrap());
        assert_eq!(
            vs.ocomp_registration(validator).unwrap().unwrap(),
            OcompKeyRegistrationV1::decode_canonical(&encoded_registration, &poc_schema_limits())
                .unwrap(),
            "same-BLS re-entry retains the identity-bound registration for exact replay"
        );

        vs.mark_pending(validator).unwrap();
        assert!(
            vs.get_reshare_target_set().unwrap().is_empty(),
            "re-entry must remain outside DKG until readiness is confirmed again"
        );

        vs.confirm_validator_ready(validator, &encoded_registration)
            .unwrap();
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
