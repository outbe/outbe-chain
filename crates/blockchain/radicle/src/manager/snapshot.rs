use super::types::{
    BoxFuture, FinalizedBlock, FinalizedBootstrapSigningState, FinalizedFeed,
    FinalizedPatchApproval, FinalizedPatchSigningState, FinalizedPublicationReceipt,
    FinalizedRepositoryAuthority, FinalizedSnapshot, FinalizedValidator, ManagerError,
    SnapshotReader,
};
use crate::endpoint::PeerId;
use alloy_primitives::{Address, B256, U256};
use bytes::Bytes;
use commonware_codec::DecodeExt;
use commonware_cryptography::bls12381;
use futures::StreamExt as _;
use outbe_primitives::storage::{
    readonly::{ReadOnlyStorageProvider, StorageReader},
    StorageHandle,
};
use outbe_radicleregistry::RadicleRegistry;
use outbe_validatorset::contract::ValidatorSet;
use reth_chain_state::ForkChoiceSubscriptions;
use reth_provider::{BlockIdReader, StateProviderFactory};
use reth_storage_api::StateProvider;
use std::sync::Arc;
use tokio::sync::mpsc;

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

impl<P: StateProviderFactory + Send + Sync + 'static> super::RepositoryReceiptReader
    for RethSnapshotReader<P>
{
    fn read_current_publication_receipt(
        &self,
        sequence: u64,
    ) -> Result<Option<FinalizedPublicationReceipt>, ManagerError> {
        self.read_publication_receipt(self.current_finalized_block()?, sequence)
    }
}

impl<P: StateProviderFactory> RethSnapshotReader<P> {
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
        let registry = RadicleRegistry::new(StorageHandle::new(&mut storage));
        if !registry
            .network_repository
            .read()
            .map_err(snapshot_error)?
            .is_zero()
            || registry
                .publication_sequence
                .read()
                .map_err(snapshot_error)?
                != 0
            || registry
                .publication_material_sequence
                .read()
                .map_err(snapshot_error)?
                != 0
            || !registry
                .publication_receipt_hash
                .read()
                .map_err(snapshot_error)?
                .is_zero()
        {
            return Err(ManagerError::Snapshot(
                "network bootstrap has already been accepted".into(),
            ));
        }
        let descriptor = registry
            .governments_descriptor()
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
        use outbe_primitives::radicle_publication::{players_commitment, PublicationAction};
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
        let registry = RadicleRegistry::new(StorageHandle::new(&mut storage));
        let approval = registry
            .patch_approval(proposal_id)
            .map_err(snapshot_error)?
            .ok_or_else(|| ManagerError::Snapshot("terminal Approved patch required".into()))?;
        if approval.approved_height > block.number {
            return Err(ManagerError::Snapshot(
                "approval is newer than signing state".into(),
            ));
        }
        let subject =
            outbe_radicleregistry::payload::PatchVotePayloadV1::decode(approval.payload.as_bytes())
                .map_err(snapshot_error)?;
        let binding = outbe_radicleregistry::repo_id_from_storage_key(
            registry.network_repository.read().map_err(snapshot_error)?,
        )
        .map_err(snapshot_error)?;
        let sequence = registry
            .publication_sequence
            .read()
            .map_err(snapshot_error)?;
        let material_sequence = registry
            .publication_material_sequence
            .read()
            .map_err(snapshot_error)?;
        if binding.is_zero()
            || subject.repo_id != binding
            || sequence == 0
            || material_sequence == 0
            || material_sequence > sequence
        {
            return Err(ManagerError::Snapshot(
                "network repository/current material is not bound".into(),
            ));
        }
        let read_receipt = |sequence| -> Result<_, ManagerError> {
            let receipt = registry
                .publication_receipt(sequence)
                .map_err(snapshot_error)?
                .ok_or_else(|| ManagerError::Snapshot("signing checkpoint missing".into()))?;
            if receipt.accepted_height > block.number {
                return Err(ManagerError::Snapshot(
                    "checkpoint is newer than signing state".into(),
                ));
            }
            Ok(receipt)
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
        let descriptor_hash = registry
            .config_descriptor_hash
            .read()
            .map_err(snapshot_error)?;
        let players = players_commitment(
            authority
                .committee()
                .committee
                .iter()
                .map(|member| (member.address, &member.consensus_pubkey)),
        )
        .map_err(snapshot_error)?;
        if bootstrap.body.action != PublicationAction::Bootstrap
            || !bootstrap.body.previous_receipt_hash.is_zero()
            || descriptor_hash.is_zero()
            || checkpoint.receipt_hash
                != registry
                    .publication_receipt_hash
                    .read()
                    .map_err(snapshot_error)?
            || !matches!(
                material.body.action,
                PublicationAction::Bootstrap | PublicationAction::BindMaterial
            )
            || [&bootstrap, &material, &checkpoint].iter().any(|r| {
                r.body.chain_id != self.chain_id
                    || r.body.genesis_hash != self.genesis_hash
                    || r.body.repo_id != binding
                    || r.body.owner_bls_public != bootstrap.body.owner_bls_public
                    || r.body.descriptor_hash != descriptor_hash
            })
            || authority.committee().vrf_group_public_key_bytes.as_slice()
                != bootstrap.body.owner_bls_public.as_slice()
            || material.body.epoch != authority.epoch()
            || material.body.material_version != authority.committee().vrf_material_version
            || material.body.players_commitment != players
            || material.body.material_hash.is_zero()
            || checkpoint.body.epoch != material.body.epoch
            || checkpoint.body.material_version != material.body.material_version
            || checkpoint.body.material_hash != material.body.material_hash
            || checkpoint.body.players_commitment != material.body.players_commitment
        {
            return Err(ManagerError::Snapshot(
                "signing material/checkpoint binding mismatch".into(),
            ));
        }
        let stage = registry
            .patch_execution_stage
            .read(&proposal_id)
            .map_err(snapshot_error)?;
        if !matches!(
            stage,
            outbe_radicleregistry::schema::PATCH_EXECUTION_PENDING
                | outbe_radicleregistry::schema::PATCH_EXECUTION_REVIEWED
                | outbe_radicleregistry::schema::PATCH_EXECUTION_MERGED
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
            return Err(ManagerError::Finality(
                "publication receipt requires a canonical finalized block".into(),
            ));
        }
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
        let registry = RadicleRegistry::new(handle);
        let latest_sequence = registry
            .publication_sequence
            .read()
            .map_err(snapshot_error)?;
        let latest_hash = registry
            .publication_receipt_hash
            .read()
            .map_err(snapshot_error)?;
        if sequence == 0 || sequence > latest_sequence {
            return Ok(None);
        }
        let receipt = registry
            .publication_receipt(sequence)
            .map_err(snapshot_error)?
            .ok_or_else(|| ManagerError::Snapshot("publication receipt missing".into()))?;
        if receipt.accepted_height == 0
            || receipt.accepted_height > block.number
            || receipt.body.chain_id != self.chain_id
            || receipt.body.genesis_hash != self.genesis_hash
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
            return Err(ManagerError::Finality(
                "patch approval requires a canonical finalized block".into(),
            ));
        }
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
    let validators = ValidatorSet::new(handle.clone());
    let epoch = validators.current_epoch_u64().map_err(snapshot_error)?;
    let effective = validators
        .get_active_consensus_set()
        .map_err(snapshot_error)?;
    let committee = outbe_validatorset::state::read_committee_snapshot_for_epoch(handle, epoch)
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
    let approval = RadicleRegistry::new(StorageHandle::new(&mut storage))
        .patch_approval(proposal_id)
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
    let validators = {
        let validator_set = ValidatorSet::new(StorageHandle::new(&mut storage));
        let active = validator_set
            .get_active_consensus_set()
            .map_err(snapshot_error)?;
        let mut validators = Vec::with_capacity(active.len());
        for record in active {
            let binding = validator_set
                .get_radicle_node_id(record.validator_address)
                .map_err(snapshot_error)?;
            let node_id = if binding.is_zero() {
                None
            } else {
                let reverse = validator_set
                    .validator_by_radicle_node_id(binding)
                    .map_err(snapshot_error)?;
                if reverse != record.validator_address {
                    return Err(ManagerError::Snapshot(format!(
                        "Radicle reverse binding mismatch for {}",
                        record.validator_address
                    )));
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
        let registry = RadicleRegistry::new(StorageHandle::new(&mut storage));
        let count = registry.repository_count.read().map_err(snapshot_error)?;
        let generation = registry
            .registry_generation
            .read()
            .map_err(snapshot_error)?;
        let mut repositories = Vec::with_capacity(count as usize);
        for index in 0..count {
            repositories.push(registry.repository_at(index).map_err(snapshot_error)?);
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
                outbe_primitives::error::PrecompileError::Storage(format!(
                    "finalized state read failed: {error}"
                ))
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
            .map_err(|_| ManagerError::Finality("finality subscription lock poisoned".into()))?
            .take()
            .ok_or_else(|| ManagerError::Finality("finality feed already subscribed".into()))
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
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    use outbe_radicleregistry::RepoId;

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

            let mut registry = RadicleRegistry::new(storage);
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
