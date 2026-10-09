//! Shared harness for the slashindicator integration test binaries.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_validatorset::contract::ValidatorSet;

const CHAIN_ID: u64 = 1;

/// Runs `f` against fresh in-memory storage at block 1.
pub fn with_storage<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_block_number(1);
    storage.enter(f)
}

/// Registers `submitter` as an ACTIVE validator, so it may submit evidence.
///
/// Its consensus pubkey is `0x77` followed by zeros. With `Some(bonded)`, the
/// submitter also gets that bonded stake projection before activation.
pub fn register_active_submitter(
    vs: &mut ValidatorSet<'_>,
    submitter: Address,
    bonded: Option<U256>,
) -> Result<(), PrecompileError> {
    let mut pubkey = [0u8; 48];
    pubkey[0] = 0x77;
    match bonded {
        Some(bonded) => vs.test_register_active_validator(submitter, &pubkey, bonded),
        None => {
            vs.test_register_validator_without_pop(submitter, &pubkey)?;
            vs.activate_validator_via_boundary_for_test(submitter)?;
            Ok(())
        }
    }
}
