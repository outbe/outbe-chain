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
fn an_issued_series_is_sent_once_without_a_promis_limit_credit() {
    with_storage(Some(six_decimal_unit()), |storage| {
        seed_source_series(
            storage,
            six_decimal_unit(),
            six_decimal_u128(),
            outbe_intex::IntexCallTrigger::default(),
        );
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
        assert!(
            !outbe_intex::api::series_exists(storage, source_intex_id()).unwrap(),
            "every unit went into the position, so the source series is spent"
        );
        assert_eq!(unallocated(storage), promis_limit);
    });
}

#[test]
fn issue_gem_position_burns_sends_and_issues_nft() {
    with_storage(None, |storage| {
        let id = seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        let capacity = sent_capacity(six_decimal_u128());

        let factory = GemFactoryContract::new(storage.clone());
        let rec = factory.positions.get(id).unwrap().unwrap();
        assert_eq!(rec.merchant, ALICE);
        assert_eq!(rec.source_intex_id, source_intex_id());
        assert_eq!(rec.remaining_capacity_minor, capacity);
        assert_eq!(factory.total_capacity_minor.read().unwrap(), capacity);

        // Position NFT issued to the merchant.
        assert_eq!(factory.owner_of(id).unwrap(), ALICE);
        assert_eq!(factory.balance_of(ALICE).unwrap(), 1);
        assert_eq!(factory.token_of_owner_by_index(ALICE, 0).unwrap(), id);
    });
}

#[test]
fn sending_every_unit_deletes_the_spent_source_series() {
    with_storage(None, |storage| {
        seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        // Their load lives in the position now, so nothing is left in the source series.
        assert!(!outbe_intex::api::series_exists(storage, source_intex_id()).unwrap());
    });
}

/// This function returns the closing timestamp of the deadline day that contains `at`.
fn day_closed(at: u64) -> u64 {
    outbe_primitives::deadline_queue::day_end(
        outbe_primitives::deadline_queue::deadline_day(at).unwrap(),
    )
}

/// An expired position reads empty at once, returns its remainder with its hour and is
/// then deleted.
#[test]
fn an_expired_position_returns_its_remainder() {
    with_storage(None, |storage| {
        let id = seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        let capacity = sent_capacity(six_decimal_u128());
        let expires = T_NOW + POSITION_VALIDITY_SECONDS;

        let ctx = block_ctx(storage, expires);
        assert_eq!(positions_expired::sweep_expired_positions(&ctx).unwrap(), 0);
        assert_eq!(unallocated(storage), U256::ZERO);
        storage.set_block_timestamp(U256::from(expires)).unwrap();
        assert_eq!(
            runtime::position_data(storage, id)
                .unwrap()
                .remainingCapacityMinor,
            U256::ZERO
        );

        let ctx = block_ctx(storage, day_closed(expires));
        assert_eq!(positions_expired::sweep_expired_positions(&ctx).unwrap(), 1);
        assert_eq!(unallocated(storage), capacity);
        assert_eq!(positions_expired::cleanup_positions(&ctx).unwrap(), 1);

        let factory = GemFactoryContract::new(storage.clone());
        assert!(factory.positions.get(id).unwrap().is_none());
        assert_eq!(factory.balance_of(ALICE).unwrap(), 0);
        assert!(runtime::position_data(storage, id).is_err());
    });
}

/// A storage fault is this node's own: it fails the block instead of skipping the hour.
#[test]
fn a_storage_fault_in_the_position_sweep_fails_the_block() {
    let mut provider = test_storage(None);
    let id = StorageHandle::enter(&mut provider, |storage| {
        seed_and_send(&storage, six_decimal_unit(), six_decimal_u128())
    });

    provider.fail_mutation_at_address(outbe_primitives::addresses::PROMIS_LIMIT_ADDRESS);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = block_ctx(&storage, day_closed(T_NOW + POSITION_VALIDITY_SECONDS));
        assert!(matches!(
            positions_expired::sweep_expired_positions(&ctx).unwrap_err(),
            outbe_primitives::error::PrecompileError::Storage(_)
        ));
        let factory = GemFactoryContract::new(storage.clone());
        let record = factory.positions.get(id).unwrap().unwrap();
        assert_ne!(
            factory
                .hour_capacity_minor
                .read(&record.expiry_hour())
                .unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn a_drained_position_returns_nothing_and_is_deleted() {
    with_storage(Some(six_decimal_unit()), |storage| {
        seed_day_vwap(storage, 840, six_decimal_unit());
        let id = seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        runtime::issue_merchant_gem(
            storage,
            crate::schema::MerchantGemIssueParams {
                caller: ALICE,
                position_id: id,
                owner: BOB,
                promis_load_minor: sent_capacity(six_decimal_u128()),
            },
        )
        .unwrap();

        let ctx = block_ctx(storage, day_closed(T_NOW + POSITION_VALIDITY_SECONDS));
        assert_eq!(positions_expired::sweep_expired_positions(&ctx).unwrap(), 1);
        assert_eq!(unallocated(storage), U256::ZERO);
        assert_eq!(positions_expired::cleanup_positions(&ctx).unwrap(), 1);
        assert!(GemFactoryContract::new(storage.clone())
            .positions
            .get(id)
            .unwrap()
            .is_none());
    });
}

#[test]
fn issue_gem_position_unknown_source_rejects() {
    with_storage(None, |storage| {
        let r =
            runtime::issue_gem_position(storage, ALICE, source_intex_id(), U256::from(SENT_UNITS));
        assert_eq!(
            r.unwrap_err().to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GemFactoryError::SourceIntexNotFound
            )
            .to_string()
        );
    });
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
                position_id: id,
                owner: BOB,
                promis_load_minor: load,
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
                position_id: id,
                owner: BOB,
                promis_load_minor: six_decimal_unit(),
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
                position_id: id,
                owner: BOB,
                promis_load_minor: U256::ONE,
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
                position_id: id,
                owner: BOB,
                promis_load_minor: six_decimal_unit(),
            },
        );
        assert_eq!(
            r.unwrap_err().to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GemFactoryError::NotPositionOwner
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
                position_id: id,
                owner: BOB,
                promis_load_minor: over,
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
                position_id: id,
                owner: BOB,
                promis_load_minor: six_decimal_unit(),
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
                position_id: id,
                owner: BOB,
                promis_load_minor: six_decimal_unit(),
            },
        )
        .unwrap();
        assert_eq!(gem_view(storage, gem_id).item.entry_price_minor, rate);
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
                issuance_currency: 840,
                reference_currency: 840,
                issued_at: T_NOW - POSITION_VALIDITY_SECONDS - 1,
                expires_at: T_NOW - 1,
            })
            .unwrap();

        let r = runtime::issue_merchant_gem(
            storage,
            crate::schema::MerchantGemIssueParams {
                caller: ALICE,
                position_id,
                owner: BOB,
                promis_load_minor: six_decimal_unit(),
            },
        );
        assert_eq!(
            r.unwrap_err().to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GemFactoryError::PositionExpired
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
        let expires_at = runtime::position_data(&storage, id).unwrap().expiresAt;
        let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), 840)
            .unwrap()
            .unwrap();
        OracleContract::new(storage.clone())
            .record_utc_day_vwap(
                previous_date_key(timestamp_to_date_key(expires_at - 1)).expect("date step"),
                index,
                rate,
            )
            .unwrap();
        (id, expires_at)
    });
    let mut issue = |now: u64, load: U256| {
        provider.set_timestamp(U256::from(now));
        StorageHandle::enter(&mut provider, |storage| {
            let issued = runtime::issue_merchant_gem(
                &storage,
                crate::schema::MerchantGemIssueParams {
                    caller: ALICE,
                    position_id: id,
                    owner: BOB,
                    promis_load_minor: load,
                },
            );
            let remaining = runtime::position_data(&storage, id)
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
            crate::errors::GemFactoryError::PositionExpired
        )
        .to_string()
    );
    assert_eq!(after, U256::ZERO, "an expired position reads empty");
}

#[test]
fn issue_merchant_gem_prices_at_the_previous_day_vwap() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let vwap = U256::from(3u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        seed_day_vwap(storage, 840, vwap);
        let id = seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        let gem_id = runtime::issue_merchant_gem(
            storage,
            crate::schema::MerchantGemIssueParams {
                caller: ALICE,
                position_id: id,
                owner: BOB,
                promis_load_minor: six_decimal_unit(),
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
                    position_id: id,
                    owner: BOB,
                    promis_load_minor: six_decimal_unit(),
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
fn issue_merchant_gem_rejects_a_missing_previous_day_vwap() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let id = seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        let r = runtime::issue_merchant_gem(
            storage,
            crate::schema::MerchantGemIssueParams {
                caller: ALICE,
                position_id: id,
                owner: BOB,
                promis_load_minor: six_decimal_unit(),
            },
        );
        assert_eq!(
            r.unwrap_err().to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GemFactoryError::OracleUnavailable
            )
            .to_string()
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
                position_id: position,
                owner: BOB,
                promis_load_minor: six_decimal_unit(),
            },
        )
        .unwrap();
        let wallet_gem = issue_at_live_rate(&storage, ALICE, six_decimal_unit(), 840, 840).unwrap();
        let expected = [(merchant_gem, position), (wallet_gem, U256::ZERO)].map(|(gem_id, pos)| {
            let item = gem_view(&storage, gem_id);
            (item, pos, gem_item(&storage, gem_id).group_key)
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
    for (event, (item, position_id, group)) in issued.iter().zip(&expected) {
        assert_eq!(event.gemId, item.item.gem_id.to_u256());
        assert_eq!(event.callPriceMinor, item.terms.call_price_minor);
        assert_eq!(event.callWindow, item.terms.call_window_seconds);
        assert_eq!(event.callThreshold, item.terms.call_threshold_seconds);
        assert_eq!(
            event.callNoticePeriod,
            item.terms.call_notice_period_seconds
        );
        assert_eq!(event.positionId, *position_id);
        assert_eq!(event.groupKey, *group);
        assert!(!group.is_zero());
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
