//! The raw-storage validator aggregate: hydrate consistency, the persistence
//! identity guard and write order, and the stake transitions that persist
//! through it.

use super::*;
use crate::state_machine::{HistoryCounters, ValidatorHistory};
use outbe_primitives::storage::hashmap::MutationPrefixViews;

const VALIDATOR: Address = address!("0x00000000000000000000000000000000000000E1");
const OTHER: Address = address!("0x00000000000000000000000000000000000000E2");

/// The variant and message of an error result.
fn error_text<T: std::fmt::Debug>(result: Result<T, PrecompileError>) -> String {
    match result.unwrap_err() {
        PrecompileError::Fatal(message) => format!("Fatal: {message}"),
        PrecompileError::Revert(message) => format!("Revert: {message}"),
        other => format!("{other:?}"),
    }
}

/// Configured storage with VALIDATOR (index 1) and OTHER (index 2) registered.
fn two_registered() -> HashMapStorageProvider {
    let mut storage = configured_storage(10);
    storage.enter(|storage| {
        let mut vs = ValidatorSet::new(storage);
        vs.register_validator(OWNER, VALIDATOR, &dummy_consensus_pubkey(0xE1))
            .unwrap();
        vs.register_validator(OWNER, OTHER, &dummy_consensus_pubkey(0xE2))
            .unwrap();
    });
    storage
}

/// Configured storage with VALIDATOR ACTIVE and `bonded` mirrored.
fn active_with_bonded(bonded: u64) -> HashMapStorageProvider {
    let mut storage = configured_storage(10);
    storage.enter(|storage| {
        ValidatorSet::new(storage)
            .test_register_active_validator(
                VALIDATOR,
                &dummy_consensus_pubkey(0xE1),
                U256::from(bonded),
            )
            .unwrap();
    });
    storage
}

#[test]
fn hydrate_rejects_inconsistent_registry_bindings_in_order() {
    let pubkey_hash = ValidatorSet::consensus_pubkey_hash(&dummy_consensus_pubkey(0xE1));
    let count = format!("Fatal: validator {VALIDATOR} registry index 1 exceeds validator_count 0");
    let forward = format!(
        "Fatal: validator registry forward index mismatch at 1: expected {VALIDATOR}, got {OTHER}"
    );
    let reverse = format!(
        "Fatal: validator consensus pubkey reverse mapping mismatch for {VALIDATOR}: got {OTHER}"
    );
    // Each case corrupts its binding and every binding checked after it.
    let cases: [(&str, [bool; 3], String); 3] = [
        ("count", [true, true, true], count),
        ("forward index", [false, true, true], forward),
        ("reverse key", [false, false, true], reverse),
    ];
    for (name, [corrupt_count, corrupt_forward, corrupt_reverse], expected) in cases {
        let mut storage = two_registered();
        let error = storage.enter(|storage| {
            let vs = ValidatorSet::new(storage);
            if corrupt_count {
                vs.validator_count.write(0).unwrap();
            }
            if corrupt_forward {
                vs.index_to_address.write(&1, OTHER).unwrap();
            }
            if corrupt_reverse {
                vs.consensus_pubkey_hash_to_address
                    .write(&pubkey_hash, OTHER)
                    .unwrap();
            }
            error_text(vs.validator_state(VALIDATOR))
        });
        assert_eq!(error, expected, "{name}");
    }
}

#[test]
fn persistence_rejects_a_registry_identity_change_before_any_write() {
    let mut storage = two_registered();
    storage.clear_mutation_failure();
    let error = storage.enter(|storage| {
        let mut vs = ValidatorSet::new(storage);
        let before = vs.validator_state(VALIDATOR).unwrap();
        let after = vs.validator_state(OTHER).unwrap();
        error_text(vs.persist_validator_state_delta(&before, &after))
    });
    assert_eq!(
        error,
        "Fatal: validator lifecycle transition attempted to change registry identity"
    );
    assert_eq!(storage.clear_mutation_failure(), 0);
}

/// The mirrored stake, status, deactivation height, unbonding hint and
/// pending-set-change flag of VALIDATOR.
#[derive(Debug, Clone, PartialEq)]
struct ExitView {
    bonded: U256,
    status: u8,
    deactivated_at: u64,
    unbonding_end: u64,
    pending: bool,
}

fn exit_view(provider: &mut HashMapStorageProvider) -> ExitView {
    provider.enter(|storage| {
        let vs = ValidatorSet::new(storage);
        ExitView {
            bonded: vs.val_stake.read(&VALIDATOR).unwrap(),
            status: vs.val_status.read(&VALIDATOR).unwrap(),
            deactivated_at: vs.val_deactivated_at_height.read(&VALIDATOR).unwrap(),
            unbonding_end: vs.val_unbonding_end.read(&VALIDATOR).unwrap(),
            pending: vs.pending_set_change.read().unwrap(),
        }
    })
}

fn view(bonded: u64, status: u8, deactivated_at: u64, pending: bool) -> ExitView {
    ExitView {
        bonded: U256::from(bonded),
        status,
        deactivated_at,
        unbonding_end: 0,
        pending,
    }
}

/// The stake slash commits its four writes in one checkpoint. A failure at
/// any write leaves the stored validator unchanged.
#[test]
fn stake_slash_below_minimum_commits_exit_fields_and_set_change_atomically() {
    let views = HashMapStorageProvider::mutation_prefix_views(
        || active_with_bonded(1_000),
        |storage| {
            ValidatorSet::new(storage).record_stake_slash(
                VALIDATOR,
                U256::from(100),
                U256::from(1_000),
                None,
            )
        },
        exit_view,
    )
    .unwrap();
    let unchanged = view(1_000, status::ACTIVE, 0, false);
    assert_eq!(
        views,
        MutationPrefixViews {
            before_mutation: vec![
                unchanged.clone(),
                unchanged.clone(),
                unchanged.clone(),
                unchanged,
            ],
            complete: view(100, status::EXITING, 1, true),
            mutations: 4,
        }
    );
}

/// Persistence of an ACTIVE -> EXITING delta writes the stake, then the
/// status, then the deactivation height. It does not touch the set-change flag.
#[test]
fn exit_persistence_writes_stake_then_status_then_deactivation_height() {
    let views = HashMapStorageProvider::mutation_prefix_views(
        || active_with_bonded(1_000),
        |storage| {
            let mut vs = ValidatorSet::new(storage);
            let before = vs.validator_state(VALIDATOR)?;
            let ValidatorLifecycle::Active(active) = before.lifecycle().clone() else {
                panic!("fixture validator is not ACTIVE");
            };
            let exiting = crate::state_machine::begin_exit(
                active,
                StakeProjection::new(U256::from(100), None),
                1,
            )?;
            let after = before
                .clone()
                .with_lifecycle(ValidatorLifecycle::Exiting(exiting))?;
            vs.persist_validator_state_delta(&before, &after)
        },
        exit_view,
    )
    .unwrap();
    assert_eq!(
        views,
        MutationPrefixViews {
            before_mutation: vec![
                view(1_000, status::ACTIVE, 0, false),
                view(100, status::ACTIVE, 0, false),
                view(100, status::EXITING, 0, false),
            ],
            complete: view(100, status::EXITING, 1, false),
            mutations: 3,
        }
    );
}

/// An unstakeable lifecycle case: its name, its fixture setup and the exact
/// rejection.
type UnstakeableCase = (&'static str, fn(&mut ValidatorSet), &'static str);

#[test]
fn stake_increase_rejects_unstakeable_lifecycles_without_writes() {
    let exiting = |vs: &mut ValidatorSet| {
        vs.register_validator(OWNER, VALIDATOR, &dummy_consensus_pubkey(0xE1))
            .unwrap();
        activate_for_test(vs, VALIDATOR);
        vs.deactivate_validator(OWNER, VALIDATOR).unwrap();
    };
    let cases: [UnstakeableCase; 4] = [
        (
            "absent",
            |_| {},
            "Revert: cannot stake before validator registration",
        ),
        (
            "exiting",
            exiting,
            "Revert: cannot increase stake while validator is exiting or unbonding",
        ),
        (
            "unbonding",
            |vs| {
                vs.register_validator(OWNER, VALIDATOR, &dummy_consensus_pubkey(0xE1))
                    .unwrap();
                activate_for_test(vs, VALIDATOR);
                vs.deactivate_validator(OWNER, VALIDATOR).unwrap();
                vs.activate_reshared_set(&[], B256::ZERO).unwrap();
            },
            "Revert: cannot increase stake while validator is exiting or unbonding",
        ),
        (
            "inactive",
            |vs| {
                vs.register_validator(OWNER, VALIDATOR, &dummy_consensus_pubkey(0xE1))
                    .unwrap();
                make_inactive_for_test(vs, VALIDATOR);
            },
            "Revert: inactive validator must re-register before staking",
        ),
    ];
    for (name, prepare, expected) in cases {
        let mut storage = configured_storage(10);
        storage.enter(|storage| prepare(&mut ValidatorSet::new(storage)));
        storage.clear_mutation_failure();
        let error = storage.enter(|storage| {
            error_text(ValidatorSet::new(storage).record_stake_increase(
                VALIDATOR,
                U256::from(5_000),
                U256::from(1_000),
            ))
        });
        assert_eq!(error, expected, "{name}");
        assert_eq!(storage.clear_mutation_failure(), 0, "{name}");
    }
}

#[test]
fn complete_unbonding_finishes_only_unbonding_validators() {
    let mut storage = active_with_bonded(1_000);
    storage.clear_mutation_failure();
    storage.enter(|storage| {
        ValidatorSet::new(storage)
            .complete_unbonding(VALIDATOR)
            .unwrap()
    });
    assert_eq!(storage.clear_mutation_failure(), 0, "ACTIVE is a no-op");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, VALIDATOR, &dummy_consensus_pubkey(0xE1))
            .unwrap();
        activate_for_test(vs, VALIDATOR);
        vs.deactivate_validator(OWNER, VALIDATOR).unwrap();
        vs.activate_reshared_set(&[], B256::ZERO).unwrap();
        assert!(matches!(
            vs.validator_lifecycle(VALIDATOR).unwrap(),
            ValidatorLifecycle::Unbonding(_)
        ));
        vs.complete_unbonding(VALIDATOR).unwrap();
        let state = vs.validator_state(VALIDATOR).unwrap();
        assert!(matches!(state.lifecycle(), ValidatorLifecycle::Inactive(_)));
        assert_eq!(state.bonded_stake(), U256::ZERO);
        assert_eq!(state.unbonding_end_hint(), None);
    });
}

#[test]
fn ocomp_bonded_slash_checks_the_window_then_the_lifecycle_then_mirrors_stake() {
    let open_window = |vs: &ValidatorSet| {
        vs.val_ocomp_recovery_deadline
            .write(&VALIDATOR, 500)
            .unwrap();
    };
    let slash = |storage: &mut HashMapStorageProvider| {
        storage.enter(|storage| {
            ValidatorSet::new(storage).record_ocomp_bonded_slash(VALIDATOR, U256::from(900))
        })
    };

    let mut closed = active_with_bonded(1_000);
    assert_eq!(
        error_text(slash(&mut closed)),
        "Revert: OCOMP bonded slash requires an open recovery window"
    );

    let mut registered = two_registered();
    registered.enter(|storage| open_window(&ValidatorSet::new(storage)));
    assert_eq!(
        error_text(slash(&mut registered)),
        "Revert: OCOMP bonded slash requires an active validator"
    );

    let mut active = active_with_bonded(1_000);
    active.enter(|storage| open_window(&ValidatorSet::new(storage)));
    active.clear_mutation_failure();
    slash(&mut active).unwrap();
    assert_eq!(active.clear_mutation_failure(), 1, "only the stake mirror");
    active.enter(|storage| {
        let vs = ValidatorSet::new(storage);
        assert_eq!(vs.val_stake.read(&VALIDATOR).unwrap(), U256::from(900));
        assert!(matches!(
            vs.validator_lifecycle(VALIDATOR).unwrap(),
            ValidatorLifecycle::Active(_)
        ));
    });
}

/// The five history counters of VALIDATOR, in persistence order.
fn history_columns(provider: &mut HashMapStorageProvider) -> [u64; 5] {
    provider.enter(|storage| {
        let vs = ValidatorSet::new(storage);
        [
            vs.val_slash_count.read(&VALIDATOR).unwrap(),
            vs.val_missed_blocks.read(&VALIDATOR).unwrap(),
            vs.val_missed_votes.read(&VALIDATOR).unwrap(),
            vs.val_blocks_proposed.read(&VALIDATOR).unwrap(),
            vs.val_joined_at_height.read(&VALIDATOR).unwrap(),
        ]
    })
}

#[test]
fn history_persistence_writes_each_changed_counter_in_order() {
    let mut seeded = active_with_bonded(1_000);
    let before = history_columns(&mut seeded);
    let views = HashMapStorageProvider::mutation_prefix_views(
        || active_with_bonded(1_000),
        |storage| {
            ValidatorSet::new(storage).test_set_history(
                VALIDATOR,
                ValidatorHistory::new(
                    40,
                    None,
                    HistoryCounters {
                        slash_count: 7,
                        missed_blocks: 8,
                        missed_votes: 9,
                        blocks_proposed: 10,
                    },
                ),
            )
        },
        history_columns,
    )
    .unwrap();
    let after = [7, 8, 9, 10, 40];
    let prefix = |written: usize| {
        let mut columns = before;
        columns[..written].copy_from_slice(&after[..written]);
        columns
    };
    assert_eq!(
        views,
        MutationPrefixViews {
            before_mutation: (0..5).map(prefix).collect(),
            complete: after,
            mutations: 5,
        }
    );
}

fn p2p_columns(provider: &mut HashMapStorageProvider) -> (u8, Vec<u8>) {
    provider.enter(|storage| {
        let vs = ValidatorSet::new(storage);
        (
            vs.val_p2p_address_version.read(&VALIDATOR).unwrap(),
            vs.val_p2p_address_payload
                .get_bytes(&VALIDATOR)
                .read()
                .unwrap(),
        )
    })
}

fn local_p2p_address() -> P2pAddress {
    P2pAddress::Symmetric(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30_400))
}

/// The P2P setter commits its two writes in one checkpoint. A failure at any
/// write leaves both columns unchanged.
#[test]
fn p2p_address_update_commits_version_and_payload_atomically() {
    let encoded = encode_v1(&local_p2p_address());
    let views = HashMapStorageProvider::mutation_prefix_views(
        || active_with_bonded(1_000),
        |storage| {
            ValidatorSet::new(storage).set_p2p_address(
                OWNER,
                VALIDATOR,
                P2P_ADDRESS_VERSION_V1,
                &encoded,
            )
        },
        p2p_columns,
    )
    .unwrap();
    assert_eq!(
        views,
        MutationPrefixViews {
            before_mutation: vec![(0, Vec::new()), (0, Vec::new())],
            complete: (P2P_ADDRESS_VERSION_V1, encoded.clone()),
            mutations: 2,
        }
    );
}

/// Persistence of a P2P delta writes the version, then the payload.
#[test]
fn p2p_persistence_writes_the_version_then_the_payload() {
    let encoded = encode_v1(&local_p2p_address());
    let views = HashMapStorageProvider::mutation_prefix_views(
        || active_with_bonded(1_000),
        |storage| {
            let mut vs = ValidatorSet::new(storage);
            let before = vs.validator_state(VALIDATOR)?;
            let lifecycle = crate::state_machine::with_p2p(
                before.lifecycle().clone(),
                crate::state_machine::P2pInfo::V1(local_p2p_address()),
            )?;
            let after = before.clone().with_lifecycle(lifecycle)?;
            vs.persist_validator_state_delta(&before, &after)
        },
        p2p_columns,
    )
    .unwrap();
    assert_eq!(
        views,
        MutationPrefixViews {
            before_mutation: vec![(0, Vec::new()), (P2P_ADDRESS_VERSION_V1, Vec::new())],
            complete: (P2P_ADDRESS_VERSION_V1, encoded.clone()),
            mutations: 2,
        }
    );
}
