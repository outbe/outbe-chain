use super::*;
use crate::test_support::test_seeded_ocomp_registration;
use outbe_primitives::error::Result;

pub(super) const CHAIN_ID: u64 = 1;

/// Owner address used across tests.
pub(super) const OWNER: Address = address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");

/// Storage at block `height` with the owner and room for `max` validators.
pub(super) fn registry_storage(height: u64, max: u32) -> Result<HashMapStorageProvider> {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_block_number(height);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = ValidatorSet::new(storage);
        vs.config_owner.write(OWNER)?;
        vs.set_config_max_validators(max)
    })?;
    Ok(storage)
}

/// Moves `storage` to block `height`, then runs `f` on its ValidatorSet.
pub(super) fn at_height<R>(
    storage: &mut HashMapStorageProvider,
    height: u64,
    f: impl FnOnce(&mut ValidatorSet) -> R,
) -> R {
    storage.set_block_number(height);
    StorageHandle::enter(storage, |storage| f(&mut ValidatorSet::new(storage)))
}

/// Storage at block 1 with config_owner, config_max_validators and a 10-block
/// epoch configured.
pub(super) fn configured_storage(max: u32) -> HashMapStorageProvider {
    // Height zero is the storage sentinel for an absent lifecycle height. Keep
    // semantic transition fixtures at a real block so EXITING/INACTIVE decode
    // through the same path as production records.
    let mut storage = registry_storage(1, max).unwrap();
    StorageHandle::enter(&mut storage, |storage| {
        ValidatorSet::new(storage)
            .config_epoch_length_blocks
            .write(10)
            .unwrap();
    });
    storage
}

/// Convenience: set config_owner and config_max_validators, then run test.
pub(super) fn with_vs_configured<R>(max: u32, f: impl FnOnce(&mut ValidatorSet) -> R) -> R {
    let mut storage = configured_storage(max);
    StorageHandle::enter(&mut storage, |storage| f(&mut ValidatorSet::new(storage)))
}

/// Move a registered validator through the canonical committee-entry path.
pub(super) fn activate_for_test(vs: &mut ValidatorSet, addr: Address) {
    vs.activate_validator(addr).unwrap();
}

/// Activate through the complete stake/readiness/boundary type-state path.
pub(super) fn activate_staked_for_test(vs: &mut ValidatorSet, addr: Address) {
    let minimum = U256::from(1_000u64);
    vs.test_activate_validator_canonically(addr, StakeProjection::new(minimum, None), minimum)
        .unwrap();
}

/// Move a registered validator through a complete canonical economic exit.
pub(super) fn make_inactive_for_test(vs: &mut ValidatorSet, addr: Address) {
    activate_for_test(vs, addr);
    vs.deactivate_validator(OWNER, addr).unwrap();
    vs.activate_reshared_set(&[], B256::ZERO).unwrap();
    vs.complete_unbonding(addr).unwrap();
}

/// Generate a dummy 48-byte consensus pubkey with a unique seed byte.
pub(super) fn dummy_consensus_pubkey(seed: u8) -> [u8; 48] {
    let mut pk = [0u8; 48];
    pk[0] = seed;
    pk
}

/// The canonical OCOMP registration of `validator` signed with the seed key
/// `key_seed` for this test chain.
pub(super) fn ocomp_registration(
    validator: Address,
    consensus_pubkey: &[u8; 48],
    key_seed: u8,
) -> (OcompKeyRegistrationV1, Vec<u8>) {
    test_seeded_ocomp_registration(
        validator,
        consensus_pubkey,
        key_seed,
        (CHAIN_ID, B256::ZERO),
    )
    .unwrap()
}

pub(super) fn confirm_ready(vs: &mut ValidatorSet<'_>, validator: Address, key_seed: u8) {
    let consensus_pubkey = vs
        .get_validator(validator)
        .unwrap()
        .unwrap()
        .consensus_pubkey;
    let (_, encoded) = ocomp_registration(validator, &consensus_pubkey, key_seed);
    vs.confirm_validator_ready(validator, &encoded).unwrap();
}

/// Registers each `(validator, key seed)` pair through the owner, in order.
pub(super) fn register_validators(
    vs: &mut ValidatorSet,
    validators: &[(Address, u8)],
) -> Result<()> {
    for (validator, seed) in validators {
        vs.register_validator(OWNER, *validator, &dummy_consensus_pubkey(*seed))?;
    }
    Ok(())
}

/// Registers `validator` through the owner and activates it through the
/// production boundary hook.
pub(super) fn register_boundary_active(
    vs: &mut ValidatorSet,
    validator: Address,
    seed: u8,
) -> Result<()> {
    register_validators(vs, &[(validator, seed)])?;
    vs.activate_validator_via_boundary_for_test(validator)?;
    Ok(())
}

/// [`register_boundary_active`] with a live BLS share: a current consensus
/// participant.
pub(super) fn register_participant(
    vs: &mut ValidatorSet,
    validator: Address,
    seed: u8,
) -> Result<()> {
    register_boundary_active(vs, validator, seed)?;
    vs.val_has_bls_share.write(&validator, true)
}
