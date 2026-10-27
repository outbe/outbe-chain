use super::*;
use crate::handlers::{ProposalAction, ProposalLifecycle, VotePlanContext};
use crate::state::ProposalSubmission;
use crate::windows::{ProposalKind, ProposalPhase};
use alloy_sol_types::SolEvent;

struct WindowTarget;
static WINDOW_TARGET: WindowTarget = WindowTarget;
static WINDOW_HANDLERS: &[&dyn VoteTarget] = &[&WINDOW_TARGET];
static WINDOW_REGISTRY: VoteTargetRegistry = VoteTargetRegistry::new(WINDOW_HANDLERS);
const C: u64 = 100;
const Q: u64 = 105;
const T: u64 = 109;
const H: u64 = 112;

fn marker(storage: &StorageHandle<'_>, slot: u64) -> U256 {
    storage.sload(UPDATE_ADDRESS, U256::from(slot)).unwrap()
}
impl VoteTarget for WindowTarget {
    fn target_module(&self) -> Address {
        UPDATE_ADDRESS
    }
    fn lifecycle(&self) -> ProposalLifecycle {
        ProposalLifecycle::ThreeWindow {
            voting_window: 4,
            replace_window: 4,
            activation_window: 3,
        }
    }
    fn proposal_action(
        &self,
        payload: &[u8],
        context: VoteTargetContext,
    ) -> Result<ProposalAction> {
        self.validate(payload, context)?;
        let value = crate::handlers::decode_proposal_payload(payload)?;
        if let Some(id) = value.get("cancel").and_then(Value::as_u64) {
            return Ok(ProposalAction::Cancel(U256::from(id)));
        }
        if let Some(id) = value.get("replace").and_then(Value::as_u64) {
            return Ok(ProposalAction::Replace(U256::from(id)));
        }
        Ok(ProposalAction::Primary)
    }
    fn validate(&self, payload: &[u8], _: VoteTargetContext) -> Result<()> {
        require_json_object(payload, || VoteError::InvalidPayload.into())
    }
    fn reserve(
        &self,
        storage: StorageHandle<'_>,
        _: U256,
        payload: &[u8],
        _: VoteTargetContext,
    ) -> Result<()> {
        if serde_json::from_slice::<Value>(payload).unwrap()["reserveError"] == true {
            storage.sstore(UPDATE_ADDRESS, U256::from(106), U256::from(1))?;
            return Err(PrecompileError::Revert("reservation refused".into()));
        }
        Ok(())
    }
    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        id: U256,
        _: &[u8],
        _: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        ctx.storage.sstore(UPDATE_ADDRESS, U256::from(110), id)?;
        Ok(TargetExecutionOutcome::Applied)
    }
    fn register_plan(
        &self,
        ctx: &BlockRuntimeContext,
        id: U256,
        payload: &[u8],
        _: VoteTargetContext,
        plan: VotePlanContext,
    ) -> Result<TargetExecutionOutcome> {
        ctx.storage.sstore(UPDATE_ADDRESS, U256::from(101), id)?;
        ctx.storage.sstore(
            UPDATE_ADDRESS,
            U256::from(102),
            U256::from(plan.activation_height),
        )?;
        ctx.storage.sstore(
            UPDATE_ADDRESS,
            U256::from(103),
            U256::from(plan.replace_end),
        )?;
        let value: Value = serde_json::from_slice(payload).unwrap();
        if value["fatal"] == true {
            return Err(PrecompileError::Fatal("target provider failed".into()));
        }
        if value["error"] == true {
            return Ok(TargetExecutionOutcome::Error {
                reason: "plan refused".into(),
            });
        }
        Ok(TargetExecutionOutcome::Applied)
    }
    fn cancel_plan(
        &self,
        ctx: &BlockRuntimeContext,
        parent: U256,
        by: U256,
    ) -> Result<TargetExecutionOutcome> {
        assert_eq!(marker(&ctx.storage, 101), parent);
        ctx.storage
            .sstore(UPDATE_ADDRESS, U256::from(101), U256::ZERO)?;
        ctx.storage.sstore(UPDATE_ADDRESS, U256::from(104), by)?;
        Ok(TargetExecutionOutcome::Applied)
    }
    fn prepare_plan(&self, ctx: &BlockRuntimeContext, id: U256, activation: u64) -> Result<()> {
        assert!(ctx.block.block_number >= activation - 3);
        ctx.storage.sstore(UPDATE_ADDRESS, U256::from(105), id)?;
        if marker(&ctx.storage, 109) != U256::ZERO {
            return Err(PrecompileError::Fatal("prepare failed".into()));
        }
        Ok(())
    }
    fn existing_plan(
        &self,
        storage: StorageHandle<'_>,
        _: &crate::schema::ProposalRecord,
    ) -> Result<Option<VotePlanContext>> {
        let activation_height = marker(&storage, 108).to::<u64>();
        Ok((activation_height != 0).then(|| VotePlanContext {
            replace_end: marker(&storage, 107).to::<u64>(),
            activation_height,
        }))
    }
}
fn create(vote: &mut Vote<'_>, proposer: Address, payload: &str, height: u64) -> U256 {
    vote.create_proposal(proposer, UPDATE_ADDRESS, payload, height, &WINDOW_REGISTRY)
        .unwrap()
}
fn approve(vote: &mut Vote<'_>, id: U256, height: u64) {
    for voter in [PROPOSER, VOTER_A] {
        vote.cast_vote_approve(id, voter, true, height).unwrap();
    }
}
fn accepted(vote: &mut Vote<'_>) -> U256 {
    let id = create(vote, PROPOSER, "{}", C);
    approve(vote, id, C + 4);
    vote.begin_block_with(Q, &WINDOW_REGISTRY).unwrap();
    id
}
fn linked(parent: U256, cancel: bool) -> String {
    if cancel {
        serde_json::json!({"cancel": parent.to::<u64>()}).to_string()
    } else {
        serde_json::json!({"replace": parent.to::<u64>()}).to_string()
    }
}

#[test]
fn primary_deadline_is_inclusive_and_preparation_waits_until_t() {
    let mut provider = test_provider();
    let id;
    {
        let (storage, mut vote) = validator_vote(&mut provider);
        id = create(&mut vote, PROPOSER, "{}", C);
        assert_eq!(
            vote.get_proposal(id)
                .unwrap()
                .unwrap()
                .voting_deadline_height,
            C + 4
        );
        approve(&mut vote, id, C + 4);
        vote.begin_block_with(C + 4, &WINDOW_REGISTRY).unwrap();
        assert_eq!(proposal_status(&vote, id), ProposalStatus::Pending);
        assert_eq!(marker(&storage, 101), U256::ZERO);
        vote.begin_block_with(Q, &WINDOW_REGISTRY).unwrap();
        assert_eq!(proposal_status(&vote, id), ProposalStatus::Approved);
        assert_eq!(
            vote.proposal_lifecycle(id).unwrap().phase,
            ProposalPhase::Replace
        );
        assert_eq!(marker(&storage, 102), U256::from(H));
        assert_eq!(marker(&storage, 105), U256::ZERO);
        vote.begin_block_with(T - 1, &WINDOW_REGISTRY).unwrap();
        assert_eq!(marker(&storage, 105), U256::ZERO);
        vote.begin_block_with(T, &WINDOW_REGISTRY).unwrap();
        assert_eq!(marker(&storage, 105), id);
        assert_eq!(
            vote.proposal_lifecycle(id).unwrap().phase,
            ProposalPhase::Activation
        );
        vote.begin_block_with(H, &WINDOW_REGISTRY).unwrap();
        assert_eq!(
            vote.proposal_lifecycle(id).unwrap().phase,
            ProposalPhase::Activation
        );
        assert!(vote.complete_plan(id, VOTE_ADDRESS, false).is_err());
        vote.complete_plan(id, UPDATE_ADDRESS, false).unwrap();
        assert_eq!(
            vote.proposal_lifecycle(id).unwrap().phase,
            ProposalPhase::Finished
        );
        assert!(vote.active_plan_ids.read_all().unwrap().is_empty());
        assert!(vote.complete_plan(id, UPDATE_ADDRESS, false).is_err());
    }
    assert_eq!(
        count_events(&provider, IVote::ProposalPhaseChanged::SIGNATURE_HASH),
        4
    );
}

#[test]
fn primary_no_quorum_expires_without_registering_a_plan() {
    with_governance(|storage, vote| {
        let id = create(vote, PROPOSER, "{}", C);
        vote.begin_block_with(Q, &WINDOW_REGISTRY).unwrap();
        assert_eq!(proposal_status(vote, id), ProposalStatus::Expired);
        assert_eq!(
            vote.proposal_lifecycle(id).unwrap().phase,
            ProposalPhase::Finished
        );
        assert_eq!(marker(&storage, 101), U256::ZERO);
        assert!(vote.active_plan_ids.read_all().unwrap().is_empty());
    });
}

#[test]
fn late_linked_cancel_has_exact_deadline_and_does_not_pause_parent() {
    with_governance(|storage, vote| {
        let parent = accepted(vote);
        let cancel = create(vote, PROPOSER, &linked(parent, true), T - 1);
        assert_eq!(
            vote.get_proposal(cancel)
                .unwrap()
                .unwrap()
                .voting_deadline_height,
            T - 1
        );
        approve(vote, cancel, T - 1);
        vote.begin_block_with(T - 1, &WINDOW_REGISTRY).unwrap();
        assert_eq!(proposal_status(vote, parent), ProposalStatus::Approved);
        assert_eq!(proposal_status(vote, cancel), ProposalStatus::Pending);
        assert_eq!(
            vote.proposal_lifecycle(parent).unwrap().activation_height,
            H
        );
        vote.begin_block_with(T, &WINDOW_REGISTRY).unwrap();
        assert_eq!(proposal_status(vote, parent), ProposalStatus::Canceled);
        assert_eq!(proposal_status(vote, cancel), ProposalStatus::Approved);
        assert_eq!(marker(&storage, 101), U256::ZERO);
        assert_eq!(marker(&storage, 105), U256::ZERO);
        assert!(vote.active_plan_ids.read_all().unwrap().is_empty());
        assert!(vote.list_pending_proposal_ids().unwrap().is_empty());
    });
}

#[test]
fn minimum_quorum_id_wins_mixed_competitors_despite_pending_order() {
    with_governance(|storage, vote| {
        let parent = accepted(vote);
        let fourth = Address::repeat_byte(0x55);
        super::register_active_validator(storage.clone(), fourth, 4);
        let lower = create(vote, PROPOSER, &linked(parent, true), Q);
        let winner = create(vote, VOTER_A, &linked(parent, false), Q);
        let higher = create(vote, VOTER_B, &linked(parent, true), Q);
        let higher_no_quorum = create(vote, fourth, &linked(parent, false), Q);
        approve(vote, winner, Q);
        vote.cast_vote_approve(winner, VOTER_B, true, Q).unwrap();
        approve(vote, higher, Q);
        vote.cast_vote_approve(higher, VOTER_B, true, Q).unwrap();
        vote.cast_vote_approve(higher, fourth, true, Q).unwrap();
        vote.pending_proposal_ids.set(0, higher).unwrap();
        vote.pending_proposal_ids.set(2, lower).unwrap();
        vote.begin_block_with(T, &WINDOW_REGISTRY).unwrap();
        assert_eq!(proposal_status(vote, lower), ProposalStatus::Expired);
        assert_eq!(proposal_status(vote, winner), ProposalStatus::Approved);
        assert_eq!(proposal_status(vote, higher), ProposalStatus::Canceled);
        assert_eq!(
            proposal_status(vote, higher_no_quorum),
            ProposalStatus::Canceled
        );
        assert_eq!(proposal_status(vote, parent), ProposalStatus::Canceled);
        let info = vote.proposal_lifecycle(winner).unwrap();
        assert_eq!(info.kind, ProposalKind::Replacement);
        assert_eq!((info.replace_end, info.activation_height), (T + 4, H + 4));
        assert_eq!(marker(&storage, 101), winner);
        assert_eq!(marker(&storage, 105), U256::ZERO);
        let second = create(vote, PROPOSER, &linked(winner, false), T + 3);
        approve(vote, second, T + 3);
        vote.cast_vote_approve(second, VOTER_B, true, T + 3)
            .unwrap();
        vote.begin_block_with(T + 4, &WINDOW_REGISTRY).unwrap();
        assert_eq!(proposal_status(vote, winner), ProposalStatus::Canceled);
        assert_eq!(marker(&storage, 101), second);
        assert_eq!(vote.proposal_lifecycle(second).unwrap().replace_end, T + 8);
        vote.begin_block_with(T + 8, &WINDOW_REGISTRY).unwrap();
        assert_eq!(marker(&storage, 105), second);
    });
}

#[test]
fn no_linked_quorum_expires_all_and_prepares_parent() {
    with_governance(|storage, vote| {
        let parent = accepted(vote);
        let cancel = create(vote, PROPOSER, &linked(parent, true), T - 1);
        let replace = create(vote, VOTER_A, &linked(parent, false), T - 1);
        vote.cast_vote_approve(cancel, VOTER_A, true, T - 1)
            .unwrap();
        vote.begin_block_with(T, &WINDOW_REGISTRY).unwrap();
        for id in [cancel, replace] {
            assert_eq!(proposal_status(vote, id), ProposalStatus::Expired);
        }
        assert_eq!(proposal_status(vote, parent), ProposalStatus::Approved);
        assert_eq!(marker(&storage, 105), parent);
    });
}

#[test]
fn linked_admission_rejects_missing_wrong_target_pending_and_closed_parent() {
    with_governance(|_, vote| {
        let unknown = U256::from(999);
        assert!(vote
            .create_proposal(
                PROPOSER,
                UPDATE_ADDRESS,
                &linked(unknown, true),
                C,
                &WINDOW_REGISTRY
            )
            .is_err());
        let pending = create(vote, PROPOSER, "{}", C);
        assert!(vote
            .create_proposal(
                PROPOSER,
                UPDATE_ADDRESS,
                &linked(pending, true),
                C,
                &WINDOW_REGISTRY
            )
            .is_err());
        let other = vote
            .write_proposal(
                &ProposalSubmission::new(VOTER_A, VOTE_ADDRESS, "{}", C),
                C + 4,
                ProposalStatus::Approved,
            )
            .unwrap();
        assert!(vote
            .create_proposal(
                PROPOSER,
                UPDATE_ADDRESS,
                &linked(other, true),
                C,
                &WINDOW_REGISTRY
            )
            .is_err());
        approve(vote, pending, C);
        vote.begin_block_with(Q, &WINDOW_REGISTRY).unwrap();
        assert!(vote
            .create_proposal(
                PROPOSER,
                UPDATE_ADDRESS,
                &linked(pending, true),
                T,
                &WINDOW_REGISTRY
            )
            .is_err());
        assert!(vote
            .create_proposal_with_voting_window(
                ProposalSubmission::new(PROPOSER, UPDATE_ADDRESS, "{}", C),
                1,
                &WINDOW_REGISTRY
            )
            .is_err());
        let unadoptable = vote
            .write_proposal(
                &ProposalSubmission::new(VOTER_B, UPDATE_ADDRESS, "{}", C),
                77,
                ProposalStatus::Approved,
            )
            .unwrap();
        assert!(vote
            .create_proposal(
                PROPOSER,
                UPDATE_ADDRESS,
                &linked(unadoptable, true),
                Q,
                &WINDOW_REGISTRY
            )
            .is_err());
        assert!(vote
            .create_proposal_with_value(
                ProposalSubmission::new(PROPOSER, UPDATE_ADDRESS, "{}", C)
                    .with_attached_value(U256::from(1)),
                &WINDOW_REGISTRY
            )
            .is_err());
    });
}

#[test]
fn selected_target_domain_error_keeps_parent_and_never_falls_back() {
    with_governance(|storage, vote| {
        let parent = accepted(vote);
        let payload = serde_json::json!({"replace":parent.to::<u64>(), "error":true}).to_string();
        let first = create(vote, PROPOSER, &payload, Q);
        let second = create(vote, VOTER_A, &linked(parent, true), Q);
        approve(vote, first, Q);
        approve(vote, second, Q);
        vote.begin_block_with(T, &WINDOW_REGISTRY).unwrap();
        assert_eq!(proposal_status(vote, first), ProposalStatus::Error);
        assert_eq!(proposal_status(vote, second), ProposalStatus::Canceled);
        assert_eq!(proposal_status(vote, parent), ProposalStatus::Approved);
        assert_eq!(marker(&storage, 101), parent);
        assert_eq!(marker(&storage, 104), U256::ZERO);
        assert_eq!(marker(&storage, 105), parent);
    });
}

#[test]
fn fatal_target_or_prepare_error_rolls_back_the_entire_window() {
    for failure in ["register", "prepare"] {
        with_governance(|storage, vote| {
            let parent = accepted(vote);
            let payload =
                serde_json::json!({"replace":parent.to::<u64>(), "fatal":failure == "register"})
                    .to_string();
            let child = create(vote, PROPOSER, &payload, Q);
            if failure == "register" {
                approve(vote, child, Q);
            } else {
                storage
                    .sstore(UPDATE_ADDRESS, U256::from(109), U256::from(1))
                    .unwrap();
            }
            assert!(matches!(
                vote.begin_block_with(T, &WINDOW_REGISTRY),
                Err(PrecompileError::Fatal(_))
            ));
            assert_eq!(proposal_status(vote, parent), ProposalStatus::Approved);
            assert_eq!(proposal_status(vote, child), ProposalStatus::Pending);
            assert_eq!(
                vote.proposal_lifecycle(parent).unwrap().phase,
                ProposalPhase::Replace
            );
            assert_eq!(marker(&storage, 101), parent);
            assert_eq!(marker(&storage, 104), U256::ZERO);
            assert_eq!(marker(&storage, 105), U256::ZERO);
        });
    }
}

#[test]
fn lifecycle_metadata_corruption_fails_fatally_before_target_effects() {
    for field in ["kind", "phase", "height", "window", "parent"] {
        with_governance(|storage, vote| {
            let parent = accepted(vote);
            let mut record = vote.proposal_windows.get(parent).unwrap().unwrap();
            match field {
                "kind" => record.kind = 99,
                "phase" => record.phase = 99,
                "height" => record.activation_height += 1,
                "window" => record.replace_window = 0,
                _ => record.parent_id = parent,
            }
            vote.proposal_windows.update(&record).unwrap();
            assert!(matches!(
                vote.begin_block_with(T, &WINDOW_REGISTRY),
                Err(PrecompileError::Fatal(_))
            ));
            assert_eq!(marker(&storage, 105), U256::ZERO);
        });
    }
    with_governance(|_, vote| {
        let parent = accepted(vote);
        vote.active_plan_ids.push(parent).unwrap();
        assert!(matches!(
            vote.begin_block_with(T, &WINDOW_REGISTRY),
            Err(PrecompileError::Fatal(_))
        ));
    });
}

fn legacy(vote: &mut Vote<'_>, storage: &StorageHandle<'_>) -> U256 {
    let id = vote
        .write_proposal(
            &ProposalSubmission::new(PROPOSER, UPDATE_ADDRESS, "{}", 10),
            77,
            ProposalStatus::Approved,
        )
        .unwrap();
    storage.sstore(UPDATE_ADDRESS, U256::from(101), id).unwrap();
    storage
        .sstore(UPDATE_ADDRESS, U256::from(107), U256::from(T))
        .unwrap();
    storage
        .sstore(UPDATE_ADDRESS, U256::from(108), U256::from(H))
        .unwrap();
    id
}

#[test]
fn legacy_adoption_preserves_h_and_deadline_and_rolls_back_on_reservation_failure() {
    with_governance(|storage, vote| {
        let parent = legacy(vote, &storage);
        let bad = serde_json::json!({"cancel":parent.to::<u64>(), "reserveError":true}).to_string();
        assert!(vote
            .create_proposal(PROPOSER, UPDATE_ADDRESS, &bad, T - 1, &WINDOW_REGISTRY)
            .is_err());
        assert!(vote.proposal_windows.get(parent).unwrap().is_none());
        assert!(vote.active_plan_ids.read_all().unwrap().is_empty());
        assert_eq!(marker(&storage, 106), U256::ZERO);
        assert_eq!(vote.proposal_count.read().unwrap(), parent);
        let child = create(vote, PROPOSER, &linked(parent, true), T - 1);
        let info = vote.proposal_lifecycle(parent).unwrap();
        assert_eq!(info.kind, ProposalKind::LegacyPlan);
        assert_eq!((info.replace_end, info.activation_height), (T, H));
        assert_eq!(
            vote.get_proposal(parent)
                .unwrap()
                .unwrap()
                .voting_deadline_height,
            77
        );
        vote.begin_block_with(T, &WINDOW_REGISTRY).unwrap();
        assert_eq!(proposal_status(vote, child), ProposalStatus::Expired);
        assert_eq!(marker(&storage, 105), parent);
    });
}

#[test]
fn legacy_pending_ballots_keep_original_deadline_and_dispatch() {
    with_governance(|storage, vote| {
        let id = vote
            .write_proposal(
                &ProposalSubmission::new(PROPOSER, UPDATE_ADDRESS, "{}", C),
                T,
                ProposalStatus::Pending,
            )
            .unwrap();
        approve(vote, id, T);
        vote.begin_block_with(T, &WINDOW_REGISTRY).unwrap();
        assert_eq!(proposal_status(vote, id), ProposalStatus::Pending);
        vote.begin_block_with(T + 1, &WINDOW_REGISTRY).unwrap();
        assert_eq!(proposal_status(vote, id), ProposalStatus::Approved);
        assert_eq!(marker(&storage, 110), id);
        assert_eq!(marker(&storage, 101), U256::ZERO);
        vote.complete_plan(id, UPDATE_ADDRESS, false).unwrap();
        assert_eq!(
            vote.proposal_lifecycle(id).unwrap().kind,
            ProposalKind::Standard
        );
    });
}

fn replacement_fixture() -> (HashMapStorageProvider, U256, U256) {
    let mut provider = test_provider();
    let (parent, child);
    {
        let (_, mut vote) = validator_vote(&mut provider);
        parent = accepted(&mut vote);
        child = create(&mut vote, PROPOSER, &linked(parent, false), T - 1);
        approve(&mut vote, child, T - 1);
    }
    provider.clear_mutation_failure();
    (provider, parent, child)
}
#[test]
fn every_replacement_window_mutation_failure_rolls_back_state_and_logs() {
    let (mut baseline, _, _) = replacement_fixture();
    {
        let storage = StorageHandle::new(&mut baseline);
        Vote::new(storage)
            .begin_block_with(T, &WINDOW_REGISTRY)
            .unwrap();
    }
    let mutations = baseline.clear_mutation_failure();
    assert!(mutations > 0);
    for point in 0..mutations {
        let (mut provider, parent, child) = replacement_fixture();
        let logs = provider.get_events(VOTE_ADDRESS).len();
        provider.fail_after_mutation_at(point);
        {
            let storage = StorageHandle::new(&mut provider);
            let mut vote = Vote::new(storage);
            assert!(
                matches!(
                    vote.begin_block_with(T, &WINDOW_REGISTRY),
                    Err(PrecompileError::Storage(_))
                ),
                "mutation {point}"
            );
        }
        provider.clear_mutation_failure();
        {
            let storage = StorageHandle::new(&mut provider);
            let vote = Vote::new(storage.clone());
            assert_eq!(proposal_status(&vote, parent), ProposalStatus::Approved);
            assert_eq!(proposal_status(&vote, child), ProposalStatus::Pending);
            assert_eq!(vote.active_plan_ids.read_all().unwrap(), vec![parent]);
            assert_eq!(vote.list_pending_proposal_ids().unwrap(), vec![child]);
            assert_eq!(
                vote.proposal_lifecycle(parent).unwrap().phase,
                ProposalPhase::Replace
            );
            assert_eq!(marker(&storage, 101), parent);
            assert_eq!(marker(&storage, 104), U256::ZERO);
        }
        assert_eq!(
            provider.get_events(VOTE_ADDRESS).len(),
            logs,
            "mutation {point}"
        );
    }
}

#[test]
fn lifecycle_abi_view_and_cancellation_events_report_real_transitions() {
    use alloy_sol_types::SolCall;
    let mut provider = test_provider();
    let (parent, child);
    {
        let (storage, mut vote) = validator_vote(&mut provider);
        parent = accepted(&mut vote);
        child = create(&mut vote, PROPOSER, &linked(parent, true), T - 1);
        approve(&mut vote, child, T - 1);
        vote.begin_block_with(T, &WINDOW_REGISTRY).unwrap();
        let call = IVote::getProposalLifecycleCall { proposalId: parent }.abi_encode();
        let bytes = crate::precompile::dispatch_with_handlers(
            storage,
            &call,
            PROPOSER,
            U256::ZERO,
            &WINDOW_REGISTRY,
        )
        .unwrap();
        let view = IVote::getProposalLifecycleCall::abi_decode_returns(&bytes).unwrap();
        assert_eq!(view.kind, IVote::ProposalKind::Primary);
        assert_eq!(view.phase, IVote::ProposalPhase::Finished);
        assert_eq!((view.replaceEnd, view.activationHeight), (T, H));
    }
    let logs = provider.get_events(VOTE_ADDRESS);
    let cancellations: Vec<_> = logs
        .iter()
        .filter(|log| log.topics().first() == Some(&IVote::ProposalCanceled::SIGNATURE_HASH))
        .collect();
    assert_eq!(cancellations.len(), 1);
    let event = IVote::ProposalCanceled::decode_log_data(cancellations[0]).unwrap();
    assert_eq!(event.proposalId, parent);
    assert_eq!(event.byProposalId, child);
}

#[test]
fn automatic_plan_cancellation_finishes_pending_children_atomically() {
    with_governance(|storage, vote| {
        let parent = accepted(vote);
        let child = create(vote, PROPOSER, &linked(parent, true), Q);
        vote.complete_plan(parent, UPDATE_ADDRESS, true).unwrap();
        for id in [parent, child] {
            assert_eq!(proposal_status(vote, id), ProposalStatus::Canceled);
            assert_eq!(
                vote.proposal_lifecycle(id).unwrap().phase,
                ProposalPhase::Finished
            );
        }
        assert!(vote.active_plan_ids.read_all().unwrap().is_empty());
        assert!(vote.list_pending_proposal_ids().unwrap().is_empty());
        // Target completion only closes Vote's state; the target has already
        // handled its own effects and must not recursively call cancel_plan.
        assert_eq!(marker(&storage, 104), U256::ZERO);
    });
}

fn legacy_admission_fixture() -> (HashMapStorageProvider, U256) {
    let mut provider = test_provider();
    let parent;
    {
        let (storage, mut vote) = validator_vote(&mut provider);
        parent = legacy(&mut vote, &storage);
    }
    provider.clear_mutation_failure();
    (provider, parent)
}

#[test]
fn every_legacy_adoption_admission_mutation_failure_rolls_back_state_and_logs() {
    let (mut baseline, parent) = legacy_admission_fixture();
    {
        let storage = StorageHandle::new(&mut baseline);
        create(
            &mut Vote::new(storage),
            PROPOSER,
            &linked(parent, true),
            T - 1,
        );
    }
    let mutations = baseline.clear_mutation_failure();
    assert!(mutations > 0);
    for point in 0..mutations {
        let (mut provider, parent) = legacy_admission_fixture();
        let logs = provider.get_events(VOTE_ADDRESS).len();
        provider.fail_after_mutation_at(point);
        {
            let storage = StorageHandle::new(&mut provider);
            let mut vote = Vote::new(storage);
            assert!(
                matches!(
                    vote.create_proposal(
                        PROPOSER,
                        UPDATE_ADDRESS,
                        &linked(parent, true),
                        T - 1,
                        &WINDOW_REGISTRY
                    ),
                    Err(PrecompileError::Storage(_))
                ),
                "mutation {point}"
            );
        }
        provider.clear_mutation_failure();
        {
            let storage = StorageHandle::new(&mut provider);
            let vote = Vote::new(storage.clone());
            assert_eq!(vote.proposal_count.read().unwrap(), parent);
            assert!(vote.proposal_windows.get(parent).unwrap().is_none());
            assert!(vote.active_plan_ids.read_all().unwrap().is_empty());
            assert!(vote.list_pending_proposal_ids().unwrap().is_empty());
            assert_eq!(
                vote.get_proposal(parent)
                    .unwrap()
                    .unwrap()
                    .voting_deadline_height,
                77
            );
            assert_eq!(marker(&storage, 108), U256::from(H));
        }
        assert_eq!(
            provider.get_events(VOTE_ADDRESS).len(),
            logs,
            "mutation {point}"
        );
    }
}

#[test]
fn legacy_completion_without_a_vote_record_is_unchanged() {
    with_governance(|_, vote| {
        let missing = U256::from(999);
        assert!(vote.proposals.get(missing).unwrap().is_none());
        vote.complete_plan(missing, UPDATE_ADDRESS, false).unwrap();
        vote.complete_plan(missing, UPDATE_ADDRESS, true).unwrap();
        assert_eq!(vote.proposal_count.read().unwrap(), U256::ZERO);
        assert!(vote.active_plan_ids.read_all().unwrap().is_empty());
    });
}

#[test]
fn active_plan_swap_remove_updates_the_moved_reverse_position() {
    with_governance(|_, vote| {
        let first = create(vote, PROPOSER, "{}", C);
        let second = create(vote, VOTER_A, "{}", C);
        approve(vote, first, C);
        approve(vote, second, C);
        vote.begin_block_with(Q, &WINDOW_REGISTRY).unwrap();
        assert_eq!(vote.active_plan_positions.read(&first).unwrap(), 1);
        assert_eq!(vote.active_plan_positions.read(&second).unwrap(), 2);
        vote.complete_plan(first, UPDATE_ADDRESS, true).unwrap();
        assert_eq!(vote.active_plan_ids.read_all().unwrap(), vec![second]);
        assert_eq!(vote.active_plan_positions.read(&first).unwrap(), 0);
        assert_eq!(vote.active_plan_positions.read(&second).unwrap(), 1);
        vote.validate_active_plan_index().unwrap();
        vote.complete_plan(second, UPDATE_ADDRESS, true).unwrap();
        assert_eq!(vote.active_plan_positions.read(&second).unwrap(), 0);
        assert!(vote.active_plan_ids.read_all().unwrap().is_empty());
    });
}

#[test]
fn active_plan_index_corruption_fails_before_any_plan_effects() {
    for corruption in [
        "missing-position",
        "out-of-range",
        "wrong-id",
        "missing-metadata",
    ] {
        with_governance(|storage, vote| {
            let parent = accepted(vote);
            match corruption {
                "missing-position" => vote.active_plan_positions.clear(&parent).unwrap(),
                "out-of-range" => vote.active_plan_positions.write(&parent, 2).unwrap(),
                "wrong-id" => vote.active_plan_ids.set(0, U256::from(999)).unwrap(),
                _ => vote.proposal_windows.delete(parent).unwrap(),
            }
            assert!(matches!(
                vote.begin_block_with(T, &WINDOW_REGISTRY),
                Err(PrecompileError::Fatal(_))
            ));
            assert_eq!(marker(&storage, 105), U256::ZERO);
            assert_eq!(marker(&storage, 101), parent);
        });
    }
}

#[test]
fn full_plan_capacity_keeps_cancellation_available_and_primary_refusal_terminal() {
    for cancel in [true, false] {
        with_governance(|storage, vote| {
            for index in 0..crate::constants::MAX_PENDING_PROPOSALS {
                let id = vote
                    .write_proposal(
                        &ProposalSubmission::new(
                            Address::with_last_byte(index as u8 + 20),
                            UPDATE_ADDRESS,
                            "{}",
                            C,
                        ),
                        Q - 1,
                        ProposalStatus::Approved,
                    )
                    .unwrap();
                vote.proposal_windows
                    .create(&crate::schema::ProposalWindowRecord {
                        id,
                        marker: 1,
                        kind: ProposalKind::Primary as u8,
                        phase: ProposalPhase::Replace as u8,
                        parent_id: U256::ZERO,
                        voting_window: 4,
                        replace_window: 4,
                        activation_window: 3,
                        replace_end: T,
                        activation_height: H,
                    })
                    .unwrap();
                vote.active_plan_ids.push(id).unwrap();
                vote.active_plan_positions.write(&id, index + 1).unwrap();
            }
            let payload = if cancel {
                storage
                    .sstore(UPDATE_ADDRESS, U256::from(101), U256::ONE)
                    .unwrap();
                linked(U256::ONE, true)
            } else {
                "{}".into()
            };
            let candidate = create(vote, PROPOSER, &payload, Q);
            approve(vote, candidate, Q);
            vote.begin_block_with(T, &WINDOW_REGISTRY).unwrap();
            if cancel {
                assert_eq!(proposal_status(vote, U256::ONE), ProposalStatus::Canceled);
                assert_eq!(proposal_status(vote, candidate), ProposalStatus::Approved);
                assert_eq!(
                    vote.active_plan_ids.len().unwrap(),
                    crate::constants::MAX_PENDING_PROPOSALS - 1
                );
            } else {
                vote.begin_block_with(Q + 5, &WINDOW_REGISTRY).unwrap();
                assert_eq!(proposal_status(vote, candidate), ProposalStatus::Error);
                assert_eq!(
                    marker(&storage, 101),
                    U256::ZERO,
                    "full capacity must refuse before target writes"
                );
                assert_eq!(
                    vote.active_plan_ids.len().unwrap(),
                    crate::constants::MAX_PENDING_PROPOSALS
                );
            }
            assert!(vote.list_pending_proposal_ids().unwrap().is_empty());
            vote.validate_active_plan_index().unwrap();
        });
    }
}

#[test]
fn stored_action_kind_cannot_turn_a_voted_replacement_into_cancellation() {
    with_governance(|storage, vote| {
        let parent = accepted(vote);
        let child = create(vote, VOTER_A, &linked(parent, false), Q);
        approve(vote, child, Q);
        let mut window = vote.proposal_windows.get(child).unwrap().unwrap();
        window.kind = ProposalKind::Cancel as u8;
        vote.proposal_windows.update(&window).unwrap();
        assert!(matches!(
            vote.begin_block_with(T, &WINDOW_REGISTRY),
            Err(PrecompileError::Fatal(_))
        ));
        assert_eq!(proposal_status(vote, parent), ProposalStatus::Approved);
        assert_eq!(proposal_status(vote, child), ProposalStatus::Pending);
        assert_eq!(marker(&storage, 101), parent);
        assert_eq!(
            vote.proposal_lifecycle(parent).unwrap().phase,
            ProposalPhase::Replace
        );
    });
}
