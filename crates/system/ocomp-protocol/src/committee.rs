use std::collections::BTreeSet;

use alloy_primitives::{Address, B256};
use k256::ecdsa::{signature::hazmat::PrehashVerifier, Signature, VerifyingKey};

use crate::{
    error::ProtocolError,
    hash::hash_framed,
    registry::HashDomain,
    schema::{encode_nested_value, impl_top_level_codec, require, wire_struct, SchemaLimits},
};

pub const POC_KEY_EPOCH: u64 = 1;
pub const RESULT_SIGNATURE_PURPOSE_BITMAP: u32 = 1;

/// Canonical OCOMP identity of one consensus validator.
///
/// The preimage deliberately excludes committee position: reordering an
/// otherwise identical ValidatorSet does not create a different validator.
pub fn validator_identity_hash_v1(
    validator_address: Address,
    consensus_bls_min_pk: &[u8; 48],
) -> Result<B256, ProtocolError> {
    let mut payload = Vec::with_capacity(20 + 48);
    payload.extend_from_slice(validator_address.as_slice());
    payload.extend_from_slice(consensus_bls_min_pk);
    hash_framed(HashDomain::ValidatorIdentity, &payload)
}

wire_struct! {
    pub struct OcompMemberV1 {
        pub validator_index: u16,
        pub validator_identity_hash: B256,
        pub ocomp_public_key_sec1: [u8; 33],
        pub key_epoch: u64,
        pub allowed_purpose_bitmap: u32,
        pub valid_from_height: u64,
        pub valid_until_height_exclusive: u64,
        pub proof_of_possession: [u8; 64],
    }
}

wire_struct! {
    pub struct OcompCommitteeSnapshotV1 {
        pub chain_id: u64,
        pub genesis_hash: B256,
        pub fork_id: B256,
        pub protocol_bundle_hash: B256,
        pub snapshot_epoch: u64,
        pub threshold: u16,
        pub ordered_members: Vec<OcompMemberV1>,
    }
    validate = validate_committee;
}
impl_top_level_codec!(OcompCommitteeSnapshotV1, OcompCommitteeSnapshotV1);

wire_struct! {
    pub struct OcompKeyRegistrationCoreV1 {
        pub chain_id: u64,
        pub genesis_hash: B256,
        pub validator_identity_hash: B256,
        pub ocomp_public_key_sec1: [u8; 33],
        pub key_epoch: u64,
        pub allowed_purpose_bitmap: u32,
    }
}

wire_struct! {
    pub struct OcompKeyRegistrationV1 {
        pub core: OcompKeyRegistrationCoreV1,
        pub proof_of_possession: [u8; 64],
    }
    validate = validate_key_registration;
}
impl_top_level_codec!(OcompKeyRegistrationV1, OcompKeyRegistrationV1);

impl OcompMemberV1 {
    #[must_use]
    pub fn registration_core(
        &self,
        snapshot: &OcompCommitteeSnapshotV1,
    ) -> OcompKeyRegistrationCoreV1 {
        OcompKeyRegistrationCoreV1 {
            chain_id: snapshot.chain_id,
            genesis_hash: snapshot.genesis_hash,
            validator_identity_hash: self.validator_identity_hash,
            ocomp_public_key_sec1: self.ocomp_public_key_sec1,
            key_epoch: self.key_epoch,
            allowed_purpose_bitmap: self.allowed_purpose_bitmap,
        }
    }
}

impl OcompKeyRegistrationV1 {
    pub fn proof_of_possession_digest(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        hash_framed(
            HashDomain::KeyPop,
            &encode_nested_value(&self.core, limits)?,
        )
    }

    pub fn validate_proof_of_possession(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
        require(self.core.key_epoch == POC_KEY_EPOCH, "PoC OCOMP key epoch")?;
        require(
            self.core.allowed_purpose_bitmap == RESULT_SIGNATURE_PURPOSE_BITMAP,
            "result-signature-only key purpose",
        )?;
        let digest = self.proof_of_possession_digest(limits)?;
        verify_low_s_prehash(
            &self.core.ocomp_public_key_sec1,
            digest,
            &self.proof_of_possession,
        )
    }
}

impl OcompCommitteeSnapshotV1 {
    pub fn validate_semantics(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
        require(
            !self.ordered_members.is_empty()
                && usize::from(self.threshold) <= self.ordered_members.len()
                && self.threshold > 0,
            "committee threshold",
        )?;
        let mut identities = BTreeSet::new();
        let mut keys = BTreeSet::new();
        for (index, member) in self.ordered_members.iter().enumerate() {
            require(
                usize::from(member.validator_index) == index,
                "committee index order",
            )?;
            require(
                identities.insert(member.validator_identity_hash),
                "unique validator identity",
            )?;
            require(
                keys.insert(member.ocomp_public_key_sec1),
                "unique OCOMP key",
            )?;
            require(
                member.valid_from_height < member.valid_until_height_exclusive,
                "committee member validity range",
            )?;
            OcompKeyRegistrationV1 {
                core: member.registration_core(self),
                proof_of_possession: member.proof_of_possession,
            }
            .validate_proof_of_possession(limits)?;
        }
        Ok(())
    }

    pub fn snapshot_hash(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        self.validate_semantics(limits)?;
        hash_framed(HashDomain::Committee, &self.encode_canonical(limits)?)
    }
}

pub fn verify_low_s_prehash(
    public_key_sec1: &[u8; 33],
    digest: B256,
    signature_rs: &[u8; 64],
) -> Result<(), ProtocolError> {
    let key = VerifyingKey::from_sec1_bytes(public_key_sec1)
        .map_err(|_| ProtocolError::InvalidPublicKey)?;
    let signature =
        Signature::from_slice(signature_rs).map_err(|_| ProtocolError::InvalidSignature)?;
    if signature.normalize_s().is_some() {
        return Err(ProtocolError::HighSignatureS);
    }
    key.verify_prehash(digest.as_slice(), &signature)
        .map_err(|_| ProtocolError::InvalidSignature)
}

fn validate_committee(
    snapshot: &OcompCommitteeSnapshotV1,
    limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    snapshot.validate_semantics(limits)
}

fn validate_key_registration(
    registration: &OcompKeyRegistrationV1,
    limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    registration.validate_proof_of_possession(limits)
}
