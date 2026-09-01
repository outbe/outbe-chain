//! The currency an Intex settles in: a reference stablecoin and the reserve
//! vault that makes it acceptable.
//!
//! `IntexFactory.settle` refuses a token the VaultRouter holds no vault for, and
//! reads the token's ISO code to decide whether it answers the series' reference
//! or issuance currency. The auction's wCOEN satisfies neither, so settlement
//! needs its own asset.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, U256};
use alloy_sol_types::sol;
use eyre::{eyre, Result};

use crate::internal::{addresses, eth};
use crate::world::forge::{self, address_from};

/// ISO 4217 numeric for USD, the reference currency every e2e series uses.
pub(crate) const USD_ISO: u16 = 840;

const REGISTRATION_TIMEOUT_SECS: u64 = 60;

sol! {
    interface ISettlementAsset {
        function decimals() external view returns (uint8);
        function isoCode() external view returns (uint16);
    }

    interface ISettlementVault {
        function owner() external view returns (address);
        function asset() external view returns (address);
    }
}

/// The stablecoin holders settle in, and the vault their payment lands in.
#[derive(Clone, Copy, Debug)]
pub struct SettlementCurrency {
    pub asset: Address,
    pub vault: Address,
}

/// Deploy the asset and its vault, then register the vault with the VaultRouter.
///
/// `owner_key` must be the router's owner - `addVault` admits nobody else. The
/// vault renounces its own owner in its constructor, which the router demands
/// before it will adopt one.
pub(crate) fn deploy(
    repo_intex: &std::path::Path,
    url: &str,
    owner_key: &str,
) -> Result<SettlementCurrency> {
    let asset = address_from(
        &forge::run_with_ctor(
            repo_intex,
            &[
                "create",
                "test/mocks/MockReferenceStablecoin.sol:MockReferenceStablecoin",
            ],
            &[&USD_ISO.to_string()],
            &[],
            url,
        )?,
        "Deployed to:",
    )?;

    let vault = address_from(
        &forge::run_with_ctor(
            repo_intex,
            &[
                "create",
                "test/mocks/MockSettlementVault.sol:MockSettlementVault",
            ],
            &[&format!("{asset:#x}")],
            &[],
            url,
        )?,
        "Deployed to:",
    )?;

    if eth::read_call(url, vault, &ISettlementVault::assetCall {}) != Some(asset) {
        return Err(eyre!("settlement vault does not hold the settlement asset"));
    }
    if eth::read_call(url, vault, &ISettlementVault::ownerCall {}) != Some(Address::ZERO) {
        return Err(eyre!("settlement vault has not renounced its owner"));
    }

    eth::send_call(
        url,
        addresses::VAULT_ROUTER_ADDR,
        owner_key,
        &eth::IVaultRouter::addVaultCall { vault },
        None,
    )?;

    // Confirm the registration itself rather than its receipt: this count is what
    // `settle` reads, and a zero here is the failure the whole fixture exists to avoid.
    let deadline = Instant::now() + Duration::from_secs(REGISTRATION_TIMEOUT_SECS);
    loop {
        let registered = eth::read_call(
            url,
            addresses::VAULT_ROUTER_ADDR,
            &eth::IVaultRouter::assetVaultsCountCall { asset },
        )
        .is_some_and(|count| !count.is_zero());
        if registered {
            return Ok(SettlementCurrency { asset, vault });
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "VaultRouter still holds no vault for the settlement asset"
            ));
        }
        sleep(Duration::from_millis(500));
    }
}

/// The vaults the router routes `asset` to - the read `settle` gates on.
pub(crate) fn registered_vaults(url: &str, asset: Address) -> Vec<Address> {
    let count = eth::read_call(
        url,
        addresses::VAULT_ROUTER_ADDR,
        &eth::IVaultRouter::assetVaultsCountCall { asset },
    )
    .unwrap_or_default();
    (0..count.to::<u64>())
        .filter_map(|index| {
            eth::read_call(
                url,
                addresses::VAULT_ROUTER_ADDR,
                &eth::IVaultRouter::assetVaultAtCall {
                    asset,
                    index: U256::from(index),
                },
            )
        })
        .collect()
}

/// The asset's ISO 4217 code, which decides whether it answers a series' currency.
pub(crate) fn iso_code(url: &str, asset: Address) -> Option<u16> {
    eth::read_call(url, asset, &ISettlementAsset::isoCodeCall {})
}

/// What the reserve vault holds of the settlement asset.
pub(crate) fn vault_balance(url: &str, asset: Address, vault: Address) -> Option<U256> {
    eth::read_call(
        url,
        asset,
        &ISettlementAssetBalance::balanceOfCall { account: vault },
    )
}

sol! {
    interface ISettlementAssetBalance {
        function balanceOf(address account) external view returns (uint256);
    }
}
