//! Durable encrypted materialization preparation before transaction signing.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use outbe_ocomp_protocol::{
    list::{leaf_hash, node_hash, pad_hash},
    nod_materialization::{
        verify_nod_materialization_batch, NodMaterializationBatchV1, NodMaterializationHeadV1,
    },
    ListKind, ProtocolError,
};
use outbe_ocomp_protocol::{nod_materialization::ProtectedNodMaterializationV2, SchemaLimits};
#[cfg(test)]
use outbe_primitives::time::WorldwideDay;
#[cfg(test)]
use outbe_primitives::wwd_entity_id::WwdEntityId;
use thiserror::Error;

use crate::nod_materialization::BuiltNodMaterializationBatchV1;

const PREPARED_FILE: &str = "protected-batch-v2.bin";
const TEMP_FILE: &str = "protected-batch-v2.tmp";

/// Prepares state-backed sources only after the enclave requests their identities.
/// A retry uses the actual prefix. A cached carrier reopens against that cached
/// prefix rather than the maximum candidate size.
pub fn prepare_state_protected_materialization(
    authority: outbe_tee::nod_state_materialization::StateNodAuthority,
    (head, built): (&NodMaterializationHeadV1, &BuiltNodMaterializationBatchV1),
    league_fractions: Vec<outbe_tee::nod_materialization::LeagueFractionInputV2>,
    reader: &mut impl outbe_tee::nod_state_materialization::TributeSourceReader,
    store: &PreparedNodMaterializationStoreV2,
) -> Result<ProtectedNodMaterializationV2, ProtectedNodPreparationError> {
    use outbe_tee::nod_state_materialization::{
        open_state_nods, prepare_state_nods, PrepareStateNodsRequest,
    };
    if !authority.source_bindings.is_empty() {
        return Err(ProtectedNodPreparationError::SourceBinding);
    }
    verify_nod_materialization_batch(&built.batch, head, authority.subtree_height, &store.limits)?;
    if let Some(carrier) = store.load()? {
        if carrier.queue_sequence != head.queue_sequence
            || carrier.first_nod_ordinal != head.next_nod_ordinal
        {
            return Err(ProtectedNodPreparationError::ConflictingReplay);
        }
        let cached_authority = state_authority_for_prefix(
            authority,
            &built.batch,
            carrier.encrypted_nods.len(),
            reader,
        )?;
        open_state_nods(&cached_authority, &carrier)?;
        return Ok(carrier);
    }
    let carrier = prepare_capacity_bounded(
        head,
        built.batch.clone(),
        authority.subtree_height,
        &store.limits,
        |batch| {
            Ok(prepare_state_nods(
                PrepareStateNodsRequest {
                    authority: authority.clone(),
                    batch: batch.encode_canonical(&store.limits)?,
                    league_fractions: league_fractions.clone(),
                    sources: Vec::new(),
                },
                reader,
            )?)
        },
    )?;
    store.persist(&carrier)?;
    Ok(carrier)
}

fn state_authority_for_prefix(
    mut authority: outbe_tee::nod_state_materialization::StateNodAuthority,
    batch: &NodMaterializationBatchV1,
    count: usize,
    reader: &mut impl outbe_tee::nod_state_materialization::TributeSourceReader,
) -> Result<outbe_tee::nod_state_materialization::StateNodAuthority, ProtectedNodPreparationError> {
    if count == 0 || count > batch.actions.len() || !authority.source_bindings.is_empty() {
        return Err(ProtectedNodPreparationError::SourceBinding);
    }
    for action in &batch.actions[..count] {
        let id = outbe_primitives::wwd_entity_id::WwdEntityId::from(action.tribute_id);
        let source = reader.read_tribute(&authority.source, id)?;
        if source.tribute.context.tribute_id != id {
            return Err(ProtectedNodPreparationError::SourceBinding);
        }
        authority.source_bindings.push(source.binding()?);
    }
    Ok(authority)
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
                entry_price_minor: U256::from(510),
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
    fn cached_state_authority_resolves_only_the_actual_carrier_prefix() {
        use outbe_primitives::{
            storage::proof::AccountStorageProof,
            tribute_encryption::{EncryptedTributeV2, TributeContextV2},
        };
        use outbe_tee::{
            nod_state_materialization::{StateNodAuthority, TributeSourceReader},
            tribute_source::{TributeSourceCheckpoint, TributeSourceSubject, TributeStateSource},
        };
        struct Reader {
            subject: TributeSourceSubject,
            ids: Vec<WwdEntityId>,
        }
        impl TributeSourceReader for Reader {
            fn read_tribute(
                &mut self,
                subject: &TributeSourceSubject,
                id: WwdEntityId,
            ) -> Result<TributeStateSource, outbe_tee::TransportError> {
                assert_eq!(subject, &self.subject);
                self.ids.push(id);
                // This regression exercises prefix selection. MPT validation is
                // covered by the real source-proof and enclave engine tests.
                Ok(TributeStateSource {
                    tribute: EncryptedTributeV2 {
                        context: TributeContextV2 {
                            chain_id: subject.chain_id,
                            tribute_id: id,
                            owner: alloy_primitives::Address::repeat_byte(1),
                            worldwide_day: subject.worldwide_day,
                            issuance_currency: 840,
                            reference_currency: 840,
                            tribute_price_minor: alloy_primitives::U256::ONE,
                            offer_input_hash: B256::repeat_byte(0x31),
                        },
                        encrypted_creator_public_key: vec![0; 56],
                        encrypted_amounts: vec![0; 88],
                    },
                    day_index: self.ids.len() as u32 - 1,
                    expected_words: vec![],
                    proof: AccountStorageProof {
                        address: outbe_primitives::addresses::TRIBUTE_ADDRESS,
                        nonce: 0,
                        balance: alloy_primitives::U256::ZERO,
                        code_hash: B256::ZERO,
                        storage_root: B256::ZERO,
                        account_nodes: vec![],
                        slots: vec![],
                    },
                })
            }
        }
        let (actions, head, proofs) = population(10);
        let batch = batch_at(&actions, &head, &proofs, 3);
        assert_eq!(batch.actions.len(), 8);
        let source = TributeSourceSubject {
            checkpoint: TributeSourceCheckpoint {
                block_number: 42,
                block_hash: B256::repeat_byte(0x21),
                state_root: B256::repeat_byte(0x22),
            },
            chain_id: 1,
            worldwide_day: WorldwideDay::new(head.worldwide_day),
            tribute_count: head.nod_count,
        };
        let authority = StateNodAuthority {
            source,
            head: head.encode_canonical(&poc_schema_limits()).unwrap(),
            subtree_height: 3,
            source_bindings: vec![],
        };
        let mut reader = Reader {
            subject: source,
            ids: vec![],
        };
        let cached = state_authority_for_prefix(authority.clone(), &batch, 2, &mut reader).unwrap();
        assert_eq!(cached.source_bindings.len(), 2);
        assert_eq!(
            reader.ids,
            batch.actions[..2]
                .iter()
                .map(|a| WwdEntityId::from(a.tribute_id))
                .collect::<Vec<_>>()
        );
        assert_eq!(cached.source, source);
        assert!(state_authority_for_prefix(authority.clone(), &batch, 0, &mut reader).is_err());
        assert!(state_authority_for_prefix(authority, &batch, 9, &mut reader).is_err());
        assert_eq!(reader.ids.len(), 2);
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
        let replay = store.load().unwrap().unwrap();
        assert_eq!(replay, chosen);
        assert_eq!(replay.encrypted_nods.len(), 2);
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
        bad.actions[0].entry_price_minor += alloy_primitives::U256::from(1);
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
