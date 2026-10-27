//! Stateless balance operations and pledged-collateral transitions.
use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::GRATIS_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_tee::protocol::{
    FidelityOpOutcome, FidelityOpSection, GratisOp, GratisOpRequest, GratisOpResult,
    GratisOpStatus, ModifyAuth,
};

use crate::enclave_client::apply_gratis_op;
use crate::errors::{GratisError, GratisFatalError};
use crate::precompile::IGratis;
use crate::schema::Gratis;

/// A co-located fidelity section was sent, so the enclave must return its
/// outcome. A missing outcome is an enclave/transport fault.
fn require_fidelity_outcome(outcome: Option<FidelityOpOutcome>) -> Result<FidelityOpOutcome> {
    outcome.ok_or_else(|| GratisFatalError::MissingFidelityOutcome.into())
}

/// The chain id the enclave binds a modify-auth to, as a `B256` (host and client
/// must agree on this encoding). The account's modify key is already chain-bound
/// via the DKG state key, so this is defense-in-depth.
fn chain_id_b256(storage: &StorageHandle<'_>) -> Result<B256> {
    Ok(B256::from(U256::from(storage.chain_id()?)))
}

/// The runtime authorizes the collateral operations.
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
        modify_auth: no_auth(),
        fidelity: None,
    }
}

/// Reject unless the supplied op-nonce equals the account's current on-chain
/// counter. This check makes a captured modify-auth non-replayable.
fn check_op_nonce(gratis: &Gratis<'_>, account: Address, provided: u64) -> Result<()> {
    let current = crate::state::account(gratis, account).op_nonce()?;
    if provided != current {
        return Err(GratisError::InvalidOpNonce {
            expected: current,
            provided,
        }
        .into());
    }
    Ok(())
}

/// Turn a business rejection from the enclave into a precompile revert.
fn ensure_applied(result: &GratisOpResult) -> Result<()> {
    match &result.status {
        GratisOpStatus::Applied => Ok(()),
        GratisOpStatus::Rejected { reason } => Err(GratisError::Rejected(reason.clone()).into()),
    }
}

/// Store the updated ciphertexts returned by the enclave.
fn write_account_blobs(
    gratis: &Gratis<'_>,
    account: Address,
    result: &GratisOpResult,
) -> Result<()> {
    if !result.new_balance.is_empty() {
        crate::state::account(gratis, account).write_balance_ct(&result.new_balance)?;
    }
    if !result.new_pledged.is_empty() {
        crate::state::account(gratis, account).write_pledged_ct(&result.new_pledged)?;
    }
    Ok(())
}

/// The enclave moved exactly the requested amount of the balance and advanced the nonce by one.
pub(crate) fn ensure_owner_result(
    amount: U256,
    op_nonce: u64,
    result: &GratisOpResult,
) -> Result<()> {
    if result.event_amount != amount
        || result.new_balance.is_empty()
        || !result.new_pledged.is_empty()
    {
        return Err(GratisFatalError::OwnerResultMismatch.into());
    }
    ensure_next_nonce(op_nonce, result.next_op_nonce)
}

/// The enclave advanced the operation nonce by exactly one.
fn ensure_next_nonce(op_nonce: u64, next_op_nonce: u64) -> Result<()> {
    if op_nonce.checked_add(1) != Some(next_op_nonce) {
        return Err(GratisFatalError::WrongOpNonce.into());
    }
    Ok(())
}

/// Apply an owner-authorized balance movement and publish its matching event.
fn apply_owner_movement(
    storage: StorageHandle<'_>,
    mut request: GratisOpRequest,
) -> Result<Option<FidelityOpOutcome>> {
    let gratis = Gratis::new(storage.clone());
    let account = request.account;
    let (amount, op_nonce) = (request.amount, request.modify_auth.op_nonce);
    check_op_nonce(&gratis, account, op_nonce)?;
    request.current_balance = crate::state::account(&gratis, account).balance_ct()?;
    let operation = request.op;
    let _scope = outbe_tee::call_context::ContextScope::from_storage(&storage)?;
    let result = apply_gratis_op(request)?;
    ensure_applied(&result)?;
    ensure_owner_result(amount, op_nonce, &result)?;
    write_account_blobs(&gratis, account, &result)?;
    crate::state::account(&gratis, account).set_op_nonce(result.next_op_nonce)?;
    let event = match operation {
        GratisOp::Mint => SolEvent::encode_log_data(&IGratis::GratisMinted {
            account,
            gratisMinor: result.event_amount,
        }),
        GratisOp::Burn => SolEvent::encode_log_data(&IGratis::GratisBurned {
            account,
            gratisMinor: result.event_amount,
        }),
        _ => return Err(GratisFatalError::InvalidOwnerMovement.into()),
    };
    storage.emit_event(GRATIS_ADDRESS, event)?;
    Ok(result.fidelity)
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
    let mut request = base_request(GratisOp::Mint, chain_id_b256(&storage)?, caller, amount);
    request.modify_auth = auth;
    request.fidelity = fidelity;
    apply_owner_movement(storage, request)
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
/// enclave round-trip. Returns the fidelity outcome for the caller to persist.
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
/// co-located fidelity cohort section. Returns the Fidelity outcome.
fn burn_impl(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
    fidelity: Option<FidelityOpSection>,
) -> Result<Option<FidelityOpOutcome>> {
    let mut request = base_request(GratisOp::Burn, chain_id_b256(&storage)?, caller, amount);
    request.modify_auth = auth;
    request.fidelity = fidelity;
    apply_owner_movement(storage, request)
}

/// Burn `amount` gratis from `caller` (owner-authorized).
pub(crate) fn burn(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    burn_impl(storage, caller, amount, auth, None).map(|_| ())
}

/// Burn gratis and apply a co-located fidelity cohort sale in one enclave
/// round-trip. Returns the fidelity outcome for the caller to persist.
pub(crate) fn burn_with_fidelity(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
    fidelity: FidelityOpSection,
) -> Result<FidelityOpOutcome> {
    require_fidelity_outcome(burn_impl(storage, caller, amount, auth, Some(fidelity))?)
}

/// Move an owner-authorized amount into the pledged balance, with a read-only
/// Fidelity eligibility probe in the same enclave round-trip.
pub(crate) fn pledge_with_fidelity(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
    fidelity: FidelityOpSection,
) -> Result<FidelityOpOutcome> {
    storage.with_checkpoint(|| {
        let gratis = Gratis::new(storage.clone());
        check_op_nonce(&gratis, caller, auth.op_nonce)?;
        let next_nonce = auth.op_nonce.checked_add(1);
        let mut req = base_request(GratisOp::Pledge, chain_id_b256(&storage)?, caller, amount);
        req.modify_auth = auth;
        req.fidelity = Some(fidelity);
        let result = apply_collateral_op(&storage, req)?;
        if next_nonce != Some(result.next_op_nonce) {
            return Err(GratisFatalError::WrongOpNonce.into());
        }
        crate::state::account(&gratis, caller).set_op_nonce(result.next_op_nonce)?;
        let pledged = crate::state::pledged_total_supply(&gratis)?
            .checked_add(amount)
            .ok_or(GratisError::PledgedSupplyOverflow)?;
        crate::state::set_pledged_total_supply(&gratis, pledged)?;
        require_fidelity_outcome(result.fidelity)
    })
}

/// Run a collateral op over the blobs it changes and store the results. The
/// caller authorizes it and owns the aggregate bookkeeping.
fn apply_collateral_op(
    storage: &StorageHandle<'_>,
    mut req: GratisOpRequest,
) -> Result<GratisOpResult> {
    let gratis = Gratis::new(storage.clone());
    let (account, amount) = (req.account, req.amount);
    let moves_balance = !matches!(req.op, GratisOp::BurnPledged);
    if moves_balance {
        req.current_balance = crate::state::account(&gratis, account).balance_ct()?;
    }
    req.current_pledged = crate::state::account(&gratis, account).pledged_ct()?;
    let _scope = outbe_tee::call_context::ContextScope::from_storage(storage)?;
    let result = apply_gratis_op(req)?;
    ensure_applied(&result)?;
    if result.event_amount != amount
        || result.new_pledged.is_empty()
        || result.new_balance.is_empty() == moves_balance
    {
        return Err(GratisFatalError::CollateralResultMismatch.into());
    }
    write_account_blobs(&gratis, account, &result)?;
    Ok(result)
}

/// Return pledged collateral to the account's liquid balance. Only call this
/// after the caller has authorized the release.
pub(crate) fn release_pledged(
    storage: &StorageHandle<'_>,
    account: Address,
    amount: U256,
) -> Result<()> {
    storage.with_checkpoint(|| {
        let req = base_request(
            GratisOp::ReleasePledged,
            chain_id_b256(storage)?,
            account,
            amount,
        );
        apply_collateral_op(storage, req)?;
        let gratis = Gratis::new(storage.clone());
        crate::state::set_pledged_total_supply(
            &gratis,
            crate::state::pledged_total_supply(&gratis)?
                .checked_sub(amount)
                .ok_or(GratisFatalError::PledgedSupplyUnderflow)?,
        )
    })
}

/// Burn pledged collateral of a defaulted position. Fidelity is unchanged.
pub(crate) fn burn_pledged(
    storage: &StorageHandle<'_>,
    account: Address,
    amount: U256,
) -> Result<()> {
    storage.with_checkpoint(|| {
        let req = base_request(
            GratisOp::BurnPledged,
            chain_id_b256(storage)?,
            account,
            amount,
        );
        apply_collateral_op(storage, req)?;
        let gratis = Gratis::new(storage.clone());
        crate::state::set_pledged_total_supply(
            &gratis,
            crate::state::pledged_total_supply(&gratis)?
                .checked_sub(amount)
                .ok_or(GratisFatalError::PledgedSupplyUnderflow)?,
        )?;
        storage.emit_event(
            GRATIS_ADDRESS,
            SolEvent::encode_log_data(&IGratis::GratisBurned {
                account,
                gratisMinor: amount,
            }),
        )
    })
}

pub(crate) fn mint_encrypted_nod(
    storage: StorageHandle<'_>,
    nod: &outbe_primitives::nod_encryption::EncryptedNodV1,
    auth: ModifyAuth,
    fidelity: FidelityOpSection,
) -> Result<FidelityOpOutcome> {
    if nod.terms.chain_id != storage.chain_id()? {
        return Err(GratisError::NodChainMismatch.into());
    }
    let gratis = Gratis::new(storage.clone());
    let op_nonce = auth.op_nonce;
    check_op_nonce(&gratis, nod.terms.owner, op_nonce)?;
    let request = outbe_tee::nod_mine::MineEncryptedNodRequestV1 {
        nod: nod.clone(),
        current_balance: crate::state::account(&gratis, nod.terms.owner).balance_ct()?,
        modify_auth: auth,
        fidelity,
    };
    let _context = outbe_tee::call_context::ContextScope::from_storage(&storage)?;
    let result = crate::enclave_client::mine_encrypted_nod(request)?;
    ensure_next_nonce(op_nonce, result.next_op_nonce)?;
    if result.new_balance.is_empty() {
        return Err(GratisFatalError::OwnerResultMismatch.into());
    }
    crate::state::account(&gratis, nod.terms.owner).write_balance_ct(&result.new_balance)?;
    crate::state::account(&gratis, nod.terms.owner).set_op_nonce(result.next_op_nonce)?;
    storage.emit_event(
        GRATIS_ADDRESS,
        SolEvent::encode_log_data(&IGratis::GratisMinedFromNod {
            account: nod.terms.owner,
            encryptedAmount: nod.encrypted_gratis_amount.clone().into(),
            encryptedBalance: result.new_balance.into(),
        }),
    )?;
    Ok(result.fidelity)
}
