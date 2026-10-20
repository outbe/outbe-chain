//! Read existing Vote events; current permission is checked separately by the signer.
use super::*;
use crate::manager::{FinalizedVotePage, MAX_VOTE_PAGE};
use alloy_sol_types::SolEvent as _;
use outbe_vote::precompile::IVote;
use reth_provider::ReceiptProvider;

impl<P: StateProviderFactory + ReceiptProvider> RethSnapshotReader<P> {
    pub fn read_finalized_vote_page(
        &self,
        height: u64,
        offset: u32,
    ) -> Result<Option<FinalizedVotePage>, ManagerError> {
        let finalized = self.current_finalized_block()?;
        if height > finalized.number {
            return Ok(None);
        }
        let hash = self
            .provider
            .block_hash(height)
            .map_err(ManagerError::provider)?
            .filter(|h| !h.is_zero())
            .ok_or_else(|| ManagerError::Finality("finalized Vote block is unavailable".into()))?;
        let receipts = self
            .provider
            .receipts_by_block(hash.into())
            .map_err(ManagerError::provider)?
            .ok_or_else(|| {
                ManagerError::Snapshot("finalized Vote receipts are unavailable".into())
            })?;
        let ids = approved_proposal_ids(&receipts)?;
        let start = offset as usize;
        if start > ids.len() {
            return Err(ManagerError::Snapshot(
                "invalid finalized Vote offset".into(),
            ));
        }
        let end = start.saturating_add(MAX_VOTE_PAGE).min(ids.len());
        let state = self
            .provider
            .state_by_block_hash(finalized.hash)
            .map_err(ManagerError::provider)?;
        let mut storage = ReadOnlyStorageProvider::new_with_chain_identity(
            RethStateReader {
                state: state.as_ref(),
            },
            self.chain_id,
            self.genesis_hash,
        );
        let registry = CodeRegistry::new(StorageHandle::new(&mut storage));
        let proposals = filter_network_proposals(&registry, height, &ids[start..end])?;
        Ok(Some(FinalizedVotePage {
            chain_id: self.chain_id,
            genesis_hash: self.genesis_hash,
            block: FinalizedBlock {
                number: height,
                hash,
            },
            next_offset: if end < ids.len() {
                Some(u32::try_from(end).map_err(snapshot_error)?)
            } else {
                None
            },
            proposals,
        }))
    }
}

fn approved_proposal_ids<R: alloy_consensus::TxReceipt<Log = alloy_primitives::Log>>(
    receipts: &[R],
) -> Result<Vec<U256>, ManagerError> {
    let mut ids = Vec::new();
    for log in receipts.iter().flat_map(|r| r.logs()) {
        if log.address == outbe_primitives::addresses::VOTE_ADDRESS
            && log.data.topics().first() == Some(&IVote::ProposalApproved::SIGNATURE_HASH)
        {
            let event =
                IVote::ProposalApproved::decode_log_validate(log).map_err(snapshot_error)?;
            ids.push(event.data.proposalId);
        }
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

fn filter_network_proposals(
    registry: &CodeRegistry<'_>,
    height: u64,
    ids: &[U256],
) -> Result<Vec<U256>, ManagerError> {
    let network_repository = registry.network_repository.read().map_err(snapshot_error)?;
    let mut proposals = Vec::new();
    for id in ids {
        if let Some(approval) = registry.patch_approval(*id).map_err(snapshot_error)? {
            if approval.approved_height != height || approval.proposal_id != *id {
                return Err(ManagerError::Snapshot(
                    "Vote event and approval disagree".into(),
                ));
            }
            let subject = outbe_coderegistry::payload::PatchVotePayloadV1::decode(
                approval.payload.as_bytes(),
            )
            .map_err(snapshot_error)?;
            if outbe_coderegistry::repo_id_storage_key(subject.repo_id) == network_repository {
                proposals.push(*id);
            }
        }
    }
    Ok(proposals)
}
