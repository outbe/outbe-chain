use super::*;
use crate::constants::{ADVANCE_DAY_BASE_GAS, ADVANCE_FAIL_GAS};
use crate::precompile::IDesis;
use outbe_promislimit::PromisLimitContract;

#[test]
fn deadline_retirement_fits_budget_for_1088_and_64_accepted_bids() {
    let mut retirement_cost = None;
    for total_batches in [17u16, 1] {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.set_timestamp(U256::from(NOW));
        provider.stub_sub_call_at(ORIGIN_ROUTER_ADDRESS, targets_stub(&[SRC_CHAIN]));
        let before = StorageHandle::enter(&mut provider, |s| {
            seed_rate(&s, NOW, ENTRY_PRICE);
            open_revealing(&s);
            let config = s
                .contract::<DesisContract>()
                .read_auction_config(EMISSION_DAY)
                .unwrap();
            for batch in 0..total_batches {
                let rows = (0..64u32)
                    .map(|offset| {
                        let index = u32::from(batch) * 64 + offset;
                        let mut address = [0u8; 20];
                        address[16..].copy_from_slice(&(index + 1).to_be_bytes());
                        BidData {
                            bidder_address: Address::from(address),
                            intex_bid_rate: config.min_intex_bid_rate.max(200),
                            timestamp: index,
                            intex_quantity: config.min_intex_bid_quantity.max(1),
                            issuance_currency: REFERENCE_ISO,
                            reference_currency: REFERENCE_ISO,
                        }
                    })
                    .collect();
                runtime::process_bids_batch(
                    s.clone(),
                    origin(SRC_CHAIN),
                    batch,
                    total_batches,
                    rows,
                )
                .unwrap();
            }
            mark_done(&s, SRC_CHAIN, total_batches, u32::from(total_batches) * 64);
            arm_clearing(&s);
            let contract = s.contract::<DesisContract>();
            assert_eq!(
                contract
                    .chain_done
                    .read(&DesisContract::chain_key(EMISSION_DAY, SRC_CHAIN))
                    .unwrap(),
                1
            );
            (0..u32::from(total_batches) * 64)
                .map(|index| {
                    contract
                        .read_bid_at(EMISSION_DAY, SRC_CHAIN, index)
                        .unwrap()
                })
                .collect::<Vec<_>>()
        });
        let calls_before = provider.sub_call_inputs().len();
        provider.enable_production_storage_gas_metering();
        provider.set_gas_limit(32_000_000);
        StorageHandle::enter(&mut provider, |s| {
            let gas_before = s.gas_used().unwrap();
            let deadline = last_clearing_for(&s);
            s.with_trigger_budget(ADVANCE_BUDGET, || runtime::schedule_tick(&s, deadline))
                .unwrap();
            let cost = s.gas_used().unwrap() - gas_before;
            assert!(cost <= ADVANCE_DAY_BASE_GAS + ADVANCE_FAIL_GAS);
            if let Some(previous) = retirement_cost.replace(cost) {
                assert_eq!(
                    cost, previous,
                    "retirement cost is independent of bid count"
                );
            }
            let contract = s.contract::<DesisContract>();
            assert_eq!(
                contract.read_stage(EMISSION_DAY).unwrap(),
                AuctionStage::Failed
            );
            assert_eq!(contract.cleanup_pending.read(&EMISSION_DAY).unwrap(), 1);
            assert_eq!(contract.cleanup_head.read().unwrap(), EMISSION_DAY.value());
            assert_eq!(contract.day_bid_count.read(&EMISSION_DAY).unwrap(), 0);
            assert_eq!(
                contract
                    .pending_desis_limit_minor
                    .read(&EMISSION_DAY)
                    .unwrap(),
                U256::ZERO
            );
            assert_eq!(
                contract
                    .pending_desis_limit_units
                    .read(&EMISSION_DAY)
                    .unwrap(),
                0
            );
            assert_eq!(contract.clearing_initiated.read(&EMISSION_DAY).unwrap(), 0);
            assert_eq!(contract.clearing_deadline.read(&EMISSION_DAY).unwrap(), 0);
            assert_eq!(contract.sched_active_count.read().unwrap(), 0);
            assert_eq!(contract.gate_active_count.read().unwrap(), 0);
            for (index, bid) in before.iter().enumerate() {
                assert_eq!(
                    contract
                        .read_bid_at(EMISSION_DAY, SRC_CHAIN, u32::try_from(index).unwrap())
                        .unwrap(),
                    *bid
                );
            }
            s.with_trigger_budget(ADVANCE_BUDGET, || runtime::schedule_tick(&s, deadline + 1))
                .unwrap();
            assert_eq!(
                PromisLimitContract::new(s.clone()).promis_limit().unwrap(),
                U256::from(10 * LOAD_MINOR)
            );
        });
        assert_eq!(
            provider.sub_call_inputs().len(),
            calls_before,
            "retirement never queries or calls the router"
        );
        let logs = provider.get_events(outbe_primitives::addresses::DESIS_ADDRESS);
        assert_eq!(
            logs.iter()
                .filter(|log| IDesis::IntexAuctionFailed::decode_log_data(log).is_ok())
                .count(),
            1
        );
        let returned = logs
            .iter()
            .filter_map(|log| IDesis::DesisLimitReturned::decode_log_data(log).ok())
            .collect::<Vec<_>>();
        assert_eq!(returned.len(), 1);
        assert_eq!(returned[0].amountMinor, U256::from(10 * LOAD_MINOR));
    }
}

#[test]
fn schedule_retires_every_backlogged_day_in_one_tick_and_keeps_a_not_ready_day() {
    with_storage(|s| {
        let later = EmissionDate::new(20231115);
        let mut contract = s.contract::<DesisContract>();
        for (day, anchor) in [
            (later, LAST_CLEARING + 1),
            (EMISSION_DAY, ANCHOR),
            (EmissionDate::new(20231114), ANCHOR),
        ] {
            contract
                .write_stage(day, AuctionStage::CommittingBids)
                .unwrap();
            contract
                .auction_at
                .write(&day, u32::try_from(anchor).unwrap())
                .unwrap();
            contract
                .pending_desis_limit_minor
                .write(&day, U256::from(5))
                .unwrap();
            contract.push_sched_active(day).unwrap();
        }
        runtime::schedule_tick(&s, LAST_CLEARING).unwrap();
        assert_eq!(
            contract.read_stage(EMISSION_DAY).unwrap(),
            AuctionStage::Failed
        );
        // The swapped-in tail must not be skipped after removing slot one.
        assert_eq!(
            contract.read_stage(EmissionDate::new(20231114)).unwrap(),
            AuctionStage::Failed
        );
        assert_eq!(contract.sched_active_count.read().unwrap(), 1);
        assert_eq!(
            contract.read_stage(later).unwrap(),
            AuctionStage::CommittingBids
        );
        assert_eq!(
            PromisLimitContract::new(s.clone()).promis_limit().unwrap(),
            U256::from(10)
        );
    });
}

#[test]
fn schedule_terminal_stages_are_idempotent_even_if_still_scheduled() {
    with_storage(|s| {
        let mut contract = s.contract::<DesisContract>();
        for stage in [
            AuctionStage::Completed,
            AuctionStage::Failed,
            AuctionStage::Cancelled,
        ] {
            contract.write_stage(EMISSION_DAY, stage).unwrap();
            contract.push_sched_active(EMISSION_DAY).unwrap();
            runtime::schedule_tick(&s, LAST_CLEARING).unwrap();
            assert_eq!(contract.read_stage(EMISSION_DAY).unwrap(), stage);
            assert_eq!(contract.sched_active_count.read().unwrap(), 0);
            assert_eq!(
                PromisLimitContract::new(s.clone()).promis_limit().unwrap(),
                U256::ZERO
            );
        }
    });
}
