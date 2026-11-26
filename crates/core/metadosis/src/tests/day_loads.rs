use super::*;
use alloy_sol_types::SolEvent;
use outbe_core_common::nod_encryption::NodTermsV1;
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
fn record_worldwide_day_nod(storage: &StorageHandle<'_>, owner: Address, nominal: u64) {
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
        U256::from(load_of(nominal)),
        U256::from(nominal),
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
                    let mut ed = outbe_emission::EmissionDay::new_with_limit(
                        DAY,
                        WWD,
                        50,
                        u128::from(limit),
                        metadosis_state::PENDING,
                    );
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
        crate::commands::process_emission_day(&block_at(storage), DAY)
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

/// The sum of the day's Nod loads takes the place of the Lysis Limit, and the limit splits
/// once between Promis Limit, Desis and the Nods, an empty day included.
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
        outbe_desis::api::schedule_auction(storage, day).unwrap();
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

/// Loads above the Metadosis Limit fail the day: the Desis auction is cancelled and nothing
/// reaches Promis Limit, while the Nods keep their loads.
#[test]
fn loads_above_the_limit_fail_the_metadosis() {
    let day = outbe_system_common::time::EmissionDate::new(DAY);
    let limit = load_of(NOMINAL) - 1;
    let mut provider = fixture(limit, &[NOMINAL]);
    StorageHandle::enter(&mut provider, |storage| {
        outbe_desis::api::schedule_auction(storage, day).unwrap();
    });
    let before = promis_limit(&mut provider);

    run(&mut provider).unwrap();

    assert!(completed(&provider).is_empty());
    let events = failed(&provider);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].emissionDay, DAY);
    assert_eq!(
        events[0].reason,
        crate::constants::METADOSIS_FAILED_LOADS_ABOVE_LIMIT
    );
    let record = record(&mut provider);
    assert_eq!(record.metadosis_state, metadosis_state::FAILED);
    assert_eq!(record.nods, u128::from(limit));
    assert_eq!(record.desis_limit, 0);
    assert_eq!(promis_limit(&mut provider), before);
    StorageHandle::enter(&mut provider, |storage| {
        let desis = outbe_desis::schema::DesisContract::new(storage.clone());
        assert_eq!(
            desis.cancel_reason.read(&day).unwrap(),
            outbe_desis::constants::CANCELED_NO_LIMIT
        );
        let anchor = u64::from(desis.auction_at.read(&day).unwrap());
        let mut ctx = block_at(storage.clone());
        ctx.block.timestamp = anchor;
        outbe_desis::advance_auctions(&ctx).unwrap();
        assert_eq!(
            desis.auction_stage.read(&day).unwrap(),
            outbe_desis::schema::AuctionStage::Canceled as u8
        );
    });
}

/// A failed day returns the limit its Nods did not take to Promis Limit, and closes for good.
/// Loads above the limit leave nothing to return.
#[test]
fn failed_metadosis_returns_the_limit_less_the_loads() {
    let nominals = [NOMINAL + 1, NOMINAL + 2];
    let mut provider = fixture(LIMIT, &nominals);
    let before = promis_limit(&mut provider);
    StorageHandle::enter(&mut provider, |storage| {
        crate::commands::fail_metadosis(&block_at(storage), DAY).unwrap();
    });
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
            outbe_emission::api::MetadosisCompletedAmounts {
                desis_limit: U256::ZERO,
                nods: U256::ZERO,
                promis_limit_added: U256::ZERO,
            }
        )
        .is_err());
        assert!(
            outbe_emission::api::record_metadosis_failed(storage.clone(), DAY, U256::ZERO).is_err()
        );
        assert!(outbe_emission::api::record_metadosis_canceled(storage, DAY).is_err());
    });
    assert_eq!(record(&mut provider), closed);

    let mut above = fixture(loads - 1, &nominals);
    let before = promis_limit(&mut above);
    run(&mut above).unwrap();
    assert_eq!(record(&mut above).metadosis_state, metadosis_state::FAILED);
    assert_eq!(promis_limit(&mut above), before);
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
    let before = promis_limit(&mut provider);
    StorageHandle::enter(&mut provider, |storage| {
        crate::commands::fail_metadosis(&block_at(storage), DAY).unwrap();
    });
    let record = record(&mut provider);
    assert_eq!(record.metadosis_state, metadosis_state::FAILED);
    assert_eq!(record.nods, u128::from(LIMIT));
    assert_eq!(promis_limit(&mut provider), before);
}
