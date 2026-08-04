use alloy_primitives::{address, Address, B256, U256};
use outbe_gem::{api as gem_api, GemContract, GemState};
use outbe_oracle::contract::OracleContract;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::SCALE_1E18;
use outbe_promisfactory::api::ModifyAuth;
use outbe_tee::protocol::PromisOp;
use outbe_tee_enclave::promis::{decrypt_balance, derive_modify_key, derive_view_key, modify_mac};

use crate::constants::POSITION_VALIDITY_SECONDS;
use crate::runtime;
use crate::schema::{GemFactoryContract, GemPosition, GemTypes};

const T_NOW: u64 = 1_700_000_000;
const ALICE: Address = address!("0x1111111111111111111111111111111111111111");
const BOB: Address = address!("0x2222222222222222222222222222222222222222");
/// Mock settlement stablecoin passed to `settle_gem` in tests; `isoCode()`
/// stubbed to 840 (USD), matching the default test currency.
const STABLE: Address = address!("0x00000000000000000000000000000000000000AA");
/// Mock stablecoin whose `isoCode()` is 978 (EUR): a currency mismatch for a
/// USD-denominated gem.
const STABLE_EUR: Address = address!("0x00000000000000000000000000000000000000BB");

/// A no-op authorization for mine paths that reject before reaching the (enclave)
/// Promis mint (ownership/state/PoW failures).
fn no_auth() -> ModifyAuth {
    ModifyAuth {
        mac: [0u8; 32],
        op_nonce: 0,
    }
}

/// The Promis modify authorization for `account`'s first mint of `amount`. Requires
/// the in-process Promis enclave to be installed (chain id 1 matches the harness).
fn promis_auth(account: Address, amount: U256, nonce: u64) -> ModifyAuth {
    let sk = outbe_promis::enclave_client::test_enclave::state_key();
    let mk = derive_modify_key(&sk, account).unwrap();
    ModifyAuth {
        mac: modify_mac(
            &mk,
            account,
            PromisOp::Mint,
            amount,
            nonce,
            B256::from(U256::from(1u64)),
        ),
        op_nonce: nonce,
    }
}

/// Units the stubbed `parkForGems` reports as burned (its `uint256` return).
const PARK_UNITS: u64 = 100;

fn with_storage<R>(rate_1e18: Option<U256>, f: impl FnOnce(&StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    // Stub IntexNFT1155: `parkForGems` returns PARK_UNITS (32-byte uint256).
    storage.stub_sub_call_at(
        outbe_primitives::addresses::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::from(U256::from(PARK_UNITS).to_be_bytes::<32>().to_vec()),
    );
    // Stub the settlement stablecoins' `isoCode()` (uint16 in a 32-byte word).
    storage.stub_sub_call_at(
        STABLE,
        alloy_primitives::Bytes::from(U256::from(840u64).to_be_bytes::<32>().to_vec()),
    );
    storage.stub_sub_call_at(
        STABLE_EUR,
        alloy_primitives::Bytes::from(U256::from(978u64).to_be_bytes::<32>().to_vec()),
    );
    StorageHandle::enter(&mut storage, |handle| {
        if let Some(rate) = rate_1e18 {
            let mut oracle = OracleContract::new(handle.clone());
            oracle.register_pair("COEN", "0xUSD").unwrap();
            oracle
                .set_exchange_rate(Address::ZERO, "COEN", "0xUSD", rate, 0, 0)
                .unwrap();
            // Register ISO 840 (USD) so mint_gem currency-validation passes.
            let pair_hash = OracleContract::pair_hash("COEN", "0xUSD");
            oracle
                .settlement_iso_to_pair
                .write(&840u16, pair_hash)
                .unwrap();
            oracle.reference_currencies.push(840u16).unwrap();
        }
        f(&handle)
    })
}

fn one_e18() -> U256 {
    SCALE_1E18
}

fn err_msg<T>(r: outbe_primitives::error::Result<T>) -> String {
    format!("{:?}", r.err().unwrap())
}

/// Brute-force the lowest nonce that satisfies `validate_pow(gem_id, _)` for
/// the current `POW_DIFFICULTY`. With difficulty=1 the expected loop length
/// is ~256 iterations.
fn find_valid_nonce(gem_id: U256) -> U256 {
    for n in 0u64..u64::MAX {
        let nonce = U256::from(n);
        if runtime::validate_pow(gem_id, nonce).is_ok() {
            return nonce;
        }
    }
    panic!("no valid nonce found")
}

#[test]
fn mint_genesis_pays_like_agents_but_born_qualified() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let load = U256::from(10u64) * one_e18();
        let gem_id = runtime::mint_gem(storage, ALICE, GemTypes::Genesis, load, 840, 840).unwrap();

        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        // Genesis now pays like Wallet/Cca/Validator: cost = entry × load,
        // floor = rate × 1.08. It only keeps the born-Qualified fast path
        // (no maturity wait) — settle still moves cost into the Reserve.
        assert_eq!(item.cost_amount_minor, U256::from(20u64) * one_e18());
        assert_eq!(item.entry_price_minor, rate);
        assert_eq!(
            item.floor_price_minor,
            rate * U256::from(108u64) / U256::from(100u64)
        );
        assert_eq!(item.state, GemState::Qualified as u8);
        assert_eq!(item.gem_type, GemTypes::Genesis as u8);

        let factory = GemFactoryContract::new(storage.clone());
        assert_eq!(factory.total_gems_issued.read().unwrap(), U256::from(1u64));
    });
}

#[test]
fn mint_validator_post_genesis_behaves_like_wallet() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let load = U256::from(5u64) * one_e18();
        let gem_id =
            runtime::mint_gem(storage, ALICE, GemTypes::Validator, load, 840, 840).unwrap();

        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        // Same as WALLET: cost = entry × load, floor with 8% markup, Issued.
        assert_eq!(item.cost_amount_minor, U256::from(10u64) * one_e18());
        assert_eq!(
            item.floor_price_minor,
            rate * U256::from(108u64) / U256::from(100u64)
        );
        assert_eq!(item.state, GemState::Issued as u8);
        assert_eq!(item.gem_type, GemTypes::Validator as u8);
    });
}

#[test]
fn mint_wallet_cost_and_floor_markup_state_issued() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let load = U256::from(5u64) * one_e18();
        let gem_id = runtime::mint_gem(storage, ALICE, GemTypes::Wallet, load, 840, 840).unwrap();

        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        // entry = coen_rate = 2; cost = entry * load / SCALE_1E18 = 2 * 5 = 10
        assert_eq!(item.entry_price_minor, rate);
        assert_eq!(item.cost_amount_minor, U256::from(10u64) * one_e18());
        // floor = rate * 108 / 100 = 2 * 1.08 = 2.16
        assert_eq!(
            item.floor_price_minor,
            rate * U256::from(108u64) / U256::from(100u64)
        );
        assert_eq!(item.state, GemState::Issued as u8);
    });
}

#[test]
fn mint_sra_applies_64_percent_discount() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let load = U256::from(10u64) * one_e18();
        let gem_id = runtime::mint_gem(storage, ALICE, GemTypes::Sra, load, 840, 840).unwrap();

        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        // entry = rate = 2; cost = 2 * 10 * 64 / 100 = 12.8 (1e18-scaled)
        let expected = rate * load * U256::from(64u64) / U256::from(100u64) / one_e18();
        assert_eq!(item.cost_amount_minor, expected);
    });
}

#[test]
fn mint_cca_no_discount() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let load = U256::from(7u64) * one_e18();
        let gem_id = runtime::mint_gem(storage, ALICE, GemTypes::Cca, load, 840, 840).unwrap();

        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        // entry = rate = 2; cost = 2 * 7 = 14
        assert_eq!(item.cost_amount_minor, U256::from(14u64) * one_e18());
    });
}

#[test]
fn mint_gem_rejects_merchant_type() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let res = runtime::mint_gem(
            storage,
            ALICE,
            GemTypes::Merchant,
            U256::from(1u64) * one_e18(),
            840,
            840,
        );
        assert!(err_msg(res).contains("unsupported gem type"));
    });
}

#[test]
fn mint_zero_owner_rejected() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let res = runtime::mint_gem(
            storage,
            Address::ZERO,
            GemTypes::Wallet,
            U256::from(1u64) * one_e18(),
            840,
            840,
        );
        assert!(err_msg(res).contains("invalid owner"));
    });
}

#[test]
fn mint_no_oracle_setup_rejected() {
    // Without `seed_oracle`, neither the reference-currency list nor the
    // settlement-iso-to-pair mapping is populated, so the first validation
    // (reference_currency) must revert before we get to rate resolution.
    with_storage(None, |storage| {
        let res = runtime::mint_gem(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(1u64) * one_e18(),
            840,
            840,
        );
        assert!(err_msg(res).contains("reference currency"));
    });
}

#[test]
fn settle_wallet_reverts_without_deployed_vault() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let gem_id = runtime::mint_gem(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * one_e18(),
            840,
            840,
        )
        .unwrap();
        gem_api::set_state(storage, gem_id, GemState::Qualified).unwrap();

        // STABLE's isoCode (840) matches the gem's settlement currency, so the
        // currency check passes and settle proceeds to the vault deposit. The
        // VaultRouter is not stubbed, so that call fails — proving the deposit
        // path is wired. Real vault interaction is covered by integration tests.
        let res = runtime::settle_gem(storage, ALICE, gem_id, STABLE);
        assert!(res.is_err());
    });
}

#[test]
fn settle_rejects_wrong_settlement_currency() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let gem_id = runtime::mint_gem(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * one_e18(),
            840,
            840,
        )
        .unwrap();
        gem_api::set_state(storage, gem_id, GemState::Qualified).unwrap();
        // Paying a USD gem with a EUR (978) stablecoin must revert before any
        // vault interaction.
        let res = runtime::settle_gem(storage, ALICE, gem_id, STABLE_EUR);
        assert!(err_msg(res).contains("settlement currency"));
    });
}

#[test]
fn settle_rejects_non_owner() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let gem_id = runtime::mint_gem(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * one_e18(),
            840,
            840,
        )
        .unwrap();
        gem_api::set_state(storage, gem_id, GemState::Qualified).unwrap();
        let res = runtime::settle_gem(storage, BOB, gem_id, STABLE);
        assert!(err_msg(res).contains("not gem owner"));
    });
}

#[test]
fn settle_rejects_non_qualified_state() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let gem_id = runtime::mint_gem(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * one_e18(),
            840,
            840,
        )
        .unwrap();
        // WALLET is born Issued — settle should reject (must be Qualified).
        let res = runtime::settle_gem(storage, ALICE, gem_id, STABLE);
        assert!(err_msg(res).contains("invalid state"));
    });
}

#[test]
fn mine_gem_promis_full_genesis_flow() {
    outbe_promis::enclave_client::test_enclave::install();
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let load = U256::from(10u64) * one_e18();
        // Genesis is born Qualified. settle now carries a non-zero cost and
        // deposits into the Reserve vault, which the storage-only harness
        // can't service — force `Settled` directly so this test still covers
        // the mine → burn → Promis path. The paid settle is exercised on
        // localnet with a real Reserve (see TODO below).
        let gem_id = runtime::mint_gem(storage, ALICE, GemTypes::Genesis, load, 840, 840).unwrap();

        gem_api::set_state(storage, gem_id, GemState::Settled).unwrap();
        let nonce = find_valid_nonce(gem_id);
        let minted =
            runtime::mine_gem_promis(storage, ALICE, gem_id, nonce, promis_auth(ALICE, load, 0))
                .unwrap();
        assert_eq!(minted, load);

        let gem = GemContract::new(storage.clone());
        assert!(gem.get_gem(gem_id).unwrap().is_none());
        assert_eq!(gem.total_supply().unwrap(), 0);

        // Promis is confidential: decrypt the ciphertext balance with the view key.
        let sk = outbe_promis::enclave_client::test_enclave::state_key();
        let vk = derive_view_key(&sk, ALICE).unwrap();
        let blob = outbe_promis::api::balance_ct(storage.clone(), ALICE).unwrap();
        assert_eq!(decrypt_balance(&vk, ALICE, &blob).unwrap(), load);
    });
    outbe_promis::enclave_client::test_enclave::uninstall();
}

// TODO(reserve-config): the paid `settle_gem` path (Reserve vault deposit)
// is not exercisable in the storage-only harness for ANY gem type now that
// Genesis also carries a non-zero cost. Unit coverage forces `Settled` via
// `gem_api::set_state` to reach the mine path; the real paid settle is
// covered on localnet with a configured `RESERVE_ASSET` / `RESERVE_VAULT`.

#[test]
fn mine_gem_promis_rejects_non_settled() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let gem_id = runtime::mint_gem(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * one_e18(),
            840,
            840,
        )
        .unwrap();
        // WALLET is Issued, not Settled — mine should reject before PoW.
        let res = runtime::mine_gem_promis(storage, ALICE, gem_id, U256::ZERO, no_auth());
        assert!(err_msg(res).contains("invalid state"));
    });
}

#[test]
fn mine_gem_promis_rejects_non_owner() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let gem_id = runtime::mint_gem(
            storage,
            ALICE,
            GemTypes::Genesis,
            U256::from(10u64) * one_e18(),
            840,
            840,
        )
        .unwrap();
        // mine_gem_promis checks ownership before state, so no settle needed.
        let res = runtime::mine_gem_promis(storage, BOB, gem_id, U256::ZERO, no_auth());
        assert!(err_msg(res).contains("not gem owner"));
    });
}

#[test]
fn statistics_track_mint_count() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let base = U256::from(1u64) * one_e18();
        // `gem_id = keccak(owner ‖ amount ‖ block_number)` — vary `load`
        // per mint so the same (owner, block) pair doesn't collide.
        for i in 0..3 {
            let load = base + U256::from(i as u64);
            runtime::mint_gem(storage, ALICE, GemTypes::Wallet, load, 840, 840).unwrap();
        }
        let factory = GemFactoryContract::new(storage.clone());
        assert_eq!(factory.total_gems_issued.read().unwrap(), U256::from(3u64));
    });
}

// --- Merchant gems ---

const SOURCE_INTEX_ID: u32 = 7;

fn e18_u128() -> u128 {
    10u128.pow(18)
}

/// Whole-position capacity for a series with `promis_load` per unit: the stubbed
/// `parkForGems` burns `PARK_UNITS`, so capacity = `promis_load × PARK_UNITS`.
fn parked_capacity(promis_load: u128) -> U256 {
    U256::from(promis_load) * U256::from(PARK_UNITS)
}

/// Seed an Intex series and park the merchant's whole holding into a GemPosition
/// NFT (burn stubbed via `with_storage`). Returns the `position_id`.
fn seed_and_park(storage: &StorageHandle, entry: U256, floor: U256, promis_load: u128) -> U256 {
    outbe_intex::api::create_series(
        storage,
        outbe_intex::CreateSeriesParams {
            series_id: SOURCE_INTEX_ID,
            worldwide_day: 0,
            issued_intex_count: PARK_UNITS as u32,
            promis_load_minor: promis_load,
            entry_price_minor: entry,
            floor_price_minor: floor,
            call_price_minor: U256::ZERO,
            call_trigger: outbe_intex::IntexCallTrigger::default(),
            issued_at: T_NOW as u32,
            issuance_currency: 840,
            reference_currency: 840,
        },
    )
    .unwrap();
    runtime::mint_gem_position(storage, ALICE, SOURCE_INTEX_ID, U256::from(PARK_UNITS)).unwrap()
}

#[test]
fn mint_gem_position_burns_parks_and_mints_nft() {
    with_storage(None, |storage| {
        let id = seed_and_park(storage, one_e18(), one_e18(), e18_u128());
        let capacity = parked_capacity(e18_u128());

        let factory = GemFactoryContract::new(storage.clone());
        let rec = factory.positions.get(id).unwrap().unwrap();
        assert_eq!(rec.merchant, ALICE);
        assert_eq!(rec.source_intex_id, SOURCE_INTEX_ID);
        assert_eq!(rec.remaining_capacity, capacity);
        assert_eq!(rec.source_entry_price, one_e18());
        assert_eq!(factory.total_intex_parked.read().unwrap(), capacity);

        // Position NFT minted to the merchant.
        assert_eq!(factory.owner_of(id).unwrap(), ALICE);
        assert_eq!(factory.balance_of(ALICE).unwrap(), 1);
        assert_eq!(factory.token_of_owner_by_index(ALICE, 0).unwrap(), id);
    });
}

#[test]
fn mint_gem_position_unknown_source_rejects() {
    with_storage(None, |storage| {
        let r = runtime::mint_gem_position(storage, ALICE, SOURCE_INTEX_ID, U256::from(PARK_UNITS));
        assert!(err_msg(r).contains("source intex"));
    });
}

#[test]
fn mint_merchant_gem_mints_issued_and_drains_capacity() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        // source entry below coen -> entry follows coen.
        let id = seed_and_park(storage, one_e18(), one_e18(), e18_u128());
        let capacity = parked_capacity(e18_u128());

        let load = U256::from(10u64) * one_e18();
        let gem_id = runtime::mint_merchant_gem(storage, ALICE, id, BOB, load).unwrap();

        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        assert_eq!(item.owner, BOB);
        assert_eq!(item.gem_type, GemTypes::Merchant as u8);
        assert_eq!(item.state, GemState::Issued as u8);
        assert_eq!(item.entry_price_minor, rate); // max(coen, source_entry) = coen
        assert_eq!(item.cost_amount_minor, U256::from(20u64) * one_e18()); // entry * load
        assert_eq!(item.floor_price_minor, rate * U256::from(108u64) / U256::from(100u64));
        assert_eq!(item.call_price_minor, rate * U256::from(228u64) / U256::from(100u64));

        let factory = GemFactoryContract::new(storage.clone());
        let rec = factory.positions.get(id).unwrap().unwrap();
        assert_eq!(rec.remaining_capacity, capacity - load);
        assert_eq!(factory.total_gems_issued.read().unwrap(), U256::from(1u64));
    });
}

#[test]
fn mint_merchant_gem_anchors_entry_and_floor_to_source() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        // source entry above coen, source floor above 1.08 * entry -> both dominate.
        let source_entry = U256::from(3u64) * one_e18();
        let source_floor = U256::from(5u64) * one_e18();
        let id = seed_and_park(storage, source_entry, source_floor, e18_u128());

        let gem_id = runtime::mint_merchant_gem(storage, ALICE, id, BOB, one_e18()).unwrap();
        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        assert_eq!(item.entry_price_minor, source_entry);
        assert_eq!(item.floor_price_minor, source_floor);
    });
}

#[test]
fn mint_merchant_gem_rejects_non_merchant() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let id = seed_and_park(storage, one_e18(), one_e18(), e18_u128());
        // BOB is not the position's merchant (ALICE) — must reject.
        let r = runtime::mint_merchant_gem(storage, BOB, id, BOB, one_e18());
        assert!(err_msg(r).contains("position owner"));
    });
}

#[test]
fn mint_merchant_gem_over_capacity_rejects() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        let id = seed_and_park(storage, one_e18(), one_e18(), e18_u128());
        let over = parked_capacity(e18_u128()) + U256::from(1u64);
        let r = runtime::mint_merchant_gem(storage, ALICE, id, BOB, over);
        assert!(err_msg(r).contains("capacity"));
    });
}

#[test]
fn mint_merchant_gem_after_expiry_rejects() {
    let rate = U256::from(2u64) * one_e18();
    with_storage(Some(rate), |storage| {
        // Craft a position whose parked_at is already past the validity window.
        let position_id = U256::from(1u64);
        let mut factory = GemFactoryContract::new(storage.clone());
        factory
            .add_position(&GemPosition {
                position_id,
                merchant: ALICE,
                source_intex_id: SOURCE_INTEX_ID,
                remaining_capacity: U256::from(100u64) * one_e18(),
                source_entry_price: one_e18(),
                source_floor_price: one_e18(),
                issuance_currency: 840,
                reference_currency: 840,
                parked_at: T_NOW - POSITION_VALIDITY_SECONDS - 1,
            })
            .unwrap();

        let r = runtime::mint_merchant_gem(storage, ALICE, position_id, BOB, one_e18());
        assert!(err_msg(r).contains("expired"));
    });
}
