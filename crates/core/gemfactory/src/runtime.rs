use alloy_primitives::{Address, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_gem::{api as gem_api, GemAddParams, GemState};
use outbe_oracle::contract::OracleContract;
use outbe_primitives::addresses::GEM_FACTORY_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::SCALE_1E18;

use outbe_common::pow;

use crate::constants::{FLOOR_MARKUP_PERCENT, RESERVE_VAULT, SRA_COEFFICIENT_PERCENT};
use crate::errors::GemFactoryError;
use crate::events::{GemBurned, GemIssued, GemSettled};
use crate::schema::{GemFactoryContract, GemTypes};
use crate::sol_ext::{IVaultProvider, IERC20};

pub fn mint_gem(
    storage: &StorageHandle<'_>,
    owner: Address,
    gem_type: GemTypes,
    gem_load: U256,
    issuance_currency: u16,
    reference_currency: u16,
) -> Result<U256> {
    if owner.is_zero() {
        return Err(GemFactoryError::InvalidOwner.into());
    }
    // TODO(merchant): wire up Merchant flow per the Merchant ADR.
    if matches!(gem_type, GemTypes::Merchant) {
        return Err(GemFactoryError::MerchantDeferred.into());
    }

    // TODO(multi-currency): only supports gems where issuance and reference
    // currencies are the same. Cross-rate resolution (e.g. issuance=EUR with
    // reference=USD) needs a chained oracle lookup that's not wired yet
    outbe_oracle::api::check_reference_currency_with_storage(storage.clone(), reference_currency)?;

    // Resolves both the issuance-currency registration check and the COEN/<iso>
    // rate in a single oracle lookup.
    let coen_rate = read_oracle_rate(storage, issuance_currency)?;
    let issued_at = storage.timestamp()?.to::<u64>();
    let (cost_amount, floor_price, initial_state) = compute_params(gem_type, gem_load, coen_rate)?;
    let entry_price = coen_rate;

    let params = GemAddParams {
        owner,
        gem_type: gem_type as u8,
        gem_load,
        entry_price,
        cost_amount,
        floor_price,
        issuance_currency,
        reference_currency,
        initial_state,
        issued_at,
    };
    let gem_id = gem_api::add_gem(storage, params)?;

    let factory = GemFactoryContract::new(storage.clone());
    let prev_total = factory.total_gems_issued.read()?;
    let new_total = prev_total
        .checked_add(U256::from(1))
        .ok_or(GemFactoryError::Overflow)?;
    factory.total_gems_issued.write(new_total)?;

    emit_event(
        storage,
        GemIssued {
            gemId: gem_id,
            gemType: gem_type as u8,
            owner,
            gemLoad: gem_load,
            entryPrice: entry_price,
            costAmount: cost_amount,
            floorPrice: floor_price,
            issuedAt: issued_at,
        },
    )?;

    Ok(gem_id)
}

pub fn settle_gem(storage: &StorageHandle<'_>, caller: Address, gem_id: U256) -> Result<()> {
    let item = gem_api::get_gem(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    if item.owner != caller {
        return Err(GemFactoryError::NotGemOwner.into());
    }
    if item.state != GemState::Qualified as u8 {
        return Err(GemFactoryError::InvalidState.into());
    }

    gem_api::set_state(storage, gem_id, GemState::Settled)?;

    if !item.cost_amount.is_zero() {
        deposit_to_vault(storage, caller, item.cost_amount)?;
    }

    emit_event(
        storage,
        GemSettled {
            gemId: gem_id,
            owner: caller,
            amountPaid: item.cost_amount,
            issuanceCurrency: item.issuance_currency,
        },
    )?;

    Ok(())
}

fn deposit_to_vault(storage: &StorageHandle<'_>, caller: Address, amount: U256) -> Result<()> {
    // Resolve the stablecoin asset address dynamically by querying the
    // VaultProvider's `assetAt(0)`. v1 assumes a single registered asset.
    let asset = read_reserve_asset(storage)?;

    let transfer = IERC20::transferFromCall {
        from: caller,
        to: GEM_FACTORY_ADDRESS,
        amount,
    }
    .abi_encode();
    storage.call(asset, U256::ZERO, transfer.into())?;

    let approve = IERC20::approveCall {
        spender: RESERVE_VAULT,
        amount,
    }
    .abi_encode();
    storage.call(asset, U256::ZERO, approve.into())?;

    let deposit = IVaultProvider::depositLiquidityCall {
        asset,
        assetsAmount: amount,
    }
    .abi_encode();
    storage.call(RESERVE_VAULT, U256::ZERO, deposit.into())?;

    Ok(())
}

/// Calls `IVaultProvider.assetAt(0)` on the configured `RESERVE_VAULT` and
/// returns the resolved stablecoin asset. Reverts with `InvalidAsset` if the
/// vault returns the zero address (mis-configured registry).
fn read_reserve_asset(storage: &StorageHandle<'_>) -> Result<Address> {
    let call = IVaultProvider::assetAtCall { index: U256::ZERO }.abi_encode();
    let ret = storage.staticcall(RESERVE_VAULT, call.into())?;
    let asset = IVaultProvider::assetAtCall::abi_decode_returns(&ret).map_err(|_| {
        outbe_primitives::error::PrecompileError::Revert(
            "vault assetAt(0) returned undecodable address".into(),
        )
    })?;
    if asset.is_zero() {
        return Err(GemFactoryError::InvalidAsset.into());
    }
    Ok(asset)
}

pub fn mine_gem_promis(
    storage: &StorageHandle<'_>,
    caller: Address,
    gem_id: U256,
    nonce: U256,
) -> Result<U256> {
    let item = gem_api::get_gem(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    if item.owner != caller {
        return Err(GemFactoryError::NotGemOwner.into());
    }
    if item.state != GemState::Settled as u8 {
        return Err(GemFactoryError::InvalidState.into());
    }

    validate_pow(gem_id, nonce)?;

    gem_api::burn(storage, gem_id)?;

    outbe_promisfactory::api::mine(storage.clone(), caller, item.gem_load)?;

    emit_event(
        storage,
        GemBurned {
            gemId: gem_id,
            owner: caller,
            gemLoad: item.gem_load,
        },
    )?;

    Ok(item.gem_load)
}

/// Looks up the COEN/`issuance_currency` rate via Oracle's
/// `settlement_iso_to_pair` registry. Reverts with
/// `IssuanceCurrencyNotRegistered` if the ISO code has no pair mapping, or
/// `OracleUnavailable` if the pair exists but no rate has been published.
fn read_oracle_rate(storage: &StorageHandle<'_>, issuance_currency: u16) -> Result<U256> {
    let oracle = OracleContract::new(storage.clone());
    let pair_hash = oracle.settlement_iso_to_pair.read(&issuance_currency)?;
    if pair_hash.is_zero() {
        return Err(GemFactoryError::IssuanceCurrencyNotRegistered {
            iso_code: issuance_currency,
        }
        .into());
    }
    let rate = oracle.exchange_rate.read(&pair_hash)?;
    if rate.is_zero() {
        return Err(GemFactoryError::OracleUnavailable.into());
    }
    Ok(rate)
}

fn compute_params(
    gem_type: GemTypes,
    gem_load: U256,
    coen_rate: U256,
) -> Result<(U256, U256, GemState)> {
    let (cost_amount, floor_price, initial_state) = match gem_type {
        // Genesis: validator gem during the genesis window — born Qualified
        // (no maturity wait), but validators pay like every other agent
        // class: cost = entry × load, floor = rate × 1.08. settleGem moves
        // `cost_amount` into the Reserve vault just like Wallet/Cca/Sra.
        GemTypes::Genesis => {
            let cost = compute_cost(coen_rate, gem_load, 100)?;
            let floor = floor_with_markup(coen_rate)?;
            (cost, floor, GemState::Qualified)
        }
        GemTypes::Sra => {
            let cost = compute_cost(coen_rate, gem_load, SRA_COEFFICIENT_PERCENT)?;
            let floor = floor_with_markup(coen_rate)?;
            (cost, floor, GemState::Issued)
        }
        // Validator (post-genesis), Wallet, Cca — standard agent-class flow:
        // cost = entry × load, floor = rate × 1.08, born Issued.
        GemTypes::Validator | GemTypes::Wallet | GemTypes::Cca => {
            let cost = compute_cost(coen_rate, gem_load, 100)?;
            let floor = floor_with_markup(coen_rate)?;
            (cost, floor, GemState::Issued)
        }
        GemTypes::Merchant => return Err(GemFactoryError::MerchantDeferred.into()),
    };
    Ok((cost_amount, floor_price, initial_state))
}

/// `(entry × load × percent / 100) / SCALE_1E18`. Both `entry` and `load` are
/// 1e18-scaled minor units; result stays in the same scale.
fn compute_cost(entry: U256, load: U256, percent: u64) -> Result<U256> {
    let acc = entry
        .checked_mul(load)
        .ok_or(GemFactoryError::Overflow)?
        .checked_mul(U256::from(percent))
        .ok_or(GemFactoryError::Overflow)?;
    Ok(acc / U256::from(100u64) / SCALE_1E18)
}

fn floor_with_markup(coen_rate: U256) -> Result<U256> {
    let acc = coen_rate
        .checked_mul(U256::from(FLOOR_MARKUP_PERCENT))
        .ok_or(GemFactoryError::Overflow)?;
    Ok(acc / U256::from(100u64))
}

fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(GEM_FACTORY_ADDRESS, event.encode_log_data())
}

/// PoW gate for `mine_gem_promis`, delegating to the shared
/// [`outbe_common::pow`] scheme and mapping failures onto [`GemFactoryError`].
pub fn validate_pow(gem_id: U256, nonce: U256) -> Result<()> {
    pow::validate_pow(gem_id, nonce).map_err(|e| GemFactoryError::from(e).into())
}
