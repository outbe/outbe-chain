//! Durable encrypted materialization preparation before transaction signing.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use outbe_compressed_entities::{
    verify_body_in_collection, CeDomain, TributePartitionExpectationV1, TributeProofArchiveV1,
    WwdEntityId, ACTIVE_COMMITMENT_SCHEME,
};
use outbe_ocomp_protocol::{
    input::InputChunkKind,
    list::{leaf_hash, node_hash, pad_hash},
    nod_materialization::{
        verify_nod_materialization_batch, NodMaterializationBatchV1, NodMaterializationHeadV1,
    },
    CasObjectRefV1, ListKind, ObjectKind, ProtocolError,
};
use outbe_ocomp_protocol::{nod_materialization::ProtectedNodMaterializationV2, SchemaLimits};
use outbe_primitives::time::WorldwideDay;
use outbe_tee::nod_materialization::{
    NodMaterializationAuthorityV2, NodSourceV2, PrepareEncryptedNodsRequestV2,
};
use thiserror::Error;

use crate::{
    input_inventory::SOURCE_PROOF_ARCHIVE_DIRECTORY,
    input_ref_catalog::VerifiedInputChunkRefCatalog, lysis_plan_audit::LocalLysisPlanAuditV1,
    nod_materialization::BuiltNodMaterializationBatchV1,
};

const PREPARED_FILE: &str = "protected-batch-v2.bin";
const TEMP_FILE: &str = "protected-batch-v2.tmp";

/// Bound the private sources to the same frozen collection used by Lysis.
pub fn materialization_sources(
    audit: &LocalLysisPlanAuditV1<'_>,
    input_refs: &VerifiedInputChunkRefCatalog,
    built: &BuiltNodMaterializationBatchV1,
    inventory_root: &Path,
) -> Result<(Vec<NodSourceV2>, Vec<CasObjectRefV1>), ProtectedNodPreparationError> {
    let archive = outbe_compressed_entities::open_tribute_proof_archive(
        inventory_root.join(SOURCE_PROOF_ARCHIVE_DIRECTORY),
        TributePartitionExpectationV1 {
            day: WorldwideDay::new(audit.manifest().wwd),
            exact_leaf_count: audit.manifest().tribute_count,
            expected_collection_root: audit.manifest().sealed_tribute_collection_root,
            commitment_scheme: ACTIVE_COMMITMENT_SCHEME,
        },
    )?;
    let mut sources = Vec::with_capacity(built.batch.actions.len());
    let mut references = Vec::new();
    let mut cached_chunk: Option<(u32, crate::input_ref_catalog::VerifiedInputChunkRefV1)> = None;
    for action in &built.batch.actions {
        let ordinal =
            action.raw_ordinal / outbe_lysis::program_v1::planner::PRIMARY_WORK_SHARD_SIZE;
        if cached_chunk
            .as_ref()
            .is_none_or(|(cached, _)| *cached != ordinal)
        {
            let verified = input_refs.verified_reference_at(
                ordinal,
                audit.reader(),
                audit.bundle().bundle(),
            )?;
            if verified.reference.kind != InputChunkKind::Tribute {
                return Err(ProtectedNodPreparationError::SourceBinding);
            }
            references.push(CasObjectRefV1 {
                transport_digest: verified.reference.transport_digest,
                encoded_bytes: verified.reference.encoded_bytes,
                expected_ocb1_kind: Some(ObjectKind::AuthenticatedInputChunkV1.tag()),
            });
            cached_chunk = Some((ordinal, verified));
        }
        let (_, verified) = cached_chunk
            .as_ref()
            .ok_or(ProtectedNodPreparationError::SourceBinding)?;
        sources.push(encrypted_source(
            action,
            &verified.chunk,
            &archive,
            audit.manifest().sealed_tribute_collection_root,
        )?);
    }
    Ok((sources, references))
}

fn encrypted_source(
    action: &outbe_ocomp_protocol::result::NodActionV1,
    chunk: &outbe_ocomp_protocol::input::AuthenticatedInputChunkV1,
    archive: &TributeProofArchiveV1,
    frozen_root: alloy_primitives::B256,
) -> Result<NodSourceV2, ProtectedNodPreparationError> {
    let id = WwdEntityId::from(action.tribute_id);
    for canonical in &chunk.canonical_records_or_openings {
        let record = outbe_tribute::record::decode_canonical(&canonical.0)?;
        if record.tribute_id != id {
            continue;
        }
        if record.owner != action.owner || record.worldwide_day.value() != action.wwd {
            return Err(ProtectedNodPreparationError::SourceBinding);
        }
        let tribute = record
            .encrypted()
            .ok_or(ProtectedNodPreparationError::SourceBinding)?
            .clone();
        let proof = archive.proof(id)?;
        verify_body_in_collection(
            frozen_root,
            CeDomain::Tribute,
            id,
            &record.stored_body()?.encode(),
            &proof,
        )?;
        return Ok(NodSourceV2 {
            tribute,
            proof: postcard::to_allocvec(&proof)?,
        });
    }
    Err(ProtectedNodPreparationError::SourceBinding)
}

pub fn prepare_protected_materialization(
    authority: NodMaterializationAuthorityV2,
    head: &NodMaterializationHeadV1,
    built: &BuiltNodMaterializationBatchV1,
    sources: Vec<NodSourceV2>,
    store: &PreparedNodMaterializationStoreV2,
) -> Result<ProtectedNodMaterializationV2, ProtectedNodPreparationError> {
    if let Some(carrier) = store.load()? {
        if carrier.queue_sequence != head.queue_sequence
            || carrier.first_nod_ordinal != head.next_nod_ordinal
        {
            return Err(ProtectedNodPreparationError::ConflictingReplay);
        }
        outbe_tee::nod_materialization::open_encrypted_nods(&authority, &carrier)?;
        return Ok(carrier);
    }
    if sources.len() != built.batch.actions.len() {
        return Err(ProtectedNodPreparationError::SourceBinding);
    }
    let carrier = prepare_capacity_bounded(
        head,
        built.batch.clone(),
        authority.subtree_height,
        &store.limits,
        |batch| {
            Ok(outbe_tee::nod_materialization::prepare_encrypted_nods(
                PrepareEncryptedNodsRequestV2 {
                    authority: authority.clone(),
                    batch: batch.encode_canonical(&store.limits)?,
                    sources: sources[..batch.actions.len()].to_vec(),
                },
            )?)
        },
    )?;
    store.persist(&carrier)?;
    Ok(carrier)
}

fn prepare_capacity_bounded(
    head: &NodMaterializationHeadV1,
    mut batch: NodMaterializationBatchV1,
    maximum_height: u8,
    limits: &SchemaLimits,
    mut prepare: impl FnMut(
        &NodMaterializationBatchV1,
    ) -> Result<ProtectedNodMaterializationV2, ProtectedNodPreparationError>,
) -> Result<ProtectedNodMaterializationV2, ProtectedNodPreparationError> {
    loop {
        verify_nod_materialization_batch(&batch, head, maximum_height, limits)?;
        let result = prepare(&batch)
            .and_then(|carrier| validate_prepared_carrier(carrier, &batch, head, limits));
        match result {
            Ok(carrier) => return Ok(carrier),
            Err(error) if error.is_capacity_exceeded() && batch.actions.len() > 1 => {
                batch = halve_materialization_batch(batch, head, limits)?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn validate_prepared_carrier(
    carrier: ProtectedNodMaterializationV2,
    batch: &NodMaterializationBatchV1,
    head: &NodMaterializationHeadV1,
    limits: &SchemaLimits,
) -> Result<ProtectedNodMaterializationV2, ProtectedNodPreparationError> {
    if carrier.queue_sequence != head.queue_sequence
        || carrier.first_nod_ordinal != head.next_nod_ordinal
        || carrier.encrypted_nods.len() != batch.actions.len()
    {
        return Err(ProtectedNodPreparationError::SourceBinding);
    }
    // Validate every final public encoding before publishing the durable carrier.
    outbe_ocomp_protocol::abi::encode_protected_materialize_certified_nods_calldata(
        &carrier, limits,
    )?;
    Ok(carrier)
}

/// Keep the left prefix and move its right sibling into the certified root path.
fn halve_materialization_batch(
    mut batch: NodMaterializationBatchV1,
    head: &NodMaterializationHeadV1,
    limits: &SchemaLimits,
) -> Result<NodMaterializationBatchV1, ProtocolError> {
    let tree_height = head
        .nod_count
        .checked_next_power_of_two()
        .ok_or(ProtocolError::IntegerOverflow {
            what: "materialization padded count",
        })?
        .trailing_zeros() as usize;
    let height =
        tree_height
            .checked_sub(batch.root_path.len())
            .ok_or(ProtocolError::InvalidInvariant(
                "materialization root path exceeds tree height",
            ))?;
    if height == 0 {
        return Err(ProtocolError::InvalidInvariant(
            "materialization minimum capacity",
        ));
    }
    let half = 1_u32 << (height - 1);
    let sibling = materialization_right_half_root(&batch, half, height, limits)?;
    batch.actions.truncate(half as usize);
    batch.root_path.insert(0, sibling);
    Ok(batch)
}

fn materialization_right_half_root(
    batch: &NodMaterializationBatchV1,
    half: u32,
    height: usize,
    limits: &SchemaLimits,
) -> Result<alloy_primitives::B256, ProtocolError> {
    let sibling_start =
        batch
            .first_nod_ordinal
            .checked_add(half)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "materialization sibling start",
            })?;
    let mut nodes = Vec::with_capacity(half as usize);
    for offset in 0..half {
        let ordinal = sibling_start
            .checked_add(offset)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "materialization sibling ordinal",
            })?;
        nodes.push(
            if let Some(action) = batch.actions.get((half + offset) as usize) {
                leaf_hash(
                    ListKind::NodActions,
                    ordinal,
                    &action.encode_canonical_record(limits)?,
                )?
            } else {
                pad_hash(ListKind::NodActions, ordinal)?
            },
        );
    }
    for level in 1..height as u16 {
        let mut parents = Vec::with_capacity(nodes.len() / 2);
        for (index, pair) in nodes.as_chunks::<2>().0.iter().enumerate() {
            parents.push(node_hash(
                ListKind::NodActions,
                level,
                (sibling_start >> level) + index as u32,
                pair[0],
                pair[1],
            )?);
        }
        nodes = parents;
    }
    Ok(nodes[0])
}

pub struct PreparedNodMaterializationStoreV2 {
    root: PathBuf,
    limits: SchemaLimits,
}

impl PreparedNodMaterializationStoreV2 {
    pub fn open(root: &Path, limits: SchemaLimits) -> Result<Self, ProtectedNodPreparationError> {
        fs::create_dir_all(root)?;
        let metadata = fs::symlink_metadata(root)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(ProtectedNodPreparationError::UnsafePath);
        }
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            root: root.to_owned(),
            limits,
        })
    }

    pub fn load(
        &self,
    ) -> Result<Option<ProtectedNodMaterializationV2>, ProtectedNodPreparationError> {
        let path = self.root.join(PREPARED_FILE);
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > self.limits.codec.max_body_bytes as u64 {
            return Err(ProtectedNodPreparationError::UnsafePath);
        }
        let mut bytes = Vec::new();
        file.take(self.limits.codec.max_body_bytes as u64 + 1)
            .read_to_end(&mut bytes)?;
        Ok(Some(ProtectedNodMaterializationV2::decode_canonical(
            &bytes,
            &self.limits,
        )?))
    }

    pub fn persist(
        &self,
        carrier: &ProtectedNodMaterializationV2,
    ) -> Result<(), ProtectedNodPreparationError> {
        if let Some(existing) = self.load()? {
            return if existing == *carrier {
                Ok(())
            } else {
                Err(ProtectedNodPreparationError::ConflictingReplay)
            };
        }
        let bytes = carrier.encode_canonical(&self.limits)?;
        let temporary = self.root.join(TEMP_FILE);
        if let Ok(metadata) = fs::symlink_metadata(&temporary) {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(ProtectedNodPreparationError::UnsafePath);
            }
            // A crash before publication cannot authorize a transaction. Recreate
            // this owned staging file; the published canonical file is immutable.
            fs::remove_file(&temporary)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(temporary, self.root.join(PREPARED_FILE))?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ProtectedNodPreparationError {
    #[error("encrypted NOD source does not match the certified Tribute")]
    SourceBinding,
    #[error("encrypted NOD preparation path is unsafe")]
    UnsafePath,
    #[error("encrypted NOD preparation conflicts with the durable carrier")]
    ConflictingReplay,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Protocol(#[from] outbe_ocomp_protocol::ProtocolError),
    #[error(transparent)]
    Enclave(#[from] outbe_tee::TransportError),
    #[error(transparent)]
    Reconstruction(#[from] outbe_compressed_entities::TributePartitionReconstructionError),
    #[error(transparent)]
    SourceProof(#[from] outbe_compressed_entities::PointReadServiceError),
    #[error(transparent)]
    SourceBody(#[from] outbe_compressed_entities::CanonicalBodyError),
    #[error(transparent)]
    InputCatalog(#[from] crate::input_ref_catalog::InputRefCatalogError),
    #[error(transparent)]
    Postcard(#[from] postcard::Error),
}

impl ProtectedNodPreparationError {
    fn is_capacity_exceeded(&self) -> bool {
        matches!(
            self,
            Self::Protocol(ProtocolError::CapacityExceeded { .. })
                | Self::Enclave(outbe_tee::TransportError::NodMaterializationCapacityExceeded(_))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use outbe_ocomp_protocol::{
        common::BoundedBytes, profile::poc_schema_limits, test_utils::nod_action_population,
    };

    fn carrier() -> ProtectedNodMaterializationV2 {
        ProtectedNodMaterializationV2 {
            queue_sequence: 1,
            first_nod_ordinal: 0,
            encryption_binding: B256::repeat_byte(1),
            encrypted_witness: BoundedBytes(vec![0xa1; 200]),
            encrypted_nods: vec![BoundedBytes(vec![0xb1; 100])],
        }
    }

    fn population(
        count: u32,
    ) -> (
        Vec<outbe_ocomp_protocol::result::NodActionV1>,
        NodMaterializationHeadV1,
        Vec<Vec<B256>>,
    ) {
        use alloy_primitives::{Address, U256};
        let day = 20_260_812_u32;
        let id = |ordinal: u32| {
            let mut bytes = [0_u8; 32];
            bytes[..4].copy_from_slice(&day.to_be_bytes());
            bytes[28..].copy_from_slice(&ordinal.to_be_bytes());
            B256::from(bytes)
        };
        let actions: Vec<_> = (0..count)
            .map(|ordinal| outbe_ocomp_protocol::result::NodActionV1 {
                raw_ordinal: ordinal,
                tribute_id: id(ordinal + 1),
                nod_id: id(ordinal + 1000),
                owner: Address::repeat_byte(1),
                wwd: day,
                league_id: 1,
                gratis_load_minor: U256::from(1000),
                entry_price_minor: U256::from(510),
                settlement_cost_minor: U256::from(2),
                issuance_currency: 840,
                reference_currency: 840,
            })
            .collect();
        let (root, proofs) = nod_action_population(&actions);
        let head = NodMaterializationHeadV1 {
            queue_sequence: 1,
            job_id: B256::repeat_byte(1),
            program_semantics_hash: B256::repeat_byte(2),
            worldwide_day: day,
            generation: 1,
            nod_root: root,
            nod_count: count,
            next_nod_ordinal: 0,
            last_progress_height: 1,
        };
        (actions, head, proofs)
    }

    fn batch_at(
        actions: &[outbe_ocomp_protocol::result::NodActionV1],
        head: &NodMaterializationHeadV1,
        proofs: &[Vec<B256>],
        maximum: u8,
    ) -> NodMaterializationBatchV1 {
        let tree_height = head.nod_count.next_power_of_two().trailing_zeros() as u8;
        let height =
            crate::nod_materialization::aligned_subtree_height(head.next_nod_ordinal, maximum)
                .min(tree_height);
        let count = (head.nod_count - head.next_nod_ordinal).min(1 << height) as usize;
        NodMaterializationBatchV1 {
            queue_sequence: head.queue_sequence,
            first_nod_ordinal: head.next_nod_ordinal,
            actions: actions
                [head.next_nod_ordinal as usize..head.next_nod_ordinal as usize + count]
                .to_vec(),
            root_path: proofs[head.next_nod_ordinal as usize][height as usize..].to_vec(),
        }
    }

    fn carrier_for(batch: &NodMaterializationBatchV1) -> ProtectedNodMaterializationV2 {
        let mut result = carrier();
        result.first_nod_ordinal = batch.first_nod_ordinal;
        result.encrypted_witness = BoundedBytes(vec![0xa1; batch.actions.len() * 200]);
        result.encrypted_nods = vec![BoundedBytes(vec![0xb1; 100]); batch.actions.len()];
        result
    }

    #[test]
    fn encoded_capacity_fallback_preserves_root_then_reuses_durable_prefix() {
        let (actions, head, proofs) = population(10);
        let original = batch_at(&actions, &head, &proofs, 3);
        let mut limits = poc_schema_limits();
        limits.codec.max_body_bytes = 1100;
        let mut attempts = Vec::new();
        let chosen = prepare_capacity_bounded(&head, original.clone(), 3, &limits, |batch| {
            attempts.push(batch.actions.len());
            Ok(carrier_for(batch))
        })
        .unwrap();
        assert_eq!(attempts, [8, 4, 2]);
        assert_eq!(chosen.encrypted_nods.len(), 2);
        let directory = tempfile::tempdir().unwrap();
        let store = PreparedNodMaterializationStoreV2::open(directory.path(), limits).unwrap();
        store.persist(&chosen).unwrap();
        drop(store);
        let store = PreparedNodMaterializationStoreV2::open(directory.path(), limits).unwrap();
        fn no_prepare(
            _: &PrepareEncryptedNodsRequestV2,
        ) -> Result<ProtectedNodMaterializationV2, outbe_tee::TransportError> {
            panic!("durable retry must not create new ciphertexts")
        }
        fn open(
            _: &NodMaterializationAuthorityV2,
            _: &ProtectedNodMaterializationV2,
        ) -> Result<Vec<outbe_primitives::nod_encryption::EncryptedNodV2>, outbe_tee::TransportError>
        {
            Ok(Vec::new())
        }
        let _scope = outbe_tee::nod_materialization::test_support::scope(no_prepare, open);
        let replay = prepare_protected_materialization(
            NodMaterializationAuthorityV2 {
                chain_id: 1,
                head: head.encode_canonical(&poc_schema_limits()).unwrap(),
                subtree_height: 3,
                sealed_tribute_root: B256::repeat_byte(1),
            },
            &head,
            &BuiltNodMaterializationBatchV1 {
                batch: original,
                dependencies: Vec::new(),
            },
            Vec::new(),
            &store,
        )
        .unwrap();
        assert_eq!(replay, chosen);
    }

    #[test]
    fn adaptive_cursor_covers_every_action_once_including_padded_remainder() {
        let (actions, mut head, proofs) = population(259);
        let limits = poc_schema_limits();
        let mut visited = Vec::new();
        while head.next_nod_ordinal < head.nod_count {
            let original = batch_at(&actions, &head, &proofs, 8);
            let chosen = prepare_capacity_bounded(&head, original, 8, &limits, |batch| {
                if batch.actions.len() > 3 {
                    return Err(
                        outbe_tee::TransportError::NodMaterializationCapacityExceeded(
                            "fixture transfer capacity".into(),
                        )
                        .into(),
                    );
                }
                visited.extend(batch.actions.iter().map(|action| action.raw_ordinal));
                Ok(carrier_for(batch))
            })
            .unwrap();
            head.next_nod_ordinal += chosen.encrypted_nods.len() as u32;
        }
        assert_eq!(visited, (0..259).collect::<Vec<_>>());
    }

    #[test]
    fn padded_final_subtree_can_shrink_to_one_without_changing_the_root() {
        let (actions, mut head, proofs) = population(10);
        head.next_nod_ordinal = 8;
        let original = batch_at(&actions, &head, &proofs, 3);
        let mut attempted_paths = Vec::new();
        let chosen = prepare_capacity_bounded(&head, original, 3, &poc_schema_limits(), |batch| {
            attempted_paths.push(batch.root_path.len());
            if batch.actions.len() > 1 {
                return Err(
                    outbe_tee::TransportError::NodMaterializationCapacityExceeded(
                        "fixture single-action transport".into(),
                    )
                    .into(),
                );
            }
            assert_eq!(batch.actions[0].raw_ordinal, 8);
            Ok(carrier_for(batch))
        })
        .unwrap();
        assert_eq!(attempted_paths, [1, 2, 3, 4]);
        assert_eq!(chosen.encrypted_nods.len(), 1);
    }

    #[test]
    fn invalid_proofs_crypto_failures_and_single_action_capacity_never_retry() {
        let (actions, head, proofs) = population(8);
        let original = batch_at(&actions, &head, &proofs, 3);
        let limits = poc_schema_limits();
        let mut calls = 0;
        let result = prepare_capacity_bounded(&head, original.clone(), 3, &limits, |_| {
            calls += 1;
            Err(outbe_tee::TransportError::EnclaveError("crypto failure".into()).into())
        });
        assert!(matches!(
            result,
            Err(ProtectedNodPreparationError::Enclave(
                outbe_tee::TransportError::EnclaveError(_)
            ))
        ));
        assert_eq!(calls, 1);
        let mut bad = original;
        bad.actions[0].gratis_load_minor += alloy_primitives::U256::from(1);
        assert!(prepare_capacity_bounded(&head, bad, 3, &limits, |_| panic!(
            "invalid proof must not reach enclave"
        ))
        .is_err());
        let single = batch_at(&actions, &head, &proofs, 0);
        let mut calls = 0;
        assert!(prepare_capacity_bounded(&head, single, 3, &limits, |_| {
            calls += 1;
            Err(
                outbe_tee::TransportError::NodMaterializationCapacityExceeded(
                    "single action too large".into(),
                )
                .into(),
            )
        })
        .is_err());
        assert_eq!(calls, 1);
    }

    #[test]
    fn cold_restart_reuses_exact_ciphertexts_and_rejects_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            PreparedNodMaterializationStoreV2::open(directory.path(), poc_schema_limits()).unwrap();
        assert!(store.load().unwrap().is_none());
        store.persist(&carrier()).unwrap();
        drop(store);
        let reopened =
            PreparedNodMaterializationStoreV2::open(directory.path(), poc_schema_limits()).unwrap();
        assert_eq!(reopened.load().unwrap(), Some(carrier()));
        reopened.persist(&carrier()).unwrap();
        let mut replacement = carrier();
        replacement.encrypted_nods[0].0[0] ^= 1;
        assert!(matches!(
            reopened.persist(&replacement),
            Err(ProtectedNodPreparationError::ConflictingReplay)
        ));
        assert_eq!(reopened.load().unwrap(), Some(carrier()));
    }

    #[test]
    fn interrupted_staging_is_replaced_but_corrupt_published_bytes_fail() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            PreparedNodMaterializationStoreV2::open(directory.path(), poc_schema_limits()).unwrap();
        fs::write(directory.path().join(TEMP_FILE), b"interrupted").unwrap();
        store.persist(&carrier()).unwrap();
        fs::write(directory.path().join(PREPARED_FILE), b"corrupt").unwrap();
        assert!(store.load().is_err());
        assert!(store.persist(&carrier()).is_err());
    }
}
