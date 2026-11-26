use super::*;

mod positions;

/// This function returns the closing timestamp of the deadline day that contains `at`.
fn day_closed(at: u64) -> u64 {
    outbe_core_common::deadline_queue::day_end(
        outbe_core_common::deadline_queue::deadline_day(at).unwrap(),
    )
}

#[test]
fn issue_merchant_gem_mints_issued_and_drains_capacity() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        seed_day_vwap(storage, 840, rate);
        // source entry below coen -> entry follows coen.
        let id = seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        let capacity = sent_capacity(six_decimal_u128());

        let load = U256::from(10u64) * six_decimal_unit();
        let gem_id = runtime::issue_merchant_gem(
            storage,
            crate::schema::MerchantGemIssueParams {
                caller: ALICE,
                gem_factory_position_id: id,
                owner: BOB,
                promis_load_minor: load,
                issuance_currency: 840,
                reference_currency: 840,
            },
        )
        .unwrap();

        let item = gem_view(storage, gem_id);
        assert_eq!(item.item.owner, BOB);
        assert_eq!(item.item.state, GemState::Issued as u8);
        assert_eq!(item.item.entry_price_minor, rate); // max(coen, source_entry) = coen
        assert_eq!(
            runtime::gem_cost_minor(&item).unwrap(),
            U256::from(20u64) * six_decimal_unit()
        ); // entry * load
        assert_eq!(
            item.terms.call_price_minor,
            rate * U256::from(228u64) / U256::from(100u64)
        );

        let factory = GemFactoryContract::new(storage.clone());
        let rec = factory.positions.get(id).unwrap().unwrap();
        assert_eq!(rec.remaining_capacity_minor, capacity - load);
        assert_eq!(factory.total_gems_issued.read().unwrap(), U256::from(1u64));
    });
}

#[test]
fn issue_merchant_gem_anchors_entry_to_source() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        seed_day_vwap(storage, 840, rate);
        // source entry above coen -> it dominates.
        let source_entry = U256::from(3u64) * six_decimal_unit();
        let id = seed_and_send(storage, source_entry, six_decimal_u128());

        let gem_id = runtime::issue_merchant_gem(
            storage,
            crate::schema::MerchantGemIssueParams {
                caller: ALICE,
                gem_factory_position_id: id,
                owner: BOB,
                promis_load_minor: six_decimal_unit(),
                issuance_currency: 840,
                reference_currency: 840,
            },
        )
        .unwrap();
        let item = gem_view(storage, gem_id);
        assert_eq!(item.item.entry_price_minor, source_entry);
    });
}

#[test]
fn issue_merchant_gem_charges_one_minor_unit_when_the_cost_rounds_to_zero() {
    with_storage(Some(U256::ONE), |storage| {
        seed_day_vwap(storage, 840, U256::ONE);
        let id = seed_and_send(storage, U256::ONE, six_decimal_u128());

        let gem_id = runtime::issue_merchant_gem(
            storage,
            crate::schema::MerchantGemIssueParams {
                caller: ALICE,
                gem_factory_position_id: id,
                owner: BOB,
                promis_load_minor: U256::ONE,
                issuance_currency: 840,
                reference_currency: 840,
            },
        )
        .unwrap();
        let item = gem_view(storage, gem_id);
        assert_eq!(runtime::gem_cost_minor(&item).unwrap(), U256::ONE);
    });
}

#[test]
fn issue_merchant_gem_rejects_non_merchant() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let id = seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        // BOB is not the position's merchant (ALICE) - must reject.
        let r = runtime::issue_merchant_gem(
            storage,
            crate::schema::MerchantGemIssueParams {
                caller: BOB,
                gem_factory_position_id: id,
                owner: BOB,
                promis_load_minor: six_decimal_unit(),
                issuance_currency: 840,
                reference_currency: 840,
            },
        );
        assert_eq!(
            r.unwrap_err().to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GemFactoryError::NotGemFactoryPositionOwner
            )
            .to_string()
        );
    });
}

#[test]
fn issue_merchant_gem_over_capacity_rejects() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let id = seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        let over = sent_capacity(six_decimal_u128()) + U256::from(1u64);
        let r = runtime::issue_merchant_gem(
            storage,
            crate::schema::MerchantGemIssueParams {
                caller: ALICE,
                gem_factory_position_id: id,
                owner: BOB,
                promis_load_minor: over,
                issuance_currency: 840,
                reference_currency: 840,
            },
        );
        assert_eq!(
            r.unwrap_err().to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GemFactoryError::InsufficientCapacity
            )
            .to_string()
        );
    });
}

#[test]
fn an_unpriced_source_emission_day_issues_nothing() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        seed_day_vwap(storage, 840, rate);
        // A zero source entry leaves the source's emission day without a VWAP.
        let id = seed_and_send(storage, U256::ZERO, six_decimal_u128());
        let r = runtime::issue_merchant_gem(
            storage,
            crate::schema::MerchantGemIssueParams {
                caller: ALICE,
                gem_factory_position_id: id,
                owner: BOB,
                promis_load_minor: six_decimal_unit(),
                issuance_currency: 840,
                reference_currency: 840,
            },
        );
        assert_eq!(
            r.unwrap_err().to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GemFactoryError::OracleUnavailable
            )
            .to_string()
        );
        let position = GemFactoryContract::new(storage.clone())
            .positions
            .get(id)
            .unwrap()
            .unwrap();
        assert_eq!(
            position.remaining_capacity_minor,
            sent_capacity(six_decimal_u128())
        );
    });
}

#[test]
fn equal_prices_take_the_market_price() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        seed_day_vwap(storage, 840, rate);
        let id = seed_and_send(storage, rate, six_decimal_u128());
        let gem_id = runtime::issue_merchant_gem(
            storage,
            crate::schema::MerchantGemIssueParams {
                caller: ALICE,
                gem_factory_position_id: id,
                owner: BOB,
                promis_load_minor: six_decimal_unit(),
                issuance_currency: 840,
                reference_currency: 840,
            },
        )
        .unwrap();
        assert_eq!(gem_view(storage, gem_id).item.entry_price_minor, rate);
    });
}

fn issue_in(
    storage: &StorageHandle<'_>,
    gem_factory_position_id: U256,
    (issuance_currency, reference_currency): (u16, u16),
) -> outbe_primitives::error::Result<U256> {
    runtime::issue_merchant_gem(
        storage,
        crate::schema::MerchantGemIssueParams {
            caller: ALICE,
            gem_factory_position_id,
            owner: BOB,
            promis_load_minor: six_decimal_unit(),
            issuance_currency,
            reference_currency,
        },
    )
}

#[test]
fn a_merchant_gem_names_a_reference_other_than_its_source() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let id = seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        let eur_market = U256::from(3u64) * six_decimal_unit();
        outbe_oracle::test_support::record_finalized_day_vwap(
            storage,
            978,
            source_series_id().day(),
            rate,
        )
        .unwrap();
        seed_hour_vwap(storage, 978, eur_market);

        let gem_id = issue_in(storage, id, (978, 978)).unwrap();
        let item = gem_item(storage, gem_id);
        assert_eq!(
            (item.issuance_currency, item.reference_currency),
            (978, 978)
        );
        assert_eq!(item.entry_price_minor, eur_market);
    });
}

#[test]
fn a_merchant_gem_refuses_currencies_it_cannot_carry() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        seed_day_vwap(storage, 840, rate);
        let id = seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        // 756 (CHF) is not a reference currency, and 1 is not an ISO 4217 code.
        assert!(issue_in(storage, id, (840, 756)).is_err());
        assert_eq!(
            err_msg(issue_in(storage, id, (1, 840))),
            format!(
                "{:?}",
                outbe_primitives::error::PrecompileError::from(
                    crate::errors::GemFactoryError::InvalidCurrency { currency: 1 }
                )
            )
        );
        let position = GemFactoryContract::new(storage.clone())
            .positions
            .get(id)
            .unwrap()
            .unwrap();
        assert_eq!(
            position.remaining_capacity_minor,
            sent_capacity(six_decimal_u128())
        );
    });
}

#[test]
fn an_unpriced_issuance_currency_still_issues() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        seed_day_vwap(storage, 840, rate);
        let id = seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        // 36 (AUD) has no oracle price; the gem settles on its reference rail.
        let gem_id = issue_in(storage, id, (36, 840)).unwrap();
        assert_eq!(gem_item(storage, gem_id).issuance_currency, 36);
    });
}

#[test]
fn issue_merchant_gem_after_expiry_rejects() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        // Craft a position whose issued_at is already past the validity window.
        let gem_factory_position_id = U256::from(1u64);
        let mut factory = GemFactoryContract::new(storage.clone());
        factory
            .add_position(&GemFactoryPosition {
                gem_factory_position_id,
                owner: ALICE,
                source_series_id: source_series_id(),
                remaining_capacity_minor: U256::from(100u64) * six_decimal_unit(),
                issued_at: T_NOW - GEM_FACTORY_POSITION_VALIDITY_SECONDS - 1,
                expires_at: T_NOW - 1,
            })
            .unwrap();

        let r = runtime::issue_merchant_gem(
            storage,
            crate::schema::MerchantGemIssueParams {
                caller: ALICE,
                gem_factory_position_id,
                owner: BOB,
                promis_load_minor: six_decimal_unit(),
                issuance_currency: 840,
                reference_currency: 840,
            },
        );
        assert_eq!(
            r.unwrap_err().to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GemFactoryError::ExpiredGemFactoryPosition
            )
            .to_string()
        );
    });
}

#[test]
fn a_merchant_gem_issues_until_the_second_its_position_expires() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(rate));
    let (id, expires_at) = StorageHandle::enter(&mut provider, |storage| {
        let id = seed_and_send(&storage, six_decimal_unit(), six_decimal_u128());
        let expires_at = runtime::gem_factory_position_data(&storage, id)
            .unwrap()
            .expiresAt;
        storage
            .set_block_timestamp(U256::from(expires_at - 1))
            .unwrap();
        seed_hour_vwap(&storage, 840, rate);
        (id, expires_at)
    });
    let mut issue = |now: u64, load: U256| {
        provider.set_timestamp(U256::from(now));
        StorageHandle::enter(&mut provider, |storage| {
            let issued = runtime::issue_merchant_gem(
                &storage,
                crate::schema::MerchantGemIssueParams {
                    caller: ALICE,
                    gem_factory_position_id: id,
                    owner: BOB,
                    promis_load_minor: load,
                    issuance_currency: 840,
                    reference_currency: 840,
                },
            );
            let remaining = runtime::gem_factory_position_data(&storage, id)
                .unwrap()
                .remainingCapacityMinor;
            (issued, remaining)
        })
    };
    let capacity = sent_capacity(six_decimal_u128());

    let (zero, remaining) = issue(expires_at - 1, U256::ZERO);
    assert_eq!(
        zero.unwrap_err().to_string(),
        outbe_primitives::error::PrecompileError::from(
            crate::errors::GemFactoryError::ZeroPromisLoad
        )
        .to_string()
    );
    assert_eq!(remaining, capacity);

    let (issued, remaining) = issue(expires_at - 1, six_decimal_unit());
    issued.unwrap();
    assert_eq!(remaining, capacity - six_decimal_unit());

    let (expired, after) = issue(expires_at, six_decimal_unit());
    assert_eq!(
        expired.unwrap_err().to_string(),
        outbe_primitives::error::PrecompileError::from(
            crate::errors::GemFactoryError::ExpiredGemFactoryPosition
        )
        .to_string()
    );
    assert_eq!(after, U256::ZERO, "an expired position reads empty");
}

#[test]
fn issue_merchant_gem_prices_at_the_vwap_of_its_hour() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let vwap = U256::from(3u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        seed_day_vwap(storage, 840, vwap);
        let id = seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        let gem_id = runtime::issue_merchant_gem(
            storage,
            crate::schema::MerchantGemIssueParams {
                caller: ALICE,
                gem_factory_position_id: id,
                owner: BOB,
                promis_load_minor: six_decimal_unit(),
                issuance_currency: 840,
                reference_currency: 840,
            },
        )
        .unwrap();
        let item = gem_view(storage, gem_id);
        assert_eq!(item.item.entry_price_minor, vwap);
        assert_eq!(
            item.terms.call_price_minor,
            vwap * U256::from(228u64) / U256::from(100u64)
        );
    });
}

#[test]
fn issue_merchant_gem_ignores_the_previous_day_vwap() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let vwap = U256::from(3u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        seed_day_vwap(storage, 840, vwap);
        let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), 840)
            .unwrap()
            .unwrap();
        OracleContract::new(storage.clone())
            .record_utc_day_vwap(
                previous_date_key(timestamp_to_date_key(T_NOW)).expect("date step"),
                index,
                U256::from(9u64) * six_decimal_unit(),
            )
            .unwrap();
        assert_eq!(merchant_entry_price(storage, six_decimal_unit()), vwap);
    });
}

#[test]
fn issue_merchant_gem_ignores_a_spot_above_the_vwap_of_its_hour() {
    let rate = U256::from(5u64) * six_decimal_unit();
    let vwap = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        seed_day_vwap(storage, 840, vwap);
        assert_eq!(merchant_entry_price(storage, six_decimal_unit()), vwap);
    });
}

#[test]
fn issue_merchant_gem_source_entry_dominates_the_market_price() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let source_entry = U256::from(4u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        seed_day_vwap(storage, 840, U256::from(3u64) * six_decimal_unit());
        assert_eq!(merchant_entry_price(storage, source_entry), source_entry);
    });
}

/// The entry is the higher of the market and the source entry. A tie takes the shared value.
#[test]
fn issue_merchant_gem_takes_the_higher_entry() {
    let unit = six_decimal_unit();
    let rate = U256::from(2u64) * unit;
    // (day VWAP, source entry, expected entry)
    for (vwap, source_entry, entry) in [
        (
            U256::from(5u64) * unit,
            U256::from(5u64) * unit,
            U256::from(5u64) * unit,
        ),
        (U256::from(4u64) * unit, unit, U256::from(4u64) * unit),
    ] {
        with_storage(Some(rate), |storage| {
            seed_day_vwap(storage, 840, vwap);
            let id = seed_and_send(storage, source_entry, six_decimal_u128());
            let gem_id = runtime::issue_merchant_gem(
                storage,
                crate::schema::MerchantGemIssueParams {
                    caller: ALICE,
                    gem_factory_position_id: id,
                    owner: BOB,
                    promis_load_minor: six_decimal_unit(),
                    issuance_currency: 840,
                    reference_currency: 840,
                },
            )
            .unwrap();
            let item = gem_view(storage, gem_id);
            assert_eq!(item.item.entry_price_minor, entry);
        });
    }
}

/// An agent gem prices its cost and call from the entry at every rate.
#[test]
fn an_agent_gem_prices_cost_and_call_from_the_entry_at_every_rate() {
    let unit = six_decimal_unit();
    let load = U256::from(10u64) * unit;
    for rate in [unit, U256::from(2u64) * unit, U256::from(7u64) * unit] {
        with_storage(Some(rate), |storage| {
            let gem_id = issue_at_live_rate(storage, ALICE, load, 840, 840).unwrap();
            let item = gem_view(storage, gem_id);
            assert_eq!(item.item.entry_price_minor, rate);
            assert_eq!(
                item.terms.call_price_minor,
                rate * U256::from(228u64) / U256::from(100u64)
            );
            assert_eq!(runtime::gem_cost_minor(&item).unwrap(), load * rate / unit);
        });
    }
}

#[test]
fn issue_merchant_gem_without_a_vwap_of_its_hour_takes_no_capacity() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let id = seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        let capacity = sent_capacity(six_decimal_u128());
        let r = runtime::issue_merchant_gem(
            storage,
            crate::schema::MerchantGemIssueParams {
                caller: ALICE,
                gem_factory_position_id: id,
                owner: BOB,
                promis_load_minor: six_decimal_unit(),
                issuance_currency: 840,
                reference_currency: 840,
            },
        );
        assert_eq!(
            r.unwrap_err().to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GemFactoryError::OracleUnavailable
            )
            .to_string()
        );
        assert_eq!(
            runtime::gem_factory_position_data(storage, id)
                .unwrap()
                .remainingCapacityMinor,
            capacity
        );
    });
}

/// Issuance events carry what an indexer needs to follow a gem through its call.
#[test]
fn issuance_events_carry_the_call_terms_the_position_and_the_group() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(rate));
    let (position, expected) = StorageHandle::enter(&mut provider, |storage| {
        seed_day_vwap(&storage, 840, rate);
        let position = seed_and_send(&storage, six_decimal_unit(), six_decimal_u128());
        let merchant_gem = runtime::issue_merchant_gem(
            &storage,
            crate::schema::MerchantGemIssueParams {
                caller: ALICE,
                gem_factory_position_id: position,
                owner: BOB,
                promis_load_minor: six_decimal_unit(),
                issuance_currency: 840,
                reference_currency: 840,
            },
        )
        .unwrap();
        let wallet_gem = issue_at_live_rate(&storage, ALICE, six_decimal_unit(), 840, 840).unwrap();
        let expected = [merchant_gem, wallet_gem].map(|gem_id| gem_view(&storage, gem_id));
        (position, expected)
    });

    let events = provider.get_ordered_events();
    let issued: Vec<_> = events
        .iter()
        .filter(|log| log.address == outbe_system_common::addresses::GEM_ADDRESS)
        .filter_map(|log| outbe_gem::precompile::IGem::GemIssued::decode_log(log).ok())
        .map(|log| log.data)
        .collect();
    assert_eq!(issued.len(), 2);
    for (event, item) in issued.iter().zip(&expected) {
        assert_eq!(event.gemId, item.item.gem_id);
        assert_eq!(event.owner, item.item.owner);
        assert_eq!(
            outbe_intex::SeriesId::from(event.seriesId),
            item.item.series_id
        );
        assert_eq!(event.subSeriesId, item.item.sub_series_id);
        assert_eq!(event.issuanceCurrency, item.item.issuance_currency);
        assert_eq!(event.referenceCurrency, item.item.reference_currency);
        assert_eq!(
            event.payload.as_ref(),
            item.item.promis_load_minor.to_be_bytes::<32>().as_slice()
        );
    }

    let opened: Vec<_> = events
        .iter()
        .filter_map(|log| {
            crate::precompile::IGemFactory::GemFactoryPositionCreated::decode_log(log).ok()
        })
        .map(|log| log.data)
        .collect();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].gemFactoryPositionId, position);
    assert_eq!(opened[0].owner, ALICE);
    assert_eq!(
        outbe_intex::SeriesId::from(opened[0].sourceSeriesId),
        source_series_id()
    );
    assert_eq!(opened[0].issuanceCurrency, 840);
    assert_eq!(opened[0].referenceCurrency, 840);
    assert_eq!(
        opened[0].payload.as_ref(),
        sent_capacity(six_decimal_u128())
            .to_be_bytes::<32>()
            .as_slice()
    );

    // Only the merchant gem names its position, and no Gem token event does.
    let linked: Vec<_> = events
        .iter()
        .filter(|log| log.address == outbe_system_common::addresses::GEM_FACTORY_ADDRESS)
        .filter_map(|log| {
            crate::precompile::IGemFactory::GemFactoryPositionRedeemed::decode_log(log).ok()
        })
        .map(|log| {
            (
                log.data.gemFactoryPositionId,
                log.data.owner,
                log.data.promisLoadMinor,
                log.data.gemId,
            )
        })
        .collect();
    assert_eq!(
        linked,
        vec![(position, ALICE, six_decimal_unit(), expected[0].item.gem_id)]
    );
}

fn set_source_price(storage: &StorageHandle<'_>, price: U256) {
    let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), 840)
        .unwrap()
        .expect("COEN pair registered");
    OracleContract::new(storage.clone())
        .record_utc_day_vwap(source_series_id().day(), index, price)
        .unwrap();
}

fn merchant_gem_to(
    storage: &StorageHandle<'_>,
    gem_factory_position_id: U256,
    owner: Address,
) -> outbe_primitives::error::Result<U256> {
    runtime::issue_merchant_gem(
        storage,
        crate::schema::MerchantGemIssueParams {
            caller: ALICE,
            gem_factory_position_id,
            owner,
            promis_load_minor: six_decimal_unit(),
            issuance_currency: 840,
            reference_currency: 840,
        },
    )
}

#[test]
fn a_series_numbers_each_source_call_price_on_first_use() {
    let unit = six_decimal_unit();
    let mut provider = test_storage(Some(U256::from(2u64) * unit));
    StorageHandle::enter(&mut provider, |storage| {
        seed_day_vwap(&storage, 840, U256::from(2u64) * unit);
        let id = seed_and_send(&storage, U256::from(3u64) * unit, six_decimal_u128());
        let hour = T_NOW_HOUR;
        // (source VWAP, sub-series): above the market numbers it from 24, at or below takes
        // the issue hour.
        for (owner, source, sub_series) in [
            (0x31, 3u64, 24u16),
            (0x32, 4, 25),
            (0x33, 3, 24),
            (0x34, 1, hour),
            (0x35, 2, hour),
            (0x36, 4, 25),
        ] {
            set_source_price(&storage, U256::from(source) * unit);
            let gem_id = merchant_gem_to(&storage, id, Address::repeat_byte(owner)).unwrap();
            let item = gem_item(&storage, gem_id);
            assert_eq!(item.sub_series_id, sub_series, "source {source}");
            assert_eq!(
                gem_id,
                GemContract::generate_gem_id(
                    item.series_id,
                    sub_series,
                    ALICE,
                    item.owner,
                    storage.block_number().unwrap(),
                )
            );
        }
    });
    let opened: Vec<_> = provider
        .get_ordered_events()
        .iter()
        .filter_map(|log| crate::precompile::IGemFactory::GemSeriesOpened::decode_log(log).ok())
        .map(|log| (log.data.subSeriesId, log.data.entryPriceMinor))
        .collect();
    let hour = T_NOW_HOUR;
    assert_eq!(
        opened,
        vec![
            (24, U256::from(3u64) * unit),
            (25, U256::from(4u64) * unit),
            (hour, U256::from(2u64) * unit),
        ]
    );
}

#[test]
fn gem_series_is_the_issue_day_with_a_sub_series_for_each_hour() {
    let unit = six_decimal_unit();
    let market = U256::from(2u64) * unit;
    let next_day = T_NOW - T_NOW % SECONDS_PER_DAY + SECONDS_PER_DAY;
    let mut provider = test_storage(Some(market));
    let (gems, hour_prices) = StorageHandle::enter(&mut provider, |storage| {
        let id = seed_and_send(&storage, unit, six_decimal_u128());
        let mut hour_prices = Vec::new();
        let gems = [
            (14u64, 30u64, market),
            (15, 10, market),
            (16, 5, U256::from(5u64) * unit),
        ]
        .map(|(hour, minute, seeded)| {
            let now = next_day + hour * 3_600 + minute * 60;
            storage.set_block_timestamp(U256::from(now)).unwrap();
            seed_hour_vwap(&storage, 840, seeded);
            let snapshot = outbe_oracle::api::current_vwap_24h_snapshot(storage.clone()).unwrap();
            hour_prices.push(
                outbe_oracle::api::vwap_24h(storage.clone(), 840, snapshot)
                    .unwrap()
                    .unwrap(),
            );
            let gem_id = merchant_gem_to(&storage, id, Address::repeat_byte(0x40 + hour as u8));
            gem_item(&storage, gem_id.unwrap())
        });
        (gems, hour_prices)
    });
    for ((gem, hour), price) in gems.iter().zip([14u16, 15, 16]).zip(&hour_prices) {
        assert_eq!(gem.series_id.day(), timestamp_to_date_key(next_day));
        assert_eq!(gem.sub_series_id, hour);
        assert_eq!(
            gem.entry_price_minor, *price,
            "the window that ends at {hour}:00"
        );
    }
    assert_eq!(hour_prices[0], market);
    assert_ne!(hour_prices[2], market);
    // Two hours at one call price are two sub-series of one call group; another price is not.
    assert_eq!(gems[0].group_key, gems[1].group_key);
    assert_ne!(gems[0].group_key, gems[2].group_key);
    let opened: Vec<_> = provider
        .get_ordered_events()
        .iter()
        .filter_map(|log| crate::precompile::IGemFactory::GemSeriesOpened::decode_log(log).ok())
        .map(|log| log.data.subSeriesId)
        .collect();
    assert_eq!(opened, vec![14, 15, 16]);

    StorageHandle::enter(&mut provider, |storage| {
        let storage = &storage;
        let group = gems[0].group_key;
        let mut gem = GemContract::new(storage.clone());
        let members = gem.group_sub_series(group).unwrap();
        assert_eq!(
            members.iter().map(|m| m.sub_series_id).collect::<Vec<_>>(),
            vec![14, 15]
        );
        let called_at = next_day + 2 * SECONDS_PER_DAY;
        let terms = gem.read_call_terms(group).unwrap();
        crate::call::call_series(&mut gem, group, &terms, called_at).unwrap();
        let [first, second, other] = gems.each_ref().map(|g| gem_view(storage, g.gem_id));
        assert_eq!(first.group_called_at, called_at);
        assert_eq!(
            (second.group_called_at, second.settlement_deadline()),
            (called_at, first.settlement_deadline())
        );
        assert_eq!(other.group_called_at, 0);

        let before = unallocated(storage);
        daily_pass(&block_ctx(storage, day_closed(first.settlement_deadline()))).unwrap();
        assert_eq!(
            unallocated(storage),
            before + gems[0].promis_load_minor + gems[1].promis_load_minor
        );
        assert_eq!(
            gem_view(storage, gems[2].gem_id).item.state,
            GemState::Issued as u8
        );
    });
}

#[test]
fn one_owner_gets_one_gem_of_a_sub_series_in_a_block() {
    let unit = six_decimal_unit();
    let mut provider = test_storage(Some(U256::from(2u64) * unit));
    let id = StorageHandle::enter(&mut provider, |storage| {
        let storage = &storage;
        seed_day_vwap(storage, 840, U256::from(2u64) * unit);
        let id = seed_and_send(storage, U256::from(3u64) * unit, six_decimal_u128());
        merchant_gem_to(storage, id, BOB).unwrap();
        set_source_price(storage, U256::from(4u64) * unit);
        merchant_gem_to(storage, id, BOB).unwrap();
        set_source_price(storage, U256::from(3u64) * unit);
        let err = merchant_gem_to(storage, id, BOB).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_gem::errors::GemError::AlreadyExists
            )
            .to_string()
        );
        id
    });
    provider.set_block_number(1);
    StorageHandle::enter(&mut provider, |storage| {
        merchant_gem_to(&storage, id, BOB).unwrap();
    });
}

#[test]
fn gem_id_holds_the_issuer() {
    let unit = six_decimal_unit();
    with_storage(Some(U256::from(2u64) * unit), |storage| {
        seed_day_vwap(storage, 840, U256::from(2u64) * unit);
        let id = seed_and_send(storage, U256::from(3u64) * unit, six_decimal_u128());
        set_source_price(storage, unit);
        let merchant = gem_item(storage, merchant_gem_to(storage, id, BOB).unwrap());
        let reward = gem_item(
            storage,
            issue_at_live_rate(storage, BOB, unit, 840, 840).unwrap(),
        );
        assert_ne!(merchant.gem_id, reward.gem_id);
        assert_eq!(
            (merchant.series_id, merchant.sub_series_id, merchant.owner),
            (reward.series_id, reward.sub_series_id, reward.owner)
        );
        assert_eq!(merchant.issuer, ALICE);
        assert_eq!(
            reward.issuer,
            outbe_system_common::addresses::AGENT_REWARD_ADDRESS
        );
    });
}

#[test]
fn forfeited_gem_loads_go_to_the_promis_limit_once() {
    let unit = six_decimal_unit();
    with_storage(Some(U256::from(2u64) * unit), |storage| {
        seed_day_vwap(storage, 840, U256::from(2u64) * unit);
        let id = seed_and_send(storage, U256::from(3u64) * unit, six_decimal_u128());
        set_source_price(storage, unit);
        let merchant = merchant_gem_to(storage, id, BOB).unwrap();
        let reward = issue_at_live_rate(storage, BOB, unit, 840, 840).unwrap();
        let settled = issue_at_live_rate(storage, ALICE, unit, 840, 840).unwrap();
        force_settled(storage, settled);
        let group = gem_item(storage, merchant).group_key;
        for gem_id in [reward, settled] {
            assert_eq!(gem_item(storage, gem_id).group_key, group);
        }
        let mut gem = GemContract::new(storage.clone());
        let terms = gem.read_call_terms(group).unwrap();
        crate::call::call_series(&mut gem, group, &terms, T_NOW).unwrap();
        let forfeited = gem_item(storage, merchant).promis_load_minor
            + gem_item(storage, reward).promis_load_minor;
        let before = unallocated(storage);

        let closed = day_closed(gem_view(storage, merchant).settlement_deadline());
        daily_pass(&block_ctx(storage, closed)).unwrap();
        assert_eq!(unallocated(storage), before + forfeited);
        assert_eq!(
            gem_view(storage, settled).item.state,
            GemState::Settled as u8
        );

        daily_pass(&block_ctx(storage, closed + SECONDS_PER_DAY)).unwrap();
        assert_eq!(unallocated(storage), before + forfeited);
    });
}

#[test]
fn a_reward_gem_is_in_the_sub_series_of_its_hour() {
    with_storage(Some(U256::from(2u64) * six_decimal_unit()), |storage| {
        let gem_id = issue_at_live_rate(storage, ALICE, six_decimal_unit(), 840, 840).unwrap();
        assert_eq!(gem_item(storage, gem_id).sub_series_id, T_NOW_HOUR);
    });
}

#[test]
fn a_discounted_reward_gem_shares_the_market_sub_series_with_a_merchant_gem() {
    let unit = six_decimal_unit();
    let market = U256::from(2u64) * unit;
    with_storage(Some(market), |storage| {
        seed_day_vwap(storage, 840, market);
        let id = seed_and_send(storage, U256::from(3u64) * unit, six_decimal_u128());
        set_source_price(storage, unit);
        let merchant = gem_item(storage, merchant_gem_to(storage, id, BOB).unwrap());
        let discounted = market * U256::from(96u64) / U256::from(100u64);
        let reward = runtime::issue_gem(
            storage,
            GemIssueParams {
                owner: ALICE,
                promis_load_minor: unit,
                issuance_currency: 840,
                reference_currency: 840,
                entry_price_minor: discounted,
                call_anchor_price_minor: market,
            },
        )
        .unwrap();
        let reward = gem_item(storage, reward);
        assert_eq!(
            (reward.series_id, reward.sub_series_id, reward.group_key),
            (
                merchant.series_id,
                merchant.sub_series_id,
                merchant.group_key
            )
        );
        assert_eq!(merchant.sub_series_id, T_NOW_HOUR);
        assert_eq!(merchant.entry_price_minor, market);
        assert_eq!(reward.entry_price_minor, discounted);
    });
}
