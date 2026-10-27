//! Shared harness for the slashindicator integration test binaries.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use outbe_slashindicator::precompile::ISlashIndicator;

/// Asserts that `err` is the typed revert `selector`.
pub fn assert_selector(err: &PrecompileError, selector: [u8; 4]) {
    match err {
        PrecompileError::RevertBytes(bytes) if bytes.len() >= 4 => {
            assert_eq!(&bytes[..4], &selector, "{err:?}");
        }
        other => panic!("expected typed revert bytes, got {other:?}"),
    }
}

/// Selector of `ISlashIndicator::EvidenceAlreadyProcessed`.
pub fn evidence_already_processed() -> [u8; 4] {
    ISlashIndicator::EvidenceAlreadyProcessed::SELECTOR
}
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
        Some(bonded) => {
            vs.test_seed_authoritative_min_stake(bonded.min(U256::from(1)))?;
            vs.test_register_active_validator(submitter, &pubkey, bonded)
        }
        None => {
            vs.test_seed_authoritative_min_stake(U256::from(1))?;
            vs.test_register_validator_without_pop(submitter, &pubkey)?;
            vs.test_seed_authoritative_bonded(submitter, U256::from(1))?;
            vs.activate_validator_via_boundary_for_test(submitter)?;
            Ok(())
        }
    }
}
