//! Immutable MPT source witnesses retained independently of node history pruning.

use outbe_primitives::storage::{
    proof::AccountStorageProof, readonly::ReadOnlyStorageProvider, StorageHandle,
};
use outbe_tee::{
    nod_state_materialization::TributeSourceReader,
    tribute_source::{TributeSourceSubject, TributeStateSource},
    TransportError,
};
use outbe_tribute::state_proof::{TributeDayProofVerifier, TributeRecordStateProof};
use serde::{Deserialize, Serialize};

use super::*;

mod order;

const CANONICAL_INDEX: &str = "state-proofs.canonical-index";

const STATE_HEADER: &str = "state-proofs.header";
const STATE_INDEX: &str = "state-proofs.index";
const RECORDS: &str = "records";

#[derive(Serialize, Deserialize)]
struct Header {
    magic: [u8; 8],
    subject: TributeSourceSubject,
    day_index_proof: AccountStorageProof,
    index_digest: B256,
    canonical_index_digest: B256,
}

/// A build is unpublished until complete physical day coverage has been verified.
pub struct StateTributeProofArchiveBuilder {
    root: PathBuf,
    build: PathBuf,
    subject: TributeSourceSubject,
    day_index_proof: AccountStorageProof,
    verifier: TributeDayProofVerifier,
    count: u32,
    index: File,
    lock: InventoryLock,
}

impl StateTributeProofArchiveBuilder {
    pub fn create(
        root: impl AsRef<Path>,
        subject: TributeSourceSubject,
        day_index_proof: AccountStorageProof,
    ) -> Result<Self, TributeInventoryError> {
        let root = root.as_ref().to_path_buf();
        create_private_directory(&root)?;
        let lock = InventoryLock::acquire(&root)?;
        if path_exists(&root.join(STATE_HEADER))? {
            return Err(TributeInventoryError::AlreadySealed);
        }
        let verifier = TributeDayProofVerifier::new(subject.into(), &day_index_proof, limits())
            .map_err(|_| TributeInventoryError::Authority("state proof day length"))?;
        let build = root.join(BUILD_DIRECTORY);
        remove_owned_build_directory(&build)?;
        remove_owned_build_directory(&root.join(RECORDS))?;
        for path in [
            root.join(STATE_INDEX),
            root.join(CANONICAL_INDEX),
            root.join(STATE_HEADER).with_extension("tmp"),
        ] {
            if path_exists(&path)? {
                open_regular_readonly(&path)?;
                fs::remove_file(&path)
                    .map_err(|e| io_error("remove incomplete state proof file", &path, e))?;
            }
        }
        create_private_directory(&build.join(RECORDS))?;
        let index_path = build.join(STATE_INDEX);
        let index = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(&index_path)
            .map_err(|e| io_error("create state proof index", &index_path, e))?;
        Ok(Self {
            root,
            build,
            subject,
            day_index_proof,
            verifier,
            count: 0,
            index,
            lock,
        })
    }

    pub fn push(&mut self, proof: &TributeRecordStateProof) -> Result<(), TributeInventoryError> {
        self.verifier.verify_next(proof, limits()).map_err(|_| {
            TributeInventoryError::Authority("state source proof or physical order")
        })?;
        let bytes = encode(proof)?;
        let path = record_path(&self.build, proof.tribute.context.tribute_id);
        create_private_directory(path.parent().expect("record path has a parent"))?;
        persist_new(&path, &bytes)?;
        self.index
            .write_all(proof.tribute.context.tribute_id.as_slice())
            .map_err(|e| io_error("write state proof index", &self.build.join(STATE_INDEX), e))?;
        self.count = self
            .count
            .checked_add(1)
            .ok_or(TributeInventoryError::IntegerOverflow)?;
        Ok(())
    }

    pub fn finish(self) -> Result<StateTributeProofArchive, TributeInventoryError> {
        self.finish_observing(&|| {})
    }

    pub fn finish_observing(
        self,
        on_progress: &impl Fn(),
    ) -> Result<StateTributeProofArchive, TributeInventoryError> {
        self.verifier
            .finish()
            .map_err(|_| TributeInventoryError::Authority("incomplete state proof day"))?;
        if self.count != self.subject.tribute_count {
            return Err(TributeInventoryError::Authority(
                "incomplete state proof index",
            ));
        }
        self.index
            .sync_all()
            .map_err(|e| io_error("fsync state proof index", &self.build.join(STATE_INDEX), e))?;
        order::sort_index(&self.build, on_progress)?;
        // There are at most 256 directories, independent of the day population.
        for entry in fs::read_dir(self.build.join(RECORDS))
            .map_err(|e| io_error("read state proof directories", &self.build, e))?
        {
            let path = entry
                .map_err(|e| io_error("read state proof directory", &self.build, e))?
                .path();
            inspect_private_directory(&path)?;
            sync_directory(&path)?;
        }
        sync_directory(&self.build.join(RECORDS))?;
        let index_digest = digest_file_observing(&self.build.join(STATE_INDEX), on_progress)?;
        let canonical_index_digest =
            digest_file_observing(&self.build.join(CANONICAL_INDEX), on_progress)?;
        for name in [RECORDS, STATE_INDEX, CANONICAL_INDEX] {
            fs::rename(self.build.join(name), self.root.join(name))
                .map_err(|e| io_error("install state proof archive", &self.root, e))?;
        }
        let header = Header {
            magic: *b"OUTBTSP1",
            subject: self.subject,
            day_index_proof: self.day_index_proof,
            index_digest,
            canonical_index_digest,
        };
        persist_atomic(&self.root, &self.root.join(STATE_HEADER), &encode(&header)?)?;
        Ok(StateTributeProofArchive {
            root: self.root,
            header,
            _lock: self.lock,
        })
    }
}

pub struct StateTributeProofArchive {
    root: PathBuf,
    header: Header,
    _lock: InventoryLock,
}

impl StateTributeProofArchive {
    pub const fn subject(&self) -> TributeSourceSubject {
        self.header.subject
    }

    pub fn try_open(
        root: impl AsRef<Path>,
        subject: TributeSourceSubject,
        on_progress: &impl Fn(),
    ) -> Result<Option<Self>, TributeInventoryError> {
        if !path_exists(&root.as_ref().join(STATE_HEADER))? {
            return Ok(None);
        }
        Self::open(root, subject, on_progress).map(Some)
    }

    /// Cold reopen rechecks every source against the immutable finalized root.
    pub fn open(
        root: impl AsRef<Path>,
        subject: TributeSourceSubject,
        on_progress: &impl Fn(),
    ) -> Result<Self, TributeInventoryError> {
        let root = root.as_ref().to_path_buf();
        inspect_private_directory(&root)?;
        let lock = InventoryLock::acquire(&root)?;
        let header = verify_archive_contents(&root, subject, on_progress)?;
        Ok(Self {
            root,
            header,
            _lock: lock,
        })
    }

    /// Inspect an immutable stopped archive without acquiring a writable lock.
    pub fn inspect_readonly(
        root: impl AsRef<Path>,
        subject: TributeSourceSubject,
        on_progress: &impl Fn(),
    ) -> Result<(), TributeInventoryError> {
        let root = root.as_ref();
        inspect_private_directory(root)?;
        let header = verify_archive_contents(root, subject, on_progress)?;
        stream_canonical(root, &header, &mut |_| Ok(()), on_progress)
    }

    /// Streams canonical identity order with one authenticated source in memory.
    /// Count, strict order and valid physical membership establish full coverage.
    pub fn for_each_canonical(
        &self,
        mut accept: impl FnMut(TributeRecordStateProof) -> Result<(), TributeInventoryError>,
        on_progress: &impl Fn(),
    ) -> Result<(), TributeInventoryError> {
        stream_canonical(&self.root, &self.header, &mut accept, on_progress)
    }

    pub fn record(
        &self,
        id: WwdEntityId,
    ) -> Result<TributeRecordStateProof, TributeInventoryError> {
        let proof = load_record(&self.root, id)?;
        proof
            .verify(&self.header.subject.into(), limits())
            .map_err(|_| TributeInventoryError::Authority("archived state source proof"))?;
        Ok(proof)
    }
}

fn verify_archive_contents(
    root: &Path,
    subject: TributeSourceSubject,
    on_progress: &impl Fn(),
) -> Result<Header, TributeInventoryError> {
    let header: Header = read_bounded(&root.join(STATE_HEADER))?;
    if header.magic != *b"OUTBTSP1" || header.subject != subject {
        return Err(TributeInventoryError::Authority(
            "state proof archive subject",
        ));
    }
    let mut index = open_regular_readonly(&root.join(STATE_INDEX))?;
    let index_len = index
        .metadata()
        .map_err(|e| io_error("stat state proof index", root, e))?
        .len();
    if index_len != u64::from(subject.tribute_count) * 32
        || digest_file_observing(&root.join(STATE_INDEX), on_progress)? != header.index_digest
    {
        return Err(TributeInventoryError::Corrupt("state proof index"));
    }
    let canonical = open_regular_readonly(&root.join(CANONICAL_INDEX))?;
    if canonical
        .metadata()
        .map_err(|e| io_error("stat canonical source index", root, e))?
        .len()
        != u64::from(subject.tribute_count) * 32
        || digest_file_observing(&root.join(CANONICAL_INDEX), on_progress)?
            != header.canonical_index_digest
    {
        return Err(TributeInventoryError::Corrupt("canonical source index"));
    }
    let mut verifier =
        TributeDayProofVerifier::new(subject.into(), &header.day_index_proof, limits())
            .map_err(|_| TributeInventoryError::Authority("state proof day length"))?;
    for _ in 0..subject.tribute_count {
        let mut id = [0; 32];
        index
            .read_exact(&mut id)
            .map_err(|e| io_error("read state proof index", root, e))?;
        let id = WwdEntityId::from(B256::from(id));
        let proof = load_record(root, id)?;
        verifier.verify_next(&proof, limits()).map_err(|_| {
            TributeInventoryError::Authority("state archive source proof or physical order")
        })?;
        on_progress();
    }
    verifier
        .finish()
        .map_err(|_| TributeInventoryError::Authority("incomplete state proof archive"))?;
    Ok(header)
}

fn stream_canonical(
    root: &Path,
    header: &Header,
    accept: &mut impl FnMut(TributeRecordStateProof) -> Result<(), TributeInventoryError>,
    on_progress: &impl Fn(),
) -> Result<(), TributeInventoryError> {
    let path = root.join(CANONICAL_INDEX);
    let mut index = open_regular_readonly(&path)?;
    if index
        .metadata()
        .map_err(|e| io_error("stat canonical source index", &path, e))?
        .len()
        != u64::from(header.subject.tribute_count) * 32
    {
        return Err(TributeInventoryError::Corrupt("canonical source count"));
    }
    let mut previous = None;
    for _ in 0..header.subject.tribute_count {
        let mut bytes = [0; 32];
        index
            .read_exact(&mut bytes)
            .map_err(|e| io_error("read canonical source index", &path, e))?;
        let id = WwdEntityId::from(B256::from(bytes));
        if previous.is_some_and(|old| old >= id) {
            return Err(TributeInventoryError::Corrupt("canonical source order"));
        }
        let proof = load_record(root, id)?;
        proof
            .verify(&header.subject.into(), limits())
            .map_err(|_| TributeInventoryError::Authority("archived state source proof"))?;
        accept(proof)?;
        previous = Some(id);
        on_progress();
    }
    Ok(())
}

impl TributeSourceReader for StateTributeProofArchive {
    fn read_tribute(
        &mut self,
        subject: &TributeSourceSubject,
        id: WwdEntityId,
    ) -> Result<TributeStateSource, TransportError> {
        let reject = |error: TributeInventoryError| TransportError::EnclaveError(error.to_string());
        if subject != &self.header.subject {
            return Err(TransportError::EnclaveError(
                "state source archive checkpoint mismatch".into(),
            ));
        }
        let proof = self.record(id).map_err(reject)?;
        let verified = proof
            .account
            .verify(
                outbe_primitives::addresses::TRIBUTE_ADDRESS,
                subject.checkpoint.state_root,
                limits(),
            )
            .map_err(|e| TransportError::EnclaveError(e.to_string()))?;
        let mut provider = ReadOnlyStorageProvider::new_with_chain_identity(
            verified,
            subject.chain_id,
            B256::ZERO,
        );
        StorageHandle::enter(&mut provider, |storage| {
            proof.source_opening(
                &outbe_tribute::TributeContract::new(storage),
                &(*subject).into(),
            )
        })
        .map_err(|e| TransportError::EnclaveError(e.to_string()))
    }
}

fn record_path(root: &Path, id: WwdEntityId) -> PathBuf {
    let name = hex::encode(id.as_slice());
    root.join(RECORDS).join(&name[8..10]).join(name)
}

fn limits() -> outbe_primitives::storage::proof::StateProofLimits {
    outbe_tee::tribute_source::proof_limits()
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>, TributeInventoryError> {
    let bytes = postcard::to_allocvec(value)
        .map_err(|_| TributeInventoryError::Corrupt("state proof encoding"))?;
    if bytes.len() > outbe_ocomp_protocol::profile::poc_schema_limits().max_bounded_bytes {
        return Err(TributeInventoryError::Corrupt("state proof encoded size"));
    }
    Ok(bytes)
}

fn read_bounded<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, TributeInventoryError> {
    reject_symlink_ancestors(path)?;
    let file = open_regular_readonly(path)?;
    let size = file
        .metadata()
        .map_err(|e| io_error("stat state proof", path, e))?
        .len();
    if size > outbe_ocomp_protocol::profile::poc_schema_limits().max_bounded_bytes as u64 {
        return Err(TributeInventoryError::Corrupt("state proof encoded size"));
    }
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(size + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error("read state proof", path, e))?;
    if bytes.len() != size as usize {
        return Err(TributeInventoryError::Corrupt("state proof file changed"));
    }
    postcard::from_bytes(&bytes).map_err(|_| TributeInventoryError::Corrupt("state proof encoding"))
}

fn load_record(
    root: &Path,
    id: WwdEntityId,
) -> Result<TributeRecordStateProof, TributeInventoryError> {
    let proof: TributeRecordStateProof = read_bounded(&record_path(root, id))?;
    if proof.tribute.context.tribute_id != id {
        return Err(TributeInventoryError::Authority(
            "archived Tribute identity",
        ));
    }
    Ok(proof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::keccak256;
    use outbe_ocomp_protocol::test_utils::{account_mpt_with_proofs, storage_trie};
    use outbe_primitives::{
        storage::{hashmap::HashMapStorageProvider, proof::StorageSlotProof},
        tribute_encryption::{TributeAmountsV2, TributeContextV2},
        wwd_entity_id::derive_poseidon_entity_id,
    };
    use outbe_tee::tribute_source::TributeSourceCheckpoint;
    use outbe_tribute::{
        state_proof::{expected_record_words, TributeStateProofSubject},
        storage_records::StoredTribute,
        TributeContract,
    };

    fn fixture() -> (
        TributeSourceSubject,
        AccountStorageProof,
        Vec<TributeRecordStateProof>,
    ) {
        let mut subject = TributeSourceSubject {
            checkpoint: TributeSourceCheckpoint {
                block_number: 42,
                block_hash: B256::repeat_byte(0x71),
                state_root: B256::ZERO,
            },
            chain_id: 54322345,
            worldwide_day: WorldwideDay::new(20261015),
            tribute_count: 2,
        };
        // Expected slot derivation requires a complete checkpoint before the
        // actual test trie root can be calculated.
        subject.checkpoint.state_root = B256::repeat_byte(0x72);
        let mut provider = HashMapStorageProvider::new(subject.chain_id);
        let (bodies, selected, length_slot) = StorageHandle::enter(&mut provider, |storage| {
            let contract = TributeContract::new(storage);
            let mut bodies = Vec::new();
            let mut selected = Vec::new();
            for index in 0..2_u8 {
                let owner = Address::repeat_byte(index + 1);
                let body = outbe_tee_enclave::tribute_encryption::encrypt_tribute(
                    &[0x52; 32],
                    &[0x35; 32],
                    TributeContextV2 {
                        chain_id: subject.chain_id,
                        tribute_id: derive_poseidon_entity_id(owner, subject.worldwide_day)
                            .unwrap(),
                        owner,
                        worldwide_day: subject.worldwide_day,
                        issuance_currency: 840,
                        reference_currency: 840,
                        tribute_price_minor: U256::from(1_000_000),
                        offer_input_hash: B256::repeat_byte(index + 1),
                    },
                    &TributeAmountsV2 {
                        issuance_amount_minor: U256::from(123),
                        nominal_amount_minor: U256::from(100),
                    },
                )
                .unwrap();
                contract
                    .insert_state_record(&StoredTribute::from(&body))
                    .unwrap();
                selected.push(
                    expected_record_words(
                        &contract,
                        &TributeStateProofSubject::from(subject),
                        u32::from(index),
                        &body,
                    )
                    .unwrap()
                    .into_iter()
                    .map(|(slot, _)| slot)
                    .collect::<Vec<_>>(),
                );
                bodies.push(body);
            }
            (
                bodies,
                selected,
                contract.day_ids.list(&subject.worldwide_day).base_slot(),
            )
        });
        let words = provider
            .storage
            .iter()
            .filter(|((address, _), _)| *address == outbe_primitives::addresses::TRIBUTE_ADDRESS)
            .map(|((_, slot), word)| (*slot, *word))
            .collect::<std::collections::BTreeMap<_, _>>()
            .into_iter()
            .collect::<Vec<_>>();
        let (storage_root, nodes) = storage_trie(&words);
        let account = alloy_consensus::TrieAccount {
            nonce: 0,
            balance: U256::ZERO,
            storage_root,
            code_hash: keccak256([]),
        };
        let (root, account_nodes) =
            account_mpt_with_proofs(&[(outbe_primitives::addresses::TRIBUTE_ADDRESS, account)]);
        subject.checkpoint.state_root = root;
        let openings = words
            .into_iter()
            .zip(nodes)
            .map(|((slot, value), nodes)| {
                let slot = B256::from(slot.to_be_bytes::<32>());
                (slot, StorageSlotProof { slot, value, nodes })
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let proof = |slots: &[B256]| AccountStorageProof {
            address: outbe_primitives::addresses::TRIBUTE_ADDRESS,
            nonce: account.nonce,
            balance: account.balance,
            storage_root,
            code_hash: account.code_hash,
            account_nodes: account_nodes[&outbe_primitives::addresses::TRIBUTE_ADDRESS].clone(),
            slots: slots.iter().map(|slot| openings[slot].clone()).collect(),
        };
        let day_proof = proof(&[B256::from(length_slot.to_be_bytes::<32>())]);
        let records = bodies
            .into_iter()
            .zip(selected)
            .enumerate()
            .map(|(index, (tribute, slots))| TributeRecordStateProof {
                day_index: index as u32,
                tribute,
                account: proof(&slots),
            })
            .collect();
        (subject, day_proof, records)
    }

    #[test]
    fn archived_real_mpt_sources_survive_restart_and_serve_the_enclave_subject() {
        let (subject, day, records) = fixture();
        let root = tempfile::tempdir().unwrap();
        let mut builder =
            StateTributeProofArchiveBuilder::create(root.path(), subject, day).unwrap();
        builder.push(&records[0]).unwrap();
        assert!(!root.path().join(STATE_HEADER).exists());
        builder.push(&records[1]).unwrap();
        let archive = builder.finish().unwrap();
        assert_eq!(
            archive
                .record(records[1].tribute.context.tribute_id)
                .unwrap(),
            records[1]
        );
        let mut identities = Vec::new();
        archive
            .for_each_canonical(
                |proof| {
                    identities.push(proof.tribute.context.tribute_id);
                    Ok(())
                },
                &|| {},
            )
            .unwrap();
        let mut expected = records
            .iter()
            .map(|proof| proof.tribute.context.tribute_id)
            .collect::<Vec<_>>();
        expected.sort_unstable();
        assert_eq!(identities, expected);
        drop(archive);
        let mut archive = StateTributeProofArchive::open(root.path(), subject, &|| {}).unwrap();
        let source = archive
            .read_tribute(&subject, records[0].tribute.context.tribute_id)
            .unwrap();
        source
            .verify(&subject, source.binding().unwrap(), limits())
            .unwrap();
        let mut other = subject;
        other.checkpoint.block_hash = B256::repeat_byte(0x81);
        assert!(archive
            .read_tribute(&other, records[0].tribute.context.tribute_id)
            .is_err());
    }

    #[test]
    fn immutable_inspection_checks_complete_sources_without_creating_a_lock() {
        let (subject, day, records) = fixture();
        let root = tempfile::tempdir().unwrap();
        let mut builder =
            StateTributeProofArchiveBuilder::create(root.path(), subject, day).unwrap();
        for record in &records {
            builder.push(record).unwrap();
        }
        drop(builder.finish().unwrap());
        fs::remove_file(root.path().join(LOCK_FILE)).unwrap();
        let header = fs::read(root.path().join(STATE_HEADER)).unwrap();
        StateTributeProofArchive::inspect_readonly(root.path(), subject, &|| {}).unwrap();
        assert!(!root.path().join(LOCK_FILE).exists());
        assert_eq!(fs::read(root.path().join(STATE_HEADER)).unwrap(), header);
        let mut other = subject;
        other.checkpoint.state_root = B256::repeat_byte(0x77);
        assert!(StateTributeProofArchive::inspect_readonly(root.path(), other, &|| {}).is_err());
        let path = record_path(root.path(), records[0].tribute.context.tribute_id);
        fs::write(path, b"invalid source").unwrap();
        assert!(StateTributeProofArchive::inspect_readonly(root.path(), subject, &|| {}).is_err());
        assert!(!root.path().join(LOCK_FILE).exists());
    }

    #[test]
    fn incomplete_build_never_publishes_and_recovery_revalidates_every_source() {
        let (subject, day, records) = fixture();
        let root = tempfile::tempdir().unwrap();
        let mut builder =
            StateTributeProofArchiveBuilder::create(root.path(), subject, day.clone()).unwrap();
        builder.push(&records[0]).unwrap();
        assert!(builder.finish().is_err());
        assert!(!root.path().join(STATE_HEADER).exists());
        let mut builder =
            StateTributeProofArchiveBuilder::create(root.path(), subject, day).unwrap();
        for proof in &records {
            builder.push(proof).unwrap();
        }
        drop(builder.finish().unwrap());
        let mut changed = records[0].clone();
        changed.tribute.encrypted_amounts[40] ^= 1;
        fs::write(
            record_path(root.path(), changed.tribute.context.tribute_id),
            encode(&changed).unwrap(),
        )
        .unwrap();
        assert!(StateTributeProofArchive::open(root.path(), subject, &|| {}).is_err());
    }
}
