//! Stateless encrypted Promis transitions, plus the Promis-to-Gratis mine.

use std::fmt::Write as _;

use alloy_primitives::{Address, B256, U256};
use outbe_core_common::confidential_tee::{
    decrypt_tee_amount, event_amount_input_hash, BalanceTransition, TeeError, FIELD_BALANCE, PROMIS,
};
use outbe_fidelity::apply_cohort_section;
use outbe_gratis::{apply_op as apply_gratis_op, GratisOp, GratisOpRequest, GratisOpStatus};
use outbe_tee::resident::{
    AccountKeysHandle, OfferKeyHandle, ResidentOfferMaterial, StateKeyHandle,
};
use thiserror::Error;

use crate::cipher;
use crate::errors::display_string;
use crate::protocol::{
    mine_gratis_from_promis_canonical_hash, promis_op_canonical_hash,
    MineGratisFromPromisRequestV1, MineGratisFromPromisResultV1, MineGratisFromPromisStatusV1,
    PromisOp, PromisOpRequest, PromisOpResult, PromisOpStatus,
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

/// Resident keys for [`apply_mine_gratis`].
pub struct MineGratisKeys<'a> {
    /// Offer key that opens `encrypted_promis_amount`.
    pub offer_secret: &'a OfferKeyHandle,
    /// Promis ledger state key.
    pub promis_state_key: &'a StateKeyHandle,
    /// Gratis ledger state key.
    pub gratis_state_key: &'a StateKeyHandle,
    /// Fidelity ledger state key.
    pub fidelity_state_key: &'a StateKeyHandle,
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

/// Burn Promis, mint Gratis, and apply the Fidelity `In` section.
///
/// A foreign `chain_id` fails inside the modify MAC as a signed rejection.
/// The host does not add a separate chain check.
#[must_use]
pub fn apply_mine_gratis(
    keys: MineGratisKeys<'_>,
    req: &MineGratisFromPromisRequestV1,
) -> MineGratisFromPromisResultV1 {
    let inputs_canonical_hash = mine_gratis_from_promis_canonical_hash(req);
    let amount = match decrypt_tee_amount(
        keys.offer_secret,
        &req.encrypted_promis_amount,
        req.chain_id,
        req.account,
    ) {
        Ok(amount) => amount,
        Err(error) => {
            return rejected_mine(
                MineGratisFromPromisStatusV1::PromisRejected {
                    reason: display_string(&error),
                },
                inputs_canonical_hash,
            );
        }
    };
    let promis = apply_op(
        keys.promis_state_key,
        &PromisOpRequest {
            op: PromisOp::Burn,
            chain_id: req.chain_id,
            account: req.account,
            amount,
            current_balance: req.current_promis_balance.clone(),
            modify_auth: req.promis_modify_auth.clone(),
        },
    );
    if let PromisOpStatus::Rejected { reason } = promis.status {
        return rejected_mine(
            MineGratisFromPromisStatusV1::PromisRejected { reason },
            inputs_canonical_hash,
        );
    }
    let gratis = apply_gratis_op(
        keys.gratis_state_key,
        &GratisOpRequest {
            op: GratisOp::Mint,
            chain_id: req.chain_id,
            account: req.account,
            amount,
            current_balance: req.current_gratis_balance.clone(),
            current_pledged: Vec::new(),
            modify_auth: req.gratis_modify_auth.clone(),
            fidelity: Some(req.fidelity.clone()),
        },
    );
    if let GratisOpStatus::Rejected { reason } = gratis.status {
        return rejected_mine(
            MineGratisFromPromisStatusV1::GratisRejected { reason },
            inputs_canonical_hash,
        );
    }
    let fidelity =
        match apply_cohort_section(keys.fidelity_state_key, req.account, amount, &req.fidelity) {
            Ok(outcome) => outcome,
            Err(error) => {
                return rejected_mine(
                    MineGratisFromPromisStatusV1::GratisRejected {
                        reason: fidelity_reason(&error),
                    },
                    inputs_canonical_hash,
                );
            }
        };
    MineGratisFromPromisResultV1 {
        status: MineGratisFromPromisStatusV1::Applied,
        new_promis_balance: promis.new_balance,
        encrypted_promis_event_amount: promis.encrypted_event_amount,
        next_promis_op_nonce: promis.next_op_nonce,
        new_gratis_balance: gratis.new_balance,
        encrypted_gratis_event_amount: gratis.encrypted_event_amount,
        next_gratis_op_nonce: gratis.next_op_nonce,
        fidelity: Some(fidelity),
        inputs_canonical_hash,
        attestation_tag: Vec::new(),
    }
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

fn rejected_mine(
    status: MineGratisFromPromisStatusV1,
    inputs_canonical_hash: B256,
) -> MineGratisFromPromisResultV1 {
    MineGratisFromPromisResultV1 {
        status,
        new_promis_balance: Vec::new(),
        encrypted_promis_event_amount: Vec::new(),
        next_promis_op_nonce: 0,
        new_gratis_balance: Vec::new(),
        encrypted_gratis_event_amount: Vec::new(),
        next_gratis_op_nonce: 0,
        fidelity: None,
        inputs_canonical_hash,
        attestation_tag: Vec::new(),
    }
}

fn fidelity_reason(error: &impl std::fmt::Display) -> String {
    let mut reason = String::from("fidelity section failed: ");
    let _ = write!(&mut reason, "{error}");
    reason
}
