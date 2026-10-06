//! Stateless encrypted day arithmetic. Each write binds the exact predecessor.

use alloy_primitives::{B256, U256};
use outbe_primitives::tribute_day_encryption::EncryptedTributeDayAmountV2;
use outbe_tee::tribute_day::{
    day_operation_inputs_hash, TributeDayOpRequestV2, TributeDayOperationV2,
};
use ring::hmac;
use zeroize::Zeroizing;

use crate::{
    confidential::Domain,
    errors::{Result, TeeError},
    tribute_encryption::decrypt_tribute,
};

const DAY_AMOUNT: Domain = Domain {
    state_info: b"outbe/tribute/day/state/v2",
    view_info: b"outbe/tribute/day/view/v2",
    modify_info: b"outbe/tribute/day/modify/v2",
    nonce_info: b"outbe/tribute/day/nonce/v2",
    modify_tag: b"outbe/tribute/day/auth/v2",
};

pub fn read_day_amount(
    network_secret: &[u8; 32],
    record: &EncryptedTributeDayAmountV2,
) -> Result<U256> {
    if record.version().is_none() {
        return Err(TeeError::DecryptFailed);
    }
    let (_, bytes) = DAY_AMOUNT.read_blob(
        network_secret,
        record.crypto_slot(),
        0,
        &record.encrypted_amount,
    )?;
    let bytes = Zeroizing::new(bytes);
    let amount: &[u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| TeeError::DecryptFailed)?;
    Ok(U256::from_be_bytes(*amount))
}

pub fn apply_day_operation(
    network_secret: &[u8; 32],
    request: &TributeDayOpRequestV2,
) -> Result<EncryptedTributeDayAmountV2> {
    let (previous_version, amount) = match &request.previous {
        None => (0, U256::ZERO),
        Some(previous) => {
            if previous.chain_id != request.chain_id
                || previous.worldwide_day != request.worldwide_day
                || previous.frozen
            {
                return Err(rejected("invalid day predecessor"));
            }
            (
                previous.version().ok_or(TeeError::DecryptFailed)?,
                read_day_amount(network_secret, previous)?,
            )
        }
    };
    let next = apply_amount(network_secret, request, amount)?;
    let inputs_hash = day_operation_inputs_hash(request).map_err(|_| TeeError::EncryptFailed)?;
    // A keyed context prevents the public operation hash from becoming a
    // guessing oracle for temporary plaintext calculation inputs.
    let mut operation_context = b"outbe/tribute/day-operation/v2".to_vec();
    operation_context.extend_from_slice(inputs_hash.as_slice());
    let tag = hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, network_secret),
        &operation_context,
    );
    let mut record = EncryptedTributeDayAmountV2 {
        chain_id: request.chain_id,
        worldwide_day: request.worldwide_day,
        frozen: matches!(request.operation, TributeDayOperationV2::Freeze),
        operation_hash: B256::from_slice(tag.as_ref()),
        encrypted_amount: Vec::new(),
    };
    let plaintext = Zeroizing::new(next.to_be_bytes::<32>());
    record.encrypted_amount = DAY_AMOUNT.write_blob(
        network_secret,
        record.crypto_slot(),
        0,
        previous_version,
        plaintext.as_ref(),
    )?;
    Ok(record)
}

fn apply_amount(
    network_secret: &[u8; 32],
    request: &TributeDayOpRequestV2,
    amount: U256,
) -> Result<U256> {
    let (delta, add) = match &request.operation {
        TributeDayOperationV2::Adjust { tribute, add } => {
            if tribute.context.chain_id != request.chain_id
                || tribute.context.worldwide_day != request.worldwide_day
            {
                return Err(rejected("Tribute belongs to another day or chain"));
            }
            let amounts = decrypt_tribute(network_secret, tribute)?;
            if amounts.issuance_amount_minor.is_zero() {
                return Err(rejected("Tribute issuance amount must be positive"));
            }
            (amounts.nominal_amount_minor, *add)
        }
        TributeDayOperationV2::AdjustTransient {
            nominal_amount_minor,
            add,
        } => (*nominal_amount_minor, *add),
        TributeDayOperationV2::Reset { expected_total } => {
            if amount != *expected_total {
                return Err(rejected("day retirement amount mismatch"));
            }
            (*expected_total, false)
        }
        TributeDayOperationV2::Freeze => return Ok(amount),
    };
    if add {
        amount.checked_add(delta)
    } else {
        amount.checked_sub(delta)
    }
    .ok_or_else(|| rejected("day nominal amount overflow or underflow"))
}

fn rejected(reason: &str) -> TeeError {
    TeeError::TributeOfferReject(reason.into())
}
