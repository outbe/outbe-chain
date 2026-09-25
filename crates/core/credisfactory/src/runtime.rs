//! Orchestration logic for the credisfactory precompile.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;

use outbe_credis::constants::{BP_DEN, POLICY_RATE_FACTOR_BP};
use outbe_credis::{CredisContract, OpenPositionParams};
use outbe_oracle::api::{
    coen_pair_index_opt, fresh_coen_rate_for, get_policy_rate, get_utc_day_vwap,
};
use outbe_oracle::schema::OracleContract;
use outbe_primitives::addresses::{CREDIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};
use outbe_primitives::units::checked_protocol_to_native;

use crate::errors::CredisFactoryError;
use crate::precompile::ICredisFactory;
use crate::sol_ext::IReferenceCurrency;
use crate::sol_ext::IERC20;

// ---------------------------------------------------------------------------
// issue_credis
// ---------------------------------------------------------------------------

/// Consumes a confidential Gratis pledge (identified by `pledge_note` +
/// `spend_auth`, which binds it to `smart_account`), crediting the collateral into
/// the pledger's own confidential pledged ledger, opens a credis position bound to
/// `smartAccount`, stores the sealed pledger EOA on the position for the later
/// collateral release / void burn. Native COEN goes to the account; the reserved
/// stablecoin principal goes directly to the issuing CCA to cover that COEN.
///
/// The loan is not priced here: the disbursed amount, the asset, the collateral and the
/// entry price were quoted and sealed into the pledge ticket by `pledgeGratis`, so the
/// borrower gets the terms they accepted rather than whatever the oracle reads now.
///
/// The call threshold is priced here, and only it. `reference_currency` is elected at
/// this call and pinned for the position's life. The call anchor is the higher of that
/// pair's previous closed UTC-day VWAP and its current price; the call price is the
/// anchor times 1.64. Neither input is derived from the entry price. A missing previous-day
/// VWAP or a missing or stale current price rejects the issuance. The policy rate is
/// pinned here too, off the issuance currency.
///
/// The pledger EOA is never in calldata: the enclave recovers it from the ticket and
/// returns it sealed (`eoa_ct`). `caller` is the CCA and is recorded on the position -
/// authorization to spend the pledge is `spend_auth`, verified inside the enclave.
/// Returns `(position_id, amount_stables)`.
#[allow(clippy::too_many_arguments)]
pub fn issue_credis(
    storage: StorageHandle<'_>,
    caller: Address,
    smart_account: Address,
    pledge_note: B256,
    spend_auth: [u8; 32],
    reference_currency: u16,
    reservation_id: U256,
    stake: U256,
) -> Result<(U256, U256)> {
    if smart_account.is_zero() {
        return Err(CredisFactoryError::InvalidSmartAccount.into());
    }

    // Origination requires an active, fully bonded CCA.
    outbe_ccaregistry::api::require_active_cca(&storage, caller)?;

    // The loan is delivered by a call into the smart account, and a CALL to a codeless
    // account succeeds returning empty - so an undeployed account would take the loan
    // into a black hole while the position and the consumed pledge stood. Same guard
    // the vault router applies to its own receiver.
    if storage.with_account_info(smart_account, |info| Ok(info.is_empty_code_hash()))? {
        return Err(CredisFactoryError::SmartAccountNotDeployed.into());
    }

    // Block timestamp is read from the execution frame rather than threaded in
    // by the caller.
    let current_time = storage.timestamp()?.to::<u64>();

    let reservation = outbe_vaultrouter::api::reservation_of(&storage, reservation_id)?;
    if reservation.asset.is_zero() {
        return Err(CredisFactoryError::ReservationNotFound.into());
    }
    if reservation.cca != caller {
        return Err(CredisFactoryError::ReservationCcaMismatch.into());
    }
    if reservation.smart_account != smart_account {
        return Err(CredisFactoryError::ReservationAccountMismatch.into());
    }
    if current_time > reservation.expires_at {
        return Err(CredisFactoryError::ReservationExpired.into());
    }

    // Consume the pledge ticket (the enclave verifies `spend_auth` binds it to
    // `smart_account`, so a mempool copy cannot redirect the loan). The collateral
    // moves into the EOA's OWN pledged ledger and the ticket is deleted. The enclave
    // reads the pledger EOA from the ticket and returns it sealed (`eoa_ct`) so it is
    // stored on the position as ciphertext, never plaintext.
    let (terms, eoa_ct) =
        outbe_gratis::api::consume_pledge(storage.clone(), pledge_note, smart_account, spend_auth)?;
    let asset = terms.asset;
    if asset.is_zero() {
        return Err(CredisFactoryError::InvalidAsset.into());
    }
    if asset != reservation.asset {
        return Err(CredisFactoryError::ReservationAssetMismatch.into());
    }
    if terms.stables_amount > reservation.amount {
        return Err(CredisFactoryError::ReservationInsufficient.into());
    }

    // The CCA matches the borrower's six-decimal GRATIS collateral one for one in
    // value, but msg.value is native 18-decimal COEN. Checked only after
    // `consume_pledge` because the required amount is sealed in the ticket, not in
    // calldata - a caller cannot know it from the call alone, and must read it from the
    // pledge quote. Exact equality, not a floor: matching one for one is the rule, and a
    // floor would let the amount handed to the borrower drift off the collateral.
    let required_stake = checked_protocol_to_native(terms.gratis_amount)
        .ok_or_else(|| PrecompileError::Revert("native COEN stake overflow".into()))?;
    if stake != required_stake {
        return Err(CredisFactoryError::CcaStakeMismatch.into());
    }

    // Preserve authenticated pledge metadata; a changed token cannot silently
    // reinterpret the accepted principal or its six-decimal entry price.
    let issuance_currency = terms.issuance_currency;
    let ret = storage.staticcall(asset, IERC20::decimalsCall {}.abi_encode().into())?;
    let decimals = IERC20::decimalsCall::abi_decode_returns_validate(&ret)
        .map_err(|_| PrecompileError::Revert("asset decimals undecodable".into()))?;
    if read_iso_code(&storage, asset)? != issuance_currency || decimals != terms.asset_decimals {
        return Err(PrecompileError::Revert(
            "asset metadata conflicts with pledge".into(),
        ));
    }
    let policy_rate = policy_rate_for(storage.clone(), issuance_currency)?;

    outbe_oracle::api::check_reference_currency_with_storage(storage.clone(), reference_currency)?;

    // Entry price was sealed on the pledge. The call anchor is a different
    // pair: COEN in the elected reference currency, not a conversion of the entry.
    let entry_price = terms.entry_price;
    let previous_day_vwap =
        previous_closed_day_vwap(storage.clone(), reference_currency, current_time)?;
    let current_price = fresh_coen_rate_for(storage.clone(), reference_currency)?;
    let call_anchor_price = previous_day_vwap.max(current_price);

    // Open the position, storing the sealed pledger EOA so settlement and the void
    // can address the right confidential pledged ledger. Its identity depends
    // only on the CCA, destination, asset and execution block, not the pledge.
    let mut credis = CredisContract::new(storage.clone());
    let position_id = credis.open_position(OpenPositionParams {
        smart_account,
        cca: caller,
        eoa_ct,
        asset,
        issuance_currency,
        reference_currency,
        policy_rate,
        principal: terms.stables_amount,
        entry_price,
        call_anchor_price,
        collateral: terms.gratis_amount,
        issued_at: current_time,
    })?;

    // The stake is the borrower's from here on: the boundary credited it to this
    // precompile's balance, and it passes straight through to the smart account as
    // ordinary native COEN - no escrow, no claim, nothing to release or burn later.
    if !stake.is_zero() {
        storage.transfer_balance(CREDIS_FACTORY_ADDRESS, smart_account, stake)?;
    }

    // Pay the issuing CCA for COEN delivered to the user. Any unused
    // remainder goes back to the origin vault inside `releaseReservation`.
    outbe_vaultrouter::api::release_reservation(
        &storage,
        reservation_id,
        smart_account,
        terms.stables_amount,
    )?;

    storage.emit_event(
        CREDIS_FACTORY_ADDRESS,
        alloy_sol_types::SolEvent::encode_log_data(&ICredisFactory::CredisIssued {
            smartAccount: smart_account,
            cca: caller,
            amount: terms.stables_amount,
        }),
    )?;

    Ok((position_id, terms.stables_amount))
}

/// Finalized VWAP of COEN/`reference_currency` on the previous closed UTC day.
///
/// A day that is not finalized yet, and a finalized day with no price for this
/// pair, both refuse issuance. The current price is not a substitute.
fn previous_closed_day_vwap(
    storage: StorageHandle<'_>,
    currency_code: u16,
    now: u64,
) -> Result<U256> {
    let day = previous_date_key(timestamp_to_date_key(now));
    let finalized = OracleContract::new(storage.clone())
        .utc_day_vwap_last_finalized
        .read()?;
    if finalized < day {
        return Err(CredisFactoryError::PreviousDayVwapUnavailable.into());
    }
    let Some(index) = coen_pair_index_opt(storage.clone(), currency_code)? else {
        return Err(CredisFactoryError::PreviousDayVwapUnavailable.into());
    };
    match get_utc_day_vwap(storage, day, index)? {
        Some(vwap) if !vwap.is_zero() => Ok(vwap),
        _ => Err(CredisFactoryError::PreviousDayVwapUnavailable.into()),
    }
}

/// The currency's official annual policy rate, scaled by the policy-rate factor.
fn policy_rate_for(storage: StorageHandle<'_>, currency_code: u16) -> Result<U256> {
    let official = get_policy_rate(storage, currency_code)?;
    official
        .checked_mul(U256::from(POLICY_RATE_FACTOR_BP))
        .map(|v| v / U256::from(BP_DEN))
        .ok_or_else(|| PrecompileError::Revert("credis policy rate overflow".into()))
}

// ---------------------------------------------------------------------------
// settle
// ---------------------------------------------------------------------------

/// Applies `amount` to the position - accrued interest first, principal second -
/// and releases the matching share of collateral from the pledger's OWN confidential
/// pledged ledger back to its balance. When `amount` exceeds what the position still
/// needs, only the required part is pulled from the caller.
///
/// ANY caller may settle - a third party can settle someone else's position. That is
/// safe by construction rather than by an access check: the debt is pulled from
/// `caller`'s own balance, while the freed collateral goes to the pledger EOA stored
/// sealed on the position and recovered here through the enclave (`reveal_owner`). The
/// payer can therefore never redirect value to themselves, and the EOA never appears
/// on-chain. The payment (the ERC20 -> vault deposit below) is the authorization for
/// the release - no separate proof is required. Returns the stablecoin actually pulled.
/// Settles `amount` against a position and returns `(principal, interest)` - the
/// principal this payment covered and the interest it collected. Their sum is what
/// was pulled from the caller.
pub fn settle(
    storage: StorageHandle<'_>,
    caller: Address,
    position_id: U256,
    amount: U256,
) -> Result<(U256, U256)> {
    if amount.is_zero() {
        return Err(CredisFactoryError::InvalidAmount.into());
    }

    // Read-only validation pass before any mutation; recover the pledger EOA from the
    // position's sealed `eoa_ct` via a RevealOwner enclave round-trip.
    let eoa_account = {
        let credis_ro = CredisContract::new(storage.clone());
        let position = credis_ro.get_position(position_id)?;
        if position.asset.is_zero() {
            return Err(CredisFactoryError::InvalidAsset.into());
        }
        outbe_gratis::api::reveal_owner(storage.clone(), &position.eoa_ct)?
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
        storage.call(asset, U256::ZERO, transfer.into())?;

        // 2) Approve the vault to spend that exact amount.
        let approve = IERC20::approveCall {
            spender: VAULT_ROUTER_ADDRESS,
            amount: paid,
        }
        .abi_encode();
        storage.call(asset, U256::ZERO, approve.into())?;

        // 3) Vault pulls and deposits into the reserve vault via its Solidity ABI.
        outbe_vaultrouter::api::deposit(&storage, asset, paid)?;
    }

    // 4) Release the collateral share freed by this settlement from the pledger's own
    //    pledged ledger back to its liquid Gratis balance.
    if !settlement.gratis_released.is_zero() {
        outbe_gratis::api::release_to_eoa(
            storage.clone(),
            eoa_account,
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
    let now = storage.timestamp()?.to::<u64>();
    let void = CredisContract::new(storage.clone()).void_position(position_id, now)?;

    // Rounded-up partial returns can exhaust collateral before the debt. The
    // write-off still completes, but there is no burn, Fidelity sale or credit.
    if void.gratis_burned.is_zero() {
        return Ok(());
    }

    // Recover the pledger EOA from the position's sealed `eoa_ct` through the enclave so
    // the burn / fidelity drop address the right confidential ledgers (reveal once, use
    // for both).
    let eoa = outbe_gratis::api::reveal_owner(storage.clone(), &void.eoa_ct)?;

    // Burn the still-locked collateral from the pledger's own pledged ledger,
    // folding the Fidelity sale cohort into the SAME enclave round-trip (no extra
    // trip); persist the returned fidelity blob.
    let section = outbe_fidelity::api::cohort_section(
        storage.clone(),
        eoa,
        outbe_fidelity::api::FidelityCohortOp::Out,
        now,
    )?;
    let (_, outcome) = outbe_gratis::api::burn_pledged_with_fidelity(
        storage.clone(),
        eoa,
        void.gratis_burned,
        section,
    )?;
    outbe_fidelity::api::apply_fidelity_outcome(storage.clone(), eoa, &outcome)?;

    // The equivalent value is deposited 1:1 into the Promis Reserve.
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .add_to_total_unallocated(void.gratis_burned)?;

    Ok(())
}

/// Reads the disbursed asset's ISO 4217 currency code via a static
/// `IReferenceCurrency.isoCode()` sub-call. Mirrors the `staticcall` +
/// `abi_decode_returns` pattern used by intexfactory's ERC20 reads.
fn read_iso_code(storage: &StorageHandle<'_>, asset: Address) -> Result<u16> {
    let ret = storage.staticcall(
        asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    IReferenceCurrency::isoCodeCall::abi_decode_returns_validate(&ret)
        .map_err(|_| CredisFactoryError::AssetIsoUndecodable.into())
}
