use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use alloy_primitives::{Address, U256};
use outbe_metadosis::{
    api::{
        capacity_forfeiture_receipt, day_limit_formation_receipt, missed_offering_receipt,
        worldwide_day, worldwide_days,
    },
    constants::MAX_RETAINED_WWDS,
    WwdMembership, WwdStatus,
};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::{CYCLE_ADDRESS, METADOSIS_ADDRESS},
    block::{BlockRuntimeContext, BlockRuntimeContext as RuntimeContext},
    storage::{hashmap::HashMapStorageProvider, MetadosisMutationPurposeTag, StorageHandle},
};
use proptest::{
    collection::vec,
    prop_assert, prop_assert_eq, prop_oneof,
    strategy::{Just, Strategy},
    test_runner::{
        Config, FileFailurePersistence, RngAlgorithm, TestCaseResult, TestRng, TestRunner,
    },
};

use super::{
    account_parent, anchor_genesis, block_ctx, capacity_scenario, cycle_storage,
    run_cycle_lifecycle, run_cycle_lifecycle_at_activation, CapacityScenario, GENESIS_TS,
    SECONDS_PER_DAY,
};

const GENERATED_OUTER_WWD_SEED: [u8; 32] = *b"metadosis-outer-wwd-model-seed!!";
const GENESIS_WWD: WorldwideDay = WorldwideDay::new(20_240_101);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelPhase {
    Forming,
    LookbackDelay,
    Offering,
    Waiting,
    Completed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OuterWwdModel {
    phase: ModelPhase,
    membership: WwdMembership,
    has_terminal_receipt: bool,
    has_capacity_forfeiture: bool,
}

impl OuterWwdModel {
    const fn genesis() -> Self {
        Self {
            phase: ModelPhase::Forming,
            membership: WwdMembership::Active,
            has_terminal_receipt: false,
            has_capacity_forfeiture: false,
        }
    }

    fn observe(
        self,
        storage: &mut HashMapStorageProvider,
        wwd: WorldwideDay,
        replay_receipt: &Option<outbe_metadosis::api::DayLimitFormationReceipt>,
    ) -> TestCaseResult {
        let (actual, terminal_receipt, forfeiture, replay) =
            StorageHandle::enter(storage, |handle| {
                let actual = worldwide_day(handle.clone(), wwd).expect("typed WWD query");
                let missed = missed_offering_receipt(handle.clone(), wwd).unwrap();
                let forfeiture = capacity_forfeiture_receipt(handle.clone(), wwd).unwrap();
                (
                    actual,
                    missed.is_some() || forfeiture.is_some(),
                    forfeiture,
                    day_limit_formation_receipt(handle, wwd).unwrap(),
                )
            });
        let actual = actual.expect("model WWD exists");
        let expected_status = match self.phase {
            ModelPhase::Forming => WwdStatus::Forming,
            ModelPhase::LookbackDelay => WwdStatus::LookbackDelay,
            ModelPhase::Offering => WwdStatus::Offering,
            ModelPhase::Waiting => WwdStatus::Waiting,
            ModelPhase::Completed => WwdStatus::Completed,
        };
        prop_assert_eq!(actual.status, expected_status);
        prop_assert_eq!(actual.membership, self.membership);
        prop_assert_eq!(terminal_receipt, self.has_terminal_receipt);
        prop_assert_eq!(forfeiture.is_some(), self.has_capacity_forfeiture);
        prop_assert_eq!(&replay, replay_receipt);
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Coverage {
    terminal: usize,
    illegal: usize,
    duplicate: usize,
    rollback: usize,
    cap_minus_one: usize,
    cap: usize,
    cap_plus_one: usize,
    forfeiture: usize,
    multi_record: usize,
}

impl Coverage {
    fn merge(&mut self, other: &Self) {
        self.terminal += other.terminal;
        self.illegal += other.illegal;
        self.duplicate += other.duplicate;
        self.rollback += other.rollback;
        self.cap_minus_one += other.cap_minus_one;
        self.cap += other.cap;
        self.cap_plus_one += other.cap_plus_one;
        self.forfeiture += other.forfeiture;
        self.multi_record += other.multi_record;
    }

    fn assert_complete(&self, cases: usize) {
        for (label, count) in [
            ("terminal", self.terminal),
            ("illegal", self.illegal),
            ("duplicate", self.duplicate),
            ("rollback", self.rollback),
            ("cap-1", self.cap_minus_one),
            ("cap", self.cap),
            ("cap+1", self.cap_plus_one),
            ("forfeiture", self.forfeiture),
            ("multi-record", self.multi_record),
        ] {
            assert!(count > 0, "generated distribution missing {label}");
        }
        println!(
            "METADOSIS_CASE_DISTRIBUTION_V1 {{\"suite\":\"outer-wwd\",\"seed_family\":\"metadosis-outer-wwd-model\",\"cases\":{cases},\"terminal\":{},\"illegal\":{},\"duplicate\":{},\"rollback\":{},\"cap-1\":{},\"cap\":{},\"cap+1\":{},\"forfeiture\":{},\"multi-record\":{}}}",
            self.terminal,
            self.illegal,
            self.duplicate,
            self.rollback,
            self.cap_minus_one,
            self.cap,
            self.cap_plus_one,
            self.forfeiture,
            self.multi_record,
        );
    }
}

fn run_block(
    storage: &mut HashMapStorageProvider,
    block_number: u64,
    timestamp: u64,
) -> outbe_primitives::error::Result<()> {
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 128);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(block_number, timestamp), handle);
        if block_number == 1 {
            anchor_genesis(&ctx);
        }
        account_parent(&ctx, block_number);
        run_cycle_lifecycle(&ctx)
    })
}

fn hourly_fire_at(timestamp: u64) -> u64 {
    timestamp.div_ceil(3_600) * 3_600
}

fn cycle_metadosis_snapshot(storage: &HashMapStorageProvider) -> BTreeMap<(Address, U256), U256> {
    storage
        .storage
        .iter()
        .filter(|((address, _), _)| matches!(*address, METADOSIS_ADDRESS | CYCLE_ADDRESS))
        .map(|(key, value)| (*key, *value))
        .collect()
}

#[derive(Clone, Debug)]
enum OuterProbe {
    Duplicate,
    Backward(u8),
    BeforeOffering(u8),
    AdvanceOffering,
    FaultAtOffering,
}

#[derive(Clone, Debug)]
struct OuterHistory {
    genesis_jitter: u8,
    probes: Vec<OuterProbe>,
}

fn outer_history_strategy() -> impl Strategy<Value = OuterHistory> {
    let probe = prop_oneof![
        Just(OuterProbe::Duplicate),
        (1_u8..=30).prop_map(OuterProbe::Backward),
        (1_u8..=30).prop_map(OuterProbe::BeforeOffering),
        Just(OuterProbe::AdvanceOffering),
        Just(OuterProbe::FaultAtOffering),
    ];
    (0_u8..59, vec(probe, 1..=16)).prop_map(|(genesis_jitter, probes)| OuterHistory {
        genesis_jitter,
        probes,
    })
}

fn outer_history(history: OuterHistory, coverage: &mut Coverage) -> TestCaseResult {
    let mut run = prepare_outer_history(history.genesis_jitter)?;
    for probe in history.probes {
        apply_outer_probe(&mut run, probe, coverage)?;
    }
    finish_outer_history(&mut run, coverage)
}

fn capacity_history(retained_before: usize, coverage: &mut Coverage) -> TestCaseResult {
    let _enclave = outbe_tribute::enclave_client::test_enclave::scope();
    prop_assert!(retained_before == MAX_RETAINED_WWDS - 1 || retained_before == MAX_RETAINED_WWDS);
    let victim = WorldwideDay::new(20_260_910);
    let day_limit = U256::from(100);
    let retained = super::retained_days_before(victim, retained_before);
    let CapacityScenario {
        mut storage,
        victim: victim_projection,
        ..
    } = capacity_scenario(&retained, victim, day_limit);
    let scheduled_process_time = victim_projection.scheduled_process_time;
    let fire_at = hourly_fire_at(scheduled_process_time);
    StorageHandle::enter(&mut storage, |handle| {
        super::hourly::seed_trigger_clock(&handle, fire_at)
    });

    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    StorageHandle::enter(&mut storage, |handle| {
        let ctx = RuntimeContext::new(block_ctx(30, fire_at), handle);
        account_parent(&ctx, 30);
        run_cycle_lifecycle_at_activation(&ctx, 0).unwrap();
    });

    let (projection, forfeiture) = StorageHandle::enter(&mut storage, |handle| {
        let projection = worldwide_day(handle.clone(), victim).unwrap().unwrap();
        let forfeiture = capacity_forfeiture_receipt(handle, victim).unwrap();
        (projection, forfeiture)
    });
    coverage.cap_minus_one += usize::from(retained_before == MAX_RETAINED_WWDS - 1);
    coverage.cap += usize::from(retained_before == MAX_RETAINED_WWDS);
    if retained_before < MAX_RETAINED_WWDS {
        prop_assert_eq!(projection.status, WwdStatus::Ready);
        prop_assert_eq!(projection.membership, WwdMembership::Active);
        prop_assert!(forfeiture.is_none());
    } else {
        prop_assert_eq!(projection.status, WwdStatus::Failed);
        prop_assert_eq!(projection.membership, WwdMembership::Closed);
        let receipt = forfeiture.expect("cap+1 attempt has a forfeiture receipt");
        prop_assert_eq!(receipt.retained_count_before as usize, MAX_RETAINED_WWDS);
        prop_assert_eq!(receipt.value_routed, day_limit);
        coverage.cap_plus_one += 1;
        coverage.forfeiture += 1;
    }
    Ok(())
}

#[test]
fn generated_outer_wwd_histories_match_cycle_begin_block_model() {
    let config = Config {
        cases: 64,
        failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
            "proptest-regressions",
        ))),
        source_file: Some(file!()),
        test_name: Some(concat!(
            module_path!(),
            "::generated_outer_wwd_histories_match_cycle_begin_block_model"
        )),
        ..Config::default()
    };
    let rng = TestRng::from_seed(RngAlgorithm::ChaCha, &GENERATED_OUTER_WWD_SEED);
    let mut runner = TestRunner::new_with_rng(config, rng);
    let strategy = outer_history_strategy();
    let distribution = Arc::new(Mutex::new(Coverage::default()));
    let observed = distribution.clone();
    runner
        .run(&strategy, move |history| {
            let mut coverage = Coverage::default();
            outer_history(history, &mut coverage)?;
            observed.lock().unwrap().merge(&coverage);
            Ok(())
        })
        .unwrap();
    let mut capacity_coverage = Coverage::default();
    capacity_history(MAX_RETAINED_WWDS - 1, &mut capacity_coverage).unwrap();
    capacity_history(MAX_RETAINED_WWDS, &mut capacity_coverage).unwrap();
    distribution.lock().unwrap().merge(&capacity_coverage);
    distribution.lock().unwrap().assert_complete(64);
}

struct OuterHistoryRun {
    storage: HashMapStorageProvider,
    model: OuterWwdModel,
    offering: OuterWwdModel,
    forming_fire: u64,
    offering_fire: u64,
    next_block: u64,
    replay_receipt: Option<outbe_metadosis::api::DayLimitFormationReceipt>,
}
fn initialize_outer_day(
    storage: &mut HashMapStorageProvider,
    genesis_jitter: u8,
) -> Result<
    (u64, Option<outbe_metadosis::api::DayLimitFormationReceipt>),
    proptest::test_runner::TestCaseError,
> {
    let genesis_time = GENESIS_TS + 1 + u64::from(genesis_jitter);
    run_block(storage, 1, genesis_time).expect("genesis Cycle command");
    let no_replay = None;
    OuterWwdModel::genesis().observe(storage, GENESIS_WWD, &no_replay)?;

    let forming_edge = StorageHandle::enter(storage, |handle| {
        worldwide_day(handle, GENESIS_WWD)
            .unwrap()
            .unwrap()
            .forming_end
    });
    let forming_fire = hourly_fire_at(forming_edge);
    let midnight = GENESIS_TS + SECONDS_PER_DAY;
    run_block(storage, 2, midnight + 1).expect("midnight Cycle command");
    let replay_receipt = StorageHandle::enter(storage, |handle| {
        day_limit_formation_receipt(handle, GENESIS_WWD).unwrap()
    });
    OuterWwdModel::genesis().observe(storage, GENESIS_WWD, &replay_receipt)?;

    Ok((forming_fire, replay_receipt))
}
fn prepare_outer_history(
    genesis_jitter: u8,
) -> Result<OuterHistoryRun, proptest::test_runner::TestCaseError> {
    let mut storage = cycle_storage();
    let (forming_fire, replay_receipt) = initialize_outer_day(&mut storage, genesis_jitter)?;

    run_block(&mut storage, 3, forming_fire - 1).expect("T-1 Cycle command");
    OuterWwdModel::genesis().observe(&mut storage, GENESIS_WWD, &replay_receipt)?;

    run_block(&mut storage, 4, forming_fire).expect("T Cycle command");
    let lookback = OuterWwdModel {
        phase: ModelPhase::LookbackDelay,
        membership: WwdMembership::Active,
        has_terminal_receipt: false,
        has_capacity_forfeiture: false,
    };
    lookback.observe(&mut storage, GENESIS_WWD, &replay_receipt)?;

    let duplicate_before = cycle_metadosis_snapshot(&storage);
    run_block(&mut storage, 5, forming_fire).expect("duplicate Cycle command");
    prop_assert_eq!(cycle_metadosis_snapshot(&storage), duplicate_before);
    lookback.observe(&mut storage, GENESIS_WWD, &replay_receipt)?;

    let backward_before = cycle_metadosis_snapshot(&storage);
    run_block(&mut storage, 6, forming_fire - 1).expect("backward timestamp is effect-free");
    prop_assert_eq!(cycle_metadosis_snapshot(&storage), backward_before);
    lookback.observe(&mut storage, GENESIS_WWD, &replay_receipt)?;

    run_block(&mut storage, 7, forming_fire + 1).expect("T+1 Cycle command");
    lookback.observe(&mut storage, GENESIS_WWD, &replay_receipt)?;

    let offering_edge = StorageHandle::enter(&mut storage, |handle| {
        worldwide_day(handle, GENESIS_WWD)
            .unwrap()
            .unwrap()
            .lookback_end
    });
    let offering_fire = hourly_fire_at(offering_edge);
    let offering = OuterWwdModel {
        phase: ModelPhase::Offering,
        membership: WwdMembership::Active,
        has_terminal_receipt: false,
        has_capacity_forfeiture: false,
    };
    Ok(OuterHistoryRun {
        storage,
        model: lookback,
        offering,
        forming_fire,
        offering_fire,
        next_block: 8,
        replay_receipt,
    })
}
fn outer_probe_expectation(
    run: &mut OuterHistoryRun,
    probe: OuterProbe,
    coverage: &mut Coverage,
) -> (u64, bool, bool) {
    match probe {
        OuterProbe::Duplicate => {
            coverage.duplicate += 1;
            (
                if run.model.phase == ModelPhase::Offering {
                    run.offering_fire
                } else {
                    run.forming_fire + 1
                },
                false,
                false,
            )
        }
        OuterProbe::Backward(delta) => {
            coverage.illegal += 1;
            (
                run.forming_fire.saturating_sub(u64::from(delta)),
                false,
                false,
            )
        }
        OuterProbe::BeforeOffering(delta) => {
            coverage.illegal += 1;
            (
                run.offering_fire.saturating_sub(u64::from(delta)),
                false,
                false,
            )
        }
        OuterProbe::AdvanceOffering => (
            run.offering_fire,
            false,
            run.model.phase == ModelPhase::LookbackDelay,
        ),
        OuterProbe::FaultAtOffering if run.model.phase == ModelPhase::LookbackDelay => {
            run.storage.fail_mutation_at_address(METADOSIS_ADDRESS);
            (run.offering_fire, true, false)
        }
        OuterProbe::FaultAtOffering => (run.offering_fire, false, false),
    }
}
fn apply_outer_probe(
    run: &mut OuterHistoryRun,
    probe: OuterProbe,
    coverage: &mut Coverage,
) -> TestCaseResult {
    let before_wwd = StorageHandle::enter(&mut run.storage, |handle| {
        worldwide_day(handle, GENESIS_WWD)
            .unwrap()
            .expect("generated-history WWD exists")
    });
    let (timestamp, expected_error, advances) = outer_probe_expectation(run, probe, coverage);
    let result = run_block(&mut run.storage, run.next_block, timestamp);
    run.next_block += 1;
    if expected_error {
        prop_assert!(result.is_err());
        run.storage.clear_mutation_failure();
        coverage.rollback += 1;
    } else {
        result.expect("generated Cycle command");
    }
    if advances {
        run.model = run.offering;
    } else {
        let after_wwd = StorageHandle::enter(&mut run.storage, |handle| {
            worldwide_day(handle, GENESIS_WWD)
                .unwrap()
                .expect("generated-history WWD exists")
        });
        prop_assert_eq!(after_wwd, before_wwd);
    }
    run.model
        .observe(&mut run.storage, GENESIS_WWD, &run.replay_receipt)?;
    Ok(())
}
fn assert_outer_cycle_rollback(run: &mut OuterHistoryRun, fire_at: u64) -> TestCaseResult {
    let rollback_storage = cycle_metadosis_snapshot(&run.storage);
    let rollback_events = run.storage.get_ordered_events().to_vec();
    run.storage.fail_mutation_at_address(METADOSIS_ADDRESS);
    let failed = run_block(&mut run.storage, run.next_block, fire_at);
    prop_assert!(failed.is_err());
    run.storage.clear_mutation_failure();
    prop_assert_eq!(cycle_metadosis_snapshot(&run.storage), rollback_storage);
    prop_assert_eq!(run.storage.get_ordered_events(), rollback_events.as_slice());
    run.offering
        .observe(&mut run.storage, GENESIS_WWD, &run.replay_receipt)?;

    Ok(())
}
fn finish_outer_history(run: &mut OuterHistoryRun, coverage: &mut Coverage) -> TestCaseResult {
    if run.model.phase == ModelPhase::LookbackDelay {
        run_block(&mut run.storage, run.next_block, run.offering_fire)
            .expect("offering T Cycle command");
        run.next_block += 1;
        run.model = run.offering;
        run.model
            .observe(&mut run.storage, GENESIS_WWD, &run.replay_receipt)?;
    }

    let offering_end = StorageHandle::enter(&mut run.storage, |handle| {
        worldwide_day(handle, GENESIS_WWD)
            .unwrap()
            .unwrap()
            .offering_end
    });
    let fire_at = hourly_fire_at(offering_end);

    assert_outer_cycle_rollback(run, fire_at)?;

    run_block(&mut run.storage, run.next_block, fire_at).expect("exact retry after Cycle rollback");
    run.next_block += 1;
    let waiting = OuterWwdModel {
        phase: ModelPhase::Waiting,
        membership: WwdMembership::Active,
        has_terminal_receipt: false,
        has_capacity_forfeiture: false,
    };
    waiting.observe(&mut run.storage, GENESIS_WWD, &run.replay_receipt)?;
    let scheduled_process_time = StorageHandle::enter(&mut run.storage, |handle| {
        worldwide_day(handle, GENESIS_WWD)
            .unwrap()
            .unwrap()
            .scheduled_process_time
    });
    let process_fire_at = hourly_fire_at(scheduled_process_time.max(fire_at + 1));
    run_block(&mut run.storage, run.next_block, process_fire_at)
        .expect("empty-day terminal Cycle command");
    OuterWwdModel {
        phase: ModelPhase::Completed,
        membership: WwdMembership::Closed,
        has_terminal_receipt: false,
        has_capacity_forfeiture: false,
    }
    .observe(&mut run.storage, GENESIS_WWD, &run.replay_receipt)?;
    let active_count = StorageHandle::enter(&mut run.storage, |handle| {
        worldwide_days(handle)
            .unwrap()
            .into_iter()
            .filter(|projection| projection.membership == WwdMembership::Active)
            .count()
    });
    prop_assert!(active_count > 1);
    coverage.terminal += 1;
    coverage.multi_record += 1;
    Ok(())
}
