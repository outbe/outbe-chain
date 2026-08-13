//! Orchestration logic for the credisfactory precompile.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;

use outbe_credis::CredisContract;
use outbe_oracle::api::get_currency_rate;
use outbe_primitives::addresses::{CREDIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::errors::CredisFactoryError;
use crate::precompile::ICredisFactory;
use crate::sol_ext::IReferenceCurrency;
use crate::sol_ext::IERC20;

// ---------------------------------------------------------------------------
// request_credis
// ---------------------------------------------------------------------------

/// Consumes a confidential Gratis pledge (identified by `pledge_handle` +
/// `spend_auth`, which binds it to `smart_account`), crediting the collateral into
/// the pledger's own confidential pledged ledger, opens a credis position bound to
/// `smartAccount`, stores the sealed pledger EOA on the position for the later
/// per-installment release / expiry burn, and delivers the stablecoin loan via the
/// vault sub-call.
///
/// Nothing is priced here. The disbursed amount, the asset and the entry rate were all
/// quoted and sealed into the pledge ticket by `pledgeGratis`, so the loan is issued at
/// the price the pledger accepted rather than whatever the oracle reads now. Only the
/// debt terms (the ISO currency rate driving the anadosis schedule) are pinned at
/// issuance, because those belong to the loan and not to the collateral.
///
/// The pledger EOA is never in calldata: the enclave recovers it from the ticket and
/// returns it sealed (`eoa_ct`). Called by the CCA; `_caller` is deliberately unused —
/// the authorization is `spend_auth`, verified inside the enclave.
/// Returns `(position_id, amount_stables)`.
pub fn request_credis(
    storage: StorageHandle<'_>,
    _caller: Address,
    smart_account: Address,
    pledge_handle: B256,
    spend_auth: [u8; 32],
) -> Result<(U256, U256)> {
    if smart_account.is_zero() {
        return Err(CredisFactoryError::InvalidSmartAccount.into());
    }

    // Block timestamp is read from the execution frame rather than threaded in
    // by the caller.
    let current_time = storage.timestamp()?.to::<u64>();

    // Reject borrowers with overdue anadosis on any of their positions.
    {
        let credis = CredisContract::new(storage.clone());
        if credis.has_overdue_anadosis(smart_account, current_time)? {
            return Err(CredisFactoryError::OverduePayments.into());
        }
    }

    // Consume the pledge ticket (the enclave verifies `spend_auth` binds it to
    // `smart_account`, so a mempool copy cannot redirect the loan). The collateral
    // moves into the EOA's OWN pledged ledger and the ticket is deleted. The enclave
    // reads the pledger EOA from the ticket and returns it sealed (`eoa_ct`) so it is
    // stored on the position as ciphertext, never plaintext.
    let (terms, eoa_ct) = outbe_gratis::api::consume_pledge(
        storage.clone(),
        pledge_handle,
        smart_account,
        spend_auth,
    )?;
    let asset = terms.asset;
    if asset.is_zero() {
        return Err(CredisFactoryError::InvalidAsset.into());
    }

    // Derive the issuance currency from the disbursed asset (it self-reports its
    // ISO 4217 code via `IReferenceCurrency.isoCode()`) and pin the matching
    // currency rate read from the Oracle's reference-currency collection.
    let issuance_currency = read_iso_code(&storage, asset)?;
    let currency_rate = get_currency_rate(storage.clone(), issuance_currency)?;

    // Open the credis position, storing the sealed pledger EOA so the anadosis release
    // and the expiry-burn sweep can address the right confidential pledged ledger. The
    // `handle_id` argument to `create_position` builds the position_id; we use the
    // globally-unique pledge handle.
    let handle_id = U256::from_be_bytes(pledge_handle.0);
    let mut credis = CredisContract::new(storage.clone());
    let position_id = credis.create_position(
        handle_id,
        smart_account,
        eoa_ct,
        asset,
        issuance_currency,
        currency_rate,
        terms.stables_amount,
        terms.entry_rate,
        terms.gratis_amount,
        current_time,
    )?;

    // Withdraw the matching stablecoin from the vault to the smart account.
    outbe_vaultrouter::api::withdraw(&storage, asset, terms.stables_amount, smart_account)?;

    storage.emit_event(
        CREDIS_FACTORY_ADDRESS,
        alloy_sol_types::SolEvent::encode_log_data(&ICredisFactory::CredisRequested {
            smartAccount: smart_account,
            amount: terms.stables_amount,
        }),
    )?;

    Ok((position_id, terms.stables_amount))
}

// ---------------------------------------------------------------------------
// pay_anadosis
// ---------------------------------------------------------------------------

/// Applies `amount` to the credis position's unpaid anadosis schedule (oldest
/// installment first, the last one reached possibly only partially) and releases the
/// matching share of collateral from the pledger's OWN confidential pledged ledger back
/// to its balance. When `amount` exceeds what the schedule still needs, only the
/// required part is pulled from the caller.
///
/// ANY caller may pay — a third party can settle someone else's position. That is safe
/// by construction rather than by an access check: the debt is pulled from `caller`'s
/// own balance, while the freed collateral goes to the pledger EOA stored sealed on the
/// position and recovered here through the enclave (`reveal_owner`). The payer can
/// therefore never redirect value to themselves, and the EOA never appears on-chain.
/// The payment (the ERC20 → vault deposit below) is the authorization for the release —
/// no separate proof is required. Returns the stablecoin actually pulled.
pub fn pay_anadosis(
    storage: StorageHandle<'_>,
    caller: Address,
    position_id: U256,
    amount: U256,
) -> Result<U256> {
    if amount.is_zero() {
        return Err(CredisFactoryError::InvalidAmount.into());
    }

    // Read-only validation pass before any mutation; recover the pledger EOA from the
    // position's sealed `eoa_ct` via a RevealOwner enclave round-trip.
    let eoa_account = {
        let credis_ro = CredisContract::new(storage.clone());
        let position = credis_ro.get_position(position_id)?;
        if credis_ro.get_next_anadosis(position_id)?.is_none() {
            return Err(CredisFactoryError::PositionCompleted.into());
        }

        if position.asset.is_zero() {
            return Err(CredisFactoryError::InvalidAsset.into());
        }
        outbe_gratis::api::reveal_owner(storage.clone(), &position.eoa_ct)?
    };

    let current_time = storage.timestamp()?.to::<u64>();
    let mut credis = CredisContract::new(storage.clone());
    let payment = credis.pay_anadosis(position_id, amount, current_time)?;

    // ERC20 + vault sequence, moving only what the schedule consumed. Sub-call reverts
    // propagate out and unwind the bookkeeping via the surrounding precompile frame.
    let paid = payment.total_paid_amount;
    let asset = payment.asset;

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

    // 4) Release the collateral share freed by this payment from the pledger's own
    //    pledged ledger back to its liquid Gratis balance. One enclave round-trip for
    //    the whole payment, however many installments it spanned.
    if !payment.gratis_released.is_zero() {
        outbe_gratis::api::release_to_eoa(storage.clone(), eoa_account, payment.gratis_released)?;
    }

    Ok(paid)
}

/// Burns the outstanding pledged collateral of an expired credis position, drops the
/// pledger's fidelity cohort by the burned amount, and deposits the equivalent value
/// into the Promis Reserve.
pub fn expire_position(storage: StorageHandle<'_>, position_id: U256) -> Result<()> {
    let now = storage.timestamp()?.to::<u64>();
    let position = CredisContract::new(storage.clone()).get_position(position_id)?;

    if now < CredisContract::expires_at(&position) {
        return Err(CredisFactoryError::NotExpired.into());
    }
    if position.outstanding_anadosis_amount.is_zero() {
        return Err(CredisFactoryError::NothingOutstanding.into());
    }

    // Recover the pledger EOA from the position's sealed `eoa_ct` through the enclave so
    // the burn / fidelity drop address the right confidential ledgers (reveal once, use
    // for both).
    let eoa = outbe_gratis::api::reveal_owner(storage.clone(), &position.eoa_ct)?;

    // Burn the still-locked collateral from the pledger's own pledged ledger,
    // folding the Fidelity sale cohort into the SAME enclave round-trip (no extra
    // trip); persist the returned fidelity blob.
    let burned = position.outstanding_gratis_amount;
    let section = outbe_fidelity::api::cohort_section(
        storage.clone(),
        eoa,
        outbe_fidelity::api::FidelityCohortOp::Out,
        now,
    )?;
    let (_, outcome) =
        outbe_gratis::api::burn_pledged_with_fidelity(storage.clone(), eoa, burned, section)?;
    outbe_fidelity::api::apply_fidelity_outcome(storage.clone(), eoa, &outcome)?;

    // The equivalent value is deposited 1:1 into the Promis Reserve.
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .add_to_total_unallocated(burned)?;

    // Close the position (zero outstanding balances, emit CollateralBurned).
    CredisContract::new(storage.clone()).expire_position(position_id)?;

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
    IReferenceCurrency::isoCodeCall::abi_decode_returns(&ret)
        .map_err(|_| CredisFactoryError::AssetIsoUndecodable.into())
}
