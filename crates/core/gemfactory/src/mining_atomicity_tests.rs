//! Mining is one transition: a Gem that fails to mint its Promis is not burned.
//!
//! Self-contained fixture (no dependency on the main test module) so the
//! module-level `mine_promis` contract is pinned on its own.

use alloy_primitives::{address, Address, B256, U256};
use outbe_gem::{api as gem_api, GemContract, GemState};
use outbe_oracle::schema::OracleContract;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_promisfactory::api::ModifyAuth;
use outbe_tee::protocol::PromisOp;
use outbe_tee_enclave::promis::{decrypt_balance, derive_modify_key, derive_view_key, modify_mac};

use crate::runtime;
use crate::schema::GemTypes;

const T_NOW: u64 = 1_700_000_000;
const ALICE: Address = address!("0x1111111111111111111111111111111111111111");

fn six_decimal_unit() -> U256 {
    U256::from(1_000_000u64)
}

/// One settled Genesis Gem of `load` Promis owned by Alice, priced at 2 COEN/USD.
fn with_settled_gem<R>(load: U256, f: impl FnOnce(&StorageHandle<'_>, U256) -> R) -> R {
    let rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut provider, |handle| {
        GemContract::new(handle.clone())
            .config_profile
            .write(outbe_gem::config::PROFILE_PROD)
            .unwrap();
        let oracle = OracleContract::new(handle.clone());
        oracle.reference_currencies.push(840u16).unwrap();
        oracle.config_lookback_duration.write(86_400).unwrap();
        outbe_oracle::api::register_pair(handle.clone(), outbe_oracle::api::DAY_TYPE_PAIR).unwrap();
        outbe_oracle::api::set_exchange_rate(
            handle.clone(),
            Address::ZERO,
            outbe_oracle::api::DAY_TYPE_PAIR,
            rate,
            1,
            T_NOW,
        )
        .unwrap();
        let price = outbe_oracle::api::fresh_coen_rate_for(handle.clone(), 840).unwrap();
        let gem_id =
            runtime::issue_gem(&handle, ALICE, GemTypes::Genesis, load, 840, 840, price).unwrap();
        gem_api::set_state(&handle, gem_id, GemState::Settled).unwrap();
        f(&handle, gem_id)
    })
}

fn find_valid_nonce(gem_id: U256, owner: Address) -> u64 {
    (0u64..u64::MAX)
        .find(|nonce| runtime::validate_pow(gem_id, owner, *nonce).is_ok())
        .expect("a nonce clears the test difficulty")
}

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

fn promis_balance(storage: &StorageHandle<'_>, account: Address) -> U256 {
    let sk = outbe_promis::enclave_client::test_enclave::state_key();
    let vk = derive_view_key(&sk, account).unwrap();
    let blob = outbe_promis::api::balance_ct(storage.clone(), account).unwrap();
    if blob.is_empty() {
        U256::ZERO
    } else {
        decrypt_balance(&vk, account, &blob).unwrap()
    }
}

#[test]
fn a_rejected_promis_mint_leaves_the_settled_gem_and_its_nonce_untouched() {
    outbe_promis::enclave_client::test_enclave::install();
    let load = U256::from(10u64) * six_decimal_unit();
    with_settled_gem(load, |storage, gem_id| {
        let nonce = find_valid_nonce(gem_id, ALICE);
        // A modify authorization for the wrong amount: the enclave rejects the
        // mint as a business failure (not an infrastructure fault).
        let wrong = promis_auth(ALICE, load - U256::ONE, 0);
        let error = runtime::mine_promis(storage, gem_id, nonce, wrong).unwrap_err();
        assert!(
            matches!(error, outbe_primitives::error::PrecompileError::Revert(_)),
            "{error:?}"
        );

        let gem = GemContract::new(storage.clone());
        let item = gem
            .get_gem(gem_id)
            .unwrap()
            .expect("a failed mint must not burn the Gem");
        assert_eq!(item.state, GemState::Settled as u8);
        assert_eq!(gem.total_supply().unwrap(), 1);
        assert_eq!(promis_balance(storage, ALICE), U256::ZERO);
        assert_eq!(
            outbe_promis::api::op_nonce(storage.clone(), ALICE).unwrap(),
            0,
            "a rejected op consumes no ledger nonce"
        );

        // The entitlement is intact: the same nonce and a correct authorization mine it.
        let minted =
            runtime::mine_promis(storage, gem_id, nonce, promis_auth(ALICE, load, 0)).unwrap();
        assert_eq!(minted, load);
        assert!(gem.get_gem(gem_id).unwrap().is_none());
        assert_eq!(promis_balance(storage, ALICE), load);
    });
    outbe_promis::enclave_client::test_enclave::uninstall();
}
