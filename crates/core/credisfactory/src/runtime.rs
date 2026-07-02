//! Orchestration logic for the credisfactory precompile.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;

use outbe_credis::{AnadosisResult, CredisContract};
use outbe_gratispool::api as pool;
use outbe_gratispool::constants::DenomAmount;
use outbe_gratispool::SpendArgs;
use outbe_oracle::api::get_exchange_rate;
use outbe_primitives::addresses::{CREDIS_FACTORY_ADDRESS, VAULT_PROVIDER_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::SCALE_1E18;

use crate::errors::CredisFactoryError;
use crate::precompile::ICredisFactory;
use crate::schema::CredisFactoryContract;
use crate::sol_ext::IERC20;

/// Native token base symbol used for the COEN/USD oracle pair lookup.
pub const NATIVE_TOKEN: &str = "COEN";

/// Stable settlement quote symbol. Matches the pair used by tributefactory and
/// metadosis.
pub const STABLECOIN: &str = "0xUSD";

/// Decimal-gap factor between COEN (10^18) and stablecoin (10^6). Cosmos:
/// `decimalsDiff = sdk.NewIntWithDecimal(1, 12)`.
fn decimals_diff() -> U256 {
    U256::from(1_000_000_000_000u128)
}

// ---------------------------------------------------------------------------
// request_credis
// ---------------------------------------------------------------------------

/// Verifies a pledge-commitment spend proof through the
///   gratispool, opens a credis position bound to `bundleAccount`, persists
///   the position's `denom_id`, and delivers the stablecoin loan via the
///   vault sub-call.
/// Returns `(position_id, amount_stables)`.
pub fn request_credis(
    storage: StorageHandle<'_>,
    _caller: Address,
    asset: Address,
    bundle_account: Address,
    args: SpendArgs,
) -> Result<(U256, U256)> {
    if asset.is_zero() {
        return Err(CredisFactoryError::InvalidAsset.into());
    }
    if bundle_account.is_zero() {
        return Err(CredisFactoryError::InvalidBundleAccount.into());
    }

    // Validate the supplied denomination up front.
    let denom = DenomAmount::try_from(args.denom_id)?;
    denom
        .anadosis_denomination()
        .ok_or(CredisFactoryError::DenomNotCredisEligible)?;

    // Block timestamp is read from the execution frame rather than threaded in
    // by the caller.
    let current_time = storage.timestamp()?.to::<u64>();

    // Reject borrowers with overdue anadosis on any of their positions.
    {
        let credis = CredisContract::new(storage.clone());
        if credis.has_overdue_anadosis(bundle_account, current_time)? {
            return Err(CredisFactoryError::OverduePayments.into());
        }
    }

    // Verify the pledge proof, mark the nullifier spent, and learn the
    // gratis amount from the pool's denomination ladder. Receiver binding is
    // recomputed against `bundle_account` (the action_tag is
    // ACTION_REQUEST_CREDIS inside the pool runtime). The context nonce is
    // unused now that reclaim happens per-installment in `pay_anadosis`, so it
    // is pinned to zero; the proof still binds `bundle_account` as the target,
    // so a mempool copy cannot redirect the loan.
    let gratis_amount =
        pool::verify_and_spend_for_credis(storage.clone(), bundle_account, U256::ZERO, &args)?;

    let amount_stables = convert_gratis_to_stables(storage.clone(), gratis_amount)?;

    // Open the credis position. The `commitment` argument to
    // `create_position` is what builds the position_id; we use the proof's
    // `nullifier_hash` because it is globally unique (the pool already
    // enforces nullifier uniqueness).
    let mut credis = CredisContract::new(storage.clone());
    let position_id = credis.create_position(
        args.nullifier_hash,
        bundle_account,
        VAULT_PROVIDER_ADDRESS,
        asset,
        amount_stables,
        gratis_amount,
        current_time,
    )?;

    // Persist the position's denomination so `pay_anadosis` can derive the
    // anadosis (one-decade-down) denomination for each installment's reclaim
    // insert.
    {
        let factory = CredisFactoryContract::new(storage.clone());
        factory
            .position_denom
            .write(&position_id, denom.id() as u32)?;
    }

    // Withdraw the matching stablecoin from the vault to the borrower's smart
    // account via the vaultprovider's.
    outbe_vaultprovider::api::withdraw_liquidity(
        storage.clone(),
        CREDIS_FACTORY_ADDRESS,
        asset,
        amount_stables,
        bundle_account,
        outbe_vaultprovider::api::LiquidityTarget::Credis,
    )?;

    storage.emit_event(
        CREDIS_FACTORY_ADDRESS,
        alloy_sol_types::SolEvent::encode_log_data(&ICredisFactory::CredisRequested {
            bundleAccount: bundle_account,
            amount: amount_stables,
        }),
    )?;

    Ok((position_id, amount_stables))
}

// ---------------------------------------------------------------------------
// pay_anadosis
// ---------------------------------------------------------------------------

/// Advances the credis position by one anadosis installment and inserts the
/// caller-supplied `reclaim_commitment` into the gratispool at the anadosis
/// (one-decade-down) denomination.
///
/// The `reclaim_commitment` MUST have been computed with the **anadosis
/// denomination id** (see [`DenomAmount::anadosis_denomination`]); the runtime
/// stores it opaquely and cannot verify the preimage, so a note built against
/// the wrong denomination inserts successfully but is permanently unspendable.
pub fn pay_anadosis(
    storage: StorageHandle<'_>,
    caller: Address,
    position_id: U256,
    reclaim_commitment: U256,
) -> Result<AnadosisResult> {
    // Read-only validation pass before any mutation.
    {
        let credis_ro = CredisContract::new(storage.clone());
        let position = credis_ro.get_position(position_id)?;
        let next = credis_ro
            .get_next_anadosis(position_id)?
            .ok_or(CredisFactoryError::PositionCompleted)?;

        if position.asset.is_zero() {
            return Err(CredisFactoryError::InvalidAsset.into());
        }
        if position.vault_provider.is_zero() {
            return Err(CredisFactoryError::InvalidVaultProvider.into());
        }
        if next.anadosis_amount.is_zero() {
            return Err(CredisFactoryError::InvalidAmount.into());
        }
        if caller != position.bundle_account {
            return Err(CredisFactoryError::UnauthorizedCaller.into());
        }
        // Checked after authorization so an unauthorized caller still sees the
        // `bundleAccount` error regardless of the reclaim value.
        if reclaim_commitment.is_zero() {
            return Err(CredisFactoryError::InvalidReclaimCommitment.into());
        }
    }

    let current_time = storage.timestamp()?.to::<u64>();
    let mut credis = CredisContract::new(storage.clone());
    let result = credis.make_next_anadosis(position_id, current_time)?;

    // ERC20 + vault sequence. Sub-call reverts propagate out and unwind the
    // bookkeeping via the surrounding precompile frame.
    let amount = result.anadosis_amount;
    let asset = result.asset;
    let vault = result.vault_provider;

    // 1) Pull stablecoin from caller into the credisfactory precompile address.
    let transfer = IERC20::transferFromCall {
        from: caller,
        to: CREDIS_FACTORY_ADDRESS,
        amount,
    }
    .abi_encode();
    storage.call(asset, U256::ZERO, transfer.into())?;

    // 2) Approve the vault to spend that exact amount.
    let approve = IERC20::approveCall {
        spender: vault,
        amount,
    }
    .abi_encode();
    storage.call(asset, U256::ZERO, approve.into())?;

    // 3) Vault pulls and deposits into the reserve vault.
    outbe_vaultprovider::api::deposit_liquidity(
        storage.clone(),
        CREDIS_FACTORY_ADDRESS,
        asset,
        amount,
        outbe_vaultprovider::api::LiquiditySource::CredisAnadosis,
    )?;

    // 4) Append this installment's reclaim note so the pledger can unpledge
    //    1/10 of the collateral immediately.
    let factory = CredisFactoryContract::new(storage.clone());
    let credis_denom = factory.position_denom.read(&position_id)?;
    let denom = DenomAmount::try_from(credis_denom)?;
    let anadosis_denom = denom
        .anadosis_denomination()
        .ok_or(CredisFactoryError::DenomNotCredisEligible)?;
    pool::add_commitment(storage.clone(), anadosis_denom, reclaim_commitment)?;

    Ok(result)
}

// ---------------------------------------------------------------------------
// Oracle conversion (gratis 10^18 → stablecoin 10^6)
// ---------------------------------------------------------------------------

/// Cosmos formula: `amountStables = gratisAmount * rateInt18 / (decimalsDiff * precision)`.
fn convert_gratis_to_stables(storage: StorageHandle<'_>, gratis_amount: U256) -> Result<U256> {
    let rate = get_exchange_rate(storage, NATIVE_TOKEN, STABLECOIN)?;
    let numerator = gratis_amount
        .checked_mul(rate)
        .ok_or_else(|| -> PrecompileError {
            CredisFactoryError::OracleConversionOverflow.into()
        })?;
    let denominator =
        decimals_diff()
            .checked_mul(SCALE_1E18)
            .ok_or_else(|| -> PrecompileError {
                CredisFactoryError::OracleConversionOverflow.into()
            })?;
    Ok(numerator / denominator)
}
