use super::*;

pub(super) fn signing_key(index: u8) -> SigningKey {
    SigningKey::from_bytes((&[index + 1; 32]).into()).unwrap()
}

pub(super) fn ocomp_key_hash(index: u8) -> B256 {
    keccak256(
        signing_key(index)
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes(),
    )
}

pub(super) fn sign(key: &SigningKey, digest: B256) -> [u8; 64] {
    let signature: Signature = key.sign_prehash(digest.as_slice()).unwrap();
    signature.to_bytes().into()
}

pub(crate) fn founder_registrations_for_validators(
    validators: &[(Address, [u8; 48])],
    chain_id: u64,
    genesis_hash: B256,
    limits: &SchemaLimits,
) -> PrecompileResult<Vec<OcompKeyRegistrationV1>> {
    let mut registrations = Vec::with_capacity(validators.len());
    for (index, (validator, consensus_pubkey)) in validators.iter().enumerate() {
        let index = u8::try_from(index).map_err(|_| {
            PrecompileError::Fatal("test founder validator index exceeds u8".into())
        })?;
        let registration = registration_for_validator(
            index,
            (*validator, *consensus_pubkey),
            &RegistrationAuthority {
                chain_id,
                genesis_hash,
                limits,
            },
        )?;
        registrations.push(registration);
    }
    Ok(registrations)
}

pub(crate) fn seed_validator_snapshot(
    storage: StorageHandle<'_>,
    limits: &SchemaLimits,
    member_count: u8,
) -> OcompSnapshotExtensionV1 {
    assert!(member_count > 0);
    let chain_id = storage.chain_id().unwrap();
    let genesis_hash = storage.genesis_hash().unwrap();
    let owner = Address::repeat_byte(0xE0);
    let mut validators = ValidatorSet::new(storage.clone());
    validators.config_owner.write(owner).unwrap();
    validators
        .set_config_max_validators(u32::from(member_count))
        .unwrap();
    let mut snapshot_key = B256::ZERO;
    for index in 0..member_count {
        let validator = Address::repeat_byte(0xB0 + index);
        let consensus_pubkey = [0x30 + index; 48];
        validators
            .register_validator(owner, validator, &consensus_pubkey)
            .unwrap();
        validators.mark_pending(validator).unwrap();
        let registration = registration_for_validator(
            index,
            (validator, consensus_pubkey),
            &RegistrationAuthority {
                chain_id,
                genesis_hash,
                limits,
            },
        )
        .unwrap();
        validators
            .confirm_validator_ready(validator, &registration.encode_canonical(limits).unwrap())
            .unwrap();
        snapshot_key = validators
            .activate_validator_via_boundary_for_test(validator)
            .unwrap();
    }
    read_ocomp_snapshot_extension(storage, snapshot_key)
        .unwrap()
        .unwrap()
}

/// Signs one deterministic fixture-committee vote for a caller-supplied
/// persisted intent/result pair. The returned vote still has to pass the real
/// public selector and block executor.
#[must_use]
pub fn signed_result_vote_for_intent(
    intent: &JobIntentV1,
    result: &LysisResultV1,
    validator_index: u8,
    limits: &SchemaLimits,
) -> ResultVoteV1 {
    let mut vote = ResultVoteV1 {
        protocol_bundle_hash: intent.protocol_bundle_hash,
        job_id: result.job_id,
        attempt: intent.attempt,
        result_validator_set_epoch: intent.result_validator_set_epoch,
        result_committee_set_hash: intent.result_committee_set_hash,
        result_ocomp_binding_hash: intent.result_ocomp_binding_hash,
        ocomp_key_hash: ocomp_key_hash(validator_index),
        key_epoch: 1,
        result: result.clone(),
        signature_rs: [0; 64],
    };
    vote.signature_rs = sign(
        &signing_key(validator_index),
        vote.signing_digest(intent, limits).unwrap(),
    );
    vote
}

pub(super) struct RegistrationAuthority<'limits> {
    pub(super) chain_id: u64,
    pub(super) genesis_hash: B256,
    pub(super) limits: &'limits SchemaLimits,
}

pub(super) fn registration_for_validator(
    index: u8,
    validator: (Address, [u8; 48]),
    authority: &RegistrationAuthority<'_>,
) -> PrecompileResult<OcompKeyRegistrationV1> {
    let (validator, consensus_pubkey) = validator;
    let RegistrationAuthority {
        chain_id,
        genesis_hash,
        limits,
    } = *authority;
    let key = signing_key(index);
    let mut registration = OcompKeyRegistrationV1 {
        core: OcompKeyRegistrationCoreV1 {
            chain_id,
            genesis_hash,
            validator_identity_hash: validator_identity_hash_v1(validator, &consensus_pubkey)
                .map_err(|error| {
                    PrecompileError::Fatal(format!(
                        "test founder validator identity failed: {error}"
                    ))
                })?,
            ocomp_public_key_sec1: key
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .map_err(|_| {
                    PrecompileError::Fatal("test founder OCOMP key is not 33 bytes".into())
                })?,
            key_epoch: 1,
            allowed_purpose_bitmap: RESULT_SIGNATURE_PURPOSE_BITMAP,
        },
        proof_of_possession: [0; 64],
    };
    registration.proof_of_possession = sign(
        &key,
        registration
            .proof_of_possession_digest(limits)
            .map_err(|error| {
                PrecompileError::Fatal(format!("test founder PoP digest failed: {error}"))
            })?,
    );
    Ok(registration)
}
