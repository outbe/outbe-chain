use alloy_primitives::{address, Address, U256};
use alloy_sol_types::SolCall;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::math::constants::REAL_ID_SHIFT;
use outbe_primitives::math::tree_math;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

use crate::api;
use crate::precompile::{dispatch, IGem};
use crate::schema::{GemAddParams, GemContract, GemState};

const T_NOW: u64 = 1_700_000_000;
const ALICE: Address = address!("0x1111111111111111111111111111111111111111");
const BOB: Address = address!("0x2222222222222222222222222222222222222222");

fn with_storage<R>(f: impl FnOnce(&StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |handle| f(&handle))
}

fn sample_params(owner: Address) -> GemAddParams {
    GemAddParams {
        owner,
        gem_type: 2, // WALLET
        gem_load_minor: U256::from(1_000_000u64),
        entry_price_minor: U256::from(500_000u64),
        cost_amount_minor: U256::from(500_000u64),
        floor_price_minor: U256::from(540_000u64),
        call_price_minor: U256::from(1_140_000u64),
        call_rate: 228,
        call_window: 28 * 86_400,
        call_threshold: 21 * 86_400,
        issuance_currency: 840,
        reference_currency: 840,
        initial_state: GemState::Issued,
        issued_at: T_NOW,
    }
}

#[test]
fn coen_iso_one_maps_to_the_center_price_bin_at_six_decimals() {
    assert_eq!(
        GemContract::price_to_bin(U256::from(1_000_000u64)).unwrap(),
        REAL_ID_SHIFT as u32
    );
}

#[test]
fn initial_state_empty() {
    with_storage(|storage| {
        let gem = GemContract::new(storage.clone());
        assert_eq!(gem.total_supply().unwrap(), 0);
        assert_eq!(gem.balance_of(ALICE).unwrap(), 0);
    });
}

#[test]
fn add_gem_inserts_and_bumps_counters() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let gem = GemContract::new(storage.clone());
        assert_eq!(gem.total_supply().unwrap(), 1);
        assert_eq!(gem.balance_of(ALICE).unwrap(), 1);
        assert_eq!(gem.owner_of(gem_id).unwrap(), ALICE);
        assert_eq!(gem.token_of_owner_by_index(ALICE, 0).unwrap(), gem_id);
        let stored = api::get_gem(storage, gem_id).unwrap().unwrap();
        assert_eq!(stored.state, GemState::Issued as u8);
    });
}

#[test]
fn add_gem_rejects_zero_owner() {
    with_storage(|storage| {
        let mut p = sample_params(ALICE);
        p.owner = Address::ZERO;
        assert!(api::add_gem(storage, p).is_err());
    });
}

#[test]
fn enumerable_returns_only_owned_gems() {
    with_storage(|storage| {
        let g1 = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let mut p2 = sample_params(ALICE);
        p2.gem_load_minor = U256::from(2u64);
        let g2 = api::add_gem(storage, p2).unwrap();
        let p3 = sample_params(BOB);
        let _g3 = api::add_gem(storage, p3).unwrap();

        let gem = GemContract::new(storage.clone());
        let alice_count = gem.balance_of(ALICE).unwrap();
        let alice_gems: Vec<U256> = (0..alice_count)
            .map(|i| gem.token_of_owner_by_index(ALICE, i).unwrap())
            .collect();
        assert_eq!(alice_gems.len(), 2);
        assert!(alice_gems.contains(&g1));
        assert!(alice_gems.contains(&g2));
        assert_eq!(gem.balance_of(ALICE).unwrap(), 2);
        assert_eq!(gem.balance_of(BOB).unwrap(), 1);
        assert_eq!(gem.total_supply().unwrap(), 3);
    });
}

#[test]
fn burn_requires_settled_state() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        assert!(api::burn(storage, gem_id).is_err());

        api::set_state(storage, gem_id, GemState::Qualified).unwrap();
        assert!(api::burn(storage, gem_id).is_err());

        api::set_state(storage, gem_id, GemState::Settled).unwrap();
        api::burn(storage, gem_id).unwrap();

        let gem = GemContract::new(storage.clone());
        assert_eq!(gem.total_supply().unwrap(), 0);
        assert_eq!(gem.balance_of(ALICE).unwrap(), 0);
        assert!(gem.get_gem(gem_id).unwrap().is_none());
    });
}

#[test]
fn burn_compacts_owner_index() {
    with_storage(|storage| {
        let g1 = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let mut p2 = sample_params(ALICE);
        p2.gem_load_minor = U256::from(2u64);
        let g2 = api::add_gem(storage, p2).unwrap();
        let mut p3 = sample_params(ALICE);
        p3.gem_load_minor = U256::from(3u64);
        let g3 = api::add_gem(storage, p3).unwrap();

        api::set_state(storage, g1, GemState::Settled).unwrap();
        api::burn(storage, g1).unwrap();

        let gem = GemContract::new(storage.clone());
        let count = gem.balance_of(ALICE).unwrap();
        let remaining: Vec<U256> = (0..count)
            .map(|i| gem.token_of_owner_by_index(ALICE, i).unwrap())
            .collect();
        assert_eq!(remaining.len(), 2);
        assert!(remaining.contains(&g2));
        assert!(remaining.contains(&g3));
        assert_eq!(gem.balance_of(ALICE).unwrap(), 2);
    });
}

#[test]
fn qualify_respects_state_and_floor() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let mut gem = GemContract::new(storage.clone());
        let floor = U256::from(540_000u64);

        // Rate equals floor (strict `>`) — must NOT qualify.
        assert!(!gem.qualify(gem_id, T_NOW, 840, floor).unwrap());

        // Rate below floor.
        assert!(!gem
            .qualify(gem_id, T_NOW, 840, floor - U256::from(1u64))
            .unwrap());

        // Rate strictly above floor — qualifies.
        assert!(gem
            .qualify(gem_id, T_NOW, 840, floor + U256::from(1u64))
            .unwrap());
        let after = gem.get_gem(gem_id).unwrap().unwrap();
        assert_eq!(after.state, GemState::Qualified as u8);

        // Second qualify is a no-op (already qualified).
        assert!(!gem
            .qualify(gem_id, T_NOW, 840, floor + U256::from(1u64))
            .unwrap());
    });
}

#[test]
fn add_gem_parks_issued_in_bin_tree() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let gem = GemContract::new(storage.clone());
        let floor = U256::from(540_000u64);
        let bin = GemContract::price_to_bin(floor).unwrap();
        assert_eq!(
            gem.unqualified_bin_count
                .read(&GemContract::scoped(840, bin))
                .unwrap(),
            1
        );
        assert_eq!(
            gem.unqualified_bin_gems
                .read(&GemContract::bin_index_key(840, bin, 0))
                .unwrap(),
            gem_id
        );
        assert!(tree_math::contains(&crate::state::CurrencyBins(&gem, 840), bin).unwrap());
    });
}

#[test]
fn qualify_removes_from_bin_tree() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let mut gem = GemContract::new(storage.clone());
        let floor = U256::from(540_000u64);
        let bin = GemContract::price_to_bin(floor).unwrap();

        assert!(gem
            .qualify(gem_id, T_NOW, 840, floor + U256::from(1u64))
            .unwrap());
        assert_eq!(
            gem.unqualified_bin_count
                .read(&GemContract::scoped(840, bin))
                .unwrap(),
            0
        );
        assert!(!tree_math::contains(&crate::state::CurrencyBins(&gem, 840), bin).unwrap());
    });
}

#[test]
fn add_gem_qualified_initial_state_skips_bin_tree() {
    with_storage(|storage| {
        let mut p = sample_params(ALICE);
        p.gem_type = 0;
        p.initial_state = GemState::Qualified;
        let _gem_id = api::add_gem(storage, p.clone()).unwrap();
        let gem = GemContract::new(storage.clone());
        let bin = GemContract::price_to_bin(p.floor_price_minor).unwrap();
        assert_eq!(
            gem.unqualified_bin_count
                .read(&GemContract::scoped(840, bin))
                .unwrap(),
            0
        );
        assert!(!tree_math::contains(&crate::state::CurrencyBins(&gem, 840), bin).unwrap());
    });
}

#[test]
fn scan_skips_bins_above_rate() {
    with_storage(|storage| {
        let mut low = sample_params(ALICE);
        low.floor_price_minor = U256::from(100_000u64);
        let low_id = api::add_gem(storage, low.clone()).unwrap();

        let mut high = sample_params(BOB);
        high.floor_price_minor = U256::from(900_000u64);
        let _high_id = api::add_gem(storage, high.clone()).unwrap();

        let mut gem = GemContract::new(storage.clone());
        let rate = U256::from(500_000u64);

        // Direct qualify call on low gem: passes (floor 0.1 < rate 0.5).
        assert!(gem.qualify(low_id, T_NOW, 840, rate).unwrap());

        // High gem stays Issued (rate 0.5 < floor 0.9). It must still be
        // in its bin and the bin must still be set in the trie.
        let high_bin = GemContract::price_to_bin(high.floor_price_minor).unwrap();
        assert_eq!(
            gem.unqualified_bin_count
                .read(&GemContract::scoped(840, high_bin))
                .unwrap(),
            1
        );
        assert!(tree_math::contains(&crate::state::CurrencyBins(&gem, 840), high_bin).unwrap());
    });
}

const EUR: u16 = 978;

/// Registers `iso_code` as a reference currency, and its `COEN/<iso>` pair when
/// `rate` is given. Returns the pair index, or 0 when no pair was registered.
fn seed_currency(storage: &StorageHandle, iso_code: u16, rate: Option<U256>) -> u32 {
    let oracle = OracleContract::new(storage.clone());
    oracle.reference_currencies.push(iso_code).unwrap();
    let Some(rate) = rate else {
        return 0;
    };
    let index =
        outbe_oracle::api::register_pair(storage.clone(), AddressPair::new_coen_to(iso_code))
            .unwrap();
    oracle.exchange_rate.write(&index, rate).unwrap();
    oracle.exchange_rate_timestamp.write(&index, T_NOW).unwrap();
    index
}

fn block_ctx<'s>(storage: &StorageHandle<'s>) -> outbe_primitives::block::BlockRuntimeContext<'s> {
    let timestamp = storage.timestamp().unwrap().to::<u64>();
    outbe_primitives::block::BlockRuntimeContext::new(
        outbe_primitives::block::BlockContext::empty_for_tests(1, timestamp, 1),
        storage.clone(),
    )
}

fn eur_gem(storage: &StorageHandle) -> U256 {
    let mut p = sample_params(BOB);
    p.reference_currency = EUR;
    api::add_gem(storage, p).unwrap()
}

/// The bin ladder is shared across currencies, so both gems below sit in the
/// same bin: each must be promoted only by its own currency's rate.
#[test]
fn scan_qualifies_each_currency_against_its_own_rate() {
    with_storage(|storage| {
        let usd_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let eur_id = eur_gem(storage);
        let floor = sample_params(ALICE).floor_price_minor;
        assert_eq!(
            GemContract::price_to_bin(floor).unwrap(),
            GemContract::price_to_bin(sample_params(BOB).floor_price_minor).unwrap()
        );

        seed_currency(storage, 840, Some(floor + U256::from(1u64)));
        seed_currency(storage, EUR, Some(floor - U256::from(1u64)));

        crate::hooks::scan_and_qualify(&block_ctx(storage)).unwrap();
        assert_eq!(
            api::get_gem(storage, usd_id).unwrap().unwrap().state,
            GemState::Qualified as u8
        );
        assert_eq!(
            api::get_gem(storage, eur_id).unwrap().unwrap().state,
            GemState::Issued as u8
        );
    });
}

/// The issuance currency is a settlement label and must never reach a lifecycle
/// decision: a gem whose two currencies differ is judged by its reference alone.
#[test]
fn a_gem_is_qualified_by_its_reference_currency_not_its_issuance_one() {
    with_storage(|storage| {
        let mut p = sample_params(ALICE);
        p.issuance_currency = EUR;
        let gem_id = api::add_gem(storage, p).unwrap();
        let floor = sample_params(ALICE).floor_price_minor;

        // The issuance currency is well above the floor, the reference one below.
        // Reading the wrong code would promote this gem.
        seed_currency(storage, 840, Some(floor - U256::from(1u64)));
        seed_currency(storage, EUR, Some(floor + U256::from(1u64)));

        crate::hooks::scan_and_qualify(&block_ctx(storage)).unwrap();
        assert_eq!(
            api::get_gem(storage, gem_id).unwrap().unwrap().state,
            GemState::Issued as u8
        );
    });
}

/// One currency filling the whole per-block budget must not starve the ones
/// behind it: the sweep resumes where it stopped instead of restarting.
#[test]
fn a_spent_budget_defers_the_rest_of_the_currency_list_to_the_next_block() {
    with_storage(|storage| {
        let floor = sample_params(ALICE).floor_price_minor;
        // Fill USD's bin past the budget. Whole bins are processed atomically, so
        // this one sweep spends everything the block had.
        for i in 0..=crate::constants::MAX_GEM_QUALIFICATIONS_PER_BLOCK {
            let mut p = sample_params(ALICE);
            p.gem_load_minor = U256::from(1_000_000u64 + u64::from(i));
            api::add_gem(storage, p).unwrap();
        }
        let eur_id = eur_gem(storage);
        seed_currency(storage, 840, Some(floor + U256::from(1u64)));
        seed_currency(storage, EUR, Some(floor + U256::from(1u64)));

        crate::hooks::scan_and_qualify(&block_ctx(storage)).unwrap();
        assert_eq!(
            api::get_gem(storage, eur_id).unwrap().unwrap().state,
            GemState::Issued as u8,
            "USD spent the budget, so EUR was not reached this block"
        );

        crate::hooks::scan_and_qualify(&block_ctx(storage)).unwrap();
        assert_eq!(
            api::get_gem(storage, eur_id).unwrap().unwrap().state,
            GemState::Qualified as u8,
            "the next block resumes at EUR rather than restarting at USD"
        );
    });
}

/// A registry entry whose COEN pair is unregistered must skip that currency for
/// the block, not halt the scan for the currencies that are priced.
#[test]
fn scan_skips_a_currency_without_a_priced_pair() {
    with_storage(|storage| {
        let usd_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let eur_id = eur_gem(storage);
        let floor = sample_params(ALICE).floor_price_minor;

        seed_currency(storage, 840, Some(floor + U256::from(1u64)));
        seed_currency(storage, EUR, None);

        crate::hooks::scan_and_qualify(&block_ctx(storage)).unwrap();
        assert_eq!(
            api::get_gem(storage, usd_id).unwrap().unwrap().state,
            GemState::Qualified as u8
        );
        assert_eq!(
            api::get_gem(storage, eur_id).unwrap().unwrap().state,
            GemState::Issued as u8
        );
    });
}

#[test]
fn scan_skips_a_currency_with_a_stale_rate() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let floor = sample_params(ALICE).floor_price_minor;
        seed_currency(storage, 840, Some(floor + U256::from(1u64)));
        storage
            .set_block_timestamp(U256::from(
                T_NOW + outbe_oracle::constants::FX_RATE_MAX_AGE_SECONDS + 1,
            ))
            .unwrap();

        crate::hooks::scan_and_qualify(&block_ctx(storage)).unwrap();
        assert_eq!(
            api::get_gem(storage, gem_id).unwrap().unwrap().state,
            GemState::Issued as u8
        );
    });
}

/// A sweep cut short by the budget must resume from its persisted bin cursor.
#[test]
fn qualify_resumes_from_the_bin_cursor_after_the_budget_runs_out() {
    with_storage(|storage| {
        let mut low = sample_params(ALICE);
        low.floor_price_minor = U256::from(100_000u64);
        let low_id = api::add_gem(storage, low).unwrap();
        let mut high = sample_params(BOB);
        high.floor_price_minor = U256::from(200_000u64);
        let high_id = api::add_gem(storage, high).unwrap();

        let rate = U256::from(500_000u64);
        let ctx = block_ctx(storage);

        // Budget of one: only the lower bin is drained this block.
        assert_eq!(
            crate::hooks::qualify_with_rate(&ctx, 840, rate, 1).unwrap(),
            1
        );
        assert_eq!(
            api::get_gem(storage, high_id).unwrap().unwrap().state,
            GemState::Issued as u8
        );
        assert!(
            GemContract::new(storage.clone())
                .qualify_scan_cursor
                .read(&840)
                .unwrap()
                > 0
        );

        // Next block picks up where it stopped, then resets for a fresh sweep.
        assert_eq!(
            crate::hooks::qualify_with_rate(&ctx, 840, rate, 256).unwrap(),
            1
        );
        for id in [low_id, high_id] {
            assert_eq!(
                api::get_gem(storage, id).unwrap().unwrap().state,
                GemState::Qualified as u8
            );
        }
        assert_eq!(
            GemContract::new(storage.clone())
                .qualify_scan_cursor
                .read(&840)
                .unwrap(),
            0
        );
    });
}

/// The qualified bins mix currencies, so the call scan must read each gem's
/// breaches off its own `COEN/<iso>` VWAP window.
#[test]
fn call_scan_reads_each_gem_own_pair_window() {
    with_storage(|storage| {
        let usd_id = qualified_gem(storage);
        let mut p = sample_params(BOB);
        p.reference_currency = EUR;
        p.issued_at = T_NOW - 100 * 86_400;
        let eur_id = api::add_gem(storage, p).unwrap();
        api::set_state(storage, eur_id, GemState::Qualified).unwrap();

        let rate = U256::from(600_000u64);
        seed_currency(storage, 840, Some(rate));
        let eur_pair = seed_currency(storage, EUR, Some(rate));

        // Only the EUR pair breaches: the USD pair has no published VWAPs.
        let breach = api::get_gem(storage, eur_id)
            .unwrap()
            .unwrap()
            .call_price_minor
            + U256::from(1u64);
        let oracle = OracleContract::new(storage.clone());
        let last_closed_day = previous_date_key(timestamp_to_date_key(T_NOW));
        let mut day = last_closed_day;
        for _ in 0..(crate::constants::CALL_THRESHOLD / 86_400) {
            oracle
                .utc_day_vwap_value
                .get_nested(&day)
                .write(&eur_pair, breach)
                .unwrap();
            day = previous_date_key(day);
        }
        oracle
            .utc_day_vwap_last_finalized
            .write(last_closed_day)
            .unwrap();

        assert_eq!(crate::hooks::scan_and_call(&block_ctx(storage)).unwrap(), 1);
        assert_eq!(
            api::get_gem(storage, eur_id).unwrap().unwrap().state,
            GemState::Called as u8
        );
        assert_eq!(
            api::get_gem(storage, usd_id).unwrap().unwrap().state,
            GemState::Qualified as u8
        );
    });
}

#[test]
fn precompile_transfer_paths_revert() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();

        let calls: Vec<Vec<u8>> = vec![
            IGem::transferFromCall {
                from: ALICE,
                to: BOB,
                gemId: gem_id,
            }
            .abi_encode(),
            IGem::safeTransferFromCall {
                from: ALICE,
                to: BOB,
                gemId: gem_id,
            }
            .abi_encode(),
            IGem::approveCall {
                to: BOB,
                gemId: gem_id,
            }
            .abi_encode(),
            IGem::setApprovalForAllCall {
                operator: BOB,
                approved: true,
            }
            .abi_encode(),
        ];

        for data in calls {
            let err = dispatch(storage.clone(), &data, ALICE, U256::ZERO).unwrap_err();
            assert!(
                format!("{err:?}").contains("non-transferable"),
                "expected NonTransferable revert, got {err:?}",
            );
        }
    });
}

#[test]
fn precompile_balance_and_owner_views() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();

        let data = IGem::balanceOfCall { owner: ALICE }.abi_encode();
        let bytes = dispatch(storage.clone(), &data, Address::ZERO, U256::ZERO).unwrap();
        let bal = IGem::balanceOfCall::abi_decode_returns(&bytes).unwrap();
        assert_eq!(bal, U256::from(1u64));

        let data = IGem::ownerOfCall { gemId: gem_id }.abi_encode();
        let bytes = dispatch(storage.clone(), &data, Address::ZERO, U256::ZERO).unwrap();
        let owner = IGem::ownerOfCall::abi_decode_returns(&bytes).unwrap();
        assert_eq!(owner, ALICE);

        let data = IGem::totalSupplyCall {}.abi_encode();
        let bytes = dispatch(storage.clone(), &data, Address::ZERO, U256::ZERO).unwrap();
        let total = IGem::totalSupplyCall::abi_decode_returns(&bytes).unwrap();
        assert_eq!(total, U256::from(1u64));
    });
}

/// Pins the flat `GemContract` storage layout that `scripts/seed_genesis.py`
/// (`seed_gems`) depends on to genesis-seed a Settled gem. If the schema field
/// order or `GemData` field count changes, these slots shift and the Python
/// seeder must be updated in lockstep — this test is the tripwire.
#[test]
fn gem_storage_layout_matches_genesis_seeder() {
    use outbe_primitives::storage::dsl::StorageRecord;
    with_storage(|storage| {
        let gem = GemContract::new(storage.clone());
        assert_eq!(gem.total_supply.slot(), U256::from(0u64));
        assert_eq!(gem.gem_items.base_slot(), U256::from(1u64));
        // GemData record spans 18 slots (owner@+0 .. settled_at@+17), so
        // the schema fields after gem_items start at 1 + 18 = 19.
        assert_eq!(<crate::schema::GemData as StorageRecord>::SLOTS, 18);
        assert_eq!(gem.owner_gem_counts.base_slot(), U256::from(19u64));
        assert_eq!(gem.owner_gem_ids.base_slot(), U256::from(20u64));
        // all_gem_ids (List) occupies slot 21.
        assert_eq!(gem.gem_index.base_slot(), U256::from(22u64));
        // The seeder writes the raw `state` byte, so its GEM_STATE_SETTLED must
        // track this discriminant.
        assert_eq!(GemState::Settled as u8, 3);
    });
}
/// Build a full-window (newest-first) list with `breach_days` entries above the
/// gem's call threshold, the rest at zero.
fn breach_window(now: u64, breach: U256, breach_days: usize) -> Vec<(u32, Option<U256>)> {
    let window_days = (crate::constants::CALL_WINDOW / 86_400) as usize;
    let mut window = Vec::with_capacity(window_days);
    let mut day = timestamp_to_date_key(now);
    for i in 0..window_days {
        let v = if i < breach_days { breach } else { U256::ZERO };
        window.push((day, Some(v)));
        day = previous_date_key(day);
    }
    window
}

fn qualified_gem(storage: &StorageHandle) -> U256 {
    let mut p = sample_params(ALICE);
    // Issue well before the window so no day is skipped as pre-issuance.
    p.issued_at = T_NOW - 100 * 86_400;
    let gem_id = api::add_gem(storage, p).unwrap();
    api::set_state(storage, gem_id, GemState::Qualified).unwrap();
    gem_id
}

/// The load of a gem nobody settled goes back to the emission accumulator.
#[test]
fn forfeiting_a_gem_returns_its_load_to_the_pool() {
    with_storage(|storage| {
        let gem_id = qualified_gem(storage);
        let load = api::get_gem(storage, gem_id)
            .unwrap()
            .unwrap()
            .gem_load_minor;
        let mut gem = GemContract::new(storage.clone());
        gem.mark_called(gem_id, T_NOW).unwrap();

        // Inside the notice period nothing moves.
        assert!(!gem.forfeit(gem_id, T_NOW + 6 * 86_400).unwrap());
        assert_eq!(unallocated(storage), U256::ZERO);

        assert!(gem.forfeit(gem_id, T_NOW + 7 * 86_400 + 1).unwrap());
        assert_eq!(unallocated(storage), load);
    });
}

/// A settled gem leaves the queue, so the forfeit arm can never reach it — its
/// holder paid the strike and the load is theirs.
#[test]
fn a_settled_gem_is_never_forfeited() {
    with_storage(|storage| {
        let gem_id = qualified_gem(storage);
        let mut gem = GemContract::new(storage.clone());
        gem.mark_called(gem_id, T_NOW).unwrap();
        gem.set_state(gem_id, GemState::Settled).unwrap();

        assert!(!gem.forfeit(gem_id, T_NOW + 7 * 86_400 + 1).unwrap());
        assert_eq!(unallocated(storage), U256::ZERO);
        assert!(gem.called_queue_slot(0).unwrap().is_none());
    });
}

fn unallocated(storage: &StorageHandle) -> U256 {
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .get_total_unallocated()
        .unwrap()
}

/// The two stages keep separate structures for a reason: a gem priced above every
/// day in the window is never visited by the price pass, yet once called it still
/// expires on schedule, because expiry reads the queue and not the tree.
#[test]
fn a_gem_above_the_window_is_not_visited_but_still_expires() {
    with_storage(|storage| {
        let gem_id = qualified_gem(storage);
        let call_price = api::get_gem(storage, gem_id)
            .unwrap()
            .unwrap()
            .call_price_minor;

        // Every published day sits below the gem's call price.
        let pair = seed_currency(storage, 840, Some(U256::from(600_000u64)));
        let oracle = OracleContract::new(storage.clone());
        let last_closed_day = previous_date_key(timestamp_to_date_key(T_NOW));
        let mut day = last_closed_day;
        for _ in 0..(crate::constants::CALL_WINDOW / 86_400) {
            oracle
                .utc_day_vwap_value
                .get_nested(&day)
                .write(&pair, call_price - U256::from(1u64))
                .unwrap();
            day = previous_date_key(day);
        }
        oracle
            .utc_day_vwap_last_finalized
            .write(last_closed_day)
            .unwrap();

        assert_eq!(crate::hooks::scan_and_call(&block_ctx(storage)).unwrap(), 0);
        assert_eq!(
            api::get_gem(storage, gem_id).unwrap().unwrap().state,
            GemState::Qualified as u8
        );

        // Called by hand, it leaves the tree for the queue and expires from there.
        let mut gem = GemContract::new(storage.clone());
        gem.mark_called(gem_id, T_NOW).unwrap();
        assert!(gem.forfeit(gem_id, T_NOW + 7 * 86_400 + 1).unwrap());
        assert!(api::get_gem(storage, gem_id).unwrap().is_none());
    });
}

#[test]
fn call_then_forfeit_lifecycle() {
    with_storage(|storage| {
        let gem_id = qualified_gem(storage);
        let threshold = api::get_gem(storage, gem_id)
            .unwrap()
            .unwrap()
            .call_price_minor;
        let breach_days = (crate::constants::CALL_THRESHOLD / 86_400) as usize;
        let window = breach_window(T_NOW, threshold + U256::from(1u64), breach_days);

        let mut gem = GemContract::new(storage.clone());
        assert!(gem.trigger_call(&window, gem_id, T_NOW).unwrap());
        let item = api::get_gem(storage, gem_id).unwrap().unwrap();
        assert_eq!(item.state, GemState::Called as u8);
        assert_eq!(item.called_at, T_NOW);

        // Within the 7-day notice period: no forfeit.
        assert!(!gem.forfeit(gem_id, T_NOW + 6 * 86_400).unwrap());
        // Past the deadline: forfeit-burned.
        assert!(gem.forfeit(gem_id, T_NOW + 7 * 86_400 + 1).unwrap());
        assert!(api::get_gem(storage, gem_id).unwrap().is_none());
    });
}

#[test]
fn call_skips_below_threshold() {
    with_storage(|storage| {
        let gem_id = qualified_gem(storage);
        let threshold = api::get_gem(storage, gem_id)
            .unwrap()
            .unwrap()
            .call_price_minor;
        // One below the threshold: not enough breach-days to force a call.
        let breach_days = (crate::constants::CALL_THRESHOLD / 86_400) as usize - 1;
        let window = breach_window(T_NOW, threshold + U256::from(1u64), breach_days);

        let mut gem = GemContract::new(storage.clone());
        assert!(!gem.trigger_call(&window, gem_id, T_NOW).unwrap());
        assert_eq!(
            api::get_gem(storage, gem_id).unwrap().unwrap().state,
            GemState::Qualified as u8
        );
    });
}
