//! Characterization of validator registration: the order in which the
//! registration checks reject a request, the complete write set of the
//! first-time and re-registration paths, and the live-signer rule of role
//! resolution.

use super::*;
use crate::delegation::ValidatorDelegateRole;
use blst::min_pk::SecretKey;
use outbe_primitives::error::Result;
use outbe_primitives::storage::hashmap::MutationPrefixViews;

const VALIDATOR: Address = address!("0x00000000000000000000000000000000000000D1");
const OTHER: Address = address!("0x00000000000000000000000000000000000000D2");
const DELEGATE: Address = address!("0x00000000000000000000000000000000000000D3");
const STRANGER: Address = address!("0x00000000000000000000000000000000000000D4");
/// A non-zero key that is not a compressed BLS12-381 point.
const NOT_A_POINT: [u8; 48] = [0x01; 48];

fn node_id(validator: Address) -> B256 {
    keccak256(validator.as_slice())
}

fn bls_key(seed: u8) -> Result<SecretKey> {
    SecretKey::key_gen(&[seed; 32], &[]).map_err(|error| {
        PrecompileError::Fatal(format!("fixture BLS key generation failed: {error:?}"))
    })
}

/// The registration proof of possession of `secret` for `validator` and
/// `node`.
fn proof_of_possession(secret: &SecretKey, validator: Address, node: B256) -> [u8; 96] {
    let message = validator_registration_message(CHAIN_ID, validator, node);
    secret
        .sign(&message, VALIDATOR_REGISTRATION_DST, &[])
        .to_bytes()
}

/// The exact text of a registration result: `Revert: <message>` or
/// `Fatal: <message>` for a rejection, else the debug form of the result.
fn outcome_text(result: Result<()>) -> String {
    match result {
        Err(PrecompileError::Revert(message)) => format!("Revert: {message}"),
        Err(PrecompileError::Fatal(message)) => format!("Fatal: {message}"),
        other => format!("{other:?}"),
    }
}

/// Requires `call` to fail with exactly `expected` before its first storage
/// write or event, on storage with room for `max` validators after `setup`.
fn assert_rejected_first(
    max: u32,
    setup: impl FnOnce(&mut ValidatorSet<'_>) -> Result<()>,
    call: impl FnOnce(&mut ValidatorSet<'_>) -> Result<()>,
    expected: &str,
) -> Result<()> {
    let mut storage = configured_storage(max);
    storage.enter(|storage| setup(&mut ValidatorSet::new(storage)))?;
    storage.clear_mutation_failure();
    let result = storage.enter(|storage| call(&mut ValidatorSet::new(storage)));
    assert_eq!(outcome_text(result), expected);
    assert_eq!(
        storage.clear_mutation_failure(),
        0,
        "registration wrote before it failed"
    );
    Ok(())
}

/// OTHER is registered and owns its own Radicle NodeId and key 0xD2.
fn other_registered(vs: &mut ValidatorSet<'_>) -> Result<()> {
    vs.register_validator(OWNER, OTHER, &dummy_consensus_pubkey(0xD2))
}

/// OTHER is registered and DELEGATE is its Oracle delegate.
fn delegate_assigned(vs: &mut ValidatorSet<'_>) -> Result<()> {
    other_registered(vs)?;
    vs.set_delegate(OTHER, ValidatorDelegateRole::Oracle, DELEGATE)
}

#[test]
fn zero_key_is_rejected_before_authorization() -> Result<()> {
    assert_rejected_first(
        40,
        |_| Ok(()),
        |vs| vs.register_validator_with_sig(STRANGER, VALIDATOR, &[0; 48], B256::ZERO, None),
        "Revert: consensus public key must not be zero",
    )
}

#[test]
fn unauthorized_caller_is_rejected_before_delegate_and_node_checks() -> Result<()> {
    assert_rejected_first(
        40,
        delegate_assigned,
        |vs| {
            let key = dummy_consensus_pubkey(0xD3);
            vs.register_validator_with_sig(STRANGER, DELEGATE, &key, B256::ZERO, None)
        },
        "Revert: unauthorized: caller must be owner or validator itself",
    )
}

#[test]
fn operational_delegate_is_rejected_before_node_checks() -> Result<()> {
    assert_rejected_first(
        40,
        delegate_assigned,
        |vs| {
            let key = dummy_consensus_pubkey(0xD3);
            vs.register_validator_with_sig(OWNER, DELEGATE, &key, B256::ZERO, None)
        },
        "Revert: validator address is already assigned as an operational delegate",
    )
}

#[test]
fn zero_node_id_is_rejected_before_proof_of_possession() -> Result<()> {
    assert_rejected_first(
        40,
        |_| Ok(()),
        |vs| {
            let key = dummy_consensus_pubkey(0xD1);
            vs.register_validator_with_sig(OWNER, VALIDATOR, &key, B256::ZERO, None)
        },
        "Revert: Radicle NodeId must not be zero",
    )
}

#[test]
fn taken_node_id_is_rejected_before_proof_of_possession() -> Result<()> {
    assert_rejected_first(
        40,
        other_registered,
        |vs| {
            let key = dummy_consensus_pubkey(0xD1);
            vs.register_validator_with_sig(OWNER, VALIDATOR, &key, node_id(OTHER), None)
        },
        "Revert: Radicle NodeId already registered by another validator",
    )
}

#[test]
fn missing_proof_of_possession_is_rejected_before_key_reuse() -> Result<()> {
    assert_rejected_first(
        40,
        other_registered,
        |vs| {
            let reused = dummy_consensus_pubkey(0xD2);
            vs.register_validator_with_sig(OWNER, VALIDATOR, &reused, node_id(VALIDATOR), None)
        },
        "Revert: validator registration requires BLS proof-of-possession signature",
    )
}

#[test]
fn malformed_proof_inputs_are_rejected_key_first() -> Result<()> {
    assert_rejected_first(
        40,
        |_| Ok(()),
        |vs| {
            let node = node_id(VALIDATOR);
            vs.register_validator_with_sig(OWNER, VALIDATOR, &NOT_A_POINT, node, Some(&[0; 96]))
        },
        "Revert: invalid BLS public key",
    )?;
    let key: [u8; 48] = bls_key(0x31)?.sk_to_pk().to_bytes();
    assert_rejected_first(
        40,
        |_| Ok(()),
        |vs| {
            let node = node_id(VALIDATOR);
            vs.register_validator_with_sig(OWNER, VALIDATOR, &key, node, Some(&[0; 96]))
        },
        "Revert: invalid BLS signature",
    )
}

#[test]
fn wrong_proof_of_possession_is_rejected_before_key_reuse() -> Result<()> {
    let secret = bls_key(0x32)?;
    let key: [u8; 48] = secret.sk_to_pk().to_bytes();
    let wrong_node = proof_of_possession(&secret, VALIDATOR, node_id(OTHER));
    assert_rejected_first(
        40,
        |vs| vs.register_validator(OWNER, OTHER, &key),
        |vs| {
            let node = node_id(VALIDATOR);
            vs.register_validator_with_sig(OWNER, VALIDATOR, &key, node, Some(&wrong_node))
        },
        "Revert: invalid BLS registration signature",
    )
}

#[test]
fn self_registration_cap_is_checked_before_key_reuse() -> Result<()> {
    assert_rejected_first(
        40,
        |vs| {
            for seed in 0..crate::runtime::MAX_SELF_REGISTERED_UNSTAKED {
                let mut bytes = [0x5a; 20];
                bytes[16..].copy_from_slice(&seed.to_be_bytes());
                let mut key = [0x5a; 48];
                key[44..].copy_from_slice(&seed.to_be_bytes());
                vs.register_validator(OWNER, Address::from(bytes), &key)?;
            }
            other_registered(vs)
        },
        |vs| vs.register_validator(VALIDATOR, VALIDATOR, &dummy_consensus_pubkey(0xD2)),
        "Revert: self-registration limit reached: too many unstaked REGISTERED validators \
         (owner may register directly)",
    )
}

#[test]
fn key_reuse_is_rejected_before_registry_state_checks() -> Result<()> {
    assert_rejected_first(
        40,
        |vs| {
            vs.register_validator(OWNER, VALIDATOR, &dummy_consensus_pubkey(0xD1))?;
            activate_for_test(vs, VALIDATOR);
            other_registered(vs)
        },
        |vs| vs.register_validator(OWNER, VALIDATOR, &dummy_consensus_pubkey(0xD2)),
        "Revert: BLS consensus pubkey already registered by another validator",
    )
}

#[test]
fn registered_validator_that_is_not_inactive_is_rejected() -> Result<()> {
    assert_rejected_first(
        40,
        |vs| {
            vs.register_validator(OWNER, VALIDATOR, &dummy_consensus_pubkey(0xD1))?;
            activate_for_test(vs, VALIDATOR);
            Ok(())
        },
        |vs| vs.register_validator(OWNER, VALIDATOR, &dummy_consensus_pubkey(0xD5)),
        "Revert: validator already registered",
    )
}

#[test]
fn reregistration_with_another_node_id_is_rejected_before_cooldown() -> Result<()> {
    let secret = bls_key(0x33)?;
    let key: [u8; 48] = secret.sk_to_pk().to_bytes();
    let new_node = B256::repeat_byte(0x77);
    let proof = proof_of_possession(&secret, VALIDATOR, new_node);
    assert_rejected_first(
        40,
        |vs| {
            vs.register_validator(OWNER, VALIDATOR, &dummy_consensus_pubkey(0xD1))?;
            make_inactive_for_test(vs, VALIDATOR);
            vs.config_reregistration_cooldown.write(10)
        },
        |vs| vs.register_validator_with_sig(OWNER, VALIDATOR, &key, new_node, Some(&proof)),
        "Revert: inactive validator must keep its Radicle NodeId until final cleanup",
    )
}

/// VALIDATOR is INACTIVE after a deactivation at height 1, with an open OCOMP
/// recovery window.
fn inactive_with_open_recovery(vs: &mut ValidatorSet<'_>) -> Result<()> {
    vs.register_validator(OWNER, VALIDATOR, &dummy_consensus_pubkey(0xD1))?;
    make_inactive_for_test(vs, VALIDATOR);
    vs.val_ocomp_recovery_deadline.write(&VALIDATOR, 5)
}

#[test]
fn reregistration_cooldown_is_checked_before_the_recovery_window() -> Result<()> {
    assert_rejected_first(
        40,
        |vs| {
            inactive_with_open_recovery(vs)?;
            vs.config_reregistration_cooldown.write(10)
        },
        |vs| vs.register_validator(OWNER, VALIDATOR, &dummy_consensus_pubkey(0xD5)),
        "Revert: re-registration cooldown not expired",
    )?;
    assert_rejected_first(
        40,
        inactive_with_open_recovery,
        |vs| vs.register_validator(OWNER, VALIDATOR, &dummy_consensus_pubkey(0xD5)),
        "Revert: cannot re-register while an OCOMP recovery window is open",
    )
}

#[test]
fn full_registry_is_rejected_before_any_write() -> Result<()> {
    assert_rejected_first(
        1,
        other_registered,
        |vs| vs.register_validator(OWNER, VALIDATOR, &dummy_consensus_pubkey(0xD1)),
        "Revert: max validators reached",
    )
}

/// The registry columns that a registration of VALIDATOR can write, with the
/// consensus keys `keys`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RegistryView {
    index: u64,
    address_at_index: Address,
    count: u32,
    key_halves: (B256, B256),
    status: u8,
    history: [u64; 6],
    node: B256,
    node_owner: Address,
    key_owners: Vec<Address>,
    pending_set_change: bool,
}

type ViewResult = std::result::Result<RegistryView, String>;

fn registry_view(
    storage: &mut HashMapStorageProvider,
    keys: &[[u8; 48]],
    index: u64,
) -> ViewResult {
    storage
        .enter(|storage| -> Result<RegistryView> {
            let vs = ValidatorSet::new(storage);
            let mut key_owners = Vec::with_capacity(keys.len());
            for key in keys {
                key_owners.push(
                    vs.consensus_pubkey_hash_to_address
                        .read(&ValidatorSet::consensus_pubkey_hash(key))?,
                );
            }
            Ok(RegistryView {
                index: vs.address_to_index.read(&VALIDATOR)?,
                address_at_index: vs.index_to_address.read(&index)?,
                count: vs.validator_count.read()?,
                key_halves: (
                    vs.val_consensus_pubkey_lo.read(&VALIDATOR)?,
                    vs.val_consensus_pubkey_hi.read(&VALIDATOR)?,
                ),
                status: vs.val_status.read(&VALIDATOR)?,
                history: [
                    vs.val_slash_count.read(&VALIDATOR)?,
                    vs.val_missed_blocks.read(&VALIDATOR)?,
                    vs.val_missed_votes.read(&VALIDATOR)?,
                    vs.val_blocks_proposed.read(&VALIDATOR)?,
                    vs.val_joined_at_height.read(&VALIDATOR)?,
                    vs.val_deactivated_at_height.read(&VALIDATOR)?,
                ],
                node: vs.val_radicle_node_id.read(&VALIDATOR)?,
                node_owner: vs.radicle_node_id_to_validator.read(&node_id(VALIDATOR))?,
                key_owners,
                pending_set_change: vs.has_pending_set_change()?,
            })
        })
        .map_err(|error| error.to_string())
}

/// Seeds storage with room for ten validators and a cleared set-change flag.
fn registration_seed(
    setup: impl Fn(&mut ValidatorSet<'_>) -> Result<()>,
) -> HashMapStorageProvider {
    let mut storage = configured_storage(10);
    let seeded = storage.enter(|storage| {
        let mut vs = ValidatorSet::new(storage);
        setup(&mut vs)?;
        vs.test_set_pending_set_change(false)
    });
    assert_eq!(outcome_text(seeded), "Ok(())", "fixture setup failed");
    storage
}

#[test]
fn first_registration_writes_its_registry_bundle_atomically() -> Result<()> {
    let key = dummy_consensus_pubkey(0xD1);
    let seed = || registration_seed(other_registered);
    let initial = registry_view(&mut seed(), &[key], 2);
    let views = HashMapStorageProvider::mutation_prefix_views(
        seed,
        |storage| ValidatorSet::new(storage).register_validator(OWNER, VALIDATOR, &key),
        |storage| registry_view(storage, &[key], 2),
    )?;
    let mut lo = [0u8; 32];
    lo[0] = 0xD1;
    let expected = RegistryView {
        index: 2,
        address_at_index: VALIDATOR,
        count: 2,
        key_halves: (B256::from(lo), B256::ZERO),
        status: status::REGISTERED,
        history: [0, 0, 0, 0, 1, 0],
        node: node_id(VALIDATOR),
        node_owner: VALIDATOR,
        key_owners: vec![VALIDATOR],
        pending_set_change: true,
    };
    // index and reverse index, key low and high halves, status (a write of
    // REGISTERED = 0 over the absent row), join height, key owner, node id,
    // node owner, count, set-change flag, and ValidatorRegistered.
    assert_eq!(
        views,
        MutationPrefixViews {
            before_mutation: vec![initial; 12],
            complete: Ok(expected),
            mutations: 12,
        }
    );
    Ok(())
}

#[test]
fn reregistration_writes_its_registry_bundle_atomically() -> Result<()> {
    let old_key = dummy_consensus_pubkey(0xD1);
    let new_key = [0xD6; 48];
    let seed = || {
        registration_seed(|vs| {
            vs.register_validator(OWNER, VALIDATOR, &old_key)?;
            make_inactive_for_test(vs, VALIDATOR);
            Ok(())
        })
    };
    let keys = [old_key, new_key];
    let initial = registry_view(&mut seed(), &keys, 1);
    let views = HashMapStorageProvider::mutation_prefix_views(
        seed,
        |storage| ValidatorSet::new(storage).register_validator(OWNER, VALIDATOR, &new_key),
        |storage| registry_view(storage, &keys, 1),
    )?;
    let (Ok(before), Ok(after)) = (&initial, &views.complete) else {
        return Err(PrecompileError::Fatal(format!(
            "registry view failed: {initial:?} / {:?}",
            views.complete
        )));
    };
    assert_eq!(after.index, 1);
    assert_eq!(after.count, 1);
    assert_eq!(after.status, status::REGISTERED);
    assert_eq!(after.key_owners, vec![Address::ZERO, VALIDATOR]);
    assert_eq!(after.history, [0, 0, 0, 0, 1, 0]);
    assert!(after.pending_set_change);
    let changed = [
        before.key_halves.0 != after.key_halves.0,
        before.key_halves.1 != after.key_halves.1,
        before.status != after.status,
        before.pending_set_change != after.pending_set_change,
    ]
    .into_iter()
    .filter(|changed| *changed)
    .count()
        + before
            .history
            .iter()
            .zip(after.history)
            .filter(|(before, after)| **before != *after)
            .count()
        + before
            .key_owners
            .iter()
            .zip(&after.key_owners)
            .filter(|(before, after)| before != after)
            .count();
    // The changed columns, plus ValidatorRegistered.
    assert_eq!(
        views,
        MutationPrefixViews {
            before_mutation: vec![initial.clone(); changed + 1],
            complete: views.complete.clone(),
            mutations: changed + 1,
        }
    );
    Ok(())
}

/// Canonical state couples the BLS share with the status: ACTIVE, EXITING and
/// retained JAILED rows hold a share, every other status holds none. An
/// ACTIVE row without a share is corrupt and never decodes, so the live-signer
/// rule is observable through REGISTERED (no share) and EXITING (a share
/// without ACTIVE status).
#[test]
fn role_signer_must_be_active_with_a_bls_share() -> Result<()> {
    let role = ValidatorDelegateRole::Oracle;
    let mut storage = configured_storage(10);
    storage.enter(|storage| -> Result<()> {
        let mut vs = ValidatorSet::new(storage);
        vs.register_validator(OWNER, VALIDATOR, &dummy_consensus_pubkey(0xD1))?;
        vs.register_validator(OWNER, OTHER, &dummy_consensus_pubkey(0xD2))?;
        assert_eq!(vs.resolve_validator_for_role(VALIDATOR, role)?, None);
        vs.set_delegate(VALIDATOR, role, DELEGATE)?;
        assert_eq!(vs.resolve_validator_for_role(DELEGATE, role)?, None);

        vs.activate_validator_via_boundary_for_test(VALIDATOR)?;
        assert!(vs.val_has_bls_share.read(&VALIDATOR)?);
        assert_eq!(
            vs.resolve_validator_for_role(DELEGATE, role)?,
            Some(VALIDATOR)
        );
        assert_eq!(vs.resolve_validator_for_role(VALIDATOR, role)?, None);
        vs.revoke_delegate(VALIDATOR, role)?;
        assert_eq!(
            vs.resolve_validator_for_role(VALIDATOR, role)?,
            Some(VALIDATOR)
        );

        vs.set_delegate(VALIDATOR, role, DELEGATE)?;
        vs.deactivate_validator(OWNER, VALIDATOR)?;
        assert!(vs.val_has_bls_share.read(&VALIDATOR)?);
        assert_eq!(vs.resolve_validator_for_role(DELEGATE, role)?, None);
        vs.revoke_delegate(VALIDATOR, role)?;
        assert_eq!(vs.resolve_validator_for_role(VALIDATOR, role)?, None);
        assert_eq!(vs.resolve_validator_for_role(OTHER, role)?, None);
        assert_eq!(vs.resolve_validator_for_role(STRANGER, role)?, None);
        Ok(())
    })
}
