//! Characterization of the punitive, readiness, founder-import and
//! participation rules: the exact rejection of each check in its order, the
//! storage writes and events of each accepted transition, and the atomicity of
//! the checkpointed ones.

use super::*;
use crate::precompile::IValidatorSet;
use alloy_sol_types::{SolError, SolEvent};
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
        Err(PrecompileError::RevertBytes(bytes)) => super::typed_revert(bytes),
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
    let absent = super::typed_revert(IValidatorSet::ValidatorNotRegistered {}.abi_encode());
    assert_without_effect(
        &mut seeded(|_| Ok(()))?,
        |vs| vs.jail_validator(FIRST),
        absent.as_str(),
    );
    assert_without_effect(
        &mut seeded(|_| Ok(()))?,
        |vs| vs.force_exit_validator(FIRST),
        absent.as_str(),
    );
    assert_without_effect(
        &mut seeded(|vs| register(vs, FIRST))?,
        |vs| vs.jail_validator(FIRST),
        &super::typed_revert(
            IValidatorSet::CannotPunishValidatorStatus {
                action: "jail".into(),
                status: 0,
            }
            .abi_encode(),
        ),
    );
    assert_without_effect(
        &mut seeded(|vs| waiting_for_readiness(vs, FIRST))?,
        |vs| vs.force_exit_validator(FIRST),
        &super::typed_revert(
            IValidatorSet::CannotPunishValidatorStatus {
                action: "force-exit".into(),
                status: 1,
            }
            .abi_encode(),
        ),
    );
    assert_without_effect(
        &mut seeded(retained_jail)?,
        |vs| vs.force_exit_validator(FIRST),
        &super::typed_revert(
            IValidatorSet::CannotPunishValidatorStatus {
                action: "force-exit".into(),
                status: 6,
            }
            .abi_encode(),
        ),
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
        &super::typed_revert(IValidatorSet::JailedValidatorStillRetained {}.abi_encode()),
    );
    assert_without_effect(
        &mut seeded(|vs| participant(vs, FIRST))?,
        |vs| vs.unjail_after_stake_check(FIRST),
        &super::typed_revert(IValidatorSet::UnjailRequiresJailedStatus { status: 2 }.abi_encode()),
    );
    assert_without_effect(
        &mut seeded(|vs| register(vs, FIRST))?,
        |vs| vs.jail_validator_for_tee_expiry(FIRST),
        &super::typed_revert(
            IValidatorSet::TeeExpirySweepIneligibleStatus {
                addr: FIRST,
                status: 0,
            }
            .abi_encode(),
        ),
    );
    Ok(())
}

#[test]
fn readiness_checks_lifecycle_before_effects() -> Result<()> {
    assert_without_effect(
        &mut seeded(|_| Ok(()))?,
        |vs| vs.confirm_validator_ready(FIRST),
        &super::typed_revert(IValidatorSet::ValidatorNotRegistered {}.abi_encode()),
    );
    assert_without_effect(
        &mut seeded(|vs| participant(vs, FIRST))?,
        |vs| vs.confirm_validator_ready(FIRST),
        &super::typed_revert(
            IValidatorSet::ConfirmValidatorReadyRequiresPending { status: 2 }.abi_encode(),
        ),
    );
    let mut storage = seeded(|vs| waiting_for_readiness(vs, FIRST))?;
    let effect = call_effect(&mut storage, |vs| vs.confirm_validator_ready(FIRST));
    assert_eq!(effect.outcome, "Ok(())");
    assert_eq!(
        effect.events,
        vec![IValidatorSet::ValidatorReadyConfirmed::SIGNATURE_HASH]
    );
    let confirmed = storage
        .get_ordered_events()
        .iter()
        .rev()
        .find_map(|log| IValidatorSet::ValidatorReadyConfirmed::decode_log(log).ok())
        .expect("ready confirmation log");
    assert_eq!(confirmed.data.validator, FIRST);
    let repeat = call_effect(&mut storage, |vs| vs.confirm_validator_ready(FIRST));
    assert_eq!(repeat.outcome, "Ok(())");
    assert_eq!(repeat.events, Vec::<B256>::new());
    storage.enter(|storage| -> Result<()> {
        let vs = ValidatorSet::new(storage);
        assert!(vs.val_join_confirmed.read(&FIRST)?);
        assert!(vs.has_pending_set_change()?);
        Ok(())
    })
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
        &super::typed_revert(
            IValidatorSet::VoterNotConsensusParticipant { addr: SECOND }.abi_encode(),
        ),
    );
    // No checkpoint: the first absent voter keeps its missed vote.
    let mut storage = seeded(setup)?;
    let effect = call_effect(&mut storage, |vs| {
        vs.record_participation(&[FIRST], &[FIRST, SECOND])
    });
    assert_eq!(
        effect.outcome,
        super::typed_revert(
            IValidatorSet::AbsentVoterNotConsensusParticipant { addr: SECOND }.abi_encode(),
        )
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
        &super::typed_revert(
            IValidatorSet::FinalizedVoterNotRegistered { addr: UNKNOWN }.abi_encode(),
        ),
    );
    let mut storage = seeded(setup)?;
    let effect = call_effect(&mut storage, |vs| {
        vs.record_finalized_participation(&[FIRST], &[FIRST, UNKNOWN])
    });
    assert_eq!(
        effect.outcome,
        super::typed_revert(
            IValidatorSet::FinalizedAbsentVoterNotRegistered { addr: UNKNOWN }.abi_encode()
        )
    );
    assert_eq!(effect.mutations, 1);
    Ok(())
}

#[test]
fn proposer_and_missed_block_counters_increment_once() -> Result<()> {
    assert_without_effect(
        &mut seeded(|vs| register(vs, FIRST))?,
        |vs| vs.record_proposer(FIRST),
        &super::typed_revert(
            IValidatorSet::ProposerNotConsensusParticipant { addr: FIRST }.abi_encode(),
        ),
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

fn pending_logs(storage: &HashMapStorageProvider) -> Vec<Address> {
    storage
        .get_ordered_events()
        .iter()
        .filter(|log| log.address == VALIDATOR_SET_ADDRESS)
        .filter_map(|log| IValidatorSet::ValidatorPending::decode_log(log).ok())
        .map(|event| event.data.validator)
        .collect()
}

#[test]
fn stake_increase_emits_pending_only_when_minimum_is_reached() -> Result<()> {
    let minimum = U256::from(1_000u64);
    let mut storage = seeded(|vs| register(vs, FIRST))?;
    let below = call_effect(&mut storage, |vs| {
        vs.record_stake_increase(FIRST, U256::from(1u64), minimum)
    });
    assert_eq!(below.outcome, "Ok(())");
    assert_eq!(below.events, Vec::<B256>::new());

    let seed = || seeded(|vs| register(vs, FIRST)).unwrap();
    let view = |storage: &mut HashMapStorageProvider| {
        let columns = storage.enter(|storage| -> Result<_> {
            let vs = ValidatorSet::new(storage);
            Ok((vs.val_status.read(&FIRST)?, vs.has_pending_set_change()?))
        });
        (outcome_text(columns), pending_logs(storage))
    };
    let initial = view(&mut seed());
    let views = HashMapStorageProvider::mutation_prefix_views(
        seed,
        |storage| ValidatorSet::new(storage).record_stake_increase(FIRST, minimum, minimum),
        view,
    )?;
    assert!(views.mutations > 0);
    assert!(views
        .before_mutation
        .iter()
        .all(|before| *before == initial));
    assert_eq!(views.complete, ("Ok((1, true))".to_string(), vec![FIRST]));

    let mut pending = seeded(|vs| register(vs, FIRST))?;
    pending.enter(|storage| {
        ValidatorSet::new(storage).record_stake_increase(FIRST, minimum, minimum)
    })?;
    let repeat = call_effect(&mut pending, |vs| {
        vs.record_stake_increase(FIRST, minimum, minimum)
    });
    assert_eq!(repeat.outcome, "Ok(())");
    assert_eq!(repeat.events, Vec::<B256>::new());
    Ok(())
}

#[test]
fn mark_pending_and_unjail_do_not_emit_validator_pending() -> Result<()> {
    let mut storage = seeded(|vs| register(vs, FIRST))?;
    let effect = call_effect(&mut storage, |vs| vs.mark_pending(FIRST));
    assert_eq!(effect.outcome, "Ok(())");
    assert_eq!(effect.events, Vec::<B256>::new());
    assert_eq!(pending_logs(&storage), Vec::<Address>::new());

    let mut unjailed = seeded(|vs| {
        retained_jail(vs)?;
        vs.activate_reshared_set(&[], B256::ZERO)
    })?;
    let effect = call_effect(&mut unjailed, |vs| vs.unjail_after_stake_check(FIRST));
    assert_eq!(
        effect.events,
        vec![IValidatorSet::ValidatorUnjailed::SIGNATURE_HASH]
    );
    assert_eq!(pending_logs(&unjailed), Vec::<Address>::new());
    Ok(())
}

#[test]
fn demotion_and_slash_do_not_emit_pending_or_ready() -> Result<()> {
    let minimum = U256::from(1_000u64);
    let mut demoted = seeded(|vs| waiting_for_readiness(vs, FIRST))?;
    let effect = call_effect(&mut demoted, |vs| {
        vs.record_unstake(FIRST, U256::from(1u64), minimum, 0)
    });
    assert_eq!(effect.outcome, "Ok(())");
    assert_eq!(effect.events, Vec::<B256>::new());

    let mut slashed = seeded(|vs| participant(vs, FIRST))?;
    let effect = call_effect(&mut slashed, |vs| {
        vs.record_stake_slash(FIRST, U256::from(1u64), minimum, None)
    });
    assert_eq!(effect.outcome, "Ok(())");
    assert_eq!(effect.events, Vec::<B256>::new());
    Ok(())
}

#[test]
fn ready_confirmation_is_atomic_with_its_event() -> Result<()> {
    let seed = || seeded(|vs| waiting_for_readiness(vs, FIRST)).unwrap();
    let view = |storage: &mut HashMapStorageProvider| {
        let joined =
            storage.enter(|storage| ValidatorSet::new(storage).val_join_confirmed.read(&FIRST));
        let ready = storage
            .get_ordered_events()
            .iter()
            .filter(|log| log.address == VALIDATOR_SET_ADDRESS)
            .filter_map(|log| IValidatorSet::ValidatorReadyConfirmed::decode_log(log).ok())
            .map(|event| event.data.validator)
            .collect::<Vec<_>>();
        (outcome_text(joined), ready)
    };
    let initial = view(&mut seed());
    let views = HashMapStorageProvider::mutation_prefix_views(
        seed,
        |storage| ValidatorSet::new(storage).confirm_validator_ready(FIRST),
        view,
    )?;
    assert!(views.mutations > 0);
    assert!(views
        .before_mutation
        .iter()
        .all(|before| *before == initial));
    assert_eq!(views.complete, ("Ok(true)".to_string(), vec![FIRST]));
    Ok(())
}
