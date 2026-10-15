//! Current finalized committee/DKG binding for native repository partials.

use alloy_primitives::Address;
use commonware_codec::{DecodeExt as _, Encode as _};
use commonware_cryptography::{
    bls12381::{
        self,
        dkg::feldman_desmedt::Output,
        primitives::variant::{MinSig, PartialSignature, Variant},
    },
    Hasher as _, Sha256, Signer as _, Verifier as _,
};
use commonware_utils::{ordered::Quorum as _, Participant};
use outbe_consensus::hybrid::{RepositorySigningError, VrfMaterialProvider};
use radicle::crypto::{BlsEnvelope, BlsPublicKey, RepositoryKey, RepositorySubject};

use super::review::{PreparedPatchReview, PreparedReviewRefs, ReviewSigningContext};
use crate::manager::FinalizedRepositoryAuthority;

const PARTIAL_NAMESPACE: &[u8] = b"outbe.radicle.partial.v1";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("DKG output differs from the current finalized committee or network owner")]
    Material,
    #[error("native Review or its signed refs are not prepared for the current signing context")]
    Context,
    #[error("partial sender is not the current DKG participant")]
    Participant,
    #[error("invalid authenticated repository partial")]
    Partial,
    #[error(transparent)]
    Signing(#[from] RepositorySigningError),
    #[error(transparent)]
    Native(#[from] radicle::crypto::repository::Error),
}

pub struct RepositorySigningMaterial {
    authority: FinalizedRepositoryAuthority,
    output: Output<MinSig, bls12381::PublicKey>,
    owner: BlsPublicKey,
    players_commitment: [u8; 32],
    material_hash: [u8; 32],
}

impl RepositorySigningMaterial {
    /// `output` is the locally authenticated canonical DKG output, never a peer's transcript.
    pub fn new(
        authority: FinalizedRepositoryAuthority,
        output: Output<MinSig, bls12381::PublicKey>,
        owner: BlsPublicKey,
    ) -> Result<Self, Error> {
        let committee = authority.committee();
        let n = committee.committee.len();
        if n == 0
            || n > outbe_consensus::bls::MAX_VALIDATORS as usize
            || output.players().len() != n
            || output.public().total().get() as usize != n
            || output.public().required() as usize != n - (n - 1) / 3
            || output.public().public().encode().as_ref() != owner.as_bytes()
            || committee.vrf_group_public_key_bytes.as_slice() != owner.as_bytes()
            || committee.vrf_public_polynomial_hash
                != outbe_consensus::dkg_manager::public_polynomial_hash(output.public())
            || output
                .players()
                .iter()
                .zip(&committee.committee)
                .any(|(key, member)| key.encode().as_ref() != member.consensus_pubkey)
        {
            return Err(Error::Material);
        }
        let players_commitment = outbe_primitives::radicle_publication::players_commitment(
            committee
                .committee
                .iter()
                .map(|member| (member.address, &member.consensus_pubkey)),
        )
        .map_err(|_| Error::Material)?
        .0;
        let encoded = output.encode();
        let length = u32::try_from(encoded.len()).map_err(|_| Error::Material)?;
        let material_hash = Sha256::hash(&[
            b"ODM1",
            &authority.epoch().to_be_bytes(),
            &committee.vrf_material_version.to_be_bytes(),
            &players_commitment,
            &length.to_be_bytes(),
            encoded.as_ref(),
        ])
        .0;
        Ok(Self {
            authority,
            output,
            owner,
            players_commitment,
            material_hash,
        })
    }

    pub fn players_commitment(&self) -> [u8; 32] {
        self.players_commitment
    }
    pub fn material_hash(&self) -> [u8; 32] {
        self.material_hash
    }

    pub fn review_context(&self, previous_receipt_hash: [u8; 32]) -> ReviewSigningContext {
        ReviewSigningContext {
            chain_id: self.authority.chain_id(),
            genesis_hash: self.authority.genesis_hash().0,
            epoch: self.authority.epoch(),
            material_version: self.authority.committee().vrf_material_version,
            material_hash: self.material_hash,
            previous_receipt_hash,
        }
    }

    fn check_context(
        &self,
        owner: RepositoryKey,
        actual: &ReviewSigningContext,
    ) -> Result<(), Error> {
        let expected = self.review_context(actual.previous_receipt_hash);
        if owner != RepositoryKey::from(self.owner)
            || actual.chain_id != expected.chain_id
            || actual.genesis_hash != expected.genesis_hash
            || actual.epoch != expected.epoch
            || actual.material_version != expected.material_version
            || actual.material_hash != expected.material_hash
        {
            return Err(Error::Context);
        }
        Ok(())
    }

    pub fn sign_review(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        review: &PreparedPatchReview,
        validator: Address,
        identity: &bls12381::PrivateKey,
    ) -> Result<RepositoryPartial, Error> {
        self.check_context(review.owner(), review.signing_context())?;
        self.sign_subject(provider, review.subject(), validator, identity)
    }

    pub fn sign_review_refs(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        refs: &PreparedReviewRefs,
        validator: Address,
        identity: &bls12381::PrivateKey,
    ) -> Result<RepositoryPartial, Error> {
        self.check_context(refs.owner(), refs.signing_context())?;
        self.sign_subject(provider, refs.subject(), validator, identity)
    }

    fn sign_subject(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        subject: &RepositorySubject,
        validator: Address,
        identity: &bls12381::PrivateKey,
    ) -> Result<RepositoryPartial, Error> {
        let key = identity.public_key();
        let index = self
            .output
            .players()
            .index(&key)
            .ok_or(Error::Participant)?;
        if self.authority.committee().committee[index.get() as usize].address != validator {
            return Err(Error::Participant);
        }
        let partial = provider.sign_repository(
            self.authority.committee().vrf_material_version,
            index,
            self.output.public(),
            subject,
        )?;
        let body = PartialBody {
            chain_id: self.authority.chain_id(),
            genesis_hash: self.authority.genesis_hash().0,
            epoch: self.authority.epoch(),
            material_version: self.authority.committee().vrf_material_version,
            material_hash: self.material_hash,
            message_hash: Sha256::hash(&[subject.as_bytes()]).0,
            partial,
        };
        let signature = identity.sign(PARTIAL_NAMESPACE, &body.to_bytes());
        Ok(RepositoryPartial { body, signature })
    }

    /// Verify before a collector stores a contribution; `sender` comes from its authenticated channel.
    pub fn verify_partial(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        review: &PreparedPatchReview,
        sender: &bls12381::PublicKey,
        frame: &RepositoryPartial,
    ) -> Result<PartialSignature<MinSig>, Error> {
        self.check_context(review.owner(), review.signing_context())?;
        self.verify_subject_partial(provider, review.subject(), sender, frame)
    }

    pub fn verify_review_refs_partial(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        refs: &PreparedReviewRefs,
        sender: &bls12381::PublicKey,
        frame: &RepositoryPartial,
    ) -> Result<PartialSignature<MinSig>, Error> {
        self.check_context(refs.owner(), refs.signing_context())?;
        self.verify_subject_partial(provider, refs.subject(), sender, frame)
    }

    fn verify_subject_partial(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        subject: &RepositorySubject,
        sender: &bls12381::PublicKey,
        frame: &RepositoryPartial,
    ) -> Result<PartialSignature<MinSig>, Error> {
        if frame.body.chain_id != self.authority.chain_id()
            || frame.body.genesis_hash != self.authority.genesis_hash().0
            || frame.body.epoch != self.authority.epoch()
            || frame.body.material_version != self.authority.committee().vrf_material_version
            || frame.body.material_hash != self.material_hash
            || frame.body.message_hash != Sha256::hash(&[subject.as_bytes()]).0
        {
            return Err(Error::Context);
        }
        if self.output.players().index(sender) != Some(frame.body.partial.index) {
            return Err(Error::Participant);
        }
        if !sender.verify(PARTIAL_NAMESPACE, &frame.body.to_bytes(), &frame.signature) {
            return Err(Error::Partial);
        }
        provider.verify_repository_partial(
            self.authority.committee().vrf_material_version,
            self.output.public(),
            subject,
            &frame.body.partial,
        )?;
        Ok(frame.body.partial.clone())
    }

    pub fn recover_review(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        review: &PreparedPatchReview,
        contributions: &[(bls12381::PublicKey, RepositoryPartial)],
    ) -> Result<BlsEnvelope, Error> {
        self.check_context(review.owner(), review.signing_context())?;
        self.recover_subject(provider, review.subject(), contributions)
    }

    pub fn recover_review_refs(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        refs: &PreparedReviewRefs,
        contributions: &[(bls12381::PublicKey, RepositoryPartial)],
    ) -> Result<BlsEnvelope, Error> {
        self.check_context(refs.owner(), refs.signing_context())?;
        self.recover_subject(provider, refs.subject(), contributions)
    }

    fn recover_subject(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        subject: &RepositorySubject,
        contributions: &[(bls12381::PublicKey, RepositoryPartial)],
    ) -> Result<BlsEnvelope, Error> {
        if contributions.len() > self.authority.committee().committee.len() {
            return Err(Error::Partial);
        }
        let partials = contributions
            .iter()
            .map(|(sender, frame)| self.verify_subject_partial(provider, subject, sender, frame))
            .collect::<Result<Vec<_>, _>>()?;
        let signature = provider.recover_repository(
            self.authority.committee().vrf_material_version,
            self.output.public(),
            subject,
            &partials,
        )?;
        Ok(BlsEnvelope::attach(
            subject.clone(),
            self.owner,
            signature
                .encode()
                .to_vec()
                .try_into()
                .map_err(|_| Error::Partial)?,
        )?)
    }
}

#[derive(Clone)]
pub struct RepositoryPartial {
    body: PartialBody,
    signature: bls12381::Signature,
}

#[derive(Clone)]
struct PartialBody {
    chain_id: u64,
    genesis_hash: [u8; 32],
    epoch: u64,
    material_version: u64,
    material_hash: [u8; 32],
    message_hash: [u8; 32],
    partial: PartialSignature<MinSig>,
}

impl PartialBody {
    // Fixed P0 fields: message hash, chain/genesis, epoch/material, index, MinSig partial.
    fn to_bytes(&self) -> Vec<u8> {
        [
            self.message_hash.as_slice(),
            &self.chain_id.to_be_bytes(),
            &self.genesis_hash,
            &self.epoch.to_be_bytes(),
            &self.material_version.to_be_bytes(),
            &self.material_hash,
            &self.partial.index.get().to_be_bytes(),
            self.partial.value.encode().as_ref(),
        ]
        .concat()
    }
}

impl RepositoryPartial {
    pub fn to_bytes(&self) -> Vec<u8> {
        [self.body.to_bytes(), self.signature.encode().to_vec()].concat()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        // 172 signing bytes + 96-byte MinPk identity signature; reject suffixes before decoding.
        if bytes.len() != 268 {
            return Err(Error::Partial);
        }
        Ok(Self {
            body: PartialBody {
                message_hash: bytes[..32].try_into().map_err(|_| Error::Partial)?,
                chain_id: u64::from_be_bytes(bytes[32..40].try_into().map_err(|_| Error::Partial)?),
                genesis_hash: bytes[40..72].try_into().map_err(|_| Error::Partial)?,
                epoch: u64::from_be_bytes(bytes[72..80].try_into().map_err(|_| Error::Partial)?),
                material_version: u64::from_be_bytes(
                    bytes[80..88].try_into().map_err(|_| Error::Partial)?,
                ),
                material_hash: bytes[88..120].try_into().map_err(|_| Error::Partial)?,
                partial: PartialSignature {
                    index: Participant::new(u32::from_be_bytes(
                        bytes[120..124].try_into().map_err(|_| Error::Partial)?,
                    )),
                    value: <MinSig as Variant>::Signature::decode(&bytes[124..172])
                        .map_err(|_| Error::Partial)?,
                },
            },
            signature: bls12381::Signature::decode(&bytes[172..]).map_err(|_| Error::Partial)?,
        })
    }
}
