//! Unit tests for the vaultprovider precompile.
//!
//! Cross-contract interaction is exercised through `HashMapStorageProvider`'s
//! sub-call stubs: `stub_sub_call_at(target, bytes)` pins a target's return
//! payload and `enable_sub_call_stub()` makes every other sub-call succeed with
//! empty returndata (matching the convention in `outbe_credisfactory::tests`).

use alloy_primitives::{address, Address, Bytes, U256};
use alloy_sol_types::SolCall;

use outbe_primitives::addresses::VAULT_PROVIDER_ADDRESS;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::precompile::{dispatch, IVaultProvider};
use crate::runtime;
use crate::schema::VaultProviderContract;

const CHAIN_ID: u64 = 1;

fn owner() -> Address {
    address!("0x0000000000000000000000000000000000000a11")
}
fn stranger() -> Address {
    address!("0x0000000000000000000000000000000000000b0b")
}
fn source_account() -> Address {
    address!("0x0000000000000000000000000000000000000111")
}
fn target_account() -> Address {
    address!("0x0000000000000000000000000000000000000222")
}
fn asset() -> Address {
    address!("0x0000000000000000000000000000000000000888")
}
fn vault() -> Address {
    address!("0x0000000000000000000000000000000000000777")
}
fn receiver() -> Address {
    address!("0x0000000000000000000000000000000000000999")
}

/// ABI encoding of a single `uint256`/`address` return: the 32-byte big-endian word.
fn word(value: U256) -> Bytes {
    Bytes::from(value.to_be_bytes::<32>().to_vec())
}

fn set_owner(storage: &StorageHandle<'_>, who: Address) {
    VaultProviderContract::new(storage.clone())
        .owner
        .write(who)
        .unwrap();
}

// --- ownership ---------------------------------------------------------------

#[test]
fn owner_view_returns_seeded_owner() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        set_owner(&storage, owner());
        let out = dispatch(
            storage.clone(),
            &IVaultProvider::ownerCall {}.abi_encode(),
            stranger(),
            U256::ZERO,
        )
        .unwrap();
        let got = IVaultProvider::ownerCall::abi_decode_returns(&out).unwrap();
        assert_eq!(got, owner());
    });
}

#[test]
fn management_methods_reject_non_owner() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        set_owner(&storage, owner());
        // onlyOwner is checked before any sub-call, so no stubs are needed.
        let err = runtime::add_liquidity_source(storage.clone(), stranger(), source_account(), 1)
            .unwrap_err();
        assert!(err.to_string().contains("unauthorized"), "{err}");

        let err = runtime::add_vault(storage.clone(), stranger(), vault()).unwrap_err();
        assert!(err.to_string().contains("unauthorized"), "{err}");
    });
}

// --- liquidity sources / targets --------------------------------------------

#[test]
fn add_remove_liquidity_source_enumerates_and_round_trips_type() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        set_owner(&storage, owner());

        // NodCostPrice == 1.
        runtime::add_liquidity_source(storage.clone(), owner(), source_account(), 1).unwrap();

        let out = dispatch(
            storage.clone(),
            &IVaultProvider::liquiditySourcesCountCall {}.abi_encode(),
            stranger(),
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IVaultProvider::liquiditySourcesCountCall::abi_decode_returns(&out).unwrap(),
            U256::from(1)
        );

        let out = dispatch(
            storage.clone(),
            &IVaultProvider::liquiditySourceAtCall { index: U256::ZERO }.abi_encode(),
            stranger(),
            U256::ZERO,
        )
        .unwrap();
        let got = IVaultProvider::liquiditySourceAtCall::abi_decode_returns(&out).unwrap();
        assert_eq!(got.sourceAddress, source_account());
        assert_eq!(got.sourceType as u8, 1);

        // Removal clears it.
        runtime::remove_liquidity_source(storage.clone(), owner(), source_account()).unwrap();
        assert_eq!(
            VaultProviderContract::new(storage.clone())
                .liquidity_sources
                .len()
                .unwrap(),
            0
        );
    });
}

#[test]
fn add_liquidity_source_rejects_unknown_type_and_remove_rejects_missing() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        set_owner(&storage, owner());

        let err = runtime::add_liquidity_source(storage.clone(), owner(), source_account(), 0)
            .unwrap_err();
        assert!(
            err.to_string().contains("invalid liquidity source"),
            "{err}"
        );

        let err = runtime::remove_liquidity_source(storage.clone(), owner(), source_account())
            .unwrap_err();
        assert!(
            err.to_string().contains("liquidity source not found"),
            "{err}"
        );
    });
}

#[test]
fn add_remove_liquidity_target_enumerates() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        set_owner(&storage, owner());

        // Credis == 1.
        runtime::add_liquidity_target(storage.clone(), owner(), target_account(), 1).unwrap();
        let out = dispatch(
            storage.clone(),
            &IVaultProvider::liquidityTargetAtCall { index: U256::ZERO }.abi_encode(),
            stranger(),
            U256::ZERO,
        )
        .unwrap();
        let got = IVaultProvider::liquidityTargetAtCall::abi_decode_returns(&out).unwrap();
        assert_eq!(got.targetAddress, target_account());
        assert_eq!(got.targetType as u8, 1);

        runtime::remove_liquidity_target(storage.clone(), owner(), target_account()).unwrap();
        assert_eq!(
            VaultProviderContract::new(storage.clone())
                .liquidity_targets
                .len()
                .unwrap(),
            0
        );
    });
}

// --- vault management --------------------------------------------------------

#[test]
fn add_vault_registers_asset_and_vault_then_remove() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    // vault.asset() resolves to `asset()`; the approve sub-call to `asset()`
    // succeeds via the generic stub.
    storage.stub_sub_call_at(vault(), word(U256::from_be_bytes(asset().into_word().0)));
    storage.enable_sub_call_stub();
    StorageHandle::enter(&mut storage, |storage| {
        set_owner(&storage, owner());

        runtime::add_vault(storage.clone(), owner(), vault()).unwrap();

        let contract = VaultProviderContract::new(storage.clone());
        assert_eq!(contract.assets.len().unwrap(), 1);
        assert_eq!(contract.assets.at(0).unwrap(), Some(asset()));
        assert_eq!(contract.asset_vault_set(asset()).len().unwrap(), 1);
        assert_eq!(
            contract.asset_vault_set(asset()).at(0).unwrap(),
            Some(vault())
        );

        // Duplicate registration reverts.
        let err = runtime::add_vault(storage.clone(), owner(), vault()).unwrap_err();
        assert!(err.to_string().contains("already added"), "{err}");

        // Remove drops both the vault and its (now-empty) asset.
        runtime::remove_vault(storage.clone(), owner(), vault()).unwrap();
        let contract = VaultProviderContract::new(storage.clone());
        assert_eq!(contract.asset_vault_set(asset()).len().unwrap(), 0);
        assert_eq!(contract.assets.len().unwrap(), 0);
    });
}

// --- liquidity flow ----------------------------------------------------------

#[test]
fn deposit_liquidity_happy_path_and_rejects_unknown_source() {
    let shares = U256::from(123u64);
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    // vault.deposit(...) returns `shares`; transferFrom on `asset` succeeds generically.
    storage.stub_sub_call_at(vault(), word(shares));
    storage.enable_sub_call_stub();
    StorageHandle::enter(&mut storage, |storage| {
        set_owner(&storage, owner());

        // An Unknown source discriminant is rejected before any sub-call.
        let err = runtime::deposit_liquidity(
            storage.clone(),
            source_account(),
            asset(),
            U256::from(10),
            IVaultProvider::LiquiditySource::Unknown,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("invalid liquidity source"),
            "{err}"
        );

        // Register a vault for the asset (seed the set directly to avoid the
        // vault.asset() stub colliding with vault.deposit()), then deposit
        // declaring a valid source.
        let contract = VaultProviderContract::new(storage.clone());
        contract.asset_vault_set(asset()).insert(vault()).unwrap();
        contract.assets.insert(asset()).unwrap();

        let got = runtime::deposit_liquidity(
            storage.clone(),
            source_account(),
            asset(),
            U256::from(10),
            IVaultProvider::LiquiditySource::NodCostPrice,
        )
        .unwrap();
        assert_eq!(got, shares);
    });
}

#[test]
fn deposit_liquidity_reverts_when_no_vault_configured() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enable_sub_call_stub();
    StorageHandle::enter(&mut storage, |storage| {
        set_owner(&storage, owner());
        let err = runtime::deposit_liquidity(
            storage.clone(),
            source_account(),
            asset(),
            U256::from(10),
            IVaultProvider::LiquiditySource::NodCostPrice,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("reserve vault not configured"),
            "{err}"
        );
    });
}

#[test]
fn withdraw_liquidity_happy_path_and_rejects_unknown_target() {
    let x = U256::from(50u64);
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    // previewWithdraw / balanceOf / withdraw all target `vault` and return `x`:
    // required == available, burned == x.
    storage.stub_sub_call_at(vault(), word(x));
    storage.enable_sub_call_stub();
    StorageHandle::enter(&mut storage, |storage| {
        set_owner(&storage, owner());

        // Zero receiver rejected first.
        let err = runtime::withdraw_liquidity(
            storage.clone(),
            target_account(),
            asset(),
            U256::from(10),
            Address::ZERO,
            IVaultProvider::LiquidityTarget::Credis,
        )
        .unwrap_err();
        assert!(err.to_string().contains("zero address"), "{err}");

        // An Unknown target discriminant is rejected before sub-calls.
        let err = runtime::withdraw_liquidity(
            storage.clone(),
            target_account(),
            asset(),
            U256::from(10),
            receiver(),
            IVaultProvider::LiquidityTarget::Unknown,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("invalid liquidity target"),
            "{err}"
        );

        // Register a vault, then withdraw declaring a valid target.
        VaultProviderContract::new(storage.clone())
            .asset_vault_set(asset())
            .insert(vault())
            .unwrap();

        let burned = runtime::withdraw_liquidity(
            storage.clone(),
            target_account(),
            asset(),
            U256::from(10),
            receiver(),
            IVaultProvider::LiquidityTarget::Credis,
        )
        .unwrap();
        assert_eq!(burned, x);
    });
}

// --- ABI-path registry gating ------------------------------------------------

#[test]
fn abi_deposit_liquidity_gates_msg_sender_against_registry() {
    let shares = U256::from(123u64);
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.stub_sub_call_at(vault(), word(shares));
    storage.enable_sub_call_stub();
    StorageHandle::enter(&mut storage, |storage| {
        set_owner(&storage, owner());
        // Register a vault for the asset so the deposit can resolve one.
        let contract = VaultProviderContract::new(storage.clone());
        contract.asset_vault_set(asset()).insert(vault()).unwrap();
        contract.assets.insert(asset()).unwrap();

        let calldata = IVaultProvider::depositLiquidityCall {
            asset: asset(),
            assetsAmount: U256::from(10),
        }
        .abi_encode();

        // Unregistered caller resolves to Unknown -> rejected.
        let err = dispatch(storage.clone(), &calldata, source_account(), U256::ZERO).unwrap_err();
        assert!(
            err.to_string().contains("invalid liquidity source"),
            "{err}"
        );

        // Register the caller as a source, then the same ABI call succeeds and
        // the precompile resolves the discriminant from the registry.
        runtime::add_liquidity_source(storage.clone(), owner(), source_account(), 1).unwrap();
        let out = dispatch(storage.clone(), &calldata, source_account(), U256::ZERO).unwrap();
        assert_eq!(
            IVaultProvider::depositLiquidityCall::abi_decode_returns(&out).unwrap(),
            shares
        );
    });
}

#[test]
fn abi_withdraw_liquidity_gates_msg_sender_against_registry() {
    let x = U256::from(50u64);
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.stub_sub_call_at(vault(), word(x));
    storage.enable_sub_call_stub();
    StorageHandle::enter(&mut storage, |storage| {
        set_owner(&storage, owner());
        VaultProviderContract::new(storage.clone())
            .asset_vault_set(asset())
            .insert(vault())
            .unwrap();

        let calldata = IVaultProvider::withdrawLiquidityCall {
            asset: asset(),
            amount: U256::from(10),
            receiver: receiver(),
        }
        .abi_encode();

        // Unregistered caller resolves to Unknown -> rejected.
        let err = dispatch(storage.clone(), &calldata, target_account(), U256::ZERO).unwrap_err();
        assert!(
            err.to_string().contains("invalid liquidity target"),
            "{err}"
        );

        // Register the caller as a target, then the ABI call succeeds.
        runtime::add_liquidity_target(storage.clone(), owner(), target_account(), 1).unwrap();
        let out = dispatch(storage.clone(), &calldata, target_account(), U256::ZERO).unwrap();
        assert_eq!(
            IVaultProvider::withdrawLiquidityCall::abi_decode_returns(&out).unwrap(),
            x
        );
    });
}

// --- gate hooks --------------------------------------------------------------

#[test]
fn gate_hooks_authorize_only_the_provider() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        for selector in [
            IVaultProvider::canReceiveSharesCall {
                account: VAULT_PROVIDER_ADDRESS,
            }
            .abi_encode(),
            IVaultProvider::canSendSharesCall {
                account: VAULT_PROVIDER_ADDRESS,
            }
            .abi_encode(),
            IVaultProvider::canReceiveAssetsCall {
                account: VAULT_PROVIDER_ADDRESS,
            }
            .abi_encode(),
            IVaultProvider::canSendAssetsCall {
                account: VAULT_PROVIDER_ADDRESS,
            }
            .abi_encode(),
        ] {
            let out = dispatch(storage.clone(), &selector, stranger(), U256::ZERO).unwrap();
            assert_eq!(
                IVaultProvider::canReceiveSharesCall::abi_decode_returns(&out).unwrap(),
                true
            );
        }

        // A non-provider account is not authorized.
        let out = dispatch(
            storage.clone(),
            &IVaultProvider::canReceiveSharesCall {
                account: stranger(),
            }
            .abi_encode(),
            stranger(),
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IVaultProvider::canReceiveSharesCall::abi_decode_returns(&out).unwrap(),
            false
        );
    });
}
