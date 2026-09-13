use super::*;
use crate::delegation::ValidatorDelegateRole;

const VALIDATOR_A: Address = address!("0x00000000000000000000000000000000000000A1");
const VALIDATOR_B: Address = address!("0x00000000000000000000000000000000000000B2");
const DELEGATE: Address = address!("0x00000000000000000000000000000000000000D1");
const ROTATED: Address = address!("0x00000000000000000000000000000000000000D2");

fn register_active(vs: &mut ValidatorSet, validator: Address, seed: u8) {
    vs.register_validator(OWNER, validator, &dummy_consensus_pubkey(seed))
        .unwrap();
    vs.activate_validator_via_boundary_for_test(validator)
        .unwrap();
    vs.val_has_bls_share.write(&validator, true).unwrap();
}

#[test]
fn delegated_key_resolves_only_for_its_assigned_role() {
    with_vs_configured(10, |vs| {
        register_active(vs, VALIDATOR_A, 1);

        vs.set_delegate(VALIDATOR_A, ValidatorDelegateRole::Oracle, DELEGATE)
            .unwrap();

        assert_eq!(
            vs.resolve_validator_for_role(DELEGATE, ValidatorDelegateRole::Oracle)
                .unwrap(),
            Some(VALIDATOR_A)
        );
        assert_eq!(
            vs.resolve_validator_for_role(DELEGATE, ValidatorDelegateRole::Ocomp)
                .unwrap(),
            None
        );
    });
}

#[test]
fn rotation_and_revoke_remove_the_previous_capability() {
    with_vs_configured(10, |vs| {
        register_active(vs, VALIDATOR_A, 1);
        vs.set_delegate(VALIDATOR_A, ValidatorDelegateRole::Ocomp, DELEGATE)
            .unwrap();
        assert_eq!(
            vs.resolve_validator_for_role(VALIDATOR_A, ValidatorDelegateRole::Ocomp)
                .unwrap(),
            None,
            "explicit delegation disables validator-address fallback for that role"
        );
        vs.set_delegate(VALIDATOR_A, ValidatorDelegateRole::Ocomp, ROTATED)
            .unwrap();

        assert_eq!(
            vs.resolve_validator_for_role(DELEGATE, ValidatorDelegateRole::Ocomp)
                .unwrap(),
            None
        );
        assert_eq!(
            vs.resolve_validator_for_role(ROTATED, ValidatorDelegateRole::Ocomp)
                .unwrap(),
            Some(VALIDATOR_A)
        );

        vs.revoke_delegate(VALIDATOR_A, ValidatorDelegateRole::Ocomp)
            .unwrap();
        assert_eq!(
            vs.resolve_validator_for_role(ROTATED, ValidatorDelegateRole::Ocomp)
                .unwrap(),
            None
        );
        assert_eq!(
            vs.resolve_validator_for_role(VALIDATOR_A, ValidatorDelegateRole::Ocomp)
                .unwrap(),
            Some(VALIDATOR_A),
            "revocation restores validator-address fallback"
        );
    });
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
            matches!(err, PrecompileError::Revert(message) if message == "delegate already assigned for role")
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
            matches!(err, PrecompileError::Revert(message) if message == "delegate must not be a registered validator")
        );
    });
}

#[test]
fn operational_delegate_cannot_later_register_as_a_validator() {
    with_vs_configured(10, |vs| {
        register_active(vs, VALIDATOR_A, 1);
        vs.set_delegate(VALIDATOR_A, ValidatorDelegateRole::Ocomp, DELEGATE)
            .unwrap();

        let err = vs
            .register_validator(OWNER, DELEGATE, &dummy_consensus_pubkey(2))
            .unwrap_err();
        assert!(
            matches!(err, PrecompileError::Revert(message) if message == "validator address is already assigned as an operational delegate")
        );
        assert!(!vs.is_validator(DELEGATE).unwrap());
        assert_eq!(
            vs.get_delegate(VALIDATOR_A, ValidatorDelegateRole::Ocomp)
                .unwrap(),
            DELEGATE
        );
        assert_eq!(
            vs.resolve_validator_for_role(DELEGATE, ValidatorDelegateRole::Ocomp)
                .unwrap(),
            Some(VALIDATOR_A)
        );
    });
}

#[test]
fn inactive_validator_can_configure_but_cannot_use_an_operational_key() {
    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, VALIDATOR_A, &dummy_consensus_pubkey(1))
            .unwrap();
        vs.set_delegate(VALIDATOR_A, ValidatorDelegateRole::Oracle, DELEGATE)
            .unwrap();

        assert_eq!(
            vs.resolve_validator_for_role(DELEGATE, ValidatorDelegateRole::Oracle)
                .unwrap(),
            None
        );
    });
}

#[test]
fn one_address_can_hold_independent_roles_for_the_same_validator() {
    with_vs_configured(10, |vs| {
        register_active(vs, VALIDATOR_A, 1);
        vs.set_delegate(VALIDATOR_A, ValidatorDelegateRole::Oracle, DELEGATE)
            .unwrap();
        vs.set_delegate(VALIDATOR_A, ValidatorDelegateRole::Ocomp, DELEGATE)
            .unwrap();

        assert_eq!(
            vs.resolve_validator_for_role(DELEGATE, ValidatorDelegateRole::Oracle)
                .unwrap(),
            Some(VALIDATOR_A)
        );
        assert_eq!(
            vs.resolve_validator_for_role(DELEGATE, ValidatorDelegateRole::Ocomp)
                .unwrap(),
            Some(VALIDATOR_A)
        );
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
        ValidatorDelegateRole::Ocomp
    );
}
