use super::types::{
    BoxFuture, FinalizedBlock, FinalizedFeed, FinalizedPatchApproval, FinalizedRepositoryAuthority,
    FinalizedSnapshot, FinalizedValidator, ManagerError, SnapshotReader,
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

impl<P: StateProviderFactory> RethSnapshotReader<P> {
    /// Sample the local finalized head; callers cannot select an archived signing set.
    pub fn read_current_repository_authority(
        &self,
    ) -> Result<FinalizedRepositoryAuthority, ManagerError> {
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
