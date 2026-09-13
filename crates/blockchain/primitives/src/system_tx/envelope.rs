use alloy_consensus::TxLegacy;
use alloy_primitives::Bytes;
use alloy_primitives::TxKind;
use alloy_primitives::U256;

use super::{
    system_tx_intrinsic_gas, SystemTxError, SystemTxInputV2, SystemTxKind,
    MAX_SYSTEM_TXS_PER_BLOCK, OUTBE_SYSTEM_TX_ADDRESS,
};

pub fn system_tx_nonce(block_number: u64, ordinal: u8) -> Result<u64, SystemTxError> {
    if ordinal >= MAX_SYSTEM_TXS_PER_BLOCK {
        return Err(SystemTxError::OrdinalTooLarge {
            ordinal,
            max: MAX_SYSTEM_TXS_PER_BLOCK,
        });
    }
    block_number
        .checked_mul(u64::from(MAX_SYSTEM_TXS_PER_BLOCK))
        .and_then(|base| base.checked_add(u64::from(ordinal)))
        .ok_or(SystemTxError::NonceOverflow {
            block_number,
            ordinal,
        })
}

pub fn build_unsigned_system_tx(
    kind: SystemTxKind,
    ordinal: u8,
    block_number: u64,
    chain_id: u64,
    calldata: Bytes,
) -> Result<TxLegacy, SystemTxError> {
    let gas_limit = system_tx_intrinsic_gas(calldata.as_ref())?;
    build_unsigned_system_tx_with_gas_limit(
        kind,
        ordinal,
        block_number,
        chain_id,
        calldata,
        gas_limit,
    )
}

pub fn build_unsigned_system_tx_with_gas_limit(
    kind: SystemTxKind,
    ordinal: u8,
    block_number: u64,
    chain_id: u64,
    calldata: Bytes,
    gas_limit: u64,
) -> Result<TxLegacy, SystemTxError> {
    let actual = SystemTxInputV2::decode(calldata.as_ref())?.kind();
    if actual != kind {
        return Err(SystemTxError::CalldataKindMismatch {
            expected: kind,
            actual,
        });
    }
    let intrinsic_gas = system_tx_intrinsic_gas(calldata.as_ref())?;
    if gas_limit < intrinsic_gas {
        return Err(SystemTxError::GasLimitBelowIntrinsic {
            gas_limit,
            intrinsic_gas,
        });
    }

    Ok(TxLegacy {
        chain_id: Some(chain_id),
        nonce: system_tx_nonce(block_number, ordinal)?,
        gas_price: 0,
        gas_limit,
        to: TxKind::Call(OUTBE_SYSTEM_TX_ADDRESS),
        value: U256::ZERO,
        input: calldata,
    })
}
