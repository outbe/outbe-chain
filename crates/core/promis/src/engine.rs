//! Stateless encrypted Promis transitions.

use alloy_primitives::{Address, B256, U256};
use outbe_core_common::confidential_tee::{
    event_amount_input_hash, BalanceTransition, TeeError, FIELD_BALANCE, PROMIS,
};
use outbe_tee::resident::{AccountKeysHandle, ResidentOfferMaterial, StateKeyHandle};
use thiserror::Error;

use crate::cipher;
use crate::errors::display_string;
use crate::protocol::{
    promis_op_canonical_hash, PromisOp, PromisOpRequest, PromisOpResult, PromisOpStatus,
};

/// The account operation covered by a Promis authorization.
pub type ModifyOperation = outbe_core_common::confidential_tee::ModifyOperation<PromisOp>;

/// Business or cryptographic rejection stored on [`PromisOpStatus::Rejected`].
#[derive(Debug, Error)]
pub(crate) enum PromisRejectReason {
    /// Amount was zero.
    #[error("amount must be positive")]
    AmountMustBePositive,
    /// Account was the zero address.
    #[error("invalid address")]
    InvalidAddress,
    /// Modify MAC did not match the account key.
    #[error("invalid modify authorization")]
    InvalidModifyAuthorization,
    /// A credit overflowed the balance.
    #[error("promis balance overflow")]
    BalanceOverflow,
    /// A debit exceeded the balance.
    #[error("insufficient balance")]
    InsufficientBalance,
    /// The owner nonce cannot advance.
    #[error("modify nonce exhausted")]
    ModifyNonceExhausted,
    /// Envelope cryptography failed. Display stays the historical sentence.
    #[error(transparent)]
    Crypto(#[from] TeeError),
}

/// Derive the resident Promis state key from the DKG group signature.
pub fn derive_promis_state_key(
    material: &ResidentOfferMaterial,
    chain_id: B256,
    epoch: u64,
) -> Result<StateKeyHandle, TeeError> {
    PROMIS.derive_state_key(material, chain_id, epoch)
}

/// Per-account view key. It is also the AEAD key for that account's balance.
pub fn derive_view_key(
    state_key: &StateKeyHandle,
    account: Address,
) -> Result<StateKeyHandle, TeeError> {
    PROMIS.account_keys.derive_view_key(state_key, account)
}

/// Opaque per-account read and modify capabilities.
pub fn derive_account_keys(
    state_key: &StateKeyHandle,
    account: Address,
) -> Result<AccountKeysHandle, TeeError> {
    PROMIS.account_keys.derive_account_keys(state_key, account)
}

/// MAC over the canonical authorization fields.
#[must_use]
pub fn modify_mac(modify_key: &AccountKeysHandle, operation: &ModifyOperation) -> [u8; 32] {
    PROMIS
        .authorization
        .modify_mac(modify_key, &operation.authorization())
}

/// Decrypt a Promis balance. An empty blob is zero.
pub fn decrypt_balance(
    view_key: &StateKeyHandle,
    account: Address,
    blob: &[u8],
) -> Result<U256, TeeError> {
    cipher::read_amount(view_key, account, FIELD_BALANCE, blob).map(|(_, amount)| amount)
}

/// Apply one Promis op. Business rejections are [`PromisOpStatus::Rejected`].
#[must_use]
pub fn apply_op(state_key: &StateKeyHandle, req: &PromisOpRequest) -> PromisOpResult {
    let inputs_canonical_hash = promis_op_canonical_hash(req);
    let mut result = match apply_op_inner(state_key, req) {
        Ok(result) => result,
        Err(reason) => rejected(&reason),
    };
    result.inputs_canonical_hash = inputs_canonical_hash;
    result
}

fn apply_op_inner(
    state_key: &StateKeyHandle,
    req: &PromisOpRequest,
) -> Result<PromisOpResult, PromisRejectReason> {
    if let Some(reason) = static_refusal(req) {
        return Err(reason);
    }
    let view_key = authorized_view(state_key, req)?;
    let (version, balance) =
        cipher::read_amount(&view_key, req.account, FIELD_BALANCE, &req.current_balance)?;
    let next_op_nonce = req
        .modify_auth
        .op_nonce
        .checked_add(1)
        .ok_or(PromisRejectReason::ModifyNonceExhausted)?;
    let new_balance = moved_balance(balance, req)?;
    let input_hash = promis_op_canonical_hash(req);
    let mut result = empty_result();
    result.event_amount = req.amount;
    result.next_op_nonce = next_op_nonce;
    result.new_balance = seal(&view_key, req.account, version, new_balance, input_hash)?;
    result.encrypted_event_amount = seal(
        &view_key,
        req.account,
        0,
        req.amount,
        event_amount_input_hash(input_hash),
    )?;
    Ok(result)
}

fn static_refusal(req: &PromisOpRequest) -> Option<PromisRejectReason> {
    if req.amount.is_zero() {
        return Some(PromisRejectReason::AmountMustBePositive);
    }
    if req.account.is_zero() {
        return Some(PromisRejectReason::InvalidAddress);
    }
    None
}

fn authorized_view(
    state_key: &StateKeyHandle,
    req: &PromisOpRequest,
) -> Result<StateKeyHandle, PromisRejectReason> {
    let modify_key = derive_account_keys(state_key, req.account)?;
    let operation = ModifyOperation {
        account: req.account,
        op: req.op,
        amount: req.amount,
        op_nonce: req.modify_auth.op_nonce,
        chain_id: req.chain_id,
    };
    if !PROMIS.authorization.verify_modify_auth(
        &modify_key,
        &operation.authorization(),
        &req.modify_auth.mac,
    ) {
        return Err(PromisRejectReason::InvalidModifyAuthorization);
    }
    Ok(derive_view_key(state_key, req.account)?)
}

fn moved_balance(balance: U256, req: &PromisOpRequest) -> Result<U256, PromisRejectReason> {
    match req.op {
        PromisOp::Mint => balance
            .checked_add(req.amount)
            .ok_or(PromisRejectReason::BalanceOverflow),
        PromisOp::Burn => {
            if balance < req.amount {
                return Err(PromisRejectReason::InsufficientBalance);
            }
            Ok(balance - req.amount)
        }
    }
}

fn seal(
    view_key: &StateKeyHandle,
    account: Address,
    previous_version: u64,
    amount: U256,
    input_hash: B256,
) -> Result<Vec<u8>, TeeError> {
    cipher::write_amount(
        view_key,
        BalanceTransition {
            account,
            field: FIELD_BALANCE,
            previous_version,
            amount,
            input_hash,
        },
    )
}

fn empty_result() -> PromisOpResult {
    PromisOpResult {
        status: PromisOpStatus::Applied,
        new_balance: Vec::new(),
        event_amount: U256::ZERO,
        encrypted_event_amount: Vec::new(),
        next_op_nonce: 0,
        inputs_canonical_hash: B256::ZERO,
        attestation_tag: Vec::new(),
    }
}

fn rejected(reason: &impl std::fmt::Display) -> PromisOpResult {
    let mut result = empty_result();
    result.status = PromisOpStatus::Rejected {
        reason: display_string(reason),
    };
    result
}
