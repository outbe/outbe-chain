//! Vault registration records, paged asset enumeration, and declared-error reverts.

use outbe_primitives::addresses::CREDIS_FACTORY_ADDRESS;
use outbe_primitives::storage::SubCallInput;

use super::*;
use crate::errors::SettlementError;

fn second_vault() -> Address {
    address!("0x0000000000000000000000000000000000000666")
}
fn third_vault() -> Address {
    address!("0x0000000000000000000000000000000000000444")
}
fn fourth_vault() -> Address {
    address!("0x0000000000000000000000000000000000000333")
}
fn second_asset() -> Address {
    address!("0x0000000000000000000000000000000000000555")
}
fn third_asset() -> Address {
    address!("0x0000000000000000000000000000000000000556")
}

/// Stubs `vault` as an ownerless vault of `asset`, and `asset` as a USD token.
fn stub_vault_of(storage: &mut HashMapStorageProvider, vault: Address, asset: Address) {
    storage.stub_sub_call_at_selector(vault, IVaultV2::assetCall::SELECTOR, word_addr(asset));
    storage.stub_sub_call_at_selector(vault, IVaultV2::ownerCall::SELECTOR, word(U256::ZERO));
    storage.stub_sub_call_at_selector(
        asset,
        IReferenceCurrency::isoCodeCall::SELECTOR,
        word(U256::from(USD_ISO_CODE)),
    );
}

/// Registers each of `vaults` through `runtime::add_vault` as the owner.
fn add_vaults(storage: &mut HashMapStorageProvider, vaults: &[Address]) {
    StorageHandle::enter(storage, |storage| {
        set_owner(&storage, owner());
        for &vault in vaults {
            runtime::add_vault(storage.clone(), owner(), vault).unwrap();
        }
    });
}

fn is_call_to(input: &SubCallInput, target: Address, selector: [u8; 4]) -> bool {
    input.target == target && input.calldata.starts_with(&selector)
}

fn asset_vaults_count(storage: &StorageHandle<'_>, asset: Address) -> U256 {
    let out = dispatch(
        storage.clone(),
        &IVaultRouter::assetVaultsCountCall { asset }.abi_encode(),
        stranger(),
        U256::ZERO,
    )
    .unwrap();
    IVaultRouter::assetVaultsCountCall::abi_decode_returns(&out).unwrap()
}

/// Checks that `removal` revoked the allowance of `vault` on `asset` with `approve(vault, 0)`.
fn assert_allowance_revoked(removal: &[SubCallInput], asset: Address, vault: Address) {
    let revoke = IERC20::approveCall {
        spender: vault,
        amount: U256::ZERO,
    }
    .abi_encode();
    assert!(
        removal
            .iter()
            .any(|input| input.target == asset && input.calldata.as_ref() == revoke.as_slice()),
        "no approve({vault}, 0) call to {asset}"
    );
}

// --- vault asset record -----------------------------------------------------

#[test]
fn vault_removable_after_asset_change() {
    // The vault first reports another asset, then an undecodable return.
    for changed_return in [word_addr(second_asset()), Bytes::new()] {
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);
        stub_vault_of(&mut storage, vault(), asset());
        storage.enable_sub_call_stub();
        add_vaults(&mut storage, &[vault()]);
        storage.stub_sub_call_at_selector(vault(), IVaultV2::assetCall::SELECTOR, changed_return);
        let before = storage.sub_call_inputs().len();

        StorageHandle::enter(&mut storage, |storage| {
            let contract = VaultRouterContract::new(storage.clone());
            assert_eq!(contract.vault_assets.read(&vault()).unwrap(), asset());

            runtime::remove_vault(storage.clone(), owner(), vault()).unwrap();

            assert_eq!(asset_vaults_count(&storage, asset()), U256::ZERO);
            assert_eq!(contract.assets.len().unwrap(), 0);
            assert_eq!(contract.vault_assets.read(&vault()).unwrap(), Address::ZERO);
        });

        let removal = &storage.sub_call_inputs()[before..];
        assert!(
            !removal
                .iter()
                .any(|input| is_call_to(input, vault(), IVaultV2::assetCall::SELECTOR)),
            "removal asked the vault for its asset"
        );
        assert!(!removal.iter().any(|input| input.target == second_asset()));
        assert_allowance_revoked(removal, asset(), vault());
    }
}

#[test]
fn remove_vault_asks_the_vault_when_no_asset_is_recorded() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    stub_vault_of(&mut storage, vault(), asset());
    storage.enable_sub_call_stub();
    add_vaults(&mut storage, &[vault()]);
    StorageHandle::enter(&mut storage, |storage| {
        // A registration written before the asset record existed.
        VaultRouterContract::new(storage.clone())
            .vault_assets
            .clear(&vault())
            .unwrap();
    });
    let before = storage.sub_call_inputs().len();

    StorageHandle::enter(&mut storage, |storage| {
        runtime::remove_vault(storage.clone(), owner(), vault()).unwrap();
        assert_eq!(asset_vaults_count(&storage, asset()), U256::ZERO);
    });

    let removal = &storage.sub_call_inputs()[before..];
    assert!(removal
        .iter()
        .any(|input| is_call_to(input, vault(), IVaultV2::assetCall::SELECTOR)));
    assert_allowance_revoked(removal, asset(), vault());
}

// --- paged reference-currency assets -----------------------------------------

fn page(storage: &StorageHandle<'_>, offset: U256, limit: U256) -> Vec<Address> {
    runtime::reference_currency_assets(storage, USD_ISO_CODE, offset, limit).unwrap()
}

#[test]
fn currency_assets_paginates() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    stub_vault_of(&mut storage, vault(), asset());
    stub_vault_of(&mut storage, second_vault(), asset());
    stub_vault_of(&mut storage, third_vault(), second_asset());
    stub_vault_of(&mut storage, fourth_vault(), third_asset());
    storage.enable_sub_call_stub();
    add_vaults(
        &mut storage,
        &[vault(), second_vault(), third_vault(), fourth_vault()],
    );

    StorageHandle::enter(&mut storage, |storage| {
        let all = vec![asset(), second_asset(), third_asset()];
        let cases = [
            (0u64, 2u64, vec![asset()]),
            (1, 2, vec![asset(), second_asset()]),
            (2, 2, vec![second_asset(), third_asset()]),
            (3, 1, vec![third_asset()]),
            (0, 10, all.clone()),
            (4, 1, vec![]),
            (10, 1, vec![]),
            (0, 0, vec![]),
        ];
        for (offset, limit, expected) in cases {
            assert_eq!(
                page(&storage, U256::from(offset), U256::from(limit)),
                expected,
                "offset={offset} limit={limit}"
            );
        }
        // Values above u32::MAX saturate instead of reverting.
        assert_eq!(page(&storage, U256::from(1), U256::MAX), all);
        assert_eq!(page(&storage, U256::MAX, U256::MAX), Vec::<Address>::new());

        let out = dispatch(
            storage.clone(),
            &IVaultRouter::referenceCurrencyAssetsCall {
                isoCode: USD_ISO_CODE,
                offset: U256::from(1),
                limit: U256::from(2),
            }
            .abi_encode(),
            stranger(),
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IVaultRouter::referenceCurrencyAssetsCall::abi_decode_returns(&out).unwrap(),
            vec![asset(), second_asset()]
        );
    });
}

#[test]
fn currency_assets_paginates_with_at_most_limit_sub_calls() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    stub_vault_of(&mut storage, vault(), asset());
    stub_vault_of(&mut storage, second_vault(), second_asset());
    // No `enable_sub_call_stub`: every sub-call to `third_vault()` fails.
    StorageHandle::enter(&mut storage, |storage| {
        for vault in [vault(), second_vault(), third_vault()] {
            crate::test_support::register_vault(&storage, asset(), vault, USD_ISO_CODE).unwrap();
        }
    });

    let before = storage.sub_call_inputs().len();
    StorageHandle::enter(&mut storage, |storage| {
        assert_eq!(
            page(&storage, U256::ZERO, U256::from(2)),
            vec![asset(), second_asset()]
        );
    });
    assert_eq!(storage.sub_call_inputs().len() - before, 2);

    StorageHandle::enter(&mut storage, |storage| {
        runtime::reference_currency_assets(&storage, USD_ISO_CODE, U256::ZERO, U256::from(3))
            .unwrap_err();
    });
}

// --- declared errors ----------------------------------------------------------

/// Checks that `err` carries the ABI encoding of `E` and decodes its fields.
fn decode_revert<E: SolError>(err: PrecompileError) -> E {
    match err {
        PrecompileError::RevertBytes(ref bytes) => {
            assert_revert::<E>(&err);
            E::abi_decode(bytes).unwrap()
        }
        other => panic!("expected {} revert, got {other:?}", E::SIGNATURE),
    }
}

#[test]
fn declared_error_insufficient_shares_for_withdraw() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.stub_sub_call_at_selector(
        vault(),
        IVaultV2::previewWithdrawCall::SELECTOR,
        word(U256::from(50)),
    );
    storage.stub_sub_call_at_selector(
        vault(),
        IERC20::balanceOfCall::SELECTOR,
        word(U256::from(10)),
    );
    storage.enable_sub_call_stub();
    StorageHandle::enter(&mut storage, |storage| {
        set_owner(&storage, owner());
        register_vault(&storage, asset(), vault());
        runtime::add_liquidity_target(storage.clone(), owner(), target_account(), 1).unwrap();

        let err = dispatch(
            storage.clone(),
            &IVaultRouter::withdrawCall {
                asset: asset(),
                amountMinor: U256::from(10),
                receiver: receiver(),
            }
            .abi_encode(),
            target_account(),
            U256::ZERO,
        )
        .unwrap_err();
        let revert: IVaultRouter::InsufficientSharesForWithdraw = decode_revert(err);
        assert_eq!(revert.availableShares, U256::from(10));
        assert_eq!(revert.requiredShares, U256::from(50));
    });
}

#[test]
fn declared_error_reserve_vault_owner_not_renounced() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    stub_vault_of(&mut storage, vault(), asset());
    storage.stub_sub_call_at_selector(
        vault(),
        IVaultV2::ownerCall::SELECTOR,
        word_addr(stranger()),
    );
    storage.enable_sub_call_stub();
    StorageHandle::enter(&mut storage, |storage| {
        set_owner(&storage, owner());
        let err = dispatch(
            storage.clone(),
            &IVaultRouter::addVaultCall { vault: vault() }.abi_encode(),
            owner(),
            U256::ZERO,
        )
        .unwrap_err();
        let revert: IVaultRouter::ReserveVaultOwnerNotRenounced = decode_revert(err);
        assert_eq!(revert.currentOwner, stranger());
    });
}

#[test]
fn declared_error_reservation_not_found() {
    with_reservable_vault(U256::from(100u64), |storage| {
        runtime::add_liquidity_target(storage.clone(), owner(), CREDIS_FACTORY_ADDRESS, 1).unwrap();
        let id = U256::from(42);
        let err = dispatch(
            storage.clone(),
            &IVaultRouter::releaseReservationCall {
                id,
                receiver: receiver(),
                amountMinor: U256::from(10),
            }
            .abi_encode(),
            CREDIS_FACTORY_ADDRESS,
            U256::ZERO,
        )
        .unwrap_err();
        let revert: IVaultRouter::ReservationNotFound = decode_revert(err);
        assert_eq!(revert.id, id);
    });
}

#[test]
fn declared_error_covers_settlement_token_failure_only() {
    let _: IVaultRouter::TokenOperationFailed =
        decode_revert(SettlementError::TokenOperationFailed.into());
    let _: IVaultRouter::TokenOperationFailed =
        decode_revert(VaultRouterError::TokenOperationFailed.into());
    let _: IVaultRouter::AmountMismatch = decode_revert(SettlementError::AmountMismatch.into());
}

#[test]
fn declared_error_absent_keeps_the_string_revert() {
    let _: IVaultRouter::ZeroAddress = decode_revert(VaultRouterError::ZeroAddress.into());
    let _: IVaultRouter::UnsupportedAssetDecimalsSimple =
        decode_revert(VaultRouterError::UnsupportedAssetDecimalsSimple.into());
}
