//! Genesis-day first cycle and daily Nod calls.

use super::*;

#[test]
fn genesis_midday_first_cycle_at_next_midnight_settles_genesis_day() {
    // Genesis at 10:00 UTC on day D. First CycleTick fires at 00:00:01 UTC
    // on day D+1. prev_day = D = genesis_utc_day -> day_number = 0 -> Ok.
    // This is the production scenario that was broken when genesis_utc_day
    // was derived from block 0 timestamp at 10:00 instead of genesisTime.
    const DAY_D_MIDNIGHT: u64 = GENESIS_TS; // 2024-01-01 00:00:00
    const DAY_D_10AM: u64 = DAY_D_MIDNIGHT + 10 * 3600; // 10:00 UTC

    let mut storage = cycle_storage();
    storage.enter(|handle| {
        // Block 1 at 10:00 - genesis anchor records genesis_utc_day = day D.
        let ctx_anchor = genesis_block(handle.clone(), DAY_D_10AM);
        run_cycle_lifecycle(&ctx_anchor).unwrap();
        let canonical_wwd = outbe_primitives::time::WorldwideDay::new(20_240_102);
        assert_eq!(
            outbe_metadosis::api::worldwide_days(ctx_anchor.storage.clone())
                .unwrap()
                .into_iter()
                .map(|day| day.worldwide_day)
                .collect::<Vec<_>>(),
            vec![canonical_wwd],
            "block 1 at the UTC+14 boundary must create only the canonical WWD"
        );

        // Block at 00:00:01 UTC day D+1 - CycleTick fires.
        // prev_day = D = genesis_utc_day -> day_number_since_genesis = 0.
        let fire_ts = DAY_D_MIDNIGHT + SECONDS_PER_DAY + 1;
        let ctx_fire = BlockRuntimeContext::new(block_ctx(2, fire_ts), handle);
        account_parent(&ctx_fire, 2);
        run_cycle_lifecycle(&ctx_fire).unwrap();

        assert_eq!(
            outbe_metadosis::api::worldwide_days(ctx_fire.storage.clone())
                .unwrap()
                .into_iter()
                .map(|day| day.worldwide_day)
                .collect::<Vec<_>>(),
            vec![
                outbe_primitives::time::WorldwideDay::new(20_240_101),
                canonical_wwd,
            ],
            "the first UTC midnight must retain the canonical genesis WWD and add only the explicitly settled previous UTC day"
        );

        let rewards = ctx_fire
            .storage
            .contract::<outbe_rewards::schema::Rewards<'_>>();
        let genesis_day = rewards.genesis_utc_day.read().unwrap();
        assert_eq!(genesis_day, 20_240_101, "genesis_utc_day = day D");
        assert!(
            rewards.daily_settled.read(&genesis_day).unwrap(),
            "CycleTick must settle genesis day (day_number=0)"
        );
    });
}

#[test]
fn nod_daily_calls_and_does_not_repeat_between_utc_days() {
    use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

    let midnight = GENESIS_TS + 40 * SECONDS_PER_DAY;
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(block_ctx(2, midnight - 1), storage.clone());
        with_execution_scope(&ctx, |scope, _| {
            let parent = outbe_nod::nod_reader(Arc::new(MemoryStorage::new()));
            let cycle = Cycle::new(storage.clone());
            let trigger = isolate_nod_call_schedule(&cycle, midnight)?;
            let (oracle, index) = seed_closed_day_prices(&storage, midnight)?;
            let called_at = |bucket_key| {
                outbe_nod::NodContract::new(storage.clone())
                    .bucket_called_at
                    .read(&bucket_key)
                    .unwrap()
            };
            let first = issue_nod(&storage, scope, &parent, Address::repeat_byte(0x51), 13)?;
            dispatch_and_call(&ctx, scope, &parent)?;
            assert_eq!(called_at(first), 0);

            let ctx = BlockRuntimeContext::new(block_ctx(3, midnight), storage.clone());
            dispatch_and_call(&ctx, scope, &parent)?;
            assert_eq!(called_at(first), midnight);
            assert_eq!(cycle.last_executed_at.read(&trigger)?, midnight);

            // New work created after the daily run waits for the next UTC slot.
            let second = issue_nod(&storage, scope, &parent, Address::repeat_byte(0x52), 14)?;
            let ctx = BlockRuntimeContext::new(block_ctx(4, midnight + 1), storage.clone());
            dispatch_and_call(&ctx, scope, &parent)?;
            assert_eq!(called_at(second), 0);

            // A multi-day halt runs once against the latest completed day,
            // rather than replaying the same latest price on subsequent blocks.
            let late = midnight + 3 * SECONDS_PER_DAY;
            let previous = previous_date_key(timestamp_to_date_key(late));
            // The sealed test terms call on 2 breach days out of a 3-day window.
            // An unpriced day counts as zero. So every day closed during
            // the halt must carry its finalized price for the call to be due.
            let mut day = previous;
            while day >= timestamp_to_date_key(midnight) {
                oracle.record_utc_day_vwap(day, index, U256::from(100))?;
                day = previous_date_key(day);
            }
            oracle.utc_day_vwap_last_finalized.write(previous)?;
            let ctx = BlockRuntimeContext::new(block_ctx(5, late), storage.clone());
            dispatch_and_call(&ctx, scope, &parent)?;
            assert_eq!(called_at(second), late);
            assert_eq!(cycle.last_executed_at.read(&trigger)?, late);
            let third = issue_nod(&storage, scope, &parent, Address::repeat_byte(0x53), 15)?;
            let ctx = BlockRuntimeContext::new(block_ctx(6, late + 1), storage.clone());
            dispatch_and_call(&ctx, scope, &parent)?;
            assert_eq!(called_at(third), 0);
            Ok(())
        })
        .unwrap();
    });
}

/// Isolates Nod's schedule. Unrelated triggers are not due, and the Nod daily
/// call is due at `midnight`. Returns the Nod call trigger id.
fn isolate_nod_call_schedule(
    cycle: &Cycle<'_>,
    midnight: u64,
) -> outbe_primitives::error::Result<u32> {
    for spec in ACTIVE_TRIGGERS {
        cycle
            .last_executed_at
            .write(&spec.id, midnight + 10 * SECONDS_PER_DAY)?;
    }
    let trigger = TriggerId::NodCallDaily.as_u32();
    cycle.last_executed_at.write(&trigger, midnight - 1)?;
    Ok(trigger)
}

/// Registers COEN/840 as the only reference currency and finalizes a VWAP of
/// 100 for each of the 28 closed days before `midnight`. Returns the Oracle
/// and the pair index.
fn seed_closed_day_prices<'s>(
    storage: &StorageHandle<'s>,
    midnight: u64,
) -> outbe_primitives::error::Result<(outbe_oracle::schema::OracleContract<'s>, u32)> {
    use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    let index = outbe_oracle::api::register_pair(
        storage.clone(),
        outbe_oracle::api::AddressPair::new_coen_to(840),
    )?;
    oracle.reference_currencies.push(840)?;
    let mut day = previous_date_key(timestamp_to_date_key(midnight));
    oracle.utc_day_vwap_last_finalized.write(day)?;
    for _ in 0..28 {
        oracle.record_utc_day_vwap(day, index, U256::from(100))?;
        day = previous_date_key(day);
    }
    Ok((oracle, index))
}

/// Issues a Nod of `owner` on the genesis WorldwideDay at entry price `entry`.
/// Returns its bucket key.
fn issue_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl outbe_compressed_entities::ParentBodySource,
    owner: Address,
    entry: u64,
) -> outbe_primitives::error::Result<B256> {
    let worldwide_day = outbe_primitives::time::WorldwideDay::from_timestamp(GENESIS_TS);
    let body = outbe_nod::test_support::item(
        outbe_nod::test_support::NodItemFixture {
            is_settled: false,
            nod_id: outbe_nod::identity::generate_nod_id(owner, worldwide_day)?,
            owner,
            gratis_load_minor: U256::from(11),
            worldwide_day,
            league_id: 4,
            bucket_key: outbe_nod::identity::bucket_key(worldwide_day, U256::from(entry), 840),
            issuance_currency: 840,
            reference_currency: 840,
            issued_at: GENESIS_TS,
        },
        U256::from(entry),
    );
    outbe_nod::api::add_nod(storage, scope, parent, &body, U256::from(entry))?;
    Ok(body.bucket_key)
}

/// Runs the Cycle dispatcher at `ctx`, then the Nod call slice.
fn dispatch_and_call(
    ctx: &BlockRuntimeContext<'_>,
    scope: &ExecutionScope,
    parent: &impl outbe_compressed_entities::ParentBodySource,
) -> outbe_primitives::error::Result<()> {
    crate::runtime::dispatch_triggers(ctx, scope, parent)?;
    outbe_nod::called::run_call_slice(ctx)?;
    Ok(())
}
