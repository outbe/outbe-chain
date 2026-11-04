use super::*;
use crate::delegation::ValidatorDelegateRole;
use crate::precompile::IValidatorSet as I;
use alloy_sol_types::SolError;

const VALIDATOR_A: Address = address!("0x00000000000000000000000000000000000000A1");
const VALIDATOR_B: Address = address!("0x00000000000000000000000000000000000000B2");
const DELEGATE: Address = address!("0x00000000000000000000000000000000000000D1");
const ROTATED: Address = address!("0x00000000000000000000000000000000000000D2");

fn register_active(vs: &mut ValidatorSet, validator: Address, seed: u8) {
    register_participant(vs, validator, seed).unwrap();
}

#[test]
fn delegated_key_resolves_only_for_its_assigned_role() {
    with_vs_configured(10, |vs| {
        register_active(vs, VALIDATOR_A, 1);

        vs.set_delegate(VALIDATOR_A, ValidatorDelegateRole::Oracle, DELEGATE)
            .unwrap();
        assert_eq!(
            vs.resolve_validator_for_role(VALIDATOR_A, ValidatorDelegateRole::Hyperlane)
                .unwrap(),
            Some(VALIDATOR_A)
        );
        vs.set_delegate(VALIDATOR_A, ValidatorDelegateRole::Hyperlane, ROTATED)
            .unwrap();

        for (role, assigned, other) in [
            (ValidatorDelegateRole::Oracle, DELEGATE, ROTATED),
            (ValidatorDelegateRole::Hyperlane, ROTATED, DELEGATE),
        ] {
            assert_eq!(
                vs.resolve_validator_for_role(assigned, role).unwrap(),
                Some(VALIDATOR_A)
            );
            assert_eq!(vs.resolve_validator_for_role(other, role).unwrap(), None);
            assert_eq!(
                vs.resolve_validator_for_role(VALIDATOR_A, role).unwrap(),
                None
            );
        }
    });
}

#[test]
fn rotation_and_revoke_remove_the_previous_capability() {
    for (role, other_role) in [
        (
            ValidatorDelegateRole::Oracle,
            ValidatorDelegateRole::Hyperlane,
        ),
        (
            ValidatorDelegateRole::Hyperlane,
            ValidatorDelegateRole::Oracle,
        ),
    ] {
        with_vs_configured(10, |vs| {
            register_active(vs, VALIDATOR_A, 1);
            let other_delegate = Address::repeat_byte(0x77);
            vs.set_delegate(VALIDATOR_A, other_role, other_delegate)
                .unwrap();
            vs.set_delegate(VALIDATOR_A, role, DELEGATE).unwrap();
            assert_eq!(
                vs.resolve_validator_for_role(VALIDATOR_A, role).unwrap(),
                None,
                "explicit delegation disables validator-address fallback for that role"
            );
            vs.set_delegate(VALIDATOR_A, role, ROTATED).unwrap();

            assert_eq!(vs.resolve_validator_for_role(DELEGATE, role).unwrap(), None);
            assert_eq!(
                vs.resolve_validator_for_role(ROTATED, role).unwrap(),
                Some(VALIDATOR_A)
            );

            vs.revoke_delegate(VALIDATOR_A, role).unwrap();
            vs.revoke_delegate(VALIDATOR_A, role).unwrap();
            assert_eq!(vs.resolve_validator_for_role(ROTATED, role).unwrap(), None);
            assert_eq!(
                vs.resolve_validator_for_role(VALIDATOR_A, role).unwrap(),
                Some(VALIDATOR_A),
                "revocation restores validator-address fallback"
            );
            assert_eq!(
                vs.get_delegate(VALIDATOR_A, other_role).unwrap(),
                other_delegate
            );
            assert_eq!(
                vs.resolve_validator_for_role(other_delegate, other_role)
                    .unwrap(),
                Some(VALIDATOR_A)
            );
        });
    }
}

#[test]
fn delegate_cannot_be_claimed_by_two_validators_for_the_same_role() {
    with_vs_configured(10, |vs| {
        register_active(vs, VALIDATOR_A, 1);
        register_active(vs, VALIDATOR_B, 2);
        vs.set_delegate(VALIDATOR_A, ValidatorDelegateRole::Oracle, DELEGATE)
            .unwrap();

        let err = vs
            .set_delegate(VALIDATOR_B, ValidatorDelegateRole::Oracle, DELEGATE)
            .unwrap_err();
        assert!(
            matches!(err, PrecompileError::RevertBytes(bytes) if bytes.len() >= 4 && bytes[..4] == I::DelegateAlreadyAssigned::SELECTOR)
        );
    });
}

#[test]
fn registered_validator_address_cannot_be_used_as_another_validators_delegate() {
    with_vs_configured(10, |vs| {
        register_active(vs, VALIDATOR_A, 1);
        register_active(vs, VALIDATOR_B, 2);

        let err = vs
            .set_delegate(VALIDATOR_A, ValidatorDelegateRole::Oracle, VALIDATOR_B)
            .unwrap_err();
        assert!(
            matches!(err, PrecompileError::RevertBytes(bytes) if bytes.len() >= 4 && bytes[..4] == I::DelegateIsRegisteredValidator::SELECTOR)
        );
    });
}

#[test]
fn operational_delegate_cannot_later_register_as_a_validator() {
    for role in [
        ValidatorDelegateRole::Oracle,
        ValidatorDelegateRole::Hyperlane,
    ] {
        with_vs_configured(10, |vs| {
            register_active(vs, VALIDATOR_A, 1);
            vs.set_delegate(VALIDATOR_A, role, DELEGATE).unwrap();

            let err = vs
                .register_validator(OWNER, DELEGATE, &dummy_consensus_pubkey(2))
                .unwrap_err();
            assert!(
                matches!(err, PrecompileError::RevertBytes(bytes) if bytes.len() >= 4 && bytes[..4] == I::ValidatorAlreadyOperationalDelegate::SELECTOR)
            );
            assert!(!vs.is_validator(DELEGATE).unwrap());
            assert_eq!(vs.get_delegate(VALIDATOR_A, role).unwrap(), DELEGATE);
        });
    }
}

#[test]
fn inactive_validator_can_configure_but_cannot_use_an_operational_key() {
    for role in [
        ValidatorDelegateRole::Oracle,
        ValidatorDelegateRole::Hyperlane,
    ] {
        with_vs_configured(10, |vs| {
            register_validators(vs, &[(VALIDATOR_A, 1)]).unwrap();
            vs.set_delegate(VALIDATOR_A, role, DELEGATE).unwrap();

            assert_eq!(vs.resolve_validator_for_role(DELEGATE, role).unwrap(), None);
        });
    }
}

#[test]
fn one_address_can_hold_independent_roles_for_the_same_validator() {
    with_vs_configured(10, |vs| {
        register_active(vs, VALIDATOR_A, 1);
        vs.set_delegate(VALIDATOR_A, ValidatorDelegateRole::Oracle, DELEGATE)
            .unwrap();
        vs.set_delegate(VALIDATOR_A, ValidatorDelegateRole::Hyperlane, DELEGATE)
            .unwrap();

        for role in [
            ValidatorDelegateRole::Oracle,
            ValidatorDelegateRole::Hyperlane,
        ] {
            assert_eq!(
                vs.resolve_validator_for_role(DELEGATE, role).unwrap(),
                Some(VALIDATOR_A)
            );
        }
    });
}

#[test]
fn unknown_role_ids_fail_closed() {
    assert!(ValidatorDelegateRole::try_from(0).is_err());
    assert!(ValidatorDelegateRole::try_from(3).is_err());
    assert_eq!(
        ValidatorDelegateRole::try_from(1).unwrap(),
        ValidatorDelegateRole::Oracle
    );
    assert_eq!(
        ValidatorDelegateRole::try_from(2).unwrap(),
        ValidatorDelegateRole::Hyperlane
    );
}
