//! Client-local opening of a context-bound Gratis balance with its view key.
use crate::errors::LocalCryptoError;
use crate::owner_local_open::{open_v2_balance_blob, GRATIS_V2_SPEC};
use alloy_primitives::{Address, U256};

pub fn decrypt_gratis_balance(
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
) -> Result<U256, LocalCryptoError> {
    open_v2_balance_blob(view_key, account, blob, GRATIS_V2_SPEC)
}
