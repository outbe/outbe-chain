use super::tee_fixture::{initial_tee_policy, successor_tee_policy};
use super::TEST_CHAIN_ID;
use alloy_primitives::{address, B256};
use alloy_sol_types::SolEvent;
use outbe_ocompregistry::{poc_schema_limits, OcompRegistry};
use outbe_primitives::addresses::UPDATE_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::PrecompileError;

use outbe_teeregistry::TeeRegistry;
use outbe_vote::constants::VOTING_WINDOW_BLOCKS;
use outbe_vote::handlers::{
    TargetExecutionOutcome, VoteTarget, VoteTargetContext, VoteTargetRegistry,
};
use outbe_vote::schema::ProposalStatus;
use outbe_vote::schema::Vote;

use crate::handlers::UpgradeHandlerRegistry;
use crate::payload::{encode_schedule_update_json, ScheduleUpdatePayload};
use crate::precompile::IUpdate;
use crate::schema::Update;
use crate::tests::{block_ctx, min_activation, ocomp_authority, ocomp_successor, PV};
use crate::vote_target::UpdateVoteTarget;

static UPDATE_VOTE_TARGET: UpdateVoteTarget = UpdateVoteTarget;
static VOTE_HANDLERS: &[&dyn VoteTarget] = &[&UPDATE_VOTE_TARGET];
static VOTE_TARGET_REGISTRY: VoteTargetRegistry = VoteTargetRegistry::new(VOTE_HANDLERS);

static EMPTY_UPGRADE_HANDLER_REGISTRY: UpgradeHandlerRegistry = UpgradeHandlerRegistry::new(&[]);

const PROPOSER: alloy_primitives::Address = address!("0x1111111111111111111111111111111111111111");
const VOTER_A: alloy_primitives::Address = address!("0x2222222222222222222222222222222222222222");
const VOTER_B: alloy_primitives::Address = address!("0x3333333333333333333333333333333333333333");
const UNKNOWN_TARGET: alloy_primitives::Address =
    address!("0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddead");

fn empty_update_payload(current_height: u64) -> String {
    encode_schedule_update_json(
        PV,
        min_activation(current_height.saturating_add(VOTING_WINDOW_BLOCKS)),
        "",
    )
}

fn with_vote<F: FnOnce(outbe_primitives::storage::StorageHandle)>(f: F) {
    let mut provider =
        outbe_primitives::storage::hashmap::HashMapStorageProvider::new_with_chain_identity(
            TEST_CHAIN_ID,
            B256::repeat_byte(0x01),
        );
    provider.set_block_number(1);
    let storage = outbe_primitives::storage::StorageHandle::new(&mut provider);
    setup_validators(storage.clone());
    f(storage);
}

fn setup_validators(storage: outbe_primitives::storage::StorageHandle) {
    let owner = address!("0xffffffffffffffffffffffffffffffffffffffff");
    for (addr, seed) in [(PROPOSER, 1u8), (VOTER_A, 2), (VOTER_B, 3)] {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(owner).unwrap();
        vs.set_config_max_validators(100).unwrap();
        let mut pk = [0u8; 48];
        pk[0] = seed;
        vs.register_validator(owner, addr, &pk).unwrap();
        vs.activate_validator_via_boundary_for_test(addr).unwrap();
    }
}

fn process_begin_block_test(storage: outbe_primitives::storage::StorageHandle, block_number: u64) {
    let ctx = block_ctx(storage.clone(), block_number);
    Vote::new(storage)
        .process_begin_block(&ctx, &VOTE_TARGET_REGISTRY)
        .unwrap();
}

fn create_approved_update(
    vote: &mut Vote<'_>,
    payload: &str,
    current_height: u64,
) -> alloy_primitives::U256 {
    let proposal = vote
        .create_proposal(
            PROPOSER,
            UPDATE_ADDRESS,
            payload,
            current_height,
            &VOTE_TARGET_REGISTRY,
        )
        .unwrap();
    vote.cast_vote_approve(proposal, VOTER_A, true, current_height + 1)
        .unwrap();
    vote.cast_vote_approve(proposal, VOTER_B, true, current_height + 2)
        .unwrap();
    proposal
}

#[test]
fn approved_vote_proposal_schedules_update_and_activates() {
    with_vote(|storage| {
        let mut governance = Vote::new(storage.clone());
        let current = 100u64;
        let deadline = current + VOTING_WINDOW_BLOCKS + 1;
        let activation = min_activation(deadline);
        let payload = encode_schedule_update_json(PV, activation, "notes");
        let proposal_id = create_approved_update(&mut governance, &payload, current);

        process_begin_block_test(storage.clone(), deadline);

        let record = governance.proposals.get(proposal_id).unwrap().unwrap();
        assert_eq!(record.proposal_status().unwrap(), ProposalStatus::Approved);

        let mut update = Update::new(storage.clone());
        let scheduled = update.read_scheduled_update(proposal_id).unwrap().unwrap();
        assert_eq!(scheduled.version, PV);
        assert_eq!(scheduled.activation_height, activation);

        let ctx = BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(activation, 0, TEST_CHAIN_ID),
            storage.clone(),
        );
        update
            .process_begin_block_with_handlers(&ctx, &EMPTY_UPGRADE_HANDLER_REGISTRY)
            .unwrap();

        assert_eq!(update.get_active_version().unwrap(), PV);
        assert_eq!(update.get_active_version_height().unwrap(), activation);
    });
}

struct CustomWindowVote {
    provider: outbe_primitives::storage::hashmap::HashMapStorageProvider,
    original: outbe_primitives::tee_attestation_v1::TeePolicyV1,
    measurement: B256,
    proposal: alloy_primitives::U256,
    deadline: u64,
    activation: u64,
}

impl CustomWindowVote {
    fn new(window: u64, change_enclave: bool) -> Self {
        use outbe_primitives::storage::hashmap::HashMapStorageProvider;
        let created = 100;
        let deadline = created + window;
        let activation = min_activation(deadline + 1) + 10;
        assert!(activation < created + VOTING_WINDOW_BLOCKS);
        let mut provider =
            HashMapStorageProvider::new_with_chain_identity(TEST_CHAIN_ID, B256::repeat_byte(1));
        provider.set_block_number(created);
        let original = initial_tee_policy(B256::repeat_byte(1), B256::repeat_byte(0x41), 0x31);
        let measurement = B256::repeat_byte(0x42);
        let mut fixture = Self {
            provider,
            original,
            measurement,
            proposal: alloy_primitives::U256::ZERO,
            deadline,
            activation,
        };
        fixture.proposal = fixture.create_proposal_and_votes(window, change_enclave);
        fixture
    }

    fn create_proposal_and_votes(
        &mut self,
        window: u64,
        change_enclave: bool,
    ) -> alloy_primitives::U256 {
        use alloy_primitives::U256;
        use alloy_sol_types::SolCall;
        use outbe_primitives::storage::StorageHandle;
        use outbe_vote::precompile::{dispatch_with_handlers, IVote};
        let created = 100;
        let storage = StorageHandle::new(&mut self.provider);
        setup_validators(storage.clone());
        TeeRegistry::new(storage.clone())
            .install_initial_policy_v1(&self.original)
            .unwrap();
        let mut payload = ScheduleUpdatePayload::new(PV, self.activation, "short vote");
        if change_enclave {
            payload.mrenclave = Some(self.measurement);
        }
        let call = IVote::createProposalWithVotingWindowCall {
            targetModule: UPDATE_ADDRESS,
            payload: serde_json::to_string(&payload).unwrap(),
            votingWindowBlocks: window,
        };
        let ret = dispatch_with_handlers(
            storage.clone(),
            &call.abi_encode(),
            PROPOSER,
            U256::ZERO,
            &VOTE_TARGET_REGISTRY,
        )
        .unwrap();
        let proposal = IVote::createProposalWithVotingWindowCall::abi_decode_returns(&ret).unwrap();
        let mut vote = Vote::new(storage);
        vote.cast_vote_approve(proposal, VOTER_A, true, created + 1)
            .unwrap();
        vote.cast_vote_approve(proposal, VOTER_B, true, created + 2)
            .unwrap();
        proposal
    }

    fn assert_pending_at_deadline(&mut self) {
        use outbe_primitives::storage::StorageHandle;
        self.provider.set_block_number(self.deadline);
        {
            let storage = StorageHandle::new(&mut self.provider);
            process_begin_block_test(storage.clone(), self.deadline);
            assert_eq!(
                Vote::new(storage.clone())
                    .proposals
                    .get(self.proposal)
                    .unwrap()
                    .unwrap()
                    .proposal_status()
                    .unwrap(),
                ProposalStatus::Pending
            );
            assert!(Update::new(storage)
                .read_scheduled_update(self.proposal)
                .unwrap()
                .is_none());
        }
    }

    fn assert_approved_after_deadline(&mut self, change_enclave: bool) {
        use outbe_primitives::storage::StorageHandle;
        self.provider.set_block_number(self.deadline + 1);
        {
            let storage = StorageHandle::new(&mut self.provider);
            process_begin_block_test(storage.clone(), self.deadline + 1);
            assert_eq!(
                Vote::new(storage.clone())
                    .proposals
                    .get(self.proposal)
                    .unwrap()
                    .unwrap()
                    .proposal_status()
                    .unwrap(),
                ProposalStatus::Approved
            );
            assert_eq!(
                Update::new(storage.clone())
                    .read_scheduled_update(self.proposal)
                    .unwrap()
                    .unwrap()
                    .activation_height,
                self.activation
            );
            let registry = TeeRegistry::new(storage);
            assert_eq!(registry.active_policy_v1().unwrap(), self.original);
            assert_eq!(
                registry.strict_upgrade_pending_v1().unwrap(),
                change_enclave
            );
        }
    }

    fn assert_activation_boundary(&mut self, change_enclave: bool) {
        use outbe_primitives::storage::StorageHandle;
        for height in [self.activation - 1, self.activation] {
            self.provider.set_block_number(height);
            let storage = StorageHandle::new(&mut self.provider);
            Update::new(storage.clone())
                .process_begin_block_with_handlers(
                    &block_ctx(storage.clone(), height),
                    &EMPTY_UPGRADE_HANDLER_REGISTRY,
                )
                .unwrap();
            let active = TeeRegistry::new(storage.clone())
                .active_policy_v1()
                .unwrap();
            let expected_measurement = if change_enclave && height == self.activation {
                self.measurement
            } else {
                self.original.measurement_rules[0].mrenclave
            };
            assert_eq!(active.measurement_rules[0].mrenclave, expected_measurement);
            if height == self.activation {
                assert_eq!(Update::new(storage).get_active_version().unwrap(), PV);
            }
        }
    }
}

#[test]
fn custom_voting_window_schedules_software_and_enclave_updates_before_default_deadline() {
    for window in [1_000, 30_000] {
        for change_enclave in [false, true] {
            let mut vote = CustomWindowVote::new(window, change_enclave);
            vote.assert_pending_at_deadline();
            vote.assert_approved_after_deadline(change_enclave);
            vote.assert_activation_boundary(change_enclave);
        }
    }
}

#[test]
fn software_only_update_keeps_enclave_for_absent_null_and_empty_measurement() {
    for field in [
        None,
        Some(serde_json::json!(null)),
        Some(serde_json::json!("")),
    ] {
        with_vote(|storage| {
            let current_height = 100;
            let deadline = current_height + VOTING_WINDOW_BLOCKS + 1;
            let activation = min_activation(deadline);
            let current = initial_tee_policy(
                storage.genesis_hash().unwrap(),
                B256::repeat_byte(0x41),
                0x31,
            );
            let mut registry = TeeRegistry::new(storage.clone());
            registry.install_initial_policy_v1(&current).unwrap();
            let mut payload =
                serde_json::to_value(ScheduleUpdatePayload::new(PV, activation, "software only"))
                    .unwrap();
            if let Some(field) = field {
                payload["mrenclave"] = field;
            }
            let mut vote = Vote::new(storage.clone());
            let proposal = create_approved_update(&mut vote, &payload.to_string(), current_height);
            process_begin_block_test(storage.clone(), deadline);
            assert_eq!(
                vote.proposals
                    .get(proposal)
                    .unwrap()
                    .unwrap()
                    .proposal_status()
                    .unwrap(),
                ProposalStatus::Approved
            );
            assert!(registry.staged_successor_policy_v1().unwrap().is_none());
            assert!(registry.enclave_upgrade_v1().unwrap().proposal_id.is_zero());
            let ctx = block_ctx(storage.clone(), activation);
            let mut update = Update::new(storage);
            update
                .process_begin_block_with_handlers(&ctx, &EMPTY_UPGRADE_HANDLER_REGISTRY)
                .unwrap();
            assert_eq!(update.get_active_version().unwrap(), PV);
            assert_eq!(registry.active_policy_v1().unwrap(), current);
        });
    }
}

fn replace_vote_policy(
    provider: &mut outbe_primitives::storage::hashmap::HashMapStorageProvider,
    current: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
    current_height: u64,
) -> outbe_primitives::tee_attestation_v1::TeePolicyV1 {
    use outbe_primitives::storage::StorageHandle;
    let mut replacement = current.clone();
    replacement.policy_version += 1;
    replacement.predecessor_policy_hash = current.policy_hash().unwrap();
    replacement.activation_height = current_height + 4;
    replacement.maximum_lease /= 2;
    {
        let storage = StorageHandle::new(provider);
        TeeRegistry::new(storage)
            .stage_successor_policy_v1(alloy_primitives::U256::from(999), &replacement)
            .unwrap();
    }
    provider.set_block_number(replacement.activation_height);
    TeeRegistry::new(StorageHandle::new(provider))
        .promote_staged_successor_policy_v1(
            alloy_primitives::U256::from(999),
            replacement.activation_height,
        )
        .unwrap();
    replacement
}

struct MeasurementUpgradeExpectation {
    current: outbe_primitives::tee_attestation_v1::TeePolicyV1,
    expected: outbe_primitives::tee_attestation_v1::TeePolicyV1,
    proposal: alloy_primitives::U256,
    activation: u64,
}

impl MeasurementUpgradeExpectation {
    fn new(
        current: outbe_primitives::tee_attestation_v1::TeePolicyV1,
        measurement: B256,
        activation: u64,
        proposal: alloy_primitives::U256,
    ) -> Self {
        let mut expected = current.clone();
        expected.policy_version += 1;
        expected.predecessor_policy_hash = current.policy_hash().unwrap();
        expected.activation_height = activation;
        expected.measurement_rules[0].mrenclave = measurement;
        expected.measurement_rules[0].admit_from_height = activation;
        expected.measurement_rules[0].admit_until_height_exclusive = u64::MAX;
        Self {
            current,
            expected,
            proposal,
            activation,
        }
    }

    fn assert_staged(
        &self,
        provider: &mut outbe_primitives::storage::hashmap::HashMapStorageProvider,
        deadline: u64,
    ) {
        use outbe_primitives::storage::StorageHandle;
        provider.set_block_number(deadline);
        {
            let storage = StorageHandle::new(provider);
            process_begin_block_test(storage.clone(), deadline);
            assert_eq!(
                Vote::new(storage.clone())
                    .proposals
                    .get(self.proposal)
                    .unwrap()
                    .unwrap()
                    .proposal_status()
                    .unwrap(),
                ProposalStatus::Approved
            );
            let mut registry = TeeRegistry::new(storage);
            assert_eq!(
                registry.enclave_upgrade_v1().unwrap().proposal_id,
                self.proposal
            );
            assert!(registry.strict_upgrade_pending_v1().unwrap());
            assert_eq!(registry.active_policy_v1().unwrap(), self.current);
            assert_eq!(
                registry.staged_successor_policy_v1().unwrap(),
                Some((self.proposal, self.expected.clone()))
            );
            assert!(registry
                .promote_staged_successor_policy_v1(self.proposal, self.activation - 1)
                .is_err());
        }
    }

    fn assert_promoted(
        &self,
        provider: &mut outbe_primitives::storage::hashmap::HashMapStorageProvider,
    ) {
        use outbe_primitives::storage::StorageHandle;
        provider.set_block_number(self.activation);
        {
            let storage = StorageHandle::new(provider);
            let ctx = block_ctx(storage.clone(), self.activation);
            Update::new(storage.clone())
                .process_begin_block_with_handlers(&ctx, &EMPTY_UPGRADE_HANDLER_REGISTRY)
                .unwrap();
            let registry = TeeRegistry::new(storage);
            assert!(!registry.strict_upgrade_pending_v1().unwrap());
            assert_eq!(registry.active_policy_v1().unwrap(), self.expected);
            assert_eq!(
                registry.last_enclave_retirement_height_v1().unwrap(),
                self.activation
            );
        }
    }
}

#[test]
fn measurement_only_vote_derives_policy_and_promotes_only_at_deadline() {
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    for changed_during_vote in [false, true] {
        let current_height = 100;
        let deadline = current_height + VOTING_WINDOW_BLOCKS + 1;
        let activation = min_activation(deadline);
        let genesis_hash = B256::repeat_byte(1);
        let mut provider =
            HashMapStorageProvider::new_with_chain_identity(TEST_CHAIN_ID, genesis_hash);
        provider.set_block_number(current_height);
        let mut current = initial_tee_policy(genesis_hash, B256::repeat_byte(0x41), 0x31);
        let measurement = B256::repeat_byte(0x42);
        let proposal = {
            let storage = StorageHandle::new(&mut provider);
            setup_validators(storage.clone());
            TeeRegistry::new(storage.clone())
                .install_initial_policy_v1(&current)
                .unwrap();
            let mut payload = ScheduleUpdatePayload::new(PV, activation, "measurement upgrade");
            payload.mrenclave = Some(measurement);
            let mut vote = Vote::new(storage);
            create_approved_update(
                &mut vote,
                &serde_json::to_string(&payload).unwrap(),
                current_height,
            )
        };
        if changed_during_vote {
            current = replace_vote_policy(&mut provider, &current, current_height);
        }
        let expected =
            MeasurementUpgradeExpectation::new(current, measurement, activation, proposal);
        expected.assert_staged(&mut provider, deadline);
        expected.assert_promoted(&mut provider);
    }
}

#[test]
fn one_approved_update_atomically_stages_tee_and_ocomp_successors() {
    with_vote(|storage| {
        let current_height = 100u64;
        let deadline = current_height + VOTING_WINDOW_BLOCKS + 1;
        let activation = min_activation(deadline);
        let genesis_hash = storage.genesis_hash().unwrap();
        let current_tee = initial_tee_policy(genesis_hash, B256::repeat_byte(0x51), 0x31);
        let successor_tee =
            successor_tee_policy(&current_tee, activation, B256::repeat_byte(0x52), 0x31);
        TeeRegistry::new(storage.clone())
            .install_initial_policy_v1(&current_tee)
            .unwrap();

        let current_ocomp = ocomp_authority(genesis_hash);
        let successor_ocomp = ocomp_successor(genesis_hash, activation);
        let limits = poc_schema_limits();
        OcompRegistry::new(storage.clone())
            .initialize_genesis_authority(&current_ocomp, B256::repeat_byte(0x53), 1, 1, &limits)
            .unwrap();

        let mut payload = ScheduleUpdatePayload::new(PV, activation, "TEE and OCOMP release")
            .with_ocomp_successor(&successor_ocomp)
            .unwrap();
        payload.mrenclave = Some(successor_tee.measurement_rules[0].mrenclave);
        let mut vote = Vote::new(storage.clone());
        let proposal_id = create_approved_update(
            &mut vote,
            &serde_json::to_string(&payload).unwrap(),
            current_height,
        );

        process_begin_block_test(storage.clone(), deadline);

        assert_eq!(
            TeeRegistry::new(storage.clone())
                .staged_successor_policy_v1()
                .unwrap(),
            Some((proposal_id, successor_tee))
        );
        assert_eq!(
            OcompRegistry::new(storage)
                .staged_successor(&limits)
                .unwrap(),
            Some((proposal_id, successor_ocomp))
        );
    });
}

#[test]
fn unchanged_measurement_rolls_back_update_schedule_and_staging() {
    with_vote(|storage| {
        let current_height = 100u64;
        let deadline = current_height + VOTING_WINDOW_BLOCKS + 1;
        let activation = min_activation(deadline);
        let genesis_hash = storage.genesis_hash().unwrap();
        let current = initial_tee_policy(genesis_hash, B256::repeat_byte(0x43), 0x31);
        TeeRegistry::new(storage.clone())
            .install_initial_policy_v1(&current)
            .unwrap();
        let mut payload = ScheduleUpdatePayload::new(PV, activation, "unchanged measurement");
        payload.mrenclave = Some(current.measurement_rules[0].mrenclave);
        let payload = serde_json::to_string(&payload).unwrap();
        let mut vote = Vote::new(storage.clone());
        let proposal_id = create_approved_update(&mut vote, &payload, current_height);

        process_begin_block_test(storage.clone(), deadline);

        assert_eq!(
            vote.proposals
                .get(proposal_id)
                .unwrap()
                .unwrap()
                .proposal_status()
                .unwrap(),
            ProposalStatus::Error
        );
        assert!(Update::new(storage.clone())
            .read_scheduled_update(proposal_id)
            .unwrap()
            .is_none());
        assert_eq!(
            TeeRegistry::new(storage)
                .staged_successor_policy_v1()
                .unwrap(),
            None
        );
    });
}

#[test]
fn invalid_json_payload_is_rejected_at_creation() {
    with_vote(|storage| {
        let mut governance = Vote::new(storage.clone());
        let err = governance
            .create_proposal(
                PROPOSER,
                UPDATE_ADDRESS,
                "not-json",
                200,
                &VOTE_TARGET_REGISTRY,
            )
            .unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::Revert(msg) if msg.contains("invalid proposal payload")
        ));
    });
}

#[test]
fn invalid_update_payload_is_rejected_at_creation() {
    with_vote(|storage| {
        let mut governance = Vote::new(storage.clone());
        let err = governance
            .create_proposal(
                PROPOSER,
                UPDATE_ADDRESS,
                r#"{"version":"0.0","activationHeight":1000,"info":""}"#,
                200,
                &VOTE_TARGET_REGISTRY,
            )
            .unwrap_err();
        assert!(matches!(err, PrecompileError::Revert(_)));
    });
}

#[test]
fn handler_conflict_marks_proposal_error() {
    with_vote(|storage| {
        let mut vote = Vote::new(storage.clone());
        let current = 250u64;
        let deadline = current + VOTING_WINDOW_BLOCKS + 1;
        let activation = min_activation(deadline + 1);
        let payload = encode_schedule_update_json(PV, activation, "");

        let first = vote
            .create_proposal(
                PROPOSER,
                UPDATE_ADDRESS,
                &payload,
                current,
                &VOTE_TARGET_REGISTRY,
            )
            .unwrap();
        for (voter, off) in [(VOTER_A, 1), (VOTER_B, 2)] {
            vote.cast_vote_approve(first, voter, true, current + off)
                .unwrap();
        }
        process_begin_block_test(storage.clone(), deadline);
        assert_eq!(
            vote.proposals
                .get(first)
                .unwrap()
                .unwrap()
                .proposal_status()
                .unwrap(),
            ProposalStatus::Approved
        );

        let second = vote
            .create_proposal(
                VOTER_A,
                UPDATE_ADDRESS,
                &payload,
                current + 1,
                &VOTE_TARGET_REGISTRY,
            )
            .unwrap();
        for (voter, off) in [(VOTER_A, 3), (VOTER_B, 4)] {
            vote.cast_vote_approve(second, voter, true, current + off)
                .unwrap();
        }
        process_begin_block_test(storage.clone(), deadline + 1);
        assert_eq!(
            vote.proposals
                .get(second)
                .unwrap()
                .unwrap()
                .proposal_status()
                .unwrap(),
            ProposalStatus::Error
        );
    });
}

#[test]
fn unknown_target_is_rejected_at_creation() {
    with_vote(|storage| {
        let mut vote = Vote::new(storage.clone());
        let current = 260u64;
        let payload = empty_update_payload(current);
        let err = vote
            .create_proposal(
                PROPOSER,
                UNKNOWN_TARGET,
                &payload,
                current,
                &VOTE_TARGET_REGISTRY,
            )
            .unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::Revert(msg) if msg.contains("unknown vote target module")
        ));
    });
}

#[test]
fn expired_update_proposal_does_not_emit_upgrade_activated() {
    let mut provider =
        outbe_primitives::storage::hashmap::HashMapStorageProvider::new(TEST_CHAIN_ID);
    let storage = outbe_primitives::storage::StorageHandle::new(&mut provider);
    setup_validators(storage.clone());

    let mut vote = Vote::new(storage.clone());
    let current = 400u64;
    let payload =
        encode_schedule_update_json(PV, min_activation(current + VOTING_WINDOW_BLOCKS), "");
    let proposal_id = vote
        .create_proposal(
            PROPOSER,
            UPDATE_ADDRESS,
            &payload,
            current,
            &VOTE_TARGET_REGISTRY,
        )
        .unwrap();
    vote.cast_vote_approve(proposal_id, VOTER_A, true, current + 1)
        .unwrap();

    let deadline = current + VOTING_WINDOW_BLOCKS + 1;
    process_begin_block_test(storage.clone(), deadline);
    assert_eq!(
        vote.proposals
            .get(proposal_id)
            .unwrap()
            .unwrap()
            .proposal_status()
            .unwrap(),
        ProposalStatus::Expired
    );

    let update = Update::new(storage);
    assert!(update.read_scheduled_update(proposal_id).unwrap().is_none());
    assert!(!provider
        .get_events(UPDATE_ADDRESS)
        .iter()
        .any(|log| log.topics().first() == Some(&IUpdate::UpgradeActivated::SIGNATURE_HASH)));
}

/// Malformed payloads at the public `VoteTarget` seam. Bytes that are not
/// JSON fail in the payload decoder. JSON of the wrong shape fails in the
/// Update payload parser.
const NOT_JSON_PAYLOADS: [&[u8]; 4] = [&[0xff, 0xfe, 0xfd], b"not-json", b"{", b""];
const WRONG_SHAPE_PAYLOADS: [&[u8]; 3] = [b"null", b"[]", b"7"];

fn payload_context() -> VoteTargetContext {
    VoteTargetContext {
        proposer: PROPOSER,
        attached_value: alloy_primitives::U256::ZERO,
        block_number: 1,
        chain_id: TEST_CHAIN_ID,
    }
}

#[test]
fn validate_reverts_with_exact_reason_for_malformed_payload() {
    for payload in NOT_JSON_PAYLOADS {
        let outcome = UPDATE_VOTE_TARGET.validate(payload, payload_context());
        assert!(
            matches!(&outcome, Err(PrecompileError::Revert(reason)) if reason == "invalid proposal payload"),
            "{payload:?}: {outcome:?}"
        );
    }
    for payload in WRONG_SHAPE_PAYLOADS {
        let outcome = UPDATE_VOTE_TARGET.validate(payload, payload_context());
        assert!(
            matches!(&outcome, Err(PrecompileError::Revert(reason)) if reason == "invalid vote payload"),
            "{payload:?}: {outcome:?}"
        );
    }
}

#[test]
fn handle_approved_fails_fatally_without_mutation_for_malformed_payload() {
    let cases = NOT_JSON_PAYLOADS
        .iter()
        .map(|payload| (*payload, "stored Update proposal payload is invalid"))
        .chain(WRONG_SHAPE_PAYLOADS.iter().map(|payload| {
            (
                *payload,
                "stored Update proposal payload is invalid: invalid vote payload",
            )
        }));
    for (payload, expected) in cases {
        let mut provider =
            outbe_primitives::storage::hashmap::HashMapStorageProvider::new_with_chain_identity(
                TEST_CHAIN_ID,
                B256::repeat_byte(0x01),
            );
        provider.set_block_number(1);
        provider.clear_mutation_failure();
        let outcome: Result<TargetExecutionOutcome, PrecompileError> = provider.enter(|storage| {
            UPDATE_VOTE_TARGET.handle_approved(
                &block_ctx(storage, 1),
                alloy_primitives::U256::from(1),
                payload,
                payload_context(),
            )
        });
        assert!(
            matches!(&outcome, Err(PrecompileError::Fatal(reason)) if reason == expected),
            "{payload:?}: {outcome:?}"
        );
        assert_eq!(
            provider.clear_mutation_failure(),
            0,
            "{payload:?}: storage mutated"
        );
        assert!(provider.get_ordered_events().is_empty(), "{payload:?}");
    }
}
