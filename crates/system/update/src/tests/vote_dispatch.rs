use super::tee_fixture::initial_tee_policy;
use super::{block_ctx, PV, TEST_CHAIN_ID};
use crate::handlers::UpgradeHandlerRegistry;
use crate::payload::{encode_schedule_update_json, encode_update_plan_json, UpdatePlanPayload};
use crate::schema::{ScheduledUpdateStatus, Update};
use crate::UpdateVoteTarget;
use alloy_primitives::{address, Address, B256, U256};
use outbe_primitives::addresses::UPDATE_ADDRESS;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_teeregistry::TeeRegistry;
use outbe_vote::handlers::{
    TargetExecutionOutcome, VotePlanContext, VoteTarget, VoteTargetContext, VoteTargetRegistry,
};
use outbe_vote::schema::{ProposalPhase, ProposalStatus, Vote};
use outbe_vote::state::ProposalSubmission;

static TARGET: UpdateVoteTarget = UpdateVoteTarget;
static TARGETS: VoteTargetRegistry = VoteTargetRegistry::new(&[&TARGET]);
static HANDLERS: UpgradeHandlerRegistry = UpgradeHandlerRegistry::new(&[]);
const PROPOSER: Address = address!("0x1111111111111111111111111111111111111111");
const VOTER_A: Address = address!("0x2222222222222222222222222222222222222222");
const VOTER_B: Address = address!("0x3333333333333333333333333333333333333333");
const C: u64 = 100;
const Q: u64 = C + 43_200 + 1;
const T: u64 = Q + 43_200;
const H: u64 = T + 100;

fn new_provider() -> HashMapStorageProvider {
    let mut provider =
        HashMapStorageProvider::new_with_chain_identity(TEST_CHAIN_ID, B256::repeat_byte(1));
    provider.set_block_number(C);
    provider.enter(|storage| {
        let owner = Address::repeat_byte(0xff);
        let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage);
        validators.config_owner.write(owner).unwrap();
        validators.set_config_max_validators(100).unwrap();
        for (address, seed) in [(PROPOSER, 1), (VOTER_A, 2), (VOTER_B, 3)] {
            let mut key = [0u8; 48];
            key[0] = seed;
            validators.register_validator(owner, address, &key).unwrap();
            validators
                .activate_validator_via_boundary_for_test(address)
                .unwrap();
        }
    });
    provider
}

fn context(height: u64) -> VoteTargetContext {
    VoteTargetContext {
        proposer: PROPOSER,
        attached_value: U256::ZERO,
        block_number: height,
        chain_id: TEST_CHAIN_ID,
    }
}

fn proposal(
    storage: StorageHandle,
    proposer: Address,
    payload: &str,
    height: u64,
    quorum: bool,
) -> U256 {
    let mut vote = Vote::new(storage);
    let id = vote
        .create_proposal(proposer, UPDATE_ADDRESS, payload, height, &TARGETS)
        .unwrap();
    if quorum {
        vote.cast_vote_approve(id, VOTER_A, true, height).unwrap();
        vote.cast_vote_approve(id, VOTER_B, true, height).unwrap();
    }
    id
}

fn begin(provider: &mut HashMapStorageProvider, height: u64) {
    provider.set_block_number(height);
    provider.enter(|storage| {
        let ctx = block_ctx(storage.clone(), height);
        Vote::new(storage.clone())
            .process_begin_block(&ctx, &TARGETS)
            .unwrap();
        Update::new(storage)
            .process_begin_block_with_handlers(&ctx, &HANDLERS)
            .unwrap();
    });
}

fn status(storage: StorageHandle, id: U256) -> ProposalStatus {
    Vote::new(storage)
        .proposals
        .get(id)
        .unwrap()
        .unwrap()
        .proposal_status()
        .unwrap()
}

fn primary(provider: &mut HashMapStorageProvider, measurement: Option<B256>) -> U256 {
    provider.enter(|storage| {
        if measurement.is_some() {
            let policy = initial_tee_policy(
                storage.genesis_hash().unwrap(),
                B256::repeat_byte(0x31),
                0x41,
            );
            TeeRegistry::new(storage.clone())
                .install_initial_policy_v1(&policy)
                .unwrap();
        }
        let mut plan = UpdatePlanPayload::new(PV, "release");
        plan.mrenclave = measurement;
        proposal(
            storage,
            PROPOSER,
            &serde_json::to_string(&plan).unwrap(),
            C,
            true,
        )
    })
}

#[test]
fn primary_has_no_tee_effect_until_t_and_finishes_only_after_real_activation() {
    for measurement in [None, Some(B256::repeat_byte(0x32))] {
        let mut provider = new_provider();
        let id = primary(&mut provider, measurement);
        begin(&mut provider, Q - 1);
        provider.enter(|storage| {
            assert_eq!(status(storage.clone(), id), ProposalStatus::Pending);
            assert!(Update::new(storage.clone())
                .read_scheduled_update(id)
                .unwrap()
                .is_none());
            assert!(TeeRegistry::new(storage)
                .staged_successor_policy_v1()
                .unwrap()
                .is_none());
        });
        begin(&mut provider, Q);
        begin(&mut provider, T - 1);
        provider.enter(|storage| {
            let update = Update::new(storage.clone());
            assert_eq!(
                update
                    .read_scheduled_update(id)
                    .unwrap()
                    .unwrap()
                    .activation_height,
                H
            );
            assert!(!update.live_plans.get(id).unwrap().unwrap().prepared);
            assert_eq!(
                Vote::new(storage.clone())
                    .proposal_lifecycle(id)
                    .unwrap()
                    .phase,
                ProposalPhase::Replace
            );
            let tee = TeeRegistry::new(storage);
            assert!(tee.staged_successor_policy_v1().unwrap().is_none());
            assert_eq!(tee.enclave_upgrade_v1().unwrap().proposal_id, U256::ZERO);
        });
        begin(&mut provider, T);
        provider.enter(|storage| {
            assert_eq!(
                Vote::new(storage.clone())
                    .proposal_lifecycle(id)
                    .unwrap()
                    .phase,
                ProposalPhase::Activation
            );
            assert!(
                Update::new(storage.clone())
                    .live_plans
                    .get(id)
                    .unwrap()
                    .unwrap()
                    .prepared
            );
            let tee = TeeRegistry::new(storage.clone());
            assert_eq!(
                tee.staged_successor_policy_v1().unwrap().is_some(),
                measurement.is_some()
            );
            TARGET.prepare_plan(&block_ctx(storage, T), id, H).unwrap();
        });
        begin(&mut provider, H - 1);
        provider.enter(|storage| {
            assert_eq!(
                Update::new(storage).get_active_version().unwrap(),
                crate::ProtocolVersion::ZERO
            )
        });
        begin(&mut provider, H);
        provider.enter(|storage| {
            assert_eq!(
                Vote::new(storage.clone())
                    .proposal_lifecycle(id)
                    .unwrap()
                    .phase,
                ProposalPhase::Finished
            );
            assert_eq!(
                Update::new(storage.clone()).get_active_version().unwrap(),
                PV
            );
            let mut tee = TeeRegistry::new(storage);
            assert!(tee.staged_successor_policy_v1().unwrap().is_none());
            if let Some(measurement) = measurement {
                let completed = tee.enclave_upgrade_v1().unwrap();
                assert_eq!(completed.mrenclave, measurement);
                tee.discard_staged_successor_policy_v1(id).unwrap();
                assert_eq!(tee.enclave_upgrade_v1().unwrap(), completed);
            }
        });
    }
}

#[test]
fn cancel_at_last_admissible_height_shadows_parent_without_staging() {
    let mut provider = new_provider();
    let parent = primary(&mut provider, Some(B256::repeat_byte(0x32)));
    begin(&mut provider, Q);
    let cancel = provider.enter(|storage| {
        proposal(
            storage,
            VOTER_A,
            &serde_json::json!({"cancelProposalId":parent}).to_string(),
            T - 1,
            true,
        )
    });
    begin(&mut provider, T - 1);
    provider.enter(|storage| assert_eq!(status(storage, cancel), ProposalStatus::Pending));
    begin(&mut provider, T);
    provider.enter(|storage| {
        assert_eq!(status(storage.clone(), parent), ProposalStatus::Canceled);
        assert_eq!(status(storage.clone(), cancel), ProposalStatus::Approved);
        assert_eq!(
            Update::new(storage.clone())
                .read_scheduled_update(parent)
                .unwrap()
                .unwrap()
                .status,
            ScheduledUpdateStatus::Canceled
        );
        assert!(Update::new(storage.clone())
            .list_waiting_for_activation_proposal_ids()
            .unwrap()
            .is_empty());
        assert!(TeeRegistry::new(storage.clone())
            .staged_successor_policy_v1()
            .unwrap()
            .is_none());
        assert_eq!(
            TeeRegistry::new(storage)
                .enclave_upgrade_v1()
                .unwrap()
                .proposal_id,
            U256::ZERO
        );
    });
    begin(&mut provider, H);
}

#[test]
fn replacement_gets_full_new_replace_window_and_can_be_replaced_again() {
    let mut provider = new_provider();
    let parent = primary(&mut provider, Some(B256::repeat_byte(0x32)));
    begin(&mut provider, Q);
    let replacement = provider.enter(|storage| proposal(storage, VOTER_A, &serde_json::json!({"replaceProposalId":parent,"update":UpdatePlanPayload::new(PV,"replacement")}).to_string(), T - 1, true));
    begin(&mut provider, T);
    let next_t = T + 43_200;
    provider.enter(|storage| {
        assert_eq!(status(storage.clone(), parent), ProposalStatus::Canceled);
        let lifecycle = Vote::new(storage.clone())
            .proposal_lifecycle(replacement)
            .unwrap();
        assert_eq!(lifecycle.replace_end, next_t);
        assert_eq!(lifecycle.activation_height, next_t + 100);
        assert!(TeeRegistry::new(storage)
            .staged_successor_policy_v1()
            .unwrap()
            .is_none());
    });
    let second = provider.enter(|storage| proposal(storage, VOTER_B, &serde_json::json!({"replaceProposalId":replacement,"update":UpdatePlanPayload::new(PV,"again")}).to_string(), next_t - 1, true));
    begin(&mut provider, next_t);
    begin(&mut provider, next_t + 43_200);
    begin(&mut provider, next_t + 43_200 + 100);
    provider.enter(|storage| {
        assert_eq!(
            status(storage.clone(), replacement),
            ProposalStatus::Canceled
        );
        assert_eq!(
            Vote::new(storage.clone())
                .proposal_lifecycle(second)
                .unwrap()
                .phase,
            ProposalPhase::Finished
        );
        assert_eq!(Update::new(storage).get_active_version().unwrap(), PV);
    });
}

#[test]
fn no_quorum_challenger_expires_and_parent_prepares() {
    let mut provider = new_provider();
    let parent = primary(&mut provider, Some(B256::repeat_byte(0x32)));
    begin(&mut provider, Q);
    let cancel = provider.enter(|storage| {
        proposal(
            storage,
            VOTER_A,
            &serde_json::json!({"cancelProposalId":parent}).to_string(),
            T - 1,
            false,
        )
    });
    begin(&mut provider, T);
    provider.enter(|storage| {
        assert_eq!(status(storage.clone(), cancel), ProposalStatus::Expired);
        assert_eq!(status(storage.clone(), parent), ProposalStatus::Approved);
        assert!(TeeRegistry::new(storage)
            .staged_successor_policy_v1()
            .unwrap()
            .is_some());
    });
}

#[test]
fn enclave_reservation_is_exclusive_before_staging_and_invalid_successor_is_domain_error() {
    let mut provider = new_provider();
    let parent = primary(&mut provider, Some(B256::repeat_byte(0x32)));
    begin(&mut provider, Q);
    provider.enter(|storage| {
        let plan = encode_update_plan_json(PV, "competing software");
        let result = TARGET
            .register_plan(
                &block_ctx(storage.clone(), Q + 1),
                U256::from(99),
                plan.as_bytes(),
                context(Q + 1),
                VotePlanContext {
                    replace_end: T + 1,
                    activation_height: H + 1,
                },
            )
            .unwrap();
        assert!(matches!(result, TargetExecutionOutcome::Error { .. }));
        assert_eq!(
            Update::new(storage.clone())
                .list_waiting_for_activation_proposal_ids()
                .unwrap(),
            vec![parent]
        );
        assert!(TeeRegistry::new(storage)
            .staged_successor_policy_v1()
            .unwrap()
            .is_none());
    });
    let mut fresh = new_provider();
    let id = primary(&mut fresh, Some(B256::repeat_byte(0x31)));
    begin(&mut fresh, Q);
    fresh.enter(|storage| {
        assert_eq!(status(storage.clone(), id), ProposalStatus::Error);
        assert!(Update::new(storage)
            .list_waiting_for_activation_proposal_ids()
            .unwrap()
            .is_empty());
    });
}

#[test]
fn legacy_software_ballot_cannot_displace_a_reserved_enclave_plan() {
    let mut provider = new_provider();
    let legacy = provider.enter(|storage| {
        let payload = encode_schedule_update_json(PV, H - 1, "old pending software ballot");
        let mut vote = Vote::new(storage);
        let id = vote
            .write_proposal(
                &ProposalSubmission::new(VOTER_A, UPDATE_ADDRESS, &payload, C),
                T - 2,
                ProposalStatus::Pending,
            )
            .unwrap();
        vote.cast_vote_approve(id, VOTER_A, true, C).unwrap();
        vote.cast_vote_approve(id, VOTER_B, true, C).unwrap();
        id
    });
    let parent = primary(&mut provider, Some(B256::repeat_byte(0x32)));
    begin(&mut provider, Q);
    begin(&mut provider, T - 1);
    provider.enter(|storage| {
        assert_eq!(status(storage.clone(), legacy), ProposalStatus::Error);
        let update = Update::new(storage.clone());
        assert!(update.read_scheduled_update(legacy).unwrap().is_none());
        assert_eq!(
            update.list_waiting_for_activation_proposal_ids().unwrap(),
            vec![parent]
        );
        assert!(TeeRegistry::new(storage)
            .staged_successor_policy_v1()
            .unwrap()
            .is_none());
    });
    begin(&mut provider, T);
    begin(&mut provider, H - 1);
    provider.enter(|storage| {
        assert_eq!(
            Update::new(storage.clone())
                .get_active_version()
                .unwrap()
                .raw(),
            0
        );
        assert_eq!(
            TeeRegistry::new(storage)
                .staged_successor_policy_v1()
                .unwrap()
                .unwrap()
                .0,
            parent
        );
    });
    begin(&mut provider, H);
    provider.enter(|storage| {
        let tee = TeeRegistry::new(storage.clone());
        assert_eq!(tee.active_v1_policy_proposal_id.read().unwrap(), parent);
        assert_eq!(
            tee.active_policy_v1().unwrap().measurement_rules[0].mrenclave,
            B256::repeat_byte(0x32)
        );
        assert_eq!(
            Vote::new(storage).proposal_lifecycle(parent).unwrap().phase,
            ProposalPhase::Finished
        );
    });
}

#[test]
fn selected_invalid_replacement_retains_and_prepares_parent_without_fallback() {
    let mut provider = new_provider();
    let parent = primary(&mut provider, Some(B256::repeat_byte(0x32)));
    begin(&mut provider, Q);
    let mut invalid = UpdatePlanPayload::new(PV, "same measurement");
    invalid.mrenclave = Some(B256::repeat_byte(0x31));
    let replacement = provider.enter(|storage| {
        proposal(
            storage,
            VOTER_A,
            &serde_json::json!({"replaceProposalId":parent,"update":invalid}).to_string(),
            T - 2,
            true,
        )
    });
    let cancel = provider.enter(|storage| {
        proposal(
            storage,
            VOTER_B,
            &serde_json::json!({"cancelProposalId":parent}).to_string(),
            T - 1,
            true,
        )
    });
    begin(&mut provider, T);
    provider.enter(|storage| {
        assert_eq!(status(storage.clone(), replacement), ProposalStatus::Error);
        assert_eq!(status(storage.clone(), cancel), ProposalStatus::Canceled);
        assert_eq!(status(storage.clone(), parent), ProposalStatus::Approved);
        assert_eq!(
            TeeRegistry::new(storage)
                .enclave_upgrade_v1()
                .unwrap()
                .proposal_id,
            parent
        );
    });
}

#[test]
fn live_payload_rejects_activation_override_and_closed_or_unknown_parent() {
    let mut provider = new_provider();
    provider.enter(|storage| {
        let mut vote = Vote::new(storage.clone());
        for payload in [
            encode_schedule_update_json(PV, H, "legacy alias"),
            serde_json::json!({"cancelProposalId":U256::from(999)}).to_string(),
        ] {
            assert!(vote
                .create_proposal(PROPOSER, UPDATE_ADDRESS, &payload, C, &TARGETS)
                .is_err());
        }
    });
    let parent = primary(&mut provider, None);
    begin(&mut provider, Q);
    provider.enter(|storage| {
        assert!(Vote::new(storage)
            .create_proposal(
                VOTER_A,
                UPDATE_ADDRESS,
                &serde_json::json!({"cancelProposalId":parent}).to_string(),
                T,
                &TARGETS
            )
            .is_err());
    });
}

fn legacy_vote(
    storage: StorageHandle,
    payload: &str,
    status: ProposalStatus,
    deadline: u64,
) -> U256 {
    Vote::new(storage)
        .write_proposal(
            &ProposalSubmission::new(PROPOSER, UPDATE_ADDRESS, payload, C),
            deadline,
            status,
        )
        .unwrap()
}

#[test]
fn legacy_software_keeps_h_and_is_adopted_with_remaining_window() {
    let mut provider = new_provider();
    let historical_h = Q + 500;
    let id = provider.enter(|storage| {
        let payload = encode_schedule_update_json(PV, historical_h, "historical");
        let id = legacy_vote(storage.clone(), &payload, ProposalStatus::Approved, Q - 1);
        let result = TARGET
            .handle_approved(&block_ctx(storage, Q), id, payload.as_bytes(), context(Q))
            .unwrap();
        assert_eq!(result, TargetExecutionOutcome::Applied);
        id
    });
    provider.enter(|storage| {
        proposal(
            storage.clone(),
            VOTER_A,
            &serde_json::json!({"cancelProposalId":id}).to_string(),
            historical_h - 101,
            false,
        );
        assert_eq!(
            Vote::new(storage.clone())
                .proposal_lifecycle(id)
                .unwrap()
                .activation_height,
            historical_h
        );
        assert_eq!(
            Update::new(storage)
                .read_scheduled_update(id)
                .unwrap()
                .unwrap()
                .activation_height,
            historical_h
        );
    });
    begin(&mut provider, historical_h - 100);
    begin(&mut provider, historical_h);
    provider.enter(|storage| {
        assert_eq!(
            Update::new(storage).get_active_version_height().unwrap(),
            historical_h
        )
    });
}

#[test]
fn legacy_pending_vote_keeps_original_deadline_and_dispatch() {
    let mut provider = new_provider();
    let deadline = C + 1_000;
    let activation = deadline + 101;
    let id = provider.enter(|storage| {
        let id = legacy_vote(
            storage.clone(),
            &encode_schedule_update_json(PV, activation, "legacy"),
            ProposalStatus::Pending,
            deadline,
        );
        let mut vote = Vote::new(storage);
        vote.cast_vote_approve(id, VOTER_A, true, C + 1).unwrap();
        vote.cast_vote_approve(id, VOTER_B, true, C + 2).unwrap();
        id
    });
    begin(&mut provider, deadline);
    provider.enter(|storage| assert_eq!(status(storage, id), ProposalStatus::Pending));
    begin(&mut provider, deadline + 1);
    provider.enter(|storage| {
        assert_eq!(
            Update::new(storage)
                .read_scheduled_update(id)
                .unwrap()
                .unwrap()
                .activation_height,
            activation
        )
    });
    begin(&mut provider, activation);
}

#[test]
fn legacy_enclave_pending_vote_and_staged_schedule_block_cutover_without_mutation() {
    let mut provider = new_provider();
    provider.enter(|storage| {
        let mut payload = crate::ScheduleUpdatePayload::new(PV, H, "legacy enclave");
        payload.mrenclave = Some(B256::repeat_byte(0x32));
        let payload = serde_json::to_string(&payload).unwrap();
        let id = legacy_vote(storage.clone(), &payload, ProposalStatus::Pending, Q);
        let mut update = Update::new(storage.clone());
        assert!(matches!(
            update.assert_no_legacy_pending_enclave_rollout(),
            Err(PrecompileError::Fatal(_))
        ));
        assert!(!update.three_window_cutover_checked.read().unwrap());
        assert!(matches!(
            TARGET.handle_approved(
                &block_ctx(storage, Q + 1),
                id,
                payload.as_bytes(),
                context(Q + 1)
            ),
            Err(PrecompileError::Fatal(_))
        ));
    });
    let mut provider = new_provider();
    provider.enter(|storage| {
        let current = initial_tee_policy(
            storage.genesis_hash().unwrap(),
            B256::repeat_byte(0x31),
            0x41,
        );
        let mut tee = TeeRegistry::new(storage.clone());
        tee.install_initial_policy_v1(&current).unwrap();
        tee.stage_measurement_upgrade_v1(
            U256::from(77),
            B256::repeat_byte(0x32),
            current.policy_hash().unwrap(),
            H,
        )
        .unwrap();
        assert!(Update::new(storage.clone())
            .assert_no_legacy_pending_enclave_rollout()
            .is_err());
        assert_eq!(
            tee.enclave_upgrade_v1().unwrap().proposal_id,
            U256::from(77)
        );
        assert!(tee.staged_successor_policy_v1().unwrap().is_some());
    });
}

#[test]
fn registration_checkpoint_rolls_back_every_failed_mutation_prefix() {
    let mut reference = new_provider();
    reference.enter(|storage| {
        Update::new(storage)
            .assert_no_legacy_pending_enclave_rollout()
            .unwrap()
    });
    reference.clear_mutation_failure();
    reference.enter(|storage| {
        TARGET
            .register_plan(
                &block_ctx(storage, Q),
                U256::from(99),
                encode_update_plan_json(PV, "release").as_bytes(),
                context(Q),
                VotePlanContext {
                    replace_end: T,
                    activation_height: H,
                },
            )
            .unwrap();
    });
    let mutations = reference.clear_mutation_failure();
    assert!(mutations > 0);
    for prefix in 0..mutations {
        let mut provider = new_provider();
        provider.enter(|storage| {
            Update::new(storage)
                .assert_no_legacy_pending_enclave_rollout()
                .unwrap()
        });
        provider.fail_mutation_at(prefix);
        provider.enter(|storage| {
            assert!(TARGET
                .register_plan(
                    &block_ctx(storage.clone(), Q),
                    U256::from(99),
                    encode_update_plan_json(PV, "release").as_bytes(),
                    context(Q),
                    VotePlanContext {
                        replace_end: T,
                        activation_height: H
                    }
                )
                .is_err());
        });
        provider.clear_mutation_failure();
        provider.enter(|storage| {
            let update = Update::new(storage);
            assert!(update
                .read_scheduled_update(U256::from(99))
                .unwrap()
                .is_none());
            assert!(update.live_plans.get(U256::from(99)).unwrap().is_none());
            assert!(update
                .list_waiting_for_activation_proposal_ids()
                .unwrap()
                .is_empty());
        });
    }
}

#[test]
fn numeric_minimum_quorum_wins_mixed_actions_despite_swapped_pending_order() {
    let mut provider = new_provider();
    let parent = primary(&mut provider, Some(B256::repeat_byte(0x32)));
    begin(&mut provider, Q);
    let lower = provider.enter(|storage| {
        proposal(
            storage,
            PROPOSER,
            &serde_json::json!({"cancelProposalId":parent}).to_string(),
            T - 2,
            false,
        )
    });
    let winner = provider.enter(|storage| {
        proposal(
            storage,
            VOTER_A,
            &serde_json::json!({"cancelProposalId":parent}).to_string(),
            T - 2,
            true,
        )
    });
    let higher = provider.enter(|storage| proposal(storage, VOTER_B, &serde_json::json!({"replaceProposalId":parent,"update":UpdatePlanPayload::new(PV,"loser")}).to_string(), T - 1, true));
    provider.enter(|storage| {
        let vote = Vote::new(storage);
        vote.pending_proposal_ids.set(0, higher).unwrap();
        vote.pending_proposal_ids.set(1, winner).unwrap();
        vote.pending_proposal_ids.set(2, lower).unwrap();
    });
    begin(&mut provider, T);
    provider.enter(|storage| {
        assert_eq!(status(storage.clone(), lower), ProposalStatus::Expired);
        assert_eq!(status(storage.clone(), winner), ProposalStatus::Approved);
        assert_eq!(status(storage.clone(), higher), ProposalStatus::Canceled);
        assert_eq!(status(storage.clone(), parent), ProposalStatus::Canceled);
        assert!(Update::new(storage.clone())
            .list_waiting_for_activation_proposal_ids()
            .unwrap()
            .is_empty());
        assert!(TeeRegistry::new(storage)
            .staged_successor_policy_v1()
            .unwrap()
            .is_none());
    });
}

#[test]
fn automatic_outdated_cancellation_finishes_phase_after_actual_effects() {
    let mut provider = new_provider();
    let first = primary(&mut provider, None);
    let later = provider.enter(|storage| {
        proposal(
            storage,
            VOTER_A,
            &encode_update_plan_json(PV, "later"),
            C + 1,
            true,
        )
    });
    begin(&mut provider, Q);
    begin(&mut provider, Q + 1);
    begin(&mut provider, T);
    begin(&mut provider, T + 1);
    begin(&mut provider, H);
    provider.enter(|storage| {
        assert_eq!(
            Vote::new(storage.clone())
                .proposal_lifecycle(first)
                .unwrap()
                .phase,
            ProposalPhase::Finished
        );
        assert_eq!(
            Vote::new(storage.clone())
                .proposal_lifecycle(later)
                .unwrap()
                .phase,
            ProposalPhase::Finished
        );
        assert_eq!(status(storage.clone(), later), ProposalStatus::Canceled);
        assert_eq!(
            Update::new(storage)
                .read_scheduled_update(later)
                .unwrap()
                .unwrap()
                .status,
            ScheduledUpdateStatus::Canceled
        );
    });
}

#[test]
fn canceled_unsupported_plan_never_reaches_binary_ceiling() {
    let mut provider = new_provider();
    let parent = provider.enter(|storage| {
        proposal(
            storage,
            PROPOSER,
            &encode_update_plan_json(crate::encode_protocol_version(255, 1), "future"),
            C,
            true,
        )
    });
    begin(&mut provider, Q);
    provider.enter(|storage| {
        proposal(
            storage,
            VOTER_A,
            &serde_json::json!({"cancelProposalId":parent}).to_string(),
            T - 1,
            true,
        );
    });
    begin(&mut provider, T);
    begin(&mut provider, H);
    provider.enter(|storage| assert_eq!(status(storage, parent), ProposalStatus::Canceled));
}

#[test]
fn cancel_checkpoint_restores_schedule_and_queue_for_every_failed_prefix() {
    let mut measured = new_provider();
    let parent = primary(&mut measured, None);
    begin(&mut measured, Q);
    measured.clear_mutation_failure();
    measured.enter(|storage| {
        TARGET
            .cancel_plan(&block_ctx(storage, T), parent, U256::from(99))
            .unwrap()
    });
    let mutations = measured.clear_mutation_failure();
    assert!(mutations > 0);
    for prefix in 0..mutations {
        let mut provider = new_provider();
        let parent = primary(&mut provider, None);
        begin(&mut provider, Q);
        provider.fail_mutation_at(prefix);
        provider.enter(|storage| {
            assert!(TARGET
                .cancel_plan(&block_ctx(storage, T), parent, U256::from(99))
                .is_err())
        });
        provider.clear_mutation_failure();
        provider.enter(|storage| {
            let update = Update::new(storage);
            assert_eq!(
                update
                    .read_scheduled_update(parent)
                    .unwrap()
                    .unwrap()
                    .status,
                ScheduledUpdateStatus::Scheduled
            );
            assert_eq!(
                update.list_waiting_for_activation_proposal_ids().unwrap(),
                vec![parent]
            );
        });
    }
}

#[test]
fn preparation_rejects_early_or_corrupt_metadata_and_governance_cannot_cancel_after_t() {
    let mut provider = new_provider();
    let id = primary(&mut provider, None);
    begin(&mut provider, Q);
    provider.enter(|storage| {
        assert!(TARGET
            .prepare_plan(&block_ctx(storage.clone(), T - 1), id, H)
            .is_err());
        assert!(
            TARGET
                .prepare_plan(&block_ctx(storage.clone(), H), id, H)
                .is_err(),
            "skipping preparation must not shorten the activation window"
        );
        let update = Update::new(storage.clone());
        let mut metadata = update.live_plans.get(id).unwrap().unwrap();
        metadata.activation_height += 1;
        update.live_plans.update(&metadata).unwrap();
        assert!(TARGET
            .prepare_plan(&block_ctx(storage.clone(), T), id, H)
            .is_err());
        metadata.activation_height = H;
        update.live_plans.update(&metadata).unwrap();
    });
    begin(&mut provider, T);
    provider.enter(|storage| {
        let result = TARGET
            .cancel_plan(&block_ctx(storage.clone(), T), id, U256::from(99))
            .unwrap();
        assert!(matches!(result, TargetExecutionOutcome::Error { .. }));
        assert_eq!(
            Update::new(storage)
                .read_scheduled_update(id)
                .unwrap()
                .unwrap()
                .status,
            ScheduledUpdateStatus::Scheduled
        );
    });
}

#[test]
fn unsupported_surviving_plan_fails_at_h_without_finishing_phase() {
    let mut provider = new_provider();
    let id = provider.enter(|storage| {
        proposal(
            storage,
            PROPOSER,
            &encode_update_plan_json(crate::encode_protocol_version(255, 1), "future"),
            C,
            true,
        )
    });
    begin(&mut provider, Q);
    begin(&mut provider, T);
    provider.set_block_number(H);
    provider.enter(|storage| {
        let ctx = block_ctx(storage.clone(), H);
        assert!(matches!(
            Update::new(storage.clone()).process_begin_block_with_handlers(&ctx, &HANDLERS),
            Err(PrecompileError::Fatal(_))
        ));
        assert_eq!(
            Vote::new(storage.clone())
                .proposal_lifecycle(id)
                .unwrap()
                .phase,
            ProposalPhase::Activation
        );
        assert_eq!(status(storage.clone(), id), ProposalStatus::Approved);
        assert_eq!(
            Update::new(storage)
                .read_scheduled_update(id)
                .unwrap()
                .unwrap()
                .status,
            ScheduledUpdateStatus::Scheduled
        );
    });
}

#[test]
fn tee_preparation_checkpoint_rolls_back_every_failed_mutation_prefix() {
    let mut measured = new_provider();
    let id = primary(&mut measured, Some(B256::repeat_byte(0x32)));
    begin(&mut measured, Q);
    measured.set_block_number(T);
    measured.clear_mutation_failure();
    measured.enter(|storage| TARGET.prepare_plan(&block_ctx(storage, T), id, H).unwrap());
    let mutations = measured.clear_mutation_failure();
    assert!(mutations > 0);
    for prefix in 0..mutations {
        let mut provider = new_provider();
        let id = primary(&mut provider, Some(B256::repeat_byte(0x32)));
        begin(&mut provider, Q);
        provider.set_block_number(T);
        provider.fail_after_mutation_at(prefix);
        provider
            .enter(|storage| assert!(TARGET.prepare_plan(&block_ctx(storage, T), id, H).is_err()));
        provider.clear_mutation_failure();
        provider.enter(|storage| {
            assert!(
                !Update::new(storage.clone())
                    .live_plans
                    .get(id)
                    .unwrap()
                    .unwrap()
                    .prepared
            );
            let tee = TeeRegistry::new(storage);
            assert!(tee.staged_successor_policy_v1().unwrap().is_none());
            assert_eq!(tee.enclave_upgrade_v1().unwrap().proposal_id, U256::ZERO);
        });
    }
}

#[test]
fn cutover_marker_is_rolled_back_with_its_enclosing_block() {
    let mut provider = new_provider();
    provider.enter(|storage| {
        let outcome: Result<(), PrecompileError> = storage.with_checkpoint(|| {
            Update::new(storage.clone()).assert_no_legacy_pending_enclave_rollout()?;
            Err(PrecompileError::Fatal("later block failure".into()))
        });
        assert!(outcome.is_err());
        assert!(!Update::new(storage.clone())
            .three_window_cutover_checked
            .read()
            .unwrap());
        Update::new(storage.clone())
            .assert_no_legacy_pending_enclave_rollout()
            .unwrap();
        assert!(Update::new(storage)
            .three_window_cutover_checked
            .read()
            .unwrap());
    });
}

#[test]
fn lost_or_early_prepared_metadata_cannot_cancel_or_activate_a_live_enclave_plan() {
    for fault in ["missing-plan", "early-prepared", "missing-staged"] {
        let mut provider = new_provider();
        let parent = primary(&mut provider, Some(B256::repeat_byte(0x32)));
        begin(&mut provider, Q);
        let check_height = if fault == "missing-staged" {
            begin(&mut provider, T);
            provider.enter(|storage| {
                TeeRegistry::new(storage)
                    .discard_staged_successor_policy_v1(parent)
                    .unwrap()
            });
            H
        } else {
            provider.enter(|storage| {
                let update = Update::new(storage.clone());
                if fault == "missing-plan" {
                    proposal(
                        storage.clone(),
                        VOTER_A,
                        &serde_json::json!({"cancelProposalId":parent}).to_string(),
                        T - 1,
                        true,
                    );
                    update.live_plans.delete(parent).unwrap();
                } else {
                    let mut metadata = update.live_plans.get(parent).unwrap().unwrap();
                    metadata.prepared = true;
                    update.live_plans.update(&metadata).unwrap();
                }
            });
            T
        };
        provider.set_block_number(check_height);
        provider.enter(|storage| {
            let ctx = block_ctx(storage.clone(), check_height);
            let result = Vote::new(storage.clone())
                .process_begin_block(&ctx, &TARGETS)
                .and_then(|()| {
                    Update::new(storage.clone()).process_begin_block_with_handlers(&ctx, &HANDLERS)
                });
            assert!(
                matches!(result, Err(PrecompileError::Fatal(_))),
                "{fault}: {result:?}"
            );
            let update = Update::new(storage.clone());
            assert_eq!(update.get_active_version().unwrap().raw(), 0);
            assert_eq!(
                update
                    .read_scheduled_update(parent)
                    .unwrap()
                    .unwrap()
                    .status,
                ScheduledUpdateStatus::Scheduled
            );
            assert_eq!(status(storage, parent), ProposalStatus::Approved);
        });
    }
}

#[test]
fn historical_short_notice_plan_keeps_h_without_a_replacement_interval() {
    let chain = outbe_primitives::chain::TESTNET_CHAIN_ID;
    let mut provider = HashMapStorageProvider::new_with_chain_identity(chain, B256::repeat_byte(1));
    provider.set_block_number(2);
    let h = crate::constants::MIN_ACTIVATION_BUFFER - 1;
    let id = provider.enter(|storage| {
        let payload = encode_schedule_update_json(PV, h, "historical short notice");
        let id = Vote::new(storage.clone())
            .write_proposal(
                &ProposalSubmission::new(PROPOSER, UPDATE_ADDRESS, &payload, 0),
                1,
                ProposalStatus::Approved,
            )
            .unwrap();
        Update::new(storage.clone())
            .write_scheduled_update(id, PV, h, "historical short notice")
            .unwrap();
        let record = Vote::new(storage.clone())
            .proposals
            .get(id)
            .unwrap()
            .unwrap();
        let plan = TARGET
            .existing_plan(storage.clone(), &record)
            .unwrap()
            .unwrap();
        assert_eq!(plan.replace_end, 0);
        assert_eq!(plan.activation_height, h);
        let result = TARGET
            .cancel_plan(&block_ctx(storage.clone(), 2), id, U256::from(2))
            .unwrap();
        assert!(matches!(result, TargetExecutionOutcome::Error { .. }));
        id
    });
    provider.set_block_number(h);
    provider.enter(|storage| {
        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(h, 0, chain),
            storage.clone(),
        );
        let mut update = Update::new(storage);
        update
            .process_begin_block_with_handlers(&ctx, &HANDLERS)
            .unwrap();
        assert_eq!(update.get_active_version().unwrap(), PV);
        assert_eq!(
            update
                .read_scheduled_update(id)
                .unwrap()
                .unwrap()
                .activation_height,
            h
        );
        assert_eq!(
            update.read_scheduled_update(id).unwrap().unwrap().status,
            ScheduledUpdateStatus::Activated
        );
    });
}
