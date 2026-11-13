//! Client-local opening of a context-bound Promis balance with its view key.

use alloy_primitives::{Address, U256};

use super::{
    errors::LocalCryptoError,
    v2_balance_decrypt::{decrypt_v2_balance, V2BalanceLedger},
};

/// Open a `PRO2` Promis balance envelope with the account's Promis view key.
pub fn decrypt_promis_balance(
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
) -> Result<U256, LocalCryptoError> {
    decrypt_v2_balance(V2BalanceLedger::Promis, view_key, account, blob)
}
