//! Current identity attestations authorize exact publication checkpoints, never Git I/O.

use alloy_primitives::{B256, U256};
use commonware_codec::DecodeExt as _;
use commonware_cryptography::{bls12381, Hasher as _, Sha256, Verifier as _};
use outbe_primitives::{
    error::Result,
    radicle_publication::{
        PublicationAction, PublicationBatch, PublicationBody, PUBLICATION_BODY_LEN,
        PUBLICATION_NAMESPACE,
    },
    storage::types::StorageKey as _,
};
use outbe_validatorset::state::CommitteeSnapshot;

use crate::{
    errors::CodeRegistryError::InvalidPublication,
    repo_id_storage_key,
    schema::{
        PublicationReceiptRecord, PublicationReceiptRecordEntryExt, PATCH_EXECUTION_MERGED,
        PATCH_EXECUTION_REVIEWED,
    },
    CodeRegistry,
};

mod validate;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationReceipt {
    pub body: PublicationBody,
    pub accepted_height: u64,
    pub receipt_hash: B256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationProgress {
    pub attestations: u16,
    pub receipt: Option<PublicationReceipt>,
}

fn pending_generation(body: &PublicationBody, committee: &CommitteeSnapshot) -> B256 {
    Sha256::hash(&[
        b"OPG1",
        &body.epoch.to_be_bytes(),
        &body.material_version.to_be_bytes(),
        committee.vrf_public_polynomial_hash.as_slice(),
        body.players_commitment.as_slice(),
        &body.sequence.to_be_bytes(),
    ])
    .0
    .into()
}

impl CodeRegistry<'_> {
    /// Read current-generation delivery progress only; this grants no publication permission.
    pub fn pending_publication_attesters(&self, body: &PublicationBody) -> Result<Vec<u32>> {
        if body.chain_id != self.storage.chain_id()?
            || body.genesis_hash != self.storage.genesis_hash()?
        {
            return Err(InvalidPublication("progress chain identity").into());
        }
        let committee = self.publication_committee(body)?;
        let generation = pending_generation(body, &committee);
        let hash = body.receipt_hash();
        let mut indices = Vec::new();
        for index in 0..committee.committee.len() as u32 {
            if self.publication_pending_generation.read(&index)? == generation
                && self.publication_pending_body_hash.read(&index)? == hash
            {
                indices.push(index);
            }
        }
        Ok(indices)
    }

    pub fn publication_receipt(&self, sequence: u64) -> Result<Option<PublicationReceipt>> {
        let entry = self.publication_receipts.entry(sequence);
        if entry.accepted_height().read()? == 0 {
            return Ok(None);
        }
        // Bound the raw Solidity bytes word before the generated dynamic record loader.
        let body_slot = sequence.mapping_slot(self.publication_receipts.base_slot());
        let raw = self
            .storage
            .sload(self.publication_receipts.address(), body_slot)?;
        if raw != U256::from(PUBLICATION_BODY_LEN * 2 + 1) {
            return Err(InvalidPublication("stored receipt length").into());
        }
        let record = self
            .publication_receipts
            .get(sequence)?
            .ok_or(InvalidPublication("stored receipt missing"))?;
        let body = PublicationBody::from_bytes(&record.body)?;
        if body.sequence != sequence || body.receipt_hash() != record.receipt_hash {
            return Err(InvalidPublication("stored receipt hash").into());
        }
        Ok(Some(PublicationReceipt {
            body,
            accepted_height: record.accepted_height,
            receipt_hash: record.receipt_hash,
        }))
    }

    /// Finality/ancestry admission is the validating node's responsibility. This
    /// checks the deterministic chain state and canonical anchor, not finality.
    pub fn apply_publication_batch(
        &mut self,
        batch: &PublicationBatch,
    ) -> Result<PublicationProgress> {
        // Also enforce wire constraints for typed callers that bypass decoding.
        batch.to_bytes()?;
        let body = &batch.body;
        let committee = self.publication_committee(body)?;
        self.validate_publication_body(body)?;
        let body_hash = body.receipt_hash();
        let generation = pending_generation(body, &committee);
        let encoded = body.to_bytes();
        let mut fresh = Vec::with_capacity(batch.attestations.len());
        for contribution in &batch.attestations {
            let index = contribution.participant_index;
            let member = committee
                .committee
                .get(index as usize)
                .ok_or(InvalidPublication("participant index"))?;
            let key = bls12381::PublicKey::decode(member.consensus_pubkey.as_slice())
                .map_err(|_| InvalidPublication("participant key"))?;
            let signature = bls12381::Signature::decode(contribution.signature.as_slice())
                .map_err(|_| InvalidPublication("identity signature encoding"))?;
            if !key.verify(PUBLICATION_NAMESPACE, &encoded, &signature) {
                return Err(InvalidPublication("identity signature").into());
            }
            let previous_generation = self.publication_pending_generation.read(&index)?;
            if previous_generation == generation {
                if self.publication_pending_body_hash.read(&index)? != body_hash {
                    return Err(
                        InvalidPublication("participant already attested another body").into(),
                    );
                }
            } else {
                fresh.push(index);
            }
        }

        // Every signature, freshness condition and conflicting slot passed before writes.
        let checkpoint = self.storage.checkpoint_guard();
        for index in fresh {
            self.publication_pending_generation
                .write(&index, generation)?;
            self.publication_pending_body_hash
                .write(&index, body_hash)?;
        }
        let mut attestations = 0;
        for index in 0..committee.committee.len() as u32 {
            if self.publication_pending_generation.read(&index)? == generation
                && self.publication_pending_body_hash.read(&index)? == body_hash
            {
                attestations += 1;
            }
        }
        let n = committee.committee.len();
        let required = n - (n - 1) / 3;
        let receipt = if attestations as usize >= required {
            self.commit_publication(body, body_hash)?;
            Some(PublicationReceipt {
                body: body.clone(),
                accepted_height: self.storage.block_number()?,
                receipt_hash: body_hash,
            })
        } else {
            None
        };
        checkpoint.commit();
        Ok(PublicationProgress {
            attestations,
            receipt,
        })
    }

    fn commit_publication(&mut self, body: &PublicationBody, hash: B256) -> Result<()> {
        if body.action == PublicationAction::Bootstrap {
            self.network_repository
                .write(repo_id_storage_key(body.repo_id))?;
            if self
                .repository_index
                .read(&repo_id_storage_key(body.repo_id))?
                == 0
            {
                self.register_repository(
                    body.repo_id,
                    outbe_primitives::addresses::SYSTEM_ADDRESS,
                )?;
            }
        }
        match body.action {
            PublicationAction::Bootstrap | PublicationAction::BindMaterial => {
                self.publication_material_sequence.write(body.sequence)?;
            }
            PublicationAction::Review => self
                .patch_execution_stage
                .write(&body.proposal_id, PATCH_EXECUTION_REVIEWED)?,
            PublicationAction::Merge => self
                .patch_execution_stage
                .write(&body.proposal_id, PATCH_EXECUTION_MERGED)?,
        }
        self.publication_receipts
            .create(&PublicationReceiptRecord {
                sequence: body.sequence,
                body: body.to_bytes().to_vec(),
                accepted_height: self.storage.block_number()?,
                receipt_hash: hash,
            })?;
        self.publication_sequence.write(body.sequence)?;
        self.publication_receipt_hash.write(hash)?;
        Ok(())
    }
}
