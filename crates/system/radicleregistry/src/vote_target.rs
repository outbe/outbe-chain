//! Network decisions on exact patch revisions; no Git, signing or network I/O.

use alloy_primitives::{Address, U256};
use outbe_primitives::{
    addresses::RADICLE_REGISTRY_ADDRESS, block::BlockRuntimeContext, error::Result,
    storage::StorageHandle,
};
use outbe_vote::{
    handlers::{TargetElectorate, TargetExecutionOutcome, VoteTarget, VoteTargetContext},
    schema::{ProposalStatus, Vote},
};

use crate::{
    errors::RadicleRegistryError, payload::PatchVotePayloadV1, repo_id_storage_key,
    PatchApprovalRecord, RadicleRegistry,
};

pub struct RadiclePatchVoteTarget;

impl VoteTarget for RadiclePatchVoteTarget {
    fn target_module(&self) -> Address {
        RADICLE_REGISTRY_ADDRESS
    }

    fn electorate(&self) -> TargetElectorate {
        TargetElectorate::ActiveConsensus
    }

    fn validate(&self, payload: &[u8], _context: VoteTargetContext) -> Result<()> {
        PatchVotePayloadV1::decode(payload).map(|_| ())
    }

    fn reserve(
        &self,
        storage: StorageHandle<'_>,
        _proposal_id: U256,
        payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<()> {
        let subject = PatchVotePayloadV1::decode(payload)?;
        if RadicleRegistry::new(storage)
            .repository_index
            .read(&repo_id_storage_key(subject.repo_id))?
            == 0
        {
            return Err(RadicleRegistryError::UnknownRepository.into());
        }
        Ok(())
    }

    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        payload: &[u8],
        context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        let subject = PatchVotePayloadV1::decode(payload)?;
        let proposal = Vote::new(ctx.storage.clone())
            .proposals
            .get(proposal_id)?
            .ok_or(RadicleRegistryError::ApprovalProposalMismatch)?;
        if proposal.target_module != RADICLE_REGISTRY_ADDRESS
            || proposal.payload.as_bytes() != payload
            || proposal.proposer != context.proposer
            || !matches!(
                proposal.proposal_status()?,
                ProposalStatus::Pending | ProposalStatus::Approved
            )
            || ctx.block.block_number <= proposal.voting_deadline_height
        {
            return Err(RadicleRegistryError::ApprovalProposalMismatch.into());
        }
        let registry = RadicleRegistry::new(ctx.storage.clone());
        if registry
            .repository_index
            .read(&repo_id_storage_key(subject.repo_id))?
            == 0
        {
            return Err(RadicleRegistryError::UnknownRepository.into());
        }
        if let Some(existing) = registry.patch_approvals.get(proposal_id)? {
            if existing.payload.as_bytes() != payload {
                return Err(RadicleRegistryError::ImmutableApproval.into());
            }
            return Ok(TargetExecutionOutcome::Applied);
        }
        registry.patch_approvals.create(&PatchApprovalRecord {
            proposal_id,
            payload: proposal.payload,
            approved_height: ctx.block.block_number,
            approved_timestamp: ctx.block.timestamp,
        })?;
        registry
            .patch_execution_stage
            .write(&proposal_id, crate::schema::PATCH_EXECUTION_PENDING)?;
        Ok(TargetExecutionOutcome::Applied)
    }
}
