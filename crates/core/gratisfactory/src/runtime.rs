//! Orchestration logic for the gratisfactory precompile.
//!
//! Bridges the confidential Gratis token (`outbe_gratis::api`) and the Fidelity
//! ledger. `pledge_gratis`/`unpledge_gratis` move gratis into/out of the credis
//! escrow; `mine`/`mine_coen` own the mint/burn plus Fidelity cohort bookkeeping.
//! The Fidelity cohort op rides INSIDE the gratis enclave round-trip (no extra
//! trip): `mine` folds an acquisition (`In`), `mine_coen` a sale (`Out`), and
//! `pledge_gratis` a read-only league `Probe` for the eligibility gate. The
//! factory persists the returned fidelity outcome.
//!
//! The credis loan is priced HERE, at pledge time: the pledger names the stablecoin
//! credit they want and this module derives the gratis it costs, sealing both plus
//! the asset and the rate into the ticket. `issueCredis` then reads that quote back
//! out instead of re-pricing the collateral a transaction later.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};

use crate::errors::GratisFactoryError;
use crate::precompile::IGratisFactory;
use crate::sol_ext::{IReferenceCurrency, IVaultRouter, IERC20};
use outbe_fidelity::api::FidelityCohortOp;
use outbe_gratis::api::{self as gratis, ModifyAuth, PledgeTerms};
use outbe_oracle::api::{previous_half_open_8hours_vwap, AddressPair};
use outbe_primitives::addresses::{GRATIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::math::scaled_math::checked_quote;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::checked_protocol_to_native;

/// Reads the pledged asset's ISO 4217 currency code via a static
/// `IReferenceCurrency.isoCode()` sub-call, mirroring credisfactory's
/// `read_iso_code`. The pledge is then priced against COEN/<that currency>
/// rather than a hardcoded pair, so it cannot be quoted in one currency while
/// the Credis position opens against another.
fn read_iso_code(storage: &StorageHandle<'_>, asset: Address) -> Result<u16> {
    let ret = storage.staticcall(
        asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    IReferenceCurrency::isoCodeCall::abi_decode_returns_validate(&ret)
        .map_err(|_| GratisFactoryError::AssetIsoUndecodable.into())
}

/// Validate the live asset before quoting or locking any Gratis.
fn asset_metadata(storage: &StorageHandle<'_>, asset: Address) -> Result<(u16, u8)> {
    if asset.is_zero() || storage.with_account_info(asset, |info| Ok(info.is_empty_code_hash()))? {
        return Err(GratisFactoryError::InvalidAsset.into());
    }
    let ret = storage.staticcall(
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall { asset }
            .abi_encode()
            .into(),
    )?;
    let vaults = IVaultRouter::assetVaultsCountCall::abi_decode_returns_validate(&ret)
        .map_err(|_| GratisFactoryError::ReserveVaultUnavailable)?;
    if vaults.is_zero() {
        return Err(GratisFactoryError::ReserveVaultUnavailable.into());
    }
    let iso = read_iso_code(storage, asset)?;
    let ret = storage.staticcall(asset, IERC20::decimalsCall {}.abi_encode().into())?;
    let decimals = IERC20::decimalsCall::abi_decode_returns_validate(&ret)
        .map_err(|_| GratisFactoryError::AssetDecimalsUndecodable)?;
    if decimals > 18 {
        return Err(GratisFactoryError::UnsupportedAssetDecimals.into());
    }
    Ok((iso, decimals))
}

/// Pledge the gratis that collateralizes `amount_stables` of credit in `asset` into a
/// pending pledge-lock ticket (authorized by the caller's modify key, which binds the
/// STABLES figure). The gratis cost is derived from the oracle rate and rejected if it
/// exceeds `max_gratis` - that cap is the pledger's slippage protection, authenticated
/// by their transaction signature rather than the MAC. Returns
/// `(pledge_note, gratis_cost)`; the handle is what the CCA presents at
/// `issueCredis`. The loan's own terms - the policy rate, the floor and call prices -
/// are sealed on the Credis position, not on the pledge.
pub fn pledge_gratis(
    storage: StorageHandle<'_>,
    caller: Address,
    stables_amount: U256,
    asset: Address,
    max_gratis: U256,
    auth: ModifyAuth,
) -> Result<(B256, U256)> {
    if stables_amount.is_zero() {
        return Err(GratisFactoryError::InvalidAmount.into());
    }
    let (issuance_currency, asset_decimals) = asset_metadata(&storage, asset)?;
    let block_timestamp = storage.timestamp()?.to::<u64>();
    let valuation_price = previous_half_open_8hours_vwap(
        storage.clone(),
        AddressPair::new_coen_to(issuance_currency),
        block_timestamp,
    )?
    .filter(|price| !price.is_zero())
    .ok_or(GratisFactoryError::PledgePriceUnavailable)?;
    let (gratis_amount, entry_price) =
        checked_quote(stables_amount, asset_decimals, valuation_price)?;
    let terms = PledgeTerms {
        stables_amount,
        gratis_amount,
        asset,
        entry_price,
        issuance_currency,
        asset_decimals,
        valuation_price,
    };
    pledge_priced(storage, caller, terms, max_gratis, auth)
}

/// Commit a fully quoted pledge after applying the transaction's slippage cap.
pub(super) fn pledge_priced(
    storage: StorageHandle<'_>,
    caller: Address,
    terms: PledgeTerms,
    max_gratis: U256,
    auth: ModifyAuth,
) -> Result<(B256, U256)> {
    let gratis_amount = terms.gratis_amount;
    if gratis_amount > max_gratis {
        return Err(GratisFactoryError::GratisCapExceeded.into());
    }
    // Fold a read-only league probe into the pledge round-trip (no separate
    // fidelity call): the pledge op returns the caller's current league.
    let now = storage.timestamp()?.to::<u64>();
    let section =
        outbe_fidelity::api::cohort_section(storage.clone(), caller, FidelityCohortOp::Probe, now)?;
    let (handle, outcome) =
        gratis::pledge_with_fidelity(storage, caller, terms.stables_amount, terms, auth, section)?;
    // todo implement correct fidelity eligibility check on `outcome.league`
    if outcome.league == u16::MAX {
        return Err(GratisFactoryError::FidelityNotEligible.into());
    }
    Ok((handle, gratis_amount))
}

/// Directly unpledge an unspent pledge back to `caller` (e.g. credis rejected).
/// `amount_stables` is the figure the pledge was quoted for; returns the gratis
/// collateral credited back.
pub fn unpledge_gratis(
    storage: StorageHandle<'_>,
    caller: Address,
    amount_stables: U256,
    pledge_note: B256,
    auth: ModifyAuth,
) -> Result<U256> {
    gratis::unpledge(storage, caller, amount_stables, pledge_note, auth)
}

/// Mint `amount` gratis to `account` (authorized by the account owner's modify
/// key) and record the Fidelity acquisition cohort. The `GratisMinted` event is
/// emitted by the Gratis token.
pub fn mint(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    // Fold the acquisition cohort into the gratis mint round-trip; persist the
    // returned fidelity blob.
    let now = storage.timestamp()?.to::<u64>();
    let section =
        outbe_fidelity::api::cohort_section(storage.clone(), account, FidelityCohortOp::In, now)?;
    let outcome = gratis::mint_with_fidelity(storage.clone(), account, amount, auth, section)?;
    outbe_fidelity::api::apply_fidelity_outcome(storage.clone(), account, &outcome)?;
    Ok(())
}

pub fn mine_coen(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    let native_amount = checked_protocol_to_native(amount)
        .ok_or_else(|| PrecompileError::Revert("native COEN amount overflow".into()))?;

    // Fold the sale cohort into the gratis burn round-trip; persist the returned
    // fidelity blob.
    let now = storage.timestamp()?.to::<u64>();
    let section =
        outbe_fidelity::api::cohort_section(storage.clone(), account, FidelityCohortOp::Out, now)?;
    let outcome = gratis::burn_with_fidelity(storage.clone(), account, amount, auth, section)?;
    outbe_fidelity::api::apply_fidelity_outcome(storage.clone(), account, &outcome)?;

    // GRATIS stays at six decimals; the matching native COEN exits at 18 decimals.
    storage.increase_balance(account, native_amount)?;

    storage.emit_event(
        GRATIS_FACTORY_ADDRESS,
        SolEvent::encode_log_data(&IGratisFactory::CoenMined {
            sender: account,
            amount: native_amount,
        }),
    )?;

    Ok(native_amount)
}
