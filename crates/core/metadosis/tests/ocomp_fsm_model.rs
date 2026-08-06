// OCOMP-TEST-ID: OCM-FSM-001

use alloy_primitives::{B256, U256};
use outbe_common::WorldwideDay;
use outbe_metadosis::model::{
    transition_rules, DayPhase, JobFsmCommand, JobFsmLimits, JobFsmState, JobFsmTransitionKind,
    ReadyAttemptSnapshot, RequestEffectMode,
};

const WWD: WorldwideDay = WorldwideDay::new(20_260_723);
const REQUEST_HEIGHT: u64 = 40;
const DEADLINE_HEIGHT: u64 = 104;
const LYSIS_BUDGET: U256 = U256::from_limbs([700, 0, 0, 0]);
const RECEIPT_HASH: B256 = B256::repeat_byte(0x51);
const FIRST_INTENT: B256 = B256::repeat_byte(0x61);
const GENERATED_SEQUENCE_SEED: u64 = 0x4f43_4d2d_4653_4d31;

#[test]
fn deferred_ready_attempt_moves_only_its_due_index() {
    let limits = JobFsmLimits {
        max_terminal_records: 4,
    };
    let mut state = JobFsmState::initial_ready(WWD, REQUEST_HEIGHT);

    let deferred = state
        .apply(
            JobFsmCommand::Defer {
                at_height: REQUEST_HEIGHT,
                next_check_height: REQUEST_HEIGHT + 1,
            },
            limits,
        )
        .unwrap();

    assert_eq!(deferred.phase, DayPhase::Ready);
    assert_eq!(deferred.pending_nonce, 0);
    assert_eq!(deferred.next_check_height, Some(REQUEST_HEIGHT + 1));
    assert_eq!(deferred.live_intent_id, None);
    assert_eq!(deferred.terminal_records, 0);
    assert_eq!(deferred.retained_lysis_budget, None);
    assert_eq!(
        state.request_effect_mode(LYSIS_BUDGET),
        Ok(RequestEffectMode::Fresh { effect_nonce: 0 })
    );

    let before_invalid_defer = state.clone();
    assert!(state
        .apply(
            JobFsmCommand::Defer {
                at_height: REQUEST_HEIGHT,
                next_check_height: REQUEST_HEIGHT + 1,
            },
            limits,
        )
        .is_err());
    assert_eq!(state, before_invalid_defer);
}

#[test]
fn request_then_expiry_requeues_one_block_later_without_repeating_budget() {
    let limits = JobFsmLimits {
        max_terminal_records: 4,
    };
    let mut state = JobFsmState::initial_ready(WWD, REQUEST_HEIGHT);

    assert_eq!(state.validate(limits), Ok(()));
    assert_eq!(state.projection().phase, DayPhase::Ready);
    assert_eq!(
        state.request_effect_mode(LYSIS_BUDGET),
        Ok(RequestEffectMode::Fresh { effect_nonce: 0 })
    );

    let requested = state
        .apply(
            JobFsmCommand::Request {
                at_height: REQUEST_HEIGHT,
                deadline_height: DEADLINE_HEIGHT,
                intent_id: FIRST_INTENT,
                lysis_budget: LYSIS_BUDGET,
                request_budget_receipt_hash: RECEIPT_HASH,
            },
            limits,
        )
        .unwrap();

    assert_eq!(requested.phase, DayPhase::OffchainPending);
    assert_eq!(requested.pending_nonce, 0);
    assert_eq!(requested.live_intent_id, Some(FIRST_INTENT));
    assert_eq!(requested.deadline_height, Some(DEADLINE_HEIGHT));
    let opened = state
        .apply(
            JobFsmCommand::OpenVoting {
                at_height: REQUEST_HEIGHT + 4,
                deadline_height: DEADLINE_HEIGHT,
            },
            limits,
        )
        .unwrap();
    assert_eq!(opened.deadline_height, Some(DEADLINE_HEIGHT));
    assert_eq!(state.validate(limits), Ok(()));

    let before_early_expiry = state.clone();
    assert!(state
        .apply(
            JobFsmCommand::Expire {
                at_height: DEADLINE_HEIGHT - 1,
                at_time: 1_753_315_263,
            },
            limits,
        )
        .is_err());
    assert_eq!(state, before_early_expiry);

    let expired = state
        .apply(
            JobFsmCommand::Expire {
                at_height: DEADLINE_HEIGHT,
                at_time: 1_753_315_264,
            },
            limits,
        )
        .unwrap();

    assert_eq!(expired.phase, DayPhase::Ready);
    assert_eq!(expired.pending_nonce, 1);
    assert_eq!(expired.next_check_height, Some(DEADLINE_HEIGHT + 1));
    assert_eq!(expired.live_intent_id, None);
    assert_eq!(expired.deadline_height, None);
    assert_eq!(expired.terminal_records, 1);
    assert_eq!(expired.retained_lysis_budget, Some(LYSIS_BUDGET));
    assert_eq!(
        state.request_effect_mode(LYSIS_BUDGET),
        Ok(RequestEffectMode::Replay { effect_nonce: 0 })
    );
    assert_eq!(state.validate(limits), Ok(()));
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelPhase {
    Ready {
        pending_nonce: u64,
        next_check_height: u64,
        retained_effect: bool,
    },
    Pending {
        pending_nonce: u64,
        requested_height: u64,
        deadline_height: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IndependentModel {
    phase: ModelPhase,
    terminal_records: u16,
}

impl IndependentModel {
    fn initial() -> Self {
        Self {
            phase: ModelPhase::Ready {
                pending_nonce: 0,
                next_check_height: REQUEST_HEIGHT,
                retained_effect: false,
            },
            terminal_records: 0,
        }
    }

    fn apply(&mut self, command: JobFsmCommand, limits: JobFsmLimits) -> Result<(), ()> {
        let next = match (self.phase, command) {
            (
                ModelPhase::Ready {
                    pending_nonce,
                    next_check_height,
                    retained_effect,
                },
                JobFsmCommand::Defer {
                    at_height,
                    next_check_height: deferred_height,
                },
            ) if at_height >= next_check_height && deferred_height > at_height => {
                ModelPhase::Ready {
                    pending_nonce,
                    next_check_height: deferred_height,
                    retained_effect,
                }
            }
            (
                ModelPhase::Ready {
                    pending_nonce,
                    next_check_height,
                    retained_effect,
                },
                JobFsmCommand::Request {
                    at_height,
                    deadline_height,
                    intent_id,
                    lysis_budget,
                    request_budget_receipt_hash,
                },
            ) if at_height >= next_check_height
                && deadline_height > at_height
                && !intent_id.is_zero()
                && !request_budget_receipt_hash.is_zero()
                && (!retained_effect
                    || (lysis_budget == LYSIS_BUDGET
                        && request_budget_receipt_hash == RECEIPT_HASH)) =>
            {
                ModelPhase::Pending {
                    pending_nonce,
                    requested_height: at_height,
                    deadline_height,
                }
            }
            (
                ModelPhase::Pending {
                    pending_nonce,
                    requested_height,
                    deadline_height: _,
                },
                JobFsmCommand::OpenVoting {
                    at_height,
                    deadline_height,
                },
            ) if at_height > requested_height && deadline_height > at_height => {
                ModelPhase::Pending {
                    pending_nonce,
                    requested_height,
                    deadline_height,
                }
            }
            (
                ModelPhase::Pending {
                    pending_nonce,
                    requested_height: _,
                    deadline_height,
                },
                JobFsmCommand::Expire {
                    at_height,
                    at_time: _,
                },
            ) if at_height >= deadline_height
                && self.terminal_records < limits.max_terminal_records =>
            {
                self.terminal_records += 1;
                ModelPhase::Ready {
                    pending_nonce: pending_nonce.checked_add(1).ok_or(())?,
                    next_check_height: at_height.checked_add(1).ok_or(())?,
                    retained_effect: true,
                }
            }
            (
                ModelPhase::Pending {
                    pending_nonce,
                    requested_height: _,
                    deadline_height: _,
                },
                JobFsmCommand::Conflict {
                    at_height,
                    at_time: _,
                },
            ) if self.terminal_records < limits.max_terminal_records => {
                self.terminal_records += 1;
                ModelPhase::Ready {
                    pending_nonce: pending_nonce.checked_add(1).ok_or(())?,
                    next_check_height: at_height.checked_add(1).ok_or(())?,
                    retained_effect: true,
                }
            }
            _ => return Err(()),
        };
        self.phase = next;
        Ok(())
    }
}

fn next_random(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state
}

#[test]
fn generated_sequences_match_independent_model_and_registered_transition_table() {
    assert_eq!(transition_rules().len(), 5);
    assert_eq!(transition_rules()[0].kind, JobFsmTransitionKind::Defer);
    assert_eq!(transition_rules()[0].from, DayPhase::Ready);
    assert_eq!(transition_rules()[0].to, DayPhase::Ready);
    assert_eq!(transition_rules()[1].kind, JobFsmTransitionKind::Request);
    assert_eq!(transition_rules()[1].from, DayPhase::Ready);
    assert_eq!(transition_rules()[1].to, DayPhase::OffchainPending);
    assert_eq!(transition_rules()[2].kind, JobFsmTransitionKind::OpenVoting);
    assert_eq!(transition_rules()[2].from, DayPhase::OffchainPending);
    assert_eq!(transition_rules()[2].to, DayPhase::OffchainPending);
    assert_eq!(transition_rules()[3].kind, JobFsmTransitionKind::Expire);
    assert_eq!(transition_rules()[3].from, DayPhase::OffchainPending);
    assert_eq!(transition_rules()[3].to, DayPhase::Ready);
    assert_eq!(transition_rules()[4].kind, JobFsmTransitionKind::Conflict);
    assert_eq!(transition_rules()[4].from, DayPhase::OffchainPending);
    assert_eq!(transition_rules()[4].to, DayPhase::Ready);

    let limits = JobFsmLimits {
        max_terminal_records: 16,
    };
    let mut production = JobFsmState::initial_ready(WWD, REQUEST_HEIGHT);
    let mut model = IndependentModel::initial();
    let mut random = GENERATED_SEQUENCE_SEED;

    for operation_index in 0..512_u64 {
        let value = next_random(&mut random);
        let command = generate_command(&production, value, operation_index);

        let production_before = production.clone();
        let model_before = model;
        let actual = production.apply(command, limits);
        let expected = model.apply(command, limits);
        assert_eq!(
            actual.is_ok(),
            expected.is_ok(),
            "seed={GENERATED_SEQUENCE_SEED:#x}, operation={operation_index}, command={command:?}"
        );
        if actual.is_err() {
            assert_eq!(production, production_before);
            assert_eq!(model, model_before);
        }

        assert_model_parity(&production, &model);
        assert_eq!(production.validate(limits), Ok(()));
    }
}

fn generate_command(production: &JobFsmState, value: u64, operation_index: u64) -> JobFsmCommand {
    let projection = production.projection();
    match projection.phase {
        DayPhase::Ready => {
            let due = projection.next_check_height.unwrap();
            let at_height = if value & 1 == 0 {
                due
            } else {
                due.saturating_sub(1)
            };
            if value & 0x10 == 0 {
                JobFsmCommand::Defer {
                    at_height,
                    next_check_height: if value & 4 == 0 {
                        at_height.saturating_add(1 + value % 8)
                    } else {
                        at_height
                    },
                }
            } else {
                let valid_retry = projection.pending_nonce == 0 || value & 2 == 0;
                JobFsmCommand::Request {
                    at_height,
                    deadline_height: at_height.saturating_add(64),
                    intent_id: B256::from(U256::from(operation_index + 1)),
                    lysis_budget: if valid_retry {
                        LYSIS_BUDGET
                    } else {
                        LYSIS_BUDGET + U256::from(1)
                    },
                    request_budget_receipt_hash: if valid_retry {
                        RECEIPT_HASH
                    } else {
                        B256::repeat_byte(0x52)
                    },
                }
            }
        }
        DayPhase::OffchainPending => {
            let deadline = projection.deadline_height.expect("pending deadline");
            if value & 0x20 == 0 {
                let requested_height = production
                    .snapshot()
                    .live
                    .expect("pending snapshot")
                    .requested_height;
                JobFsmCommand::OpenVoting {
                    at_height: if value & 1 == 0 {
                        requested_height.saturating_add(4)
                    } else {
                        requested_height
                    },
                    deadline_height: if value & 4 == 0 {
                        requested_height.saturating_add(8)
                    } else {
                        requested_height
                    },
                }
            } else if value & 8 == 0 {
                JobFsmCommand::Request {
                    at_height: deadline,
                    deadline_height: deadline.saturating_add(64),
                    intent_id: B256::from(U256::from(operation_index + 1)),
                    lysis_budget: LYSIS_BUDGET,
                    request_budget_receipt_hash: RECEIPT_HASH,
                }
            } else if value & 0x10 == 0 {
                JobFsmCommand::Conflict {
                    at_height: deadline.saturating_sub(1),
                    at_time: 1_753_315_200 + operation_index,
                }
            } else {
                JobFsmCommand::Expire {
                    at_height: if value & 1 == 0 {
                        deadline
                    } else {
                        deadline - 1
                    },
                    at_time: 1_753_315_200 + operation_index,
                }
            }
        }
    }
}

fn assert_model_parity(production: &JobFsmState, model: &IndependentModel) {
    let projection = production.projection();
    match model.phase {
        ModelPhase::Ready {
            pending_nonce,
            next_check_height,
            retained_effect,
        } => {
            assert_eq!(projection.phase, DayPhase::Ready);
            assert_eq!(projection.pending_nonce, pending_nonce);
            assert_eq!(projection.next_check_height, Some(next_check_height));
            assert_eq!(
                projection.retained_lysis_budget,
                retained_effect.then_some(LYSIS_BUDGET)
            );
        }
        ModelPhase::Pending {
            pending_nonce,
            requested_height: _,
            deadline_height,
        } => {
            assert_eq!(projection.phase, DayPhase::OffchainPending);
            assert_eq!(projection.pending_nonce, pending_nonce);
            assert_eq!(projection.deadline_height, Some(deadline_height));
            assert_eq!(projection.retained_lysis_budget, Some(LYSIS_BUDGET));
        }
    }
    assert_eq!(projection.terminal_records, model.terminal_records);
}

/// Two WorldwideDays' FSMs share no state: an interleaved generated sequence
/// must evolve each day exactly as if it ran alone, and one day's terminal
/// budget must not constrain the other's. This freezes the per-day cap
/// semantics at the model level.
#[test]
fn two_worldwide_days_generated_sequences_stay_independent() {
    let limits = JobFsmLimits {
        max_terminal_records: 16,
    };
    let other_wwd = outbe_common::WorldwideDay::new(WWD.value() + 1);
    let mut lanes = [
        (
            JobFsmState::initial_ready(WWD, REQUEST_HEIGHT),
            IndependentModel::initial(),
        ),
        (
            JobFsmState::initial_ready(other_wwd, REQUEST_HEIGHT),
            IndependentModel::initial(),
        ),
    ];
    let mut random = GENERATED_SEQUENCE_SEED ^ 0x0002;

    for operation_index in 0..512_u64 {
        let lane_index = usize::try_from(next_random(&mut random) & 1).unwrap();
        let value = next_random(&mut random);
        let other_before = lanes[1 - lane_index].clone();

        let (production, model) = &mut lanes[lane_index];
        let command = generate_command(production, value, operation_index);
        let production_before = production.clone();
        let model_before = *model;
        let actual = production.apply(command, limits);
        let expected = model.apply(command, limits);
        assert_eq!(
            actual.is_ok(),
            expected.is_ok(),
            "lane={lane_index}, operation={operation_index}, command={command:?}"
        );
        if actual.is_err() {
            assert_eq!(*production, production_before);
            assert_eq!(*model, model_before);
        }
        assert_model_parity(production, model);
        assert_eq!(production.validate(limits), Ok(()));

        // The untouched day must be byte-identical: no shared budget, no
        // shared index, no cross-day coupling of any kind.
        assert_eq!(lanes[1 - lane_index], other_before);
    }

    // Each day owns its full terminal budget independently: their combined
    // history may exceed one day's cap, which the former global index forbade.
    let combined: u32 = lanes
        .iter()
        .map(|(production, _)| u32::from(production.projection().terminal_records))
        .sum();
    assert!(
        combined <= 2 * u32::from(limits.max_terminal_records),
        "combined history is bounded only by the sum of per-day caps"
    );
}

#[test]
fn restored_snapshot_rejects_corrupted_status_index_and_budget_equivalences() {
    let limits = JobFsmLimits {
        max_terminal_records: 4,
    };
    let mut pending = JobFsmState::initial_ready(WWD, REQUEST_HEIGHT);
    pending
        .apply(
            JobFsmCommand::Request {
                at_height: REQUEST_HEIGHT,
                deadline_height: DEADLINE_HEIGHT,
                intent_id: FIRST_INTENT,
                lysis_budget: LYSIS_BUDGET,
                request_budget_receipt_hash: RECEIPT_HASH,
            },
            limits,
        )
        .unwrap();
    pending
        .apply(
            JobFsmCommand::OpenVoting {
                at_height: REQUEST_HEIGHT + 4,
                deadline_height: DEADLINE_HEIGHT,
            },
            limits,
        )
        .unwrap();

    let snapshot = pending.snapshot();
    assert_eq!(
        JobFsmState::restore(snapshot.clone(), limits),
        Ok(pending.clone())
    );

    let mut duplicate_phase = snapshot.clone();
    duplicate_phase.ready = Some(ReadyAttemptSnapshot {
        pending_nonce: 0,
        next_check_height: REQUEST_HEIGHT,
        retained_effect: None,
    });
    assert!(JobFsmState::restore(duplicate_phase, limits).is_err());

    let mut future_effect = snapshot.clone();
    future_effect
        .live
        .as_mut()
        .unwrap()
        .retained_effect
        .effect_nonce = 1;
    assert!(JobFsmState::restore(future_effect, limits).is_err());

    let mut invalid_deadline = snapshot;
    let live = invalid_deadline.live.as_mut().unwrap();
    live.deadline_height = Some(live.requested_height);
    assert!(JobFsmState::restore(invalid_deadline, limits).is_err());

    let mut expired = pending;
    expired
        .apply(
            JobFsmCommand::Expire {
                at_height: DEADLINE_HEIGHT,
                at_time: 1_753_315_264,
            },
            limits,
        )
        .unwrap();
    let mut skipped_nonce = expired.snapshot();
    skipped_nonce.terminal[0].next_pending_nonce = 2;
    assert!(JobFsmState::restore(skipped_nonce, limits).is_err());

    let mut over_terminal_cap = expired.snapshot();
    let terminal = over_terminal_cap.terminal[0];
    over_terminal_cap.terminal.extend([terminal; 4]);
    assert!(JobFsmState::restore(over_terminal_cap, limits).is_err());
}
