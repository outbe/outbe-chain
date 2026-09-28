//! Gratis economics over a global encrypted state journal. Owner writes require
//! the account MAC/nonce; Credis operations carry an allocation authorization.
//! Both domains' completed updates join the surrounding EVM checkpoint.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::GRATIS_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_tee::protocol::{
    FidelityOpOutcome, FidelityOpSection, GratisOp, GratisOpRequest, GratisOpResult,
    GratisOpStatus, ModifyAuth, PledgeTerms,
};

use crate::precompile::IGratis;
use crate::schema::Gratis;
use outbe_tee::confidential::{Call, CollateralAction, CollateralAuthorization, Value};

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
        block_timestamp: 0,
        account,
        amount,
        current_balance: Vec::new(),
        current_pledged: Vec::new(),
        current_pledge_record: Vec::new(),
        modify_auth: no_auth(),
        pledge_note: None,
        smart_account: None,
        spend_auth: None,
        pledge_terms: None,
        fidelity: None,
    }
}

/// Turn a business rejection from the enclave into a precompile revert.
fn ensure_applied(result: &GratisOpResult) -> Result<()> {
    match &result.status {
        GratisOpStatus::Applied => Ok(()),
        GratisOpStatus::Rejected { reason } => Err(PrecompileError::Revert(reason.clone())),
    }
}

/// Pledge timestamps use u64 Unix seconds, matching the execution block clock.
/// Reject an out-of-range storage value rather than truncating it.
fn pledge_timestamp(storage: &StorageHandle<'_>) -> Result<u64> {
    u64::try_from(storage.timestamp()?)
        .map_err(|_| PrecompileError::Revert("pledge timestamp exceeds u64".into()))
}

fn apply_gratis_op(storage: &StorageHandle<'_>, req: GratisOpRequest) -> Result<GratisOpResult> {
    let result = crate::enclave_client::execute(storage, Call::Gratis(Box::new(req)))?;
    storage.with_checkpoint(|| {
        for update in &result.updates {
            outbe_tee::confidential::persist(storage, update)?;
        }
        match result.value {
            Value::Gratis(result) => Ok(*result),
            _ => Err(PrecompileError::Fatal("invalid Gratis result".into())),
        }
    })
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
    storage.with_checkpoint(|| {
        let gratis = Gratis::new(storage.clone());
        let mut req = base_request(GratisOp::Mint, chain_id_b256(&storage)?, caller, amount);
        req.modify_auth = auth;
        req.fidelity = fidelity;
        let result = apply_gratis_op(&storage, req)?;
        ensure_applied(&result)?;
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
    })
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
    storage.with_checkpoint(|| {
        let gratis = Gratis::new(storage.clone());
        let mut req = base_request(GratisOp::Burn, chain_id_b256(&storage)?, caller, amount);
        req.modify_auth = auth;
        req.fidelity = fidelity;
        let result = apply_gratis_op(&storage, req)?;
        ensure_applied(&result)?;
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
    })
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

/// Lock the gratis that covers `terms.stables_amount` into a new pending
/// `PledgeLockTicket`, sealing the loan terms alongside it. The gratis leaves the
/// liquid balance but is NOT yet credited to the pledged ledger (that happens at
/// `consume_pledge`). `amount_stables` is the MAC-bound figure; the gratis actually
/// debited comes from `terms`. Returns the pledge note the CCA later presents at
/// `requestCredis`.
fn pledge_impl(
    storage: StorageHandle<'_>,
    caller: Address,
    amount_stables: U256,
    terms: PledgeTerms,
    auth: ModifyAuth,
    fidelity: Option<FidelityOpSection>,
) -> Result<(Vec<u8>, Option<FidelityOpOutcome>)> {
    storage.with_checkpoint(|| {
        let gratis = Gratis::new(storage.clone());
        let mut req = base_request(
            GratisOp::Pledge,
            chain_id_b256(&storage)?,
            caller,
            amount_stables,
        );
        req.block_timestamp = pledge_timestamp(&storage)?;
        req.modify_auth = auth;
        req.pledge_terms = Some(terms);
        req.fidelity = fidelity;
        let result = apply_gratis_op(&storage, req)?;
        ensure_applied(&result)?;
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
        Ok((result.new_pledge_record, result.fidelity))
    })
}

pub(crate) fn pledge(
    storage: StorageHandle<'_>,
    caller: Address,
    amount_stables: U256,
    terms: PledgeTerms,
    auth: ModifyAuth,
) -> Result<Vec<u8>> {
    Ok(pledge_impl(storage, caller, amount_stables, terms, auth, None)?.0)
}

/// Pledge and carry a co-located fidelity **probe** (read-only league) in the
/// same round-trip; returns the pledge note + the caller's league outcome for
/// the eligibility gate.
pub(crate) fn pledge_with_fidelity(
    storage: StorageHandle<'_>,
    caller: Address,
    amount_stables: U256,
    terms: PledgeTerms,
    auth: ModifyAuth,
    fidelity: FidelityOpSection,
) -> Result<(Vec<u8>, FidelityOpOutcome)> {
    let (handle, outcome) =
        pledge_impl(storage, caller, amount_stables, terms, auth, Some(fidelity))?;
    Ok((handle, require_fidelity_outcome(outcome)?))
}

/// Return a still-pending pledge (e.g. credis rejected): credit the ticket's gratis
/// back to `caller`'s balance and delete the ticket. `amount_stables` is the stables
/// figure the pledge was quoted for; the enclave cross-checks it against the ticket
/// and returns the matching gratis.
pub(crate) fn unpledge(
    storage: StorageHandle<'_>,
    caller: Address,
    amount_stables: U256,
    pledge_note: B256,
    auth: ModifyAuth,
) -> Result<U256> {
    storage.with_checkpoint(|| {
        let gratis = Gratis::new(storage.clone());
        let mut req = base_request(
            GratisOp::Unpledge,
            chain_id_b256(&storage)?,
            caller,
            amount_stables,
        );
        req.modify_auth = auth;
        req.pledge_note = Some(pledge_note);
        let result = apply_gratis_op(&storage, req)?;
        ensure_applied(&result)?;
        // `new_pledge_record` is empty -> this clears (deletes) the ticket slot.
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
        Ok(result.event_amount)
    })
}

/// Consume an encrypted credential and bind its allocation to this exact Credis.
pub(crate) fn consume_pledge(
    storage: StorageHandle<'_>,
    credis_id: U256,
    credential: Vec<u8>,
    smart_account: Address,
) -> Result<(PledgeTerms, B256)> {
    let result = crate::enclave_client::execute(
        &storage,
        Call::Activate {
            credis_id,
            smart_account,
            credential,
            timestamp: pledge_timestamp(&storage)?,
        },
    )?;
    storage.with_checkpoint(|| {
        for update in &result.updates {
            outbe_tee::confidential::persist(&storage, update)?;
        }
        match result.value {
            Value::Activated {
                terms,
                collateral_id,
            } => Ok((terms, collateral_id)),
            _ => Err(PrecompileError::Fatal(
                "invalid collateral activation".into(),
            )),
        }
    })
}

pub(crate) fn apply_collateral(
    storage: StorageHandle<'_>,
    authorization: CollateralAuthorization,
    fidelity_anchor: u64,
) -> Result<U256> {
    storage.with_checkpoint(|| {
        let result = crate::enclave_client::execute(
            &storage,
            Call::Collateral {
                authorization,
                timestamp: pledge_timestamp(&storage)?,
                fidelity_anchor,
            },
        )?;
        let Value::Collateral { amount } = result.value else {
            return Err(PrecompileError::Fatal("invalid collateral response".into()));
        };
        if amount != authorization.amount {
            return Err(PrecompileError::Fatal("collateral amount mismatch".into()));
        }
        for update in &result.updates {
            outbe_tee::confidential::persist(&storage, update)?;
        }
        let gratis = Gratis::new(storage.clone());
        let pledged = gratis
            .pledged_total_supply()?
            .checked_sub(amount)
            .ok_or_else(|| PrecompileError::Fatal("pledged supply underflow".into()))?;
        gratis.set_pledged_total_supply(pledged)?;
        match authorization.action {
            CollateralAction::Return => storage.emit_event(
                GRATIS_ADDRESS,
                SolEvent::encode_log_data(&IGratis::GratisUnpledged {
                    account: Address::ZERO,
                    amount,
                    remainingPledged: pledged,
                }),
            )?,
            CollateralAction::Burn => {
                let supply = gratis
                    .total_supply()?
                    .checked_sub(amount)
                    .ok_or_else(|| PrecompileError::Fatal("Gratis supply underflow".into()))?;
                gratis.set_total_supply(supply)?;
                storage.emit_event(
                    GRATIS_ADDRESS,
                    SolEvent::encode_log_data(&IGratis::GratisBurned {
                        account: Address::ZERO,
                        amount,
                        remainingSupply: supply,
                    }),
                )?;
            }
        }
        Ok(amount)
    })
}
