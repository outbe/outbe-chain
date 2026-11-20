//! Rollback-safe mutable Promis balance envelopes (`PRO2`).

use alloy_primitives::{Address, U256};
use outbe_core_common::confidential_tee::{BalanceTransition, TeeError, PROMIS_BALANCE_ENVELOPE};
use outbe_tee::resident::StateKeyHandle;

/// Open a Promis amount. An empty blob is version 0 and amount 0.
pub fn read_amount(
    key: &StateKeyHandle,
    account: Address,
    field: u8,
    blob: &[u8],
) -> Result<(u64, U256), TeeError> {
    PROMIS_BALANCE_ENVELOPE.read_amount(key, account, field, blob)
}

/// Seal the next Promis amount for `transition`.
pub fn write_amount(
    key: &StateKeyHandle,
    transition: BalanceTransition,
) -> Result<Vec<u8>, TeeError> {
    PROMIS_BALANCE_ENVELOPE.write_amount(key, transition)
}
