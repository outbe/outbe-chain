use super::*;
use alloy_sol_types::SolEvent;
use outbe_primitives::{
    addresses::{NOD_ADDRESS, TRIBUTE_ADDRESS},
    time::WorldwideDay,
};

const WWD: WorldwideDay = WorldwideDay::new(20260920);
const DAY: u32 = 20261014;
pub(crate) const CLOCK: u64 = 1_792_022_400 + 3_600;
const NOMINAL: u64 = 7_654_321;
const LIMIT: u64 = 1_000_000;

pub(crate) fn fixture() -> HashMapStorageProvider {
    fixture_with_amounts(LIMIT, &[NOMINAL + 1, NOMINAL + 2, NOMINAL + 3])
}

fn fixture_with_amounts(limit: u64, amounts: &[u64]) -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(10);
    provider.set_timestamp(U256::from(CLOCK));
    provider.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 8);
    outbe_tribute::enclave_client::test_enclave::install();
    outbe_fidelity::enclave_client::test_enclave::install();
    StorageHandle::enter(&mut provider, |storage| {
        install_test_enclaves();
        arm_reference_price(&storage, CLOCK);
        let forming_start = WWD.start_timestamp().unwrap();
        let forming_end =
            forming_start + outbe_chain_constants::get_metadosis_forming_period_seconds();
        let offerings_start =
            forming_end + outbe_chain_constants::get_metadosis_lookback_delay_seconds();
        let offering_end =
            offerings_start + outbe_chain_constants::get_metadosis_offering_period_seconds();
        let binding = outbe_primitives::storage::metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        storage
            .clone()
            .with_metadosis_mutation::<outbe_primitives::storage::MetadosisCycleLifecycle, _>(
                binding,
                || {
                    let mut factory = TributeFactoryContract::new(storage.clone());
                    let mut tribute = TributeContract::new(storage.clone());
                    let metadosis = MetadosisContract::new(storage.clone());
                    let mut ed = crate::schema::EmissionDay::new_with_limit(
                        DAY,
                        WWD,
                        50,
                        u128::from(limit),
                        metadosis_state::PENDING,
                    );
                    ed.state = outbe_emission::schema::emission_day_state::PROCESSING;
                    metadosis.emission_days().create(&ed)?;
                    for (index, amount) in amounts.iter().enumerate() {
                        let owner = Address::repeat_byte(u8::try_from(index + 1).unwrap());
                        let amount = U256::from(*amount);
                        tribute.issue(&outbe_tribute::TributeData {
                            tribute_id: outbe_primitives::wwd_entity_id::derive_poseidon_entity_id(
                                owner, WWD,
                            ),
                            owner,
                            worldwide_day: WWD,
                            issuance_amount_minor: amount,
                            issuance_currency: 840,
                            nominal_amount_minor: amount,
                            reference_currency: 840,
                            tribute_price_minor: U256::from(2),
                        })?;
                    }
                    outbe_tributefactory::worldwideday::create_worldwide_day(
                        &mut factory,
                        WWD,
                        outbe_tributefactory::worldwideday::NewWwdSchedule {
                            day: DAY,
                            forming_start,
                            forming_end,
                            offerings_start,
                            offering_end,
                        },
                    )?;
                    // A6 seals the WorldwideDay. Metadosis settles only a sealed day.
                    tribute.seal_day(WWD)?;
                    Ok(())
                },
            )
            .unwrap();
    });
    provider
}

fn run(provider: &mut HashMapStorageProvider) -> outbe_primitives::error::Result<()> {
    StorageHandle::enter(provider, |storage| {
        crate::commands::start_metadosis(&BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 10,
                timestamp: CLOCK,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: u64::max(CLOCK, 1),
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage,
        ))
    })
}

#[test]
fn native_fraction_completion_keeps_tributes_for_owner_issue_nod() {
    let mut provider = fixture();

    run(&mut provider).unwrap();

    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(
            metadosis
                .emission_days()
                .get(DAY)
                .unwrap()
                .unwrap()
                .metadosis_state,
            metadosis_state::COMPLETED
        );

        let tribute = TributeContract::new(storage.clone());
        assert_eq!(tribute.state_records_by_day(WWD).unwrap().len(), 3);
        assert!(!tribute.retired_days.read(&WWD).unwrap());
        assert!((1..=3u8).all(|byte| {
            outbe_nod::api::balance_of(&storage, Address::repeat_byte(byte)).unwrap() == 0
        }));
    });
}

#[test]
fn native_completion_credits_the_unused_lysis_limit_once() {
    let mut provider = fixture();
    let before = StorageHandle::enter(&mut provider, |storage| {
        outbe_promislimit::PromisLimitContract::new(storage)
            .promis_limit()
            .unwrap()
    });

    run(&mut provider).unwrap();

    let executed = provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| IMetadosis::MetadosisCompleted::decode_log(event).ok())
        .map(|event| event.data)
        .collect::<Vec<_>>();
    assert_eq!(executed.len(), 1);
    assert_eq!(executed[0].emissionDay, DAY);
    assert_eq!(executed[0].status, "COMPLETED");
    let unused = executed[0].unusedLysisLimitMinor;
    assert!(!unused.is_zero());
    StorageHandle::enter(&mut provider, |storage| {
        let emission = MetadosisContract::new(storage.clone())
            .emission_days()
            .get(DAY)
            .unwrap()
            .unwrap()
            .emission_limit_minor();
        let desis = outbe_desis::schema::DesisContract::new(storage.clone())
            .pending_desis_limit_minor
            .read(&outbe_primitives::time::EmissionDate::new(DAY))
            .unwrap();
        let after = outbe_promislimit::PromisLimitContract::new(storage)
            .promis_limit()
            .unwrap();
        assert_eq!(
            after + desis,
            before + (emission - executed[0].lysisLimitMinor) + unused
        );
    });

    let storage_before = provider.storage.clone();
    let logs = provider.get_ordered_events().to_vec();
    run(&mut provider).unwrap();
    assert_eq!(provider.storage, storage_before);
    assert_eq!(provider.get_ordered_events(), logs);
}

#[test]
fn native_completion_briefs_the_emission_day() {
    let mut provider = fixture();
    run(&mut provider).unwrap();
    StorageHandle::enter(&mut provider, |storage| {
        let desis = outbe_desis::schema::DesisContract::new(storage);
        let day = outbe_primitives::time::EmissionDate::new(DAY);
        assert_eq!(
            desis.auction_stage.read(&day).unwrap(),
            outbe_desis::schema::AuctionStage::Briefed as u8
        );
    });
}

#[test]
fn gap_ed14_failed_metadosis_forfeits_tributes() {
    let amounts = [NOMINAL + 1, NOMINAL + 2, NOMINAL + 3];
    for (limit, end_state) in [
        (LIMIT, metadosis_state::FAILED),
        (0, metadosis_state::CANCELLED),
    ] {
        let mut provider = fixture_with_amounts(limit, &amounts);
        StorageHandle::enter(&mut provider, |storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext {
                    block_gas_limit: 0,
                    user_gas_used: 0,
                    block_number: 10,
                    timestamp: CLOCK,
                    chain_id: CHAIN_ID,
                    genesis_hash: alloy_primitives::B256::ZERO,
                    genesis_timestamp: u64::max(CLOCK, 1),
                    proposer: alloy_primitives::Address::ZERO,
                    validators: Vec::new(),
                },
                storage.clone(),
            );
            if end_state == metadosis_state::FAILED {
                crate::commands::fail_metadosis(&ctx, DAY).unwrap();
            } else {
                crate::commands::start_metadosis(&ctx).unwrap();
            }
            let closed = MetadosisContract::new(storage.clone())
                .emission_days()
                .get(DAY)
                .unwrap()
                .unwrap();
            assert_eq!(closed.metadosis_state, end_state);

            let mut tribute = TributeContract::new(storage.clone());
            for byte in 1..=3u8 {
                tribute
                    .clean(outbe_primitives::wwd_entity_id::derive_poseidon_entity_id(
                        Address::repeat_byte(byte),
                        WWD,
                    ))
                    .unwrap();
            }
            assert!(tribute.state_records_by_day(WWD).unwrap().is_empty());

            let completed = outbe_emission::api::MetadosisCompletedAmounts {
                lysis_limit: U256::ZERO,
                desis_limit: U256::ZERO,
                nods: U256::ZERO,
                promis_limit_added: U256::ZERO,
            };
            assert!(outbe_emission::api::record_metadosis_completed(
                storage.clone(),
                DAY,
                completed
            )
            .is_err());
            assert!(outbe_emission::api::record_metadosis_failed(storage.clone(), DAY).is_err());
            assert!(outbe_emission::api::record_metadosis_canceled(storage.clone(), DAY).is_err());
            assert_eq!(
                MetadosisContract::new(storage)
                    .emission_days()
                    .get(DAY)
                    .unwrap(),
                Some(closed)
            );
        });
    }
}

#[test]
fn metadosis_writes_roll_back_and_retry_exactly() {
    let mut control = fixture_with_amounts(1, &[NOMINAL]);
    control.fail_after_mutation_at(usize::MAX);
    run(&mut control).unwrap();
    let operations = control.clear_mutation_failure();
    assert!(operations > 0);
    for operation in 0..operations {
        let mut provider = fixture_with_amounts(1, &[NOMINAL]);
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

#[test]
fn failed_owner_writes_roll_back_nods_tribute_and_economics() {
    for address in [NOD_ADDRESS, TRIBUTE_ADDRESS] {
        let mut provider = fixture();
        let before = provider.storage.clone();
        let events = provider.get_ordered_events().to_vec();
        provider.fail_mutation_at_address(address);
        assert!(run(&mut provider).is_err());
        provider.clear_mutation_failure();
        assert_eq!(provider.storage, before);
        assert_eq!(provider.get_ordered_events(), events);
    }
}

#[test]
fn gap_ed17_one_rule_for_metadosis_limit() {
    for amounts in [&[][..], &[1, 2], &[NOMINAL + 1, NOMINAL + 2, NOMINAL + 3]] {
        let mut provider = fixture_with_amounts(LIMIT, amounts);
        let before = StorageHandle::enter(&mut provider, |storage| {
            outbe_promislimit::PromisLimitContract::new(storage)
                .promis_limit()
                .unwrap()
        });

        run(&mut provider).unwrap();

        let completed = provider
            .get_ordered_events()
            .iter()
            .filter_map(|event| IMetadosis::MetadosisCompleted::decode_log(event).ok())
            .map(|event| event.data)
            .collect::<Vec<_>>();
        assert_eq!(completed.len(), 1, "amounts {amounts:?}");
        StorageHandle::enter(&mut provider, |storage| {
            let record = outbe_emission::api::get_emission_day_record(storage.clone(), DAY)
                .unwrap()
                .unwrap();
            assert_eq!(record.metadosis_state, metadosis_state::COMPLETED);
            let desis = outbe_desis::schema::DesisContract::new(storage.clone())
                .pending_desis_limit_minor
                .read(&outbe_primitives::time::EmissionDate::new(DAY))
                .unwrap();
            let after = outbe_promislimit::PromisLimitContract::new(storage)
                .promis_limit()
                .unwrap();
            assert_eq!(U256::from(record.desis_limit), desis);
            assert_eq!(U256::from(record.nods), completed[0].lysisAllocationMinor);
            assert_eq!(U256::from(record.lysis_limit), completed[0].lysisLimitMinor);
            assert_eq!(
                after + desis + U256::from(record.nods),
                before + U256::from(LIMIT),
                "amounts {amounts:?}"
            );
        });
    }
}
