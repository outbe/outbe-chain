//! Verification of exact accepted history. This type cannot sign or authorize a new job.

use super::Error;
use crate::{
    integration::{publication::PublicationManifest, review::ReviewSigningContext},
    manager::FinalizedPublicationReceipt,
};
use radicle::crypto::{BlsEnvelope, BlsPublicKey, RepositorySubject};

/// Exact receipt-bound native verification; signing-generation proofs are checked before admission.
pub struct AcceptedPublicationVerifier {
    receipt: FinalizedPublicationReceipt,
    manifest: PublicationManifest,
    owner: BlsPublicKey,
}

impl AcceptedPublicationVerifier {
    /// Finality and current-set admission were checked by the validating chain.
    /// Historical reads require its exact native commitment, not retired shares or public Output.
    pub fn from_receipt(
        receipt: FinalizedPublicationReceipt,
        manifest: PublicationManifest,
    ) -> Result<Self, Error> {
        let body = &receipt.receipt().body;
        if receipt.receipt().receipt_hash != body.receipt_hash()
            || receipt.receipt().accepted_height > receipt.block().number
        {
            return Err(Error::Accepted);
        }
        manifest.bind_receipt(body).map_err(|_| Error::Accepted)?;
        let owner = BlsPublicKey::from_bytes(body.owner_bls_public.0)?;
        Ok(Self {
            receipt,
            manifest,
            owner,
        })
    }

    pub fn receipt(&self) -> &FinalizedPublicationReceipt {
        &self.receipt
    }
    pub fn manifest(&self) -> &PublicationManifest {
        &self.manifest
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

    /// Subjects must be reconstructed from native objects, never supplied by a peer as claims.
    pub fn verify_native(&self, actual: &[(RepositorySubject, BlsEnvelope)]) -> Result<(), Error> {
        if actual.len() != self.manifest.subjects().len() {
            return Err(Error::Accepted);
        }
        for (ordinal, ((subject, envelope), committed)) in
            actual.iter().zip(self.manifest.subjects()).enumerate()
        {
            if committed.ordinal as usize != ordinal || committed.envelope != envelope.to_hex() {
                return Err(Error::Accepted);
            }
            envelope.verify(subject, self.owner.into())?;
        }
        Ok(())
    }
}
