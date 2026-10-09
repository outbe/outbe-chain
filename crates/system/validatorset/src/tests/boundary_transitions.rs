//! Characterization of the validated-boundary transition matrix: the exact
//! error of every rejected lifecycle arm, the check precedence between
//! validators, and the complete write set of an accepted boundary.

use super::*;
use outbe_primitives::error::Result;
use outbe_primitives::storage::hashmap::MutationPrefixViews;
use std::cell::RefCell;

const MINIMUM: u64 = 1_000;
const A: Address = address!("0x00000000000000000000000000000000000000C1");
const B: Address = address!("0x00000000000000000000000000000000000000C2");
const C: Address = address!("0x00000000000000000000000000000000000000C3");
const D: Address = address!("0x00000000000000000000000000000000000000C4");
const E: Address = address!("0x00000000000000000000000000000000000000C5");
const F: Address = address!("0x00000000000000000000000000000000000000C6");
const G: Address = address!("0x00000000000000000000000000000000000000C7");
const UNREGISTERED: Address = address!("0x00000000000000000000000000000000000000CF");
const HASH: B256 = B256::with_last_byte(0xC0);

/// Storage at block 11 with an owner and room for ten validators, after
/// `setup`. Fixture exits and jails therefore record height 11.
fn boundary_storage(
    setup: impl FnOnce(&mut ValidatorSet<'_>) -> Result<()>,
) -> (HashMapStorageProvider, Result<()>) {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_block_number(11);
    let setup = storage.enter(|storage| {
        let mut vs = ValidatorSet::new(storage);
        vs.config_owner.write(OWNER)?;
        vs.config_max_validators.write(10)?;
        setup(&mut vs)
    });
    (storage, setup)
}

fn register(vs: &mut ValidatorSet<'_>, validator: Address) -> Result<()> {
    let seed = validator.as_slice()[19];
    vs.register_validator(OWNER, validator, &dummy_consensus_pubkey(seed))
}

fn active(vs: &mut ValidatorSet<'_>, validator: Address) -> Result<()> {
    register(vs, validator)?;
    activate_staked_for_test(vs, validator);
    Ok(())
}

fn joining(vs: &mut ValidatorSet<'_>, validator: Address) -> Result<()> {
    register(vs, validator)?;
    let minimum = U256::from(MINIMUM);
    vs.record_stake_increase(validator, minimum, minimum)?;
    confirm_ready(vs, validator, validator.as_slice()[19]);
    Ok(())
}

fn waiting_for_stake(vs: &mut ValidatorSet<'_>, validator: Address) -> Result<()> {
    joining(vs, validator)?;
    vs.record_unstake(validator, U256::from(1u64), U256::from(MINIMUM), 0)
}

fn waiting_for_readiness(vs: &mut ValidatorSet<'_>, validator: Address) -> Result<()> {
    waiting_for_stake(vs, validator)?;
    let minimum = U256::from(MINIMUM);
    vs.record_stake_increase(validator, minimum, minimum)
}

fn exiting(vs: &mut ValidatorSet<'_>, validator: Address) -> Result<()> {
    active(vs, validator)?;
    vs.deactivate_validator(OWNER, validator)
}

fn jail_retained(vs: &mut ValidatorSet<'_>, validator: Address) -> Result<()> {
    active(vs, validator)?;
    vs.jail_validator(validator)
}

/// The exact text of a boundary result: `Fatal: <message>` for a fatal
/// rejection, else the debug form of the result.
fn outcome_text(result: Result<()>) -> String {
    match result {
        Err(PrecompileError::Fatal(message)) => format!("Fatal: {message}"),
        other => format!("{other:?}"),
    }
}

/// Requires the boundary to fail with exactly `expected` before its first
/// storage write or event.
fn assert_rejected_before_writes(
    setup: impl FnOnce(&mut ValidatorSet<'_>) -> Result<()>,
    call: impl FnOnce(&mut ValidatorSet<'_>) -> Result<()>,
    expected: String,
) -> Result<()> {
    let (mut storage, setup) = boundary_storage(setup);
    setup?;
    storage.clear_mutation_failure();
    let result = storage.enter(|storage| call(&mut ValidatorSet::new(storage)));
    assert_eq!(outcome_text(result), format!("Fatal: {expected}"));
    assert_eq!(
        storage.clear_mutation_failure(),
        0,
        "the boundary wrote before it failed"
    );
    Ok(())
}

fn boundary(
    set: &'static [Address],
    freeze: u64,
) -> impl FnOnce(&mut ValidatorSet<'_>) -> Result<()> {
    move |vs| vs.test_activate_validated_boundary_set(set, HASH, freeze)
}

#[test]
fn included_joiner_without_ocomp_admission_is_rejected() -> Result<()> {
    assert_rejected_before_writes(
        |vs| {
            joining(vs, A)?;
            vs.val_ocomp_registration.get_bytes(&A).clear()
        },
        boundary(&[A], 10),
        format!("certified active set contains validator {A} without OCOMP admission"),
    )
}

#[test]
fn omitted_active_validator_is_rejected() -> Result<()> {
    assert_rejected_before_writes(
        |vs| active(vs, A),
        boundary(&[], 10),
        format!("validated boundary omitted active validator {A}"),
    )
}

#[test]
fn included_exit_at_or_before_freeze_is_rejected() -> Result<()> {
    assert_rejected_before_writes(
        |vs| exiting(vs, A),
        boundary(&[A], 11),
        format!("validated boundary retained validator {A} that exited at 11 before freeze 11"),
    )
}

#[test]
fn omitted_exit_after_freeze_is_rejected() -> Result<()> {
    assert_rejected_before_writes(
        |vs| exiting(vs, A),
        boundary(&[], 10),
        format!("validated boundary omitted validator {A} that exited at 11 after freeze 10"),
    )
}

#[test]
fn included_jail_at_or_before_freeze_is_rejected() -> Result<()> {
    assert_rejected_before_writes(
        |vs| jail_retained(vs, A),
        boundary(&[A], 11),
        format!("validated boundary retained validator {A} jailed at 11 before freeze 11"),
    )
}

#[test]
fn omitted_jail_after_freeze_is_rejected() -> Result<()> {
    assert_rejected_before_writes(
        |vs| jail_retained(vs, A),
        boundary(&[], 10),
        format!("validated boundary omitted validator {A} jailed at 11 after freeze 10"),
    )
}

fn assert_included_ineligible(
    setup: impl FnOnce(&mut ValidatorSet<'_>) -> Result<()>,
    stored_status: u8,
) -> Result<()> {
    assert_rejected_before_writes(
        setup,
        boundary(&[A], 11),
        format!("validated boundary included ineligible validator {A} with status {stored_status}"),
    )
}

#[test]
fn included_demoted_joiner_at_or_before_freeze_is_rejected_with_its_status() -> Result<()> {
    assert_included_ineligible(|vs| waiting_for_stake(vs, A), status::REGISTERED)?;
    assert_included_ineligible(|vs| waiting_for_readiness(vs, A), status::PENDING)
}

#[test]
fn included_demoted_joiner_without_height_at_the_last_freeze_is_rejected() -> Result<()> {
    assert_rejected_before_writes(
        |vs| {
            waiting_for_stake(vs, A)?;
            vs.val_deactivated_at_height.write(&A, 0)
        },
        |vs| vs.activate_reshared_set(&[A], HASH),
        "validator deactivation height must be non-zero".to_string(),
    )
}

#[test]
fn included_ineligible_validator_is_rejected_with_its_status() -> Result<()> {
    assert_included_ineligible(
        |vs| {
            exiting(vs, A)?;
            vs.test_activate_validated_boundary_set(&[], HASH, 11)
        },
        status::UNBONDING,
    )?;
    assert_included_ineligible(
        |vs| {
            jail_retained(vs, A)?;
            vs.test_activate_validated_boundary_set(&[], HASH, 11)
        },
        status::JAILED,
    )?;
    assert_included_ineligible(
        |vs| {
            register(vs, A)?;
            make_inactive_for_test(vs, A);
            Ok(())
        },
        status::INACTIVE,
    )
}

#[test]
fn tee_expiry_of_an_ineligible_validator_is_rejected_with_its_status() -> Result<()> {
    assert_rejected_before_writes(
        |vs| exiting(vs, A),
        |vs| vs.test_activate_validated_boundary_set_with_expiry_exclusions(&[], HASH, 11, &[A]),
        format!(
            "TEE expiry exclusion contains validator {A} with ineligible status {}",
            status::EXITING
        ),
    )
}

#[test]
fn participant_membership_mismatch_is_rejected_after_planning() -> Result<()> {
    let mismatch = |planned: &[Address], artifact: &[Address]| {
        format!(
            "validated boundary participant membership mismatch: planned {planned:?}, artifact {artifact:?}"
        )
    };
    assert_rejected_before_writes(
        |vs| active(vs, A),
        boundary(&[A, A], 10),
        mismatch(&[A], &[A, A]),
    )?;
    assert_rejected_before_writes(
        |vs| active(vs, A),
        boundary(&[A, UNREGISTERED], 10),
        mismatch(&[A], &[A, UNREGISTERED]),
    )
}

#[test]
fn first_rejected_validator_in_registry_order_wins() -> Result<()> {
    assert_rejected_before_writes(
        |vs| {
            active(vs, A)?;
            exiting(vs, B)
        },
        boundary(&[B], 11),
        format!("validated boundary omitted active validator {A}"),
    )?;
    assert_rejected_before_writes(
        |vs| {
            exiting(vs, B)?;
            active(vs, A)
        },
        boundary(&[B], 11),
        format!("validated boundary retained validator {B} that exited at 11 before freeze 11"),
    )
}

#[test]
fn lifecycle_rejection_precedes_membership_mismatch() -> Result<()> {
    assert_rejected_before_writes(
        |vs| active(vs, A),
        boundary(&[UNREGISTERED], 10),
        format!("validated boundary omitted active validator {A}"),
    )
}

/// The persisted columns of one validator that a boundary can change, plus
/// the P2P columns that it must not change.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BoundaryColumns {
    stake: U256,
    status: u8,
    history: [u64; 6],
    unbonding_end: u64,
    has_bls_share: bool,
    join_confirmed: bool,
    jailed_at: u64,
    p2p: (u8, Vec<u8>),
}

impl BoundaryColumns {
    fn read(vs: &ValidatorSet<'_>, validator: Address) -> Result<Self> {
        Ok(Self {
            stake: vs.val_stake.read(&validator)?,
            status: vs.val_status.read(&validator)?,
            history: [
                vs.val_slash_count.read(&validator)?,
                vs.val_missed_blocks.read(&validator)?,
                vs.val_missed_votes.read(&validator)?,
                vs.val_blocks_proposed.read(&validator)?,
                vs.val_joined_at_height.read(&validator)?,
                vs.val_deactivated_at_height.read(&validator)?,
            ],
            unbonding_end: vs.val_unbonding_end.read(&validator)?,
            has_bls_share: vs.val_has_bls_share.read(&validator)?,
            join_confirmed: vs.val_join_confirmed.read(&validator)?,
            jailed_at: vs.val_jailed_at_height.read(&validator)?,
            p2p: (
                vs.val_p2p_address_version.read(&validator)?,
                vs.val_p2p_address_payload.get_bytes(&validator).read()?,
            ),
        })
    }

    /// The number of single-column writes that turn `self` into `after`.
    fn changed_columns(&self, after: &Self) -> usize {
        let scalar = [
            self.stake != after.stake,
            self.status != after.status,
            self.unbonding_end != after.unbonding_end,
            self.has_bls_share != after.has_bls_share,
            self.join_confirmed != after.join_confirmed,
            self.jailed_at != after.jailed_at,
        ];
        let history = self
            .history
            .iter()
            .zip(after.history)
            .filter(|(before, after)| **before != *after)
            .count();
        scalar.into_iter().filter(|changed| *changed).count() + history
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BoundaryView {
    validators: Vec<(Address, ValidatorLifecycle, BoundaryColumns)>,
    active_set_hash: B256,
    pending_set_change: bool,
}

const MIXED: [Address; 7] = [A, B, C, D, E, F, G];

/// The boundary view of every MIXED validator, or the text of the first read
/// error.
fn boundary_view(storage: &mut HashMapStorageProvider) -> ViewResult {
    storage
        .enter(|storage| -> Result<BoundaryView> {
            let vs = ValidatorSet::new(storage);
            let mut validators = Vec::with_capacity(MIXED.len());
            for validator in MIXED {
                validators.push((
                    validator,
                    vs.validator_lifecycle(validator)?,
                    BoundaryColumns::read(&vs, validator)?,
                ));
            }
            Ok(BoundaryView {
                validators,
                active_set_hash: vs.active_consensus_set_hash()?,
                pending_set_change: vs.has_pending_set_change()?,
            })
        })
        .map_err(|error| error.to_string())
}

/// Registry order A..G. A stays active, B activates, C exits to unbonding, D
/// moves to boundary jail, E is a demoted joiner retained as exiting, F is an
/// active validator and G a joiner, both with a certified TEE expiry.
fn mixed_boundary_storage() -> (HashMapStorageProvider, Result<()>) {
    let (mut storage, setup) = boundary_storage(|vs| {
        active(vs, A)?;
        joining(vs, B)?;
        exiting(vs, C)?;
        jail_retained(vs, D)?;
        joining(vs, E)?;
        active(vs, F)?;
        joining(vs, G)
    });
    // The demotion of E lands after the height-11 freeze.
    storage.set_block_number(12);
    let demotion = storage.enter(|storage| {
        ValidatorSet::new(storage).record_unstake(E, U256::from(1u64), U256::from(MINIMUM), 0)
    });
    (storage, setup.and(demotion))
}

type ViewResult = std::result::Result<BoundaryView, String>;

/// The mutation-prefix views of the mixed boundary. Each fixture rebuild must
/// succeed.
fn mixed_boundary_views() -> Result<MutationPrefixViews<ViewResult>> {
    let seed_errors = RefCell::new(Vec::new());
    let views = HashMapStorageProvider::mutation_prefix_views(
        || {
            let (storage, setup) = mixed_boundary_storage();
            if let Err(error) = setup {
                seed_errors.borrow_mut().push(error.to_string());
            }
            storage
        },
        |storage| {
            ValidatorSet::new(storage).test_activate_validated_boundary_set_with_expiry_exclusions(
                &[A, B, E],
                HASH,
                11,
                &[F, G],
            )
        },
        boundary_view,
    )?;
    assert_eq!(seed_errors.into_inner(), Vec::<String>::new());
    Ok(views)
}

/// A and B are active, C unbonding, D boundary-jailed, E exiting at its
/// height-12 demotion, and F and G wait for readiness again.
fn assert_mixed_outcome(complete: &BoundaryView) {
    let lifecycles: Vec<_> = complete
        .validators
        .iter()
        .map(|(_, lifecycle, _)| lifecycle)
        .collect();
    assert!(matches!(lifecycles[0], ValidatorLifecycle::Active(_)));
    assert!(matches!(lifecycles[1], ValidatorLifecycle::Active(_)));
    assert!(matches!(lifecycles[2], ValidatorLifecycle::Unbonding(_)));
    assert!(matches!(lifecycles[3], ValidatorLifecycle::Jail(_)));
    assert!(matches!(lifecycles[4], ValidatorLifecycle::Exiting(_)));
    assert!(matches!(
        lifecycles[5],
        ValidatorLifecycle::WaitingForReadiness(_)
    ));
    assert!(matches!(
        lifecycles[6],
        ValidatorLifecycle::WaitingForReadiness(_)
    ));
    assert_eq!(complete.validators[4].2.history[5], 12);
    assert_eq!(complete.active_set_hash, HASH);
    assert!(complete.pending_set_change);
}

#[test]
fn accepted_boundary_writes_exactly_the_changed_columns_then_hash_flag_and_event_atomically(
) -> Result<()> {
    let (mut seeded, setup) = mixed_boundary_storage();
    setup?;
    let initial = boundary_view(&mut seeded);
    let views = mixed_boundary_views()?;
    let (Ok(initial), Ok(complete)) = (&initial, &views.complete) else {
        return Err(PrecompileError::Fatal(format!(
            "boundary view failed: {initial:?} / {:?}",
            views.complete
        )));
    };
    assert_mixed_outcome(complete);

    let mut changed = 0;
    for ((_, _, before), (_, _, after)) in initial.validators.iter().zip(&complete.validators) {
        assert_eq!(
            before.p2p, after.p2p,
            "a boundary must not change P2P columns"
        );
        changed += before.changed_columns(after);
    }
    // Hash, pending flag and ConsensusSetUpdated follow the validator columns.
    assert_eq!(
        views,
        MutationPrefixViews {
            before_mutation: vec![Ok(initial.clone()); changed + 3],
            complete: Ok(complete.clone()),
            mutations: changed + 3,
        }
    );
    Ok(())
}
