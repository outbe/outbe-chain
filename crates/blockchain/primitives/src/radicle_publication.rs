//! Fixed publication checkpoint bytes shared by signing, block carrier and execution.

use alloy_primitives::{FixedBytes, B256, U256};
use commonware_cryptography::{Hasher as _, Sha256};
use std::sync::{Arc, Mutex};

use crate::error::{PrecompileError, Result};

pub const PUBLICATION_BODY_LEN: usize = 477;
pub const MAX_PUBLICATION_ATTESTATIONS: usize = 32;
pub const PUBLICATION_ATTESTATION_LEN: usize = 100;
pub const MAX_PUBLICATION_BATCH_LEN: usize =
    PUBLICATION_BODY_LEN + 1 + MAX_PUBLICATION_ATTESTATIONS * PUBLICATION_ATTESTATION_LEN;
pub const PUBLICATION_NAMESPACE: &[u8] = b"outbe.radicle.publication.v1";

/// One carrier-sized ready batch. The collector replaces it from finalized state;
/// proposal reads never consume it, so abandoned candidates cannot lose signatures.
#[derive(Clone, Default)]
pub struct RepositoryPublicationMailbox(Arc<Mutex<Option<PublicationBatch>>>);

impl RepositoryPublicationMailbox {
    pub fn set_ready(&self, batch: Option<PublicationBatch>) -> Result<()> {
        if let Some(batch) = &batch {
            batch.to_bytes()?;
        }
        *self
            .0
            .lock()
            .map_err(|_| PrecompileError::revert("publication mailbox poisoned"))? = batch;
        Ok(())
    }

    /// Largest prefix fitting a payload budget, without waiting for the collector.
    pub fn ready_prefix(&self, payload_budget: usize) -> Option<PublicationBatch> {
        let state = self.0.try_lock().ok()?;
        let batch = state.as_ref()?;
        let count =
            payload_budget.saturating_sub(PUBLICATION_BODY_LEN + 1) / PUBLICATION_ATTESTATION_LEN;
        let count = count.min(batch.attestations.len());
        (count > 0).then(|| PublicationBatch {
            body: batch.body.clone(),
            attestations: batch.attestations[..count].to_vec(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PublicationAction {
    Bootstrap = 0,
    BindMaterial = 1,
    Review = 2,
    Merge = 3,
}

impl TryFrom<u8> for PublicationAction {
    type Error = PrecompileError;
    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Bootstrap),
            1 => Ok(Self::BindMaterial),
            2 => Ok(Self::Review),
            3 => Ok(Self::Merge),
            _ => Err(PrecompileError::revert(
                "unknown repository publication action",
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationBody {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub repo_id: FixedBytes<20>,
    pub descriptor_hash: B256,
    pub owner_bls_public: FixedBytes<96>,
    pub players_commitment: B256,
    pub epoch: u64,
    pub material_version: u64,
    pub material_hash: B256,
    pub finalized_anchor_height: u64,
    pub finalized_anchor_hash: B256,
    pub sequence: u64,
    pub previous_receipt_hash: B256,
    pub action: PublicationAction,
    pub proposal_id: U256,
    pub patch_id: FixedBytes<20>,
    pub revision_id: FixedBytes<20>,
    pub expected_target_head: FixedBytes<20>,
    pub package_manifest_hash: B256,
}

impl PublicationBody {
    pub fn to_bytes(&self) -> [u8; PUBLICATION_BODY_LEN] {
        let mut out = [0; PUBLICATION_BODY_LEN];
        let mut offset = 0;
        let mut write = |value: &[u8]| {
            out[offset..offset + value.len()].copy_from_slice(value);
            offset += value.len();
        };
        write(b"OPB1");
        write(&self.chain_id.to_be_bytes());
        write(self.genesis_hash.as_slice());
        write(self.repo_id.as_slice());
        write(self.descriptor_hash.as_slice());
        write(self.owner_bls_public.as_slice());
        write(self.players_commitment.as_slice());
        write(&self.epoch.to_be_bytes());
        write(&self.material_version.to_be_bytes());
        write(self.material_hash.as_slice());
        write(&self.finalized_anchor_height.to_be_bytes());
        write(self.finalized_anchor_hash.as_slice());
        write(&self.sequence.to_be_bytes());
        write(self.previous_receipt_hash.as_slice());
        write(&[self.action as u8]);
        write(&self.proposal_id.to_be_bytes::<32>());
        write(self.patch_id.as_slice());
        write(self.revision_id.as_slice());
        write(self.expected_target_head.as_slice());
        write(self.package_manifest_hash.as_slice());
        debug_assert_eq!(offset, PUBLICATION_BODY_LEN);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != PUBLICATION_BODY_LEN || &bytes[..4] != b"OPB1" {
            return Err(PrecompileError::revert(
                "invalid repository publication body",
            ));
        }
        let mut input = &bytes[4..];
        let body = Self {
            chain_id: u64::from_be_bytes(take(&mut input)?),
            genesis_hash: take::<32>(&mut input)?.into(),
            repo_id: take::<20>(&mut input)?.into(),
            descriptor_hash: take::<32>(&mut input)?.into(),
            owner_bls_public: take::<96>(&mut input)?.into(),
            players_commitment: take::<32>(&mut input)?.into(),
            epoch: u64::from_be_bytes(take(&mut input)?),
            material_version: u64::from_be_bytes(take(&mut input)?),
            material_hash: take::<32>(&mut input)?.into(),
            finalized_anchor_height: u64::from_be_bytes(take(&mut input)?),
            finalized_anchor_hash: take::<32>(&mut input)?.into(),
            sequence: u64::from_be_bytes(take(&mut input)?),
            previous_receipt_hash: take::<32>(&mut input)?.into(),
            action: PublicationAction::try_from(take::<1>(&mut input)?[0])?,
            proposal_id: U256::from_be_bytes(take::<32>(&mut input)?),
            patch_id: take::<20>(&mut input)?.into(),
            revision_id: take::<20>(&mut input)?.into(),
            expected_target_head: take::<20>(&mut input)?.into(),
            package_manifest_hash: take::<32>(&mut input)?.into(),
        };
        if !input.is_empty() {
            return Err(PrecompileError::revert(
                "trailing repository publication body bytes",
            ));
        }
        Ok(body)
    }

    pub fn receipt_hash(&self) -> B256 {
        Sha256::hash(&[&self.to_bytes()]).0.into()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationAttestation {
    pub participant_index: u32,
    pub signature: FixedBytes<96>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationBatch {
    pub body: PublicationBody,
    pub attestations: Vec<PublicationAttestation>,
}

impl PublicationBatch {
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        validate_indices(&self.attestations)?;
        let mut out = Vec::with_capacity(
            PUBLICATION_BODY_LEN + 1 + self.attestations.len() * PUBLICATION_ATTESTATION_LEN,
        );
        out.extend_from_slice(&self.body.to_bytes());
        out.push(self.attestations.len() as u8);
        for entry in &self.attestations {
            out.extend_from_slice(&entry.participant_index.to_be_bytes());
            out.extend_from_slice(entry.signature.as_slice());
        }
        Ok(out)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let count = bytes
            .get(PUBLICATION_BODY_LEN)
            .copied()
            .ok_or_else(|| PrecompileError::revert("truncated repository publication batch"))?
            as usize;
        if count == 0
            || count > MAX_PUBLICATION_ATTESTATIONS
            || bytes.len() != PUBLICATION_BODY_LEN + 1 + count * PUBLICATION_ATTESTATION_LEN
        {
            return Err(PrecompileError::revert(
                "invalid repository publication batch length/count",
            ));
        }
        let body = PublicationBody::from_bytes(&bytes[..PUBLICATION_BODY_LEN])?;
        let mut input = &bytes[PUBLICATION_BODY_LEN + 1..];
        let mut attestations = Vec::with_capacity(count);
        for _ in 0..count {
            attestations.push(PublicationAttestation {
                participant_index: u32::from_be_bytes(take(&mut input)?),
                signature: take::<96>(&mut input)?.into(),
            });
        }
        validate_indices(&attestations)?;
        Ok(Self { body, attestations })
    }
}

fn validate_indices(entries: &[PublicationAttestation]) -> Result<()> {
    if entries.is_empty()
        || entries.len() > MAX_PUBLICATION_ATTESTATIONS
        || entries
            .windows(2)
            .any(|pair| pair[0].participant_index >= pair[1].participant_index)
    {
        return Err(PrecompileError::revert(
            "publication requires 1..=32 strictly ordered participant indices",
        ));
    }
    Ok(())
}

fn take<const N: usize>(input: &mut &[u8]) -> Result<[u8; N]> {
    let (head, tail) = input
        .split_at_checked(N)
        .ok_or_else(|| PrecompileError::revert("truncated repository publication field"))?;
    *input = tail;
    head.try_into()
        .map_err(|_| PrecompileError::revert("invalid repository publication field"))
}

#[cfg(test)]
pub(crate) mod tests;
