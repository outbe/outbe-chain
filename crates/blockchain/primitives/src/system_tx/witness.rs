use alloy_consensus::SignableTransaction;
use alloy_consensus::Transaction as AlloyTransaction;
use alloy_eips::eip2718::Decodable2718;
use alloy_primitives::Bytes;
use alloy_primitives::B256;
use alloy_primitives::U256;
use reth_ethereum::TransactionSigned;
use reth_primitives_traits::SignedTransaction;

use super::{
    build_unsigned_system_tx, system_tx_nonce, system_tx_visible_gas_limit, SystemTxError,
    SystemTxInputV2, SystemTxKind, OUTBE_SYSTEM_TX_ADDRESS,
};

/// Validate that a signed Phase 1 system transaction is the canonical
/// `CertifiedParentAccounting` witness for `expected_calldata`.
pub fn validate_phase1_witness_against(
    tx: &TransactionSigned,
    expected_calldata: &[u8],
    expected_proposer: alloy_primitives::Address,
    chain_id: u64,
    block_number: u64,
) -> Result<B256, SystemTxError> {
    let tx_hash = validate_phase1_envelope_shape(tx, expected_calldata, chain_id, block_number)?;
    let signer = tx
        .try_recover()
        .map_err(|error| SystemTxError::Phase1SignatureRecovery(error.to_string()))?;
    if signer != expected_proposer {
        return Err(SystemTxError::Phase1SignerMismatch {
            expected: expected_proposer,
            actual: signer,
        });
    }
    Ok(tx_hash)
}

/// Decode and validate a signed Phase 1 transaction from evidence bytes,
/// returning the recovered proposer and canonical calldata.
pub fn recover_phase1_proposer(
    tx_bytes: &[u8],
    chain_id: u64,
    block_number: u64,
) -> Result<(alloy_primitives::Address, Bytes), SystemTxError> {
    let mut tx_slice = tx_bytes;
    let tx = TransactionSigned::decode_2718(&mut tx_slice)
        .map_err(|error| SystemTxError::Phase1TxDecode(error.to_string()))?;
    if !tx_slice.is_empty() {
        return Err(SystemTxError::Phase1TxDecode(format!(
            "phase1 tx has {} trailing bytes after EIP-2718 envelope",
            tx_slice.len()
        )));
    }
    let calldata = tx.input().clone();
    validate_phase1_envelope_shape(&tx, calldata.as_ref(), chain_id, block_number)?;
    let proposer = tx
        .try_recover()
        .map_err(|error| SystemTxError::Phase1SignatureRecovery(error.to_string()))?;
    Ok((proposer, calldata))
}

fn validate_phase1_envelope_shape(
    tx: &TransactionSigned,
    calldata: &[u8],
    chain_id: u64,
    block_number: u64,
) -> Result<B256, SystemTxError> {
    if tx.to() != Some(OUTBE_SYSTEM_TX_ADDRESS) {
        return Err(SystemTxError::Phase1WrongRecipient);
    }
    if tx.value() != U256::ZERO {
        return Err(SystemTxError::Phase1NonZeroValue);
    }
    if tx.chain_id() != Some(chain_id) {
        return Err(SystemTxError::Phase1ChainIdMismatch {
            expected: chain_id,
            actual: tx.chain_id(),
        });
    }
    let expected_nonce = system_tx_nonce(block_number, 0)?;
    if tx.nonce() != expected_nonce {
        return Err(SystemTxError::Phase1NonceMismatch {
            expected: expected_nonce,
            actual: tx.nonce(),
        });
    }
    let expected_gas_limit = system_tx_visible_gas_limit(calldata)?;
    if tx.gas_limit() != expected_gas_limit {
        return Err(SystemTxError::Phase1GasLimitMismatch {
            expected: expected_gas_limit,
            actual: tx.gas_limit(),
        });
    }
    if tx.input().as_ref() != calldata {
        return Err(SystemTxError::Phase1CalldataMismatch);
    }
    let actual = SystemTxInputV2::decode(calldata)?.kind();
    if actual != SystemTxKind::CertifiedParentAccounting {
        return Err(SystemTxError::CalldataKindMismatch {
            expected: SystemTxKind::CertifiedParentAccounting,
            actual,
        });
    }
    let expected_unsigned = build_unsigned_system_tx(
        SystemTxKind::CertifiedParentAccounting,
        0,
        block_number,
        chain_id,
        Bytes::copy_from_slice(calldata),
    )?;
    if tx.signature_hash() != expected_unsigned.signature_hash() {
        return Err(SystemTxError::Phase1SignatureHashMismatch);
    }
    Ok(tx.signature_hash())
}
