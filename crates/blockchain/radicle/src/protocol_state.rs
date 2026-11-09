//! Chain facts the radicle manager reads from finalized state.
//!
//! Snapshot code keeps the signing checks. This trait supplies registry rows,
//! validator-set bindings, committee snapshots, and approved Vote log ids.
//! Callers pass the installed reader. [`UninstalledRadicleState`] fails closed.

use alloy_primitives::{Address, Log, B256, U256};
use outbe_primitives::code_registry::{PatchApprovalRecord, PublicationReceipt, RepoId};
use outbe_primitives::committee_snapshot::CommitteeSnapshot;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::radicle_genesis::GovernmentsDescriptor;
use outbe_primitives::radicle_publication::PublicationBody;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::validator_dto::{EpochSnapshot, ValidatorRecord};
use thiserror::Error;

/// Chain-read or Vote-log failure. Display matches the source error.
#[derive(Debug, Error)]
pub enum RadicleProtocolError {
    /// Storage or precompile read failed.
    #[error("{0}")]
    State(#[from] PrecompileError),
    /// An approved-proposal log did not decode.
    #[error("{0}")]
    VoteLog(String),
}

/// Object-safe chain reads used by the radicle manager.
pub trait RadicleProtocolState: Send + Sync {
    /// Accepted publication receipt at `sequence`, if one exists.
    fn publication_receipt(
        &self,
        storage: StorageHandle<'_>,
        sequence: u64,
    ) -> Result<Option<PublicationReceipt>, RadicleProtocolError>;

    /// Participant indexes that attested `body` in the current pending generation.
    fn pending_publication_attesters(
        &self,
        storage: StorageHandle<'_>,
        body: &PublicationBody,
    ) -> Result<Vec<u32>, RadicleProtocolError>;

    /// Left-aligned network repository storage key. Zero before bootstrap.
    fn network_repository(&self, storage: StorageHandle<'_>) -> Result<B256, RadicleProtocolError>;

    /// Latest accepted publication sequence.
    fn publication_sequence(&self, storage: StorageHandle<'_>)
        -> Result<u64, RadicleProtocolError>;

    /// Sequence of the receipt that binds repository material.
    fn publication_material_sequence(
        &self,
        storage: StorageHandle<'_>,
    ) -> Result<u64, RadicleProtocolError>;

    /// Hash of the latest accepted publication receipt.
    fn publication_receipt_hash(
        &self,
        storage: StorageHandle<'_>,
    ) -> Result<B256, RadicleProtocolError>;

    /// Genesis governments descriptor, if the registry has one.
    fn governments_descriptor(
        &self,
        storage: StorageHandle<'_>,
    ) -> Result<Option<GovernmentsDescriptor>, RadicleProtocolError>;

    /// Immutable approval for `proposal_id`, if the vote is terminally approved.
    fn patch_approval(
        &self,
        storage: StorageHandle<'_>,
        proposal_id: U256,
    ) -> Result<Option<PatchApprovalRecord>, RadicleProtocolError>;

    /// Immutable genesis config descriptor hash.
    fn config_descriptor_hash(
        &self,
        storage: StorageHandle<'_>,
    ) -> Result<B256, RadicleProtocolError>;

    /// Execution stage byte for `proposal_id`.
    fn patch_execution_stage(
        &self,
        storage: StorageHandle<'_>,
        proposal_id: U256,
    ) -> Result<u8, RadicleProtocolError>;

    /// Number of registered repositories.
    fn repository_count(&self, storage: StorageHandle<'_>) -> Result<u32, RadicleProtocolError>;

    /// Monotonic registry generation.
    fn registry_generation(&self, storage: StorageHandle<'_>) -> Result<u64, RadicleProtocolError>;

    /// Repository id at append-only `index`.
    fn repository_at(
        &self,
        storage: StorageHandle<'_>,
        index: u32,
    ) -> Result<RepoId, RadicleProtocolError>;

    /// Current validator-set epoch.
    fn current_epoch(&self, storage: StorageHandle<'_>) -> Result<u64, RadicleProtocolError>;

    /// Active consensus validators.
    fn active_consensus_set(
        &self,
        storage: StorageHandle<'_>,
    ) -> Result<Vec<ValidatorRecord>, RadicleProtocolError>;

    /// Current epoch metadata, including the activation block.
    fn epoch_snapshot(
        &self,
        storage: StorageHandle<'_>,
    ) -> Result<EpochSnapshot, RadicleProtocolError>;

    /// Radicle node id bound to `validator`. Zero when unbound.
    fn radicle_node_id(
        &self,
        storage: StorageHandle<'_>,
        validator: Address,
    ) -> Result<B256, RadicleProtocolError>;

    /// Validator bound to `node_id`.
    fn validator_by_radicle_node_id(
        &self,
        storage: StorageHandle<'_>,
        node_id: B256,
    ) -> Result<Address, RadicleProtocolError>;

    /// Committee snapshot retained for `epoch`, if the prune ring still holds it.
    fn committee_snapshot(
        &self,
        storage: StorageHandle<'_>,
        epoch: u64,
    ) -> Result<Option<CommitteeSnapshot>, RadicleProtocolError>;

    /// Proposal id when `log` is an approved Vote event. `Ok(None)` for other logs.
    fn approved_proposal_id(&self, log: &Log) -> Result<Option<U256>, RadicleProtocolError>;
}

/// Fail-closed reader used when no composition root installed chain reads.
pub struct UninstalledRadicleState;

fn not_installed<T>() -> Result<T, RadicleProtocolError> {
    Err(RadicleProtocolError::State(PrecompileError::storage(
        "radicle protocol state is not installed",
    )))
}

impl RadicleProtocolState for UninstalledRadicleState {
    fn publication_receipt(
        &self,
        _storage: StorageHandle<'_>,
        _sequence: u64,
    ) -> Result<Option<PublicationReceipt>, RadicleProtocolError> {
        not_installed()
    }

    fn pending_publication_attesters(
        &self,
        _storage: StorageHandle<'_>,
        _body: &PublicationBody,
    ) -> Result<Vec<u32>, RadicleProtocolError> {
        not_installed()
    }

    fn network_repository(
        &self,
        _storage: StorageHandle<'_>,
    ) -> Result<B256, RadicleProtocolError> {
        not_installed()
    }

    fn publication_sequence(
        &self,
        _storage: StorageHandle<'_>,
    ) -> Result<u64, RadicleProtocolError> {
        not_installed()
    }

    fn publication_material_sequence(
        &self,
        _storage: StorageHandle<'_>,
    ) -> Result<u64, RadicleProtocolError> {
        not_installed()
    }

    fn publication_receipt_hash(
        &self,
        _storage: StorageHandle<'_>,
    ) -> Result<B256, RadicleProtocolError> {
        not_installed()
    }

    fn governments_descriptor(
        &self,
        _storage: StorageHandle<'_>,
    ) -> Result<Option<GovernmentsDescriptor>, RadicleProtocolError> {
        not_installed()
    }

    fn patch_approval(
        &self,
        _storage: StorageHandle<'_>,
        _proposal_id: U256,
    ) -> Result<Option<PatchApprovalRecord>, RadicleProtocolError> {
        not_installed()
    }

    fn config_descriptor_hash(
        &self,
        _storage: StorageHandle<'_>,
    ) -> Result<B256, RadicleProtocolError> {
        not_installed()
    }

    fn patch_execution_stage(
        &self,
        _storage: StorageHandle<'_>,
        _proposal_id: U256,
    ) -> Result<u8, RadicleProtocolError> {
        not_installed()
    }

    fn repository_count(&self, _storage: StorageHandle<'_>) -> Result<u32, RadicleProtocolError> {
        not_installed()
    }

    fn registry_generation(
        &self,
        _storage: StorageHandle<'_>,
    ) -> Result<u64, RadicleProtocolError> {
        not_installed()
    }

    fn repository_at(
        &self,
        _storage: StorageHandle<'_>,
        _index: u32,
    ) -> Result<RepoId, RadicleProtocolError> {
        not_installed()
    }

    fn current_epoch(&self, _storage: StorageHandle<'_>) -> Result<u64, RadicleProtocolError> {
        not_installed()
    }

    fn active_consensus_set(
        &self,
        _storage: StorageHandle<'_>,
    ) -> Result<Vec<ValidatorRecord>, RadicleProtocolError> {
        not_installed()
    }

    fn epoch_snapshot(
        &self,
        _storage: StorageHandle<'_>,
    ) -> Result<EpochSnapshot, RadicleProtocolError> {
        not_installed()
    }

    fn radicle_node_id(
        &self,
        _storage: StorageHandle<'_>,
        _validator: Address,
    ) -> Result<B256, RadicleProtocolError> {
        not_installed()
    }

    fn validator_by_radicle_node_id(
        &self,
        _storage: StorageHandle<'_>,
        _node_id: B256,
    ) -> Result<Address, RadicleProtocolError> {
        not_installed()
    }

    fn committee_snapshot(
        &self,
        _storage: StorageHandle<'_>,
        _epoch: u64,
    ) -> Result<Option<CommitteeSnapshot>, RadicleProtocolError> {
        not_installed()
    }

    fn approved_proposal_id(&self, _log: &Log) -> Result<Option<U256>, RadicleProtocolError> {
        not_installed()
    }
}

#[cfg(test)]
pub(crate) struct StorageRadicleState;

#[cfg(test)]
impl RadicleProtocolState for StorageRadicleState {
    fn publication_receipt(
        &self,
        storage: StorageHandle<'_>,
        sequence: u64,
    ) -> Result<Option<PublicationReceipt>, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage).publication_receipt(sequence)?)
    }

    fn pending_publication_attesters(
        &self,
        storage: StorageHandle<'_>,
        body: &PublicationBody,
    ) -> Result<Vec<u32>, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage).pending_publication_attesters(body)?)
    }

    fn network_repository(&self, storage: StorageHandle<'_>) -> Result<B256, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage)
            .network_repository
            .read()?)
    }

    fn publication_sequence(
        &self,
        storage: StorageHandle<'_>,
    ) -> Result<u64, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage)
            .publication_sequence
            .read()?)
    }

    fn publication_material_sequence(
        &self,
        storage: StorageHandle<'_>,
    ) -> Result<u64, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage)
            .publication_material_sequence
            .read()?)
    }

    fn publication_receipt_hash(
        &self,
        storage: StorageHandle<'_>,
    ) -> Result<B256, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage)
            .publication_receipt_hash
            .read()?)
    }

    fn governments_descriptor(
        &self,
        storage: StorageHandle<'_>,
    ) -> Result<Option<GovernmentsDescriptor>, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage).governments_descriptor()?)
    }

    fn patch_approval(
        &self,
        storage: StorageHandle<'_>,
        proposal_id: U256,
    ) -> Result<Option<PatchApprovalRecord>, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage).patch_approval(proposal_id)?)
    }

    fn config_descriptor_hash(
        &self,
        storage: StorageHandle<'_>,
    ) -> Result<B256, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage)
            .config_descriptor_hash
            .read()?)
    }

    fn patch_execution_stage(
        &self,
        storage: StorageHandle<'_>,
        proposal_id: U256,
    ) -> Result<u8, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage)
            .patch_execution_stage
            .read(&proposal_id)?)
    }

    fn repository_count(&self, storage: StorageHandle<'_>) -> Result<u32, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage)
            .repository_count
            .read()?)
    }

    fn registry_generation(&self, storage: StorageHandle<'_>) -> Result<u64, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage)
            .registry_generation
            .read()?)
    }

    fn repository_at(
        &self,
        storage: StorageHandle<'_>,
        index: u32,
    ) -> Result<RepoId, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage).repository_at(index)?)
    }

    fn current_epoch(&self, storage: StorageHandle<'_>) -> Result<u64, RadicleProtocolError> {
        Ok(outbe_validatorset::contract::ValidatorSet::new(storage).current_epoch_u64()?)
    }

    fn active_consensus_set(
        &self,
        storage: StorageHandle<'_>,
    ) -> Result<Vec<ValidatorRecord>, RadicleProtocolError> {
        Ok(outbe_validatorset::contract::ValidatorSet::new(storage).get_active_consensus_set()?)
    }

    fn epoch_snapshot(
        &self,
        storage: StorageHandle<'_>,
    ) -> Result<EpochSnapshot, RadicleProtocolError> {
        Ok(outbe_validatorset::contract::ValidatorSet::new(storage).epoch_snapshot()?)
    }

    fn radicle_node_id(
        &self,
        storage: StorageHandle<'_>,
        validator: Address,
    ) -> Result<B256, RadicleProtocolError> {
        Ok(outbe_validatorset::contract::ValidatorSet::new(storage)
            .get_radicle_node_id(validator)?)
    }

    fn validator_by_radicle_node_id(
        &self,
        storage: StorageHandle<'_>,
        node_id: B256,
    ) -> Result<Address, RadicleProtocolError> {
        Ok(outbe_validatorset::contract::ValidatorSet::new(storage)
            .validator_by_radicle_node_id(node_id)?)
    }

    fn committee_snapshot(
        &self,
        storage: StorageHandle<'_>,
        epoch: u64,
    ) -> Result<Option<CommitteeSnapshot>, RadicleProtocolError> {
        Ok(outbe_validatorset::state::read_committee_snapshot_for_epoch(storage, epoch)?)
    }

    fn approved_proposal_id(&self, log: &Log) -> Result<Option<U256>, RadicleProtocolError> {
        use alloy_sol_types::SolEvent as _;
        use outbe_vote::precompile::IVote;
        if log.address == outbe_primitives::addresses::VOTE_ADDRESS
            && log.data.topics().first() == Some(&IVote::ProposalApproved::SIGNATURE_HASH)
        {
            let event = IVote::ProposalApproved::decode_log_validate(log)
                .map_err(|error| RadicleProtocolError::VoteLog(error.to_string()))?;
            Ok(Some(event.data.proposalId))
        } else {
            Ok(None)
        }
    }
}
