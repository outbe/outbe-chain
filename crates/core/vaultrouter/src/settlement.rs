//! Settlement pricing and payment that Nod, Gem and Intex share. The asset must have a
//! vault and carry one of the right's two currencies. The cost is floored once into
//! the asset's minor units. The payment lands in the reserve vault through the router.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_common::settlement::{settlement_cost_units, PaymentCurrency};
use outbe_oracle::api::{settlement_fx_rates, VwapSnapshotId};
use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

pub use outbe_common::settlement::SettlementCost;

pub use crate::errors::SettlementError;

use crate::api::IVaultRouter;
use crate::sol_ext::{IReferenceCurrency, IERC20};

/// The two currencies of a right: what its values are stated in, and what gates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Currencies {
    pub issuance: u16,
    pub reference: u16,
}

/// What a settlement charges in one asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quote {
    /// ISO 4217 code the payment is denominated in.
    pub settlement_currency: u16,
    pub payment_minor: U256,
    /// The VWAP snapshot an issuance-currency payment converts at.
    pub snapshot: Option<VwapSnapshotId>,
}

impl Quote {
    /// The snapshot id a payment must name; zero on the reference rail.
    pub fn snapshot_id(&self) -> U256 {
        self.snapshot.map_or(U256::ZERO, VwapSnapshotId::to_u256)
    }

    /// Rejects an issuance-rail payment authorized for any snapshot but the required one.
    pub fn require_snapshot(&self, authorized: U256) -> Result<()> {
        match self.snapshot.map(VwapSnapshotId::to_u256) {
            Some(required) if required != authorized => Err(SettlementError::SnapshotMismatch {
                authorized,
                required,
            }
            .into()),
            _ => Ok(()),
        }
    }
}

/// Prices `cost` in `asset`. The reference currency matches first, so a
/// single-currency right takes the no-rate branch.
pub fn quote(
    storage: &StorageHandle<'_>,
    asset: Address,
    currencies: Currencies,
    cost: &SettlementCost,
) -> Result<Quote> {
    let currency = accept_payment_asset(storage, asset, currencies)?;
    let asset_decimals = decimals(storage, asset)?;
    let (rate, snapshot) = currency.conversion(|| -> Result<_> {
        let fx = settlement_fx_rates(storage.clone(), currencies.issuance, currencies.reference)?
            .ok_or(SettlementError::OracleUnavailable)?;
        let rate = (
            fx.issuance_currency_vwap_minor,
            fx.reference_currency_vwap_minor,
        );
        Ok((rate, fx.snapshot))
    })?;
    let payment_minor =
        settlement_cost_units(cost, rate, asset_decimals).map_err(SettlementError::Rounding)?;
    let settlement_currency = match currency {
        PaymentCurrency::Reference => currencies.reference,
        PaymentCurrency::Issuance => currencies.issuance,
    };
    Ok(Quote {
        settlement_currency,
        payment_minor,
        snapshot,
    })
}

/// Pulls exactly `amount` of `asset` from `payer` into `factory` and deposits it into the
/// reserve vault through the router, leaving the factory's own balance untouched.
pub fn collect(
    storage: &StorageHandle<'_>,
    factory: Address,
    payer: Address,
    asset: Address,
    amount: U256,
) -> Result<()> {
    if amount.is_zero() {
        return Ok(());
    }
    let before = balance_of(storage, asset, factory)?;
    token_call(
        storage,
        asset,
        IERC20::transferFromCall {
            from: payer,
            to: factory,
            amount,
        },
    )?;
    if balance_of(storage, asset, factory)?.checked_sub(before) != Some(amount) {
        return Err(SettlementError::AmountMismatch.into());
    }
    token_call(
        storage,
        asset,
        IERC20::approveCall {
            spender: VAULT_ROUTER_ADDRESS,
            amount,
        },
    )?;
    crate::api::deposit(storage, asset, amount)?;
    if balance_of(storage, asset, factory)? != before {
        return Err(SettlementError::AmountMismatch.into());
    }
    Ok(())
}

/// Which of the right's currencies `asset` is denominated in. Registration is checked
/// first, so an unregistered asset need not implement `isoCode()` at all.
fn accept_payment_asset(
    storage: &StorageHandle<'_>,
    asset: Address,
    currencies: Currencies,
) -> Result<PaymentCurrency> {
    let ret = storage.staticcall(
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall { asset }
            .abi_encode()
            .into(),
    )?;
    let vaults = IVaultRouter::assetVaultsCountCall::abi_decode_returns(&ret)
        .map_err(|_| SettlementError::AssetVaultsCount)?;
    if vaults.is_zero() {
        return Err(SettlementError::AssetNotRegistered { asset }.into());
    }
    let ret = storage.staticcall(
        asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    let iso_code = IReferenceCurrency::isoCodeCall::abi_decode_returns(&ret)
        .map_err(|_| SettlementError::AssetIsoCode)?;
    if iso_code == currencies.reference {
        return Ok(PaymentCurrency::Reference);
    }
    if iso_code == currencies.issuance {
        return Ok(PaymentCurrency::Issuance);
    }
    Err(SettlementError::CurrencyMismatch { iso_code }.into())
}

fn decimals(storage: &StorageHandle<'_>, asset: Address) -> Result<u8> {
    let ret = storage.staticcall(asset, IERC20::decimalsCall {}.abi_encode().into())?;
    IERC20::decimalsCall::abi_decode_returns(&ret)
        .map_err(|_| SettlementError::AssetDecimals.into())
}

fn token_call(storage: &StorageHandle<'_>, asset: Address, call: impl SolCall) -> Result<()> {
    let ret = storage.call(asset, U256::ZERO, call.abi_encode().into())?;
    if !ret.is_empty() && ret.as_ref() != U256::ONE.to_be_bytes::<32>() {
        return Err(SettlementError::TokenOperationFailed.into());
    }
    Ok(())
}

fn balance_of(storage: &StorageHandle<'_>, asset: Address, account: Address) -> Result<U256> {
    let ret = storage.staticcall(asset, IERC20::balanceOfCall { account }.abi_encode().into())?;
    IERC20::balanceOfCall::abi_decode_returns(&ret)
        .map_err(|_| SettlementError::TokenOperationFailed.into())
}
