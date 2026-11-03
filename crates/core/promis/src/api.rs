//! Cross-module API for the confidential Promis token.

use alloy_primitives::{Address, U256};

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

pub use crate::runtime::MineGratisInput;
pub use outbe_tee::protocol::{FidelityOpSection, MineGratisFromPromisResultV1, ModifyAuth};

use crate::runtime;
use crate::schema::Promis;

// --- Reads ---

/// Encrypted balance blob for `account`. Decrypt it client-side with the view key.
pub fn balance_ct(storage: StorageHandle<'_>, account: Address) -> Result<Vec<u8>> {
    Promis::new(storage).balance_ct_of(account)
}

/// The account's current modify-auth replay counter (the value the client's next
/// write authorization must bind).
pub fn op_nonce(storage: StorageHandle<'_>, account: Address) -> Result<u64> {
    Promis::new(storage).op_nonce_of(account)
}

// --- Owner-authorized mutations ---

/// Mint `amount` promis to `caller` from a settled Gem position (`gem_id`).
pub fn mint_from_gem(
    storage: StorageHandle<'_>,
    gem_id: U256,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    runtime::mint(
        storage,
        runtime::MintSource::Gem(gem_id),
        caller,
        amount,
        auth,
    )
}

/// Mint `amount` promis to `caller` from a settled Intex position (`intex_id`).
pub fn mint_from_intex(
    storage: StorageHandle<'_>,
    intex_id: U256,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    runtime::mint(
        storage,
        runtime::MintSource::Intex(intex_id),
        caller,
        amount,
        auth,
    )
}

/// Burn `amount` promis from `caller`.
pub fn burn(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    runtime::burn(storage, caller, amount, auth)
}

/// Execute the combined Promis->Gratis conversion in the enclave, persist the
/// Promis burn state + `PromisBurned` event, and return the enclave result.
pub fn execute_mine_gratis(
    storage: &StorageHandle<'_>,
    input: MineGratisInput,
) -> Result<MineGratisFromPromisResultV1> {
    runtime::execute_mine_gratis(storage, input)
}
