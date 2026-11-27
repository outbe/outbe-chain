use super::*;
use alloy_sol_types::SolEvent;
use outbe_core_common::nod_encryption::NodTermsV1;
use outbe_emission::schema::emission_day_state;
use outbe_system_common::time::WorldwideDay;

const WWD: WorldwideDay = WorldwideDay::new(20260920);
const DAY: u32 = 20261014;
const CLOCK: u64 = 1_792_022_400 + 3_600;
const NOMINAL: u64 = 700_000;
const LIMIT: u64 = 1_000_000;

fn load_of(nominal: u64) -> u64 {
    u64::try_from(outbe_nod::offer::issue::nod_load_of(U256::from(nominal))).unwrap()
}

fn loads_of(nominals: &[u64]) -> u64 {
    nominals.iter().map(|&nominal| load_of(nominal)).sum()
}

/// Adds one Nod of nominal `nominal` to the day sums of [`WWD`], as its offer does.
fn record_worldwide_day_nod(storage: &StorageHandle<'_>, owner: Address, nominal: U256) {
    use outbe_nod::offers::{
        decode_worldwide_day_sum, encode_worldwide_day_sum, worldwide_day_load_scope,
        worldwide_day_nominal_scope,
    };
    let terms = NodTermsV1 {
        chain_id: CHAIN_ID,
        nod_id: outbe_nod::test_support::nod_id_of(owner, DAY, 840, 840),
        owner,
        emission_day: DAY,
        entry_price_minor: U256::from(2),
        issuance_currency: 840,
        reference_currency: 840,
    };
    let nod = outbe_nod::nod_encryption::encrypt_nod(
        &outbe_nod::test_support::state_key(),
        terms,
        outbe_nod::offer::issue::nod_load_of(nominal),
        nominal,
    )
    .unwrap();
    let totals = outbe_worldwideday::worldwide_day_totals(storage.clone(), WWD).unwrap();
    let sum = |scope, previous: &[u8], nominal| {
        let previous = decode_worldwide_day_sum(previous).unwrap();
        let record =
            outbe_nod::api::add_to_worldwide_day_sum(storage, scope, previous, (&nod, nominal))
                .unwrap();
        encode_worldwide_day_sum(&record).unwrap()
    };
    let nominal_sum = sum(
        worldwide_day_nominal_scope(WWD),
        &totals.encrypted_nominal,
        true,
    );
    let load_sum = sum(worldwide_day_load_scope(WWD), &totals.encrypted_load, false);
    outbe_worldwideday::record_worldwide_day_nod(storage.clone(), WWD, (&nominal_sum, &load_sum))
        .unwrap();
}

/// A pending emission day of limit `limit` whose sealed Worldwide Day issued one Nod of
/// each of `nominals`.
fn fixture(limit: u64, nominals: &[u64]) -> HashMapStorageProvider {
    let nominals: Vec<_> = nominals
        .iter()
        .map(|&nominal| U256::from(nominal))
        .collect();
    fixture_of(u128::from(limit), &nominals)
}

fn fixture_of(limit: u128, nominals: &[U256]) -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(10);
    provider.set_timestamp(U256::from(CLOCK));
    provider.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 8);
    StorageHandle::enter(&mut provider, |storage| {
        install_test_enclaves();
        arm_reference_price(&storage, CLOCK);
        let forming_start = WWD.start_timestamp().unwrap();
        let forming_end =
            forming_start + outbe_chain_constants::get_metadosis_forming_period_seconds();
        let offering_start =
            forming_start + outbe_chain_constants::OFFERING_START_AFTER_FORMING_START_SECONDS;
        let offering_end =
            offering_start + outbe_chain_constants::get_metadosis_offering_period_seconds();
        let binding = outbe_system_common::metadosis_bindings::metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        storage
            .clone()
            .with_scoped_mutation::<outbe_primitives::storage::CycleLifecyclePurpose, _>(
                binding,
                || {
                    let mut factory = WorldwideDayContract::new(storage.clone());
                    let mut ed =
                        outbe_emission::EmissionDay::new(DAY, WWD, 50, limit, [0, 0, 0, 0, limit]);
                    ed.state = outbe_emission::schema::emission_day_state::PROCESSING;
                    outbe_emission::api::put_emission_day(storage.clone(), &ed)?;
                    outbe_worldwideday::open_worldwide_day(
                        &mut factory,
                        WWD,
                        outbe_worldwideday::NewWwdSchedule {
                            emission_day: DAY,
                            forming_start,
                            forming_end,
                            offering_start,
                            offering_end,
                        },
                    )?;
                    for (index, &nominal) in nominals.iter().enumerate() {
                        let owner = Address::repeat_byte(u8::try_from(index + 1).unwrap());
                        record_worldwide_day_nod(&storage, owner, nominal);
                    }
                    outbe_worldwideday::seal_worldwide_day(storage.clone(), WWD)
                },
            )
            .unwrap();
    });
    provider
}

fn block_at(storage: StorageHandle<'_>) -> BlockRuntimeContext<'_> {
    BlockRuntimeContext::new(
        BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number: 10,
            timestamp: CLOCK,
            chain_id: CHAIN_ID,
            genesis_hash: alloy_primitives::B256::ZERO,
            genesis_timestamp: CLOCK,
            proposer: Address::ZERO,
            validators: Vec::new(),
        },
        storage,
    )
}

fn run(provider: &mut HashMapStorageProvider) -> outbe_primitives::error::Result<()> {
    StorageHandle::enter(provider, |storage| {
        crate::commands::start_metadosis(&block_at(storage), DAY)
    })
}

fn metadosis_limit_added(provider: &HashMapStorageProvider) -> U256 {
    use alloy_sol_types::SolEvent;
    use outbe_promislimit::{precompile::IPromisLimit, LimitSource};
    provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| IPromisLimit::PromisLimitAdded::decode_log(event).ok())
        .filter(|event| event.data.source == LimitSource::MetadosisLimit as u8)
        .map(|event| event.data.amountMinor)
        .sum()
}

fn completed(provider: &HashMapStorageProvider) -> Vec<IMetadosis::MetadosisCompleted> {
    provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| IMetadosis::MetadosisCompleted::decode_log(event).ok())
        .map(|event| event.data)
        .collect()
}

fn failed(provider: &HashMapStorageProvider) -> Vec<IMetadosis::MetadosisFailed> {
    provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| IMetadosis::MetadosisFailed::decode_log(event).ok())
        .map(|event| event.data)
        .collect()
}

fn exceeded(provider: &HashMapStorageProvider) -> Vec<IMetadosis::MetadosisLimitExceeded> {
    provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| IMetadosis::MetadosisLimitExceeded::decode_log(event).ok())
        .map(|event| event.data)
        .collect()
}

fn excess(provider: &mut HashMapStorageProvider) -> U256 {
    StorageHandle::enter(provider, |storage| {
        outbe_emission::api::excess_nod_loads(storage).unwrap()
    })
}

fn set_excess(provider: &mut HashMapStorageProvider, amount: u64) {
    StorageHandle::enter(provider, |storage| {
        outbe_emission::api::record_excess_nod_loads(storage, U256::from(amount)).unwrap();
    });
}

fn promis_limit(provider: &mut HashMapStorageProvider) -> U256 {
    StorageHandle::enter(provider, |storage| {
        outbe_promislimit::PromisLimitContract::new(storage)
            .promis_limit()
            .unwrap()
    })
}

fn record(provider: &mut HashMapStorageProvider) -> outbe_emission::schema::EmissionDay {
    StorageHandle::enter(provider, |storage| {
        outbe_emission::api::get_emission_day(storage, DAY)
            .unwrap()
            .unwrap()
    })
}

fn pending_desis_limit(provider: &mut HashMapStorageProvider) -> U256 {
    StorageHandle::enter(provider, |storage| {
        outbe_desis::schema::DesisContract::new(storage)
            .pending_desis_limit_minor
            .read(&outbe_system_common::time::EmissionDate::new(DAY))
            .unwrap()
    })
}

/// The limit splits once between Promis Limit, Desis and the sum of the day's Nod loads,
/// an empty day included.
#[test]
fn metadosis_splits_the_limit_with_the_loads() {
    for nominals in [
        &[][..],
        &[NOMINAL],
        &[NOMINAL + 1, NOMINAL + 2, NOMINAL + 3],
    ] {
        let mut provider = fixture(LIMIT, nominals);
        let before = promis_limit(&mut provider);

        run(&mut provider).unwrap();

        let events = completed(&provider);
        assert_eq!(events.len(), 1, "nominals {nominals:?}");
        let loads = U256::from(loads_of(nominals));
        assert_eq!(
            events[0].nominalAmountMinor,
            U256::from(nominals.iter().sum::<u64>())
        );
        assert_eq!(
            metadosis_limit_added(&provider),
            U256::from(LIMIT) - loads,
            "the loads of the day are the Metadosis limit less its Promis Limit add"
        );
        let record = record(&mut provider);
        assert_eq!(record.metadosis_state, metadosis_state::COMPLETED);
        assert_eq!(U256::from(record.nods), loads);
        let desis = pending_desis_limit(&mut provider);
        assert_eq!(U256::from(record.desis_limit), desis);
        assert_eq!(
            promis_limit(&mut provider) + desis + loads,
            before + U256::from(LIMIT),
            "nominals {nominals:?}"
        );
    }
}

#[test]
fn completion_runs_once() {
    let mut provider = fixture(LIMIT, &[NOMINAL]);
    run(&mut provider).unwrap();
    let storage = provider.storage.clone();
    let logs = provider.get_ordered_events().to_vec();
    run(&mut provider).unwrap();
    assert_eq!(provider.storage, storage);
    assert_eq!(provider.get_ordered_events(), logs);
}

#[test]
fn completion_briefs_the_emission_day() {
    let day = outbe_system_common::time::EmissionDate::new(DAY);
    let mut provider = fixture(LIMIT, &[NOMINAL]);
    StorageHandle::enter(&mut provider, |storage| {
        outbe_desis::test_support::take_day(storage, day).unwrap();
    });
    run(&mut provider).unwrap();
    StorageHandle::enter(&mut provider, |storage| {
        let desis = outbe_desis::schema::DesisContract::new(storage);
        assert_eq!(desis.desis_limit_set.read(&day).unwrap(), 1);
        assert_eq!(
            desis.auction_stage.read(&day).unwrap(),
            outbe_desis::schema::AuctionStage::None as u8
        );
    });
}

#[test]
fn loads_equal_to_the_limit_complete() {
    let mut provider = fixture(load_of(NOMINAL), &[NOMINAL]);
    let before = promis_limit(&mut provider);
    run(&mut provider).unwrap();
    assert_eq!(completed(&provider).len(), 1);
    assert!(failed(&provider).is_empty());
    assert_eq!(
        record(&mut provider).metadosis_state,
        metadosis_state::COMPLETED
    );
    assert_eq!(promis_limit(&mut provider), before);
    assert_eq!(pending_desis_limit(&mut provider), U256::ZERO);
}

/// Loads above the Metadosis Limit complete the day with nothing for Promis Limit and a Desis
/// brief with no limit, and the loads the limit does not cover carry to the next days.
#[test]
fn loads_above_the_limit_carry_the_excess() {
    let day = outbe_system_common::time::EmissionDate::new(DAY);
    let limit = load_of(NOMINAL) - 1;
    let mut provider = fixture(limit, &[NOMINAL]);
    StorageHandle::enter(&mut provider, |storage| {
        outbe_desis::test_support::take_day(storage, day).unwrap();
    });
    let before = promis_limit(&mut provider);

    run(&mut provider).unwrap();

    assert!(failed(&provider).is_empty());
    assert_eq!(completed(&provider).len(), 1);
    let events = exceeded(&provider);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].emissionDay, DAY);
    assert_eq!(events[0].excessMinor, U256::from(1));
    assert_eq!(excess(&mut provider), U256::from(1));
    let record = record(&mut provider);
    assert_eq!(record.metadosis_state, metadosis_state::COMPLETED);
    assert_eq!(record.state, emission_day_state::COMPLETED);
    assert_eq!(record.nods, u128::from(limit));
    assert_eq!(record.desis_limit, 0);
    assert_eq!(promis_limit(&mut provider), before);
    StorageHandle::enter(&mut provider, |storage| {
        let desis = outbe_desis::schema::DesisContract::new(storage);
        assert_eq!(desis.desis_limit_set.read(&day).unwrap(), 1);
        assert_eq!(desis.has_desis_limit.read(&day).unwrap(), 0);
    });
}

/// A Desis Limit above the auction domain cancels the auction and goes back to the Promis Limit
/// once, and Metadosis still ends Completed.
#[test]
fn a_desis_limit_above_the_auction_domain_completes_the_metadosis() {
    let day = outbe_system_common::time::EmissionDate::new(DAY);
    let limit = U256::from(u128::MAX);
    let nominal = limit * U256::from(2);
    let loads = outbe_nod::offer::issue::nod_load_of(nominal);
    let mut provider = fixture_of(u128::MAX, &[nominal]);
    StorageHandle::enter(&mut provider, |storage| {
        outbe_desis::test_support::take_day(storage.clone(), day).unwrap();
        outbe_promislimit::PromisLimitContract::new(storage)
            .add(outbe_promislimit::LimitSource::Nod, limit)
            .unwrap();
    });
    let before = promis_limit(&mut provider);

    run(&mut provider).unwrap();

    assert!(failed(&provider).is_empty());
    assert_eq!(completed(&provider).len(), 1);
    let record = record(&mut provider);
    assert_eq!(record.metadosis_state, metadosis_state::COMPLETED);
    assert_eq!(U256::from(record.nods), loads);
    assert_eq!(record.desis_limit, 0);
    assert_eq!(promis_limit(&mut provider), before + limit - loads);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            outbe_desis::schema::DesisContract::new(storage)
                .cancel_reason
                .read(&day)
                .unwrap(),
            outbe_desis::constants::CANCELED_LIMIT_OVERFLOW
        );
    });
}

/// The excess of past days comes out of the next limit first, and the day still closes.
#[test]
fn excess_lowers_the_next_day() {
    let mut provider = fixture(LIMIT, &[NOMINAL]);
    set_excess(&mut provider, 1_000);

    run(&mut provider).unwrap();

    let loads = load_of(NOMINAL);
    assert_eq!(
        metadosis_limit_added(&provider),
        U256::from(LIMIT - loads - 1_000)
    );
    assert_eq!(excess(&mut provider), U256::ZERO);
    assert!(exceeded(&provider).is_empty());
    let record = record(&mut provider);
    assert_eq!(record.nods, u128::from(loads + 1_000));
    assert_eq!(record.state, emission_day_state::COMPLETED);
}

/// An excess above the next limit takes the whole of it and carries the rest on.
#[test]
fn excess_above_the_next_limit_carries_on() {
    let mut provider = fixture(LIMIT, &[NOMINAL]);
    set_excess(&mut provider, LIMIT + 5);
    let before = promis_limit(&mut provider);

    run(&mut provider).unwrap();

    let left = U256::from(load_of(NOMINAL) + 5);
    assert_eq!(excess(&mut provider), left);
    assert_eq!(exceeded(&provider)[0].excessMinor, left);
    assert_eq!(promis_limit(&mut provider), before);
    assert_eq!(pending_desis_limit(&mut provider), U256::ZERO);
    let record = record(&mut provider);
    assert_eq!(record.nods, u128::from(LIMIT));
    assert_eq!(record.state, emission_day_state::PROCESSING);
}

/// A day with no limit completes its Metadosis and carries all its loads.
#[test]
fn a_day_with_no_limit_carries_its_loads() {
    let mut provider = fixture(0, &[NOMINAL]);
    set_excess(&mut provider, 3);

    run(&mut provider).unwrap();

    let left = U256::from(load_of(NOMINAL) + 3);
    assert_eq!(excess(&mut provider), left);
    assert_eq!(exceeded(&provider)[0].excessMinor, left);
    let record = record(&mut provider);
    assert_eq!(record.metadosis_state, metadosis_state::COMPLETED);
    assert_eq!(record.nods, 0);
    assert_eq!(record.lysis_limit, 0);
    assert_eq!(record.nominal_amount, u128::from(NOMINAL));
}

fn fail(provider: &mut HashMapStorageProvider) {
    StorageHandle::enter(provider, |storage| {
        crate::commands::fail_metadosis(&block_at(storage), DAY).unwrap();
    });
}

/// A failed day returns the limit its Nods and the excess did not take to Promis Limit, and
/// closes for good.
#[test]
fn failed_metadosis_returns_the_limit_less_the_loads() {
    let nominals = [NOMINAL + 1, NOMINAL + 2];
    let mut provider = fixture(LIMIT, &nominals);
    let before = promis_limit(&mut provider);
    fail(&mut provider);
    let loads = loads_of(&nominals);
    assert_eq!(
        promis_limit(&mut provider),
        before + U256::from(LIMIT - loads)
    );
    let closed = record(&mut provider);
    assert_eq!(closed.metadosis_state, metadosis_state::FAILED);
    assert_eq!(closed.nods, u128::from(loads));
    StorageHandle::enter(&mut provider, |storage| {
        assert!(outbe_emission::api::record_metadosis_completed(
            storage.clone(),
            DAY,
            outbe_emission::api::MetadosisAmounts::default()
        )
        .is_err());
        assert!(outbe_emission::api::record_metadosis_failed(
            storage,
            DAY,
            outbe_emission::api::MetadosisAmounts::default()
        )
        .is_err());
    });
    assert_eq!(record(&mut provider), closed);

    let mut repaid = fixture(LIMIT, &nominals);
    set_excess(&mut repaid, 500);
    let before = promis_limit(&mut repaid);
    fail(&mut repaid);
    assert_eq!(
        promis_limit(&mut repaid),
        before + U256::from(LIMIT - loads - 500)
    );
    assert_eq!(excess(&mut repaid), U256::ZERO);
    assert_eq!(record(&mut repaid).nods, u128::from(loads + 500));

    let mut carried = fixture(LIMIT, &nominals);
    set_excess(&mut carried, LIMIT);
    let before = promis_limit(&mut carried);
    fail(&mut carried);
    assert_eq!(promis_limit(&mut carried), before);
    assert_eq!(excess(&mut carried), U256::from(loads));
    assert_eq!(exceeded(&carried)[0].excessMinor, U256::from(loads));
    assert_eq!(record(&mut carried).nods, u128::from(LIMIT));
}

#[test]
fn metadosis_writes_roll_back_and_retry_exactly() {
    for limit in [LIMIT, load_of(NOMINAL) - 1] {
        let mut control = fixture(limit, &[NOMINAL]);
        control.fail_after_mutation_at(usize::MAX);
        run(&mut control).unwrap();
        let operations = control.clear_mutation_failure();
        assert!(operations > 0);
        for operation in 0..operations {
            let mut provider = fixture(limit, &[NOMINAL]);
            let before = provider.storage.clone();
            let events = provider.get_ordered_events().to_vec();
            provider.fail_after_mutation_at(operation);
            assert!(run(&mut provider).is_err(), "operation {operation}");
            provider.clear_mutation_failure();
            assert_eq!(provider.storage, before, "operation {operation}");
            assert_eq!(
                provider.get_ordered_events(),
                events,
                "operation {operation}"
            );
            run(&mut provider).unwrap();
            assert_eq!(provider.storage, control.storage, "retry {operation}");
            assert_eq!(
                provider.get_ordered_events(),
                control.get_ordered_events(),
                "retry {operation}"
            );
        }
    }
}

/// A day sum that cannot be read does not stop the failure path: the day fails and the
/// whole limit stays with its Nods.
#[test]
fn an_unreadable_day_sum_fails_the_day_with_no_credit() {
    let mut provider = fixture(LIMIT, &[NOMINAL]);
    StorageHandle::enter(&mut provider, |storage| {
        WorldwideDayContract::new(storage.clone())
            .worldwide_day_load_ct
            .bytes(&WWD)
            .write(&[0xFF; 3])
            .unwrap();
    });
    set_excess(&mut provider, 7);
    let before = promis_limit(&mut provider);
    fail(&mut provider);
    let record = record(&mut provider);
    assert_eq!(record.metadosis_state, metadosis_state::FAILED);
    assert_eq!(record.nods, u128::from(LIMIT));
    assert_eq!(promis_limit(&mut provider), before);
    assert_eq!(excess(&mut provider), U256::from(7));
}
