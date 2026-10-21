//! All conditions of a day are fixed when its auction starts. The clearing and the
//! issuance read them only from the stored day config, not from the Intex profile.

use super::*;
use crate::constants::{DAY_STATE_CANCELLED, DAY_STATE_LIVE};
use crate::schema::{IntexCallTrigger, ReferenceCurrencyPrice};
use outbe_intex::config::IntexParams;

const EUR_ISO: u16 = 978;
const EUR_ENTRY_PRICE: u128 = 3_000_000;

fn read_config(s: &StorageHandle) -> AuctionConfig {
    s.contract::<DesisContract>()
        .read_auction_config(WORLDWIDE_DAY)
        .unwrap()
}

/// The AUCTION_STAGE_START payload the stored day config gives.
fn announced(
    config: &AuctionConfig,
    day_state: u8,
) -> crate::sol_ext::IOriginRouter::AuctionStageStartParams {
    runtime::stage_start_params(
        runtime::StageStart {
            worldwide_day: WORLDWIDE_DAY,
            config,
            commit_end: 1,
            reveal_end: 2,
            issuance_end: 3,
        },
        day_state,
    )
    .unwrap()
}

fn trigger_of(profile: IntexParams) -> IntexCallTrigger {
    IntexCallTrigger {
        call_window_seconds: profile.call_window_seconds,
        call_threshold_seconds: profile.call_threshold_seconds,
        call_notice_period_seconds: profile.call_notice_period_seconds,
    }
}

fn call_price(entry: u128, profile: IntexParams) -> U256 {
    outbe_intexfactory::marked_up(U256::from(entry), profile.call_rate).unwrap()
}

fn select_profile(s: &StorageHandle, profile: u8) {
    outbe_intex::IntexContract::new(s.clone())
        .config_profile
        .write(profile)
        .unwrap();
}

#[test]
fn the_start_fixes_the_call_terms_of_each_currency_from_the_profile() {
    with_storage(|s| {
        brief(&s, true);
        seed_price(&s, NOW, EUR_ISO, OTHER_PAIR_ID, EUR_ENTRY_PRICE);
        runtime::schedule_tick(&s, NOW).unwrap();

        let config = read_config(&s);
        assert_eq!(
            config.reference_prices,
            vec![
                ReferenceCurrencyPrice {
                    iso_code: REFERENCE_ISO,
                    entry_price_minor: U256::from(ENTRY_PRICE),
                    call_price_minor: call_price(ENTRY_PRICE, IntexParams::PROD),
                },
                ReferenceCurrencyPrice {
                    iso_code: EUR_ISO,
                    entry_price_minor: U256::from(EUR_ENTRY_PRICE),
                    call_price_minor: call_price(EUR_ENTRY_PRICE, IntexParams::PROD),
                },
            ]
        );
        assert_eq!(config.call_trigger, trigger_of(IntexParams::PROD));
        assert_eq!(
            config.commit_bond_minor,
            IntexParams::PROD.commit_bond_minor
        );
    });
}

#[test]
fn the_start_message_carries_only_the_stored_day_config() {
    with_storage(|s| {
        brief(&s, true);
        seed_price(&s, NOW, EUR_ISO, OTHER_PAIR_ID, EUR_ENTRY_PRICE);
        runtime::schedule_tick(&s, NOW).unwrap();
        let config = read_config(&s);
        // A profile change after the start does not change what the day announces.
        select_profile(&s, outbe_intex::config::PROFILE_DEV);

        let message = announced(&config, DAY_STATE_LIVE);
        assert_eq!(message.worldwideDay, WORLDWIDE_DAY.value());
        assert_eq!(message.promisLoadMinor, config.promis_load_minor);
        assert_eq!(message.minIntexBidRate, config.min_intex_bid_rate);
        assert_eq!(message.minIntexBidQuantity, config.min_intex_bid_quantity);
        assert_eq!(message.commitBondMinor, config.commit_bond_minor);
        assert_eq!(message.callWindow, config.call_trigger.call_window_seconds);
        assert_eq!(
            message.callThreshold,
            config.call_trigger.call_threshold_seconds
        );
        assert_eq!(
            message.callNoticePeriod,
            config.call_trigger.call_notice_period_seconds
        );
        assert_eq!(message.prices.len(), config.reference_prices.len());
        for (wire, row) in message.prices.iter().zip(&config.reference_prices) {
            assert_eq!(wire.isoCode, row.iso_code);
            assert_eq!(U256::from(wire.entryPriceMinor), row.entry_price_minor);
            assert_eq!(U256::from(wire.callPriceMinor), row.call_price_minor);
        }
        assert_eq!(message.dayState, DAY_STATE_LIVE);
    });
}

#[test]
fn an_unpriced_day_still_fixes_and_announces_the_call_trigger() {
    with_storage(|s| {
        brief(&s, true);
        unprice_day(&s, NOW);
        runtime::schedule_tick(&s, NOW).unwrap();
        assert_eq!(
            s.contract::<DesisContract>()
                .read_stage(WORLDWIDE_DAY)
                .unwrap(),
            AuctionStage::Cancelled
        );

        let config = read_config(&s);
        assert!(config.reference_prices.is_empty());
        assert_eq!(config.call_trigger, trigger_of(IntexParams::PROD));
        let message = announced(&config, DAY_STATE_CANCELLED);
        assert!(message.prices.is_empty());
        assert_eq!(message.callWindow, IntexParams::PROD.call_window_seconds);
        assert_eq!(
            message.callThreshold,
            IntexParams::PROD.call_threshold_seconds
        );
        assert_eq!(
            message.callNoticePeriod,
            IntexParams::PROD.call_notice_period_seconds
        );
    });
}

/// An upgrade that changes the Intex profile between the start and the clearing
/// gives the series the conditions the bidders saw, not the new profile's.
/// The test walks the day-long production windows, so an e2e build does not run it.
#[test]
#[cfg(not(feature = "e2e-test"))]
fn a_profile_change_after_the_start_leaves_the_series_conditions_fixed() {
    use outbe_intexfactory::SeriesId;

    with_storage(|s| {
        brief_at(&s, WORLDWIDE_DAY, 2 * LOAD_MINOR, true);
        runtime::schedule_tick(&s, NOW).unwrap();
        let config = read_config(&s);
        let message = announced(&config, DAY_STATE_LIVE);
        let wire_row = &message.prices[0];
        assert_eq!(
            U256::from(wire_row.callPriceMinor),
            call_price(ENTRY_PRICE, IntexParams::PROD)
        );

        select_profile(&s, outbe_intex::config::PROFILE_DEV);
        assert_ne!(IntexParams::DEV.call_rate, IntexParams::PROD.call_rate);
        assert_ne!(trigger_of(IntexParams::DEV), trigger_of(IntexParams::PROD));

        runtime::schedule_tick(&s, ANCHOR + SECONDS_PER_DAY).unwrap();
        arm_clearing(&s);
        relay_bids(&s, SRC_CHAIN, 2, 200);
        let result = clear(&s);
        assert_eq!(result.issued_units, 2);

        let announced_trigger = outbe_intex::IntexCallTrigger {
            call_window_seconds: message.callWindow,
            call_threshold_seconds: message.callThreshold,
            call_notice_period_seconds: message.callNoticePeriod,
        };
        let series_id = SeriesId::for_pair(WORLDWIDE_DAY, REFERENCE_ISO, REFERENCE_ISO).unwrap();
        let series = outbe_intex::api::read_series(&s, series_id).unwrap();
        assert_eq!(
            series.promis_load_minor,
            U256::from(message.promisLoadMinor)
        );
        assert_eq!(
            series.entry_price_minor,
            U256::from(wire_row.entryPriceMinor)
        );
        assert_eq!(series.call_price_minor, U256::from(wire_row.callPriceMinor));
        assert_eq!(series.call_trigger(), announced_trigger);

        // The issuance gets the same conditions. IntexFactory builds the legs from them.
        assert_eq!(read_config(&s), config);
        let groups =
            runtime::issuance_groups(&result, &config, WORLDWIDE_DAY, &[SRC_CHAIN]).unwrap();
        assert_eq!(groups.len(), 1);
        let group = &groups[0];
        assert_eq!(group.promis_load_minor, message.promisLoadMinor);
        assert_eq!(
            group.entry_price_minor,
            U256::from(wire_row.entryPriceMinor)
        );
        assert_eq!(group.call_price_minor, U256::from(wire_row.callPriceMinor));
        assert_eq!(group.call_trigger, announced_trigger);
    });
}

#[test]
fn a_winner_in_an_unpriced_reference_currency_is_an_error() {
    with_storage(|s| {
        brief_at(&s, WORLDWIDE_DAY, 2 * LOAD_MINOR, true);
        runtime::schedule_tick(&s, NOW).unwrap();
        let config = read_config(&s);
        let result = crate::schema::ClearingResult {
            issued_units: 1,
            winner_currencies: vec![(REFERENCE_ISO, EUR_ISO)],
            winner_quantities: vec![U256::from(1u8)],
            ..Default::default()
        };
        let err =
            runtime::issuance_groups(&result, &config, WORLDWIDE_DAY, &[SRC_CHAIN]).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::DesisError::UnpricedReferenceCurrency(EUR_ISO)
            )
            .to_string()
        );
    });
}

#[test]
fn the_day_config_round_trips_with_its_call_prices() {
    with_storage(|s| {
        let config = AuctionConfig {
            promis_load_minor: LOAD_MINOR,
            call_trigger: IntexCallTrigger {
                call_window_seconds: 11,
                call_threshold_seconds: 7,
                call_notice_period_seconds: 5,
            },
            min_intex_bid_rate: 3,
            min_intex_bid_quantity: 2,
            commit_bond_minor: 9,
            reference_prices: vec![
                ReferenceCurrencyPrice {
                    iso_code: REFERENCE_ISO,
                    entry_price_minor: U256::from(ENTRY_PRICE),
                    call_price_minor: U256::from(4_560_000u64),
                },
                ReferenceCurrencyPrice {
                    iso_code: EUR_ISO,
                    entry_price_minor: U256::from(EUR_ENTRY_PRICE),
                    call_price_minor: U256::from(6_840_000u64),
                },
            ],
        };
        let contract = s.contract::<DesisContract>();
        contract
            .write_auction_config(WORLDWIDE_DAY, &config)
            .unwrap();
        assert_eq!(contract.read_auction_config(WORLDWIDE_DAY).unwrap(), config);
    });
}
