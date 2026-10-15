//! Verification of exact accepted history. This type cannot sign or authorize a new job.

use super::{
    compute_material_hash, decode_output, verify_native_witness, Error, RepositoryPartial,
};
use crate::{
    integration::{publication::PublicationManifest, review::ReviewSigningContext},
    manager::FinalizedPublicationReceipt,
};
use commonware_codec::Encode as _;
use commonware_cryptography::bls12381::{
    self, dkg::feldman_desmedt::Output, primitives::variant::MinSig,
};
use radicle::crypto::{BlsEnvelope, BlsPublicKey, RepositorySubject};

/// Public witness verification for exact accepted envelopes; callers also check native objects.
pub struct AcceptedPublicationVerifier {
    receipt: FinalizedPublicationReceipt,
    manifest: PublicationManifest,
    output: Output<MinSig, bls12381::PublicKey>,
    owner: BlsPublicKey,
}

impl AcceptedPublicationVerifier {
    /// Receipt finality/current-set admission was checked by the validating chain.
    /// Its committed Output hash permits historical verification without current-key substitution.
    pub fn from_receipt(
        receipt: FinalizedPublicationReceipt,
        manifest: PublicationManifest,
        public_output: &[u8],
    ) -> Result<Self, Error> {
        let body = &receipt.receipt().body;
        if receipt.receipt().receipt_hash != body.receipt_hash()
            || receipt.receipt().accepted_height > receipt.block().number
        {
            return Err(Error::Accepted);
        }
        manifest.bind_receipt(body).map_err(|_| Error::Accepted)?;
        let output = decode_output(public_output)?;
        let owner = BlsPublicKey::from_bytes(body.owner_bls_public.0)?;
        let n = output.players().len();
        if n == 0
            || n > outbe_consensus::bls::MAX_VALIDATORS as usize
            || output.public().total().get() as usize != n
            || output.public().required() as usize != n - (n - 1) / 3
            || output.public().public().encode().as_ref() != owner.as_bytes()
            || compute_material_hash(
                body.epoch,
                body.material_version,
                body.players_commitment.0,
                &output,
            )? != body.material_hash.0
        {
            return Err(Error::Material);
        }
        Ok(Self {
            receipt,
            manifest,
            output,
            owner,
        })
    }

    pub fn receipt(&self) -> &FinalizedPublicationReceipt {
        &self.receipt
    }
    pub fn manifest(&self) -> &PublicationManifest {
        &self.manifest
    }
    pub fn canonical_output(&self) -> Vec<u8> {
        self.output.encode().to_vec()
    }
    pub fn review_context(&self) -> ReviewSigningContext {
        let body = &self.receipt.receipt().body;
        ReviewSigningContext {
            chain_id: body.chain_id,
            genesis_hash: body.genesis_hash.0,
            epoch: body.epoch,
            material_version: body.material_version,
            material_hash: body.material_hash.0,
            previous_receipt_hash: body.previous_receipt_hash.0,
        }
    }

    pub(crate) fn verify_witness(
        &self,
        ordinal: usize,
        actual: &RepositorySubject,
        envelope: &BlsEnvelope,
        frames: &[RepositoryPartial],
    ) -> Result<(), Error> {
        let committed = self
            .manifest
            .subjects()
            .get(ordinal)
            .ok_or(Error::Accepted)?;
        if committed.ordinal as usize != ordinal || committed.envelope != envelope.to_hex() {
            return Err(Error::Accepted);
        }
        verify_native_witness(
            &self.review_context(),
            &self.output,
            self.owner,
            actual,
            envelope,
            frames,
        )
    }
}
