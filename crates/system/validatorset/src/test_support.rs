//! Shared test setup that reaches ACTIVE state only through the production
//! certified-boundary hook.
//!
//! This module is intentionally not an alternative runtime API. It exists so
//! workspace tests do not recreate the removed manual activation transition or
//! write `status::ACTIVE` / `val_has_bls_share` directly.

use alloy_primitives::{keccak256, Address, B256};
use k256::ecdsa::{signature::hazmat::PrehashSigner as _, Signature, SigningKey};
use outbe_ocomp_protocol::{
    committee::{
        validator_identity_hash_v1, OcompKeyRegistrationCoreV1, OcompKeyRegistrationV1,
        POC_KEY_EPOCH, RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    profile::poc_schema_limits,
};
use outbe_primitives::error::{PrecompileError, Result};

use crate::{
    hooks::{activate_boundary_atomic, BoundaryActivationInputs},
    runtime::status,
    schema::ValidatorSet,
    state::{CommitteeEntry, CommitteeSnapshot},
};

const TEST_OCOMP_KEY_DOMAIN: &[u8] = b"OUTBE_TEST_OCOMP_KEY_V1";

fn test_ocomp_signing_key(identity: B256) -> Result<SigningKey> {
    let mut preimage = Vec::with_capacity(TEST_OCOMP_KEY_DOMAIN.len() + 32);
    preimage.extend_from_slice(TEST_OCOMP_KEY_DOMAIN);
    preimage.extend_from_slice(identity.as_slice());
    let mut candidate = keccak256(preimage).0;
    for _ in 0..16 {
        if let Ok(key) = SigningKey::from_bytes((&candidate).into()) {
            return Ok(key);
        }
        candidate = keccak256(candidate).0;
    }
    Err(PrecompileError::Fatal(
        "failed to derive deterministic test OCOMP signing key".into(),
    ))
}

fn test_ocomp_registration(
    validator: Address,
    consensus_pubkey: &[u8; 48],
    chain_id: u64,
    genesis_hash: B256,
) -> Result<Vec<u8>> {
    let identity = validator_identity_hash_v1(validator, consensus_pubkey).map_err(|error| {
        PrecompileError::Fatal(format!("cannot derive test validator identity: {error}"))
    })?;
    let signing_key = test_ocomp_signing_key(identity)?;
    let mut registration = OcompKeyRegistrationV1 {
        core: OcompKeyRegistrationCoreV1 {
            chain_id,
            genesis_hash,
            validator_identity_hash: identity,
            ocomp_public_key_sec1: signing_key
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .map_err(|_| {
                    PrecompileError::Fatal("test OCOMP SEC1 key is not 33 bytes".into())
                })?,
            key_epoch: POC_KEY_EPOCH,
            allowed_purpose_bitmap: RESULT_SIGNATURE_PURPOSE_BITMAP,
        },
        proof_of_possession: [0; 64],
    };
    let limits = poc_schema_limits();
    let digest = registration
        .proof_of_possession_digest(&limits)
        .map_err(|error| PrecompileError::Fatal(format!("test OCOMP PoP digest: {error}")))?;
    let signature: Signature = signing_key
        .sign_prehash(digest.as_slice())
        .map_err(|error| PrecompileError::Fatal(format!("test OCOMP PoP signing: {error}")))?;
    registration.proof_of_possession = signature
        .normalize_s()
        .unwrap_or(signature)
        .to_bytes()
        .into();
    registration
        .encode_canonical(&limits)
        .map_err(|error| PrecompileError::Fatal(format!("test OCOMP registration encode: {error}")))
}

/// Move one registered test validator through the real admission transitions,
/// stopping at confirmed PENDING so a separately-constructed boundary artifact
/// can promote it.
pub fn admit_validator_for_boundary(
    validators: &mut ValidatorSet<'_>,
    validator: Address,
) -> Result<()> {
    let record = validators
        .get_validator(validator)?
        .ok_or_else(|| PrecompileError::Revert("test validator is not registered".into()))?;
    match record.status {
        status::REGISTERED => validators.mark_pending(validator)?,
        status::PENDING | status::ACTIVE => {}
        other => {
            return Err(PrecompileError::Revert(format!(
                "test boundary activation cannot admit validator status {other}"
            )))
        }
    }

    if validators.val_status.read(&validator)? == status::PENDING
        && !validators.val_join_confirmed.read(&validator)?
    {
        let encoded = match validators.ocomp_registration(validator)? {
            Some(registration) => registration
                .encode_canonical(&poc_schema_limits())
                .map_err(|error| {
                    PrecompileError::Fatal(format!(
                        "stored test OCOMP registration encode: {error}"
                    ))
                })?,
            None => test_ocomp_registration(
                validator,
                &record.consensus_pubkey,
                validators.storage.chain_id()?,
                validators.storage.genesis_hash()?,
            )?,
        };
        validators.confirm_validator_ready(validator, &encoded)?;
    }
    Ok(())
}

/// Promote one registered test validator by running the same atomic boundary
/// hook used by production. Existing active validators remain in the incoming
/// set; participant order is the canonical BLS-public-key order.
pub fn activate_validator_via_boundary(
    validators: &mut ValidatorSet<'_>,
    validator: Address,
) -> Result<B256> {
    admit_validator_for_boundary(validators, validator)?;

    let mut incoming: Vec<_> = validators
        .get_active_consensus_set()?
        .into_iter()
        .filter(|member| member.status == status::ACTIVE)
        .collect();
    if !incoming
        .iter()
        .any(|member| member.validator_address == validator)
    {
        incoming.push(
            validators
                .get_validator(validator)?
                .ok_or_else(|| PrecompileError::Fatal("test validator disappeared".into()))?,
        );
    }
    incoming.sort_by_key(|member| member.consensus_pubkey);
    let new_active_set: Vec<Address> = incoming
        .iter()
        .map(|member| member.validator_address)
        .collect();
    let snapshot = CommitteeSnapshot {
        committee: incoming
            .iter()
            .map(|member| CommitteeEntry {
                address: member.validator_address,
                consensus_pubkey: member.consensus_pubkey,
            })
            .collect(),
        vrf_material_version: 0,
        vrf_group_public_key_bytes: Vec::new(),
        vrf_public_polynomial_hash: B256::ZERO,
    };
    let mut active_hash_preimage = Vec::with_capacity(8 + new_active_set.len() * 20);
    active_hash_preimage.extend_from_slice(&(new_active_set.len() as u64).to_be_bytes());
    for address in &new_active_set {
        active_hash_preimage.extend_from_slice(address.as_slice());
    }
    let active_set_hash = keccak256(active_hash_preimage);
    let incoming_epoch: u64 = validators
        .epoch_number
        .read()?
        .try_into()
        .map_err(|_| PrecompileError::Fatal("test epoch exceeds u64".into()))?;
    let storage = validators.storage.clone();
    let (_, incoming_key) = activate_boundary_atomic(
        storage,
        &BoundaryActivationInputs {
            outgoing: None,
            incoming_epoch,
            incoming: snapshot,
            new_active_set,
            active_set_hash,
            tee_expired_target_exclusions: Vec::new(),
        },
    )?;
    Ok(incoming_key)
}

impl ValidatorSet<'_> {
    /// Admission-only form for tests whose subject is a separately encoded
    /// certified boundary artifact.
    pub fn admit_validator_for_boundary_for_test(&mut self, validator: Address) -> Result<()> {
        admit_validator_for_boundary(self, validator)
    }

    /// Inherent form used to migrate legacy workspace fixtures without giving
    /// each crate its own activation setup.
    pub fn activate_validator_via_boundary_for_test(&mut self, validator: Address) -> Result<B256> {
        activate_validator_via_boundary(self, validator)
    }
}
