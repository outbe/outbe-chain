//! Orchestration logic for the credisfactory precompile.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;

use outbe_credis::constants::{BP_DEN, POLICY_RATE_FACTOR_BP};
use outbe_credis::{CredisContract, OpenPositionParams};
use outbe_oracle::api::{coen_pair_index_opt, get_policy_rate, get_utc_day_vwap};
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

/// Consume an encrypted pledge credential and activate its per-Credis allocation.
/// Source resolution stays inside the enclave. Native COEN goes to the smart
/// account; reserved stablecoin principal goes to the issuing CCA.
///
/// The loan is not priced here: the disbursed amount, the asset, the collateral and the
/// entry price were quoted and sealed into the pledge ticket by `pledgeGratis`, so the
/// borrower gets the terms they accepted rather than whatever the oracle reads now.
///
/// The call threshold is priced here, and only it. `reference_currency` is elected at
/// this call and pinned for the position's life. The call anchor is that pair's
/// previous closed UTC-day VWAP; the call price is the anchor times 1.64.
/// It is independent of the entry price and spot price. A missing previous-day
/// VWAP rejects the issuance. The policy rate is
/// pinned here too, off the issuance currency.
///
/// The pledger EOA is never in calldata: the enclave recovers it from the ticket and
/// returns a Credis-bound allocation handle. `caller` is the recorded CCA -
/// the encrypted credential carries the spend authorization checked by the enclave.
/// Returns `(position_id, amount_stables)`.
#[allow(clippy::too_many_arguments)]
pub fn issue_credis(
    storage: StorageHandle<'_>,
    caller: Address,
    smart_account: Address,
    credential: Vec<u8>,
    reference_currency: u16,
    reservation_id: U256,
    stake: U256,
) -> Result<(U256, U256)> {
    storage.with_checkpoint(|| {
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

        // Resolve the encrypted credential and activate its allocation inside the
        // enclave. The existing Credis ID formula is independent of the note.
        let expected_id = CredisContract::position_id(
            caller,
            smart_account,
            reservation.asset,
            storage.block_number()?,
        );
        let (terms, collateral_id) = outbe_gratis::api::consume_pledge(
            storage.clone(),
            expected_id,
            credential,
            smart_account,
        )?;
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
        if read_iso_code(&storage, asset)? != issuance_currency || decimals != terms.asset_decimals
        {
            return Err(PrecompileError::Revert(
                "asset metadata conflicts with pledge".into(),
            ));
        }
        let policy_rate = policy_rate_for(storage.clone(), issuance_currency)?;

        outbe_oracle::api::check_reference_currency_with_storage(
            storage.clone(),
            reference_currency,
        )?;

        // Entry price was sealed on the pledge. The call anchor is a different
        // pair: COEN in the elected reference currency, not a conversion of the entry.
        let entry_price = terms.entry_price;
        let call_anchor_price =
            previous_closed_day_vwap(storage.clone(), reference_currency, current_time)?;

        // Store only the internal allocation handle on the position.
        let mut credis = CredisContract::new(storage.clone());
        let position_id = credis.open_position(OpenPositionParams {
            smart_account,
            cca: caller,
            collateral_id,
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
    })
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
/// `caller`'s own balance. The enclave resolves the collateral source internally.
/// Payment, Credis state and both confidential stores share one checkpoint.
/// Settles `amount` against a position and returns `(principal, interest)` - the
/// principal this payment covered and the interest it collected. Their sum is what
/// was pulled from the caller.
pub fn settle(
    storage: StorageHandle<'_>,
    caller: Address,
    position_id: U256,
    amount: U256,
) -> Result<(U256, U256)> {
    storage.with_checkpoint(|| {
        if amount.is_zero() {
            return Err(CredisFactoryError::InvalidAmount.into());
        }

        let position = CredisContract::new(storage.clone()).get_position(position_id)?;
        if position.asset.is_zero() {
            return Err(CredisFactoryError::InvalidAsset.into());
        }
        let current_time = storage.timestamp()?.to::<u64>();
        let mut credis = CredisContract::new(storage.clone());
        let settlement = credis.settle(position_id, amount, current_time)?;
        let settled_position = credis.get_position(position_id)?;

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
            let returned = storage.call(asset, U256::ZERO, transfer.into())?;
            if !returned.is_empty()
                && IERC20::transferFromCall::abi_decode_returns_validate(&returned) != Ok(true)
            {
                return Err(PrecompileError::Revert("ERC20 transfer failed".into()));
            }

            // 2) Approve the vault to spend that exact amount.
            let approve = IERC20::approveCall {
                spender: VAULT_ROUTER_ADDRESS,
                amount: paid,
            }
            .abi_encode();
            let returned = storage.call(asset, U256::ZERO, approve.into())?;
            if !returned.is_empty()
                && IERC20::approveCall::abi_decode_returns_validate(&returned) != Ok(true)
            {
                return Err(PrecompileError::Revert("ERC20 approval failed".into()));
            }

            // 3) Vault pulls and deposits into the reserve vault via its Solidity ABI.
            outbe_vaultrouter::api::deposit(&storage, asset, paid)?;
        }

        if credis.get_position(position_id)? != settled_position {
            return Err(PrecompileError::Revert(
                "Credis changed during payment".into(),
            ));
        }
        // 4) Release the collateral share freed by this settlement from the pledger's own
        //    pledged ledger back to its liquid Gratis balance.
        if !settlement.gratis_released.is_zero() {
            outbe_gratis::api::apply_collateral(
                storage.clone(),
                outbe_gratis::api::CollateralAuthorization {
                    credis_id: position_id,
                    collateral_id: position.collateral_id,
                    action: outbe_gratis::api::CollateralAction::Return,
                    amount: settlement.gratis_released,
                    expected_remaining: position.collateral_locked,
                },
                0,
            )?;
        }

        Ok((settlement.principal_paid, settlement.interest))
    })
}

// ---------------------------------------------------------------------------
// void
// ---------------------------------------------------------------------------

/// Voids the remainder of a called position whose settlement window has lapsed:
/// burns the unpaid share of the pledged collateral, drops the pledger's fidelity
/// cohort by that amount, and deposits the equivalent value into the Promis Reserve.
///
/// Nothing is market-sold and nothing is collected. The written-off principal ceases
/// to exist. Unpaid interest is left unrecorded. The burned collateral becomes
/// invest-side capacity instead.
pub fn void_position(storage: StorageHandle<'_>, position_id: U256) -> Result<()> {
    storage.with_checkpoint(|| {
        let now = storage.timestamp()?.to::<u64>();
        let void = CredisContract::new(storage.clone()).void_position(position_id, now)?;

        // Rounded-up partial returns can exhaust collateral before the debt. The
        // write-off still completes, but there is no burn, Fidelity sale or credit.
        if void.gratis_burned.is_zero() {
            return Ok(());
        }

        let anchor =
            outbe_fidelity::FidelityContract::new(storage.clone()).first_qualified_start()?;
        let burned = outbe_gratis::api::apply_collateral(
            storage.clone(),
            outbe_gratis::api::CollateralAuthorization {
                credis_id: position_id,
                collateral_id: void.collateral_id,
                action: outbe_gratis::api::CollateralAction::Burn,
                amount: void.gratis_burned,
                expected_remaining: void.gratis_burned,
            },
            anchor,
        )?;

        // The equivalent value is deposited 1:1 into the Promis Reserve.
        outbe_promislimit::PromisLimitContract::new(storage.clone())
            .add_to_total_unallocated(burned)?;

        Ok(())
    })
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
