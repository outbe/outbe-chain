use std::sync::{Arc, Mutex};

use alloy_primitives::{B256, U256};
use alloy_sol_types::SolEvent;
use outbe_ocomp_protocol::{
    abi::encode_submit_lysis_result_calldata,
    state::{OcompJobRecordV1, OcompJobStatus, OcompTerminalOutcome},
};
use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    chain,
    storage::{
        hashmap::HashMapStorageProvider, MetadosisCertifiedFinalityBinding,
        MetadosisMutationPurposeTag, StorageHandle,
    },
};
use outbe_promislimit::schema::PromisLimitContract;
use proptest::{
    arbitrary::any,
    collection::vec,
    prop_assert, prop_assert_eq, prop_oneof,
    strategy::{Just, Strategy},
    test_runner::{
        Config, FileFailurePersistence, RngAlgorithm, TestCaseResult, TestRng, TestRunner,
    },
};

use super::{
    poc_schema_limits, prepare_ready_days_fixture, prepare_request_fixture,
    prepare_request_fixture_with_day_type, DayPhase, IMetadosis,
};
use crate::{
    api, commands,
    fixture_kernel::{ActivationFixture, TEST_WWD},
    WwdDayType, WwdMembership, WwdStatus,
};

const OCOMP_ADAPTER_SEED: [u8; 32] = *b"metadosis-ocomp-adapter-model!!!";
const GENERATED_CASES: u32 = 64;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct OcompDistribution {
    cases: usize,
    request: usize,
    finality: usize,
    open: usize,
    vote: usize,
    expiry: usize,
    duplicate: usize,
    rejected: usize,
    rollback: usize,
}

impl OcompDistribution {
    fn observe_expiry(&mut self, history: &ExpiryHistory) {
        self.cases += 1;
        self.request += 2;
        self.finality += 1;
        self.open += 1;
        self.expiry += 1;
        self.rollback += 1;
        for probe in &history.pre_finality {
            match probe {
                PreFinalityProbe::DuplicateRequest => self.duplicate += 1,
                PreFinalityProbe::UnrelatedFinality(_) | PreFinalityProbe::EarlyLifecycle(_) => {
                    self.rejected += 1;
                }
            }
        }
        for probe in &history.open {
            match probe {
                OpenProbe::DuplicateOpen => self.duplicate += 1,
                OpenProbe::BeforeDeadline(_) | OpenProbe::UnrelatedFinality(_) => {
                    self.rejected += 1;
                }
            }
        }
    }

    fn observe_quorum(&mut self, history: &QuorumHistory) {
        self.cases += 1;
        self.vote += 3 + history.before_second.len() + history.before_quorum.len();
        self.rollback += 1;
        for probe in &history.before_second {
            match probe {
                BeforeSecondProbe::DuplicateFirst => self.duplicate += 1,
                BeforeSecondProbe::Invalid(_) => self.rejected += 1,
            }
        }
        for probe in &history.before_quorum {
            match probe {
                BeforeQuorumProbe::DuplicateFirst | BeforeQuorumProbe::DuplicateSecond => {
                    self.duplicate += 1;
                }
                BeforeQuorumProbe::Invalid(_) => self.rejected += 1,
            }
        }
    }

    fn assert_expiry_complete(&self) {
        assert_eq!(self.cases, GENERATED_CASES as usize);
        for (label, count) in [
            ("request", self.request),
            ("finality", self.finality),
            ("open", self.open),
            ("expiry", self.expiry),
            ("duplicate", self.duplicate),
            ("rejected", self.rejected),
            ("rollback", self.rollback),
        ] {
            assert!(count > 0, "OCOMP expiry distribution missing {label}");
        }
    }

    fn assert_quorum_complete(&self) {
        assert_eq!(self.cases, GENERATED_CASES as usize);
        for (label, count) in [
            ("vote", self.vote),
            ("duplicate", self.duplicate),
            ("rejected", self.rejected),
            ("rollback", self.rollback),
        ] {
            assert!(count > 0, "OCOMP quorum distribution missing {label}");
        }
    }

    fn print_retained(&self, suite: &str) {
        println!(
            "METADOSIS_CASE_DISTRIBUTION_V1 {{\"suite\":\"{suite}\",\"seed_family\":\"metadosis-ocomp-adapter-model\",\"cases\":{},\"request\":{},\"finality\":{},\"open\":{},\"vote\":{},\"expiry\":{},\"duplicate\":{},\"rejected\":{},\"rollback\":{}}}",
            self.cases,
            self.request,
            self.finality,
            self.open,
            self.vote,
            self.expiry,
            self.duplicate,
            self.rejected,
            self.rollback,
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelJob {
    AwaitingFinality,
    VotingOpen,
    Expired,
    Completed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PersistedModel {
    wwd_status: WwdStatus,
    membership: WwdMembership,
    job: ModelJob,
    pending_nonce: u64,
    attempt: u32,
    has_terminal_receipt: bool,
    has_active_generation: bool,
}

impl PersistedModel {
    fn observe(
        self,
        provider: &mut HashMapStorageProvider,
        wwd: outbe_common::WorldwideDay,
        intent_id: B256,
    ) -> TestCaseResult {
        StorageHandle::enter(provider, |storage| {
            let projection = api::worldwide_day(storage.clone(), wwd)
                .expect("typed WWD query")
                .expect("model WWD");
            prop_assert_eq!(projection.status, self.wwd_status);
            prop_assert_eq!(projection.membership, self.membership);

            let encoded =
                api::get_offchain_job(storage.clone(), intent_id).expect("public canonical job");
            let record = OcompJobRecordV1::decode_canonical(&encoded, &poc_schema_limits())
                .expect("public job decodes");
            let expected_status = match self.job {
                ModelJob::AwaitingFinality => OcompJobStatus::AwaitingFinality,
                ModelJob::VotingOpen => OcompJobStatus::VotingOpen,
                ModelJob::Expired => OcompJobStatus::Expired,
                ModelJob::Completed => OcompJobStatus::Completed,
            };
            prop_assert_eq!(record.status, expected_status);
            prop_assert_eq!(record.intent.pending_nonce, self.pending_nonce);
            prop_assert_eq!(record.intent.attempt, self.attempt);
            prop_assert_eq!(
                api::get_lysis_terminal_receipt(storage.clone(), intent_id).is_ok(),
                self.has_terminal_receipt
            );
            prop_assert_eq!(
                api::get_active_lysis_generation(storage, wwd).is_ok(),
                self.has_active_generation
            );
            Ok(())
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SemanticCheckpoint {
    storage: std::collections::HashMap<(alloy_primitives::Address, U256), U256>,
    events: Vec<alloy_primitives::Log>,
    ce_work: outbe_compressed_entities::CeWorkCheckpoint,
}

fn checkpoint(
    provider: &HashMapStorageProvider,
    scope: &outbe_compressed_entities::ExecutionScope,
) -> SemanticCheckpoint {
    SemanticCheckpoint {
        storage: provider.storage.clone(),
        events: provider.get_ordered_events().to_vec(),
        ce_work: scope.ce_work_checkpoint().expect("CE checkpoint"),
    }
}

fn set_block(provider: &mut HashMapStorageProvider, block_number: u64, timestamp: u64) {
    provider.set_block_number(block_number);
    provider.set_timestamp(U256::from(timestamp));
}

fn runtime_command<R>(
    provider: &mut HashMapStorageProvider,
    purpose: MetadosisMutationPurposeTag,
    block_number: u64,
    timestamp: u64,
    command: impl FnOnce(&BlockRuntimeContext<'_>) -> R,
) -> R {
    set_block(provider, block_number, timestamp);
    provider.enable_metadosis_mutation_frame(purpose);
    StorageHandle::enter(provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(block_number, timestamp, chain::CHAIN_ID),
            storage,
        );
        command(&ctx)
    })
}

fn submit_vote(
    fixture: &mut ActivationFixture,
    validator_index: u8,
    activation_entitled: bool,
) -> outbe_primitives::error::Result<alloy_primitives::Bytes> {
    let calldata = encode_submit_lysis_result_calldata(
        &fixture.signed_result_vote(validator_index),
        &fixture.limits,
    )
    .expect("canonical result vote calldata");
    fixture
        .provider
        .enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::VerifiedResultVote);
    if activation_entitled {
        fixture.provider.enable_lysis_activation_frame();
    }
    StorageHandle::enter(&mut fixture.provider, |storage| {
        commands::submit_verified_result_vote(storage, &fixture.scope, &calldata, U256::ZERO, false)
    })
}

fn public_intent(provider: &mut HashMapStorageProvider, wwd: outbe_common::WorldwideDay) -> B256 {
    let (intent_id, event_wwd, pending_nonce, attempt, activation_preconditions_hash) = {
        let requested = provider
            .get_ordered_events()
            .iter()
            .rev()
            .find_map(|log| IMetadosis::OffchainJobRequested::decode_log(log).ok())
            .expect("public OffchainJobRequested event");
        (
            requested.data.intentId,
            requested.data.wwd,
            requested.data.pendingNonce,
            requested.data.attempt,
            requested.data.activationPreconditionsHash,
        )
    };
    assert_eq!(event_wwd, wwd.value());
    StorageHandle::enter(provider, |storage| {
        let limits = poc_schema_limits();
        let encoded =
            api::get_offchain_job(storage, intent_id).expect("event-keyed public canonical job");
        let record =
            OcompJobRecordV1::decode_canonical(&encoded, &limits).expect("public job decodes");
        assert_eq!(record.intent.intent_id(&limits).unwrap(), intent_id);
        assert_eq!(record.intent.wwd, event_wwd);
        assert_eq!(record.intent.pending_nonce, pending_nonce);
        assert_eq!(record.intent.attempt, attempt);
        assert_eq!(
            record
                .intent
                .activation_preconditions
                .activation_preconditions_hash(&limits)
                .unwrap(),
            activation_preconditions_hash
        );
        intent_id
    })
}

struct ExpiringProductionFixture {
    scope: outbe_compressed_entities::ExecutionScope,
    wwd: outbe_common::WorldwideDay,
    intent_id: B256,
    deadline_height: u64,
    command_time: u64,
    retained_lysis_budget: U256,
}

struct RetryableExpiryProductionFixture {
    scope: outbe_compressed_entities::ExecutionScope,
    deadline_height: u64,
    command_time: u64,
}

fn prepare_retryable_expiry_production_fixture(
    provider: &mut HashMapStorageProvider,
) -> RetryableExpiryProductionFixture {
    let fixture = prepare_request_fixture(provider, true);
    runtime_command(
        provider,
        MetadosisMutationPurposeTag::OcompLifecycle,
        fixture.block_number,
        fixture.block_time,
        |ctx| commands::run_ocomp_terminal_request(ctx, &fixture.scope),
    )
    .expect("production terminal request");
    let intent_id = public_intent(provider, fixture.wwd);

    let finality_height = fixture.block_number + 2;
    let certified = MetadosisCertifiedFinalityBinding::new(
        chain::CHAIN_ID,
        finality_height,
        fixture.block_number,
        B256::repeat_byte(0x46),
        B256::repeat_byte(0x98),
    );
    assert!(runtime_command(
        provider,
        MetadosisMutationPurposeTag::CertifiedFinality,
        finality_height,
        fixture.block_time + 2,
        |ctx| commands::record_certified_parent_finality(ctx, &certified),
    )
    .expect("production certified finality"));

    let (open_height, deadline_height) = StorageHandle::enter(provider, |storage| {
        let encoded = api::get_offchain_job(storage, intent_id).expect("finalized public job");
        let record = OcompJobRecordV1::decode_canonical(&encoded, &poc_schema_limits())
            .expect("finalized public job decodes");
        let finalized = record.finalized.expect("finality binding");
        (finalized.open_height, finalized.deadline_height)
    });
    runtime_command(
        provider,
        MetadosisMutationPurposeTag::OcompLifecycle,
        open_height,
        fixture.block_time + 3,
        commands::run_ocomp_lifecycle_begin,
    )
    .expect("production voting open");

    RetryableExpiryProductionFixture {
        scope: fixture.scope,
        deadline_height,
        command_time: fixture.block_time + 4,
    }
}

fn prepare_expiring_production_fixture(
    provider: &mut HashMapStorageProvider,
) -> ExpiringProductionFixture {
    let fixture = prepare_request_fixture(provider, true);
    runtime_command(
        provider,
        MetadosisMutationPurposeTag::OcompLifecycle,
        fixture.block_number,
        fixture.block_time,
        |ctx| commands::run_ocomp_terminal_request(ctx, &fixture.scope),
    )
    .expect("production terminal request");
    let intent_id = public_intent(provider, fixture.wwd);
    let retained_lysis_budget = StorageHandle::enter(provider, |storage| {
        let encoded = api::get_offchain_job(storage, intent_id).expect("public job");
        OcompJobRecordV1::decode_canonical(&encoded, &poc_schema_limits())
            .expect("public job decodes")
            .intent
            .frozen_metadosis_values
            .lysis_budget
    });

    let finality_height = fixture.block_number + 2;
    let certified = MetadosisCertifiedFinalityBinding::new(
        chain::CHAIN_ID,
        finality_height,
        fixture.block_number,
        B256::repeat_byte(0x46),
        B256::repeat_byte(0x98),
    );
    assert!(runtime_command(
        provider,
        MetadosisMutationPurposeTag::CertifiedFinality,
        finality_height,
        fixture.block_time + 2,
        |ctx| commands::record_certified_parent_finality(ctx, &certified),
    )
    .expect("production certified finality"));

    let (open_height, deadline_height) = StorageHandle::enter(provider, |storage| {
        let encoded = api::get_offchain_job(storage, intent_id).expect("finalized public job");
        let record = OcompJobRecordV1::decode_canonical(&encoded, &poc_schema_limits())
            .expect("finalized public job decodes");
        let finalized = record.finalized.expect("finality binding");
        (finalized.open_height, finalized.deadline_height)
    });
    runtime_command(
        provider,
        MetadosisMutationPurposeTag::OcompLifecycle,
        open_height,
        fixture.block_time + 3,
        commands::run_ocomp_lifecycle_begin,
    )
    .expect("production voting open");
    let intent_id = StorageHandle::enter(provider, |storage| {
        let limits = poc_schema_limits();
        let mut metadosis = crate::schema::MetadosisContract::new(storage.clone());
        let profile = metadosis
            .read_ocomp_request_profile(&limits)
            .unwrap()
            .expect("frozen request profile");
        assert_eq!(profile.capacity_profile.max_terminal_job_records, 365);
        let fsm_limits = crate::ocomp::state::JobFsmLimits {
            max_terminal_records: profile.capacity_profile.max_terminal_job_records,
        };
        let seeded = metadosis
            .fixture_seed_ocomp_terminal_history(
                intent_id,
                fsm_limits.max_terminal_records - 1,
                &limits,
                fsm_limits,
            )
            .expect("canonical near-cap terminal history");
        let projection = metadosis
            .ocomp_fsm_state(fixture.wwd, &limits, fsm_limits)
            .unwrap()
            .projection();
        assert_eq!(projection.terminal_records, 364);
        assert_eq!(projection.pending_nonce, 364);
        assert_eq!(projection.live_intent_id, Some(seeded));
        // The near-cap history charges only this day's own budget; this pin is
        // what distinguishes the per-day cap from the former global index.
        assert_eq!(metadosis.terminal_intent_count(fixture.wwd).unwrap(), 364);
        crate::aggregate::ValidatedWwdAggregate::load_and_validate(storage)
            .expect("seeded pre-state passes production aggregate validation");
        seeded
    });

    ExpiringProductionFixture {
        scope: fixture.scope,
        wwd: fixture.wwd,
        intent_id,
        deadline_height,
        command_time: fixture.block_time + 4,
        retained_lysis_budget,
    }
}

fn assert_exhausted_production_state(
    provider: &mut HashMapStorageProvider,
    fixture: &ExpiringProductionFixture,
    carry_over_before: U256,
) {
    let limits = poc_schema_limits();
    let fsm_limits = crate::ocomp::state::JobFsmLimits {
        max_terminal_records: 365,
    };
    StorageHandle::enter(provider, |storage| {
        let projection = api::worldwide_day(storage.clone(), fixture.wwd)
            .expect("typed WWD query")
            .expect("persisted WWD");
        assert_eq!(projection.status, WwdStatus::Failed);
        assert_eq!(projection.membership, WwdMembership::Closed);

        let metadosis = crate::schema::MetadosisContract::new(storage.clone());
        assert!(metadosis.ocomp_scheduler.is_empty().unwrap());
        assert!(metadosis
            .next_ocomp_ready(&limits, fsm_limits)
            .unwrap()
            .is_none());
        assert!(metadosis
            .ocomp_fsm_state(fixture.wwd, &limits, fsm_limits)
            .is_err());
        assert!(metadosis
            .live_ocomp_fsm_state_by_intent(fixture.intent_id, &limits, fsm_limits)
            .unwrap()
            .is_none());

        let record = metadosis
            .ocomp_job_record(fixture.intent_id, &limits)
            .unwrap()
            .expect("retained terminal job record");
        assert_eq!(record.status, OcompJobStatus::Expired);
        let terminal = record.terminal.expect("terminal expiry");
        assert_eq!(terminal.outcome, OcompTerminalOutcome::Expired);
        assert_eq!(terminal.next_pending_nonce, Some(365));
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            carry_over_before + fixture.retained_lysis_budget
        );
        assert_eq!(
            outbe_tribute::TributeContract::new(storage)
                .total_supply()
                .unwrap(),
            1
        );
    });
}

#[derive(Clone, Debug)]
enum PreFinalityProbe {
    DuplicateRequest,
    UnrelatedFinality(u8),
    EarlyLifecycle(u8),
}

#[derive(Clone, Debug)]
enum OpenProbe {
    DuplicateOpen,
    BeforeDeadline(u8),
    UnrelatedFinality(u8),
}

#[derive(Clone, Debug)]
struct ExpiryHistory {
    request_jitter: u8,
    pre_finality: Vec<PreFinalityProbe>,
    open: Vec<OpenProbe>,
}

fn expiry_history_strategy() -> impl Strategy<Value = ExpiryHistory> {
    let pre_finality = prop_oneof![
        Just(PreFinalityProbe::DuplicateRequest),
        any::<u8>().prop_map(PreFinalityProbe::UnrelatedFinality),
        any::<u8>().prop_map(PreFinalityProbe::EarlyLifecycle),
    ];
    let open = prop_oneof![
        Just(OpenProbe::DuplicateOpen),
        (1_u8..=63).prop_map(OpenProbe::BeforeDeadline),
        any::<u8>().prop_map(OpenProbe::UnrelatedFinality),
    ];
    (any::<u8>(), vec(pre_finality, 0..=12), vec(open, 0..=12)).prop_map(
        |(request_jitter, pre_finality, open)| ExpiryHistory {
            request_jitter,
            pre_finality,
            open,
        },
    )
}

fn run_expiry_retry_history(history: ExpiryHistory) -> TestCaseResult {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = prepare_request_fixture(&mut provider, true);
    let request_time = fixture.block_time + u64::from(history.request_jitter % 3);

    runtime_command(
        &mut provider,
        MetadosisMutationPurposeTag::OcompLifecycle,
        fixture.block_number,
        request_time,
        |ctx| commands::run_ocomp_terminal_request(ctx, &fixture.scope),
    )
    .expect("production terminal-request command");
    let intent_id = public_intent(&mut provider, fixture.wwd);
    PersistedModel {
        wwd_status: WwdStatus::OffchainPending,
        membership: WwdMembership::Active,
        job: ModelJob::AwaitingFinality,
        pending_nonce: 0,
        attempt: 0,
        has_terminal_receipt: false,
        has_active_generation: false,
    }
    .observe(&mut provider, fixture.wwd, intent_id)?;

    for probe in history.pre_finality {
        let before = checkpoint(&provider, &fixture.scope);
        match probe {
            PreFinalityProbe::DuplicateRequest => {
                runtime_command(
                    &mut provider,
                    MetadosisMutationPurposeTag::OcompLifecycle,
                    fixture.block_number,
                    request_time,
                    |ctx| commands::run_ocomp_terminal_request(ctx, &fixture.scope),
                )
                .expect("generated exact request replay");
            }
            PreFinalityProbe::UnrelatedFinality(tag) => {
                let unrelated_height = fixture.block_number + 1;
                let unrelated = MetadosisCertifiedFinalityBinding::new(
                    chain::CHAIN_ID,
                    unrelated_height,
                    fixture.block_number - 1,
                    B256::repeat_byte(tag),
                    B256::repeat_byte(tag.wrapping_add(1)),
                );
                let matched = runtime_command(
                    &mut provider,
                    MetadosisMutationPurposeTag::CertifiedFinality,
                    unrelated_height,
                    request_time + 1,
                    |ctx| commands::record_certified_parent_finality(ctx, &unrelated),
                )
                .expect("generated unrelated finality is a no-op");
                prop_assert!(!matched, "distribution: unrelated-finality rejection");
            }
            PreFinalityProbe::EarlyLifecycle(delta) => {
                runtime_command(
                    &mut provider,
                    MetadosisMutationPurposeTag::OcompLifecycle,
                    fixture.block_number + 1 + u64::from(delta % 2),
                    request_time + 1,
                    commands::run_ocomp_lifecycle_begin,
                )
                .expect("generated pre-finality lifecycle probe");
            }
        }
        prop_assert_eq!(checkpoint(&provider, &fixture.scope), before);
        PersistedModel {
            wwd_status: WwdStatus::OffchainPending,
            membership: WwdMembership::Active,
            job: ModelJob::AwaitingFinality,
            pending_nonce: 0,
            attempt: 0,
            has_terminal_receipt: false,
            has_active_generation: false,
        }
        .observe(&mut provider, fixture.wwd, intent_id)?;
    }

    let finality_height = fixture.block_number + 2;
    let certified = MetadosisCertifiedFinalityBinding::new(
        chain::CHAIN_ID,
        finality_height,
        fixture.block_number,
        B256::repeat_byte(0x46),
        B256::repeat_byte(0x98),
    );
    prop_assert!(runtime_command(
        &mut provider,
        MetadosisMutationPurposeTag::CertifiedFinality,
        finality_height,
        request_time + 2,
        |ctx| commands::record_certified_parent_finality(ctx, &certified),
    )
    .expect("production certified-finality command"));
    PersistedModel {
        wwd_status: WwdStatus::OffchainPending,
        membership: WwdMembership::Active,
        job: ModelJob::AwaitingFinality,
        pending_nonce: 0,
        attempt: 0,
        has_terminal_receipt: false,
        has_active_generation: false,
    }
    .observe(&mut provider, fixture.wwd, intent_id)?;

    let (open_height, deadline_height) = StorageHandle::enter(&mut provider, |storage| {
        let encoded = api::get_offchain_job(storage, intent_id).expect("finalized public job");
        let record = OcompJobRecordV1::decode_canonical(&encoded, &poc_schema_limits())
            .expect("finalized public job decodes");
        let finalized = record.finalized.expect("finality binding");
        (finalized.open_height, finalized.deadline_height)
    });
    runtime_command(
        &mut provider,
        MetadosisMutationPurposeTag::OcompLifecycle,
        open_height,
        request_time + 3,
        commands::run_ocomp_lifecycle_begin,
    )
    .expect("production voting-open command");
    PersistedModel {
        wwd_status: WwdStatus::OffchainPending,
        membership: WwdMembership::Active,
        job: ModelJob::VotingOpen,
        pending_nonce: 0,
        attempt: 0,
        has_terminal_receipt: false,
        has_active_generation: false,
    }
    .observe(&mut provider, fixture.wwd, intent_id)?;

    for probe in history.open {
        let before = checkpoint(&provider, &fixture.scope);
        match probe {
            OpenProbe::DuplicateOpen => {
                runtime_command(
                    &mut provider,
                    MetadosisMutationPurposeTag::OcompLifecycle,
                    open_height,
                    request_time + 3,
                    commands::run_ocomp_lifecycle_begin,
                )
                .expect("generated duplicate open command");
            }
            OpenProbe::BeforeDeadline(delta) => {
                let span = deadline_height.saturating_sub(open_height).max(1);
                let height = open_height + (u64::from(delta) % span);
                runtime_command(
                    &mut provider,
                    MetadosisMutationPurposeTag::OcompLifecycle,
                    height,
                    request_time + 3,
                    commands::run_ocomp_lifecycle_begin,
                )
                .expect("generated pre-deadline lifecycle command");
            }
            OpenProbe::UnrelatedFinality(tag) => {
                let unrelated = MetadosisCertifiedFinalityBinding::new(
                    chain::CHAIN_ID,
                    open_height,
                    fixture.block_number - 1,
                    B256::repeat_byte(tag),
                    B256::repeat_byte(tag.wrapping_add(1)),
                );
                let matched = runtime_command(
                    &mut provider,
                    MetadosisMutationPurposeTag::CertifiedFinality,
                    open_height,
                    request_time + 3,
                    |ctx| commands::record_certified_parent_finality(ctx, &unrelated),
                )
                .expect("generated post-open unrelated finality");
                prop_assert!(!matched);
            }
        }
        prop_assert_eq!(
            checkpoint(&provider, &fixture.scope),
            before,
            "distribution: generated open-state probe is effect-free"
        );
        PersistedModel {
            wwd_status: WwdStatus::OffchainPending,
            membership: WwdMembership::Active,
            job: ModelJob::VotingOpen,
            pending_nonce: 0,
            attempt: 0,
            has_terminal_receipt: false,
            has_active_generation: false,
        }
        .observe(&mut provider, fixture.wwd, intent_id)?;
    }

    // Fail immediately after the first expiry mutation. The command journal
    // must restore storage, ordered events, and CE work before an exact retry.
    set_block(&mut provider, deadline_height, request_time + 4);
    let before_fault = checkpoint(&provider, &fixture.scope);
    provider.fail_after_mutation_at(0);
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::OcompLifecycle);
    let failed = StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(deadline_height, request_time + 4, chain::CHAIN_ID),
            storage,
        );
        commands::run_ocomp_lifecycle_begin(&ctx)
    });
    prop_assert!(failed.is_err(), "distribution: injected expiry rollback");
    prop_assert!(provider.clear_mutation_failure() > 0);
    prop_assert_eq!(checkpoint(&provider, &fixture.scope), before_fault);
    PersistedModel {
        wwd_status: WwdStatus::OffchainPending,
        membership: WwdMembership::Active,
        job: ModelJob::VotingOpen,
        pending_nonce: 0,
        attempt: 0,
        has_terminal_receipt: false,
        has_active_generation: false,
    }
    .observe(&mut provider, fixture.wwd, intent_id)?;

    runtime_command(
        &mut provider,
        MetadosisMutationPurposeTag::OcompLifecycle,
        deadline_height,
        request_time + 4,
        commands::run_ocomp_lifecycle_begin,
    )
    .expect("exact expiry retry");
    PersistedModel {
        wwd_status: WwdStatus::Ready,
        membership: WwdMembership::Active,
        job: ModelJob::Expired,
        pending_nonce: 0,
        attempt: 0,
        has_terminal_receipt: false,
        has_active_generation: false,
    }
    .observe(&mut provider, fixture.wwd, intent_id)?;

    let retry_height = deadline_height + 1;
    runtime_command(
        &mut provider,
        MetadosisMutationPurposeTag::OcompLifecycle,
        retry_height,
        request_time + 5,
        commands::run_ocomp_lifecycle_begin,
    )
    .expect("retry-height lifecycle command");
    PersistedModel {
        wwd_status: WwdStatus::Ready,
        membership: WwdMembership::Active,
        job: ModelJob::Expired,
        pending_nonce: 0,
        attempt: 0,
        has_terminal_receipt: false,
        has_active_generation: false,
    }
    .observe(&mut provider, fixture.wwd, intent_id)?;
    runtime_command(
        &mut provider,
        MetadosisMutationPurposeTag::OcompLifecycle,
        retry_height,
        request_time + 5,
        |ctx| commands::run_ocomp_terminal_request(ctx, &fixture.scope),
    )
    .expect("production retry request");
    let retry_intent_id = public_intent(&mut provider, fixture.wwd);
    prop_assert!(
        retry_intent_id != intent_id,
        "composition: request-finality-open-expiry-retry"
    );
    PersistedModel {
        wwd_status: WwdStatus::OffchainPending,
        membership: WwdMembership::Active,
        job: ModelJob::AwaitingFinality,
        pending_nonce: 1,
        attempt: 1,
        has_terminal_receipt: false,
        has_active_generation: false,
    }
    .observe(&mut provider, fixture.wwd, retry_intent_id)?;

    Ok(())
}

#[derive(Clone, Debug)]
enum BeforeSecondProbe {
    DuplicateFirst,
    Invalid(u8),
}

#[derive(Clone, Debug)]
enum BeforeQuorumProbe {
    DuplicateFirst,
    DuplicateSecond,
    Invalid(u8),
}

#[derive(Clone, Debug)]
struct QuorumHistory {
    timestamp_jitter: u8,
    rotation: u8,
    before_second: Vec<BeforeSecondProbe>,
    before_quorum: Vec<BeforeQuorumProbe>,
}

fn quorum_history_strategy() -> impl Strategy<Value = QuorumHistory> {
    let before_second = prop_oneof![
        Just(BeforeSecondProbe::DuplicateFirst),
        any::<u8>().prop_map(BeforeSecondProbe::Invalid),
    ];
    let before_quorum = prop_oneof![
        Just(BeforeQuorumProbe::DuplicateFirst),
        Just(BeforeQuorumProbe::DuplicateSecond),
        any::<u8>().prop_map(BeforeQuorumProbe::Invalid),
    ];
    (
        any::<u8>(),
        any::<u8>(),
        vec(before_second, 0..=12),
        vec(before_quorum, 0..=12),
    )
        .prop_map(
            |(timestamp_jitter, rotation, before_second, before_quorum)| QuorumHistory {
                timestamp_jitter,
                rotation,
                before_second,
                before_quorum,
            },
        )
}

fn submit_invalid_vote(
    fixture: &mut ActivationFixture,
    validator_index: u8,
) -> outbe_primitives::error::Result<alloy_primitives::Bytes> {
    let mut rejected = fixture.signed_result_vote(validator_index % 4);
    rejected.signature_rs[0] ^= 1;
    let calldata =
        encode_submit_lysis_result_calldata(&rejected, &fixture.limits).expect("rejected calldata");
    fixture
        .provider
        .enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::VerifiedResultVote);
    StorageHandle::enter(&mut fixture.provider, |storage| {
        commands::submit_verified_result_vote(storage, &fixture.scope, &calldata, U256::ZERO, false)
    })
}

fn run_quorum_history(history: QuorumHistory) -> TestCaseResult {
    let mut fixture =
        ActivationFixture::new_voting(20, 1_010 + u64::from(history.timestamp_jitter % 7), true);
    let rotation = usize::from(history.rotation % 4);
    let validators = [
        (rotation % 4) as u8,
        ((rotation + 1) % 4) as u8,
        ((rotation + 2) % 4) as u8,
    ];
    let voting = PersistedModel {
        wwd_status: WwdStatus::OffchainPending,
        membership: WwdMembership::Active,
        job: ModelJob::VotingOpen,
        pending_nonce: 0,
        attempt: 0,
        has_terminal_receipt: false,
        has_active_generation: false,
    };
    voting.observe(&mut fixture.provider, TEST_WWD, fixture.intent_id)?;

    submit_vote(&mut fixture, validators[0], false).expect("first production vote");
    voting.observe(&mut fixture.provider, TEST_WWD, fixture.intent_id)?;

    for probe in history.before_second {
        let before = checkpoint(&fixture.provider, &fixture.scope);
        match probe {
            BeforeSecondProbe::DuplicateFirst => {
                submit_vote(&mut fixture, validators[0], false)
                    .expect("generated duplicate first vote");
            }
            BeforeSecondProbe::Invalid(index) => {
                prop_assert!(
                    submit_invalid_vote(&mut fixture, index).is_err(),
                    "distribution: generated invalid signature"
                );
            }
        }
        prop_assert_eq!(checkpoint(&fixture.provider, &fixture.scope), before);
        voting.observe(&mut fixture.provider, TEST_WWD, fixture.intent_id)?;
    }

    submit_vote(&mut fixture, validators[1], false).expect("second production vote");
    voting.observe(&mut fixture.provider, TEST_WWD, fixture.intent_id)?;

    for probe in history.before_quorum {
        let before = checkpoint(&fixture.provider, &fixture.scope);
        match probe {
            BeforeQuorumProbe::DuplicateFirst => {
                submit_vote(&mut fixture, validators[0], false)
                    .expect("generated duplicate first vote before quorum");
            }
            BeforeQuorumProbe::DuplicateSecond => {
                submit_vote(&mut fixture, validators[1], false)
                    .expect("generated duplicate second vote before quorum");
            }
            BeforeQuorumProbe::Invalid(index) => {
                prop_assert!(
                    submit_invalid_vote(&mut fixture, index).is_err(),
                    "distribution: generated invalid signature before quorum"
                );
            }
        }
        prop_assert_eq!(checkpoint(&fixture.provider, &fixture.scope), before);
        voting.observe(&mut fixture.provider, TEST_WWD, fixture.intent_id)?;
    }

    let before_fault = checkpoint(&fixture.provider, &fixture.scope);
    fixture.provider.fail_after_mutation_at(0);
    let failed = submit_vote(&mut fixture, validators[2], true);
    prop_assert!(failed.is_err(), "distribution: q-forming rollback");
    prop_assert!(fixture.provider.clear_mutation_failure() > 0);
    prop_assert_eq!(checkpoint(&fixture.provider, &fixture.scope), before_fault);
    voting.observe(&mut fixture.provider, TEST_WWD, fixture.intent_id)?;

    submit_vote(&mut fixture, validators[2], true).expect("exact q-forming retry");
    PersistedModel {
        wwd_status: WwdStatus::Completed,
        membership: WwdMembership::Closed,
        job: ModelJob::Completed,
        pending_nonce: 0,
        attempt: 0,
        has_terminal_receipt: true,
        has_active_generation: true,
    }
    .observe(&mut fixture.provider, TEST_WWD, fixture.intent_id)?;
    prop_assert!(
        validators.windows(2).all(|pair| pair[0] != pair[1]),
        "composition: verified-vote-quorum-activation"
    );
    Ok(())
}

fn exercise_terminal_request_fault_matrix(day_type: WwdDayType) {
    let mut control = HashMapStorageProvider::new(chain::CHAIN_ID);
    let control_fixture = prepare_request_fixture_with_day_type(&mut control, true, day_type);
    control.fail_mutation_at(usize::MAX);
    runtime_command(
        &mut control,
        MetadosisMutationPurposeTag::OcompLifecycle,
        control_fixture.block_number,
        control_fixture.block_time,
        |ctx| commands::run_ocomp_terminal_request(ctx, &control_fixture.scope),
    )
    .expect("clean production terminal request");
    let mutation_count = control.clear_mutation_failure();
    assert!(
        mutation_count >= 8,
        "request must cross storage, index, and event mutation boundaries"
    );
    let clean_after = checkpoint(&control, &control_fixture.scope);
    assert_eq!(
        control
            .get_ordered_events()
            .iter()
            .filter(|log| IMetadosis::OffchainJobRequested::decode_log(log).is_ok())
            .count(),
        1
    );

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
        let fixture = prepare_request_fixture_with_day_type(&mut provider, true, day_type);
        let before = checkpoint(&provider, &fixture.scope);
        provider.fail_after_mutation_at(operation);

        let error = runtime_command(
            &mut provider,
            MetadosisMutationPurposeTag::OcompLifecycle,
            fixture.block_number,
            fixture.block_time,
            |ctx| commands::run_ocomp_terminal_request(ctx, &fixture.scope),
        )
        .expect_err("every injected request mutation failure must propagate");
        assert!(matches!(
            error,
            outbe_primitives::error::PrecompileError::Storage(_)
        ));
        assert_eq!(
            provider.clear_mutation_failure(),
            operation + 1,
            "fault must occur after the selected persistent mutation"
        );
        assert_eq!(
            checkpoint(&provider, &fixture.scope),
            before,
            "production request fault {operation} leaked storage, ordered events, or CE work"
        );

        runtime_command(
            &mut provider,
            MetadosisMutationPurposeTag::OcompLifecycle,
            fixture.block_number,
            fixture.block_time,
            |ctx| commands::run_ocomp_terminal_request(ctx, &fixture.scope),
        )
        .expect("exact production request retry");
        assert_eq!(
            checkpoint(&provider, &fixture.scope),
            clean_after,
            "request retry after fault {operation} diverged from a clean execution"
        );
    }
}

#[test]
fn production_terminal_request_rolls_back_every_mutation_boundary_and_retries_exactly() {
    exercise_terminal_request_fault_matrix(WwdDayType::Green);
    exercise_terminal_request_fault_matrix(WwdDayType::Red);
}

#[test]
fn production_deferred_request_rolls_back_every_mutation_and_retries_exactly() {
    let mut control = HashMapStorageProvider::new(chain::CHAIN_ID);
    let control_fixture = prepare_request_fixture(&mut control, false);
    control.fail_after_mutation_at(usize::MAX);
    runtime_command(
        &mut control,
        MetadosisMutationPurposeTag::OcompLifecycle,
        control_fixture.block_number,
        control_fixture.block_time,
        |ctx| commands::run_ocomp_terminal_request(ctx, &control_fixture.scope),
    )
    .expect("clean deferred production request");
    let mutation_count = control.clear_mutation_failure();
    assert!(
        mutation_count >= 2,
        "deferred request must rewrite the FSM and canonical READY index"
    );
    let clean_after = checkpoint(&control, &control_fixture.scope);

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
        let fixture = prepare_request_fixture(&mut provider, false);
        let before = checkpoint(&provider, &fixture.scope);
        provider.fail_after_mutation_at(operation);

        assert!(matches!(
            runtime_command(
                &mut provider,
                MetadosisMutationPurposeTag::OcompLifecycle,
                fixture.block_number,
                fixture.block_time,
                |ctx| commands::run_ocomp_terminal_request(ctx, &fixture.scope),
            ),
            Err(outbe_primitives::error::PrecompileError::Storage(_))
        ));
        assert_eq!(provider.clear_mutation_failure(), operation + 1);
        assert_eq!(checkpoint(&provider, &fixture.scope), before);

        runtime_command(
            &mut provider,
            MetadosisMutationPurposeTag::OcompLifecycle,
            fixture.block_number,
            fixture.block_time,
            |ctx| commands::run_ocomp_terminal_request(ctx, &fixture.scope),
        )
        .expect("exact deferred production request retry");
        assert_eq!(checkpoint(&provider, &fixture.scope), clean_after);
    }
}

#[test]
fn production_finality_record_rolls_back_every_mutation_and_retries_exactly() {
    let prepare = |provider: &mut HashMapStorageProvider| {
        let fixture = prepare_request_fixture(provider, true);
        runtime_command(
            provider,
            MetadosisMutationPurposeTag::OcompLifecycle,
            fixture.block_number,
            fixture.block_time,
            |ctx| commands::run_ocomp_terminal_request(ctx, &fixture.scope),
        )
        .expect("production terminal request before finality");
        let finality_height = fixture.block_number + 2;
        let certified = MetadosisCertifiedFinalityBinding::new(
            chain::CHAIN_ID,
            finality_height,
            fixture.block_number,
            B256::repeat_byte(0x46),
            B256::repeat_byte(0x98),
        );
        (fixture, finality_height, certified)
    };

    let mut control = HashMapStorageProvider::new(chain::CHAIN_ID);
    let (control_fixture, finality_height, certified) = prepare(&mut control);
    control.fail_after_mutation_at(usize::MAX);
    assert!(runtime_command(
        &mut control,
        MetadosisMutationPurposeTag::CertifiedFinality,
        finality_height,
        control_fixture.block_time + 2,
        |ctx| commands::record_certified_parent_finality(ctx, &certified),
    )
    .expect("clean production finality"));
    let mutation_count = control.clear_mutation_failure();
    assert!(
        mutation_count >= 1,
        "finality must persist its exact job binding"
    );
    let clean_after = checkpoint(&control, &control_fixture.scope);

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
        let (fixture, finality_height, certified) = prepare(&mut provider);
        let before = checkpoint(&provider, &fixture.scope);
        provider.fail_after_mutation_at(operation);

        assert!(matches!(
            runtime_command(
                &mut provider,
                MetadosisMutationPurposeTag::CertifiedFinality,
                finality_height,
                fixture.block_time + 2,
                |ctx| commands::record_certified_parent_finality(ctx, &certified),
            ),
            Err(outbe_primitives::error::PrecompileError::Storage(_))
        ));
        assert_eq!(provider.clear_mutation_failure(), operation + 1);
        assert_eq!(checkpoint(&provider, &fixture.scope), before);

        assert!(runtime_command(
            &mut provider,
            MetadosisMutationPurposeTag::CertifiedFinality,
            finality_height,
            fixture.block_time + 2,
            |ctx| commands::record_certified_parent_finality(ctx, &certified),
        )
        .expect("exact production finality retry"));
        assert_eq!(checkpoint(&provider, &fixture.scope), clean_after);
    }
}

#[test]
fn production_voting_open_rolls_back_every_mutation_and_retries_exactly() {
    let prepare = |provider: &mut HashMapStorageProvider| {
        let fixture = prepare_request_fixture(provider, true);
        runtime_command(
            provider,
            MetadosisMutationPurposeTag::OcompLifecycle,
            fixture.block_number,
            fixture.block_time,
            |ctx| commands::run_ocomp_terminal_request(ctx, &fixture.scope),
        )
        .expect("production request before voting open");
        let intent_id = public_intent(provider, fixture.wwd);
        let finality_height = fixture.block_number + 2;
        let certified = MetadosisCertifiedFinalityBinding::new(
            chain::CHAIN_ID,
            finality_height,
            fixture.block_number,
            B256::repeat_byte(0x46),
            B256::repeat_byte(0x98),
        );
        assert!(runtime_command(
            provider,
            MetadosisMutationPurposeTag::CertifiedFinality,
            finality_height,
            fixture.block_time + 2,
            |ctx| commands::record_certified_parent_finality(ctx, &certified),
        )
        .expect("production finality before voting open"));
        let open_height = StorageHandle::enter(provider, |storage| {
            let encoded = api::get_offchain_job(storage, intent_id).expect("finalized job");
            OcompJobRecordV1::decode_canonical(&encoded, &poc_schema_limits())
                .expect("finalized job decodes")
                .finalized
                .expect("finality binding")
                .open_height
        });
        (fixture, open_height)
    };

    let mut control = HashMapStorageProvider::new(chain::CHAIN_ID);
    let (control_fixture, open_height) = prepare(&mut control);
    control.fail_after_mutation_at(usize::MAX);
    runtime_command(
        &mut control,
        MetadosisMutationPurposeTag::OcompLifecycle,
        open_height,
        control_fixture.block_time + 3,
        commands::run_ocomp_lifecycle_begin,
    )
    .expect("clean production voting open");
    let mutation_count = control.clear_mutation_failure();
    assert!(
        mutation_count >= 5,
        "voting open must persist slots, job, FSM, scheduler and deadline index"
    );
    let clean_after = checkpoint(&control, &control_fixture.scope);

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
        let (fixture, open_height) = prepare(&mut provider);
        let before = checkpoint(&provider, &fixture.scope);
        provider.fail_after_mutation_at(operation);

        assert!(matches!(
            runtime_command(
                &mut provider,
                MetadosisMutationPurposeTag::OcompLifecycle,
                open_height,
                fixture.block_time + 3,
                commands::run_ocomp_lifecycle_begin,
            ),
            Err(outbe_primitives::error::PrecompileError::Storage(_))
        ));
        assert_eq!(provider.clear_mutation_failure(), operation + 1);
        assert_eq!(checkpoint(&provider, &fixture.scope), before);

        runtime_command(
            &mut provider,
            MetadosisMutationPurposeTag::OcompLifecycle,
            open_height,
            fixture.block_time + 3,
            commands::run_ocomp_lifecycle_begin,
        )
        .expect("exact production voting-open retry");
        assert_eq!(checkpoint(&provider, &fixture.scope), clean_after);
    }
}

#[test]
fn production_retryable_expiry_rolls_back_every_mutation_then_retries_exactly() {
    let mut control = HashMapStorageProvider::new(chain::CHAIN_ID);
    let control_fixture = prepare_retryable_expiry_production_fixture(&mut control);
    control.fail_mutation_at(usize::MAX);
    runtime_command(
        &mut control,
        MetadosisMutationPurposeTag::OcompLifecycle,
        control_fixture.deadline_height,
        control_fixture.command_time,
        commands::run_ocomp_lifecycle_begin,
    )
    .expect("clean retryable expiry");
    let mutation_count = control.clear_mutation_failure();
    assert!(
        mutation_count >= 6,
        "retryable expiry must cross terminal record, outer READY, FSM, READY index, and live scheduler boundaries"
    );
    let clean_after = checkpoint(&control, &control_fixture.scope);

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
        let fixture = prepare_retryable_expiry_production_fixture(&mut provider);
        let before_fault = checkpoint(&provider, &fixture.scope);
        provider.fail_after_mutation_at(operation);

        let error = runtime_command(
            &mut provider,
            MetadosisMutationPurposeTag::OcompLifecycle,
            fixture.deadline_height,
            fixture.command_time,
            commands::run_ocomp_lifecycle_begin,
        )
        .expect_err("every retryable-expiry mutation failure must propagate");
        assert!(matches!(
            error,
            outbe_primitives::error::PrecompileError::Storage(_)
        ));
        assert_eq!(
            provider.clear_mutation_failure(),
            operation + 1,
            "retryable-expiry fault must occur at the selected mutation"
        );
        assert_eq!(
            checkpoint(&provider, &fixture.scope),
            before_fault,
            "retryable-expiry fault {operation} leaked storage, events, or CE work"
        );

        runtime_command(
            &mut provider,
            MetadosisMutationPurposeTag::OcompLifecycle,
            fixture.deadline_height,
            fixture.command_time,
            commands::run_ocomp_lifecycle_begin,
        )
        .expect("exact retryable-expiry retry");
        assert_eq!(
            checkpoint(&provider, &fixture.scope),
            clean_after,
            "retryable-expiry retry after fault {operation} diverged from clean execution"
        );
    }
}

#[test]
fn production_response_window_close_and_final_expiry_roll_back_every_mutation_then_retry_exactly() {
    let mut control = HashMapStorageProvider::new(chain::CHAIN_ID);
    let control_fixture = prepare_expiring_production_fixture(&mut control);
    let control_carry_over_before = StorageHandle::enter(&mut control, |storage| {
        PromisLimitContract::new(storage)
            .get_total_unallocated()
            .unwrap()
    });
    control.fail_mutation_at(usize::MAX);
    runtime_command(
        &mut control,
        MetadosisMutationPurposeTag::OcompLifecycle,
        control_fixture.deadline_height,
        control_fixture.command_time,
        commands::run_ocomp_lifecycle_begin,
    )
    .expect("clean production final expiry");
    let mutation_count = control.clear_mutation_failure();
    assert!(
        mutation_count >= 8,
        "final expiry must cross inner FSM, Promis routing, outer status, membership, and event boundaries"
    );
    assert_exhausted_production_state(&mut control, &control_fixture, control_carry_over_before);
    let clean_after = checkpoint(&control, &control_fixture.scope);

    let before_replay = checkpoint(&control, &control_fixture.scope);
    runtime_command(
        &mut control,
        MetadosisMutationPurposeTag::OcompLifecycle,
        control_fixture.deadline_height,
        control_fixture.command_time,
        commands::run_ocomp_lifecycle_begin,
    )
    .expect("completed final-expiry replay is a no-op");
    assert_eq!(
        checkpoint(&control, &control_fixture.scope),
        before_replay,
        "final-expiry replay credited Promis or mutated closed state"
    );
    assert_exhausted_production_state(&mut control, &control_fixture, control_carry_over_before);

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
        let fixture = prepare_expiring_production_fixture(&mut provider);
        let carry_over_before = StorageHandle::enter(&mut provider, |storage| {
            PromisLimitContract::new(storage)
                .get_total_unallocated()
                .unwrap()
        });
        let before_fault = checkpoint(&provider, &fixture.scope);
        provider.fail_after_mutation_at(operation);

        let error = runtime_command(
            &mut provider,
            MetadosisMutationPurposeTag::OcompLifecycle,
            fixture.deadline_height,
            fixture.command_time,
            commands::run_ocomp_lifecycle_begin,
        )
        .expect_err("every response-close/final-expiry mutation failure must propagate");
        assert!(matches!(
            error,
            outbe_primitives::error::PrecompileError::Storage(_)
        ));
        assert_eq!(
            provider.clear_mutation_failure(),
            operation + 1,
            "response-close/final-expiry fault must occur at the selected mutation"
        );
        assert_eq!(
            checkpoint(&provider, &fixture.scope),
            before_fault,
            "response-close/final-expiry fault {operation} leaked storage, events, or CE work"
        );

        runtime_command(
            &mut provider,
            MetadosisMutationPurposeTag::OcompLifecycle,
            fixture.deadline_height,
            fixture.command_time,
            commands::run_ocomp_lifecycle_begin,
        )
        .expect("exact response-close/final-expiry retry");
        assert_eq!(
            checkpoint(&provider, &fixture.scope),
            clean_after,
            "response-close/final-expiry retry after fault {operation} diverged from clean execution"
        );
        assert_exhausted_production_state(&mut provider, &fixture, carry_over_before);
    }
}

fn finalized_open_and_deadline(
    provider: &mut HashMapStorageProvider,
    intent_id: B256,
) -> (u64, u64) {
    StorageHandle::enter(provider, |storage| {
        let encoded = api::get_offchain_job(storage, intent_id).expect("finalized public job");
        let record = OcompJobRecordV1::decode_canonical(&encoded, &poc_schema_limits())
            .expect("finalized public job decodes");
        let finalized = record.finalized.expect("finality binding");
        (finalized.open_height, finalized.deadline_height)
    })
}

fn drive_day_to_voting_open(
    provider: &mut HashMapStorageProvider,
    scope: &outbe_compressed_entities::ExecutionScope,
    expected_wwd: outbe_common::WorldwideDay,
    request_block: u64,
    request_time: u64,
    finality_hashes: (B256, B256),
) -> (B256, u64) {
    runtime_command(
        provider,
        MetadosisMutationPurposeTag::OcompLifecycle,
        request_block,
        request_time,
        |ctx| commands::run_ocomp_terminal_request(ctx, scope),
    )
    .expect("production terminal request");
    let intent_id = public_intent(provider, expected_wwd);

    let finality_height = request_block + 2;
    let certified = MetadosisCertifiedFinalityBinding::new(
        chain::CHAIN_ID,
        finality_height,
        request_block,
        finality_hashes.0,
        finality_hashes.1,
    );
    assert!(runtime_command(
        provider,
        MetadosisMutationPurposeTag::CertifiedFinality,
        finality_height,
        request_time + 1,
        |ctx| commands::record_certified_parent_finality(ctx, &certified),
    )
    .expect("production certified finality"));

    let (open_height, deadline_height) = finalized_open_and_deadline(provider, intent_id);
    runtime_command(
        provider,
        MetadosisMutationPurposeTag::OcompLifecycle,
        open_height,
        request_time + 2,
        commands::run_ocomp_lifecycle_begin,
    )
    .expect("production voting open");
    (intent_id, deadline_height)
}

fn seed_near_cap_history(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_common::WorldwideDay,
    live_intent_id: B256,
) {
    StorageHandle::enter(provider, |storage| {
        let limits = poc_schema_limits();
        let fsm_limits = crate::ocomp::state::JobFsmLimits {
            max_terminal_records: 365,
        };
        let mut metadosis = crate::schema::MetadosisContract::new(storage.clone());
        metadosis
            .fixture_seed_ocomp_terminal_history(
                live_intent_id,
                fsm_limits.max_terminal_records - 1,
                &limits,
                fsm_limits,
            )
            .expect("canonical near-cap terminal history");
        assert_eq!(
            metadosis.terminal_intent_count(wwd).unwrap(),
            364,
            "seeded history must charge only its own day's budget"
        );
    });
}

/// THE per-day-cap regression: with one day's terminal budget fully spent, a
/// fresh day's first terminal event must still be accepted. On the pre-fix
/// global index this fails inside `expire_ocomp_job` with
/// `"OCOMP terminal record cap exhausted"`.
#[test]
fn terminal_cap_is_per_worldwide_day_not_global() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = prepare_ready_days_fixture(&mut provider, true);
    let limits = poc_schema_limits();
    let fsm_limits = crate::ocomp::state::JobFsmLimits {
        max_terminal_records: 365,
    };

    // First day: request -> finality -> voting open -> near-cap history ->
    // final expiry. The day fails gracefully and owns its entire 365 budget.
    let (first_intent, _) = drive_day_to_voting_open(
        &mut provider,
        &fixture.scope,
        fixture.first_wwd,
        fixture.block_number,
        fixture.block_time,
        (B256::repeat_byte(0x46), B256::repeat_byte(0x98)),
    );
    seed_near_cap_history(&mut provider, fixture.first_wwd, first_intent);
    let (_, first_deadline) = StorageHandle::enter(&mut provider, |storage| {
        let projection = crate::schema::MetadosisContract::new(storage)
            .ocomp_fsm_state(fixture.first_wwd, &limits, fsm_limits)
            .unwrap()
            .projection();
        (
            projection.live_intent_id.expect("seeded live intent"),
            projection.deadline_height.expect("seeded live deadline"),
        )
    });
    runtime_command(
        &mut provider,
        MetadosisMutationPurposeTag::OcompLifecycle,
        first_deadline,
        fixture.block_time + 3,
        commands::run_ocomp_lifecycle_begin,
    )
    .expect("first-day final expiry fails the day gracefully, not the block");

    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = crate::schema::MetadosisContract::new(storage.clone());
        assert_eq!(
            metadosis.terminal_intent_count(fixture.first_wwd).unwrap(),
            365
        );
        let projection = api::worldwide_day(storage, fixture.first_wwd)
            .unwrap()
            .expect("exhausted first day persists");
        assert_eq!(projection.status, WwdStatus::Failed);
        assert_eq!(projection.membership, WwdMembership::Closed);
    });

    // Later day: the first day's exhausted budget must not leak into it.
    let later_request_block = first_deadline + 1;
    let (later_intent, later_deadline) = drive_day_to_voting_open(
        &mut provider,
        &fixture.scope,
        fixture.later_wwd,
        later_request_block,
        fixture.block_time + 4,
        (B256::repeat_byte(0x47), B256::repeat_byte(0x99)),
    );
    runtime_command(
        &mut provider,
        MetadosisMutationPurposeTag::OcompLifecycle,
        later_deadline,
        fixture.block_time + 7,
        commands::run_ocomp_lifecycle_begin,
    )
    .expect("a fresh day's first expiry must not be blocked by another day's exhausted budget");

    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = crate::schema::MetadosisContract::new(storage.clone());
        assert_eq!(
            metadosis.terminal_intent_count(fixture.later_wwd).unwrap(),
            1
        );
        assert_eq!(
            metadosis.terminal_intent_count(fixture.first_wwd).unwrap(),
            365
        );
        let state = metadosis
            .ocomp_fsm_state(fixture.later_wwd, &limits, fsm_limits)
            .expect("later day's FSM stays readable");
        let projection = state.projection();
        assert_eq!(projection.terminal_records, 1);
        assert_eq!(projection.phase, DayPhase::Ready);
        assert_eq!(
            metadosis
                .latest_terminal_job_record(fixture.later_wwd, &limits)
                .unwrap()
                .expect("later day's own terminal evidence")
                .0,
            later_intent
        );
        crate::aggregate::ValidatedWwdAggregate::load_and_validate(storage)
            .expect("post-exhaustion two-day state passes production aggregate validation");
    });
}

/// Two concurrently live days each own an independent 365-record budget: their
/// combined history (728 records) is impossible under a global index.
#[test]
fn two_concurrent_live_days_do_not_share_terminal_budget() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = prepare_ready_days_fixture(&mut provider, true);
    let limits = poc_schema_limits();
    let fsm_limits = crate::ocomp::state::JobFsmLimits {
        max_terminal_records: 365,
    };

    let (first_intent, _) = drive_day_to_voting_open(
        &mut provider,
        &fixture.scope,
        fixture.first_wwd,
        fixture.block_number,
        fixture.block_time,
        (B256::repeat_byte(0x46), B256::repeat_byte(0x98)),
    );
    let (later_intent, _) = drive_day_to_voting_open(
        &mut provider,
        &fixture.scope,
        fixture.later_wwd,
        // Stay well inside the first day's response window (open + 64 blocks):
        // the begin-zone enforces exact response-deadline heights.
        fixture.block_number + 5,
        fixture.block_time + 4,
        (B256::repeat_byte(0x47), B256::repeat_byte(0x99)),
    );

    seed_near_cap_history(&mut provider, fixture.first_wwd, first_intent);
    seed_near_cap_history(&mut provider, fixture.later_wwd, later_intent);

    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = crate::schema::MetadosisContract::new(storage);
        assert_eq!(
            metadosis.terminal_intent_count(fixture.first_wwd).unwrap(),
            364
        );
        assert_eq!(
            metadosis.terminal_intent_count(fixture.later_wwd).unwrap(),
            364
        );
        for wwd in [fixture.first_wwd, fixture.later_wwd] {
            let projection = metadosis
                .ocomp_fsm_state(wwd, &limits, fsm_limits)
                .expect("each day's FSM stays readable at combined 728 records")
                .projection();
            assert_eq!(projection.terminal_records, 364);
            assert_eq!(projection.phase, DayPhase::OffchainPending);
        }
    });
}

fn runner(test_name: &'static str, seed: &[u8; 32]) -> TestRunner {
    let config = Config {
        cases: GENERATED_CASES,
        failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
            "proptest-regressions",
        ))),
        source_file: Some(file!()),
        test_name: Some(test_name),
        ..Config::default()
    };
    TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, seed))
}

#[test]
fn generated_expiry_retry_histories_match_production_adapters_and_composition() {
    let distribution = Arc::new(Mutex::new(OcompDistribution::default()));
    let observed = distribution.clone();
    runner(
        concat!(
            module_path!(),
            "::generated_expiry_retry_histories_match_production_adapters_and_composition"
        ),
        &OCOMP_ADAPTER_SEED,
    )
    .run(&expiry_history_strategy(), move |history| {
        observed.lock().unwrap().observe_expiry(&history);
        run_expiry_retry_history(history)
    })
    .unwrap();
    let distribution = distribution.lock().unwrap();
    distribution.assert_expiry_complete();
    distribution.print_retained("ocomp-expiry-retry");
}

#[test]
fn generated_quorum_histories_match_production_adapters_and_composition() {
    let mut quorum_seed = OCOMP_ADAPTER_SEED;
    quorum_seed[0] ^= 0x51;
    let distribution = Arc::new(Mutex::new(OcompDistribution::default()));
    let observed = distribution.clone();
    runner(
        concat!(
            module_path!(),
            "::generated_quorum_histories_match_production_adapters_and_composition"
        ),
        &quorum_seed,
    )
    .run(&quorum_history_strategy(), move |history| {
        observed.lock().unwrap().observe_quorum(&history);
        run_quorum_history(history)
    })
    .unwrap();
    let distribution = distribution.lock().unwrap();
    distribution.assert_quorum_complete();
    distribution.print_retained("ocomp-quorum");
}
