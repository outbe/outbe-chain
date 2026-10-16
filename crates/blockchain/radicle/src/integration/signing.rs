//! Current finalized committee/DKG binding for native repository partials.

pub mod accepted;
pub mod witness;

use alloy_primitives::Address;
use commonware_codec::{DecodeExt as _, Encode as _, Read as _};
use commonware_cryptography::{
    bls12381::{
        self,
        dkg::feldman_desmedt::Output,
        primitives::{
            sharing::ModeVersion,
            variant::{MinSig, PartialSignature, Variant},
        },
    },
    Hasher as _, Sha256, Signer as _, Verifier as _,
};
use commonware_utils::{ordered::Quorum as _, Participant};
use outbe_consensus::hybrid::{RepositorySigningError, VrfMaterialProvider};
use radicle::crypto::{BlsEnvelope, BlsPublicKey, RepositoryKey, RepositorySubject};

use super::publication::CheckpointOwnerSnapshot;
use super::review::{CheckpointPreparedReview, CheckpointPreparedReviewRefs};
use super::review::{PreparedPatchReview, PreparedReviewRefs, ReviewSigningContext};
use crate::manager::FinalizedPatchSigningState;
use crate::manager::FinalizedRepositoryAuthority;

const PARTIAL_NAMESPACE: &[u8] = b"outbe.radicle.partial.v1";
pub(crate) const MAX_PUBLIC_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("current authenticated repository DKG output is unavailable")]
    Unavailable,
    #[error(transparent)]
    Authority(#[from] crate::manager::ManagerError),
    #[error("DKG output does not match the trusted repository material or owner")]
    Material,
    #[error("native Review or its signed refs are not prepared for the current signing context")]
    Context,
    #[error("partial sender is not the current DKG participant")]
    Participant,
    #[error("native witness does not match the exact finalized publication receipt")]
    Accepted,
    #[error("invalid authenticated repository partial")]
    Partial,
    #[error("invalid, noncanonical or oversized repository witness bundle")]
    Witness,
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
    /// Decode the public Output component of a witness against local finalized authority.
    /// The caller also binds its material hash to the current checkpoint before publication.
    pub fn from_canonical_output(
        authority: FinalizedRepositoryAuthority,
        bytes: &[u8],
        owner: BlsPublicKey,
    ) -> Result<Self, Error> {
        Self::new(authority, decode_output(bytes)?, owner)
    }

    /// Independently verify sorted current q-partial witnesses for a reconstructed native subject.
    /// This checks cryptographic evidence, not Git contents or permission to publish.
    pub fn verify_witness(
        &self,
        actual: &RepositorySubject,
        envelope: &BlsEnvelope,
        frames: &[RepositoryPartial],
    ) -> Result<(), Error> {
        verify_native_witness(
            &self.review_context([0; 32]),
            &self.output,
            self.owner,
            actual,
            envelope,
            frames,
        )
    }

    fn authorize_current_review(
        &self,
        state: &FinalizedPatchSigningState,
        approval: &outbe_radicleregistry::PatchApprovalRecord,
        snapshot: &CheckpointOwnerSnapshot,
        context: &ReviewSigningContext,
        owner: RepositoryKey,
    ) -> Result<(), Error> {
        self.check_context(owner, context)?;
        if state.authority() != &self.authority
            || &state.approval().approval != approval
            || state.stage() != outbe_radicleregistry::schema::PATCH_EXECUTION_PENDING
            || snapshot.checkpoint().receipt() != state.checkpoint().receipt()
            || context.previous_receipt_hash != state.checkpoint().receipt().receipt_hash.0
            || self.material_hash != state.material().receipt().body.material_hash.0
            || self.players_commitment != state.material().receipt().body.players_commitment.0
        {
            return Err(Error::Context);
        }
        Ok(())
    }

    fn authorize_bootstrap(
        &self,
        state: &crate::manager::FinalizedBootstrapSigningState,
        body: &outbe_primitives::radicle_publication::PublicationBody,
    ) -> Result<(), Error> {
        use outbe_primitives::radicle_publication::PublicationAction;
        if state.authority() != &self.authority
            || body.chain_id != self.authority.chain_id()
            || body.genesis_hash != self.authority.genesis_hash()
            || body.epoch != self.authority.epoch()
            || body.material_version != self.authority.committee().vrf_material_version
            || body.owner_bls_public.as_slice() != self.owner.as_bytes()
            || body.material_hash.0 != self.material_hash
            || body.players_commitment.0 != self.players_commitment
            || body.descriptor_hash != state.descriptor().hash().map_err(|_| Error::Context)?
            || body.action != PublicationAction::Bootstrap
            || body.sequence != 1
            || !body.previous_receipt_hash.is_zero()
        {
            return Err(Error::Context);
        }
        Ok(())
    }

    /// Verify a current participant's endorsement; package permission is checked separately.
    pub fn verify_publication_attestation(
        &self,
        body: &outbe_primitives::radicle_publication::PublicationBody,
        attestation: &outbe_primitives::radicle_publication::PublicationAttestation,
    ) -> Result<(), Error> {
        use outbe_primitives::radicle_publication::PUBLICATION_NAMESPACE;
        if body.chain_id != self.authority.chain_id()
            || body.genesis_hash != self.authority.genesis_hash()
            || body.epoch != self.authority.epoch()
            || body.material_version != self.authority.committee().vrf_material_version
            || body.owner_bls_public.as_slice() != self.owner.as_bytes()
            || body.material_hash.0 != self.material_hash
            || body.players_commitment.0 != self.players_commitment
        {
            return Err(Error::Context);
        }
        let member = self
            .authority
            .committee()
            .committee
            .get(attestation.participant_index as usize)
            .ok_or(Error::Participant)?;
        let key = bls12381::PublicKey::decode(member.consensus_pubkey.as_slice())
            .map_err(|_| Error::Participant)?;
        let signature = bls12381::Signature::decode(attestation.signature.as_slice())
            .map_err(|_| Error::Partial)?;
        if !key.verify(PUBLICATION_NAMESPACE, &body.to_bytes(), &signature) {
            return Err(Error::Partial);
        }
        Ok(())
    }

    pub(crate) fn verify_relay_partial(
        &self,
        sender: &bls12381::PublicKey,
        frame: &RepositoryPartial,
    ) -> Result<(), Error> {
        verify_partial_identity(&self.review_context([0; 32]), &self.output, sender, frame)
    }

    pub fn sign_bootstrap_document(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        job: &super::bootstrap::PreparedBootstrapDocument,
        state: &crate::manager::FinalizedBootstrapSigningState,
        validator: Address,
        identity: &bls12381::PrivateKey,
    ) -> Result<RepositoryPartial, Error> {
        self.authorize_bootstrap(state, job.body())?;
        self.sign_subject(provider, job.subject(), validator, identity)
    }

    pub fn sign_bootstrap_identity(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        job: &super::bootstrap::PreparedBootstrapIdentity,
        state: &crate::manager::FinalizedBootstrapSigningState,
        validator: Address,
        identity: &bls12381::PrivateKey,
    ) -> Result<RepositoryPartial, Error> {
        self.authorize_bootstrap(state, job.body())?;
        self.sign_subject(provider, job.subject(), validator, identity)
    }

    pub fn sign_bootstrap_refs(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        job: &super::bootstrap::PreparedBootstrapRefs,
        state: &crate::manager::FinalizedBootstrapSigningState,
        validator: Address,
        identity: &bls12381::PrivateKey,
    ) -> Result<RepositoryPartial, Error> {
        self.authorize_bootstrap(state, job.body())?;
        self.sign_subject(provider, job.subject(), validator, identity)
    }

    /// Public-only verification for a contribution to the exact prepared genesis document.
    pub fn verify_bootstrap_document_partial(
        &self,
        job: &super::bootstrap::PreparedBootstrapDocument,
        state: &crate::manager::FinalizedBootstrapSigningState,
        sender: &bls12381::PublicKey,
        frame: &RepositoryPartial,
    ) -> Result<(), Error> {
        self.authorize_bootstrap(state, job.body())?;
        let verifier = VrfMaterialProvider::new(
            self.authority.committee().vrf_material_version,
            self.output.public().clone(),
            None,
        );
        self.verify_subject_partial(&verifier, job.subject(), sender, frame)
            .map(|_| ())
    }

    pub fn verify_bootstrap_identity_partial(
        &self,
        job: &super::bootstrap::PreparedBootstrapIdentity,
        state: &crate::manager::FinalizedBootstrapSigningState,
        sender: &bls12381::PublicKey,
        frame: &RepositoryPartial,
    ) -> Result<(), Error> {
        self.authorize_bootstrap(state, job.body())?;
        let verifier = VrfMaterialProvider::new(
            self.authority.committee().vrf_material_version,
            self.output.public().clone(),
            None,
        );
        self.verify_subject_partial(&verifier, job.subject(), sender, frame)
            .map(|_| ())
    }

    pub fn verify_bootstrap_refs_partial(
        &self,
        job: &super::bootstrap::PreparedBootstrapRefs,
        state: &crate::manager::FinalizedBootstrapSigningState,
        sender: &bls12381::PublicKey,
        frame: &RepositoryPartial,
    ) -> Result<(), Error> {
        self.authorize_bootstrap(state, job.body())?;
        let verifier = VrfMaterialProvider::new(
            self.authority.committee().vrf_material_version,
            self.output.public().clone(),
            None,
        );
        self.verify_subject_partial(&verifier, job.subject(), sender, frame)
            .map(|_| ())
    }

    pub fn recover_bootstrap_document(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        job: &super::bootstrap::PreparedBootstrapDocument,
        state: &crate::manager::FinalizedBootstrapSigningState,
        contributions: &[(bls12381::PublicKey, RepositoryPartial)],
    ) -> Result<BlsEnvelope, Error> {
        self.authorize_bootstrap(state, job.body())?;
        self.recover_subject(provider, job.subject(), contributions)
    }

    pub fn recover_bootstrap_identity(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        job: &super::bootstrap::PreparedBootstrapIdentity,
        state: &crate::manager::FinalizedBootstrapSigningState,
        contributions: &[(bls12381::PublicKey, RepositoryPartial)],
    ) -> Result<BlsEnvelope, Error> {
        self.authorize_bootstrap(state, job.body())?;
        self.recover_subject(provider, job.subject(), contributions)
    }

    pub fn recover_bootstrap_refs(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        job: &super::bootstrap::PreparedBootstrapRefs,
        state: &crate::manager::FinalizedBootstrapSigningState,
        contributions: &[(bls12381::PublicKey, RepositoryPartial)],
    ) -> Result<BlsEnvelope, Error> {
        self.authorize_bootstrap(state, job.body())?;
        self.recover_subject(provider, job.subject(), contributions)
    }

    pub fn sign_current_review(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        job: &CheckpointPreparedReview,
        state: &FinalizedPatchSigningState,
        validator: Address,
        identity: &bls12381::PrivateKey,
    ) -> Result<RepositoryPartial, Error> {
        self.authorize_current_review(
            state,
            job.approval(),
            job.snapshot(),
            job.signing_context(),
            job.owner(),
        )?;
        self.sign_review(provider, job, validator, identity)
    }

    pub fn sign_current_review_refs(
        &self,
        provider: &VrfMaterialProvider<MinSig>,
        job: &CheckpointPreparedReviewRefs,
        state: &FinalizedPatchSigningState,
        validator: Address,
        identity: &bls12381::PrivateKey,
    ) -> Result<RepositoryPartial, Error> {
        self.authorize_current_review(
            state,
            job.approval(),
            job.snapshot(),
            job.signing_context(),
            job.owner(),
        )?;
        self.sign_review_refs(provider, job, validator, identity)
    }

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
        let material_hash = compute_material_hash(
            authority.epoch(),
            committee.vrf_material_version,
            players_commitment,
            &output,
        )?;
        Ok(Self {
            authority,
            output,
            owner,
            players_commitment,
            material_hash,
        })
    }

    pub(crate) fn authority(&self) -> &FinalizedRepositoryAuthority {
        &self.authority
    }

    pub fn players_commitment(&self) -> [u8; 32] {
        self.players_commitment
    }
    pub fn material_hash(&self) -> [u8; 32] {
        self.material_hash
    }

    /// Canonical public DKG data only; no local share is part of `Output`.
    pub fn canonical_output(&self) -> Vec<u8> {
        self.output.encode().to_vec()
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
        let index = self.participant_for(validator, identity)?;
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

    pub(crate) fn participant_for(
        &self,
        validator: Address,
        identity: &bls12381::PrivateKey,
    ) -> Result<Participant, Error> {
        let key = identity.public_key();
        let index = self
            .output
            .players()
            .index(&key)
            .ok_or(Error::Participant)?;
        if self.authority.committee().committee[index.get() as usize].address != validator {
            return Err(Error::Participant);
        }
        Ok(index)
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
        verify_native_partial(
            &self.review_context([0; 32]),
            &self.output,
            provider,
            subject,
            sender,
            frame,
        )
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
        recover_native_subject(
            &self.review_context([0; 32]),
            &self.output,
            self.owner,
            provider,
            subject,
            contributions,
        )
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
    pub fn participant_index(&self) -> u32 {
        self.body.partial.index.get()
    }

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

fn verify_native_witness(
    context: &ReviewSigningContext,
    output: &Output<MinSig, bls12381::PublicKey>,
    owner: BlsPublicKey,
    actual: &RepositorySubject,
    envelope: &BlsEnvelope,
    frames: &[RepositoryPartial],
) -> Result<(), Error> {
    if frames.len() < output.public().required() as usize || frames.len() > output.players().len() {
        return Err(Error::Partial);
    }
    let mut previous = None;
    let contributions = frames
        .iter()
        .map(|frame| {
            let index = frame.body.partial.index;
            if previous.is_some_and(|old| old >= index) {
                return Err(Error::Partial);
            }
            previous = Some(index);
            let sender = output
                .players()
                .iter()
                .nth(index.get() as usize)
                .ok_or(Error::Participant)?;
            Ok((sender.clone(), frame.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    // A receiver needs only the authenticated public polynomial, never a local Share.
    let verifier =
        VrfMaterialProvider::new(context.material_version, output.public().clone(), None);
    if recover_native_subject(context, output, owner, &verifier, actual, &contributions)?
        != *envelope
    {
        return Err(Error::Partial);
    }
    Ok(())
}

fn verify_native_partial(
    context: &ReviewSigningContext,
    output: &Output<MinSig, bls12381::PublicKey>,
    provider: &VrfMaterialProvider<MinSig>,
    subject: &RepositorySubject,
    sender: &bls12381::PublicKey,
    frame: &RepositoryPartial,
) -> Result<PartialSignature<MinSig>, Error> {
    if frame.body.message_hash != Sha256::hash(&[subject.as_bytes()]).0 {
        return Err(Error::Context);
    }
    verify_partial_identity(context, output, sender, frame)?;
    provider.verify_repository_partial(
        context.material_version,
        output.public(),
        subject,
        &frame.body.partial,
    )?;
    Ok(frame.body.partial.clone())
}

fn verify_partial_identity(
    context: &ReviewSigningContext,
    output: &Output<MinSig, bls12381::PublicKey>,
    sender: &bls12381::PublicKey,
    frame: &RepositoryPartial,
) -> Result<(), Error> {
    if frame.body.chain_id != context.chain_id
        || frame.body.genesis_hash != context.genesis_hash
        || frame.body.epoch != context.epoch
        || frame.body.material_version != context.material_version
        || frame.body.material_hash != context.material_hash
    {
        return Err(Error::Context);
    }
    if output.players().index(sender) != Some(frame.body.partial.index) {
        return Err(Error::Participant);
    }
    if !sender.verify(PARTIAL_NAMESPACE, &frame.body.to_bytes(), &frame.signature) {
        return Err(Error::Partial);
    }
    Ok(())
}

fn recover_native_subject(
    context: &ReviewSigningContext,
    output: &Output<MinSig, bls12381::PublicKey>,
    owner: BlsPublicKey,
    provider: &VrfMaterialProvider<MinSig>,
    subject: &RepositorySubject,
    contributions: &[(bls12381::PublicKey, RepositoryPartial)],
) -> Result<BlsEnvelope, Error> {
    if contributions.len() > output.players().len() {
        return Err(Error::Partial);
    }
    let partials = contributions
        .iter()
        .map(|(sender, frame)| {
            verify_native_partial(context, output, provider, subject, sender, frame)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let signature = provider.recover_repository(
        context.material_version,
        output.public(),
        subject,
        &partials,
    )?;
    Ok(BlsEnvelope::attach(
        subject.clone(),
        owner,
        signature
            .encode()
            .to_vec()
            .try_into()
            .map_err(|_| Error::Partial)?,
    )?)
}

fn compute_material_hash(
    epoch: u64,
    version: u64,
    players_commitment: [u8; 32],
    output: &Output<MinSig, bls12381::PublicKey>,
) -> Result<[u8; 32], Error> {
    let encoded = output.encode();
    let length = u32::try_from(encoded.len()).map_err(|_| Error::Material)?;
    let material_hash = Sha256::hash(&[
        b"ODM1",
        &epoch.to_be_bytes(),
        &version.to_be_bytes(),
        &players_commitment,
        &length.to_be_bytes(),
        encoded.as_ref(),
    ])
    .0;
    Ok(material_hash)
}

fn decode_output(bytes: &[u8]) -> Result<Output<MinSig, bls12381::PublicKey>, Error> {
    if bytes.len() > MAX_PUBLIC_OUTPUT_BYTES {
        return Err(Error::Material);
    }
    let mut reader = bytes;
    let cfg = (
        std::num::NonZeroU32::new(outbe_consensus::bls::MAX_VALIDATORS).ok_or(Error::Material)?,
        ModeVersion::v0(),
    );
    let output = Output::<MinSig, bls12381::PublicKey>::read_cfg(&mut reader, &cfg)
        .map_err(|_| Error::Material)?;
    if !reader.is_empty() || output.encode().as_ref() != bytes {
        return Err(Error::Material);
    }
    Ok(output)
}
