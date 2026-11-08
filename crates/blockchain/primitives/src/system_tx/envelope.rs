use alloy_consensus::TxLegacy;
use alloy_primitives::Bytes;
use alloy_primitives::TxKind;
use alloy_primitives::U256;

use super::{SystemTx, SystemTxError, MAX_SYSTEM_TXS_PER_BLOCK, OUTBE_SYSTEM_TX_ADDRESS};

/// Ordered system-transaction envelope inputs under the Tempo 0-gas model (`gas_limit == 0`).
#[derive(Debug)]
pub struct SystemTxEnvelopeInput {
    pub ordinal: u8,
    pub block_number: u64,
    pub chain_id: u64,
    pub calldata: Bytes,
    pub gas_limit: u64,
}

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
    ordinal: u8,
    block_number: u64,
    chain_id: u64,
    calldata: Bytes,
) -> Result<TxLegacy, SystemTxError> {
    build_unsigned_system_tx_with_gas_limit(SystemTxEnvelopeInput {
        ordinal,
        block_number,
        chain_id,
        calldata,
        gas_limit: 0,
    })
}

pub fn build_unsigned_system_tx_with_gas_limit(
    input: SystemTxEnvelopeInput,
) -> Result<TxLegacy, SystemTxError> {
    let SystemTxEnvelopeInput {
        ordinal,
        block_number,
        chain_id,
        calldata,
        gas_limit,
    } = input;

    SystemTx::decode(calldata.as_ref())?;
    if gas_limit != 0 {
        return Err(SystemTxError::NonZeroGasLimit { actual: gas_limit });
    }

    Ok(TxLegacy {
        chain_id: Some(chain_id),
        nonce: system_tx_nonce(block_number, ordinal)?,
        gas_price: 0,
        gas_limit: 0,
        to: TxKind::Call(OUTBE_SYSTEM_TX_ADDRESS),
        value: U256::ZERO,
        input: calldata,
    })
}
