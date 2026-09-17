//! Orchestration logic for the credisfactory precompile.

use alloy_primitives::{Address, U256};
use alloy_sol_types::{SolCall, SolValue};

use outbe_credis::constants::{BP_DEN, POLICY_RATE_FACTOR_BP};
use outbe_credis::{CredisContract, OpenPositionParams};
use outbe_oracle::api::get_policy_rate;
use outbe_primitives::addresses::{CREDIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::checked_protocol_to_native;

use crate::errors::CredisFactoryError;
use crate::precompile::ICredisFactory;
use crate::sol_ext::IERC20;

/// Atomically consume a private note, open the position and deliver reserved funds.
pub fn issue_credis(
    storage: StorageHandle<'_>,
    caller: Address,
    smart_account: Address,
    encrypted_use_auth: Vec<u8>,
    stake: U256,
) -> Result<(U256, U256)> {
    if smart_account.is_zero() {
        return Err(CredisFactoryError::InvalidSmartAccount.into());
    }
    if !outbe_ccaregistry::api::is_active(&storage, caller)? {
        return Err(CredisFactoryError::CcaNotActive.into());
    }
    if storage.with_account_info(smart_account, |info| Ok(info.is_empty_code_hash()))? {
        return Err(CredisFactoryError::SmartAccountNotDeployed.into());
    }
    storage.with_checkpoint(|| {
        let outcome = outbe_gratis::api::use_note(&storage, smart_account, encrypted_use_auth)?;
        let terms = outcome
            .terms
            .ok_or_else(|| PrecompileError::Fatal("ledger omitted issue terms".into()))?;
        let required = checked_protocol_to_native(terms.gratis_minor)
            .ok_or_else(|| PrecompileError::Revert("native COEN stake overflow".into()))?;
        if stake != required {
            return Err(CredisFactoryError::CcaStakeMismatch.into());
        }
        let policy_rate = policy_rate_for(storage.clone(), terms.issuance_currency)?;
        // Header timestamps are u64; never truncate a malformed provider value.
        let now = storage
            .timestamp()?
            .try_into()
            .map_err(|_| PrecompileError::Revert("invalid issue timestamp".into()))?;
        let id = CredisContract::new(storage.clone()).open_position(OpenPositionParams {
            credis_id: U256::from_be_bytes(outcome.credis_id.0),
            smart_account,
            cca: caller,
            collateral_handle: outcome.collateral_handle,
            asset: terms.asset,
            issuance_currency: terms.issuance_currency,
            reference_currency: terms.reference_currency,
            policy_rate,
            principal: terms.principal_minor,
            entry_price: terms.entry_price_minor,
            collateral: terms.gratis_minor,
            originated_at: now,
        })?;
        storage.transfer_balance(CREDIS_FACTORY_ADDRESS, smart_account, stake)?;
        outbe_vaultrouter::api::release_reservation(
            &storage,
            outcome.reservation_id,
            terms.asset,
            terms.principal_minor,
            smart_account,
        )?;
        storage.emit_event(
            CREDIS_FACTORY_ADDRESS,
            alloy_sol_types::SolEvent::encode_log_data(&ICredisFactory::CredisIssued {
                credisId: id,
                smartAccount: smart_account,
                cca: caller,
                amount: terms.principal_minor,
            }),
        )?;
        Ok((id, terms.principal_minor))
    })
}

/// The currency's official annual policy rate, scaled by the policy-rate factor.
fn policy_rate_for(storage: StorageHandle<'_>, issuance_currency: u16) -> Result<U256> {
    let official = get_policy_rate(storage, issuance_currency)?;
    official
        .checked_mul(U256::from(POLICY_RATE_FACTOR_BP))
        .map(|v| v / U256::from(BP_DEN))
        .ok_or_else(|| PrecompileError::Revert("credis policy rate overflow".into()))
}

// ---------------------------------------------------------------------------
// settle
// ---------------------------------------------------------------------------

/// Repay interest first, then principal, and release the corresponding collateral
/// to its original private owner. Any payer may settle; the opaque allocation
/// handle cannot redirect the released Gratis. Returns `(principal, interest)`.
pub fn settle(
    storage: StorageHandle<'_>,
    caller: Address,
    position_id: U256,
    amount: U256,
) -> Result<(U256, U256)> {
    storage.with_checkpoint(|| settle_inner(storage.clone(), caller, position_id, amount))
}

fn settle_inner(
    storage: StorageHandle<'_>,
    caller: Address,
    position_id: U256,
    amount: U256,
) -> Result<(U256, U256)> {
    if amount.is_zero() {
        return Err(CredisFactoryError::InvalidAmount.into());
    }
    let collateral_handle = {
        let position = CredisContract::new(storage.clone()).get_position(position_id)?;
        if position.asset.is_zero() {
            return Err(CredisFactoryError::InvalidAsset.into());
        }
        position.collateral_handle
    };

    let current_time = storage.timestamp()?.to::<u64>();
    let mut credis = CredisContract::new(storage.clone());
    let settlement = credis.settle(position_id, amount, current_time)?;

    // ERC20 + vault sequence, moving only what the position consumed. Sub-call reverts
    // propagate out and unwind the bookkeeping via the surrounding precompile frame.
    let paid = settlement.total_paid;
    let asset = settlement.asset;

    if !paid.is_zero() {
        // 1) Pull stablecoin from caller into the credisfactory precompile address.
        let transfer = IERC20::transferFromCall {
            from: caller,
            to: CREDIS_FACTORY_ADDRESS,
            amount: paid,
        }
        .abi_encode();
        check_token_result(&storage.call(asset, U256::ZERO, transfer.into())?)?;

        // 2) Approve the vault to spend that exact amount.
        check_token_result(
            &storage.call(
                asset,
                U256::ZERO,
                IERC20::approveCall {
                    spender: VAULT_ROUTER_ADDRESS,
                    amount: U256::ZERO,
                }
                .abi_encode()
                .into(),
            )?,
        )?;
        let approve = IERC20::approveCall {
            spender: VAULT_ROUTER_ADDRESS,
            amount: paid,
        }
        .abi_encode();
        check_token_result(&storage.call(asset, U256::ZERO, approve.into())?)?;

        // 3) Vault pulls and deposits into the reserve vault via its Solidity ABI.
        outbe_vaultrouter::api::deposit(&storage, asset, paid)?;
        check_token_result(
            &storage.call(
                asset,
                U256::ZERO,
                IERC20::approveCall {
                    spender: VAULT_ROUTER_ADDRESS,
                    amount: U256::ZERO,
                }
                .abi_encode()
                .into(),
            )?,
        )?;
    }

    // 4) Release the collateral share freed by this settlement from the pledger's own
    //    pledged ledger back to its liquid Gratis balance.
    if !settlement.gratis_released.is_zero() {
        outbe_gratis::api::release_collateral(
            &storage,
            collateral_handle,
            settlement.gratis_released,
        )?;
    }

    Ok((settlement.principal_paid, settlement.interest))
}

// ---------------------------------------------------------------------------
// void
// ---------------------------------------------------------------------------

/// Voids the remainder of a called position whose settlement window has lapsed:
/// burns the unpaid share of the pledged collateral, drops the pledger's fidelity
/// cohort by that amount, and deposits the equivalent value into the Promis Reserve.
///
/// Nothing is market-sold and nothing is collected - the written-off principal and its
/// accrued interest simply cease to exist, and the burned collateral becomes invest-side
/// capacity instead.
pub fn void_position(storage: StorageHandle<'_>, position_id: U256) -> Result<()> {
    storage.with_checkpoint(|| void_inner(storage.clone(), position_id))
}

fn void_inner(storage: StorageHandle<'_>, position_id: U256) -> Result<()> {
    let now = storage.timestamp()?.to::<u64>();
    let void = CredisContract::new(storage.clone()).void_position(position_id, now)?;

    // Rounded-up partial returns can exhaust collateral before the debt. The
    // write-off still completes, but there is no burn, Fidelity sale or credit.
    if void.gratis_burned.is_zero() {
        return Ok(());
    }

    outbe_gratis::api::forfeit_collateral(&storage, void.collateral_handle, void.gratis_burned)?;

    // The equivalent value is deposited 1:1 into the Promis Reserve.
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .add_to_total_unallocated(void.gratis_burned)?;

    Ok(())
}

fn check_token_result(bytes: &[u8]) -> Result<()> {
    if !bytes.is_empty()
        && !bool::abi_decode(bytes)
            .map_err(|_| PrecompileError::Revert("invalid ERC20 return".into()))?
    {
        return Err(PrecompileError::Revert("ERC20 operation failed".into()));
    }
    Ok(())
}
