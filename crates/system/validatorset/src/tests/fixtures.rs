use super::*;

pub(super) const CHAIN_ID: u64 = 1;

/// Owner address used across tests.
pub(super) const OWNER: Address = address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");

/// Convenience: set config_owner and config_max_validators, then run test.
pub(super) fn with_vs_configured<R>(max: u32, f: impl FnOnce(&mut ValidatorSet) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    // Height zero is the storage sentinel for an absent lifecycle height. Keep
    // semantic transition fixtures at a real block so EXITING/INACTIVE decode
    // through the same path as production records.
    storage.set_block_number(1);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(max).unwrap();
        vs.config_epoch_length_blocks.write(10).unwrap();
        f(&mut vs)
    })
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

pub(super) fn ocomp_registration(
    validator: Address,
    consensus_pubkey: &[u8; 48],
    key_seed: u8,
) -> (OcompKeyRegistrationV1, Vec<u8>) {
    let signing_key = SigningKey::from_bytes((&[key_seed; 32]).into()).unwrap();
    let ocomp_public_key_sec1 = signing_key
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let mut registration = OcompKeyRegistrationV1 {
        core: OcompKeyRegistrationCoreV1 {
            chain_id: CHAIN_ID,
            genesis_hash: B256::ZERO,
            validator_identity_hash: validator_identity_hash_v1(validator, consensus_pubkey)
                .unwrap(),
            ocomp_public_key_sec1,
            key_epoch: POC_KEY_EPOCH,
            allowed_purpose_bitmap: RESULT_SIGNATURE_PURPOSE_BITMAP,
        },
        proof_of_possession: [0; 64],
    };
    let limits = poc_schema_limits();
    let digest = registration.proof_of_possession_digest(&limits).unwrap();
    let signature: Signature = signing_key.sign_prehash(digest.as_slice()).unwrap();
    registration.proof_of_possession = signature
        .normalize_s()
        .unwrap_or(signature)
        .to_bytes()
        .into();
    let encoded = registration.encode_canonical(&limits).unwrap();
    (registration, encoded)
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
