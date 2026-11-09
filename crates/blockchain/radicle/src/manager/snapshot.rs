use super::types::{
    BoxFuture, FinalizedBlock, FinalizedBootstrapSigningState, FinalizedFeed,
    FinalizedPatchApproval, FinalizedPatchSigningState, FinalizedPublicationProgress,
    FinalizedPublicationReceipt, FinalizedRepositoryAuthority, FinalizedSnapshot,
    FinalizedValidator, ManagerError, SnapshotReader,
};
use crate::endpoint::PeerId;
use crate::protocol_state::active_radicle_protocol_state;
use alloy_primitives::{Address, B256, U256};
use bytes::Bytes;
use commonware_codec::DecodeExt;
use commonware_cryptography::bls12381;
use futures::StreamExt as _;
use outbe_primitives::storage::{
    readonly::{ReadOnlyStorageProvider, StorageReader},
    StorageHandle,
};
use reth_chain_state::ForkChoiceSubscriptions;
use reth_provider::{BlockIdReader, StateProviderFactory};
use reth_storage_api::StateProvider;
use std::sync::Arc;
use tokio::sync::mpsc;

mod material;
mod vote;

#[derive(Clone)]
pub struct RethSnapshotReader<P> {
    provider: P,
    chain_id: u64,
    genesis_hash: B256,
}

impl<P> RethSnapshotReader<P> {
    #[must_use]
    pub fn new(provider: P, chain_id: u64, genesis_hash: B256) -> Self {
        Self {
            provider,
            chain_id,
            genesis_hash,
        }
    }
}

impl<P: StateProviderFactory + Send + Sync> super::RepositoryAuthorityReader
    for RethSnapshotReader<P>
{
    fn read_current_material_signing_state(
        &self,
    ) -> Result<super::FinalizedMaterialSigningState, ManagerError> {
        RethSnapshotReader::read_current_material_signing_state(self)
    }
    fn read_current_bootstrap_signing_state(
        &self,
    ) -> Result<FinalizedBootstrapSigningState, ManagerError> {
        RethSnapshotReader::read_current_bootstrap_signing_state(self)
    }
    fn read_current_repository_authority(
        &self,
    ) -> Result<FinalizedRepositoryAuthority, ManagerError> {
        RethSnapshotReader::read_current_repository_authority(self)
    }
    fn read_current_patch_signing_state(
        &self,
        proposal_id: U256,
    ) -> Result<FinalizedPatchSigningState, ManagerError> {
        RethSnapshotReader::read_current_patch_signing_state(self, proposal_id)
    }
}

impl<P: StateProviderFactory + reth_provider::ReceiptProvider + Send + Sync + 'static>
    super::RepositoryReceiptReader for RethSnapshotReader<P>
{
    fn read_governments_descriptor(
        &self,
    ) -> Result<Option<outbe_primitives::radicle_genesis::GovernmentsDescriptor>, ManagerError>
    {
        let block = self.current_finalized_block()?;
        let state = self
            .provider
            .state_by_block_hash(block.hash)
            .map_err(ManagerError::provider)?;
        let mut storage = ReadOnlyStorageProvider::new_with_chain_identity(
            RethStateReader {
                state: state.as_ref(),
            },
            self.chain_id,
            self.genesis_hash,
        );
        active_radicle_protocol_state()
            .governments_descriptor(StorageHandle::new(&mut storage))
            .map_err(snapshot_error)
    }
    fn read_finalized_vote_page(
        &self,
        height: u64,
        offset: u32,
    ) -> Result<Option<super::FinalizedVotePage>, ManagerError> {
        RethSnapshotReader::read_finalized_vote_page(self, height, offset)
    }
    fn read_current_patch_approval(
        &self,
        proposal_id: U256,
    ) -> Result<Option<FinalizedPatchApproval>, ManagerError> {
        self.read_patch_approval(self.current_finalized_block()?, proposal_id)
    }
    fn read_current_publication_progress(
        &self,
        body: &outbe_primitives::radicle_publication::PublicationBody,
    ) -> Result<FinalizedPublicationProgress, ManagerError> {
        RethSnapshotReader::read_current_publication_progress(self, body)
    }
    fn read_current_publication_receipt(
        &self,
        sequence: u64,
    ) -> Result<Option<FinalizedPublicationReceipt>, ManagerError> {
        self.read_publication_receipt(self.current_finalized_block()?, sequence)
    }
}

impl<P: StateProviderFactory> RethSnapshotReader<P> {
    pub fn read_current_publication_progress(
        &self,
        body: &outbe_primitives::radicle_publication::PublicationBody,
    ) -> Result<FinalizedPublicationProgress, ManagerError> {
        if body.chain_id != self.chain_id || body.genesis_hash != self.genesis_hash {
            return Err(ManagerError::Snapshot(
                "publication progress belongs to another chain".into(),
            ));
        }
        let block = self.current_finalized_block()?;
        let state = self
            .provider
            .state_by_block_hash(block.hash)
            .map_err(ManagerError::provider)?;
        let mut storage = ReadOnlyStorageProvider::new_with_chain_identity(
            RethStateReader {
                state: state.as_ref(),
            },
            self.chain_id,
            self.genesis_hash,
        );
        let handle = StorageHandle::new(&mut storage);
        let protocol = active_radicle_protocol_state();
        let receipt = protocol
            .publication_receipt(handle.clone(), body.sequence)
            .map_err(snapshot_error)?;
        let (attesters, receipt) = if let Some(receipt) = receipt {
            if receipt.body != *body || receipt.accepted_height > block.number {
                return Err(ManagerError::Snapshot(
                    "publication progress receipt does not match".into(),
                ));
            }
            (vec![], Some(FinalizedPublicationReceipt { block, receipt }))
        } else {
            (
                protocol
                    .pending_publication_attesters(handle, body)
                    .map_err(snapshot_error)?,
                None,
            )
        };
        Ok(FinalizedPublicationProgress {
            block,
            body_hash: body.receipt_hash(),
            attesters,
            receipt,
        })
    }

    fn current_finalized_block(&self) -> Result<FinalizedBlock, ManagerError> {
        let head = self
            .provider
            .finalized_block_num_hash()
            .map_err(ManagerError::provider)?
            .ok_or_else(|| ManagerError::Finality("no finalized block".into()))?;
        if self
            .provider
            .block_hash(head.number)
            .map_err(ManagerError::provider)?
            != Some(head.hash)
        {
            return Err(ManagerError::Finality(
                "repository authority requires a canonical finalized block".into(),
            ));
        }
        Ok(FinalizedBlock {
            number: head.number,
            hash: head.hash,
        })
    }

    /// Genesis descriptor and unbound bootstrap permission from one current finalized state.
    pub fn read_current_bootstrap_signing_state(
        &self,
    ) -> Result<FinalizedBootstrapSigningState, ManagerError> {
        let block = self.current_finalized_block()?;
        let state = self
            .provider
            .state_by_block_hash(block.hash)
            .map_err(ManagerError::provider)?;
        let authority = read_repository_authority_from_storage(
            RethStateReader {
                state: state.as_ref(),
            },
            self.chain_id,
            self.genesis_hash,
            block,
        )?;
        let mut storage = ReadOnlyStorageProvider::new_with_chain_identity(
            RethStateReader {
                state: state.as_ref(),
            },
            self.chain_id,
            self.genesis_hash,
        );
        let handle = StorageHandle::new(&mut storage);
        let protocol = active_radicle_protocol_state();
        if !protocol
            .network_repository(handle.clone())
            .map_err(snapshot_error)?
            .is_zero()
            || protocol
                .publication_sequence(handle.clone())
                .map_err(snapshot_error)?
                != 0
            || protocol
                .publication_material_sequence(handle.clone())
                .map_err(snapshot_error)?
                != 0
            || !protocol
                .publication_receipt_hash(handle.clone())
                .map_err(snapshot_error)?
                .is_zero()
        {
            return Err(ManagerError::Snapshot(
                "network bootstrap has already been accepted".into(),
            ));
        }
        let descriptor = protocol
            .governments_descriptor(handle)
            .map_err(snapshot_error)?
            .ok_or_else(|| {
                ManagerError::Snapshot("governments genesis descriptor is disabled".into())
            })?;
        Ok(FinalizedBootstrapSigningState {
            authority,
            descriptor,
        })
    }

    pub fn read_current_patch_signing_state(
        &self,
        proposal_id: U256,
    ) -> Result<FinalizedPatchSigningState, ManagerError> {
        use outbe_primitives::radicle_publication::players_commitment;
        let block = self.current_finalized_block()?;
        let state = self
            .provider
            .state_by_block_hash(block.hash)
            .map_err(ManagerError::provider)?;
        let authority = read_repository_authority_from_storage(
            RethStateReader {
                state: state.as_ref(),
            },
            self.chain_id,
            self.genesis_hash,
            block,
        )?;
        let mut storage = ReadOnlyStorageProvider::new_with_chain_identity(
            RethStateReader {
                state: state.as_ref(),
            },
            self.chain_id,
            self.genesis_hash,
        );
        let handle = StorageHandle::new(&mut storage);
        let protocol = active_radicle_protocol_state();
        let approval = protocol
            .patch_approval(handle.clone(), proposal_id)
            .map_err(snapshot_error)?
            .ok_or_else(|| ManagerError::Snapshot("terminal Approved patch required".into()))?;
        if approval.approved_height > block.number {
            return Err(ManagerError::Snapshot(
                "approval is newer than signing state".into(),
            ));
        }
        let subject = outbe_primitives::code_registry::PatchVotePayloadV1::decode(
            approval.payload.as_bytes(),
        )
        .map_err(snapshot_error)?;
        let binding = outbe_primitives::code_registry::repo_id_from_storage_key(
            protocol
                .network_repository(handle.clone())
                .map_err(snapshot_error)?,
        )
        .map_err(snapshot_error)?;
        let (sequence, material_sequence) = (
            protocol
                .publication_sequence(handle.clone())
                .map_err(snapshot_error)?,
            protocol
                .publication_material_sequence(handle.clone())
                .map_err(snapshot_error)?,
        );
        ensure_bound_patch_sequence(binding, subject.repo_id, sequence, material_sequence)?;
        let read_receipt = {
            let protocol = Arc::clone(&protocol);
            let handle = handle.clone();
            move |sequence| -> Result<_, ManagerError> {
                let receipt = protocol
                    .publication_receipt(handle.clone(), sequence)
                    .map_err(snapshot_error)?
                    .ok_or_else(|| ManagerError::Snapshot("signing checkpoint missing".into()))?;
                if receipt.accepted_height > block.number {
                    return Err(ManagerError::Snapshot(
                        "checkpoint is newer than signing state".into(),
                    ));
                }
                Ok(receipt)
            }
        };
        let bootstrap = read_receipt(1)?;
        let checkpoint = if sequence == 1 {
            bootstrap.clone()
        } else {
            read_receipt(sequence)?
        };
        let material = if material_sequence == sequence {
            checkpoint.clone()
        } else if material_sequence == 1 {
            bootstrap.clone()
        } else {
            read_receipt(material_sequence)?
        };
        let descriptor_hash = protocol
            .config_descriptor_hash(handle.clone())
            .map_err(snapshot_error)?;
        let players = players_commitment(
            authority
                .committee()
                .committee
                .iter()
                .map(|member| (member.address, &member.consensus_pubkey)),
        )
        .map_err(snapshot_error)?;
        let latest_receipt_hash = protocol
            .publication_receipt_hash(handle.clone())
            .map_err(snapshot_error)?;
        if !self.is_patch_signing_bound(
            &authority,
            (&bootstrap, &material, &checkpoint),
            (binding, descriptor_hash, latest_receipt_hash, players),
        ) {
            return Err(ManagerError::Snapshot(
                "signing material/checkpoint binding mismatch".into(),
            ));
        }
        let stage = protocol
            .patch_execution_stage(handle, proposal_id)
            .map_err(snapshot_error)?;
        if !matches!(
            stage,
            outbe_primitives::code_registry::PATCH_EXECUTION_PENDING
                | outbe_primitives::code_registry::PATCH_EXECUTION_REVIEWED
                | outbe_primitives::code_registry::PATCH_EXECUTION_MERGED
        ) {
            return Err(ManagerError::Snapshot(
                "unknown patch execution stage".into(),
            ));
        }
        let anchor_height = approval
            .approved_height
            .max(material.accepted_height)
            .max(checkpoint.accepted_height);
        let anchor_hash = self
            .provider
            .block_hash(anchor_height)
            .map_err(ManagerError::provider)?
            .filter(|hash| !hash.is_zero())
            .ok_or_else(|| {
                ManagerError::Finality("signing anchor is locally unavailable".into())
            })?;
        Ok(FinalizedPatchSigningState {
            authority,
            approval: FinalizedPatchApproval { block, approval },
            stage,
            anchor: FinalizedBlock {
                number: anchor_height,
                hash: anchor_hash,
            },
            checkpoint: FinalizedPublicationReceipt {
                block,
                receipt: checkpoint,
            },
            material: FinalizedPublicationReceipt {
                block,
                receipt: material,
            },
        })
    }

    /// Replay immutable receipts from an exact local canonical finalized snapshot.
    pub fn read_publication_receipt(
        &self,
        block: FinalizedBlock,
        sequence: u64,
    ) -> Result<Option<FinalizedPublicationReceipt>, ManagerError> {
        self.ensure_canonical_finalized_block(
            block,
            "publication receipt requires a canonical finalized block",
        )?;
        let state = self
            .provider
            .state_by_block_hash(block.hash)
            .map_err(ManagerError::provider)?;
        let mut storage = ReadOnlyStorageProvider::new_with_chain_identity(
            RethStateReader {
                state: state.as_ref(),
            },
            self.chain_id,
            self.genesis_hash,
        );
        let handle = StorageHandle::new(&mut storage);
        let protocol = active_radicle_protocol_state();
        let latest_sequence = protocol
            .publication_sequence(handle.clone())
            .map_err(snapshot_error)?;
        let latest_hash = protocol
            .publication_receipt_hash(handle.clone())
            .map_err(snapshot_error)?;
        if sequence == 0 || sequence > latest_sequence {
            return Ok(None);
        }
        let receipt = protocol
            .publication_receipt(handle, sequence)
            .map_err(snapshot_error)?
            .ok_or_else(|| ManagerError::Snapshot("publication receipt missing".into()))?;
        if receipt.accepted_height == 0
            || receipt.accepted_height > block.number
            || receipt.body.chain_id != self.chain_id
        {
            return Err(ManagerError::Snapshot(
                "invalid finalized publication receipt".into(),
            ));
        }
        if receipt.body.genesis_hash != self.genesis_hash
            || (sequence == latest_sequence && receipt.receipt_hash != latest_hash)
        {
            return Err(ManagerError::Snapshot(
                "invalid finalized publication receipt".into(),
            ));
        }
        Ok(Some(FinalizedPublicationReceipt { block, receipt }))
    }

    /// Sample the local finalized head; callers cannot select an archived signing set.
    pub fn read_current_repository_authority(
        &self,
    ) -> Result<FinalizedRepositoryAuthority, ManagerError> {
        let head = self.current_finalized_block()?;
        let state = self
            .provider
            .state_by_block_hash(head.hash)
            .map_err(ManagerError::provider)?;
        read_repository_authority_from_storage(
            RethStateReader {
                state: state.as_ref(),
            },
            self.chain_id,
            self.genesis_hash,
            FinalizedBlock {
                number: head.number,
                hash: head.hash,
            },
        )
    }

    /// Reads an exact canonical finalized snapshot, never latest or pending state.
    pub fn read_patch_approval(
        &self,
        block: FinalizedBlock,
        proposal_id: U256,
    ) -> Result<Option<FinalizedPatchApproval>, ManagerError> {
        self.ensure_canonical_finalized_block(
            block,
            "patch approval requires a canonical finalized block",
        )?;
        let state = self
            .provider
            .state_by_block_hash(block.hash)
            .map_err(ManagerError::provider)?;
        read_patch_approval_from_storage(
            RethStateReader {
                state: state.as_ref(),
            },
            self.chain_id,
            self.genesis_hash,
            block,
            proposal_id,
        )
    }

    fn ensure_canonical_finalized_block(
        &self,
        block: FinalizedBlock,
        reason: &'static str,
    ) -> Result<(), ManagerError> {
        let finalized = self
            .provider
            .finalized_block_num_hash()
            .map_err(ManagerError::provider)?
            .ok_or_else(|| ManagerError::Finality("no finalized block".into()))?;
        if block.number > finalized.number
            || self
                .provider
                .block_hash(block.number)
                .map_err(ManagerError::provider)?
                != Some(block.hash)
            || self
                .provider
                .block_hash(finalized.number)
                .map_err(ManagerError::provider)?
                != Some(finalized.hash)
        {
            return Err(ManagerError::Finality(reason.into()));
        }
        Ok(())
    }

    fn is_patch_signing_bound(
        &self,
        authority: &FinalizedRepositoryAuthority,
        (bootstrap, material, checkpoint): (
            &outbe_primitives::code_registry::PublicationReceipt,
            &outbe_primitives::code_registry::PublicationReceipt,
            &outbe_primitives::code_registry::PublicationReceipt,
        ),
        (binding, descriptor_hash, latest_receipt_hash, players): (
            outbe_primitives::code_registry::RepoId,
            B256,
            B256,
            B256,
        ),
    ) -> bool {
        use outbe_primitives::radicle_publication::PublicationAction;
        if bootstrap.body.action != PublicationAction::Bootstrap
            || !bootstrap.body.previous_receipt_hash.is_zero()
            || descriptor_hash.is_zero()
        {
            return false;
        }
        if checkpoint.receipt_hash != latest_receipt_hash
            || !matches!(
                material.body.action,
                PublicationAction::Bootstrap | PublicationAction::BindMaterial
            )
            || authority.committee().vrf_group_public_key_bytes.as_slice()
                != bootstrap.body.owner_bls_public.as_slice()
        {
            return false;
        }
        let receipts_match = [bootstrap, material, checkpoint].iter().all(|r| {
            !self.receipt_mismatch(
                r,
                u64::MAX,
                binding,
                bootstrap.body.owner_bls_public,
                descriptor_hash,
            )
        });
        receipts_match
            && is_material_and_checkpoint_consistent(authority, material, checkpoint, players)
    }
}

fn is_material_and_checkpoint_consistent(
    authority: &FinalizedRepositoryAuthority,
    material: &outbe_primitives::code_registry::PublicationReceipt,
    checkpoint: &outbe_primitives::code_registry::PublicationReceipt,
    players: B256,
) -> bool {
    if material.body.epoch != authority.epoch()
        || material.body.material_version != authority.committee().vrf_material_version
        || material.body.players_commitment != players
    {
        return false;
    }
    if material.body.material_hash.is_zero()
        || checkpoint.body.epoch != material.body.epoch
        || checkpoint.body.material_version != material.body.material_version
    {
        return false;
    }
    checkpoint.body.material_hash == material.body.material_hash
        && checkpoint.body.players_commitment == material.body.players_commitment
}

fn ensure_bound_patch_sequence(
    binding: outbe_primitives::code_registry::RepoId,
    subject_repo: outbe_primitives::code_registry::RepoId,
    sequence: u64,
    material_sequence: u64,
) -> Result<(), ManagerError> {
    if binding.is_zero() || subject_repo != binding {
        return Err(ManagerError::Snapshot(
            "network repository/current material is not bound".into(),
        ));
    }
    if sequence == 0 || material_sequence == 0 || material_sequence > sequence {
        return Err(ManagerError::Snapshot(
            "network repository/current material is not bound".into(),
        ));
    }
    Ok(())
}

fn read_repository_authority_from_storage(
    reader: impl StorageReader,
    chain_id: u64,
    genesis_hash: B256,
    block: FinalizedBlock,
) -> Result<FinalizedRepositoryAuthority, ManagerError> {
    let mut storage =
        ReadOnlyStorageProvider::new_with_chain_identity(reader, chain_id, genesis_hash);
    let handle = StorageHandle::new(&mut storage);
    let protocol = active_radicle_protocol_state();
    let epoch = protocol
        .current_epoch(handle.clone())
        .map_err(snapshot_error)?;
    let effective = protocol
        .active_consensus_set(handle.clone())
        .map_err(snapshot_error)?;
    let committee = protocol
        .committee_snapshot(handle, epoch)
        .map_err(snapshot_error)?
        .ok_or_else(|| {
            ManagerError::Snapshot("current repository signing committee is unavailable".into())
        })?;
    let addresses: std::collections::BTreeSet<_> = committee
        .committee
        .iter()
        .map(|entry| entry.address)
        .collect();
    if effective.is_empty()
        || committee.committee.len() != effective.len()
        || addresses.len() != effective.len()
        || committee.committee.iter().any(|entry| {
            !effective.iter().any(|validator| {
                validator.validator_address == entry.address
                    && validator.consensus_pubkey.as_slice() == entry.consensus_pubkey
            })
        })
    {
        return Err(ManagerError::Snapshot(
            "effective validator set differs from the repository signing committee".into(),
        ));
    }
    Ok(FinalizedRepositoryAuthority {
        block,
        chain_id,
        genesis_hash,
        epoch,
        committee,
    })
}

fn read_patch_approval_from_storage(
    reader: impl StorageReader,
    chain_id: u64,
    genesis_hash: B256,
    block: FinalizedBlock,
    proposal_id: U256,
) -> Result<Option<FinalizedPatchApproval>, ManagerError> {
    let mut storage =
        ReadOnlyStorageProvider::new_with_chain_identity(reader, chain_id, genesis_hash);
    let approval = active_radicle_protocol_state()
        .patch_approval(StorageHandle::new(&mut storage), proposal_id)
        .map_err(snapshot_error)?;
    match approval {
        Some(approval) if approval.approved_height > block.number => Err(ManagerError::Snapshot(
            "approval is newer than its source snapshot".into(),
        )),
        Some(approval) => Ok(Some(FinalizedPatchApproval { block, approval })),
        None => Ok(None),
    }
}

impl<P> SnapshotReader for RethSnapshotReader<P>
where
    P: StateProviderFactory + Send + Sync + 'static,
{
    fn read_exact(&self, block: FinalizedBlock) -> Result<FinalizedSnapshot, ManagerError> {
        let state = self
            .provider
            .state_by_block_hash(block.hash)
            .map_err(ManagerError::provider)?;
        let reader = RethStateReader {
            state: state.as_ref(),
        };
        read_snapshot_from_storage(reader, self.chain_id, self.genesis_hash, block)
    }
}

fn read_snapshot_from_storage(
    reader: impl StorageReader,
    chain_id: u64,
    genesis_hash: B256,
    block: FinalizedBlock,
) -> Result<FinalizedSnapshot, ManagerError> {
    let mut storage =
        ReadOnlyStorageProvider::new_with_chain_identity(reader, chain_id, genesis_hash);
    let protocol = active_radicle_protocol_state();
    let validators = {
        let handle = StorageHandle::new(&mut storage);
        let active = protocol
            .active_consensus_set(handle.clone())
            .map_err(snapshot_error)?;
        let mut validators = Vec::with_capacity(active.len());
        for record in active {
            let binding = protocol
                .radicle_node_id(handle.clone(), record.validator_address)
                .map_err(snapshot_error)?;
            let node_id = if binding.is_zero() {
                None
            } else {
                let reverse = protocol
                    .validator_by_radicle_node_id(handle.clone(), binding)
                    .map_err(snapshot_error)?;
                if reverse != record.validator_address {
                    return Err(
                        crate::errors::SnapshotInconsistencyReason::ReverseBindingMismatch(
                            record.validator_address,
                        )
                        .into(),
                    );
                }
                Some(binding.0)
            };
            let public_key = <bls12381::PublicKey as DecodeExt<()>>::decode(
                Bytes::copy_from_slice(&record.consensus_pubkey),
            )
            .map_err(snapshot_error)?;
            let peer = PeerId::from_public_key(&public_key);
            validators.push(FinalizedValidator {
                address: record.validator_address,
                peer,
                node_id,
            });
        }
        validators
    };
    let (registry_generation, repositories) = {
        let handle = StorageHandle::new(&mut storage);
        let count = protocol
            .repository_count(handle.clone())
            .map_err(snapshot_error)?;
        let generation = protocol
            .registry_generation(handle.clone())
            .map_err(snapshot_error)?;
        let mut repositories = Vec::with_capacity(count as usize);
        for index in 0..count {
            repositories.push(
                protocol
                    .repository_at(handle.clone(), index)
                    .map_err(snapshot_error)?,
            );
        }
        (generation, repositories)
    };
    Ok(FinalizedSnapshot {
        block,
        validators,
        registry_generation,
        repositories,
    })
}

struct RethStateReader<'a> {
    state: &'a dyn StateProvider,
}

impl StorageReader for RethStateReader<'_> {
    fn read_storage(&self, address: Address, key: B256) -> outbe_primitives::error::Result<U256> {
        self.state
            .storage(address, key)
            .map(|value| value.unwrap_or(U256::ZERO))
            .map_err(|error| {
                crate::errors::SnapshotStateReadError::FinalizedStateReadFailed(error.to_string())
                    .into()
            })
    }
}

fn snapshot_error(error: impl std::fmt::Display) -> ManagerError {
    ManagerError::Snapshot(error.to_string())
}

#[cfg(test)]
mod approval_tests;

#[derive(Clone)]
pub struct RethFinalizedFeed<P> {
    provider: P,
    subscription: Arc<std::sync::Mutex<Option<mpsc::UnboundedReceiver<FinalizedBlock>>>>,
}

impl<P> RethFinalizedFeed<P>
where
    P: ForkChoiceSubscriptions + Clone + Send + Sync + 'static,
    P::Header: alloy_consensus::BlockHeader + alloy_primitives::Sealable,
{
    #[must_use]
    pub fn new(provider: P) -> Self {
        let mut stream = provider.finalized_block_stream();
        let (sender, receiver) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(header) = stream.next().await {
                let block = header.num_hash();
                let _ = sender.send(FinalizedBlock {
                    number: block.number,
                    hash: block.hash,
                });
            }
        });
        Self {
            provider,
            subscription: Arc::new(std::sync::Mutex::new(Some(receiver))),
        }
    }
}

impl<P> FinalizedFeed for RethFinalizedFeed<P>
where
    P: ForkChoiceSubscriptions + BlockIdReader + Clone + Send + Sync + 'static,
    P::Header: alloy_consensus::BlockHeader + alloy_primitives::Sealable,
{
    fn subscribe(&self) -> Result<mpsc::UnboundedReceiver<FinalizedBlock>, ManagerError> {
        self.subscription
            .lock()
            .map_err(|_| {
                ManagerError::from(crate::errors::FinalityFeedReason::SubscriptionLockPoisoned)
            })?
            .take()
            .ok_or_else(|| crate::errors::FinalityFeedReason::AlreadySubscribed.into())
    }

    fn sample(&self) -> BoxFuture<'_, Result<Option<FinalizedBlock>, ManagerError>> {
        Box::pin(async move {
            self.provider
                .finalized_block_num_hash()
                .map(|block| {
                    block.map(|block| FinalizedBlock {
                        number: block.number,
                        hash: block.hash,
                    })
                })
                .map_err(ManagerError::finality)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonware_cryptography::{bls12381, Signer as _};
    use outbe_coderegistry::CodeRegistry;
    use outbe_primitives::code_registry::RepoId;
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    use outbe_validatorset::contract::ValidatorSet;

    struct MapReader(HashMapStorageProvider);

    impl StorageReader for MapReader {
        fn read_storage(
            &self,
            address: Address,
            key: B256,
        ) -> outbe_primitives::error::Result<U256> {
            Ok(self
                .0
                .storage
                .get(&(address, U256::from_be_bytes(key.0)))
                .copied()
                .unwrap_or_default())
        }
    }

    #[test]
    fn exact_snapshot() {
        let chain_id = 54_322_345;
        let genesis_hash = B256::repeat_byte(0x44);
        let validator = Address::repeat_byte(0x11);
        let signer = bls12381::PrivateKey::from_seed(7);
        let mut public_key = [0; 48];
        public_key.copy_from_slice(signer.public_key().as_ref());
        let repository = RepoId::repeat_byte(0x22);
        let mut provider = HashMapStorageProvider::new_with_chain_identity(chain_id, genesis_hash);
        provider.set_block_number(1);
        StorageHandle::enter(&mut provider, |storage| {
            let mut validators = ValidatorSet::new(storage.clone());
            validators
                .config_owner
                .write(Address::repeat_byte(0xaa))
                .unwrap();
            validators.set_config_max_validators(4).unwrap();
            validators.config_epoch_length_blocks.write(10).unwrap();
            validators
                .test_register_validator_without_pop(validator, &public_key)
                .unwrap();
            validators.activate_validator(validator).unwrap();

            let mut registry = CodeRegistry::new(storage);
            registry.config_max_repositories.write(4).unwrap();
            registry.register_repository(repository, validator).unwrap();
        });

        let expected = FinalizedBlock {
            number: 9,
            hash: B256::repeat_byte(0x99),
        };
        let snapshot =
            read_snapshot_from_storage(MapReader(provider), chain_id, genesis_hash, expected)
                .unwrap();
        assert_eq!(snapshot.block, expected);
        assert_eq!(snapshot.registry_generation, 1);
        assert_eq!(snapshot.repositories, [repository]);
        assert_eq!(snapshot.validators.len(), 1);
        assert_eq!(snapshot.validators[0].address, validator);
        assert_eq!(
            snapshot.validators[0].peer,
            PeerId::from_public_key(&signer.public_key())
        );
        assert!(snapshot.validators[0].node_id.is_some());
    }
}
