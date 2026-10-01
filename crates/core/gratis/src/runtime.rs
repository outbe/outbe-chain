//! Stateless balance operations and funded pledge-note transitions.
use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::GRATIS_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_tee::protocol::{
    FidelityOpOutcome, FidelityOpSection, GratisOp, GratisOpRequest, GratisOpResult,
    GratisOpStatus, ModifyAuth,
};

use crate::enclave_client::apply_gratis_op;
use crate::precompile::IGratis;
use crate::schema::Gratis;

/// A co-located fidelity section was sent, so the enclave must return its
/// outcome; a missing one is an enclave/transport fault.
fn require_fidelity_outcome(outcome: Option<FidelityOpOutcome>) -> Result<FidelityOpOutcome> {
    outcome.ok_or_else(|| {
        PrecompileError::Fatal("enclave dropped the fidelity section outcome".to_string())
    })
}

/// The chain id the enclave binds a modify-auth to, as a `B256` (host and client
/// must agree on this encoding). The account's modify key is already chain-bound
/// via the DKG state key, so this is defense-in-depth.
fn chain_id_b256(storage: &StorageHandle<'_>) -> Result<B256> {
    Ok(B256::from(U256::from(storage.chain_id()?)))
}

/// Proof-backed and position-backed operations are authorized by the runtime.
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
        modify_auth: no_auth(),
        fidelity: None,
    }
}

/// Reject unless the supplied op-nonce equals the account's current on-chain
/// counter - this is what makes a captured modify-auth non-replayable.
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

/// Store the updated balance ciphertext returned by the enclave.
fn write_account_blobs(
    gratis: &Gratis<'_>,
    account: Address,
    result: &GratisOpResult,
) -> Result<()> {
    if !result.new_balance.is_empty() {
        gratis.write_balance_ct(account, &result.new_balance)?;
    }
    Ok(())
}

/// Mint `amount` gratis to `caller` (owner-authorized), optionally carrying a
/// co-located fidelity cohort section applied in the SAME enclave round-trip.
fn mint_impl(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
    fidelity: Option<FidelityOpSection>,
) -> Result<Option<FidelityOpOutcome>> {
    let gratis = Gratis::new(storage.clone());
    check_op_nonce(&gratis, caller, auth.op_nonce)?;
    let mut req = base_request(GratisOp::Mint, chain_id_b256(&storage)?, caller, amount);
    req.current_balance = gratis.balance_ct_of(caller)?;
    req.modify_auth = auth;
    req.fidelity = fidelity;
    let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&storage)?;
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
    Ok(result.fidelity)
}

/// Mint `amount` gratis to `caller` (owner-authorized).
pub(crate) fn mint(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    mint_impl(storage, caller, amount, auth, None).map(|_| ())
}

/// Mint gratis and apply a co-located fidelity cohort acquisition in one
/// enclave round-trip; returns the fidelity outcome for the caller to persist.
pub(crate) fn mint_with_fidelity(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
    fidelity: FidelityOpSection,
) -> Result<FidelityOpOutcome> {
    require_fidelity_outcome(mint_impl(storage, caller, amount, auth, Some(fidelity))?)
}

/// Burn `amount` gratis from `caller` (owner-authorized), optionally carrying a
/// co-located fidelity cohort section. Returns remaining supply + the outcome.
fn burn_impl(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
    fidelity: Option<FidelityOpSection>,
) -> Result<(U256, Option<FidelityOpOutcome>)> {
    let gratis = Gratis::new(storage.clone());
    check_op_nonce(&gratis, caller, auth.op_nonce)?;
    let mut req = base_request(GratisOp::Burn, chain_id_b256(&storage)?, caller, amount);
    req.current_balance = gratis.balance_ct_of(caller)?;
    req.modify_auth = auth;
    req.fidelity = fidelity;
    let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&storage)?;
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
    Ok((remaining, result.fidelity))
}

/// Burn `amount` gratis from `caller` (owner-authorized). Returns remaining supply.
pub(crate) fn burn(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    Ok(burn_impl(storage, caller, amount, auth, None)?.0)
}

/// Burn gratis and apply a co-located fidelity cohort sale in one enclave
/// round-trip; returns the fidelity outcome for the caller to persist.
pub(crate) fn burn_with_fidelity(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
    fidelity: FidelityOpSection,
) -> Result<FidelityOpOutcome> {
    require_fidelity_outcome(burn_impl(storage, caller, amount, auth, Some(fidelity))?.1)
}

/// Debit an authenticated Gratis amount and append the enclave's owner-bound note.
pub(crate) fn pledge_with_fidelity(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
    fidelity: FidelityOpSection,
) -> Result<(B256, FidelityOpOutcome)> {
    storage.with_checkpoint(|| {
        let gratis = Gratis::new(storage.clone());
        check_op_nonce(&gratis, caller, auth.op_nonce)?;
        let mut req = base_request(GratisOp::Pledge, chain_id_b256(&storage)?, caller, amount);
        req.current_balance = gratis.balance_ct_of(caller)?;
        req.modify_auth = auth;
        req.fidelity = Some(fidelity);
        let _scope = outbe_tee::call_context::ContextScope::from_storage(&storage)?;
        let result = apply_gratis_op(req)?;
        ensure_applied(&result)?;
        if result.event_amount != amount {
            return Err(PrecompileError::Fatal("pledge amount mismatch".into()));
        }
        let outcome = require_fidelity_outcome(result.fidelity.clone())?;
        let commitment = crate::pledge::fund(&storage, result.note_serial, amount, B256::ZERO)?;
        write_account_blobs(&gratis, caller, &result)?;
        gratis.set_op_nonce(caller, result.next_op_nonce)?;
        gratis.set_pledged_total_supply(
            gratis
                .pledged_total_supply()?
                .checked_add(amount)
                .ok_or_else(|| PrecompileError::Revert("pledged supply overflow".into()))?,
        )?;
        Ok((commitment, outcome))
    })
}

/// Only called after runtime proof/position authorization, never through a public balance-write ABI.
fn collateral_balance(
    storage: &StorageHandle<'_>,
    account: Address,
    amount: U256,
    op: GratisOp,
) -> Result<()> {
    let gratis = Gratis::new(storage.clone());
    let mut req = base_request(op, chain_id_b256(storage)?, account, amount);
    req.current_balance = gratis.balance_ct_of(account)?;
    let _scope = outbe_tee::call_context::ContextScope::from_storage(storage)?;
    let result = apply_gratis_op(req)?;
    ensure_applied(&result)?;
    if result.event_amount != amount {
        return Err(PrecompileError::Fatal("collateral amount mismatch".into()));
    }
    write_account_blobs(&gratis, account, &result)
}
pub(crate) fn unpledge(storage: StorageHandle<'_>, proof: &[u8]) -> Result<U256> {
    storage.with_checkpoint(|| {
        let claim = crate::pledge::consume_unpledge(&storage, proof)?;
        if claim.context
            != crate::api::unpledge_context(
                storage.chain_id()?,
                claim.destination,
                claim.spend_amount,
            )?
        {
            return Err(PrecompileError::Revert("pledge context mismatch".into()));
        }
        collateral_balance(
            &storage,
            claim.destination,
            claim.spend_amount,
            GratisOp::Unpledge,
        )?;
        let gratis = Gratis::new(storage.clone());
        gratis.set_pledged_total_supply(
            gratis
                .pledged_total_supply()?
                .checked_sub(claim.spend_amount)
                .ok_or_else(|| PrecompileError::Fatal("pledged supply underflow".into()))?,
        )?;
        Ok(claim.spend_amount)
    })
}
pub(crate) fn activate(storage: &StorageHandle<'_>, amount: U256) -> Result<()> {
    collateral_balance(
        storage,
        outbe_primitives::addresses::CREDIS_ADDRESS,
        amount,
        GratisOp::ConsumePledge,
    )
}
pub(crate) fn return_collateral(
    storage: &StorageHandle<'_>,
    position_id: U256,
    serial: B256,
    amount: U256,
    released_total: U256,
) -> Result<()> {
    storage.with_checkpoint(|| {
        collateral_balance(
            storage,
            outbe_primitives::addresses::CREDIS_ADDRESS,
            amount,
            GratisOp::ReleaseCollateral,
        )?;
        let receipt = outbe_zk_canonical::pledge::receipt_context(position_id, released_total)
            .and_then(|field| outbe_protocol::codec::field_to_b256(&field))
            .map_err(|error| PrecompileError::Fatal(error.to_string()))?;
        crate::pledge::fund(storage, serial, amount, receipt)?;
        Ok(())
    })
}
pub(crate) fn forfeit(storage: &StorageHandle<'_>, amount: U256) -> Result<()> {
    storage.with_checkpoint(|| {
        collateral_balance(
            storage,
            outbe_primitives::addresses::CREDIS_ADDRESS,
            amount,
            GratisOp::BurnPledged,
        )?;
        let gratis = Gratis::new(storage.clone());
        gratis.set_total_supply(
            gratis
                .total_supply()?
                .checked_sub(amount)
                .ok_or_else(|| PrecompileError::Fatal("Gratis supply underflow".into()))?,
        )?;
        gratis.set_pledged_total_supply(
            gratis
                .pledged_total_supply()?
                .checked_sub(amount)
                .ok_or_else(|| PrecompileError::Fatal("pledged supply underflow".into()))?,
        )?;
        Ok(())
    })
}
