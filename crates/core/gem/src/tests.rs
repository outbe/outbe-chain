use alloy_primitives::{address, Address, U256};
use alloy_sol_types::SolCall;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::math::constants::REAL_ID_SHIFT;
use outbe_primitives::math::tree_math;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{date_key_to_utc_timestamp, previous_date_key, timestamp_to_date_key};

use crate::api;
use crate::config::GemParams;
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

/// Two days after the sample gems were issued: the day before it they held in full.
const QUALIFY_TS: u64 = T_NOW + 2 * 86_400;

/// Closes the day before `QUALIFY_TS` at `vwap` for the pair at `index`.
fn close_day(storage: &StorageHandle, index: u32, vwap: U256) {
    let oracle = OracleContract::new(storage.clone());
    let day = previous_date_key(timestamp_to_date_key(QUALIFY_TS));
    oracle.record_utc_day_vwap(day, index, vwap).unwrap();
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < day {
        oracle.utc_day_vwap_last_finalized.write(day).unwrap();
    }
}

/// Lists `iso_code` and, given a price, registers its pair and closes the day at it.
fn seed_day_price(storage: &StorageHandle, iso_code: u16, vwap: Option<U256>) -> u32 {
    let oracle = OracleContract::new(storage.clone());
    oracle.reference_currencies.push(iso_code).unwrap();
    let Some(vwap) = vwap else {
        return 0;
    };
    let index =
        outbe_oracle::api::register_pair(storage.clone(), AddressPair::new_coen_to(iso_code))
            .unwrap();
    close_day(storage, index, vwap);
    index
}

fn sample_params(owner: Address) -> GemAddParams {
    GemAddParams {
        owner,
        gem_type: 2, // WALLET
        promis_load_minor: U256::from(1_000_000u64),
        entry_price_minor: U256::from(500_000u64),
        floor_price_minor: U256::from(540_000u64),
        call_price_minor: U256::from(1_140_000u64),
        call_rate: 228,
        issuance_currency: 840,
        reference_currency: 840,
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
        p2.promis_load_minor = U256::from(2u64);
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
        p2.promis_load_minor = U256::from(2u64);
        let g2 = api::add_gem(storage, p2).unwrap();
        let mut p3 = sample_params(ALICE);
        p3.promis_load_minor = U256::from(3u64);
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

fn burn_settled(storage: &StorageHandle, gem_id: U256) {
    api::set_state(storage, gem_id, GemState::Settled).unwrap();
    api::burn(storage, gem_id).unwrap();
}

fn alice_gems(storage: &StorageHandle, loads: &[u64]) -> Vec<U256> {
    loads
        .iter()
        .map(|load| {
            let mut params = sample_params(ALICE);
            params.promis_load_minor = U256::from(*load);
            api::add_gem(storage, params).unwrap()
        })
        .collect()
}

/// The owner list is compacted through each gem's stored position, not by a scan.
#[test]
fn burn_moves_the_last_gem_into_the_hole_and_updates_its_position() {
    with_storage(|storage| {
        let gems = alice_gems(storage, &[1, 2, 3]);
        let gem = GemContract::new(storage.clone());
        assert_eq!(gem.owner_gem_position.read(&gems[2]).unwrap(), 3);

        burn_settled(storage, gems[0]);
        assert_eq!(gem.token_of_owner_by_index(ALICE, 0).unwrap(), gems[2]);
        assert_eq!(gem.owner_gem_position.read(&gems[2]).unwrap(), 1);
        assert_eq!(gem.owner_gem_position.read(&gems[0]).unwrap(), 0);

        burn_settled(storage, gems[2]);
        assert_eq!(gem.balance_of(ALICE).unwrap(), 1);
        assert_eq!(gem.token_of_owner_by_index(ALICE, 0).unwrap(), gems[1]);
    });
}

/// A gem written before positions existed, such as a genesis seed, is still found.
#[test]
fn a_gem_without_a_position_is_found_by_scanning_its_owner_list() {
    with_storage(|storage| {
        let gems = alice_gems(storage, &[1, 2]);
        let gem = GemContract::new(storage.clone());
        gem.owner_gem_position.clear(&gems[0]).unwrap();

        burn_settled(storage, gems[0]);
        assert_eq!(gem.balance_of(ALICE).unwrap(), 1);
        assert_eq!(gem.token_of_owner_by_index(ALICE, 0).unwrap(), gems[1]);
        assert_eq!(gem.owner_gem_position.read(&gems[1]).unwrap(), 1);
    });
}

fn bucket_of(storage: &StorageHandle, gem_id: U256) -> alloy_primitives::B256 {
    GemContract::new(storage.clone())
        .gem_bucket
        .read(&gem_id)
        .unwrap()
}

/// Calls the gem's whole bucket at `at`.
fn call_gem(storage: &StorageHandle, gem_id: U256, at: u64) {
    let mut gem = GemContract::new(storage.clone());
    let bucket = gem.gem_bucket.read(&gem_id).unwrap();
    let terms = gem.read_bucket_terms(bucket).unwrap();
    gem.mark_bucket_called(bucket, &terms, at).unwrap();
}

/// Gems share a bucket only when every term of the call decision matches.
#[test]
fn a_bucket_holds_the_gems_whose_call_terms_all_match() {
    with_storage(|storage| {
        let gems = alice_gems(storage, &[1, 2]);
        let bucket = bucket_of(storage, gems[0]);
        assert!(!bucket.is_zero());
        assert_eq!(
            bucket_of(storage, gems[1]),
            bucket,
            "the load is not a term"
        );

        let nonce = std::cell::Cell::new(0u64);
        let apart = |edit: &dyn Fn(&mut GemAddParams)| {
            let mut params = sample_params(BOB);
            nonce.set(nonce.get() + 1);
            params.promis_load_minor = U256::from(nonce.get());
            edit(&mut params);
            bucket_of(storage, api::add_gem(storage, params).unwrap())
        };
        assert_ne!(apart(&|p| p.issued_at += 86_400), bucket);
        assert_eq!(
            apart(&|p| p.issued_at += 1),
            bucket,
            "the same first full day"
        );
        assert_ne!(apart(&|p| p.reference_currency = EUR), bucket);
        assert_ne!(apart(&|p| p.call_price_minor += U256::from(1u64)), bucket);
        GemContract::new(storage.clone())
            .config_profile
            .write(crate::config::PROFILE_PROD)
            .unwrap();
        assert_ne!(apart(&|_| {}), bucket, "other window, threshold and notice");
    });
}

#[test]
fn leaving_a_bucket_moves_its_last_member_into_the_hole() {
    with_storage(|storage| {
        let gems = alice_gems(storage, &[1, 2, 3]);
        let bucket = bucket_of(storage, gems[0]);
        let gem = GemContract::new(storage.clone());

        burn_settled(storage, gems[0]);
        assert_eq!(gem.bucket_gem_count.read(&bucket).unwrap(), 2);
        assert_eq!(
            gem.bucket_gems
                .read(&GemContract::bucket_member_key(bucket, 0))
                .unwrap(),
            gems[2]
        );
        assert_eq!(gem.bucket_gem_index.read(&gems[2]).unwrap(), 0);
        assert!(gem.gem_bucket.read(&gems[0]).unwrap().is_zero());
    });
}

#[test]
fn the_last_gem_out_closes_its_bucket() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let bucket = bucket_of(storage, gem_id);
        let gem = GemContract::new(storage.clone());
        let bin = GemContract::price_to_bin(sample_params(ALICE).call_price_minor).unwrap();
        assert!(tree_math::contains(&crate::buckets::BucketBins(&gem, 840), bin).unwrap());

        api::set_state(storage, gem_id, GemState::Settled).unwrap();
        assert_eq!(gem.bucket_gem_count.read(&bucket).unwrap(), 0);
        assert!(gem.bucket_call_price.read(&bucket).unwrap().is_zero());
        assert_eq!(gem.bucket_bin_index.read(&bucket).unwrap(), 0);
        assert!(!tree_math::contains(&crate::buckets::BucketBins(&gem, 840), bin).unwrap());
    });
}

/// Whether the gem has qualified, as settlement and the view read it.
fn is_qualified(storage: &StorageHandle, gem_id: U256) -> bool {
    let item = api::get_gem(storage, gem_id).unwrap().unwrap();
    api::is_qualified(storage, &item).unwrap()
}

#[test]
fn a_gem_qualifies_on_a_closed_day_above_its_floor() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let floor = sample_params(ALICE).floor_price_minor;

        assert!(!is_qualified(storage, gem_id), "no finalized day yet");
        let index = seed_day_price(storage, 840, Some(floor));
        assert!(
            !is_qualified(storage, gem_id),
            "a day at the floor does not qualify"
        );
        close_day(storage, index, floor + U256::from(1u64));
        assert!(is_qualified(storage, gem_id));
        assert_eq!(
            gem_state(storage, gem_id),
            GemState::Issued as u8,
            "nothing is stored"
        );
    });
}

#[test]
fn only_a_genesis_gem_is_issued_without_a_floor() {
    with_storage(|storage| {
        let mut p = sample_params(ALICE);
        p.floor_price_minor = U256::ZERO;
        assert!(api::add_gem(storage, p.clone()).is_err());

        p.gem_type = crate::GENESIS_GEM_TYPE;
        assert!(api::add_gem(storage, p).is_ok());
    });
}

#[test]
fn set_state_only_settles() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        for state in [GemState::Issued, GemState::Called] {
            assert!(api::set_state(storage, gem_id, state).is_err());
        }
        assert_eq!(gem_state(storage, gem_id), GemState::Issued as u8);
    });
}

#[test]
fn is_qualified_dispatch() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let floor = sample_params(ALICE).floor_price_minor;
        seed_day_price(storage, 840, Some(floor + U256::from(1u64)));

        let data = IGem::isQualifiedCall { gemId: gem_id }.abi_encode();
        let bytes = dispatch(storage.clone(), &data, Address::ZERO, U256::ZERO).unwrap();
        assert!(IGem::isQualifiedCall::abi_decode_returns(&bytes).unwrap());
    });
}

/// A zero floor still needs one finalized day; any positive price then clears it.
#[test]
fn a_gem_without_a_floor_qualifies_on_its_first_closed_day() {
    with_storage(|storage| {
        let mut p = sample_params(ALICE);
        p.gem_type = 0;
        p.floor_price_minor = U256::ZERO;
        let gem_id = api::add_gem(storage, p.clone()).unwrap();
        assert!(!is_qualified(storage, gem_id), "no finalized day yet");
        seed_day_price(storage, 840, Some(U256::from(1u64)));
        assert!(is_qualified(storage, gem_id));
        let gem = GemContract::new(storage.clone());
        let bin = GemContract::price_to_bin(p.call_price_minor).unwrap();
        assert!(tree_math::contains(&crate::buckets::BucketBins(&gem, 840), bin).unwrap());
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

fn block_ctx_at<'s>(
    storage: &StorageHandle<'s>,
    timestamp: u64,
) -> outbe_primitives::block::BlockRuntimeContext<'s> {
    outbe_primitives::block::BlockRuntimeContext::new(
        outbe_primitives::block::BlockContext::empty_for_tests(1, timestamp, 1),
        storage.clone(),
    )
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

/// Both gems carry the same floor: each must be qualified only by its own currency.
#[test]
fn each_currency_qualifies_against_its_own_day_price() {
    with_storage(|storage| {
        let usd_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let eur_id = eur_gem(storage);
        let floor = sample_params(ALICE).floor_price_minor;

        seed_day_price(storage, 840, Some(floor + U256::from(1u64)));
        seed_day_price(storage, EUR, Some(floor - U256::from(1u64)));

        assert!(is_qualified(storage, usd_id));
        assert!(!is_qualified(storage, eur_id));
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
        seed_day_price(storage, 840, Some(floor - U256::from(1u64)));
        seed_day_price(storage, EUR, Some(floor + U256::from(1u64)));

        assert!(!is_qualified(storage, gem_id));
    });
}

/// A currency whose COEN pair is unregistered qualifies nothing, and says so
/// without an error.
#[test]
fn a_currency_without_a_priced_pair_qualifies_nothing() {
    with_storage(|storage| {
        let eur_id = eur_gem(storage);
        seed_day_price(storage, EUR, None);
        assert!(!is_qualified(storage, eur_id));
    });
}

#[test]
fn a_day_without_a_price_qualifies_nothing_and_the_next_one_still_can() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let floor = sample_params(ALICE).floor_price_minor;
        // Finalized, but no trade that day: the pair is known and the day is empty.
        let index = seed_day_price(storage, 840, Some(U256::ZERO));
        assert!(!is_qualified(storage, gem_id));

        let next_day = previous_date_key(timestamp_to_date_key(QUALIFY_TS + 86_400));
        let oracle = OracleContract::new(storage.clone());
        oracle
            .record_utc_day_vwap(next_day, index, floor + U256::from(1u64))
            .unwrap();
        oracle.utc_day_vwap_last_finalized.write(next_day).unwrap();
        assert!(is_qualified(storage, gem_id));
    });
}

/// The call-price bins mix currencies, so the call scan must read each gem's
/// breaches off its own `COEN/<iso>` VWAP window.
#[test]
fn call_scan_reads_each_gem_own_pair_window() {
    with_storage(|storage| {
        let usd_id = mature_gem(storage);
        let mut p = sample_params(BOB);
        p.reference_currency = EUR;
        p.issued_at = T_NOW - 100 * 86_400;
        let eur_id = api::add_gem(storage, p).unwrap();

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
            oracle.record_utc_day_vwap(day, eur_pair, breach).unwrap();
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
            GemState::Issued as u8
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
            IGem::safeTransferFrom_0Call {
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
/// seeder must be updated in lockstep - this test is the tripwire.
#[test]
fn gem_storage_layout_matches_genesis_seeder() {
    use outbe_primitives::storage::dsl::StorageRecord;
    with_storage(|storage| {
        let gem = GemContract::new(storage.clone());
        assert_eq!(gem.total_supply.slot(), U256::from(0u64));
        assert_eq!(gem.gem_items.base_slot(), U256::from(1u64));
        // GemData record spans 16 slots (owner@+0 .. settled_at@+15), so
        // the schema fields after gem_items start at 1 + 16 = 17.
        assert_eq!(<crate::schema::GemData as StorageRecord>::SLOTS, 16);
        assert_eq!(gem.owner_gem_counts.base_slot(), U256::from(17u64));
        assert_eq!(gem.owner_gem_ids.base_slot(), U256::from(18u64));
        // all_gem_ids (List) occupies slot 19.
        assert_eq!(gem.gem_index.base_slot(), U256::from(20u64));
        assert_eq!(gem.owner_gem_position.base_slot(), U256::from(43u64));
        assert_eq!(gem.gem_bucket.base_slot(), U256::from(44u64));
        assert_eq!(gem.bucket_scan_cursor.base_slot(), U256::from(61u64));
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

fn mature_gem(storage: &StorageHandle) -> U256 {
    // These cases reason in the PROD call terms; the test chain id resolves to DEV.
    GemContract::new(storage.clone())
        .config_profile
        .write(crate::config::PROFILE_PROD)
        .unwrap();
    let mut p = sample_params(ALICE);
    // Issue well before the window so no day is skipped as pre-issuance.
    p.issued_at = T_NOW - 100 * 86_400;
    api::add_gem(storage, p).unwrap()
}

#[test]
fn the_call_pass_resumes_from_its_bin_cursor() {
    with_storage(|storage| {
        let mut low = sample_params(ALICE);
        low.issued_at = T_NOW - 100 * 86_400;
        low.call_price_minor = U256::from(100_000u64);
        let low_id = api::add_gem(storage, low).unwrap();
        let mut high = sample_params(BOB);
        high.issued_at = T_NOW - 100 * 86_400;
        high.call_price_minor = U256::from(200_000u64);
        let high_id = api::add_gem(storage, high).unwrap();

        // Every day of the window sits above both call prices.
        let pair = seed_currency(storage, 840, Some(U256::from(600_000u64)));
        let oracle = OracleContract::new(storage.clone());
        let last_closed_day = previous_date_key(timestamp_to_date_key(T_NOW));
        let mut day = last_closed_day;
        for _ in 0..(crate::constants::CALL_WINDOW / 86_400) {
            oracle
                .record_utc_day_vwap(day, pair, U256::from(300_000u64))
                .unwrap();
            day = previous_date_key(day);
        }
        oracle
            .utc_day_vwap_last_finalized
            .write(last_closed_day)
            .unwrap();

        // A budget of one takes the lower bin and persists the cursor above it.
        let ctx = block_ctx(storage);
        let mut budget = 1u32;
        let window = vec![
            (last_closed_day, Some(U256::from(300_000u64)));
            (crate::constants::CALL_WINDOW / 86_400) as usize
        ];
        assert_eq!(
            crate::hooks::call_currency(
                &ctx,
                840,
                &window,
                outbe_primitives::math::constants::MAX_BIN_ID,
                &mut budget
            )
            .unwrap(),
            (1, false)
        );
        assert_eq!(
            api::get_gem(storage, low_id).unwrap().unwrap().state,
            GemState::Called as u8
        );
        assert_eq!(
            api::get_gem(storage, high_id).unwrap().unwrap().state,
            GemState::Issued as u8
        );

        let mut budget = 8u32;
        assert_eq!(
            crate::hooks::call_currency(
                &ctx,
                840,
                &window,
                outbe_primitives::math::constants::MAX_BIN_ID,
                &mut budget
            )
            .unwrap(),
            (1, true)
        );
        assert_eq!(
            api::get_gem(storage, high_id).unwrap().unwrap().state,
            GemState::Called as u8
        );
    });
}

/// The daily trigger opens a sweep and closes it once nothing is left to walk;
/// with none open, a block costs nothing and calls nobody.
#[test]
fn a_finished_sweep_closes_itself_and_idle_blocks_do_nothing() {
    with_storage(|storage| {
        let mut first = sample_params(ALICE);
        first.issued_at = T_NOW - 100 * 86_400;
        first.call_price_minor = U256::from(100_000u64);
        let first_id = api::add_gem(storage, first).unwrap();

        let pair = seed_currency(storage, 840, Some(U256::from(600_000u64)));
        let oracle = OracleContract::new(storage.clone());
        let last_closed_day = previous_date_key(timestamp_to_date_key(T_NOW));
        let mut day = last_closed_day;
        for _ in 0..(crate::constants::CALL_WINDOW / 86_400) {
            oracle
                .record_utc_day_vwap(day, pair, U256::from(300_000u64))
                .unwrap();
            day = previous_date_key(day);
        }
        oracle
            .utc_day_vwap_last_finalized
            .write(last_closed_day)
            .unwrap();

        let ctx = block_ctx(storage);
        crate::hooks::run_daily(&ctx).unwrap();
        let gem = GemContract::new(storage.clone());
        assert_eq!(
            api::get_gem(storage, first_id).unwrap().unwrap().state,
            GemState::Called as u8
        );
        assert_eq!(gem.call_sweep_day.read().unwrap(), 0);

        // A gem that breaches just as hard is left alone: no sweep is open.
        let mut second = sample_params(BOB);
        second.issued_at = T_NOW - 100 * 86_400;
        second.call_price_minor = U256::from(100_001u64);
        let second_id = api::add_gem(storage, second).unwrap();
        assert_eq!(crate::hooks::run_call_slice(&ctx).unwrap(), 0);
        assert_eq!(
            api::get_gem(storage, second_id).unwrap().unwrap().state,
            GemState::Issued as u8
        );
    });
}

/// A bin holding more buckets than the budget resumes inside itself on the next slice.
#[test]
fn a_bin_wider_than_the_budget_resumes_inside_it() {
    with_storage(|storage| {
        let mut ids = Vec::new();
        for (owner, price) in [(ALICE, 100_000u64), (BOB, 100_001)] {
            let mut params = sample_params(owner);
            params.issued_at = T_NOW - 100 * 86_400;
            params.call_price_minor = U256::from(price);
            ids.push(api::add_gem(storage, params).unwrap());
        }
        let bin = GemContract::price_to_bin(U256::from(100_000u64)).unwrap();
        assert_eq!(
            GemContract::price_to_bin(U256::from(100_001u64)).unwrap(),
            bin
        );

        let ctx = block_ctx(storage);
        let last_closed_day = previous_date_key(timestamp_to_date_key(T_NOW));
        let window = vec![
            (last_closed_day, Some(U256::from(300_000u64)));
            (crate::constants::CALL_WINDOW / 86_400) as usize
        ];
        let walk = |budget: u32| {
            let mut budget = budget;
            crate::hooks::call_currency(
                &ctx,
                840,
                &window,
                outbe_primitives::math::constants::MAX_BIN_ID,
                &mut budget,
            )
            .unwrap()
        };
        assert_eq!(walk(1), (1, false));
        assert_eq!(gem_state(storage, ids[1]), GemState::Called as u8);
        assert_eq!(gem_state(storage, ids[0]), GemState::Issued as u8);
        assert_eq!(
            GemContract::new(storage.clone())
                .bucket_scan_cursor
                .read(&840)
                .unwrap(),
            (u64::from(bin) << 32) | 1
        );

        assert_eq!(walk(8), (1, true));
        assert_eq!(gem_state(storage, ids[0]), GemState::Called as u8);
    });
}

/// An entry the sweep cannot retire credits nothing - the burn and the credit
/// share a checkpoint - and leaves its bucket, so the gems behind it still drain.
#[test]
fn an_entry_the_sweep_cannot_retire_does_not_hold_up_its_bucket() {
    with_storage(|storage| {
        let live = mature_gem(storage);
        let mut gem = GemContract::new(storage.clone());
        // A slot pointing at a gem that is not there: forfeit errors every run.
        let ghost = U256::from(0xdeadu64);
        let deadline = T_NOW + 7 * 86_400;
        gem.push_called(ghost, deadline).unwrap();
        call_gem(storage, live, T_NOW);
        let day = GemContract::deadline_hour(deadline);
        let load = api::get_gem(storage, live)
            .unwrap()
            .unwrap()
            .promis_load_minor;

        let ctx = block_ctx_at(storage, GemContract::hour_end(day));
        <crate::hooks::GemLifecycle as outbe_primitives::block::BlockLifecycle>::begin_block(&ctx)
            .unwrap();

        assert_eq!(gem.expiry_slot(day, 0).unwrap(), None, "the ghost is out");
        assert_eq!(
            unallocated(storage),
            load,
            "and the gem behind it was still forfeited"
        );
    });
}

/// Same for a due entry whose gem is no longer Called: it burns nothing.
#[test]
fn a_due_entry_that_cannot_burn_credits_nothing() {
    with_storage(|storage| {
        let gem_id = mature_gem(storage);
        let mut gem = GemContract::new(storage.clone());
        gem.push_called(gem_id, T_NOW).unwrap();

        let ctx = block_ctx_at(
            storage,
            GemContract::hour_end(GemContract::deadline_hour(T_NOW)),
        );
        <crate::hooks::GemLifecycle as outbe_primitives::block::BlockLifecycle>::begin_block(&ctx)
            .unwrap();

        assert_eq!(unallocated(storage), U256::ZERO);
    });
}

#[test]
fn forfeiting_a_gem_returns_its_load_to_the_pool() {
    with_storage(|storage| {
        let gem_id = mature_gem(storage);
        let load = api::get_gem(storage, gem_id)
            .unwrap()
            .unwrap()
            .promis_load_minor;
        let mut gem = GemContract::new(storage.clone());
        call_gem(storage, gem_id, T_NOW);

        assert!(!gem.forfeit(gem_id, T_NOW + 6 * 86_400).unwrap());
        assert_eq!(unallocated(storage), U256::ZERO);

        assert!(gem.forfeit(gem_id, T_NOW + 7 * 86_400 + 1).unwrap());
        assert_eq!(unallocated(storage), load);
    });
}

/// Its owner paid the strike, so the load is theirs.
#[test]
fn a_settled_gem_is_never_forfeited() {
    with_storage(|storage| {
        let gem_id = mature_gem(storage);
        let bucket = bucket_of(storage, gem_id);
        let mut gem = GemContract::new(storage.clone());
        call_gem(storage, gem_id, T_NOW);
        gem.set_state(gem_id, GemState::Settled).unwrap();

        assert!(!gem.forfeit(gem_id, T_NOW + 7 * 86_400 + 1).unwrap());
        assert_eq!(unallocated(storage), U256::ZERO);
        let entry = crate::buckets::bucket_entry(bucket);
        assert_eq!(gem.called_bucket_slot.read(&entry).unwrap(), 0);
    });
}

fn unallocated(storage: &StorageHandle) -> U256 {
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .get_total_unallocated()
        .unwrap()
}

/// Expiry reads the queue, not the tree: the reason the two stages are separate.
#[test]
fn a_gem_above_the_window_is_not_visited_but_still_expires() {
    with_storage(|storage| {
        let gem_id = mature_gem(storage);
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
                .record_utc_day_vwap(day, pair, call_price - U256::from(1u64))
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
            GemState::Issued as u8
        );

        let mut gem = GemContract::new(storage.clone());
        call_gem(storage, gem_id, T_NOW);
        assert!(gem.forfeit(gem_id, T_NOW + 7 * 86_400 + 1).unwrap());
        assert!(api::get_gem(storage, gem_id).unwrap().is_none());
    });
}

#[test]
fn call_then_forfeit_lifecycle() {
    with_storage(|storage| {
        let gem_id = mature_gem(storage);
        let threshold = api::get_gem(storage, gem_id)
            .unwrap()
            .unwrap()
            .call_price_minor;
        let breach_days = (crate::constants::CALL_THRESHOLD / 86_400) as usize;
        let window = breach_window(T_NOW, threshold + U256::from(1u64), breach_days);

        let mut gem = GemContract::new(storage.clone());
        assert!(gem
            .trigger_bucket_call(&window, bucket_of(storage, gem_id), T_NOW)
            .unwrap());
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
fn an_unindexable_price_skips_its_currency_for_the_day_and_says_so() {
    use alloy_sol_types::SolEvent;

    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(T_NOW));
    let pinned_day = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = mature_gem(&storage);
        // A price no bin can hold; a begin-block error would fail the whole block.
        let pair = seed_currency(&storage, 840, Some(U256::from(600_000u64)));
        let oracle = OracleContract::new(storage.clone());
        let last_closed_day = previous_date_key(timestamp_to_date_key(T_NOW));
        oracle
            .record_utc_day_vwap(last_closed_day, pair, U256::MAX)
            .unwrap();
        oracle
            .utc_day_vwap_last_finalized
            .write(last_closed_day)
            .unwrap();

        let ctx = block_ctx(&storage);
        assert_eq!(crate::hooks::scan_and_call(&ctx).unwrap(), 0);
        assert_eq!(
            api::get_gem(&storage, gem_id).unwrap().unwrap().state,
            GemState::Issued as u8,
            "the gem is untouched, not lost"
        );
        let gem = GemContract::new(storage.clone());
        assert_eq!(
            gem.call_scan_failed_day.read(&840).unwrap(),
            last_closed_day,
            "the currency is marked for the day it failed on"
        );

        // Another slice of the same pass skips it instead of re-reading the window.
        gem.call_sweep_day.write(last_closed_day).unwrap();
        assert_eq!(crate::hooks::run_call_slice(&ctx).unwrap(), 0);
        last_closed_day
    });

    let skipped: Vec<_> = provider
        .get_events(outbe_primitives::addresses::GEM_ADDRESS)
        .iter()
        .filter_map(|log| IGem::CallScanSkipped::decode_log_data(log).ok())
        .collect();
    assert_eq!(skipped.len(), 1, "one event for the day, not one per slice");
    assert_eq!(skipped[0].referenceCurrency, 840);
    assert_eq!(skipped[0].utcDay, pinned_day);
}

#[test]
fn the_issue_day_counts_only_for_a_gem_issued_at_midnight() {
    let threshold_days = (crate::constants::CALL_THRESHOLD / 86_400) as usize;
    // Oldest day of a breach run that is exactly threshold-long.
    let mut oldest_breach = timestamp_to_date_key(T_NOW);
    for _ in 1..threshold_days {
        oldest_breach = previous_date_key(oldest_breach);
    }
    let midnight = date_key_to_utc_timestamp(oldest_breach);

    // A second past midnight loses the day and falls one breach short.
    for (issued_at, expected) in [(midnight, true), (midnight + 1, false)] {
        with_storage(|storage| {
            GemContract::new(storage.clone())
                .config_profile
                .write(crate::config::PROFILE_PROD)
                .unwrap();
            let mut p = sample_params(ALICE);
            p.issued_at = issued_at;
            let gem_id = api::add_gem(storage, p).unwrap();
            let threshold = api::get_gem(storage, gem_id)
                .unwrap()
                .unwrap()
                .call_price_minor;
            let window = breach_window(T_NOW, threshold + U256::from(1u64), threshold_days);

            let mut gem = GemContract::new(storage.clone());
            assert_eq!(
                gem.trigger_bucket_call(&window, bucket_of(storage, gem_id), T_NOW)
                    .unwrap(),
                expected,
                "issued at {issued_at}"
            );
        });
    }
}

#[test]
fn call_skips_below_threshold() {
    with_storage(|storage| {
        let gem_id = mature_gem(storage);
        let threshold = api::get_gem(storage, gem_id)
            .unwrap()
            .unwrap()
            .call_price_minor;
        // One below the threshold: not enough breach-days to force a call.
        let breach_days = (crate::constants::CALL_THRESHOLD / 86_400) as usize - 1;
        let window = breach_window(T_NOW, threshold + U256::from(1u64), breach_days);

        let mut gem = GemContract::new(storage.clone());
        assert!(!gem
            .trigger_bucket_call(&window, bucket_of(storage, gem_id), T_NOW)
            .unwrap());
        assert_eq!(
            api::get_gem(storage, gem_id).unwrap().unwrap().state,
            GemState::Issued as u8
        );
    });
}

#[test]
fn a_registry_edit_does_not_move_the_cursor_onto_another_currency() {
    let currencies = [840u16, 978u16];
    assert_eq!(
        crate::hooks::currency_position(&currencies, 978),
        1,
        "the cursor names a currency, not a slot"
    );
    assert_eq!(
        crate::hooks::currency_position(&currencies[1..], 978),
        0,
        "dropping the currency ahead of it does not shift the cursor onto a stranger"
    );
    assert_eq!(
        crate::hooks::currency_position(&currencies, 392),
        0,
        "a currency the registry no longer carries restarts at the head"
    );
}

#[test]
fn a_wider_window_than_the_live_profile_widens_the_span_the_scan_collects() {
    with_storage(|storage| {
        let gem = GemContract::new(storage.clone());
        let iso = sample_params(ALICE).reference_currency;
        let profile = |p: u8| {
            GemContract::new(storage.clone())
                .config_profile
                .write(p)
                .unwrap()
        };

        profile(crate::config::PROFILE_DEV);
        api::add_gem(storage, sample_params(ALICE)).unwrap();
        assert_eq!(
            gem.max_call_window_seconds.read(&iso).unwrap(),
            GemParams::DEV.call_window_seconds
        );

        profile(crate::config::PROFILE_PROD);
        api::add_gem(storage, sample_params(BOB)).unwrap();
        assert_eq!(
            gem.max_call_window_seconds.read(&iso).unwrap(),
            GemParams::PROD.call_window_seconds,
            "a wider profile widens the span"
        );

        profile(crate::config::PROFILE_DEV);
        let mut third = sample_params(ALICE);
        third.promis_load_minor = U256::from(2_000_000u64);
        api::add_gem(storage, third).unwrap();
        assert_eq!(
            gem.max_call_window_seconds.read(&iso).unwrap(),
            GemParams::PROD.call_window_seconds,
            "going back to the narrow one does not shrink it"
        );
    });
}

#[test]
fn config_unset_resolves_by_chain_id() {
    with_storage(|storage| {
        // No genesis profile selected -> resolved by network; the test chain is not mainnet.
        assert_eq!(crate::config::read(storage).unwrap(), GemParams::DEV);
        // An explicit selector still wins over the network default.
        GemContract::new(storage.clone())
            .config_profile
            .write(crate::config::PROFILE_PROD)
            .unwrap();
        assert_eq!(crate::config::read(storage).unwrap(), GemParams::PROD);
        assert_eq!(GemParams::PROD.call_window_seconds, 28 * 24 * 3600);
        assert_eq!(GemParams::PROD.position_validity, 365 * 24 * 3600);
    });
}

/// The network default: only mainnet runs the real timings.
#[test]
fn config_auto_profile_follows_the_network() {
    use outbe_primitives::chain::{DEVNET_CHAIN_ID, MAINNET_CHAIN_ID, TESTNET_CHAIN_ID};

    assert_eq!(GemParams::for_chain_id(MAINNET_CHAIN_ID), GemParams::PROD);
    for chain_id in [TESTNET_CHAIN_ID, DEVNET_CHAIN_ID, 31_337] {
        assert_eq!(GemParams::for_chain_id(chain_id), GemParams::DEV);
    }
}

#[test]
fn config_unknown_selector_errors() {
    with_storage(|storage| {
        GemContract::new(storage.clone())
            .config_profile
            .write(99u8)
            .unwrap();
        assert!(crate::config::read(storage).is_err());
    });
}

/// Pin the selector slot index: the seeder writes a raw slot, and `gem_items`
/// spans a 16-slot record, so the attribute order is not the slot.
#[test]
fn config_profile_slot_matches_seeder_layout() {
    with_storage(|storage| {
        assert_eq!(
            GemContract::new(storage.clone()).config_profile.slot(),
            U256::from(34)
        );
    });
}

#[test]
fn a_bucket_that_outlives_its_hour_is_retired_rather_than_left_in_front() {
    with_storage(|storage| {
        let gem_id = mature_gem(storage);
        let gem = GemContract::new(storage.clone());
        call_gem(storage, gem_id, T_NOW);
        let deadline = T_NOW + 7 * 86_400;
        let bucket = GemContract::deadline_hour(deadline);

        let entry = crate::buckets::bucket_entry(bucket_of(storage, gem_id));
        gem.called_deadline
            .write(&entry, deadline + 400 * 86_400)
            .unwrap();

        let ctx = block_ctx_at(storage, GemContract::hour_end(bucket));
        <crate::hooks::GemLifecycle as outbe_primitives::block::BlockLifecycle>::begin_block(&ctx)
            .unwrap();

        let retry = GemContract::deadline_hour(GemContract::hour_end(bucket)) + 1;
        assert_eq!(
            gem.first_expiry_day().unwrap(),
            Some(retry),
            "the bucket leaves the tree instead of blocking every later one"
        );
        assert_eq!(
            gem.called_deadline.read(&entry).unwrap(),
            deadline,
            "and its called bucket waits at its own deadline again"
        );

        let ctx = block_ctx_at(storage, GemContract::hour_end(retry));
        <crate::hooks::GemLifecycle as outbe_primitives::block::BlockLifecycle>::begin_block(&ctx)
            .unwrap();
        assert!(api::get_gem(storage, gem_id).unwrap().is_none());
    });
}

/// Calls a gem the way it was called before buckets: on its own record, queued by id.
fn call_before_buckets(storage: &StorageHandle, gem_id: U256, at: u64) {
    let mut gem = GemContract::new(storage.clone());
    gem.leave_bucket(gem_id).unwrap();
    let mut item = gem.gem_items.get(gem_id).unwrap().unwrap();
    item.state = GemState::Called as u8;
    item.called_at = at;
    gem.gem_items.update(&item).unwrap();
    gem.push_called(gem_id, at + u64::from(item.call_notice_period_seconds))
        .unwrap();
}

/// Arms a mature gem with `call`, fails its first forfeit, and checks it moved to the
/// next hour and burned there with its credit. Returns the provider for its events.
fn forfeit_fails_once(call: fn(&StorageHandle, U256, u64)) -> (HashMapStorageProvider, U256) {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(T_NOW));
    let (gem_id, load, retry) = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = mature_gem(&storage);
        let load = api::get_gem(&storage, gem_id)
            .unwrap()
            .unwrap()
            .promis_load_minor;
        call(&storage, gem_id, T_NOW);
        let hour = GemContract::deadline_hour(T_NOW + 7 * 86_400);
        // The credit overflows, so the forfeit reverts as a whole.
        outbe_promislimit::PromisLimitContract::new(storage.clone())
            .set_total_unallocated(U256::MAX)
            .unwrap();

        let now = GemContract::hour_end(hour);
        let ctx = block_ctx_at(&storage, now);
        <crate::hooks::GemLifecycle as outbe_primitives::block::BlockLifecycle>::begin_block(&ctx)
            .unwrap();

        let retry = GemContract::deadline_hour(now) + 1;
        assert_eq!(gem_state(&storage, gem_id), GemState::Called as u8);
        assert_eq!(
            GemContract::new(storage.clone())
                .first_expiry_day()
                .unwrap(),
            Some(retry)
        );
        (gem_id, load, retry)
    });

    StorageHandle::enter(&mut provider, |storage| {
        outbe_promislimit::PromisLimitContract::new(storage.clone())
            .set_total_unallocated(U256::ZERO)
            .unwrap();
        let ctx = block_ctx_at(&storage, GemContract::hour_end(retry));
        <crate::hooks::GemLifecycle as outbe_primitives::block::BlockLifecycle>::begin_block(&ctx)
            .unwrap();

        assert!(api::get_gem(&storage, gem_id).unwrap().is_none());
        assert_eq!(unallocated(&storage), load);
    });
    (provider, gem_id)
}

/// A bucket member whose forfeit fails is not lost: it moves to the next hour on its
/// own, then burns.
#[test]
fn a_bucket_member_whose_forfeit_fails_is_queued_on_its_own_and_burned_later() {
    use alloy_sol_types::SolEvent;

    let (provider, gem_id) = forfeit_fails_once(call_gem);
    let events = provider.get_events(outbe_primitives::addresses::GEM_ADDRESS);
    let deferred: Vec<U256> = events
        .iter()
        .filter_map(|log| IGem::GemExpiryDeferred::decode_log_data(log).ok())
        .map(|event| event.gemId)
        .collect();
    assert_eq!(deferred, vec![gem_id]);
    assert!(!events
        .iter()
        .any(|log| IGem::GemBucketExpiryDeferred::decode_log_data(log).is_ok()));
}

/// Unix time of the first block past the deadline of a bucket called at `T_NOW`.
fn first_due_block(storage: &StorageHandle, gem_id: U256) -> u64 {
    let notice = api::get_gem(storage, gem_id)
        .unwrap()
        .unwrap()
        .call_notice_period_seconds;
    GemContract::hour_end(GemContract::deadline_hour(T_NOW + u64::from(notice)))
}

fn begin_block_at(storage: &StorageHandle, ts: u64) {
    <crate::hooks::GemLifecycle as outbe_primitives::block::BlockLifecycle>::begin_block(
        &block_ctx_at(storage, ts),
    )
    .unwrap();
}

/// One member that cannot burn leaves its bucket; the others burn on time.
#[test]
fn a_member_that_cannot_burn_does_not_hold_back_its_bucket() {
    with_storage(|storage| {
        let gems = alice_gems(storage, &[1, 1_000]);
        call_gem(storage, gems[0], T_NOW);
        let now = first_due_block(storage, gems[0]);
        // Room for the small load's credit only.
        let mut limit = outbe_promislimit::PromisLimitContract::new(storage.clone());
        limit
            .set_total_unallocated(U256::MAX - U256::from(500u64))
            .unwrap();

        // The failing member sits on top, where the sweep starts.
        begin_block_at(storage, now);
        assert!(api::get_gem(storage, gems[0]).unwrap().is_none(), "burned");
        assert_eq!(gem_state(storage, gems[1]), GemState::Called as u8);
        assert!(bucket_of(storage, gems[1]).is_zero(), "queued on its own");

        limit.set_total_unallocated(U256::ZERO).unwrap();
        begin_block_at(
            storage,
            GemContract::hour_end(GemContract::deadline_hour(now) + 1),
        );
        assert!(api::get_gem(storage, gems[1]).unwrap().is_none());
        assert_eq!(unallocated(storage), U256::from(1_000u64));
    });
}

/// A broken bucket index reverts, so the sweep defers the bucket instead of failing
/// every block.
#[test]
fn a_broken_bucket_index_defers_the_bucket_and_keeps_blocks_going() {
    use alloy_sol_types::SolEvent;

    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(T_NOW));
    let bucket = StorageHandle::enter(&mut provider, |storage| {
        let gems = alice_gems(&storage, &[1, 2]);
        call_gem(&storage, gems[0], T_NOW);
        GemContract::new(storage.clone())
            .bucket_gem_index
            .write(&gems[1], 7)
            .unwrap();
        begin_block_at(&storage, first_due_block(&storage, gems[0]));
        assert_eq!(gem_state(&storage, gems[1]), GemState::Called as u8);
        bucket_of(&storage, gems[0])
    });
    let deferred: Vec<_> = provider
        .get_events(outbe_primitives::addresses::GEM_ADDRESS)
        .iter()
        .filter_map(|log| IGem::GemBucketExpiryDeferred::decode_log_data(log).ok())
        .map(|event| event.bucketKey)
        .collect();
    assert_eq!(deferred, vec![bucket]);
}

/// Gem ids carry no order, so a slice refreshes the whole range once, however many
/// buckets it called.
#[test]
fn a_call_slice_refreshes_all_metadata_once() {
    use alloy_sol_types::SolEvent;

    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut provider, |storage| {
        GemContract::new(storage.clone())
            .config_profile
            .write(crate::config::PROFILE_PROD)
            .unwrap();
        let pair = seed_currency(&storage, 840, Some(U256::from(600_000u64)));
        for nonce in 0..3 {
            callable_gem_of(
                &storage,
                840,
                nonce,
                T_NOW - 100 * 86_400,
                U256::from(100_000u64),
            );
        }
        let day = previous_date_key(timestamp_to_date_key(T_NOW));
        priced_window(&storage, pair, day, U256::from(300_000u64));
        assert_eq!(
            crate::hooks::scan_and_call(&block_ctx_at(&storage, T_NOW)).unwrap(),
            3
        );
    });
    let events = provider.get_events(outbe_primitives::addresses::GEM_ADDRESS);
    let batches: Vec<(U256, U256)> = events
        .iter()
        .filter_map(|log| IGem::BatchMetadataUpdate::decode_log_data(log).ok())
        .map(|event| (event._fromTokenId, event._toTokenId))
        .collect();
    assert_eq!(batches, vec![(U256::ZERO, U256::MAX)]);
    let called = events
        .iter()
        .filter(|log| IGem::GemBucketCalled::decode_log_data(log).is_ok())
        .count();
    assert_eq!(called, 3);
}

/// A gem called before buckets still drains through the queue, deferral included.
#[test]
fn a_gem_called_before_buckets_is_deferred_and_burned_later() {
    use alloy_sol_types::SolEvent;

    let (provider, gem_id) = forfeit_fails_once(call_before_buckets);
    let deferred: Vec<U256> = provider
        .get_events(outbe_primitives::addresses::GEM_ADDRESS)
        .iter()
        .filter_map(|log| IGem::GemExpiryDeferred::decode_log_data(log).ok())
        .map(|event| event.gemId)
        .collect();
    assert_eq!(deferred, vec![gem_id]);
}

/// A called bucket wider than one block's budget burns over several blocks, then
/// leaves the queue.
#[test]
fn a_called_bucket_wider_than_the_budget_burns_over_several_blocks() {
    with_storage(|storage| {
        let loads: Vec<u64> = (0..=u64::from(crate::constants::MAX_EXPIRY_STEPS_PER_BLOCK))
            .map(|n| 1_000 + n)
            .collect();
        let gems = alice_gems(storage, &loads);
        call_gem(storage, gems[0], T_NOW);
        let notice = api::get_gem(storage, gems[0])
            .unwrap()
            .unwrap()
            .call_notice_period_seconds;
        let now = GemContract::hour_end(GemContract::deadline_hour(T_NOW + u64::from(notice)));
        let begin = |ts: u64| {
            <crate::hooks::GemLifecycle as outbe_primitives::block::BlockLifecycle>::begin_block(
                &block_ctx_at(storage, ts),
            )
            .unwrap()
        };
        let live = || {
            gems.iter()
                .filter(|id| api::get_gem(storage, **id).unwrap().is_some())
                .count()
        };

        begin(now);
        assert_eq!(live(), 1, "one block burns its budget");
        begin(now + 1);
        assert_eq!(live(), 0);
        assert_eq!(
            GemContract::new(storage.clone())
                .first_expiry_day()
                .unwrap(),
            None
        );
        assert_eq!(unallocated(storage), U256::from(loads.iter().sum::<u64>()));
    });
}

/// Settling one gem of a called bucket keeps its call stamp and leaves the rest called.
#[test]
fn settling_one_gem_of_a_called_bucket_leaves_the_rest_called() {
    with_storage(|storage| {
        let gems = alice_gems(storage, &[1, 2]);
        call_gem(storage, gems[0], T_NOW);
        assert_eq!(gem_state(storage, gems[1]), GemState::Called as u8);

        api::set_state(storage, gems[0], GemState::Settled).unwrap();
        let settled = api::get_gem(storage, gems[0]).unwrap().unwrap();
        assert_eq!(settled.state, GemState::Settled as u8);
        assert_eq!(settled.called_at, T_NOW);
        assert_eq!(gem_state(storage, gems[1]), GemState::Called as u8);
        let entry = crate::buckets::bucket_entry(bucket_of(storage, gems[1]));
        assert_ne!(
            GemContract::new(storage.clone())
                .called_bucket_slot
                .read(&entry)
                .unwrap(),
            0,
            "the bucket still waits for its deadline"
        );
    });
}

/// A storage fault is this node's own: it fails the block instead of moving the gem.
#[test]
fn a_storage_fault_in_a_forfeit_fails_the_block() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(T_NOW));
    let (gem_id, now) = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = mature_gem(&storage);
        call_gem(&storage, gem_id, T_NOW);
        let now = GemContract::hour_end(GemContract::deadline_hour(T_NOW + 7 * 86_400));
        (gem_id, now)
    });

    provider.fail_mutation_at_address(outbe_primitives::addresses::PROMIS_LIMIT_ADDRESS);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = block_ctx_at(&storage, now);
        let error =
            <crate::hooks::GemLifecycle as outbe_primitives::block::BlockLifecycle>::begin_block(
                &ctx,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            outbe_primitives::error::PrecompileError::Storage(_)
        ));
        assert_eq!(gem_state(&storage, gem_id), GemState::Called as u8);
    });
}

#[test]
fn leaving_called_frees_the_expiry_slot() {
    with_storage(|storage| {
        let gem_id = mature_gem(storage);
        let mut gem = GemContract::new(storage.clone());
        call_gem(storage, gem_id, T_NOW);
        let bucket = GemContract::deadline_hour(T_NOW + 7 * 86_400);
        assert_eq!(gem.expiry_bucket_live.read(&bucket).unwrap(), 1);

        gem.set_state(gem_id, GemState::Settled).unwrap();
        assert_eq!(gem.expiry_bucket_live.read(&bucket).unwrap(), 0);
        assert_eq!(gem.first_expiry_day().unwrap(), None);
    });
}

#[test]
fn config_dev_profile_terms_a_new_gem() {
    with_storage(|storage| {
        GemContract::new(storage.clone())
            .config_profile
            .write(crate::config::PROFILE_DEV)
            .unwrap();

        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();

        // A gem snapshots its call terms at issuance, so the dev bundle has to
        // reach the record; the prod one must not.
        let item = api::get_gem(storage, gem_id).unwrap().unwrap();
        assert_eq!(item.call_window_seconds, GemParams::DEV.call_window_seconds);
        assert_eq!(
            item.call_threshold_seconds,
            GemParams::DEV.call_threshold_seconds
        );
        assert_eq!(
            item.call_notice_period_seconds,
            GemParams::DEV.call_notice_period_seconds
        );
        assert!(item.call_window_seconds < GemParams::PROD.call_window_seconds);
    });
}

/// A gem of `iso` issued at `issued_at` with its own call price; `nonce` keeps the ids apart.
fn callable_gem_of(
    storage: &StorageHandle,
    iso: u16,
    nonce: u64,
    issued_at: u64,
    call_price: U256,
) -> U256 {
    let mut p = sample_params(ALICE);
    p.promis_load_minor = U256::from(1_000_000 + u64::from(iso) * 1_000 + nonce);
    p.reference_currency = iso;
    p.issued_at = issued_at;
    // Its own price, so each gem opens its own bucket.
    p.call_price_minor = call_price + U256::from(nonce);
    api::add_gem(storage, p).unwrap()
}

/// Prices the call window back from `latest` at `vwap` and finalizes through it.
fn priced_window(storage: &StorageHandle, pair: u32, latest: u32, vwap: U256) {
    let oracle = OracleContract::new(storage.clone());
    let mut day = latest;
    for _ in 0..(crate::constants::CALL_WINDOW / 86_400) {
        oracle.record_utc_day_vwap(day, pair, vwap).unwrap();
        day = previous_date_key(day);
    }
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < latest {
        oracle.utc_day_vwap_last_finalized.write(latest).unwrap();
    }
}

/// A trigger that finds the sweep unfinished queues its day rather than restarting
/// the walk, so the day in flight still reaches every bin against its own prices.
#[test]
fn a_trigger_during_a_running_call_sweep_queues_its_day() {
    with_storage(|storage| {
        let gem = GemContract::new(storage.clone());
        gem.config_profile
            .write(crate::config::PROFILE_PROD)
            .unwrap();
        let pair = seed_currency(storage, 840, Some(U256::from(600_000u64)));
        let issued_at = T_NOW - 100 * 86_400;
        // A bin that spends the whole budget, and one gem priced above it.
        for nonce in 0..u64::from(crate::constants::MAX_BUCKET_VISITS_PER_BLOCK) {
            callable_gem_of(storage, 840, nonce, issued_at, U256::from(100_000u64));
        }
        let above = callable_gem_of(storage, 840, 999, issued_at, U256::from(200_000u64));

        let day = previous_date_key(timestamp_to_date_key(T_NOW));
        priced_window(storage, pair, day, U256::from(300_000u64));
        crate::hooks::scan_and_call(&block_ctx_at(storage, T_NOW)).unwrap();
        let cursor = gem.bucket_scan_cursor.read(&840).unwrap();
        assert_ne!(cursor, 0, "the first slice gave out inside the range");

        let next_ts = T_NOW + 86_400;
        let next_day = previous_date_key(timestamp_to_date_key(next_ts));
        priced_window(storage, pair, next_day, U256::from(300_000u64));
        let next = block_ctx_at(storage, next_ts);
        assert_eq!(crate::hooks::scan_and_call(&next).unwrap(), 0);
        assert_eq!(gem.call_sweep_day.read().unwrap(), day);
        assert_eq!(gem.call_pending_day.read().unwrap(), next_day);
        assert_eq!(
            gem.bucket_scan_cursor.read(&840).unwrap(),
            cursor,
            "the walk in flight was not restarted"
        );

        // The next slice finishes the old day and hands the sweep to the queued one.
        crate::hooks::run_call_slice(&next).unwrap();
        assert_eq!(
            api::get_gem(storage, above).unwrap().unwrap().state,
            GemState::Called as u8
        );
        assert_eq!(gem.call_sweep_day.read().unwrap(), next_day);
        assert_eq!(gem.call_pending_day.read().unwrap(), 0);
    });
}

/// With a day already waiting, a newer one takes its place, and the day that will
/// never be walked is named.
#[test]
fn a_newer_day_pushes_out_the_waiting_call_day_and_names_it() {
    use alloy_sol_types::SolEvent;

    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(T_NOW));
    let (in_flight, skipped) = StorageHandle::enter(&mut provider, |storage| {
        GemContract::new(storage.clone())
            .config_profile
            .write(crate::config::PROFILE_PROD)
            .unwrap();
        let pair = seed_currency(&storage, 840, Some(U256::from(600_000u64)));
        for nonce in 0..=u64::from(crate::constants::MAX_BUCKET_VISITS_PER_BLOCK) {
            callable_gem_of(
                &storage,
                840,
                nonce,
                T_NOW - 100 * 86_400,
                U256::from(100_000u64),
            );
        }
        let mut closed = Vec::new();
        for offset in 0..3u64 {
            let ts = T_NOW + offset * 86_400;
            let day = previous_date_key(timestamp_to_date_key(ts));
            priced_window(&storage, pair, day, U256::from(300_000u64));
            crate::hooks::scan_and_call(&block_ctx_at(&storage, ts)).unwrap();
            closed.push(day);
        }
        let gem = GemContract::new(storage.clone());
        assert_eq!(gem.call_sweep_day.read().unwrap(), closed[0]);
        assert_eq!(gem.call_pending_day.read().unwrap(), closed[2]);
        (closed[0], closed[1])
    });

    let events: Vec<_> = provider
        .get_events(outbe_primitives::addresses::GEM_ADDRESS)
        .iter()
        .filter_map(|log| IGem::SweepDaySkipped::decode_log_data(log).ok())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].sweep, crate::constants::CALL_SWEEP);
    assert_eq!(events[0].skippedDay, skipped);
    assert_eq!(events[0].inFlightDay, in_flight);
}

/// Each currency is walked once a sweep. Were the ones closed behind the cursor
/// walked again, two currencies each holding more undecided gems than a slice may
/// visit would keep the sweep open for good.
#[test]
fn a_call_sweep_over_several_currencies_always_ends() {
    with_storage(|storage| {
        let gem = GemContract::new(storage.clone());
        gem.config_profile
            .write(crate::config::PROFILE_PROD)
            .unwrap();
        let day = previous_date_key(timestamp_to_date_key(T_NOW));
        for iso in [840u16, 978] {
            let pair = seed_currency(storage, iso, Some(U256::from(600_000u64)));
            priced_window(storage, pair, day, U256::from(300_000u64));
            // Issued five days ago: every gem is visited, decided and left where it is.
            for nonce in 0..=u64::from(crate::constants::MAX_BUCKET_VISITS_PER_BLOCK) {
                callable_gem_of(
                    storage,
                    iso,
                    nonce,
                    T_NOW - 5 * 86_400,
                    U256::from(100_000u64),
                );
            }
        }

        let ctx = block_ctx_at(storage, T_NOW);
        crate::hooks::scan_and_call(&ctx).unwrap();
        for _ in 0..3 {
            crate::hooks::run_call_slice(&ctx).unwrap();
        }
        assert_eq!(gem.call_sweep_day.read().unwrap(), 0, "the sweep closed");
    });
}

/// An Issued gem of `iso` issued at `issued_at` with its own `floor`; `nonce` keeps
/// the ids apart.
fn issued_gem_of(
    storage: &StorageHandle,
    iso: u16,
    nonce: u64,
    issued_at: u64,
    floor: U256,
) -> U256 {
    let mut p = sample_params(ALICE);
    p.promis_load_minor = U256::from(1_000_000 + u64::from(iso) * 1_000 + nonce);
    p.reference_currency = iso;
    p.issued_at = issued_at;
    p.floor_price_minor = floor;
    api::add_gem(storage, p).unwrap()
}

fn gem_state(storage: &StorageHandle, gem_id: U256) -> u8 {
    api::get_gem(storage, gem_id).unwrap().unwrap().state
}

/// A spike inside the day that leaves the day's VWAP at the floor qualifies nothing,
/// however high and fresh the live rate stands.
#[test]
fn a_spike_the_day_price_does_not_share_qualifies_no_gem() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let floor = sample_params(ALICE).floor_price_minor;
        let index = seed_currency(storage, 840, Some(floor * U256::from(10u64)));
        OracleContract::new(storage.clone())
            .exchange_rate_timestamp
            .write(&index, QUALIFY_TS)
            .unwrap();
        close_day(storage, index, floor);

        assert!(!is_qualified(storage, gem_id));
    });
}

/// The issuance day counts only when the gem was issued at midnight: one issued a
/// second later waits for the next day's price.
#[test]
fn the_issue_day_qualifies_only_a_gem_issued_at_midnight() {
    let day = previous_date_key(timestamp_to_date_key(QUALIFY_TS));
    let midnight = date_key_to_utc_timestamp(day);
    for (issued_at, expected) in [(midnight, true), (midnight + 1, false)] {
        with_storage(|storage| {
            let floor = sample_params(ALICE).floor_price_minor;
            let gem_id = issued_gem_of(storage, 840, 0, issued_at, floor);
            seed_day_price(storage, 840, Some(floor + U256::from(1u64)));
            assert_eq!(
                is_qualified(storage, gem_id),
                expected,
                "issued at {issued_at}"
            );
        });
    }
}
#[test]
fn token_by_index_reads_the_live_gem_list() {
    with_storage(|storage| {
        let burned = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let kept = api::add_gem(storage, sample_params(BOB)).unwrap();
        api::set_state(storage, burned, GemState::Settled).unwrap();
        api::burn(storage, burned).unwrap();

        let token_at = |index: u64| {
            let data = IGem::tokenByIndexCall {
                index: U256::from(index),
            }
            .abi_encode();
            dispatch(storage.clone(), &data, Address::ZERO, U256::ZERO)
        };
        let out = token_at(0).unwrap();
        assert_eq!(
            IGem::tokenByIndexCall::abi_decode_returns(&out).unwrap(),
            kept
        );
        let err = token_at(1).unwrap_err();
        assert!(
            format!("{err:?}").contains("index out of bounds"),
            "{err:?}"
        );
    });
}

#[test]
fn precompile_safe_transfer_with_data_reverts() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let data = IGem::safeTransferFrom_1Call {
            from: ALICE,
            to: BOB,
            gemId: gem_id,
            data: Default::default(),
        }
        .abi_encode();
        let err = dispatch(storage.clone(), &data, ALICE, U256::ZERO).unwrap_err();
        assert!(format!("{err:?}").contains("non-transferable"), "{err:?}");
    });
}

#[test]
fn metadata_update_marks_each_lifecycle_transition() {
    use alloy_sol_types::SolEvent;

    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(T_NOW));
    let gem_id = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = api::add_gem(&storage, sample_params(ALICE)).unwrap();
        call_gem(&storage, gem_id, T_NOW);
        api::set_state(&storage, gem_id, GemState::Settled).unwrap();
        api::burn(&storage, gem_id).unwrap();
        gem_id
    });

    let updates: Vec<U256> = provider
        .get_events(outbe_primitives::addresses::GEM_ADDRESS)
        .iter()
        .filter_map(|log| IGem::MetadataUpdate::decode_log_data(log).ok())
        .map(|event| event._tokenId)
        .collect();
    assert_eq!(updates, vec![gem_id], "settled; qualifying writes nothing");
}

#[test]
fn transfer_logs_announce_mint_and_burn() {
    use alloy_sol_types::SolEvent;

    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(T_NOW));
    let gem_id = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = api::add_gem(&storage, sample_params(ALICE)).unwrap();
        api::set_state(&storage, gem_id, GemState::Settled).unwrap();
        api::burn(&storage, gem_id).unwrap();
        gem_id
    });

    let transfers: Vec<(Address, Address, U256)> = provider
        .get_events(outbe_primitives::addresses::GEM_ADDRESS)
        .iter()
        .filter_map(|log| IGem::Transfer::decode_log_data(log).ok())
        .map(|event| (event.from, event.to, event.tokenId))
        .collect();
    assert_eq!(
        transfers,
        vec![
            (Address::ZERO, ALICE, gem_id),
            (ALICE, Address::ZERO, gem_id)
        ]
    );
}

#[test]
fn supported_interfaces_match_the_implemented_selectors() {
    use alloy_sol_types::SolEvent;
    use outbe_primitives::erc::{
        ERC165_INTERFACE_ID, ERC20_INTERFACE_ID, ERC4906_INTERFACE_ID,
        ERC721_ENUMERABLE_INTERFACE_ID, ERC721_INTERFACE_ID, ERC721_METADATA_INTERFACE_ID,
    };

    let interface_id = |selectors: &[[u8; 4]]| {
        selectors.iter().fold([0u8; 4], |acc, selector| {
            std::array::from_fn(|i| acc[i] ^ selector[i])
        })
    };
    assert_eq!(
        interface_id(&[
            IGem::balanceOfCall::SELECTOR,
            IGem::ownerOfCall::SELECTOR,
            IGem::safeTransferFrom_0Call::SELECTOR,
            IGem::safeTransferFrom_1Call::SELECTOR,
            IGem::transferFromCall::SELECTOR,
            IGem::approveCall::SELECTOR,
            IGem::setApprovalForAllCall::SELECTOR,
            IGem::getApprovedCall::SELECTOR,
            IGem::isApprovedForAllCall::SELECTOR,
        ]),
        ERC721_INTERFACE_ID
    );
    assert_eq!(
        interface_id(&[
            IGem::nameCall::SELECTOR,
            IGem::symbolCall::SELECTOR,
            IGem::tokenURICall::SELECTOR,
        ]),
        ERC721_METADATA_INTERFACE_ID
    );
    assert_eq!(
        interface_id(&[
            IGem::totalSupplyCall::SELECTOR,
            IGem::tokenByIndexCall::SELECTOR,
            IGem::tokenOfOwnerByIndexCall::SELECTOR,
        ]),
        ERC721_ENUMERABLE_INTERFACE_ID
    );
    assert_eq!(IGem::MetadataUpdate::SIGNATURE, "MetadataUpdate(uint256)");
    assert_eq!(
        IGem::BatchMetadataUpdate::SIGNATURE,
        "BatchMetadataUpdate(uint256,uint256)"
    );

    with_storage(|storage| {
        let supports = |id: [u8; 4]| {
            let data = IGem::supportsInterfaceCall {
                interfaceId: id.into(),
            }
            .abi_encode();
            let out = dispatch(storage.clone(), &data, Address::ZERO, U256::ZERO).unwrap();
            IGem::supportsInterfaceCall::abi_decode_returns(&out).unwrap()
        };
        for id in [
            ERC165_INTERFACE_ID,
            ERC721_INTERFACE_ID,
            ERC721_METADATA_INTERFACE_ID,
            ERC721_ENUMERABLE_INTERFACE_ID,
            ERC4906_INTERFACE_ID,
        ] {
            assert!(supports(id), "{id:02x?}");
        }
        assert!(!supports(ERC20_INTERFACE_ID));
        assert!(!supports([0xff; 4]));
    });
}

fn token_uri_parts(storage: &StorageHandle, gem_id: U256) -> (serde_json::Value, String) {
    use base64::Engine;

    let engine = base64::engine::general_purpose::STANDARD;
    let data = IGem::tokenURICall { gemId: gem_id }.abi_encode();
    let out = dispatch(storage.clone(), &data, Address::ZERO, U256::ZERO).unwrap();
    let uri = IGem::tokenURICall::abi_decode_returns(&out).unwrap();
    let json = engine
        .decode(uri.strip_prefix("data:application/json;base64,").unwrap())
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&json).unwrap();
    let svg = engine
        .decode(
            json["image"]
                .as_str()
                .unwrap()
                .strip_prefix("data:image/svg+xml;base64,")
                .unwrap(),
        )
        .unwrap();
    (json, String::from_utf8(svg).unwrap())
}

fn trait_value(json: &serde_json::Value, name: &str) -> Option<serde_json::Value> {
    json["attributes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["trait_type"] == name)
        .map(|entry| entry["value"].clone())
}

#[test]
fn token_uri_renders_the_gem_card() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        seed_day_price(storage, 840, Some(U256::from(540_001u64)));
        let (json, svg) = token_uri_parts(storage, gem_id);

        let id = outbe_common::nft_card::short_id(gem_id);
        assert_eq!(json["name"], format!("Gem {id}"));
        assert_eq!(json["description"], crate::constants::TOKEN_DESCRIPTION);
        assert!(!json.to_string().contains("https://"));
        assert_eq!(trait_value(&json, "State").unwrap(), "Qualified");
        assert_eq!(trait_value(&json, "Gem Type").unwrap(), "SRA");
        assert_eq!(trait_value(&json, "Entry Price").unwrap(), 0.5);
        assert_eq!(trait_value(&json, "Floor Price").unwrap(), 0.54);
        assert_eq!(trait_value(&json, "Call Price").unwrap(), 1.14);
        assert_eq!(trait_value(&json, "Promis Load").unwrap(), 1);
        assert_eq!(trait_value(&json, "Issued At").unwrap(), T_NOW);
        assert!(trait_value(&json, "Call Deadline").is_none());

        assert!(svg.contains(">GEM</text>"));
        assert!(svg.contains(&format!(">{id}</text>")));
        assert!(svg.contains(">QUALIFIED</text>"));
        assert!(svg.contains(">Call Price</text>"));
        assert!(svg.contains(">1.14</text>"));
        assert!(!svg.contains("Floor Price"));
    });
}

#[test]
fn an_issued_card_leads_with_the_load_and_shows_the_floor_price() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let (_, svg) = token_uri_parts(storage, gem_id);

        let row = |label: &str| svg.find(&format!(">{label}</text>")).unwrap();
        assert!(row("Promis Load") < row("Entry Price"));
        assert!(row("Entry Price") < row("Floor Price"));
        assert!(row("Floor Price") < row("Call Price"));
    });
}

#[test]
fn token_uri_stays_called_past_the_call_deadline() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(T_NOW));
    let (gem_id, deadline) = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = api::add_gem(&storage, sample_params(ALICE)).unwrap();
        call_gem(&storage, gem_id, T_NOW);
        let item = api::get_gem(&storage, gem_id).unwrap().unwrap();
        (gem_id, T_NOW + u64::from(item.call_notice_period_seconds))
    });

    provider.set_timestamp(U256::from(deadline));
    StorageHandle::enter(&mut provider, |storage| {
        let (json, svg) = token_uri_parts(&storage, gem_id);
        assert_eq!(trait_value(&json, "State").unwrap(), "Called");
        assert_eq!(trait_value(&json, "Called At").unwrap(), T_NOW);
        assert_eq!(trait_value(&json, "Call Deadline").unwrap(), deadline);
        assert!(svg.contains(">CALLED</text>"));
        assert!(svg.contains(&outbe_common::nft_card::timestamp_utc(deadline)));
    });

    provider.set_timestamp(U256::from(deadline + 1));
    StorageHandle::enter(&mut provider, |storage| {
        let (json, svg) = token_uri_parts(&storage, gem_id);
        assert_eq!(trait_value(&json, "State").unwrap(), "Called");
        assert!(svg.contains(">CALLED</text>"));
    });
}
