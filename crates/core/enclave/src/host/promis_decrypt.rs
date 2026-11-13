//! Client-local opening of a context-bound Promis balance with its view key.

use alloy_primitives::{Address, U256};

use super::errors::{LocalCryptoError, V2LedgerSpec};

/// Open a `PRO2` Promis balance envelope with the account's Promis view key.
pub fn decrypt_promis_balance(
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
) -> Result<U256, LocalCryptoError> {
    V2LedgerSpec::PROMIS.open_u256(view_key, account, blob)
}
