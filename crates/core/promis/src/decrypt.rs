//! Client-local opening of a context-bound Promis balance with its view key.

use alloy_primitives::{Address, U256};
use outbe_core_common::confidential_tee::{
    decrypt_promis_balance as open_balance, LocalCryptoError,
};

/// Open a `PRO2` Promis balance envelope with the account view key.
pub fn decrypt_promis_balance(
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
) -> Result<U256, LocalCryptoError> {
    open_balance(view_key, account, blob)
}
