//! Business logic for the confidential Gratis token.
//!
//! Each write reads the account's current ciphertext from storage, hands the op
//! to the enclave (which decrypts, enforces invariants, and re-encrypts
//! deterministically), then stores the returned ciphertext verbatim, applies the
//! plaintext aggregate delta, and emits the matching event. These methods are
//! crate-private; other crates reach them through [`crate::api`]. The enclave is
//! the sole party that sees plaintext balances (Enclave Return Rule).

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::{CREDIS_ADDRESS, GRATIS_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_tee::protocol::{GratisOp, GratisOpRequest, GratisOpResult, GratisOpStatus, ModifyAuth};

use crate::enclave_client::apply_gratis_op;
use crate::precompile::IGratis;
use crate::schema::Gratis;

/// The chain id the enclave binds a modify-auth to, as a `B256` (host and client
/// must agree on this encoding). The account's modify key is already chain-bound
/// via the DKG state key, so this is defense-in-depth.
fn chain_id_b256(storage: &StorageHandle<'_>) -> Result<B256> {
    Ok(B256::from(U256::from(storage.chain_id()?)))
}

/// A placeholder authorization for the credis-driven ops (`PledgeToBundle`,
/// `UnlockToEoa`), which are gated by the pledge-record state machine and the
/// spend-auth binding rather than a modify key.
fn no_auth() -> ModifyAuth {
    ModifyAuth {
        mac: [0u8; 32],
        op_nonce: 0,
    }
}

/// Reject unless the supplied op-nonce equals the account's current on-chain
/// counter — this is what makes a captured modify-auth non-replayable.
fn check_op_nonce(gratis: &Gratis<'_>, account: Address, provided: u64) -> Result<()> {
    let current = gratis.op_nonce_of(account)?;
    if provided != current {
        return Err(PrecompileError::Revert(format!(
            "invalid op nonce: expected {current}, got {provided}"
        )));
    }
    Ok(())
}

/// Turn a business rejection from the enclave into a precompile revert.
fn ensure_applied(result: &GratisOpResult) -> Result<()> {
    match &result.status {
        GratisOpStatus::Applied => Ok(()),
        GratisOpStatus::Rejected { reason } => Err(PrecompileError::Revert(reason.clone())),
    }
}

/// Store the balance / pledged ciphertext blobs the enclave produced (an empty
/// blob means the op did not touch that slot).
fn write_account_blobs(
    gratis: &Gratis<'_>,
    account: Address,
    result: &GratisOpResult,
) -> Result<()> {
    if !result.new_balance.is_empty() {
        gratis.write_balance_ct(account, &result.new_balance)?;
    }
    if !result.new_pledged.is_empty() {
        gratis.write_pledged_ct(account, &result.new_pledged)?;
    }
    Ok(())
}

/// Store the escrow balance ciphertext the enclave produced for the two-account
/// credis moves (an empty blob means the op did not touch the escrow).
fn write_credis_balance(gratis: &Gratis<'_>, result: &GratisOpResult) -> Result<()> {
    if !result.new_credis_balance.is_empty() {
        gratis.write_balance_ct(CREDIS_ADDRESS, &result.new_credis_balance)?;
    }
    Ok(())
}

/// Mint `amount` gratis to `caller` (owner-authorized).
pub(crate) fn mint(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    let gratis = Gratis::new(storage.clone());
    check_op_nonce(&gratis, caller, auth.op_nonce)?;
    let req = GratisOpRequest {
        op: GratisOp::Mint,
        chain_id: chain_id_b256(&storage)?,
        account: caller,
        amount,
        current_balance: gratis.balance_ct_of(caller)?,
        current_pledged: Vec::new(),
        current_pledge_record: Vec::new(),
        modify_auth: auth,
        installments: 0,
        pledge_handle: None,
        bundle_account: None,
        spend_auth: None,
        credis_account: None,
        credis_balance: Vec::new(),
    };
    let result = apply_gratis_op(req)?;
    ensure_applied(&result)?;
    write_account_blobs(&gratis, caller, &result)?;
    gratis.set_op_nonce(caller, result.next_op_nonce)?;
    let new_supply = gratis
        .total_supply()?
        .checked_add(result.event_amount)
        .ok_or_else(|| PrecompileError::Fatal("gratis total_supply overflow".to_string()))?;
    gratis.set_total_supply(new_supply)?;
    storage.emit_event(
        GRATIS_ADDRESS,
        SolEvent::encode_log_data(&IGratis::GratisMinted {
            account: caller,
            amount: result.event_amount,
            newTotalSupply: new_supply,
        }),
    )?;
    Ok(())
}

/// Burn `amount` gratis from `caller` (owner-authorized). Returns remaining supply.
pub(crate) fn burn(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    let gratis = Gratis::new(storage.clone());
    check_op_nonce(&gratis, caller, auth.op_nonce)?;
    let req = GratisOpRequest {
        op: GratisOp::Burn,
        chain_id: chain_id_b256(&storage)?,
        account: caller,
        amount,
        current_balance: gratis.balance_ct_of(caller)?,
        current_pledged: Vec::new(),
        current_pledge_record: Vec::new(),
        modify_auth: auth,
        installments: 0,
        pledge_handle: None,
        bundle_account: None,
        spend_auth: None,
        credis_account: None,
        credis_balance: Vec::new(),
    };
    let result = apply_gratis_op(req)?;
    ensure_applied(&result)?;
    write_account_blobs(&gratis, caller, &result)?;
    gratis.set_op_nonce(caller, result.next_op_nonce)?;
    let remaining = gratis
        .total_supply()?
        .checked_sub(result.event_amount)
        .ok_or_else(|| PrecompileError::Fatal("gratis total_supply underflow".to_string()))?;
    gratis.set_total_supply(remaining)?;
    storage.emit_event(
        GRATIS_ADDRESS,
        SolEvent::encode_log_data(&IGratis::GratisBurned {
            account: caller,
            amount: result.event_amount,
            remainingSupply: remaining,
        }),
    )?;
    Ok(remaining)
}

/// Lock `amount` gratis from `caller` into the credis escrow, opening a pledge
/// record spread over `installments` anadosis payments. Returns the pledge handle
/// (the public record id the CCA later presents at `requestCredis`).
pub(crate) fn pledge(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    installments: u32,
    auth: ModifyAuth,
) -> Result<B256> {
    let gratis = Gratis::new(storage.clone());
    check_op_nonce(&gratis, caller, auth.op_nonce)?;
    let req = GratisOpRequest {
        op: GratisOp::Pledge,
        chain_id: chain_id_b256(&storage)?,
        account: caller,
        amount,
        current_balance: gratis.balance_ct_of(caller)?,
        current_pledged: gratis.pledged_ct_of(caller)?,
        current_pledge_record: Vec::new(),
        modify_auth: auth,
        installments,
        pledge_handle: None,
        bundle_account: None,
        spend_auth: None,
        credis_account: None,
        credis_balance: Vec::new(),
    };
    let result = apply_gratis_op(req)?;
    ensure_applied(&result)?;
    write_account_blobs(&gratis, caller, &result)?;
    gratis.write_pledge_record_ct(result.pledge_handle, &result.new_pledge_record)?;
    gratis.set_op_nonce(caller, result.next_op_nonce)?;
    let total_pledged = gratis
        .pledged_total_supply()?
        .checked_add(result.event_amount)
        .ok_or_else(|| PrecompileError::Fatal("gratis pledged_total overflow".to_string()))?;
    gratis.set_pledged_total_supply(total_pledged)?;
    storage.emit_event(
        GRATIS_ADDRESS,
        SolEvent::encode_log_data(&IGratis::GratisPledged {
            account: caller,
            amount: result.event_amount,
            totalPledged: total_pledged,
        }),
    )?;
    Ok(result.pledge_handle)
}

/// Direct unpledge of an UNSPENT pledge (e.g. credis rejected): returns the full
/// collateral to `caller` and closes the record.
pub(crate) fn unpledge(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    pledge_handle: B256,
    auth: ModifyAuth,
) -> Result<()> {
    let gratis = Gratis::new(storage.clone());
    check_op_nonce(&gratis, caller, auth.op_nonce)?;
    let req = GratisOpRequest {
        op: GratisOp::Unpledge,
        chain_id: chain_id_b256(&storage)?,
        account: caller,
        amount,
        current_balance: gratis.balance_ct_of(caller)?,
        current_pledged: gratis.pledged_ct_of(caller)?,
        current_pledge_record: gratis.pledge_record_ct_of(pledge_handle)?,
        modify_auth: auth,
        installments: 0,
        pledge_handle: Some(pledge_handle),
        bundle_account: None,
        spend_auth: None,
        credis_account: None,
        credis_balance: Vec::new(),
    };
    let result = apply_gratis_op(req)?;
    ensure_applied(&result)?;
    write_account_blobs(&gratis, caller, &result)?;
    gratis.write_pledge_record_ct(pledge_handle, &result.new_pledge_record)?;
    gratis.set_op_nonce(caller, result.next_op_nonce)?;
    let total_pledged = gratis
        .pledged_total_supply()?
        .checked_sub(result.event_amount)
        .ok_or_else(|| PrecompileError::Fatal("gratis pledged_total underflow".to_string()))?;
    gratis.set_pledged_total_supply(total_pledged)?;
    storage.emit_event(
        GRATIS_ADDRESS,
        SolEvent::encode_log_data(&IGratis::GratisUnpledged {
            account: caller,
            amount: result.event_amount,
            remainingPledged: total_pledged,
        }),
    )?;
    Ok(())
}

/// requestCredis: consume `pledge_handle` for a credis request, binding it to
/// `bundle`, and move the pledged collateral out of `eoa`'s pledged ledger into
/// the `CREDIS_ADDRESS` escrow balance. Returns `gratis_amount` so credis can size
/// the position. Authorized by `spend_auth` (not a modify key); `eoa` is supplied
/// by the host and checked by the enclave against the pledge record.
pub(crate) fn pledge_to_bundle(
    storage: StorageHandle<'_>,
    pledge_handle: B256,
    bundle: Address,
    eoa: Address,
    spend_auth: [u8; 32],
) -> Result<U256> {
    let gratis = Gratis::new(storage.clone());
    let req = GratisOpRequest {
        op: GratisOp::PledgeToBundle,
        chain_id: chain_id_b256(&storage)?,
        account: eoa,
        amount: U256::ZERO,
        current_balance: Vec::new(),
        current_pledged: gratis.pledged_ct_of(eoa)?,
        current_pledge_record: gratis.pledge_record_ct_of(pledge_handle)?,
        modify_auth: no_auth(),
        installments: 0,
        pledge_handle: Some(pledge_handle),
        bundle_account: Some(bundle),
        spend_auth: Some(spend_auth),
        credis_account: Some(CREDIS_ADDRESS),
        credis_balance: gratis.balance_ct_of(CREDIS_ADDRESS)?,
    };
    let result = apply_gratis_op(req)?;
    ensure_applied(&result)?;
    // Debit the EOA's pledged ledger and credit the escrow balance.
    write_account_blobs(&gratis, eoa, &result)?;
    write_credis_balance(&gratis, &result)?;
    gratis.write_pledge_record_ct(pledge_handle, &result.new_pledge_record)?;
    // The collateral left the pledge pool for the escrow.
    let total_pledged = gratis
        .pledged_total_supply()?
        .checked_sub(result.event_amount)
        .ok_or_else(|| PrecompileError::Fatal("gratis pledged_total underflow".to_string()))?;
    gratis.set_pledged_total_supply(total_pledged)?;
    storage.emit_event(
        GRATIS_ADDRESS,
        SolEvent::encode_log_data(&IGratis::GratisUnpledged {
            account: eoa,
            amount: result.event_amount,
            remainingPledged: total_pledged,
        }),
    )?;
    Ok(result.gratis_amount)
}

/// payAnadosis: release one installment of collateral from the `CREDIS_ADDRESS`
/// escrow balance back to the original `eoa`'s balance. Returns the released
/// amount. The host supplies `eoa` (the `eoaAccount` calldata arg); the enclave
/// checks the record binds to it. No aggregate change — the pledge left the pool
/// at `pledge_to_bundle`; this is a balance→balance move.
pub(crate) fn unlock_to_eoa(
    storage: StorageHandle<'_>,
    eoa: Address,
    pledge_handle: B256,
) -> Result<U256> {
    let gratis = Gratis::new(storage.clone());
    let req = GratisOpRequest {
        op: GratisOp::UnlockToEoa,
        chain_id: chain_id_b256(&storage)?,
        account: eoa,
        amount: U256::ZERO,
        current_balance: gratis.balance_ct_of(eoa)?,
        current_pledged: Vec::new(),
        current_pledge_record: gratis.pledge_record_ct_of(pledge_handle)?,
        modify_auth: no_auth(),
        installments: 0,
        pledge_handle: Some(pledge_handle),
        bundle_account: None,
        spend_auth: None,
        credis_account: Some(CREDIS_ADDRESS),
        credis_balance: gratis.balance_ct_of(CREDIS_ADDRESS)?,
    };
    let result = apply_gratis_op(req)?;
    ensure_applied(&result)?;
    // Credit the EOA's balance and debit the escrow balance.
    write_account_blobs(&gratis, eoa, &result)?;
    write_credis_balance(&gratis, &result)?;
    gratis.write_pledge_record_ct(pledge_handle, &result.new_pledge_record)?;
    Ok(result.gratis_amount)
}
