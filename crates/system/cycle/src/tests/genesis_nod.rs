//! Genesis-day first cycle and daily Nod calls.

use super::*;

#[test]
fn genesis_midday_first_cycle_at_next_midnight_settles_genesis_day() {
    // Genesis at 10:00 UTC on day D. First CycleTick fires at 00:00:01 UTC
    // on day D+1. prev_day = D = genesis_utc_day -> day_number = 0 -> Ok.
    // This scenario failed in production when the node derived genesis_utc_day
    // from the block 0 timestamp at 10:00 instead of genesisTime.
    const DAY_D_MIDNIGHT: u64 = GENESIS_TS; // 2024-01-01 00:00:00
    const DAY_D_10AM: u64 = DAY_D_MIDNIGHT + 10 * 3600; // 10:00 UTC

    let mut storage = cycle_storage();
    storage.enter(|handle| {
        // Block 1 at 10:00 - genesis anchor records genesis_utc_day = day D.
        let ctx_anchor = genesis_block(handle.clone(), DAY_D_10AM);
        run_cycle_lifecycle(&ctx_anchor).unwrap();
        let canonical_wwd = outbe_primitives::time::WorldwideDay::new(20_240_102);
        assert!(
            outbe_metadosis::api::worldwide_day(ctx_anchor.storage.clone(), canonical_wwd)
                .unwrap()
                .is_some(),
            "block 1 at the UTC+14 boundary must create the canonical WWD"
        );
    });
    storage.set_block_number(2);
    storage.enter(|handle| {
        let canonical_wwd = outbe_primitives::time::WorldwideDay::new(20_240_102);
        // Block at 00:00:01 UTC day D+1 - CycleTick fires.
        // prev_day = D = genesis_utc_day -> day_number_since_genesis = 0.
        let fire_ts = DAY_D_MIDNIGHT + SECONDS_PER_DAY + 1;
        let ctx_fire = runtime_context(block_ctx(2, fire_ts), handle);
        account_parent(&ctx_fire, 2);
        run_cycle_lifecycle(&ctx_fire).unwrap();

        assert!(
            outbe_metadosis::api::worldwide_day(
                ctx_fire.storage.clone(),
                outbe_primitives::time::WorldwideDay::new(20_240_101),
            )
            .unwrap()
            .is_some(),
            "the first UTC midnight must create the explicitly settled previous UTC day"
        );
        assert!(
            outbe_metadosis::api::worldwide_day(ctx_fire.storage.clone(), canonical_wwd)
                .unwrap()
                .is_some(),
            "the first UTC midnight must retain the canonical genesis WWD"
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
        (|| {
            let cycle = Cycle::new(storage.clone());
            let trigger = isolate_nod_call_schedule(&cycle, midnight)?;
            let (oracle, index) = seed_closed_day_prices(&storage, midnight)?;
            let called_at = |group_key| {
                outbe_nod::NodContract::new(storage.clone())
                    .group_called_at
                    .read(&group_key)
                    .unwrap()
            };
            let first = issue_nod(&storage, Address::repeat_byte(0x51), 13)?;
            dispatch_and_call(&ctx)?;
            assert_eq!(called_at(first), 0);

            let ctx = BlockRuntimeContext::new(block_ctx(3, midnight), storage.clone());
            dispatch_and_call(&ctx)?;
            assert_eq!(called_at(first), midnight);
            assert_eq!(cycle.last_executed_at.read(&trigger)?, midnight);

            // New work created after the daily run waits for the next UTC slot.
            let second = issue_nod(&storage, Address::repeat_byte(0x52), 14)?;
            let ctx = BlockRuntimeContext::new(block_ctx(4, midnight + 1), storage.clone());
            dispatch_and_call(&ctx)?;
            assert_eq!(called_at(second), 0);

            // A multi-day halt walks every day it missed, one per block, and then stops
            // rather than replaying the latest price on subsequent blocks.
            let late = midnight + 3 * SECONDS_PER_DAY;
            let previous = previous_date_key(timestamp_to_date_key(late)).expect("date step");
            // The sealed test terms call on 2 breach days out of a 3-day window.
            // An unpriced day counts as zero. So every day closed during
            // the halt must carry its finalized price for the call to be due.
            let mut day = previous;
            while day >= timestamp_to_date_key(midnight) {
                oracle.record_utc_day_vwap(day, index, U256::from(100))?;
                day = previous_date_key(day).expect("date step");
            }
            oracle.utc_day_vwap_last_finalized.write(previous)?;
            let ctx = BlockRuntimeContext::new(block_ctx(5, late), storage.clone());
            dispatch_and_call(&ctx)?;
            let nod = outbe_nod::NodContract::new(storage.clone());
            for height in 6..14 {
                if nod.call_sweep_day.read()? == 0 {
                    break;
                }
                outbe_nodfactory::called::run_call_slice(&BlockRuntimeContext::new(
                    block_ctx(height, late),
                    storage.clone(),
                ))?;
            }
            assert_eq!(nod.call_sweep_day.read()?, 0);
            assert_eq!(called_at(second), late);
            assert_eq!(cycle.last_executed_at.read(&trigger)?, late);
            let third = issue_nod(&storage, Address::repeat_byte(0x53), 15)?;
            let ctx = BlockRuntimeContext::new(block_ctx(14, late + 1), storage.clone());
            dispatch_and_call(&ctx)?;
            assert_eq!(called_at(third), 0);
            Ok::<_, outbe_primitives::error::PrecompileError>(())
        })()
        .unwrap();
    });
}

/// Runs a Nod call across a one-day halt: day D closes while no block is made, and the
/// restart schedules only D+1. Returns the group's call and the sweep's newest day.
fn call_across_a_halt(price_on_d: bool) -> (u64, u32, u64) {
    use outbe_primitives::time::{next_date_key, previous_date_key, timestamp_to_date_key};

    let midnight = GENESIS_TS + 40 * SECONDS_PER_DAY;
    let d = timestamp_to_date_key(midnight);
    let restart = midnight + 2 * SECONDS_PER_DAY;
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        (|| {
            let cycle = Cycle::new(storage.clone());
            isolate_nod_call_schedule(&cycle, midnight)?;
            let (oracle, index) = seed_closed_day_prices(&storage, midnight)?;
            // The sealed DEV terms call on 2 breach days of a 3-day window; 1 never breaches.
            let back = |n: u32| (0..n).fold(d, |day, _| previous_date_key(day).expect("date step"));
            for day in [back(4), back(3), back(1)] {
                oracle.record_utc_day_vwap(day, index, U256::from(1))?;
            }
            let group = issue_nod(&storage, Address::repeat_byte(0x51), 13)?;
            let nod = outbe_nod::NodContract::new(storage.clone());

            // Day D: the window up to D-1 holds one breach day.
            dispatch_and_call(&BlockRuntimeContext::new(
                block_ctx(3, midnight),
                storage.clone(),
            ))?;
            assert_eq!(nod.group_called_at.read(&group)?, 0);
            assert_eq!(nod.call_latest_day.read()?, back(1));

            // No block while D closes. D+1 closes unpriced before the restart.
            if price_on_d {
                oracle.record_utc_day_vwap(d, index, U256::from(100))?;
            }
            oracle
                .utc_day_vwap_last_finalized
                .write(next_date_key(d).expect("date step"))?;
            let ctx = BlockRuntimeContext::new(block_ctx(4, restart), storage.clone());
            dispatch_and_call(&ctx)?;
            for height in 5..13 {
                if nod.call_sweep_day.read()? == 0 {
                    break;
                }
                outbe_nodfactory::called::run_call_slice(&BlockRuntimeContext::new(
                    block_ctx(height, restart),
                    storage.clone(),
                ))?;
            }
            assert_eq!(nod.call_sweep_day.read()?, 0);
            Ok::<_, outbe_primitives::error::PrecompileError>((
                nod.group_called_at.read(&group)?,
                nod.call_latest_day.read()?,
                restart,
            ))
        })()
        .unwrap()
    })
}

/// D's window [D-2, D-1, D] holds two breach days and D+1's [D-1, D, D+1] only one, so
/// only a sweep that walks the day the halt skipped calls the group.
#[test]
fn a_halt_does_not_skip_the_day_that_calls_a_nod_group() {
    use outbe_primitives::time::{next_date_key, timestamp_to_date_key};

    let d = timestamp_to_date_key(GENESIS_TS + 40 * SECONDS_PER_DAY);
    let (called_at, latest, restart) = call_across_a_halt(true);
    assert_eq!(called_at, restart);
    assert_eq!(latest, next_date_key(d).expect("date step"));

    let (called_at, latest, _) = call_across_a_halt(false);
    assert_eq!(called_at, 0, "without D's price no window breaches twice");
    assert_eq!(latest, next_date_key(d).expect("date step"));
}

#[test]
fn cycle_retries_oracle_lag_and_expires_only_the_closed_deadline_day() {
    use outbe_primitives::time::timestamp_to_date_key;

    let day_start = GENESIS_TS + 40 * SECONDS_PER_DAY;
    let deadline = day_start + 10 * 3_600;
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let cycle = Cycle::new(storage.clone());
        // No unrelated trigger is due. This test exercises the per-block lifecycle continuation.
        // The test includes recovery after a daily scheduling attempt finds no finalized day.
        for spec in ACTIVE_TRIGGERS {
            cycle
                .last_executed_at
                .write(&spec.id, day_start + 10 * SECONDS_PER_DAY)
                .unwrap();
        }
        let (oracle, _) = seed_closed_day_prices(&storage, day_start).unwrap();
        let init = BlockRuntimeContext::new(block_ctx(2, deadline), storage.clone());
        outbe_nodfactory::called::initialize_daily(&init).unwrap();
        let mut nod = outbe_nod::NodContract::new(storage.clone());
        for (owner, entry, due) in [(0x61, 13, deadline), (0x62, 14, deadline + SECONDS_PER_DAY)] {
            let group = issue_nod(&storage, Address::repeat_byte(owner), entry).unwrap();
            let notice = u64::from(nod.group_call_notice_period_seconds.read(&group).unwrap());
            nod.record_group_called(group, due - notice, due).unwrap();
        }
        let reserve = outbe_promislimit::PromisLimitContract::new(storage.clone());
        let before = reserve.promis_limit().unwrap();

        run_cycle_lifecycle(&BlockRuntimeContext::new(
            block_ctx(3, day_start + 11 * 3_600),
            storage.clone(),
        ))
        .unwrap();
        assert_eq!(
            reserve.promis_limit().unwrap(),
            before,
            "no intraday forfeit"
        );

        let closed_at = day_start + SECONDS_PER_DAY;
        run_cycle_lifecycle(&BlockRuntimeContext::new(
            block_ctx(4, closed_at),
            storage.clone(),
        ))
        .unwrap();
        assert_eq!(
            reserve.promis_limit().unwrap(),
            before,
            "Oracle is not ready"
        );

        oracle
            .utc_day_vwap_last_finalized
            .write(timestamp_to_date_key(day_start))
            .unwrap();
        let ready = BlockRuntimeContext::new(block_ctx(5, closed_at + 1), storage.clone());
        run_cycle_lifecycle(&ready).unwrap();
        assert_eq!(reserve.promis_limit().unwrap(), before + U256::from(11));
        assert_eq!(nod.call_sweep_day.read().unwrap(), 0);
        run_cycle_lifecycle(&ready).unwrap();
        assert_eq!(reserve.promis_limit().unwrap(), before + U256::from(11));

        // The queue retains the next day's deadline even though calls have no work.
        oracle
            .utc_day_vwap_last_finalized
            .write(timestamp_to_date_key(closed_at))
            .unwrap();
        run_cycle_lifecycle(&BlockRuntimeContext::new(
            block_ctx(6, closed_at + SECONDS_PER_DAY),
            storage.clone(),
        ))
        .unwrap();
        assert_eq!(reserve.promis_limit().unwrap(), before + U256::from(22));
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

    initialize_daily_passes(&BlockRuntimeContext::new(
        block_ctx(1, midnight - SECONDS_PER_DAY),
        storage.clone(),
    ));
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    let index = outbe_oracle::api::register_pair(
        storage.clone(),
        outbe_oracle::api::AddressPair::new_coen_to(840),
    )?;
    let mut day = previous_date_key(timestamp_to_date_key(midnight)).expect("date step");
    oracle.utc_day_vwap_last_finalized.write(day)?;
    for _ in 0..28 {
        oracle.record_utc_day_vwap(day, index, U256::from(100))?;
        day = previous_date_key(day).expect("date step");
    }
    Ok((oracle, index))
}

/// Issues a Nod of `owner` on the genesis WorldwideDay at entry price `entry`.
/// Returns its group key.
fn issue_nod(
    storage: &StorageHandle<'_>,
    owner: Address,
    entry: u64,
) -> outbe_primitives::error::Result<B256> {
    let worldwide_day = outbe_primitives::time::WorldwideDay::from_timestamp(GENESIS_TS);
    let mut body = outbe_nod::test_support::item(
        outbe_nod::test_support::NodItemFixture {
            is_settled: false,
            nod_id: outbe_nod::identity::generate_nod_id(owner, worldwide_day)?,
            owner,
            gratis_load_minor: U256::from(11),
            worldwide_day,
            league_id: 4,
            group_key: B256::ZERO,
            issuance_currency: 840,
            reference_currency: 840,
            issued_at: GENESIS_TS,
        },
        U256::from(entry),
    );
    let terms = outbe_nod::test_support::sealed_terms(storage, &body, U256::from(entry));
    body.group_key = outbe_nod::identity::group_key(
        GENESIS_TS,
        terms.map_or(U256::ZERO, |terms| terms.call_price_minor),
        840,
    )
    .expect("date step");
    outbe_nod::api::add_nod(
        storage,
        &body,
        terms,
        outbe_nod::api::calculation_amount(&body)?,
    )?;
    Ok(body.group_key)
}

/// Runs the Cycle dispatcher at `ctx`, then the Nod call slice.
fn dispatch_and_call(ctx: &BlockRuntimeContext<'_>) -> outbe_primitives::error::Result<()> {
    crate::runtime::dispatch_triggers(ctx)?;
    outbe_nodfactory::called::run_call_slice(ctx)?;
    Ok(())
}
