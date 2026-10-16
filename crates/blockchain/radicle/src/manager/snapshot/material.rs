use super::*;
use crate::manager::FinalizedMaterialSigningState;
use outbe_primitives::radicle_publication::PublicationAction;

impl<P: StateProviderFactory> RethSnapshotReader<P> {
    pub fn read_current_material_signing_state(
        &self,
    ) -> Result<FinalizedMaterialSigningState, ManagerError> {
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
        let storage = StorageHandle::new(&mut storage);
        let registry = RadicleRegistry::new(storage.clone());
        let sequence = registry
            .publication_sequence
            .read()
            .map_err(snapshot_error)?;
        let bound = registry
            .publication_material_sequence
            .read()
            .map_err(snapshot_error)?;
        if sequence == 0 || bound == 0 || bound > sequence {
            return Err(ManagerError::Snapshot(
                "material binding requires accepted bootstrap".into(),
            ));
        }
        let root = registry
            .publication_receipt(1)
            .map_err(snapshot_error)?
            .ok_or_else(|| ManagerError::Snapshot("bootstrap receipt missing".into()))?;
        let checkpoint = registry
            .publication_receipt(sequence)
            .map_err(snapshot_error)?
            .ok_or_else(|| ManagerError::Snapshot("latest checkpoint missing".into()))?;
        let material = registry
            .publication_receipt(bound)
            .map_err(snapshot_error)?
            .ok_or_else(|| ManagerError::Snapshot("bound material missing".into()))?;
        let repository = outbe_radicleregistry::repo_id_from_storage_key(
            registry.network_repository.read().map_err(snapshot_error)?,
        )
        .map_err(snapshot_error)?;
        let descriptor = registry
            .config_descriptor_hash
            .read()
            .map_err(snapshot_error)?;
        if root.body.action != PublicationAction::Bootstrap
            || !matches!(
                material.body.action,
                PublicationAction::Bootstrap | PublicationAction::BindMaterial
            )
            || checkpoint.receipt_hash
                != registry
                    .publication_receipt_hash
                    .read()
                    .map_err(snapshot_error)?
            || authority.committee().vrf_group_public_key_bytes.as_slice()
                != root.body.owner_bls_public.as_slice()
            || [&root, &checkpoint, &material].iter().any(|r| {
                r.accepted_height > block.number
                    || r.body.chain_id != self.chain_id
                    || r.body.genesis_hash != self.genesis_hash
                    || r.body.repo_id != repository
                    || r.body.owner_bls_public != root.body.owner_bls_public
                    || r.body.descriptor_hash != descriptor
            })
        {
            return Err(ManagerError::Snapshot(
                "material binding chain/repository mismatch".into(),
            ));
        }
        let height = ValidatorSet::new(storage)
            .epoch_snapshot()
            .map_err(snapshot_error)?
            .start_block;
        if height > block.number {
            return Err(ManagerError::Finality(
                "material activation is not finalized".into(),
            ));
        }
        let hash = if height == 0 {
            self.genesis_hash
        } else {
            self.provider
                .block_hash(height)
                .map_err(ManagerError::provider)?
                .filter(|h| !h.is_zero())
                .ok_or_else(|| {
                    ManagerError::Finality("material activation anchor missing".into())
                })?
        };
        Ok(FinalizedMaterialSigningState {
            authority,
            checkpoint: FinalizedPublicationReceipt {
                block,
                receipt: checkpoint,
            },
            material: FinalizedPublicationReceipt {
                block,
                receipt: material,
            },
            anchor: FinalizedBlock {
                number: height,
                hash,
            },
        })
    }
}
