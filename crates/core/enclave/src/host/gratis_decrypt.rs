//! Client-local opening of a context-bound Gratis balance with its view key.

use alloy_primitives::{Address, U256};

use super::errors::{LocalCryptoError, V2LedgerSpec};

/// Open a `GRA2` Gratis balance envelope with the account's Gratis view key.
pub fn decrypt_gratis_balance(
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
) -> Result<U256, LocalCryptoError> {
    V2LedgerSpec::GRATIS.open_u256(view_key, account, blob)
}
