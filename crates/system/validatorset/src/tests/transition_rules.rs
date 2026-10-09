//! Characterization of the punitive, readiness, founder-import and
//! participation rules: the exact rejection of each check in its order, the
//! storage writes and events of each accepted transition, and the atomicity of
//! the checkpointed ones.

use super::*;
use crate::precompile::IValidatorSet;
use alloy_sol_types::SolEvent;
use outbe_ocomp_protocol::committee::OcompKeyRegistrationV1;
use outbe_primitives::addresses::VALIDATOR_SET_ADDRESS;
use outbe_primitives::error::Result;

const FIRST: Address = address!("0x00000000000000000000000000000000000000E1");
const SECOND: Address = address!("0x00000000000000000000000000000000000000E2");
const UNKNOWN: Address = address!("0x00000000000000000000000000000000000000EF");

/// The exact text of a result: `Revert: <message>` or `Fatal: <message>` for
/// a rejection, else the debug form of the result.
fn outcome_text<T: std::fmt::Debug>(result: Result<T>) -> String {
    match result {
        Err(PrecompileError::Revert(message)) => format!("Revert: {message}"),
        Err(PrecompileError::Fatal(message)) => format!("Fatal: {message}"),
        other => format!("{other:?}"),
    }
}

/// What one call did: its exact result text, its storage writes and events,
/// and the topics of the ValidatorSet events it emitted, in order.
#[derive(Debug, PartialEq, Eq)]
struct CallEffect {
    outcome: String,
    mutations: usize,
    events: Vec<B256>,
}

fn call_effect<T: std::fmt::Debug>(
    storage: &mut HashMapStorageProvider,
    call: impl FnOnce(&mut ValidatorSet<'_>) -> Result<T>,
) -> CallEffect {
    let events_before = storage.get_ordered_events().len();
    storage.clear_mutation_failure();
    let result = storage.enter(|storage| call(&mut ValidatorSet::new(storage)));
    let mutations = storage.clear_mutation_failure();
    let events = storage.get_ordered_events()[events_before..]
        .iter()
        .filter(|log| log.address == VALIDATOR_SET_ADDRESS)
        .filter_map(|log| log.data.topics().first().copied())
        .collect();
    CallEffect {
        outcome: outcome_text(result),
        mutations,
        events,
    }
}

/// Storage with room for ten validators at block 1 after `setup`, with a
/// cleared set-change flag.
fn seeded(
    setup: impl FnOnce(&mut ValidatorSet<'_>) -> Result<()>,
) -> Result<HashMapStorageProvider> {
    seeded_on(configured_storage(10), setup)
}

fn seeded_on(
    mut storage: HashMapStorageProvider,
    setup: impl FnOnce(&mut ValidatorSet<'_>) -> Result<()>,
) -> Result<HashMapStorageProvider> {
    storage.enter(|storage| {
        let mut vs = ValidatorSet::new(storage);
        setup(&mut vs)?;
        vs.test_set_pending_set_change(false)
    })?;
    Ok(storage)
}

/// Configured storage like [`configured_storage`] for another chain identity.
fn storage_for_chain(chain_id: u64, genesis_hash: B256) -> Result<HashMapStorageProvider> {
    let mut storage = HashMapStorageProvider::new_with_chain_identity(chain_id, genesis_hash);
    storage.set_block_number(1);
    storage.enter(|storage| {
        let mut vs = ValidatorSet::new(storage);
        vs.config_owner.write(OWNER)?;
        vs.set_config_max_validators(10)?;
        vs.config_epoch_length_blocks.write(10)
    })?;
    Ok(storage)
}

/// Requires `call` to return exactly `expected` with no storage write and no
/// event.
fn assert_without_effect<T: std::fmt::Debug>(
    storage: &mut HashMapStorageProvider,
    call: impl FnOnce(&mut ValidatorSet<'_>) -> Result<T>,
    expected: &str,
) {
    assert_eq!(
        call_effect(storage, call),
        CallEffect {
            outcome: expected.to_string(),
            mutations: 0,
            events: Vec::new(),
        }
    );
}

fn register(vs: &mut ValidatorSet<'_>, validator: Address) -> Result<()> {
    vs.register_validator(
        OWNER,
        validator,
        &dummy_consensus_pubkey(validator.as_slice()[19]),
    )
}

/// An ACTIVE validator with a live BLS share.
fn participant(vs: &mut ValidatorSet<'_>, validator: Address) -> Result<()> {
    register(vs, validator)?;
    activate_staked_for_test(vs, validator);
    vs.val_has_bls_share.write(&validator, true)
}

/// A PENDING validator that waits for its readiness confirmation.
fn waiting_for_readiness(vs: &mut ValidatorSet<'_>, validator: Address) -> Result<()> {
    register(vs, validator)?;
    vs.mark_pending(validator)
}

fn retained_jail(vs: &mut ValidatorSet<'_>) -> Result<()> {
    participant(vs, FIRST)?;
    vs.jail_validator(FIRST)
}

#[test]
fn punishment_rejects_absent_and_waiting_validators_without_effect() -> Result<()> {
    let absent = "Revert: validator not registered";
    assert_without_effect(
        &mut seeded(|_| Ok(()))?,
        |vs| vs.jail_validator(FIRST),
        absent,
    );
    assert_without_effect(
        &mut seeded(|_| Ok(()))?,
        |vs| vs.force_exit_validator(FIRST),
        absent,
    );
    assert_without_effect(
        &mut seeded(|vs| register(vs, FIRST))?,
        |vs| vs.jail_validator(FIRST),
        "Revert: cannot jail validator with status 0: only ACTIVE, EXITING, UNBONDING, or INACTIVE allowed",
    );
    assert_without_effect(
        &mut seeded(|vs| waiting_for_readiness(vs, FIRST))?,
        |vs| vs.force_exit_validator(FIRST),
        "Revert: cannot force-exit validator with status 1: only ACTIVE, EXITING, UNBONDING, or INACTIVE allowed",
    );
    assert_without_effect(
        &mut seeded(retained_jail)?,
        |vs| vs.force_exit_validator(FIRST),
        "Revert: cannot force-exit validator with status 6: only ACTIVE, EXITING, UNBONDING, or INACTIVE allowed",
    );
    Ok(())
}

#[test]
fn punishment_of_a_departed_validator_is_a_no_op() -> Result<()> {
    assert_without_effect(
        &mut seeded(retained_jail)?,
        |vs| vs.jail_validator_deferred(FIRST),
        "Ok(None)",
    );
    let exiting = |vs: &mut ValidatorSet<'_>| {
        participant(vs, FIRST)?;
        vs.deactivate_validator(OWNER, FIRST)
    };
    assert_without_effect(
        &mut seeded(exiting)?,
        |vs| vs.jail_validator_deferred(FIRST),
        "Ok(None)",
    );
    assert_without_effect(
        &mut seeded(exiting)?,
        |vs| vs.force_exit_validator(FIRST),
        "Ok(())",
    );
    let inactive = |vs: &mut ValidatorSet<'_>| {
        register(vs, FIRST)?;
        make_inactive_for_test(vs, FIRST);
        Ok(())
    };
    assert_without_effect(
        &mut seeded(inactive)?,
        |vs| vs.force_exit_validator(FIRST),
        "Ok(())",
    );
    Ok(())
}

/// The status, slash count, deactivation height, jail height and set-change
/// flag of FIRST.
fn punishment_columns(storage: &mut HashMapStorageProvider) -> Result<(u8, u64, u64, u64, bool)> {
    storage.enter(|storage| {
        let vs = ValidatorSet::new(storage);
        Ok((
            vs.val_status.read(&FIRST)?,
            vs.val_slash_count.read(&FIRST)?,
            vs.val_deactivated_at_height.read(&FIRST)?,
            vs.val_jailed_at_height.read(&FIRST)?,
            vs.has_pending_set_change()?,
        ))
    })
}

#[test]
fn punishment_writes_status_history_flag_and_events_in_one_checkpoint() -> Result<()> {
    let mut jailed = seeded(|vs| participant(vs, FIRST))?;
    let effect = call_effect(&mut jailed, |vs| vs.jail_validator(FIRST));
    assert_eq!(effect.outcome, "Ok(())");
    assert_eq!(
        effect.events,
        vec![IValidatorSet::ValidatorJailed::SIGNATURE_HASH]
    );
    // status, slash count, deactivation height, jail height, flag, event
    assert_eq!(effect.mutations, 6);
    assert_eq!(
        punishment_columns(&mut jailed)?,
        (status::JAILED, 1, 1, 1, true)
    );

    let mut exited = seeded(|vs| participant(vs, FIRST))?;
    let effect = call_effect(&mut exited, |vs| vs.force_exit_validator(FIRST));
    assert_eq!(effect.outcome, "Ok(())");
    assert_eq!(
        effect.events,
        vec![
            IValidatorSet::ValidatorDeactivated::SIGNATURE_HASH,
            IValidatorSet::ValidatorForcedExit::SIGNATURE_HASH,
        ]
    );
    // status, slash count, deactivation height, flag, two events
    assert_eq!(effect.mutations, 6);
    assert_eq!(
        punishment_columns(&mut exited)?,
        (status::EXITING, 1, 1, 0, true)
    );
    Ok(())
}

#[test]
fn set_changing_transitions_are_atomic_with_their_events() -> Result<()> {
    let seed = || {
        let mut storage = configured_storage(10);
        let setup = storage.enter(|storage| participant(&mut ValidatorSet::new(storage), FIRST));
        assert_eq!(outcome_text(setup), "Ok(())", "fixture setup failed");
        storage
    };
    let view = |storage: &mut HashMapStorageProvider| {
        let columns = outcome_text(punishment_columns(storage));
        (columns, storage.get_ordered_events().len())
    };
    let initial = view(&mut seed());
    for jail in [true, false] {
        let views = HashMapStorageProvider::mutation_prefix_views(
            seed,
            |storage| {
                let mut vs = ValidatorSet::new(storage);
                if jail {
                    vs.jail_validator(FIRST)
                } else {
                    vs.deactivate_validator(OWNER, FIRST)
                }
            },
            view,
        )?;
        assert!(views.mutations > 0);
        assert!(views
            .before_mutation
            .iter()
            .all(|before| *before == initial));
        assert_ne!(views.complete, initial);
    }
    Ok(())
}

#[test]
fn deactivation_unjail_and_tee_expiry_emit_their_events() -> Result<()> {
    let mut deactivated = seeded(|vs| participant(vs, FIRST))?;
    let effect = call_effect(&mut deactivated, |vs| vs.deactivate_validator(OWNER, FIRST));
    assert_eq!(effect.outcome, "Ok(())");
    assert_eq!(
        effect.events,
        vec![IValidatorSet::ValidatorDeactivated::SIGNATURE_HASH]
    );

    let mut expired = seeded(|vs| participant(vs, FIRST))?;
    let effect = call_effect(&mut expired, |vs| vs.jail_validator_for_tee_expiry(FIRST));
    assert_eq!(effect.outcome, "Ok(true)");
    assert_eq!(
        effect.events,
        vec![IValidatorSet::ValidatorJailed::SIGNATURE_HASH]
    );
    assert_without_effect(
        &mut expired,
        |vs| vs.jail_validator_for_tee_expiry(FIRST),
        "Ok(false)",
    );

    let mut unjailed = seeded(|vs| {
        retained_jail(vs)?;
        vs.activate_reshared_set(&[], B256::ZERO)
    })?;
    let effect = call_effect(&mut unjailed, |vs| vs.unjail_after_stake_check(FIRST));
    assert_eq!(effect.outcome, "Ok(())");
    assert_eq!(
        effect.events,
        vec![IValidatorSet::ValidatorUnjailed::SIGNATURE_HASH]
    );
    Ok(())
}

#[test]
fn unjail_and_tee_expiry_reject_ineligible_validators_without_effect() -> Result<()> {
    assert_without_effect(
        &mut seeded(retained_jail)?,
        |vs| vs.unjail_after_stake_check(FIRST),
        "Revert: jailed validator is still retained in the current committee",
    );
    assert_without_effect(
        &mut seeded(|vs| participant(vs, FIRST))?,
        |vs| vs.unjail_after_stake_check(FIRST),
        "Revert: unjailValidator requires JAILED status, got 2",
    );
    assert_without_effect(
        &mut seeded(|vs| register(vs, FIRST))?,
        |vs| vs.jail_validator_for_tee_expiry(FIRST),
        &format!("Fatal: TEE expiry sweep selected validator {FIRST} with ineligible status 0"),
    );
    Ok(())
}

fn registration_of(
    vs: &ValidatorSet<'_>,
    validator: Address,
    key_seed: u8,
) -> Result<(OcompKeyRegistrationV1, Vec<u8>)> {
    let consensus_pubkey = vs
        .get_validator(validator)?
        .ok_or_else(|| PrecompileError::Fatal(format!("fixture validator {validator} is absent")))?
        .consensus_pubkey;
    Ok(ocomp_registration(validator, &consensus_pubkey, key_seed))
}

#[test]
fn readiness_checks_lifecycle_then_encoding_then_chain_binding() -> Result<()> {
    assert_without_effect(
        &mut seeded(|_| Ok(()))?,
        |vs| vs.confirm_validator_ready(FIRST, &[0xff]),
        "Revert: validator not registered",
    );
    assert_without_effect(
        &mut seeded(|vs| participant(vs, FIRST))?,
        |vs| vs.confirm_validator_ready(FIRST, &[0xff]),
        "Revert: confirmValidatorReady requires PENDING status, got 2",
    );
    let Err(decode_error) = OcompKeyRegistrationV1::decode_canonical(
        &[0xff],
        &outbe_ocomp_protocol::profile::poc_schema_limits(),
    ) else {
        return Err(PrecompileError::Fatal(
            "0xff decodes as a registration".into(),
        ));
    };
    assert_without_effect(
        &mut seeded(|vs| waiting_for_readiness(vs, FIRST))?,
        |vs| vs.confirm_validator_ready(FIRST, &[0xff]),
        &format!("Revert: invalid OCOMP registration: {decode_error}"),
    );
    let other_genesis = B256::repeat_byte(0x99);
    for (chain_id, genesis_hash, expected) in [
        (
            CHAIN_ID + 1,
            other_genesis,
            "Revert: OCOMP registration chain id mismatch",
        ),
        (
            CHAIN_ID,
            other_genesis,
            "Revert: OCOMP registration genesis hash mismatch",
        ),
    ] {
        let mut storage = seeded_on(storage_for_chain(chain_id, genesis_hash)?, |vs| {
            waiting_for_readiness(vs, FIRST)
        })?;
        let encoded = storage
            .enter(|storage| registration_of(&ValidatorSet::new(storage), FIRST, 0x41))?
            .1;
        assert_without_effect(
            &mut storage,
            |vs| vs.confirm_validator_ready(FIRST, &encoded),
            expected,
        );
    }
    Ok(())
}

#[test]
fn readiness_checks_identity_then_key_pin_then_key_owner() -> Result<()> {
    let mut storage = seeded(|vs| {
        waiting_for_readiness(vs, FIRST)?;
        waiting_for_readiness(vs, SECOND)
    })?;
    let (first, second_with_first_key, wrong_identity, first_rotated) =
        storage.enter(|storage| {
            let vs = ValidatorSet::new(storage);
            Ok::<_, PrecompileError>((
                registration_of(&vs, FIRST, 0x41)?.1,
                registration_of(&vs, SECOND, 0x41)?.1,
                ocomp_registration(FIRST, &dummy_consensus_pubkey(0x77), 0x42).1,
                registration_of(&vs, FIRST, 0x43)?.1,
            ))
        })?;
    assert_without_effect(
        &mut storage,
        |vs| vs.confirm_validator_ready(FIRST, &wrong_identity),
        "Revert: OCOMP registration validator identity mismatch",
    );
    let effect = call_effect(&mut storage, |vs| vs.confirm_validator_ready(FIRST, &first));
    assert_eq!(
        (effect.outcome.as_str(), effect.events.len()),
        ("Ok(())", 0)
    );
    assert_without_effect(
        &mut storage,
        |vs| vs.confirm_validator_ready(FIRST, &first_rotated),
        "Revert: OCOMP public key is immutable in key_epoch 1",
    );
    assert_without_effect(
        &mut storage,
        |vs| vs.confirm_validator_ready(SECOND, &second_with_first_key),
        "Revert: OCOMP public key already registered by another validator",
    );
    storage.enter(|storage| -> Result<()> {
        let vs = ValidatorSet::new(storage);
        assert!(matches!(
            vs.validator_lifecycle(FIRST)?,
            ValidatorLifecycle::Joining(_)
        ));
        assert!(vs.val_join_confirmed.read(&FIRST)?);
        assert_eq!(vs.val_ocomp_registration.get_bytes(&FIRST).read()?, first);
        assert!(vs.has_pending_set_change()?);
        Ok(())
    })
}

/// Two founders, FIRST then SECOND, both ACTIVE without OCOMP registrations.
fn founders(vs: &mut ValidatorSet<'_>) -> Result<()> {
    participant(vs, FIRST)?;
    participant(vs, SECOND)
}

fn founder_registrations(
    storage: &mut HashMapStorageProvider,
    seeds: [(Address, u8); 2],
) -> Result<Vec<OcompKeyRegistrationV1>> {
    storage.enter(|storage| {
        let vs = ValidatorSet::new(storage);
        let mut registrations = Vec::with_capacity(seeds.len());
        for (validator, key_seed) in seeds {
            registrations.push(registration_of(&vs, validator, key_seed)?.0);
        }
        Ok(registrations)
    })
}

#[test]
fn founder_import_checks_cover_proof_binding_identity_and_uniqueness_in_order() -> Result<()> {
    let mut storage = seeded(founders)?;
    let valid = founder_registrations(&mut storage, [(FIRST, 0x51), (SECOND, 0x52)])?;
    assert_without_effect(
        &mut storage,
        |vs| vs.initialize_founder_ocomp_registrations(&valid[..1]),
        "Fatal: OCOMP founder registrations must exactly cover ACTIVE ValidatorSet: 1 registrations for 2 validators",
    );
    let mut tampered = valid.clone();
    tampered[0].proof_of_possession[0] ^= 1;
    let Err(proof_error) = tampered[0]
        .validate_proof_of_possession(&outbe_ocomp_protocol::profile::poc_schema_limits())
    else {
        return Err(PrecompileError::Fatal(
            "tampered proof still verifies".into(),
        ));
    };
    assert_without_effect(
        &mut storage,
        |vs| vs.initialize_founder_ocomp_registrations(&tampered),
        &format!("Fatal: invalid OCOMP founder proof of possession: {proof_error}"),
    );
    let swapped = vec![valid[1].clone(), valid[0].clone()];
    assert_without_effect(
        &mut storage,
        |vs| vs.initialize_founder_ocomp_registrations(&swapped),
        &format!("Fatal: OCOMP founder registration identity mismatch for {FIRST}"),
    );
    let shared_key = founder_registrations(&mut storage, [(FIRST, 0x51), (SECOND, 0x51)])?;
    assert_without_effect(
        &mut storage,
        |vs| vs.initialize_founder_ocomp_registrations(&shared_key),
        "Fatal: OCOMP founder registrations contain duplicate identity or key",
    );
    let mut other_chain = seeded_on(
        storage_for_chain(CHAIN_ID, B256::repeat_byte(0x99))?,
        founders,
    )?;
    let unbound = founder_registrations(&mut other_chain, [(FIRST, 0x51), (SECOND, 0x52)])?;
    assert_without_effect(
        &mut other_chain,
        |vs| vs.initialize_founder_ocomp_registrations(&unbound),
        "Fatal: OCOMP founder registration chain binding mismatch",
    );
    Ok(())
}

#[test]
fn founder_import_is_atomic_exactly_replayable_and_rejects_partial_state() -> Result<()> {
    let mut storage = seeded(founders)?;
    let valid = founder_registrations(&mut storage, [(FIRST, 0x51), (SECOND, 0x52)])?;
    let effect = call_effect(&mut storage, |vs| {
        vs.initialize_founder_ocomp_registrations(&valid)
    });
    assert_eq!(
        (effect.outcome.as_str(), effect.events.len()),
        ("Ok(())", 0)
    );
    assert!(effect.mutations > 0);
    assert_without_effect(
        &mut storage,
        |vs| vs.initialize_founder_ocomp_registrations(&valid),
        "Ok(())",
    );

    let second_key_hash = keccak256(valid[1].core.ocomp_public_key_sec1);
    storage.enter(|storage| -> Result<()> {
        let vs = ValidatorSet::new(storage);
        vs.val_ocomp_registration.get_bytes(&SECOND).clear()?;
        vs.ocomp_key_hash_to_validator
            .write(&second_key_hash, Address::ZERO)
    })?;
    assert_without_effect(
        &mut storage,
        |vs| vs.initialize_founder_ocomp_registrations(&valid),
        "Fatal: partial OCOMP founder registration import is fatal",
    );
    let conflicting = founder_registrations(&mut storage, [(FIRST, 0x53), (SECOND, 0x52)])?;
    assert_without_effect(
        &mut storage,
        |vs| vs.initialize_founder_ocomp_registrations(&conflicting),
        &format!("Fatal: partial or conflicting OCOMP founder state for {FIRST}"),
    );
    Ok(())
}

#[test]
fn current_participation_checks_every_voter_before_absent_voters() -> Result<()> {
    let setup = |vs: &mut ValidatorSet<'_>| {
        participant(vs, FIRST)?;
        register(vs, SECOND)
    };
    assert_without_effect(
        &mut seeded(setup)?,
        |vs| vs.record_participation(&[FIRST, SECOND], &[FIRST]),
        &format!("Revert: voter is not a current consensus participant: {SECOND}"),
    );
    // No checkpoint: the first absent voter keeps its missed vote.
    let mut storage = seeded(setup)?;
    let effect = call_effect(&mut storage, |vs| {
        vs.record_participation(&[FIRST], &[FIRST, SECOND])
    });
    assert_eq!(
        effect.outcome,
        format!("Revert: absent voter is not a current consensus participant: {SECOND}")
    );
    assert_eq!(effect.mutations, 1);
    let missed =
        storage.enter(|storage| ValidatorSet::new(storage).val_missed_votes.read(&FIRST))?;
    assert_eq!(missed, 1);
    Ok(())
}

#[test]
fn finalized_participation_requires_registration_only() -> Result<()> {
    let setup = |vs: &mut ValidatorSet<'_>| register(vs, FIRST);
    assert_without_effect(
        &mut seeded(setup)?,
        |vs| vs.record_finalized_participation(&[FIRST, UNKNOWN], &[]),
        &format!("Revert: finalized voter is not a registered validator: {UNKNOWN}"),
    );
    let mut storage = seeded(setup)?;
    let effect = call_effect(&mut storage, |vs| {
        vs.record_finalized_participation(&[FIRST], &[FIRST, UNKNOWN])
    });
    assert_eq!(
        effect.outcome,
        format!("Revert: finalized absent voter is not a registered validator: {UNKNOWN}")
    );
    assert_eq!(effect.mutations, 1);
    Ok(())
}

#[test]
fn proposer_and_missed_block_counters_increment_once() -> Result<()> {
    assert_without_effect(
        &mut seeded(|vs| register(vs, FIRST))?,
        |vs| vs.record_proposer(FIRST),
        &format!("Revert: proposer is not a current consensus participant: {FIRST}"),
    );
    let mut storage = seeded(|vs| participant(vs, FIRST))?;
    assert_eq!(
        call_effect(&mut storage, |vs| vs.record_proposer(FIRST)).mutations,
        1
    );
    assert_eq!(
        call_effect(&mut storage, |vs| vs.record_missed_block(FIRST)).mutations,
        1
    );
    let counters = storage.enter(|storage| -> Result<(u64, u64)> {
        let vs = ValidatorSet::new(storage);
        Ok((
            vs.val_blocks_proposed.read(&FIRST)?,
            vs.val_missed_blocks.read(&FIRST)?,
        ))
    })?;
    assert_eq!(counters, (1, 1));
    Ok(())
}
