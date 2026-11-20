//! Stateless encrypted Gratis transitions over liquid and pledged balances.

use alloy_primitives::{Address, B256, U256};
use outbe_core_common::confidential_tee::{
    event_amount_input_hash, BalanceTransition, TeeError, FIELD_BALANCE, GRATIS,
};
use outbe_tee::resident::{AccountKeysHandle, ResidentOfferMaterial, StateKeyHandle};
use thiserror::Error;

use crate::cipher;
use crate::errors::display_string;
use crate::protocol::{
    gratis_op_canonical_hash, GratisOp, GratisOpRequest, GratisOpResult, GratisOpStatus, ModifyAuth,
};

/// Pledged-blob field tag. It differs from [`FIELD_BALANCE`], so the blobs never share a nonce.
pub const FIELD_PLEDGED: u8 = 1;

/// The account operation covered by a Gratis authorization.
pub type ModifyOperation = outbe_core_common::confidential_tee::ModifyOperation<GratisOp>;

/// Business or cryptographic rejection stored on [`GratisOpStatus::Rejected`].
#[derive(Debug, Error)]
pub(crate) enum GratisRejectReason {
    /// Modify MAC did not match the account key.
    #[error("invalid modify authorization")]
    InvalidModifyAuthorization,
    /// The owner nonce cannot advance.
    #[error("modify nonce exhausted")]
    ModifyNonceExhausted,
    /// Amount or account was zero.
    #[error("amount and account must be nonzero")]
    AmountAndAccountMustBeNonzero,
    /// Release or pledged-burn carried a Fidelity section.
    #[error("collateral must not change Fidelity")]
    CollateralMustNotChangeFidelity,
    /// Debit exceeded the blob, or a credit overflowed.
    #[error("insufficient balance or balance overflow")]
    InsufficientBalanceOrOverflow,
    /// Envelope cryptography failed. Display stays the historical sentence.
    #[error(transparent)]
    Crypto(#[from] TeeError),
}

/// Derive the resident Gratis state key from the DKG group signature.
pub fn derive_gratis_state_key(
    material: &ResidentOfferMaterial,
    chain_id: B256,
    epoch: u64,
) -> Result<StateKeyHandle, TeeError> {
    GRATIS.derive_state_key(material, chain_id, epoch)
}

/// Per-account view key. It is also the AEAD key for that account's blobs.
pub fn derive_view_key(
    state_key: &StateKeyHandle,
    account: Address,
) -> Result<StateKeyHandle, TeeError> {
    GRATIS.account_keys.derive_view_key(state_key, account)
}

/// Opaque per-account read and modify capabilities.
pub fn derive_account_keys(
    state_key: &StateKeyHandle,
    account: Address,
) -> Result<AccountKeysHandle, TeeError> {
    GRATIS.account_keys.derive_account_keys(state_key, account)
}

/// MAC over the canonical authorization fields.
#[must_use]
pub fn modify_mac(modify_key: &AccountKeysHandle, operation: &ModifyOperation) -> [u8; 32] {
    GRATIS
        .authorization
        .modify_mac(modify_key, &operation.authorization())
}

/// Decrypt a liquid balance. An empty blob is zero.
pub fn decrypt_balance(
    view_key: &StateKeyHandle,
    account: Address,
    blob: &[u8],
) -> Result<U256, TeeError> {
    cipher::read_amount(view_key, account, FIELD_BALANCE, blob).map(|(_, amount)| amount)
}

/// Decrypt a pledged balance. An empty blob is zero.
pub fn decrypt_pledged(
    view_key: &StateKeyHandle,
    account: Address,
    blob: &[u8],
) -> Result<U256, TeeError> {
    cipher::read_amount(view_key, account, FIELD_PLEDGED, blob).map(|(_, amount)| amount)
}

/// Rejected result that keeps the caller's inputs hash.
#[must_use]
pub fn rejected_result(reason: String, inputs_canonical_hash: B256) -> GratisOpResult {
    let mut result = empty_result();
    result.status = GratisOpStatus::Rejected { reason };
    result.inputs_canonical_hash = inputs_canonical_hash;
    result
}

/// Apply one Gratis op. Business rejections are [`GratisOpStatus::Rejected`].
#[must_use]
pub fn apply_op(state_key: &StateKeyHandle, req: &GratisOpRequest) -> GratisOpResult {
    let inputs_canonical_hash = gratis_op_canonical_hash(req);
    let mut result = match apply_op_inner(state_key, req, inputs_canonical_hash) {
        Ok(result) => result,
        Err(error) => rejected_reason(GratisRejectReason::from(error)),
    };
    result.inputs_canonical_hash = inputs_canonical_hash;
    result
}

fn empty_result() -> GratisOpResult {
    GratisOpResult {
        status: GratisOpStatus::Applied,
        new_balance: Vec::new(),
        new_pledged: Vec::new(),
        event_amount: U256::ZERO,
        encrypted_event_amount: Vec::new(),
        next_op_nonce: 0,
        fidelity: None,
        inputs_canonical_hash: B256::ZERO,
        attestation_tag: Vec::new(),
    }
}

fn rejected_reason(reason: GratisRejectReason) -> GratisOpResult {
    let mut result = empty_result();
    result.status = GratisOpStatus::Rejected {
        reason: display_string(&reason),
    };
    result
}

fn owner_operation(op: GratisOp) -> bool {
    matches!(op, GratisOp::Mint | GratisOp::Burn | GratisOp::Pledge)
}

fn prepare_owner(
    state_key: &StateKeyHandle,
    req: &GratisOpRequest,
    result: &mut GratisOpResult,
) -> Result<Option<GratisRejectReason>, TeeError> {
    let modify_key = derive_account_keys(state_key, req.account)?;
    let operation = ModifyOperation {
        account: req.account,
        op: req.op,
        amount: req.amount,
        op_nonce: req.modify_auth.op_nonce,
        chain_id: req.chain_id,
    };
    if !GRATIS.authorization.verify_modify_auth(
        &modify_key,
        &operation.authorization(),
        &req.modify_auth.mac,
    ) {
        return Ok(Some(GratisRejectReason::InvalidModifyAuthorization));
    }
    let Some(nonce) = req.modify_auth.op_nonce.checked_add(1) else {
        return Ok(Some(GratisRejectReason::ModifyNonceExhausted));
    };
    result.next_op_nonce = nonce;
    Ok(None)
}

struct FieldWrite<'a> {
    credit: Option<bool>,
    field: u8,
    blob: &'a [u8],
}

fn next_amount(current: U256, amount: U256, credit: bool) -> Option<U256> {
    if credit {
        current.checked_add(amount)
    } else {
        current.checked_sub(amount)
    }
}

fn write_field(
    view: &StateKeyHandle,
    account: Address,
    amount: U256,
    input_hash: B256,
    write: FieldWrite<'_>,
) -> Result<Result<Vec<u8>, GratisRejectReason>, TeeError> {
    let Some(credit) = write.credit else {
        return Ok(Ok(Vec::new()));
    };
    let (version, current) = cipher::read_amount(view, account, write.field, write.blob)?;
    let Some(next) = next_amount(current, amount, credit) else {
        return Ok(Err(GratisRejectReason::InsufficientBalanceOrOverflow));
    };
    cipher::write_amount(
        view,
        BalanceTransition {
            account,
            field: write.field,
            previous_version: version,
            amount: next,
            input_hash,
        },
    )
    .map(Ok)
}

fn apply_op_inner(
    state_key: &StateKeyHandle,
    req: &GratisOpRequest,
    inputs_canonical_hash: B256,
) -> Result<GratisOpResult, TeeError> {
    if req.amount.is_zero() || req.account.is_zero() {
        return Ok(rejected_reason(
            GratisRejectReason::AmountAndAccountMustBeNonzero,
        ));
    }
    let mut result = empty_result();
    if owner_operation(req.op) {
        if let Some(reason) = prepare_owner(state_key, req, &mut result)? {
            return Ok(rejected_reason(reason));
        }
    } else if req.fidelity.is_some() {
        return Ok(rejected_reason(
            GratisRejectReason::CollateralMustNotChangeFidelity,
        ));
    }
    let view = derive_view_key(state_key, req.account)?;
    let (balance, pledged) = field_moves(req.op);
    let writes = [
        FieldWrite {
            credit: balance,
            field: FIELD_BALANCE,
            blob: &req.current_balance,
        },
        FieldWrite {
            credit: pledged,
            field: FIELD_PLEDGED,
            blob: &req.current_pledged,
        },
    ];
    let mut written = [Vec::new(), Vec::new()];
    for (write, out) in writes.into_iter().zip(&mut written) {
        match write_field(&view, req.account, req.amount, inputs_canonical_hash, write)? {
            Ok(blob) => *out = blob,
            Err(reason) => return Ok(rejected_reason(reason)),
        }
    }
    [result.new_balance, result.new_pledged] = written;
    result.event_amount = req.amount;
    if matches!(
        req.op,
        GratisOp::Mint | GratisOp::Burn | GratisOp::BurnPledged
    ) {
        result.encrypted_event_amount = cipher::write_amount(
            &view,
            BalanceTransition {
                account: req.account,
                field: FIELD_BALANCE,
                previous_version: 0,
                amount: req.amount,
                input_hash: event_amount_input_hash(inputs_canonical_hash),
            },
        )?;
    }
    Ok(result)
}

fn field_moves(op: GratisOp) -> (Option<bool>, Option<bool>) {
    match op {
        GratisOp::Mint => (Some(true), None),
        GratisOp::Burn => (Some(false), None),
        GratisOp::Pledge => (Some(false), Some(true)),
        GratisOp::ReleasePledged => (Some(true), Some(false)),
        GratisOp::BurnPledged => (None, Some(false)),
    }
}

/// Zero authorization used by collateral ops and tests that fill the MAC later.
#[must_use]
pub fn empty_auth(op_nonce: u64) -> ModifyAuth {
    ModifyAuth {
        mac: [0; 32],
        op_nonce,
    }
}
