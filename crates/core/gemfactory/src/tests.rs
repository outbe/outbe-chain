use alloy_primitives::{address, Address, U256};
use outbe_gem::{api as gem_api, GemContract, GemState};
use outbe_oracle::contract::OracleContract;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::SCALE_1E18;

use crate::runtime;
use crate::schema::{GemFactoryContract, GemTypes};

const T_NOW: u64 = 1_700_000_000;
const ALICE: Address = address!("0x1111111111111111111111111111111111111111");
const BOB: Address = address!("0x2222222222222222222222222222222222222222");

fn with_storage<R>(rate_1e18: Option<U256>, f: impl FnOnce(&StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
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
        assert_eq!(item.cost_amount, U256::from(20u64) * one_e18());
        assert_eq!(item.entry_price, rate);
        assert_eq!(
            item.floor_price,
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
        assert_eq!(item.cost_amount, U256::from(10u64) * one_e18());
        assert_eq!(
            item.floor_price,
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
        assert_eq!(item.entry_price, rate);
        assert_eq!(item.cost_amount, U256::from(10u64) * one_e18());
        // floor = rate * 108 / 100 = 2 * 1.08 = 2.16
        assert_eq!(
            item.floor_price,
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
        assert_eq!(item.cost_amount, expected);
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
        assert_eq!(item.cost_amount, U256::from(14u64) * one_e18());
    });
}

#[test]
fn mint_merchant_returns_deferred() {
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
        assert!(err_msg(res).contains("merchant"));
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

        // settle_gem now staticcalls `RESERVE_VAULT.assetAt(0)` to resolve
        // the stablecoin asset. HashMapStorageProvider doesn't resolve
        // sub-call targets, so the staticcall fails — proving the integration
        // path is wired. Real vault interaction is covered by integration
        // tests once a deployed VaultProvider becomes available.
        let res = runtime::settle_gem(storage, ALICE, gem_id);
        assert!(res.is_err());
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
        let res = runtime::settle_gem(storage, BOB, gem_id);
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
        let res = runtime::settle_gem(storage, ALICE, gem_id);
        assert!(err_msg(res).contains("invalid state"));
    });
}

#[test]
fn mine_gem_promis_full_genesis_flow() {
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
        let minted = runtime::mine_gem_promis(storage, ALICE, gem_id, nonce).unwrap();
        assert_eq!(minted, load);

        let gem = GemContract::new(storage.clone());
        assert!(gem.get_gem(gem_id).unwrap().is_none());
        assert_eq!(gem.total_supply().unwrap(), 0);

        let promis = outbe_promis::Promis::new(storage.clone());
        assert_eq!(promis.balance_of(ALICE).unwrap(), load);
    });
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
        let res = runtime::mine_gem_promis(storage, ALICE, gem_id, U256::ZERO);
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
        let res = runtime::mine_gem_promis(storage, BOB, gem_id, U256::ZERO);
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
