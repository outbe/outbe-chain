//! Business logic for the confidential Gratis token.
//!
//! Each write reads the account's current ciphertext from storage, hands the op
//! to the enclave (which decrypts, enforces invariants, and re-encrypts
//! deterministically), then stores the returned ciphertext verbatim, applies the
//! plaintext aggregate delta, and emits the matching event. These methods are
//! crate-private; other crates reach them through [`crate::api`]. The enclave is
//! the sole party that sees plaintext balances (Enclave Return Rule).
//!
//! Pledge model (two-phase, no escrow account): `pledge` debits `balance` and parks
//! the amount in an encrypted `PledgeLockTicket`; `consume_pledge` (at requestCredis)
//! deletes the ticket and credits the EOA's OWN pledged ledger; `release_to_eoa`
//! (per anadosis) and `burn_pledged` (at credis expiry) draw the collateral back down
//! from that same EOA's pledged ledger. `pledged_total_supply` counts both the
//! pending (in-ticket) and active (in `pledged_ct`) locked gratis.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::GRATIS_ADDRESS;
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

/// A placeholder authorization for the credis-driven ops (`ConsumePledge`,
/// `ReleaseToEoa`, `BurnPledged`), which are gated by the pledge-ticket state /
/// spend-auth binding and the on-chain Credis position schedule rather than a modify
/// key.
fn no_auth() -> ModifyAuth {
    ModifyAuth {
        mac: [0u8; 32],
        op_nonce: 0,
    }
}

/// Build a request with the fields common to every op left at their empty defaults.
fn base_request(op: GratisOp, chain_id: B256, account: Address, amount: U256) -> GratisOpRequest {
    GratisOpRequest {
        op,
        chain_id,
        account,
        amount,
        current_balance: Vec::new(),
        current_pledged: Vec::new(),
        current_pledge_record: Vec::new(),
        modify_auth: no_auth(),
        pledge_handle: None,
        bundle_account: None,
        spend_auth: None,
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

/// Mint `amount` gratis to `caller` (owner-authorized).
pub(crate) fn mint(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    let gratis = Gratis::new(storage.clone());
    check_op_nonce(&gratis, caller, auth.op_nonce)?;
    let mut req = base_request(GratisOp::Mint, chain_id_b256(&storage)?, caller, amount);
    req.current_balance = gratis.balance_ct_of(caller)?;
    req.modify_auth = auth;
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
    let mut req = base_request(GratisOp::Burn, chain_id_b256(&storage)?, caller, amount);
    req.current_balance = gratis.balance_ct_of(caller)?;
    req.modify_auth = auth;
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

/// Lock `amount` of `caller`'s balance into a new pending `PledgeLockTicket`. The
/// amount leaves the liquid balance but is NOT yet credited to the pledged ledger
/// (that happens at `consume_pledge`). Returns the pledge handle the CCA later
/// presents at `requestCredis`.
pub(crate) fn pledge(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<B256> {
    let gratis = Gratis::new(storage.clone());
    check_op_nonce(&gratis, caller, auth.op_nonce)?;
    let mut req = base_request(GratisOp::Pledge, chain_id_b256(&storage)?, caller, amount);
    req.current_balance = gratis.balance_ct_of(caller)?;
    req.modify_auth = auth;
    let result = apply_gratis_op(req)?;
    ensure_applied(&result)?;
    write_account_blobs(&gratis, caller, &result)?;
    gratis.write_pledge_ticket_ct(result.pledge_handle, &result.new_pledge_record)?;
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

/// Return a still-pending pledge (e.g. credis rejected): credit the ticket amount
/// back to `caller`'s balance and delete the ticket.
pub(crate) fn unpledge(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    pledge_handle: B256,
    auth: ModifyAuth,
) -> Result<()> {
    let gratis = Gratis::new(storage.clone());
    check_op_nonce(&gratis, caller, auth.op_nonce)?;
    let mut req = base_request(GratisOp::Unpledge, chain_id_b256(&storage)?, caller, amount);
    req.current_balance = gratis.balance_ct_of(caller)?;
    req.current_pledge_record = gratis.pledge_ticket_ct_of(pledge_handle)?;
    req.modify_auth = auth;
    req.pledge_handle = Some(pledge_handle);
    let result = apply_gratis_op(req)?;
    ensure_applied(&result)?;
    write_account_blobs(&gratis, caller, &result)?;
    // `new_pledge_record` is empty → this clears (deletes) the ticket slot.
    gratis.write_pledge_ticket_ct(pledge_handle, &result.new_pledge_record)?;
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

/// requestCredis: consume `pledge_handle`'s ticket (authorized by `spend_auth`, which
/// binds it to `bundle`), crediting the collateral into the EOA's OWN pledged ledger
/// and deleting the ticket. No escrow account and no aggregate change (it stays
/// pledged, pending → active). Returns `gratis_amount` so credis can size the
/// position. `eoa` is supplied by the caller and checked by the enclave against the
/// ticket owner.
pub(crate) fn consume_pledge(
    storage: StorageHandle<'_>,
    pledge_handle: B256,
    bundle: Address,
    eoa: Address,
    spend_auth: [u8; 32],
) -> Result<U256> {
    let gratis = Gratis::new(storage.clone());
    let mut req = base_request(
        GratisOp::ConsumePledge,
        chain_id_b256(&storage)?,
        eoa,
        U256::ZERO,
    );
    req.current_pledged = gratis.pledged_ct_of(eoa)?;
    req.current_pledge_record = gratis.pledge_ticket_ct_of(pledge_handle)?;
    req.pledge_handle = Some(pledge_handle);
    req.bundle_account = Some(bundle);
    req.spend_auth = Some(spend_auth);
    let result = apply_gratis_op(req)?;
    ensure_applied(&result)?;
    // Credit the EOA's own pledged ledger and delete the consumed ticket.
    write_account_blobs(&gratis, eoa, &result)?;
    gratis.write_pledge_ticket_ct(pledge_handle, &result.new_pledge_record)?;
    Ok(result.gratis_amount)
}

/// payAnadosis: release `amount` of collateral from `eoa`'s own pledged ledger back
/// to its balance. Amount-based (no ticket): the credis position schedule is the
/// accounting authority. Returns the released amount.
pub(crate) fn release_to_eoa(
    storage: StorageHandle<'_>,
    eoa: Address,
    amount: U256,
) -> Result<U256> {
    let gratis = Gratis::new(storage.clone());
    let mut req = base_request(
        GratisOp::ReleaseToEoa,
        chain_id_b256(&storage)?,
        eoa,
        amount,
    );
    req.current_balance = gratis.balance_ct_of(eoa)?;
    req.current_pledged = gratis.pledged_ct_of(eoa)?;
    let result = apply_gratis_op(req)?;
    ensure_applied(&result)?;
    write_account_blobs(&gratis, eoa, &result)?;
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

/// Credis expiry: burn `amount` of collateral from `eoa`'s own pledged ledger,
/// reducing both `total_supply` and `pledged_total_supply`. Amount-based (no ticket):
/// the credis position's outstanding collateral is the authority. Returns the burned
/// amount.
pub(crate) fn burn_pledged(storage: StorageHandle<'_>, eoa: Address, amount: U256) -> Result<U256> {
    let gratis = Gratis::new(storage.clone());
    let mut req = base_request(GratisOp::BurnPledged, chain_id_b256(&storage)?, eoa, amount);
    req.current_pledged = gratis.pledged_ct_of(eoa)?;
    let result = apply_gratis_op(req)?;
    ensure_applied(&result)?;
    write_account_blobs(&gratis, eoa, &result)?;
    let remaining = gratis
        .total_supply()?
        .checked_sub(result.event_amount)
        .ok_or_else(|| PrecompileError::Fatal("gratis total_supply underflow".to_string()))?;
    gratis.set_total_supply(remaining)?;
    let total_pledged = gratis
        .pledged_total_supply()?
        .checked_sub(result.event_amount)
        .ok_or_else(|| PrecompileError::Fatal("gratis pledged_total underflow".to_string()))?;
    gratis.set_pledged_total_supply(total_pledged)?;
    storage.emit_event(
        GRATIS_ADDRESS,
        SolEvent::encode_log_data(&IGratis::GratisBurned {
            account: eoa,
            amount: result.event_amount,
            remainingSupply: remaining,
        }),
    )?;
    Ok(result.gratis_amount)
}
