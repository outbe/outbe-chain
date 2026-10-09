use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use alloy_primitives::{Address, U256};
use outbe_primitives::consensus_p2p::{encode_v1, P2pAddress, P2P_ADDRESS_VERSION_V1};

use crate::runtime::status::{ACTIVE, EXITING, INACTIVE, JAILED, PENDING, REGISTERED, UNBONDING};

use super::*;

const ADDRESS: Address = Address::repeat_byte(0x11);
const KEY: ConsensusPubkey = [7; 48];

fn history() -> ValidatorHistory {
    ValidatorHistory::new(
        13,
        Some(21),
        HistoryCounters {
            slash_count: 2,
            missed_blocks: 3,
            missed_votes: 5,
            blocks_proposed: 8,
        },
    )
}

fn canonical_history(status: u8, jailed_at: u64) -> ValidatorHistory {
    let deactivated_at = match status {
        ACTIVE => None,
        JAILED => Some(jailed_at),
        EXITING | UNBONDING | INACTIVE => Some(21),
        _ => Some(21),
    };
    ValidatorHistory::new(
        13,
        deactivated_at,
        HistoryCounters {
            slash_count: 2,
            missed_blocks: 3,
            missed_votes: 5,
            blocks_proposed: 8,
        },
    )
}

fn canonical(status: u8, share: bool, confirmed: bool, jailed_at: u64) -> ValidatorState {
    let stake = if status == INACTIVE {
        StakeProjection::zero()
    } else {
        StakeProjection::new(U256::from(1_500), Some(34))
    };
    RawColumns {
        stake,
        ..RawColumns::registered(status, share, confirmed, jailed_at)
    }
    .decode()
    .unwrap()
}

#[test]
fn raw_statuses_decode_to_every_canonical_state() {
    let cases = [
        (REGISTERED, false, false, 0, "waiting-for-stake"),
        (PENDING, false, false, 0, "waiting-for-readiness"),
        (PENDING, false, true, 0, "joining"),
        (ACTIVE, true, false, 0, "active"),
        (EXITING, true, false, 0, "exiting"),
        (UNBONDING, false, false, 0, "unbonding"),
        (INACTIVE, false, false, 0, "inactive"),
        (JAILED, true, false, 55, "jail-retained"),
        (JAILED, false, false, 55, "jail"),
    ];

    for (status, share, confirmed, jailed_at, expected) in cases {
        let state = canonical(status, share, confirmed, jailed_at);
        let actual = match state.lifecycle() {
            ValidatorLifecycle::Absent => "absent",
            ValidatorLifecycle::WaitingForStake(_) => "waiting-for-stake",
            ValidatorLifecycle::WaitingForReadiness(_) => "waiting-for-readiness",
            ValidatorLifecycle::Joining(_) => "joining",
            ValidatorLifecycle::Active(_) => "active",
            ValidatorLifecycle::JailRetained(_) => "jail-retained",
            ValidatorLifecycle::Jail(_) => "jail",
            ValidatorLifecycle::Exiting(_) => "exiting",
            ValidatorLifecycle::Unbonding(_) => "unbonding",
            ValidatorLifecycle::Inactive(_) => "inactive",
        };
        assert_eq!(actual, expected);
        assert_eq!(state.stored_status(), Some(status));
        assert_eq!(state.has_bls_share(), share);
        assert_eq!(state.join_confirmed(), confirmed);
        assert_eq!(state.stored_jailed_at(), jailed_at);
    }
}

#[test]
fn unknown_statuses_fail_closed() {
    for status in 7..=u8::MAX {
        let raw = RawColumns {
            stake: StakeProjection::zero(),
            ..RawColumns::registered(status, false, false, 0)
        };
        assert!(raw.decode().is_err());
    }
}

#[test]
fn every_noncanonical_coupled_field_combination_fails_closed() {
    let invalid = [
        (REGISTERED, true, false, 0),
        (REGISTERED, false, true, 0),
        (PENDING, true, false, 0),
        (PENDING, true, true, 0),
        (ACTIVE, false, false, 0),
        (ACTIVE, true, true, 0),
        (EXITING, false, false, 0),
        (EXITING, true, true, 0),
        (UNBONDING, true, false, 0),
        (UNBONDING, false, true, 0),
        (INACTIVE, true, false, 0),
        (INACTIVE, false, true, 0),
        (JAILED, true, true, 10),
        (JAILED, false, true, 10),
        (ACTIVE, true, false, 10),
    ];

    for (status, share, confirmed, jailed_at) in invalid {
        let stake = if status == INACTIVE {
            StakeProjection::zero()
        } else {
            StakeProjection::new(U256::from(1_500), None)
        };
        let raw = RawColumns {
            stake,
            ..RawColumns::registered(status, share, confirmed, jailed_at)
        };
        assert!(raw.decode().is_err());
    }
}

#[test]
fn absence_requires_all_validator_owned_fields_to_be_empty() {
    let absent = ValidatorState::decode_stored(
        ADDRESS,
        StoredValidatorFields {
            registry_index: 0,
            consensus_pubkey: [0; 48],
            stake: StakeProjection::zero(),
            stored_status: REGISTERED,
            p2p_version: 0,
            p2p_payload: Vec::new(),
            history: ValidatorHistory::fresh(0),
            has_bls_share: false,
            join_confirmed: false,
            jailed_at: 0,
        },
    )
    .unwrap();
    assert_eq!(absent.lifecycle(), &ValidatorLifecycle::Absent);
    assert!(!absent.is_registered());
    assert_eq!(absent.registry_index(), None);
    assert_eq!(absent.consensus_pubkey(), None);
    assert_eq!(absent.bonded_stake(), U256::ZERO);

    let pre_registration_stake = ValidatorState::decode_stored(
        ADDRESS,
        StoredValidatorFields {
            registry_index: 0,
            consensus_pubkey: [0; 48],
            stake: StakeProjection::new(U256::from(1), None),
            stored_status: REGISTERED,
            p2p_version: 0,
            p2p_payload: Vec::new(),
            history: ValidatorHistory::fresh(0),
            has_bls_share: false,
            join_confirmed: false,
            jailed_at: 0,
        },
    );
    assert!(pre_registration_stake.is_err());
}

#[test]
fn registered_identity_requires_a_nonzero_consensus_key() {
    let state = ValidatorState::decode_stored(
        ADDRESS,
        StoredValidatorFields {
            registry_index: 1,
            consensus_pubkey: [0; 48],
            stake: StakeProjection::zero(),
            stored_status: REGISTERED,
            p2p_version: 0,
            p2p_payload: Vec::new(),
            history: history(),
            has_bls_share: false,
            join_confirmed: false,
            jailed_at: 0,
        },
    );
    assert!(state.is_err());
}

#[test]
fn p2p_version_and_payload_decode_atomically() {
    let address = P2pAddress::Symmetric(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30_400));
    let encoded = encode_v1(&address);
    let state = ValidatorState::decode_stored(
        ADDRESS,
        StoredValidatorFields {
            registry_index: 1,
            consensus_pubkey: KEY,
            stake: StakeProjection::zero(),
            stored_status: REGISTERED,
            p2p_version: P2P_ADDRESS_VERSION_V1,
            p2p_payload: encoded.clone(),
            history: history(),
            has_bls_share: false,
            join_confirmed: false,
            jailed_at: 0,
        },
    )
    .unwrap();
    assert_eq!(state.p2p().and_then(P2pInfo::address), Some(&address));
    assert_eq!(
        state.p2p().unwrap().encode_stored(),
        (P2P_ADDRESS_VERSION_V1, encoded)
    );

    assert!(ValidatorState::decode_stored(
        ADDRESS,
        StoredValidatorFields {
            registry_index: 1,
            consensus_pubkey: KEY,
            stake: StakeProjection::zero(),
            stored_status: REGISTERED,
            p2p_version: P2P_ADDRESS_VERSION_V1,
            p2p_payload: Vec::new(),
            history: history(),
            has_bls_share: false,
            join_confirmed: false,
            jailed_at: 0
        }
    )
    .is_err());

    let opaque_payload = vec![0xAA, 0xBB];
    let opaque = ValidatorState::decode_stored(
        ADDRESS,
        StoredValidatorFields {
            registry_index: 1,
            consensus_pubkey: KEY,
            stake: StakeProjection::zero(),
            stored_status: REGISTERED,
            p2p_version: 99,
            p2p_payload: opaque_payload.clone(),
            history: history(),
            has_bls_share: false,
            join_confirmed: false,
            jailed_at: 0,
        },
    )
    .unwrap();
    assert_eq!(opaque.p2p().and_then(P2pInfo::address), None);
    assert_eq!(opaque.p2p().unwrap().encode_stored(), (99, opaque_payload));
}

#[test]
fn predicates_follow_effective_states_not_raw_status_groups() {
    let active = canonical(ACTIVE, true, false, 0);
    let exiting = canonical(EXITING, true, false, 0);
    let retained = canonical(JAILED, true, false, 9);
    let jail = canonical(JAILED, false, false, 9);
    let waiting = canonical(PENDING, false, false, 0);
    let joining = canonical(PENDING, false, true, 0);

    assert!(active.lifecycle().is_current_consensus_participant());
    assert!(exiting.lifecycle().is_current_consensus_participant());
    assert!(retained.lifecycle().is_current_consensus_participant());
    assert!(!jail.lifecycle().is_current_consensus_participant());
    assert!(!waiting.lifecycle().is_reshare_target());
    assert!(joining.lifecycle().is_reshare_target());
    assert!(active.lifecycle().is_reshare_target());
    assert!(jail.lifecycle().is_secondary_admission());
}

#[test]
fn join_path_consumes_each_source_state() {
    let absent = ValidatorState::absent(ADDRESS);
    let registered = register(absent, std::num::NonZeroU64::new(1).unwrap(), KEY, 10).unwrap();
    let ValidatorLifecycle::WaitingForStake(registered) = registered.into_lifecycle() else {
        panic!("expected WaitingForStake");
    };
    let pending = reach_minimum(
        registered,
        StakeProjection::new(U256::from(1_000), None),
        U256::from(1_000),
    )
    .unwrap();
    let joining = confirm_ready(pending);
    let active = activate_at_boundary(joining);

    assert_eq!(active.stake.bonded(), U256::from(1_000));
    assert_eq!(active.history.joined_at_height(), 10);
}

#[test]
fn pending_demotion_consumes_readiness() {
    let ValidatorLifecycle::Joining(joining) = canonical(PENDING, false, true, 0).into_lifecycle()
    else {
        panic!("expected Joining");
    };
    let registered = demote_joining(
        joining,
        StakeProjection::new(U256::from(900), Some(40)),
        U256::from(1_000),
        11,
    )
    .unwrap();
    let state =
        ValidatorState::from_lifecycle(ADDRESS, ValidatorLifecycle::WaitingForStake(registered))
            .unwrap();
    assert!(!state.join_confirmed());
    assert!(!state.has_bls_share());
    assert_eq!(
        state
            .history()
            .and_then(ValidatorHistory::last_deactivated_at_height),
        Some(11)
    );
}

#[test]
fn jail_has_distinct_retained_and_excluded_states() {
    let ValidatorLifecycle::Active(active) = canonical(ACTIVE, true, false, 0).into_lifecycle()
    else {
        panic!("expected Active");
    };
    let retained = jail(active, 77).unwrap();
    let retained_history = retained.history;
    let excluded = exclude_jailed_at_boundary(retained);

    assert_eq!(excluded.jailed_at, 77);
    assert_eq!(
        excluded.history.joined_at_height(),
        retained_history.joined_at_height()
    );
    assert_eq!(
        excluded.history.slash_count(),
        retained_history.slash_count()
    );
    assert_eq!(
        excluded.history.blocks_proposed(),
        retained_history.blocks_proposed()
    );
    assert_eq!(excluded.history.missed_blocks(), 0);
    assert_eq!(excluded.history.missed_votes(), 0);
    let state =
        ValidatorState::from_lifecycle(ADDRESS, ValidatorLifecycle::Jail(excluded)).unwrap();
    assert!(!state.has_bls_share());
    assert!(!state.lifecycle().is_current_consensus_participant());
}

#[test]
fn unjail_clears_missed_counters_and_checks_cooldown() {
    let ValidatorLifecycle::Jail(jail) = canonical(JAILED, false, false, 50).into_lifecycle()
    else {
        panic!("expected Jail");
    };
    let expected_history = jail.history;
    assert!(unjail(jail.clone(), 59, 10, U256::from(1_000)).is_err());

    let pending = unjail(jail, 60, 10, U256::from(1_000)).unwrap();
    assert_eq!(
        pending.history.joined_at_height(),
        expected_history.joined_at_height()
    );
    assert_eq!(
        pending.history.slash_count(),
        expected_history.slash_count()
    );
    assert_eq!(pending.history.blocks_proposed(), 8);
    assert_eq!(pending.history.missed_blocks(), 0);
    assert_eq!(pending.history.missed_votes(), 0);
}

#[test]
fn excluded_jail_full_exit_goes_directly_to_unbonding() {
    let ValidatorLifecycle::Jail(jail) = canonical(JAILED, false, false, 50).into_lifecycle()
    else {
        panic!("expected Jail");
    };
    assert!(
        full_exit_jailed(jail.clone(), StakeProjection::new(U256::from(1), Some(90)),).is_err()
    );

    let unbonding = full_exit_jailed(jail, StakeProjection::new(U256::ZERO, Some(90))).unwrap();
    assert_eq!(unbonding.stake.bonded(), U256::ZERO);
    assert_eq!(unbonding.stake.unbonding_end_hint(), Some(90));
}

#[test]
fn unbonding_completes_only_after_economic_projection_is_clear() {
    let ValidatorLifecycle::Unbonding(unbonding) =
        canonical(UNBONDING, false, false, 0).into_lifecycle()
    else {
        panic!("expected Unbonding");
    };
    assert!(complete_unbonding(unbonding.clone()).is_err());

    let ValidatorLifecycle::Unbonding(clear) = with_stake(
        ValidatorLifecycle::Unbonding(unbonding),
        StakeProjection::zero(),
    )
    .unwrap() else {
        panic!("expected Unbonding");
    };
    let inactive = complete_unbonding(clear).unwrap();
    let state =
        ValidatorState::from_lifecycle(ADDRESS, ValidatorLifecycle::Inactive(inactive)).unwrap();
    assert_eq!(state.bonded_stake(), U256::ZERO);
    assert_eq!(state.stake(), None);
}

#[test]
fn absent_and_inactive_reject_direct_stake_updates() {
    assert!(with_stake(
        ValidatorLifecycle::Absent,
        StakeProjection::new(U256::from(1), None)
    )
    .is_err());

    let inactive = canonical(INACTIVE, false, false, 0).into_lifecycle();
    assert!(with_stake(inactive, StakeProjection::new(U256::from(1), None)).is_err());
}

#[test]
fn inactive_reregistration_resets_lifecycle_information() {
    let ValidatorLifecycle::Inactive(inactive) =
        canonical(INACTIVE, false, false, 0).into_lifecycle()
    else {
        panic!("expected Inactive");
    };
    let index = inactive.registry_index;
    let registered = reregister(inactive, [9; 48], 100).unwrap();
    assert_eq!(registered.registry_index, index);
    assert_eq!(registered.consensus_pubkey, [9; 48]);
    assert_eq!(registered.p2p, P2pInfo::Unset);
    assert_eq!(registered.stake, StakeProjection::zero());
    assert_eq!(registered.history, ValidatorHistory::fresh(100));
}

#[test]
fn exit_transition_rejects_zero_height_and_excludes_only_at_boundary() {
    let ValidatorLifecycle::Active(active) = canonical(ACTIVE, true, false, 0).into_lifecycle()
    else {
        panic!("expected Active");
    };

    assert!(begin_exit(active.clone(), active.stake, 0).is_err());
    let exiting = begin_exit(active, StakeProjection::new(U256::from(900), Some(80)), 42).unwrap();
    let retained =
        ValidatorState::from_lifecycle(ADDRESS, ValidatorLifecycle::Exiting(exiting.clone()))
            .unwrap();
    assert!(retained.lifecycle().is_current_consensus_participant());
    assert!(retained.has_bls_share());

    let unbonding = exclude_exiting_at_boundary(exiting);
    let excluded =
        ValidatorState::from_lifecycle(ADDRESS, ValidatorLifecycle::Unbonding(unbonding)).unwrap();
    assert!(!excluded.lifecycle().is_current_consensus_participant());
    assert!(!excluded.has_bls_share());
    assert_eq!(excluded.unbonding_end_hint(), Some(80));
}

#[test]
fn jail_and_retained_jail_exit_reject_invalid_transition_inputs() {
    let ValidatorLifecycle::Active(active) = canonical(ACTIVE, true, false, 0).into_lifecycle()
    else {
        panic!("expected Active");
    };

    assert!(jail(active.clone(), 0).is_err());
    let retained = jail(active, 50).unwrap();
    assert!(full_exit_jailed_retained(
        retained.clone(),
        StakeProjection::new(U256::from(1), Some(90)),
    )
    .is_err());

    let exiting =
        full_exit_jailed_retained(retained, StakeProjection::new(U256::ZERO, Some(90))).unwrap();
    assert_eq!(exiting.history.last_deactivated_at_height(), Some(50));
    assert_eq!(exiting.stake.unbonding_end_hint(), Some(90));
}

#[test]
fn waiting_for_readiness_demotion_and_active_retention_are_explicit() {
    let ValidatorLifecycle::WaitingForReadiness(waiting) =
        canonical(PENDING, false, false, 0).into_lifecycle()
    else {
        panic!("expected WaitingForReadiness");
    };
    let registered = demote_waiting_for_readiness(
        waiting,
        StakeProjection::new(U256::from(999), Some(70)),
        U256::from(1_000),
    )
    .unwrap();
    assert_eq!(registered.stake.bonded(), U256::from(999));

    let ValidatorLifecycle::Active(active) = canonical(ACTIVE, true, false, 0).into_lifecycle()
    else {
        panic!("expected Active");
    };
    let expected = active.clone();
    assert_eq!(retain_active_at_boundary(active), expected);
}

#[test]
fn generic_internal_updates_reject_noncanonical_sentinels_and_preserve_metadata() {
    let active = canonical(ACTIVE, true, false, 0).into_lifecycle();
    assert!(with_stake(
        active.clone(),
        StakeProjection::new(U256::from(1_500), Some(0)),
    )
    .is_err());

    let p2p = P2pInfo::V1(P2pAddress::Symmetric(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        30_401,
    )));
    let updated = with_p2p(active, p2p.clone()).unwrap();
    assert_eq!(updated.p2p(), Some(&p2p));

    let invalid_history = ValidatorHistory::new(
        13,
        Some(0),
        HistoryCounters {
            slash_count: 2,
            missed_blocks: 3,
            missed_votes: 5,
            blocks_proposed: 8,
        },
    );
    assert!(with_history(updated, invalid_history).is_err());
}

#[test]
fn inactive_cleanup_consumes_the_terminal_payload() {
    let ValidatorLifecycle::Inactive(inactive) =
        canonical(INACTIVE, false, false, 0).into_lifecycle()
    else {
        panic!("expected Inactive");
    };
    assert_eq!(cleanup(inactive), ValidatorLifecycle::Absent);
}

// ---------------------------------------------------------------------------
// Exact errors and check precedence of the raw-column adapter and the generic
// lifecycle updates.
// ---------------------------------------------------------------------------

/// The raw stored columns of one validator, as the hydrate path reads them.
#[derive(Clone)]
struct RawColumns {
    registry_index: u64,
    consensus_pubkey: ConsensusPubkey,
    stake: StakeProjection,
    stored_status: u8,
    p2p_version: u8,
    p2p_payload: Vec<u8>,
    history: ValidatorHistory,
    has_bls_share: bool,
    join_confirmed: bool,
    jailed_at: u64,
}

impl RawColumns {
    /// The columns of an address that was never registered.
    fn absent() -> Self {
        Self {
            registry_index: 0,
            consensus_pubkey: [0; 48],
            stake: StakeProjection::zero(),
            stored_status: REGISTERED,
            p2p_version: 0,
            p2p_payload: Vec::new(),
            history: ValidatorHistory::fresh(0),
            has_bls_share: false,
            join_confirmed: false,
            jailed_at: 0,
        }
    }

    /// The columns of a canonical ACTIVE validator.
    fn active() -> Self {
        Self {
            registry_index: 3,
            consensus_pubkey: KEY,
            stake: StakeProjection::new(U256::from(1_500), None),
            stored_status: ACTIVE,
            history: ValidatorHistory::new(
                13,
                None,
                HistoryCounters {
                    slash_count: 2,
                    missed_blocks: 3,
                    missed_votes: 5,
                    blocks_proposed: 8,
                },
            ),
            has_bls_share: true,
            ..Self::absent()
        }
    }

    /// The columns of a canonical registered validator in `status`.
    fn registered(status: u8, share: bool, confirmed: bool, jailed_at: u64) -> Self {
        let stake = if status == INACTIVE {
            StakeProjection::zero()
        } else {
            StakeProjection::new(U256::from(1_500), Some(34))
        };
        Self {
            stake,
            stored_status: status,
            history: canonical_history(status, jailed_at),
            has_bls_share: share,
            join_confirmed: confirmed,
            jailed_at,
            ..Self::active()
        }
    }

    fn decode(&self) -> outbe_primitives::error::Result<ValidatorState> {
        ValidatorState::decode_stored(
            ADDRESS,
            StoredValidatorFields {
                registry_index: self.registry_index,
                consensus_pubkey: self.consensus_pubkey,
                stake: self.stake,
                stored_status: self.stored_status,
                p2p_version: self.p2p_version,
                p2p_payload: self.p2p_payload.clone(),
                history: self.history,
                has_bls_share: self.has_bls_share,
                join_confirmed: self.join_confirmed,
                jailed_at: self.jailed_at,
            },
        )
    }
}

/// The variant and message of an error result.
fn error_text<T: std::fmt::Debug>(result: outbe_primitives::error::Result<T>) -> String {
    match result.unwrap_err() {
        PrecompileError::Fatal(message) => format!("Fatal: {message}"),
        PrecompileError::Revert(message) => format!("Revert: {message}"),
        other => format!("{other:?}"),
    }
}

fn corrupt(detail: &str) -> String {
    format!("Fatal: corrupt validator state for {ADDRESS}: {detail}")
}

/// A retained column: its field name and the write that retains it in an
/// absent row.
type RetainedColumn = (&'static str, fn(&mut RawColumns));

#[test]
fn absent_columns_fail_on_each_retained_field_with_one_exact_error() {
    let retained: [RetainedColumn; 9] = [
        ("consensus key", |raw| raw.consensus_pubkey = KEY),
        ("bonded stake", |raw| {
            raw.stake = StakeProjection::new(U256::from(1), None)
        }),
        ("unbonding hint", |raw| {
            raw.stake = StakeProjection::new(U256::ZERO, Some(5))
        }),
        ("status", |raw| raw.stored_status = ACTIVE),
        ("p2p", |raw| {
            raw.p2p_version = 9;
            raw.p2p_payload = vec![1];
        }),
        ("history", |raw| raw.history = ValidatorHistory::fresh(1)),
        ("bls share", |raw| raw.has_bls_share = true),
        ("readiness", |raw| raw.join_confirmed = true),
        ("jail height", |raw| raw.jailed_at = 1),
    ];
    for (field, retain) in retained {
        let mut raw = RawColumns::absent();
        retain(&mut raw);
        assert_eq!(
            error_text(raw.decode()),
            corrupt("absent validator retains registry, stake, or lifecycle data"),
            "{field}"
        );
    }
}

#[test]
fn raw_column_checks_keep_their_precedence() {
    let cases: [(&str, RawColumns, String); 7] = [
        (
            "hint sentinel before absent residue",
            RawColumns {
                consensus_pubkey: KEY,
                stake: StakeProjection::new(U256::ZERO, Some(0)),
                ..RawColumns::absent()
            },
            corrupt("unbonding-end hint uses a non-canonical zero sentinel"),
        ),
        (
            "p2p framing before absent residue",
            RawColumns {
                consensus_pubkey: KEY,
                p2p_payload: vec![1],
                ..RawColumns::absent()
            },
            corrupt("P2P version and payload must be both set or both empty"),
        ),
        (
            "unknown status before zero key",
            RawColumns {
                stored_status: 9,
                consensus_pubkey: [0; 48],
                ..RawColumns::active()
            },
            "Fatal: unknown validator status 9".to_string(),
        ),
        (
            "zero key before jail height",
            RawColumns {
                consensus_pubkey: [0; 48],
                jailed_at: 7,
                ..RawColumns::active()
            },
            corrupt("registered validator is missing its consensus public key"),
        ),
        (
            "jail height outside a jailed lifecycle",
            RawColumns {
                jailed_at: 7,
                ..RawColumns::active()
            },
            corrupt("jail height is present outside a jailed lifecycle"),
        ),
        (
            "inactive stake",
            RawColumns {
                stake: StakeProjection::new(U256::from(1), None),
                ..RawColumns::registered(INACTIVE, false, false, 0)
            },
            corrupt("inactive validator retains bonded stake or an unbonding hint"),
        ),
        (
            "non-canonical coupled fields",
            RawColumns {
                has_bls_share: false,
                ..RawColumns::active()
            },
            corrupt(&format!(
                "non-canonical coupled fields: status={ACTIVE}, share=false, readiness=false, jailed_at=0"
            )),
        ),
    ];
    for (name, raw, expected) in cases {
        assert_eq!(error_text(raw.decode()), expected, "{name}");
    }
}

#[test]
fn decoded_lifecycles_enforce_exact_history_invariants() {
    let cases: [(&str, RawColumns, &str); 7] = [
        (
            "zero sentinel before the active rule",
            RawColumns {
                history: ValidatorHistory::new(
                    13,
                    Some(0),
                    HistoryCounters {
                        slash_count: 2,
                        missed_blocks: 3,
                        missed_votes: 5,
                        blocks_proposed: 8,
                    },
                ),
                ..RawColumns::active()
            },
            "deactivation height uses a non-canonical zero sentinel",
        ),
        (
            "active",
            RawColumns {
                history: ValidatorHistory::new(
                    13,
                    Some(21),
                    HistoryCounters {
                        slash_count: 2,
                        missed_blocks: 3,
                        missed_votes: 5,
                        blocks_proposed: 8,
                    },
                ),
                ..RawColumns::active()
            },
            "active validator retains a deactivation height",
        ),
        (
            "exiting",
            RawColumns {
                history: ValidatorHistory::new(
                    13,
                    None,
                    HistoryCounters {
                        slash_count: 2,
                        missed_blocks: 3,
                        missed_votes: 5,
                        blocks_proposed: 8,
                    },
                ),
                ..RawColumns::registered(EXITING, true, false, 0)
            },
            "exiting validator is missing its deactivation height",
        ),
        (
            "unbonding",
            RawColumns {
                history: ValidatorHistory::new(
                    13,
                    None,
                    HistoryCounters {
                        slash_count: 2,
                        missed_blocks: 3,
                        missed_votes: 5,
                        blocks_proposed: 8,
                    },
                ),
                ..RawColumns::registered(UNBONDING, false, false, 0)
            },
            "unbonding validator is missing its deactivation height",
        ),
        (
            "inactive",
            RawColumns {
                history: ValidatorHistory::new(
                    13,
                    None,
                    HistoryCounters {
                        slash_count: 2,
                        missed_blocks: 3,
                        missed_votes: 5,
                        blocks_proposed: 8,
                    },
                ),
                ..RawColumns::registered(INACTIVE, false, false, 0)
            },
            "inactive validator is missing its deactivation height",
        ),
        (
            "retained jail",
            RawColumns {
                history: ValidatorHistory::new(
                    13,
                    Some(54),
                    HistoryCounters {
                        slash_count: 2,
                        missed_blocks: 3,
                        missed_votes: 5,
                        blocks_proposed: 8,
                    },
                ),
                ..RawColumns::registered(JAILED, true, false, 55)
            },
            "retained jail height and history deactivation height disagree",
        ),
        (
            "excluded jail",
            RawColumns {
                history: ValidatorHistory::new(
                    13,
                    Some(54),
                    HistoryCounters {
                        slash_count: 2,
                        missed_blocks: 3,
                        missed_votes: 5,
                        blocks_proposed: 8,
                    },
                ),
                ..RawColumns::registered(JAILED, false, false, 55)
            },
            "jail height and history deactivation height disagree",
        ),
    ];
    for (name, raw, detail) in cases {
        assert_eq!(error_text(raw.decode()), corrupt(detail), "{name}");
    }
}

#[test]
fn history_updates_keep_exact_errors_and_precedence() {
    let lifecycle = |status, share, confirmed, jailed_at| {
        canonical(status, share, confirmed, jailed_at).into_lifecycle()
    };
    let deactivated = |height| {
        ValidatorHistory::new(
            13,
            height,
            HistoryCounters {
                slash_count: 2,
                missed_blocks: 3,
                missed_votes: 5,
                blocks_proposed: 8,
            },
        )
    };
    let cases: [(&str, ValidatorLifecycle, ValidatorHistory, &str); 9] = [
        (
            "zero sentinel before absence",
            ValidatorLifecycle::Absent,
            deactivated(Some(0)),
            "Fatal: deactivation height must not use the zero sentinel",
        ),
        (
            "absent",
            ValidatorLifecycle::Absent,
            deactivated(None),
            "Revert: validator not registered",
        ),
        (
            "active",
            lifecycle(ACTIVE, true, false, 0),
            deactivated(Some(5)),
            "Fatal: active history must not contain a deactivation height",
        ),
        (
            "retained jail",
            lifecycle(JAILED, true, false, 55),
            deactivated(Some(54)),
            "Fatal: retained jail history must match the jail height",
        ),
        (
            "excluded jail",
            lifecycle(JAILED, false, false, 55),
            deactivated(Some(54)),
            "Fatal: jail history must match the jail height",
        ),
        (
            "exiting",
            lifecycle(EXITING, true, false, 0),
            deactivated(None),
            "Fatal: exiting history requires a deactivation height",
        ),
        (
            "unbonding",
            lifecycle(UNBONDING, false, false, 0),
            deactivated(None),
            "Fatal: unbonding history requires a deactivation height",
        ),
        (
            "inactive",
            lifecycle(INACTIVE, false, false, 0),
            deactivated(None),
            "Fatal: inactive history requires a deactivation height",
        ),
        (
            "zero sentinel before the active rule",
            lifecycle(ACTIVE, true, false, 0),
            deactivated(Some(0)),
            "Fatal: deactivation height must not use the zero sentinel",
        ),
    ];
    for (name, lifecycle, history, expected) in cases {
        assert_eq!(
            error_text(with_history(lifecycle, history)),
            expected,
            "{name}"
        );
    }

    let updated = with_history(
        lifecycle(REGISTERED, false, false, 0),
        deactivated(Some(40)),
    )
    .unwrap();
    let ValidatorLifecycle::WaitingForStake(waiting) = updated else {
        panic!("history update changed the lifecycle variant");
    };
    assert_eq!(waiting.history, deactivated(Some(40)));
    assert_eq!(waiting.consensus_pubkey, KEY);
    assert_eq!(
        waiting.stake,
        StakeProjection::new(U256::from(1_500), Some(34))
    );
}

#[test]
fn stake_and_p2p_updates_keep_exact_errors_and_precedence() {
    let inactive = canonical(INACTIVE, false, false, 0).into_lifecycle();
    let stake = StakeProjection::new(U256::from(1), None);
    assert_eq!(
        error_text(with_stake(
            ValidatorLifecycle::Absent,
            StakeProjection::new(U256::from(1), Some(0))
        )),
        "Fatal: unbonding-end hint must not use the zero sentinel"
    );
    assert_eq!(
        error_text(with_stake(ValidatorLifecycle::Absent, stake)),
        "Revert: cannot stake before validator registration"
    );
    assert_eq!(
        error_text(with_stake(inactive.clone(), stake)),
        "Revert: inactive validator must re-register before staking"
    );
    assert_eq!(
        error_text(with_p2p(ValidatorLifecycle::Absent, P2pInfo::Unset)),
        "Revert: validator not registered"
    );
    let ValidatorLifecycle::Inactive(updated) = with_p2p(inactive, P2pInfo::Unset).unwrap() else {
        panic!("p2p update changed the lifecycle variant");
    };
    assert_eq!(updated.p2p, P2pInfo::Unset);
}

#[test]
fn demoted_joiner_exit_requires_a_nonzero_height() {
    let ValidatorLifecycle::WaitingForStake(waiting) =
        canonical(REGISTERED, false, false, 0).into_lifecycle()
    else {
        panic!("REGISTERED did not decode to WaitingForStake");
    };
    assert_eq!(
        error_text(exit_waiting_for_stake_at_boundary(waiting.clone(), 0)),
        "Fatal: validator deactivation height must be non-zero"
    );
    let exiting = exit_waiting_for_stake_at_boundary(waiting.clone(), 77).unwrap();
    assert_eq!(exiting.history.last_deactivated_at_height, Some(77));
    assert_eq!(exiting.stake, waiting.stake);
    assert_eq!(exiting.registry_index, waiting.registry_index);
}

#[test]
fn only_the_fresh_genesis_history_is_zero() {
    assert!(ValidatorHistory::fresh(0).is_zero());
    let nonzero = [
        ValidatorHistory::new(
            1,
            None,
            HistoryCounters {
                slash_count: 0,
                missed_blocks: 0,
                missed_votes: 0,
                blocks_proposed: 0,
            },
        ),
        ValidatorHistory::new(
            0,
            Some(1),
            HistoryCounters {
                slash_count: 0,
                missed_blocks: 0,
                missed_votes: 0,
                blocks_proposed: 0,
            },
        ),
        ValidatorHistory::new(
            0,
            None,
            HistoryCounters {
                slash_count: 1,
                missed_blocks: 0,
                missed_votes: 0,
                blocks_proposed: 0,
            },
        ),
        ValidatorHistory::new(
            0,
            None,
            HistoryCounters {
                slash_count: 0,
                missed_blocks: 1,
                missed_votes: 0,
                blocks_proposed: 0,
            },
        ),
        ValidatorHistory::new(
            0,
            None,
            HistoryCounters {
                slash_count: 0,
                missed_blocks: 0,
                missed_votes: 1,
                blocks_proposed: 0,
            },
        ),
        ValidatorHistory::new(
            0,
            None,
            HistoryCounters {
                slash_count: 0,
                missed_blocks: 0,
                missed_votes: 0,
                blocks_proposed: 1,
            },
        ),
    ];
    for history in nonzero {
        assert!(!history.is_zero(), "{history:?}");
    }
}
