use alloy_primitives::{Address, FixedBytes, B256, U256};
use outbe_primitives::error::Result;

use crate::{errors::RadicleRegistryError, precompile::IRadicleRegistry, schema::RadicleRegistry};

pub type RepoId = FixedBytes<20>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepositoryRegistration {
    pub index: u32,
    pub generation: u64,
}

#[must_use]
pub fn repo_id_storage_key(repo_id: RepoId) -> B256 {
    let mut word = [0_u8; 32];
    word[..20].copy_from_slice(repo_id.as_slice());
    B256::from(word)
}

pub fn repo_id_from_storage_key(key: B256) -> Result<RepoId> {
    if key.as_slice()[20..] != [0_u8; 12] {
        return Err(RadicleRegistryError::NonZeroStorageKeyTail.into());
    }
    Ok(RepoId::from_slice(&key.as_slice()[..20]))
}

impl RadicleRegistry<'_> {
    /// Only a terminal Approved Vote exposes an immutable network permission.
    /// Consumers must obtain this record from their exact finalized snapshot.
    pub fn patch_approval(&self, proposal_id: U256) -> Result<Option<crate::PatchApprovalRecord>> {
        let Some(approval) = self.patch_approvals.get(proposal_id)? else {
            return Ok(None);
        };
        let proposal = outbe_vote::Vote::new(self.storage.clone())
            .proposals
            .get(proposal_id)?
            .ok_or(RadicleRegistryError::ApprovalProposalMismatch)?;
        if proposal.proposal_status()? != outbe_vote::ProposalStatus::Approved {
            return Ok(None);
        }
        if proposal.target_module != outbe_primitives::addresses::RADICLE_REGISTRY_ADDRESS
            || proposal.payload != approval.payload
            || approval.approved_height <= proposal.voting_deadline_height
        {
            return Err(RadicleRegistryError::ApprovalProposalMismatch.into());
        }
        crate::payload::PatchVotePayloadV1::decode(approval.payload.as_bytes())?;
        Ok(Some(approval))
    }

    pub fn register_repository(
        &mut self,
        repo_id: RepoId,
        registrant: Address,
    ) -> Result<RepositoryRegistration> {
        if repo_id.is_zero() {
            return Err(RadicleRegistryError::ZeroRepositoryId.into());
        }
        let key = repo_id_storage_key(repo_id);
        if self.repository_index.read(&key)? != 0 {
            return Err(RadicleRegistryError::AlreadyRegistered.into());
        }

        let count = self.repository_count.read()?;
        let maximum = self.config_max_repositories.read()?;
        if count >= maximum {
            return Err(RadicleRegistryError::CapacityReached { maximum }.into());
        }
        let next_count = count + 1;
        let generation = self.registry_generation.read()? + 1;

        let checkpoint = self.storage.checkpoint_guard();
        self.repository_by_index.write(&count, key)?;
        self.repository_index.write(&key, next_count)?;
        self.repository_registrant.write(&key, registrant)?;
        self.repository_count.write(next_count)?;
        self.registry_generation.write(generation)?;
        self.emit(IRadicleRegistry::RepositoryRegistered {
            repoId: repo_id,
            registrant,
            index: count,
            generation,
        })?;
        checkpoint.commit();
        Ok(RepositoryRegistration {
            index: count,
            generation,
        })
    }

    pub fn repository_at(&self, index: u32) -> Result<RepoId> {
        let count = self.repository_count.read()?;
        if index >= count {
            return Err(RadicleRegistryError::IndexOutOfBounds { index, count }.into());
        }
        let key = self.repository_by_index.read(&index)?;
        if key.is_zero() || self.repository_index.read(&key)? != index + 1 {
            return Err(RadicleRegistryError::InconsistentRepositoryIndex { index }.into());
        }
        repo_id_from_storage_key(key)
    }

    pub fn registrant_of(&self, repo_id: RepoId) -> Result<Address> {
        self.repository_registrant
            .read(&repo_id_storage_key(repo_id))
    }
}
