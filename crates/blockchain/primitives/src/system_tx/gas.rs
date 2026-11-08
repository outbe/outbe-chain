use super::{SystemTx, SystemTxError};

/// Under the Tempo 0-gas model, all valid system transaction envelopes carry
/// `gas_limit == 0` and contribute `0` gas to block headers and receipts.
pub fn system_tx_intrinsic_gas(calldata: &[u8]) -> Result<u64, SystemTxError> {
    SystemTx::decode(calldata)?;
    Ok(0)
}

/// Visible gas limit of a system transaction envelope (`0` under the Tempo 0-gas model).
pub fn system_tx_visible_gas_limit(calldata: &[u8]) -> Result<u64, SystemTxError> {
    system_tx_intrinsic_gas(calldata)
}
