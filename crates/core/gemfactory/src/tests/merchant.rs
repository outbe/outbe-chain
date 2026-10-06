use super::*;

#[test]
fn a_series_called_in_the_same_block_cannot_be_sent() {
    assert_called_source_cannot_be_sent(T_NOW);
}

#[test]
fn a_called_series_at_its_deadline_cannot_be_sent() {
    assert_called_source_cannot_be_sent(T_NOW - u64::from(SOURCE_NOTICE_SECONDS));
}

#[test]
fn a_series_past_its_deadline_cannot_be_sent() {
    assert_called_source_cannot_be_sent(T_NOW - u64::from(SOURCE_NOTICE_SECONDS) - 1);
}

#[test]
fn a_qualified_series_is_sent_once_without_a_promis_limit_credit() {
    with_storage(Some(six_decimal_unit()), |storage| {
        let floor = six_decimal_unit();
        seed_source_series(
            storage,
            six_decimal_unit(),
            floor,
            six_decimal_u128(),
            outbe_intex::IntexCallTrigger::default(),
        );
        let day = outbe_primitives::time::first_full_day(T_NOW);
        let oracle = OracleContract::new(storage.clone());
        let pair = oracle
            .pair_index_of(outbe_oracle::api::AddressPair::new_coen_to(840))
            .unwrap();
        oracle
            .record_utc_day_vwap(day, pair, floor + U256::ONE)
            .unwrap();
        oracle.utc_day_vwap_last_finalized.write(day).unwrap();
        assert!(outbe_oracle::api::closed_above_floor(storage.clone(), 840, floor, day).unwrap());
        let promis_limit = unallocated(storage);

        let id = send_whole_holding(storage).unwrap();

        let capacity = sent_capacity(six_decimal_u128());
        let factory = GemFactoryContract::new(storage.clone());
        assert_eq!(
            factory
                .positions
                .get(id)
                .unwrap()
                .unwrap()
                .remaining_capacity_minor,
            capacity
        );
        assert_eq!(factory.total_capacity_minor.read().unwrap(), capacity);
        assert_eq!(
            outbe_intex::api::gem_factory_units(storage, source_intex_id()).unwrap(),
            SENT_UNITS as u32
        );
        assert_eq!(unallocated(storage), promis_limit);
    });
}

#[test]
fn issue_gem_position_burns_sends_and_issues_nft() {
    with_storage(None, |storage| {
        let id = seed_and_send(
            storage,
            six_decimal_unit(),
            six_decimal_unit(),
            six_decimal_u128(),
        );
        let capacity = sent_capacity(six_decimal_u128());

        let factory = GemFactoryContract::new(storage.clone());
        let rec = factory.positions.get(id).unwrap().unwrap();
        assert_eq!(rec.merchant, ALICE);
        assert_eq!(rec.source_intex_id, source_intex_id());
        assert_eq!(rec.remaining_capacity_minor, capacity);
        assert_eq!(rec.source_entry_price_minor, six_decimal_unit());
        assert_eq!(factory.total_capacity_minor.read().unwrap(), capacity);

        // Position NFT issued to the merchant.
        assert_eq!(factory.owner_of(id).unwrap(), ALICE);
        assert_eq!(factory.balance_of(ALICE).unwrap(), 1);
        assert_eq!(factory.token_of_owner_by_index(ALICE, 0).unwrap(), id);
    });
}

#[test]
fn sending_marks_the_units_realized_on_the_source_series() {
    with_storage(None, |storage| {
        seed_and_send(
            storage,
            six_decimal_unit(),
            six_decimal_unit(),
            six_decimal_u128(),
        );
        // Their load lives in the position now, so the source series can no
        // longer forfeit them.
        assert_eq!(
            outbe_intex::api::gem_factory_units(storage, source_intex_id()).unwrap(),
            SENT_UNITS as u32
        );
    });
}

/// The position stays visible afterwards, as the spent object it is.
#[test]
fn an_expired_position_returns_its_remainder() {
    with_storage(None, |storage| {
        let id = seed_and_send(
            storage,
            six_decimal_unit(),
            six_decimal_unit(),
            six_decimal_u128(),
        );
        let capacity = sent_capacity(six_decimal_u128());

        let ctx = block_ctx(storage, T_NOW + POSITION_VALIDITY_SECONDS - 1);
        assert_eq!(expired::sweep_expired_positions(&ctx).unwrap(), 0);
        assert_eq!(unallocated(storage), U256::ZERO);

        // At the instant issuing starts reverting, the sweep takes it.
        let ctx = block_ctx(storage, T_NOW + POSITION_VALIDITY_SECONDS);
        assert_eq!(expired::sweep_expired_positions(&ctx).unwrap(), 1);
        assert_eq!(unallocated(storage), capacity);

        let factory = GemFactoryContract::new(storage.clone());
        let record = factory.positions.get(id).unwrap().unwrap();
        assert_eq!(record.remaining_capacity_minor, U256::ZERO);
        assert_eq!(factory.owner_of(id).unwrap(), ALICE);
    });
}

#[test]
fn a_drained_position_leaves_the_queue() {
    with_storage(Some(six_decimal_unit()), |storage| {
        seed_day_vwap(storage, 840, six_decimal_unit());
        let id = seed_and_send(
            storage,
            six_decimal_unit(),
            six_decimal_unit(),
            six_decimal_u128(),
        );
        runtime::issue_merchant_gem(storage, ALICE, id, BOB, sent_capacity(six_decimal_u128()))
            .unwrap();

        let factory = GemFactoryContract::new(storage.clone());
        assert!(factory.live_queue_slot(0).unwrap().is_none());

        let ctx = block_ctx(storage, T_NOW + POSITION_VALIDITY_SECONDS);
        assert_eq!(expired::sweep_expired_positions(&ctx).unwrap(), 0);
        assert_eq!(unallocated(storage), U256::ZERO);
    });
}

#[test]
fn issue_gem_position_unknown_source_rejects() {
    with_storage(None, |storage| {
        let r =
            runtime::issue_gem_position(storage, ALICE, source_intex_id(), U256::from(SENT_UNITS));
        assert!(err_msg(r).contains("source intex"));
    });
}

#[test]
fn issue_merchant_gem_mints_issued_and_drains_capacity() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        seed_day_vwap(storage, 840, rate);
        // source entry below coen -> entry follows coen.
        let id = seed_and_send(
            storage,
            six_decimal_unit(),
            six_decimal_unit(),
            six_decimal_u128(),
        );
        let capacity = sent_capacity(six_decimal_u128());

        let load = U256::from(10u64) * six_decimal_unit();
        let gem_id = runtime::issue_merchant_gem(storage, ALICE, id, BOB, load).unwrap();

        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        assert_eq!(item.owner, BOB);
        assert_eq!(item.gem_type, GemTypes::Merchant as u8);
        assert_eq!(item.state, GemState::Issued as u8);
        assert_eq!(item.entry_price_minor, rate); // max(coen, source_entry) = coen
        assert_eq!(
            runtime::gem_cost_minor(&item).unwrap(),
            U256::from(20u64) * six_decimal_unit()
        ); // entry * load
        assert_eq!(
            item.floor_price_minor,
            rate * U256::from(108u64) / U256::from(100u64)
        );
        assert_eq!(
            item.call_price_minor,
            rate * U256::from(228u64) / U256::from(100u64)
        );

        let factory = GemFactoryContract::new(storage.clone());
        let rec = factory.positions.get(id).unwrap().unwrap();
        assert_eq!(rec.remaining_capacity_minor, capacity - load);
        assert_eq!(factory.total_gems_issued.read().unwrap(), U256::from(1u64));
    });
}

#[test]
fn issue_merchant_gem_anchors_entry_and_floor_to_source() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        seed_day_vwap(storage, 840, rate);
        // source entry above coen, source floor above 1.08 * entry -> both dominate.
        let source_entry = U256::from(3u64) * six_decimal_unit();
        let source_floor = U256::from(5u64) * six_decimal_unit();
        let id = seed_and_send(storage, source_entry, source_floor, six_decimal_u128());

        let gem_id =
            runtime::issue_merchant_gem(storage, ALICE, id, BOB, six_decimal_unit()).unwrap();
        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        assert_eq!(item.entry_price_minor, source_entry);
        assert_eq!(item.floor_price_minor, source_floor);
    });
}

#[test]
fn issue_merchant_gem_charges_one_minor_unit_when_the_cost_rounds_to_zero() {
    with_storage(Some(U256::ONE), |storage| {
        seed_day_vwap(storage, 840, U256::ONE);
        let id = seed_and_send(storage, U256::ONE, U256::ONE, six_decimal_u128());

        let gem_id = runtime::issue_merchant_gem(storage, ALICE, id, BOB, U256::ONE).unwrap();
        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        assert_eq!(runtime::gem_cost_minor(&item).unwrap(), U256::ONE);
    });
}

#[test]
fn issue_merchant_gem_rejects_non_merchant() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let id = seed_and_send(
            storage,
            six_decimal_unit(),
            six_decimal_unit(),
            six_decimal_u128(),
        );
        // BOB is not the position's merchant (ALICE) - must reject.
        let r = runtime::issue_merchant_gem(storage, BOB, id, BOB, six_decimal_unit());
        assert!(err_msg(r).contains("position owner"));
    });
}

#[test]
fn issue_merchant_gem_over_capacity_rejects() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let id = seed_and_send(
            storage,
            six_decimal_unit(),
            six_decimal_unit(),
            six_decimal_u128(),
        );
        let over = sent_capacity(six_decimal_u128()) + U256::from(1u64);
        let r = runtime::issue_merchant_gem(storage, ALICE, id, BOB, over);
        assert!(err_msg(r).contains("capacity"));
    });
}

#[test]
fn issue_merchant_gem_after_expiry_rejects() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        // Craft a position whose issued_at is already past the validity window.
        let position_id = U256::from(1u64);
        let mut factory = GemFactoryContract::new(storage.clone());
        factory
            .add_position(&GemPosition {
                position_id,
                merchant: ALICE,
                source_intex_id: source_intex_id(),
                remaining_capacity_minor: U256::from(100u64) * six_decimal_unit(),
                source_entry_price_minor: six_decimal_unit(),
                source_floor_price_minor: six_decimal_unit(),
                issuance_currency: 840,
                reference_currency: 840,
                issued_at: T_NOW - POSITION_VALIDITY_SECONDS - 1,
                expires_at: T_NOW - 1,
            })
            .unwrap();

        let r = runtime::issue_merchant_gem(storage, ALICE, position_id, BOB, six_decimal_unit());
        assert!(err_msg(r).contains("expired"));
    });
}

#[test]
fn a_merchant_gem_issues_until_the_second_its_position_expires() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(rate));
    let (id, expires_at) = StorageHandle::enter(&mut provider, |storage| {
        let id = seed_and_send(
            &storage,
            six_decimal_unit(),
            six_decimal_unit(),
            six_decimal_u128(),
        );
        let expires_at = runtime::position_data(&storage, id).unwrap().expiresAt;
        let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), 840)
            .unwrap()
            .unwrap();
        OracleContract::new(storage.clone())
            .record_utc_day_vwap(
                previous_date_key(timestamp_to_date_key(expires_at - 1)),
                index,
                rate,
            )
            .unwrap();
        (id, expires_at)
    });
    let mut issue = |now: u64, load: U256| {
        provider.set_timestamp(U256::from(now));
        StorageHandle::enter(&mut provider, |storage| {
            let issued = runtime::issue_merchant_gem(&storage, ALICE, id, BOB, load);
            let remaining = runtime::position_data(&storage, id)
                .unwrap()
                .remainingCapacityMinor;
            (issued, remaining)
        })
    };
    let capacity = sent_capacity(six_decimal_u128());

    let (zero, remaining) = issue(expires_at - 1, U256::ZERO);
    assert!(err_msg(zero).contains("promis load must be positive"));
    assert_eq!(remaining, capacity);

    let (issued, remaining) = issue(expires_at - 1, six_decimal_unit());
    issued.unwrap();
    assert_eq!(remaining, capacity - six_decimal_unit());

    let (expired, after) = issue(expires_at, six_decimal_unit());
    assert!(err_msg(expired).contains("position expired"));
    assert_eq!(after, remaining);
}

#[test]
fn issue_merchant_gem_prices_at_the_previous_day_vwap() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let vwap = U256::from(3u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        seed_day_vwap(storage, 840, vwap);
        let id = seed_and_send(
            storage,
            six_decimal_unit(),
            six_decimal_unit(),
            six_decimal_u128(),
        );
        let gem_id =
            runtime::issue_merchant_gem(storage, ALICE, id, BOB, six_decimal_unit()).unwrap();
        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        assert_eq!(item.entry_price_minor, vwap);
        assert_eq!(
            item.floor_price_minor,
            vwap * U256::from(108u64) / U256::from(100u64)
        );
        assert_eq!(
            item.call_price_minor,
            vwap * U256::from(228u64) / U256::from(100u64)
        );
    });
}

#[test]
fn issue_merchant_gem_ignores_the_trailing_settlement_window() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let vwap = U256::from(3u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        seed_day_vwap(storage, 840, vwap);
        let snapshot = outbe_oracle::api::current_vwap_snapshot(storage.clone()).unwrap();
        OracleContract::new(storage.clone())
            .write_snapshot(
                snapshot.cutoff() - 1,
                &[(
                    outbe_oracle::api::DAY_TYPE_PAIR,
                    U256::from(9u64) * six_decimal_unit(),
                    U256::from(1_000u64) * six_decimal_unit(),
                )],
            )
            .unwrap();
        assert_eq!(merchant_entry_price(storage, six_decimal_unit()), vwap);
    });
}

#[test]
fn issue_merchant_gem_ignores_a_spot_above_the_previous_day_vwap() {
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

/// Entry and floor are two independent maxima against the source terms: a tie takes the shared
/// value, and the floor can come from the source while the entry comes from the market.
#[test]
fn issue_merchant_gem_takes_each_maximum_independently() {
    let unit = six_decimal_unit();
    let rate = U256::from(2u64) * unit;
    let markup = |entry: U256| entry * U256::from(108u64) / U256::from(100u64);
    // (day VWAP, source entry, source floor, expected entry, expected floor)
    for (vwap, source_entry, source_floor, entry, floor) in [
        (
            U256::from(5u64) * unit,
            U256::from(5u64) * unit,
            markup(U256::from(5u64) * unit),
            U256::from(5u64) * unit,
            markup(U256::from(5u64) * unit),
        ),
        (
            U256::from(4u64) * unit,
            unit,
            U256::from(6u64) * unit,
            U256::from(4u64) * unit,
            U256::from(6u64) * unit,
        ),
    ] {
        with_storage(Some(rate), |storage| {
            seed_day_vwap(storage, 840, vwap);
            let id = seed_and_send(storage, source_entry, source_floor, six_decimal_u128());
            let gem_id =
                runtime::issue_merchant_gem(storage, ALICE, id, BOB, six_decimal_unit()).unwrap();
            let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
            assert_eq!(item.entry_price_minor, entry);
            assert_eq!(item.floor_price_minor, floor);
        });
    }
}

/// Every Genesis gem carries a zero floor whatever its entry, and keeps its cost and call terms.
#[test]
fn a_genesis_floor_is_zero_at_every_entry_price() {
    let unit = six_decimal_unit();
    let load = U256::from(10u64) * unit;
    for rate in [unit, U256::from(2u64) * unit, U256::from(7u64) * unit] {
        with_storage(Some(rate), |storage| {
            let gem_id =
                issue_at_live_rate(storage, ALICE, GemTypes::Genesis, load, 840, 840).unwrap();
            let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
            assert_eq!(item.floor_price_minor, U256::ZERO);
            assert_eq!(item.entry_price_minor, rate);
            assert_eq!(
                item.call_price_minor,
                rate * U256::from(228u64) / U256::from(100u64)
            );
            assert_eq!(runtime::gem_cost_minor(&item).unwrap(), load * rate / unit);
        });
    }
}

#[test]
fn issue_merchant_gem_rejects_a_missing_previous_day_vwap() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let id = seed_and_send(
            storage,
            six_decimal_unit(),
            six_decimal_unit(),
            six_decimal_u128(),
        );
        let r = runtime::issue_merchant_gem(storage, ALICE, id, BOB, six_decimal_unit());
        assert!(err_msg(r).contains("oracle nominal unavailable"));
    });
}

#[test]
fn merged_paynote_settles_a_gem_without_additional_funding() {
    let mut provider = test_storage(Some(U256::from(2) * six_decimal_unit()));
    let (gem_id, cost, snapshot) = StorageHandle::enter(&mut provider, |storage| {
        let id = issue_at_live_rate(
            &storage,
            ALICE,
            GemTypes::Genesis,
            U256::from(10) * six_decimal_unit(),
            840,
            840,
        )
        .unwrap();
        seed_qualifying_day(&storage, id);
        let (_, cost, snapshot) = runtime::quote_settlement(&storage, id, STABLE).unwrap();
        (id, cost, snapshot)
    });
    let (proof, _) = outbe_paynote::test_support::merged_note_spend_proof(
        &mut provider,
        1,
        STABLE,
        gem_context(gem_id, snapshot),
        cost,
    );
    StorageHandle::enter(&mut provider, |storage| {
        runtime::settle_gem_with_paynote(&storage, ALICE, gem_id, &proof).unwrap();
        assert_eq!(
            gem_api::get_gem(&storage, gem_id).unwrap().unwrap().state,
            GemState::Settled as u8
        );
        assert!(runtime::settle_gem_with_paynote(&storage, ALICE, gem_id, &proof).is_err());
    });
}

/// Issuance events carry what an indexer needs to follow a gem through its call.
#[test]
fn issuance_events_carry_the_call_terms_the_position_and_the_bucket() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(rate));
    let (position, expected) = StorageHandle::enter(&mut provider, |storage| {
        seed_day_vwap(&storage, 840, rate);
        let position = seed_and_send(
            &storage,
            six_decimal_unit(),
            six_decimal_unit(),
            six_decimal_u128(),
        );
        let merchant_gem =
            runtime::issue_merchant_gem(&storage, ALICE, position, BOB, six_decimal_unit())
                .unwrap();
        let wallet_gem = issue_at_live_rate(
            &storage,
            ALICE,
            GemTypes::Wallet,
            six_decimal_unit(),
            840,
            840,
        )
        .unwrap();
        let expected = [(merchant_gem, position), (wallet_gem, U256::ZERO)].map(|(gem_id, pos)| {
            let item = gem_api::get_gem(&storage, gem_id).unwrap().unwrap();
            (item, pos, gem_api::bucket_of(&storage, gem_id).unwrap())
        });
        (position, expected)
    });

    let events = provider.get_ordered_events();
    let issued: Vec<_> = events
        .iter()
        .filter_map(|log| crate::precompile::IGemFactory::GemIssued::decode_log(log).ok())
        .map(|log| log.data)
        .collect();
    assert_eq!(issued.len(), 2);
    for (event, (item, position_id, bucket)) in issued.iter().zip(&expected) {
        assert_eq!(event.gemId, item.gem_id);
        assert_eq!(event.callPriceMinor, item.call_price_minor);
        assert_eq!(event.callWindow, item.call_window_seconds);
        assert_eq!(event.callThreshold, item.call_threshold_seconds);
        assert_eq!(event.callNoticePeriod, item.call_notice_period_seconds);
        assert_eq!(event.positionId, *position_id);
        assert_eq!(event.bucketKey, *bucket);
        assert!(!bucket.is_zero());
    }

    let opened: Vec<_> = events
        .iter()
        .filter_map(|log| crate::precompile::IGemFactory::GemPositionIssued::decode_log(log).ok())
        .map(|log| log.data)
        .collect();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].positionId, position);
    assert_eq!(opened[0].merchant, ALICE);
    assert_eq!(opened[0].capacityMinor, sent_capacity(six_decimal_u128()));
    assert_eq!(opened[0].expiresAt, T_NOW + POSITION_VALIDITY_SECONDS);
}
