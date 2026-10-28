use super::*;
use alloy_eips::{BlockNumHash, BlockNumberOrTag};
use outbe_coderegistry::{payload::PatchVotePayloadV1, vote_target::CodePatchVoteTarget, RepoId};
use outbe_primitives::{
    addresses::CODE_REGISTRY_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    storage::hashmap::HashMapStorageProvider,
};
use reth_chainspec::ChainInfo;
use reth_provider::{
    test_utils::{ExtendedAccount, MockEthProvider},
    BlockHashReader, BlockNumReader, ProviderError, ProviderResult, StateProviderBox,
};
use std::{collections::BTreeMap, sync::Mutex};

/// Distinct immutable states per hash; a latest/pending fallback fails the test.
struct ApprovalProvider {
    states: BTreeMap<B256, MockEthProvider>,
    canonical: BTreeMap<u64, B256>,
    finalized: Option<BlockNumHash>,
    reads: Mutex<Vec<B256>>,
}

impl BlockHashReader for ApprovalProvider {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        Ok(self.canonical.get(&number).copied())
    }
    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        Ok(self
            .canonical
            .range(start..end)
            .map(|(_, hash)| *hash)
            .collect())
    }
}
impl reth_provider::ReceiptProvider for ApprovalProvider {
    type Receipt = <MockEthProvider as reth_provider::ReceiptProvider>::Receipt;
    fn receipt(&self, _: u64) -> ProviderResult<Option<Self::Receipt>> {
        panic!("wrong receipt API")
    }
    fn receipt_by_hash(&self, _: B256) -> ProviderResult<Option<Self::Receipt>> {
        panic!("wrong receipt API")
    }
    fn receipts_by_block(
        &self,
        block: alloy_eips::BlockHashOrNumber,
    ) -> ProviderResult<Option<Vec<Self::Receipt>>> {
        let alloy_eips::BlockHashOrNumber::Hash(hash) = block else {
            panic!("receipts must use exact hash")
        };
        let number = self
            .canonical
            .iter()
            .find_map(|(n, h)| (*h == hash).then_some(*n))
            .unwrap();
        self.states
            .get(&hash)
            .unwrap()
            .receipts_by_block(number.into())
    }
    fn receipts_by_tx_range(
        &self,
        _: impl std::ops::RangeBounds<u64>,
    ) -> ProviderResult<Vec<Self::Receipt>> {
        panic!("wrong receipt API")
    }
    fn receipts_by_block_range(
        &self,
        _: std::ops::RangeInclusive<u64>,
    ) -> ProviderResult<Vec<Vec<Self::Receipt>>> {
        panic!("wrong receipt API")
    }
}
impl BlockNumReader for ApprovalProvider {
    fn chain_info(&self) -> ProviderResult<ChainInfo> {
        let (&best_number, &best_hash) = self.canonical.last_key_value().unwrap();
        Ok(ChainInfo {
            best_hash,
            best_number,
        })
    }
    fn best_block_number(&self) -> ProviderResult<u64> {
        Ok(self.chain_info()?.best_number)
    }
    fn last_block_number(&self) -> ProviderResult<u64> {
        self.best_block_number()
    }
    fn block_number(&self, hash: B256) -> ProviderResult<Option<u64>> {
        Ok(self
            .canonical
            .iter()
            .find_map(|(n, h)| (*h == hash).then_some(*n)))
    }
}
impl BlockIdReader for ApprovalProvider {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(None)
    }
    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(None)
    }
    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(self.finalized)
    }
}
impl StateProviderFactory for ApprovalProvider {
    type Primitives = <MockEthProvider as StateProviderFactory>::Primitives;

    fn latest(&self) -> ProviderResult<StateProviderBox> {
        panic!("must read exact hash, not latest")
    }
    fn state_with_block_appended(
        &self,
        _: B256,
        _: reth_chain_state::ExecutedBlock<Self::Primitives>,
    ) -> ProviderResult<StateProviderBox> {
        panic!("must read exact hash, not appended block")
    }
    fn state_by_block_number_or_tag(
        &self,
        _: BlockNumberOrTag,
    ) -> ProviderResult<StateProviderBox> {
        panic!("must read exact hash, not a number/tag")
    }
    fn history_by_block_number(&self, _: u64) -> ProviderResult<StateProviderBox> {
        panic!("must read exact hash")
    }
    fn history_by_block_hash(&self, _: B256) -> ProviderResult<StateProviderBox> {
        panic!("wrong state API")
    }
    fn state_by_block_hash(&self, hash: B256) -> ProviderResult<StateProviderBox> {
        self.reads.lock().unwrap().push(hash);
        self.states
            .get(&hash)
            .ok_or(ProviderError::BestBlockNotFound)?
            .latest()
    }
    fn pending(&self) -> ProviderResult<StateProviderBox> {
        panic!("must not read pending state")
    }
    fn pending_state_by_hash(&self, _: B256) -> ProviderResult<Option<StateProviderBox>> {
        panic!("must not read pending state")
    }
    fn maybe_pending(&self) -> ProviderResult<Option<StateProviderBox>> {
        panic!("must not read pending state")
    }
}

fn snapshot(provider: &HashMapStorageProvider) -> MockEthProvider {
    let result = MockEthProvider::new();
    let mut accounts: BTreeMap<Address, Vec<(B256, U256)>> = BTreeMap::new();
    for ((address, slot), value) in &provider.storage {
        accounts
            .entry(*address)
            .or_default()
            .push((B256::from(slot.to_be_bytes::<32>()), *value));
    }
    for (address, storage) in accounts {
        result.add_account(
            address,
            ExtendedAccount::new(0, U256::ZERO).extend_storage(storage),
        );
    }
    result
}

fn publication_receipt_fixture() -> RethSnapshotReader<ApprovalProvider> {
    use outbe_coderegistry::schema::{PublicationReceiptRecord, PublicationReceiptRecordEntryExt};
    use outbe_primitives::storage::types::StorageKey as _;
    let (mut body, manifest) = crate::integration::publication::tests::bootstrap_fixture();
    body.package_manifest_hash = manifest.hash().unwrap();
    let hash = body.receipt_hash();
    let mut storage = HashMapStorageProvider::new_with_chain_identity(2026, B256::repeat_byte(7));
    storage.enter(|s| {
        let registry = CodeRegistry::new(s);
        registry.publication_sequence.write(1).unwrap();
        registry.publication_receipt_hash.write(hash).unwrap();
        registry
            .publication_receipts
            .create(&PublicationReceiptRecord {
                sequence: 1,
                body: body.to_bytes().to_vec(),
                accepted_height: 1,
                receipt_hash: hash,
            })
            .unwrap();
    });
    let accepted = snapshot(&storage);
    storage.enter(|s| {
        CodeRegistry::new(s)
            .publication_receipt_hash
            .write(B256::ZERO)
            .unwrap()
    });
    let bad_hash = snapshot(&storage);
    storage.enter(|s| {
        let registry = CodeRegistry::new(s);
        registry.publication_receipt_hash.write(hash).unwrap();
        registry
            .publication_receipts
            .entry(1)
            .accepted_height()
            .write(6)
            .unwrap();
    });
    let future = snapshot(&storage);
    storage.enter(|s| {
        let registry = CodeRegistry::new(s.clone());
        registry
            .publication_receipts
            .entry(1)
            .accepted_height()
            .write(1)
            .unwrap();
        let slot = 1u64.mapping_slot(registry.publication_receipts.base_slot());
        s.sstore(CODE_REGISTRY_ADDRESS, slot, U256::MAX).unwrap();
    });
    let bad_length = snapshot(&storage);
    RethSnapshotReader::new(
        ApprovalProvider {
            states: [
                (block(3).hash, accepted),
                (block(4).hash, bad_hash),
                (block(5).hash, future),
                (block(6).hash, bad_length),
            ]
            .into(),
            canonical: (3..=6).map(|n| (n, block(n).hash)).collect(),
            finalized: Some(BlockNumHash::new(3, block(3).hash)),
            reads: Mutex::new(vec![]),
        },
        2026,
        B256::repeat_byte(7),
    )
}

#[test]
fn publication_receipt_reader_pins_history_and_rejects_corrupt_or_future_records() {
    let mut reader = publication_receipt_fixture();
    let receipt = reader
        .read_publication_receipt(block(3), 1)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.block(), block(3));
    assert_eq!(receipt.receipt().body.sequence, 1);
    assert!(reader
        .read_publication_receipt(block(3), 0)
        .unwrap()
        .is_none());
    assert!(reader
        .read_publication_receipt(block(3), 2)
        .unwrap()
        .is_none());
    assert_eq!(*reader.provider.reads.lock().unwrap(), [block(3).hash; 3]);
    reader.provider.finalized = Some(BlockNumHash::new(6, block(6).hash));
    for n in 4..=6 {
        assert!(matches!(
            reader.read_publication_receipt(block(n), 1),
            Err(ManagerError::Snapshot(_))
        ));
    }
    assert_eq!(
        reader
            .read_publication_receipt(block(3), 1)
            .unwrap()
            .unwrap(),
        receipt
    );
}

#[test]
fn publication_receipt_reader_rejects_unfinalized_noncanonical_missing_history_without_fallback() {
    let mut reader = publication_receipt_fixture();
    assert!(matches!(
        reader.read_publication_receipt(block(4), 1),
        Err(ManagerError::Finality(_))
    ));
    assert!(reader.provider.reads.lock().unwrap().is_empty());
    reader.provider.finalized = None;
    assert!(matches!(
        reader.read_publication_receipt(block(3), 1),
        Err(ManagerError::Finality(_))
    ));
    reader.provider.finalized = Some(BlockNumHash::new(3, B256::repeat_byte(99)));
    assert!(matches!(
        reader.read_publication_receipt(block(3), 1),
        Err(ManagerError::Finality(_))
    ));
    reader.provider.finalized = Some(BlockNumHash::new(3, block(3).hash));
    reader.provider.states.remove(&block(3).hash);
    assert!(matches!(
        reader.read_publication_receipt(block(3), 1),
        Err(ManagerError::Provider(_))
    ));
    assert_eq!(*reader.provider.reads.lock().unwrap(), [block(3).hash]);
}

#[test]
fn local_receipt_reader_samples_own_current_finality_without_archived_fallback() {
    use crate::manager::RepositoryReceiptReader as _;
    let mut reader = publication_receipt_fixture();
    let receipt = reader.read_current_publication_receipt(1).unwrap().unwrap();
    assert_eq!(receipt.block(), block(3));
    assert_eq!(*reader.provider.reads.lock().unwrap(), [block(3).hash]);
    reader.provider.finalized = Some(BlockNumHash::new(4, block(4).hash));
    assert!(matches!(
        reader.read_current_publication_receipt(1),
        Err(ManagerError::Snapshot(_))
    ));
    assert_eq!(
        *reader.provider.reads.lock().unwrap(),
        [block(3).hash, block(4).hash]
    );
    reader.provider.states.remove(&block(4).hash);
    assert!(matches!(
        reader.read_current_publication_receipt(1),
        Err(ManagerError::Provider(_))
    ));
    reader.provider.finalized = None;
    assert!(matches!(
        reader.read_current_publication_receipt(1),
        Err(ManagerError::Finality(_))
    ));
    assert_eq!(
        *reader.provider.reads.lock().unwrap(),
        [block(3).hash, block(4).hash, block(4).hash]
    );
}

fn repository_authority_storage() -> (
    HashMapStorageProvider,
    outbe_validatorset::state::CommitteeSnapshot,
) {
    use commonware_codec::Encode as _;
    use commonware_cryptography::Signer as _;
    use commonware_utils::TryCollect as _;
    use outbe_consensus::{
        bls::bootstrap_dkg_for_participants, dkg_manager::public_polynomial_hash,
    };
    use outbe_validatorset::state::{write_committee_snapshot, CommitteeEntry, CommitteeSnapshot};
    let mut keys: Vec<_> = (101..105).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let players = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let dkg = bootstrap_dkg_for_participants(players).unwrap();
    let committee = CommitteeSnapshot {
        committee: keys
            .iter()
            .enumerate()
            .map(|(i, key)| CommitteeEntry {
                address: Address::repeat_byte(i as u8 + 1),
                consensus_pubkey: key.public_key().encode().to_vec().try_into().unwrap(),
            })
            .collect(),
        vrf_material_version: 7,
        vrf_group_public_key_bytes: dkg.polynomial.public().encode().to_vec(),
        vrf_public_polynomial_hash: public_polynomial_hash(&dkg.polynomial),
    };
    let mut storage = HashMapStorageProvider::new_with_chain_identity(2026, B256::repeat_byte(7));
    storage.set_block_number(1);
    storage.enter(|s| {
        let mut validators = ValidatorSet::new(s.clone());
        validators
            .config_owner
            .write(Address::repeat_byte(99))
            .unwrap();
        validators.set_config_max_validators(4).unwrap();
        let minimum = U256::from(1_000u64);
        validators
            .test_seed_authoritative_min_stake(minimum)
            .unwrap();
        for member in &committee.committee {
            validators
                .test_register_validator_without_pop(member.address, &member.consensus_pubkey)
                .unwrap();
            validators
                .test_seed_authoritative_bonded(member.address, minimum)
                .unwrap();
            validators
                .activate_validator_via_boundary_for_test(member.address)
                .unwrap();
        }
        validators.advance_epoch(1001, 1).unwrap();
        validators.advance_epoch(1002, 2).unwrap();
        write_committee_snapshot(s, 2, &committee).unwrap();
    });
    (storage, committee)
}

fn repository_authority_fixture() -> (
    RethSnapshotReader<ApprovalProvider>,
    outbe_validatorset::state::CommitteeSnapshot,
) {
    use outbe_validatorset::state::write_committee_snapshot;
    let (mut storage, committee) = repository_authority_storage();
    let accepted = snapshot(&storage);
    let mut wrong_committee = committee.clone();
    wrong_committee.committee.pop();
    storage.enter(|s| write_committee_snapshot(s, 2, &wrong_committee).unwrap());
    let mismatched = snapshot(&storage);
    storage.enter(|s| ValidatorSet::new(s).advance_epoch(1003, 3).unwrap());
    let missing = snapshot(&storage);
    let provider = ApprovalProvider {
        states: [
            (block(3).hash, accepted),
            (block(4).hash, mismatched),
            (block(5).hash, missing),
        ]
        .into(),
        canonical: [(3, block(3).hash), (4, block(4).hash), (5, block(5).hash)].into(),
        finalized: Some(BlockNumHash::new(3, block(3).hash)),
        reads: Mutex::new(vec![]),
    };
    (
        RethSnapshotReader::new(provider, 2026, B256::repeat_byte(7)),
        committee,
    )
}

#[test]
fn repository_authority_uses_current_finalized_committee_and_rejects_effective_set_mismatch() {
    let (mut reader, committee) = repository_authority_fixture();
    let authority = reader.read_current_repository_authority().unwrap();
    assert_eq!(authority.block(), block(3));
    assert_eq!(authority.chain_id(), 2026);
    assert_eq!(authority.genesis_hash(), B256::repeat_byte(7));
    assert_eq!(authority.epoch(), 2);
    assert_eq!(authority.committee(), &committee);
    reader.provider.finalized = Some(BlockNumHash::new(4, block(4).hash));
    assert!(matches!(
        reader.read_current_repository_authority(),
        Err(ManagerError::Snapshot(_))
    ));
    reader.provider.finalized = Some(BlockNumHash::new(5, block(5).hash));
    assert!(matches!(
        reader.read_current_repository_authority(),
        Err(ManagerError::Snapshot(_))
    ));
    assert_eq!(
        *reader.provider.reads.lock().unwrap(),
        [block(3).hash, block(4).hash, block(5).hash]
    );
}

#[test]
fn repository_authority_rejects_missing_finality_noncanonical_and_unavailable_exact_state() {
    let (mut reader, _) = repository_authority_fixture();
    reader.provider.finalized = None;
    assert!(matches!(
        reader.read_current_repository_authority(),
        Err(ManagerError::Finality(_))
    ));
    reader.provider.finalized = Some(BlockNumHash::new(3, B256::repeat_byte(99)));
    assert!(matches!(
        reader.read_current_repository_authority(),
        Err(ManagerError::Finality(_))
    ));
    assert!(reader.provider.reads.lock().unwrap().is_empty());
    reader.provider.finalized = Some(BlockNumHash::new(3, block(3).hash));
    reader.provider.states.remove(&block(3).hash);
    assert!(matches!(
        reader.read_current_repository_authority(),
        Err(ManagerError::Provider(_))
    ));
    assert_eq!(*reader.provider.reads.lock().unwrap(), [block(3).hash]);
}

fn block(number: u64) -> FinalizedBlock {
    FinalizedBlock {
        number,
        hash: B256::repeat_byte(number as u8),
    }
}

fn signing_fixture() -> RethSnapshotReader<ApprovalProvider> {
    use outbe_coderegistry::schema::{
        PatchApprovalRecordEntryExt, PublicationReceiptRecord, PublicationReceiptRecordEntryExt,
    };
    use outbe_vote::{
        handlers::{VoteTarget, VoteTargetRegistry},
        schema::ProposalRecordEntryExt,
        ProposalStatus, ProposalSubmission, Vote,
    };
    static TARGET: CodePatchVoteTarget = CodePatchVoteTarget;
    static HANDLERS: &[&dyn VoteTarget] = &[&TARGET];
    let targets = VoteTargetRegistry::new(HANDLERS);
    let (mut storage, committee) = repository_authority_storage();
    let (mut body, _) = crate::integration::publication::tests::bootstrap_fixture();
    body.owner_bls_public =
        alloy_primitives::FixedBytes::<96>::from_slice(&committee.vrf_group_public_key_bytes);
    body.players_commitment = outbe_primitives::radicle_publication::players_commitment(
        committee
            .committee
            .iter()
            .map(|m| (m.address, &m.consensus_pubkey)),
    )
    .unwrap();
    body.package_manifest_hash = B256::repeat_byte(20);
    let payload = String::from_utf8(
        PatchVotePayloadV1::accept(body.repo_id, [6; 20].into(), [7; 20].into(), [8; 20].into())
            .encode()
            .unwrap(),
    )
    .unwrap();
    let record = PublicationReceiptRecord {
        sequence: 1,
        body: body.to_bytes().to_vec(),
        accepted_height: 2,
        receipt_hash: body.receipt_hash(),
    };
    storage.set_block_number(2);
    storage.enter(|s| {
        let mut registry = CodeRegistry::new(s.clone());
        registry.config_max_repositories.write(4).unwrap();
        registry
            .register_repository(body.repo_id, Address::repeat_byte(99))
            .unwrap();
        registry
            .config_descriptor_hash
            .write(body.descriptor_hash)
            .unwrap();
        registry
            .network_repository
            .write(outbe_coderegistry::repo_id_storage_key(body.repo_id))
            .unwrap();
        registry.publication_sequence.write(1).unwrap();
        registry.publication_material_sequence.write(1).unwrap();
        registry
            .publication_receipt_hash
            .write(record.receipt_hash)
            .unwrap();
        registry.publication_receipts.create(&record).unwrap();
        let mut vote = Vote::new(s);
        let id = vote
            .create_proposal_with_voting_window(
                ProposalSubmission::new(
                    committee.committee[0].address,
                    CODE_REGISTRY_ADDRESS,
                    &payload,
                    2,
                ),
                1,
                &targets,
            )
            .unwrap();
        assert_eq!(id, U256::ONE);
        for member in &committee.committee[..3] {
            vote.cast_vote_approve(id, member.address, true, 3).unwrap();
        }
    });
    let pending = snapshot(&storage);
    storage.set_block_number(4);
    storage.enter(|s| {
        let context =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(4, 1004, 2026), s.clone());
        Vote::new(s)
            .process_begin_block(&context, &targets)
            .unwrap();
    });
    let approved = snapshot(&storage);
    storage.enter(|s| {
        Vote::new(s)
            .proposals
            .entry(U256::ONE)
            .status()
            .write(ProposalStatus::Expired.to_u8())
            .unwrap()
    });
    let expired = snapshot(&storage);
    storage.enter(|s| {
        Vote::new(s.clone())
            .proposals
            .entry(U256::ONE)
            .status()
            .write(ProposalStatus::Approved.to_u8())
            .unwrap();
        CodeRegistry::new(s)
            .patch_execution_stage
            .write(
                &U256::ONE,
                outbe_coderegistry::schema::PATCH_EXECUTION_REVIEWED,
            )
            .unwrap();
    });
    let reviewed = snapshot(&storage);
    storage.enter(|s| {
        CodeRegistry::new(s)
            .patch_execution_stage
            .write(&U256::ONE, 99)
            .unwrap()
    });
    let unknown_stage = snapshot(&storage);
    storage.enter(|s| {
        let registry = CodeRegistry::new(s);
        registry.patch_execution_stage.write(&U256::ONE, 1).unwrap();
        registry.publication_material_sequence.write(0).unwrap();
    });
    let unbound = snapshot(&storage);
    storage.enter(|s| {
        let registry = CodeRegistry::new(s);
        registry.publication_material_sequence.write(1).unwrap();
        registry
            .config_descriptor_hash
            .write(B256::repeat_byte(99))
            .unwrap();
    });
    let wrong_descriptor = snapshot(&storage);
    storage.enter(|s| {
        let registry = CodeRegistry::new(s);
        registry
            .config_descriptor_hash
            .write(body.descriptor_hash)
            .unwrap();
        registry.publication_sequence.write(2).unwrap();
    });
    let missing_receipt = snapshot(&storage);
    storage.enter(|s| {
        let registry = CodeRegistry::new(s);
        registry.publication_sequence.write(1).unwrap();
        registry
            .patch_approvals
            .entry(U256::ONE)
            .approved_height()
            .write(12)
            .unwrap();
    });
    let future_approval = snapshot(&storage);
    storage.enter(|s| {
        let registry = CodeRegistry::new(s);
        registry
            .patch_approvals
            .entry(U256::ONE)
            .approved_height()
            .write(4)
            .unwrap();
        registry
            .publication_receipts
            .entry(1)
            .accepted_height()
            .write(13)
            .unwrap();
    });
    let future_receipt = snapshot(&storage);
    RethSnapshotReader::new(
        ApprovalProvider {
            states: [
                (block(3).hash, pending),
                (block(4).hash, approved),
                (block(5).hash, expired),
                (block(6).hash, reviewed),
                (block(7).hash, unknown_stage),
                (block(8).hash, unbound),
                (block(9).hash, wrong_descriptor),
                (block(10).hash, missing_receipt),
                (block(11).hash, future_approval),
                (block(12).hash, future_receipt),
            ]
            .into(),
            canonical: (2..=12).map(|n| (n, block(n).hash)).collect(),
            finalized: Some(BlockNumHash::new(4, block(4).hash)),
            reads: Mutex::new(vec![]),
        },
        2026,
        B256::repeat_byte(7),
    )
}

#[test]
fn current_patch_signing_state_uses_one_exact_finalized_state_and_preserves_consumed_permission() {
    let mut reader = signing_fixture();
    let current = reader.read_current_patch_signing_state(U256::ONE).unwrap();
    assert_eq!(current.authority().block(), block(4));
    assert_eq!(current.approval().block, block(4));
    assert_eq!(current.approval().approval.approved_height, 4);
    assert_eq!(
        current.stage(),
        outbe_coderegistry::schema::PATCH_EXECUTION_PENDING
    );
    assert_eq!(current.anchor(), block(4));
    assert_eq!(current.checkpoint().receipt().body.sequence, 1);
    assert_eq!(*reader.provider.reads.lock().unwrap(), [block(4).hash]);
    reader.provider.finalized = Some(BlockNumHash::new(6, block(6).hash));
    let reviewed = reader.read_current_patch_signing_state(U256::ONE).unwrap();
    assert_eq!(
        reviewed.stage(),
        outbe_coderegistry::schema::PATCH_EXECUTION_REVIEWED
    );
    assert_eq!(reviewed.approval().approval, current.approval().approval);
}

#[test]
fn current_patch_signing_state_rejects_pending_expired_bad_binding_and_future_records() {
    let mut reader = signing_fixture();
    for number in [3, 5, 7, 8, 9, 10, 11, 12] {
        reader.provider.finalized = Some(BlockNumHash::new(number, block(number).hash));
        assert!(
            matches!(
                reader.read_current_patch_signing_state(U256::ONE),
                Err(ManagerError::Snapshot(_))
            ),
            "height {number}"
        );
    }
    reader.provider.finalized = None;
    assert!(matches!(
        reader.read_current_patch_signing_state(U256::ONE),
        Err(ManagerError::Finality(_))
    ));
    reader.provider.finalized = Some(BlockNumHash::new(4, B256::repeat_byte(99)));
    assert!(matches!(
        reader.read_current_patch_signing_state(U256::ONE),
        Err(ManagerError::Finality(_))
    ));
    reader.provider.finalized = Some(BlockNumHash::new(4, block(4).hash));
    reader.provider.states.remove(&block(4).hash);
    assert!(matches!(
        reader.read_current_patch_signing_state(U256::ONE),
        Err(ManagerError::Provider(_))
    ));
}

fn fixture() -> (RethSnapshotReader<ApprovalProvider>, String) {
    use outbe_vote::{
        handlers::{VoteTarget, VoteTargetRegistry},
        Vote,
    };
    static TARGET: CodePatchVoteTarget = CodePatchVoteTarget;
    static HANDLERS: &[&dyn VoteTarget] = &[&TARGET];
    let registry = VoteTargetRegistry::new(HANDLERS);
    let payload = String::from_utf8(
        PatchVotePayloadV1::accept(
            RepoId::repeat_byte(0xaa),
            RepoId::repeat_byte(0xbb),
            RepoId::repeat_byte(0xcc),
            RepoId::repeat_byte(0xdd),
        )
        .encode()
        .unwrap(),
    )
    .unwrap();
    let owner = Address::repeat_byte(0xff);
    let voters = [
        Address::repeat_byte(1),
        Address::repeat_byte(2),
        Address::repeat_byte(3),
    ];
    let mut storage = HashMapStorageProvider::new(1);
    storage.set_block_number(1);
    storage.enter(|s| {
        let mut vs = ValidatorSet::new(s.clone());
        vs.config_owner.write(owner).unwrap();
        vs.set_config_max_validators(4).unwrap();
        for (i, voter) in voters.iter().enumerate() {
            vs.register_validator(owner, *voter, &[i as u8 + 1; 48])
                .unwrap();
            vs.activate_validator(*voter).unwrap();
        }
        let mut repos = CodeRegistry::new(s.clone());
        repos.config_max_repositories.write(2).unwrap();
        repos
            .register_repository(RepoId::repeat_byte(0xaa), owner)
            .unwrap();
        repos
            .network_repository
            .write(outbe_coderegistry::repo_id_storage_key(
                RepoId::repeat_byte(0xaa),
            ))
            .unwrap();
        let mut vote = Vote::new(s);
        let id = vote
            .create_proposal_with_voting_window(
                outbe_vote::ProposalSubmission::new(voters[0], CODE_REGISTRY_ADDRESS, &payload, 1),
                1,
                &registry,
            )
            .unwrap();
        assert_eq!(id, U256::ONE);
        for voter in &voters[..2] {
            vote.cast_vote_approve(id, *voter, true, 2).unwrap();
        }
    });
    let before = snapshot(&storage);
    storage.set_block_number(3);
    storage.enter(|s| {
        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(3, 1003, 1), s.clone());
        Vote::new(s).process_begin_block(&ctx, &registry).unwrap();
    });
    let accepted = snapshot(&storage);
    storage.set_block_number(4);
    storage.enter(|s| {
        ValidatorSet::new(s).jail_validator(voters[0]).unwrap();
    });
    let later = snapshot(&storage);
    let provider = ApprovalProvider {
        states: [
            (block(2).hash, before),
            (block(3).hash, accepted),
            (block(4).hash, later),
        ]
        .into(),
        canonical: [(2, block(2).hash), (3, block(3).hash), (4, block(4).hash)].into(),
        finalized: Some(BlockNumHash::new(3, block(3).hash)),
        reads: Mutex::new(vec![]),
    };
    (RethSnapshotReader::new(provider, 1, B256::ZERO), payload)
}

#[test]
fn exact_finalized_state_does_not_leak_latest_approval_or_revoke_accepted_history() {
    let (mut reader, payload) = fixture();
    assert!(reader
        .read_patch_approval(block(2), U256::ONE)
        .unwrap()
        .is_none());
    let accepted = reader
        .read_patch_approval(block(3), U256::ONE)
        .unwrap()
        .unwrap();
    assert_eq!(accepted.block, block(3));
    assert_eq!(accepted.approval.payload, payload);
    assert_eq!(accepted.approval.approved_height, 3);
    assert_eq!(accepted.approval.approved_timestamp, 1003);
    assert!(matches!(
        reader.read_patch_approval(block(4), U256::ONE),
        Err(ManagerError::Finality(_))
    ));
    reader.provider.finalized = Some(BlockNumHash::new(4, block(4).hash));
    let later = reader
        .read_patch_approval(block(4), U256::ONE)
        .unwrap()
        .unwrap();
    assert_eq!(
        later.approval, accepted.approval,
        "membership changes do not revoke accepted history"
    );
    assert_eq!(
        reader
            .read_patch_approval(block(3), U256::ONE)
            .unwrap()
            .unwrap(),
        accepted
    );
    assert_eq!(
        *reader.provider.reads.lock().unwrap(),
        [block(2).hash, block(3).hash, block(4).hash, block(3).hash]
    );
}

#[test]
fn finalized_vote_pages_are_bounded_canonical_and_tied_to_immutable_patch_approval() {
    use crate::manager::MAX_VOTE_PAGE;
    use alloy_sol_types::SolEvent as _;
    use outbe_vote::precompile::IVote;
    let (mut reader, _) = fixture();
    let event = |id| {
        IVote::ProposalApproved {
            proposalId: U256::from(id),
            state: IVote::VoteTally { yes: 3, no: 0 },
        }
        .encode_log_data()
    };
    let mut logs: Vec<_> = (1..=65)
        .rev()
        .map(|id| alloy_primitives::Log {
            address: outbe_primitives::addresses::VOTE_ADDRESS,
            data: event(id),
        })
        .collect();
    logs.push(logs.last().unwrap().clone());
    logs.push(alloy_primitives::Log {
        address: Address::repeat_byte(99),
        data: event(0),
    });
    let receipts = vec![alloy_consensus::EthereumReceipt {
        tx_type: alloy_consensus::TxType::Legacy,
        success: true,
        cumulative_gas_used: 1,
        logs,
    }];
    reader
        .provider
        .states
        .get(&block(3).hash)
        .unwrap()
        .add_receipts(3, receipts.clone());
    let first = reader.read_finalized_vote_page(3, 0).unwrap().unwrap();
    assert_eq!(first.block, block(3));
    assert_eq!(first.proposals, [U256::ONE]);
    assert_eq!(first.next_offset, Some(MAX_VOTE_PAGE as u32));
    let second = reader.read_finalized_vote_page(3, 32).unwrap().unwrap();
    assert!(second.proposals.is_empty());
    assert_eq!(second.next_offset, Some(64));
    let last = reader.read_finalized_vote_page(3, 64).unwrap().unwrap();
    assert!(last.proposals.is_empty());
    assert_eq!(last.next_offset, None);
    assert!(reader.read_finalized_vote_page(3, 66).is_err());
    assert!(reader.read_finalized_vote_page(4, 0).unwrap().is_none());
    assert!(
        reader.read_finalized_vote_page(2, 0).is_err(),
        "missing receipts cannot skip a block"
    );
    reader
        .provider
        .states
        .get(&block(2).hash)
        .unwrap()
        .add_receipts(2, receipts);
    assert!(
        reader.read_finalized_vote_page(2, 0).is_err(),
        "wrong approval height must reject"
    );
    reader.provider.finalized = Some(BlockNumHash::new(4, block(4).hash));
    assert_eq!(
        reader
            .read_finalized_vote_page(3, 0)
            .unwrap()
            .unwrap()
            .proposals,
        [U256::ONE]
    );
    let mut malformed = event(1);
    malformed.data = Default::default();
    reader
        .provider
        .states
        .get(&block(3).hash)
        .unwrap()
        .add_receipts(
            3,
            vec![alloy_consensus::EthereumReceipt {
                tx_type: alloy_consensus::TxType::Legacy,
                success: true,
                cumulative_gas_used: 1,
                logs: vec![alloy_primitives::Log {
                    address: outbe_primitives::addresses::VOTE_ADDRESS,
                    data: malformed,
                }],
            }],
        );
    assert!(
        reader.read_finalized_vote_page(3, 0).is_err(),
        "malformed Approved ABI must reject"
    );
    reader.provider.canonical.insert(4, B256::repeat_byte(99));
    assert!(matches!(
        reader.read_finalized_vote_page(3, 0),
        Err(ManagerError::Finality(_))
    ));
}

#[test]
fn archive_reader_uses_current_finality_without_revoking_an_accepted_vote() {
    use crate::manager::RepositoryReceiptReader as _;
    let (mut reader, _) = fixture();
    let accepted = reader
        .read_current_patch_approval(U256::ONE)
        .unwrap()
        .unwrap();
    assert_eq!(accepted.block, block(3));
    assert_eq!(*reader.provider.reads.lock().unwrap(), [block(3).hash]);
    reader.provider.finalized = Some(BlockNumHash::new(4, block(4).hash));
    let later = reader
        .read_current_patch_approval(U256::ONE)
        .unwrap()
        .unwrap();
    assert_eq!(later.block, block(4));
    assert_eq!(later.approval, accepted.approval);
    assert_eq!(
        *reader.provider.reads.lock().unwrap(),
        [block(3).hash, block(4).hash]
    );
    reader.provider.finalized = None;
    assert!(matches!(
        reader.read_current_patch_approval(U256::ONE),
        Err(ManagerError::Finality(_))
    ));
}

#[test]
fn finalized_vote_pages_select_only_the_network_repository() {
    use outbe_vote::{
        handlers::{VoteTarget, VoteTargetRegistry},
        Vote,
    };
    static TARGET: CodePatchVoteTarget = CodePatchVoteTarget;
    static HANDLERS: &[&dyn VoteTarget] = &[&TARGET];
    let handlers = VoteTargetRegistry::new(HANDLERS);
    let voters = [
        Address::repeat_byte(1),
        Address::repeat_byte(2),
        Address::repeat_byte(3),
    ];
    for selected in [None, Some(1u8), Some(2)] {
        let mut storage = HashMapStorageProvider::new(1);
        storage.set_block_number(1);
        storage.enter(|s| {
            let mut validators = ValidatorSet::new(s.clone());
            validators.config_owner.write(voters[0]).unwrap();
            validators.set_config_max_validators(4).unwrap();
            for (index, voter) in voters.iter().enumerate() {
                validators
                    .register_validator(voters[0], *voter, &[index as u8 + 1; 48])
                    .unwrap();
                validators.activate_validator(*voter).unwrap();
            }
            let mut registry = CodeRegistry::new(s.clone());
            registry.config_max_repositories.write(4).unwrap();
            if let Some(repo) = selected {
                registry
                    .network_repository
                    .write(outbe_coderegistry::repo_id_storage_key(
                        RepoId::repeat_byte(repo),
                    ))
                    .unwrap();
            }
            for repo in [1u8, 2] {
                registry
                    .register_repository(RepoId::repeat_byte(repo), voters[0])
                    .unwrap();
                let payload = String::from_utf8(
                    PatchVotePayloadV1::accept(
                        RepoId::repeat_byte(repo),
                        RepoId::repeat_byte(3),
                        RepoId::repeat_byte(4),
                        RepoId::repeat_byte(5),
                    )
                    .encode()
                    .unwrap(),
                )
                .unwrap();
                let mut vote = Vote::new(s.clone());
                let id = vote
                    .create_proposal_with_voting_window(
                        outbe_vote::ProposalSubmission::new(
                            voters[usize::from(repo) - 1],
                            CODE_REGISTRY_ADDRESS,
                            &payload,
                            1,
                        ),
                        1,
                        &handlers,
                    )
                    .unwrap();
                for voter in voters {
                    vote.cast_vote_approve(id, voter, true, 2).unwrap();
                }
            }
        });
        storage.set_block_number(3);
        storage.enter(|s| {
            let ctx =
                BlockRuntimeContext::new(BlockContext::empty_for_tests(3, 1003, 1), s.clone());
            Vote::new(s).process_begin_block(&ctx, &handlers).unwrap();
        });
        let state = snapshot(&storage);
        state.add_receipts(
            3,
            vec![alloy_consensus::EthereumReceipt {
                tx_type: alloy_consensus::TxType::Legacy,
                success: true,
                cumulative_gas_used: 1,
                logs: storage.get_ordered_events().to_vec(),
            }],
        );
        let reader = RethSnapshotReader::new(
            ApprovalProvider {
                states: [(block(3).hash, state)].into(),
                canonical: [(3, block(3).hash)].into(),
                finalized: Some(BlockNumHash::new(3, block(3).hash)),
                reads: Mutex::new(vec![]),
            },
            1,
            B256::ZERO,
        );
        let page = reader.read_finalized_vote_page(3, 0).unwrap().unwrap();
        assert_eq!(
            page.proposals,
            selected.into_iter().map(U256::from).collect::<Vec<_>>()
        );
    }
}

#[test]
fn missing_finality_noncanonical_and_unavailable_snapshots_fail_without_fallback() {
    let (mut reader, _) = fixture();
    let wrong = FinalizedBlock {
        number: 3,
        hash: B256::repeat_byte(99),
    };
    assert!(matches!(
        reader.read_patch_approval(wrong, U256::ONE),
        Err(ManagerError::Finality(_))
    ));
    reader.provider.finalized = None;
    assert!(matches!(
        reader.read_patch_approval(block(3), U256::ONE),
        Err(ManagerError::Finality(_))
    ));
    reader.provider.finalized = Some(BlockNumHash::new(3, B256::repeat_byte(99)));
    assert!(matches!(
        reader.read_patch_approval(block(3), U256::ONE),
        Err(ManagerError::Finality(_))
    ));
    assert!(reader.provider.reads.lock().unwrap().is_empty());
    reader.provider.finalized = Some(BlockNumHash::new(3, block(3).hash));
    reader.provider.states.remove(&block(3).hash);
    assert!(matches!(
        reader.read_patch_approval(block(3), U256::ONE),
        Err(ManagerError::Provider(_))
    ));
    assert_eq!(*reader.provider.reads.lock().unwrap(), [block(3).hash]);
}

#[test]
fn bootstrap_permission_uses_one_current_finalized_state_and_stops_after_binding() {
    let (mut storage, _) = repository_authority_storage();
    let seeded: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../system/coderegistry/tests/fixtures/governments-genesis-storage.json"
    ))
    .unwrap();
    for (slot, value) in seeded["storage"].as_object().unwrap() {
        storage.storage.insert(
            (CODE_REGISTRY_ADDRESS, slot.parse().unwrap()),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    let mut states = BTreeMap::from([(block(3).hash, snapshot(&storage))]);
    let slots = [
        (10u64, U256::ZERO),
        (10, U256::from(1)),
        (12, U256::from(1)),
        (13, U256::from(1)),
        (14, U256::from(1)),
        (15, U256::from(1)),
    ];
    for (index, (slot, value)) in slots.into_iter().enumerate() {
        let original = storage
            .storage
            .insert((CODE_REGISTRY_ADDRESS, U256::from(slot)), value);
        states.insert(block(index as u64 + 4).hash, snapshot(&storage));
        match original {
            Some(value) => {
                storage
                    .storage
                    .insert((CODE_REGISTRY_ADDRESS, U256::from(slot)), value);
            }
            None => {
                storage
                    .storage
                    .remove(&(CODE_REGISTRY_ADDRESS, U256::from(slot)));
            }
        }
    }
    let provider = ApprovalProvider {
        states,
        canonical: (3..10).map(|i| (i, block(i).hash)).collect(),
        finalized: Some(BlockNumHash::new(3, block(3).hash)),
        reads: Mutex::new(vec![]),
    };
    let mut reader = RethSnapshotReader::new(provider, 2026, B256::repeat_byte(7));
    let state = reader.read_current_bootstrap_signing_state().unwrap();
    assert_eq!(state.authority().block(), block(3));
    assert_eq!(state.authority().epoch(), 2);
    assert_eq!(state.descriptor().description, "Network governance");
    assert_eq!(
        reader.provider.reads.lock().unwrap().as_slice(),
        &[block(3).hash]
    );
    for i in 4..10 {
        reader.provider.finalized = Some(BlockNumHash::new(i, block(i).hash));
        assert!(
            reader.read_current_bootstrap_signing_state().is_err(),
            "state {i} accepted"
        );
    }
    reader.provider.finalized = None;
    assert!(reader.read_current_bootstrap_signing_state().is_err());
    assert_eq!(reader.provider.reads.lock().unwrap().len(), 7);
    reader.provider.finalized = Some(BlockNumHash::new(3, B256::repeat_byte(99)));
    assert!(reader.read_current_bootstrap_signing_state().is_err());
    assert_eq!(reader.provider.reads.lock().unwrap().len(), 7);
}

#[test]
fn publication_delivery_progress_uses_one_finalized_state_and_preserves_accepted_history() {
    use commonware_codec::Encode as _;
    use commonware_cryptography::Signer as _;
    use outbe_primitives::radicle_publication::{
        players_commitment, PublicationAttestation, PublicationBatch, PUBLICATION_NAMESPACE,
    };
    let (mut storage, committee) = repository_authority_storage();
    let seeded: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../system/coderegistry/tests/fixtures/governments-genesis-storage.json"
    ))
    .unwrap();
    for (slot, value) in seeded["storage"].as_object().unwrap() {
        storage.storage.insert(
            (CODE_REGISTRY_ADDRESS, slot.parse().unwrap()),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    let (mut body, manifest) = crate::integration::publication::tests::bootstrap_fixture();
    body.package_manifest_hash = manifest.hash().unwrap();
    body.descriptor_hash = storage
        .enter(|s| CodeRegistry::new(s).config_descriptor_hash.read())
        .unwrap();
    body.owner_bls_public =
        alloy_primitives::FixedBytes::from_slice(&committee.vrf_group_public_key_bytes);
    body.players_commitment = players_commitment(
        committee
            .committee
            .iter()
            .map(|m| (m.address, &m.consensus_pubkey)),
    )
    .unwrap();
    body.epoch = 2;
    body.material_version = committee.vrf_material_version;
    let mut keys: Vec<_> = (101..105).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|k| k.public_key().encode());
    let batch = |indices: &[usize]| PublicationBatch {
        body: body.clone(),
        attestations: indices
            .iter()
            .map(|&i| PublicationAttestation {
                participant_index: i as u32,
                signature: alloy_primitives::FixedBytes::from_slice(
                    keys[i]
                        .sign(PUBLICATION_NAMESPACE, &body.to_bytes())
                        .encode()
                        .as_ref(),
                ),
            })
            .collect(),
    };
    storage.set_block_number(3);
    assert!(storage
        .enter(|s| CodeRegistry::new(s).apply_publication_batch(&batch(&[0, 1])))
        .unwrap()
        .receipt
        .is_none());
    let pending = snapshot(&storage);
    storage.set_block_number(4);
    let accepted = storage
        .enter(|s| CodeRegistry::new(s).apply_publication_batch(&batch(&[2])))
        .unwrap()
        .receipt
        .unwrap();
    let complete = snapshot(&storage);
    storage
        .enter(|s| ValidatorSet::new(s).advance_epoch(1005, 5))
        .unwrap();
    let rotated = snapshot(&storage);
    let provider = ApprovalProvider {
        states: [
            (block(3).hash, pending),
            (block(4).hash, complete),
            (block(5).hash, rotated),
        ]
        .into(),
        canonical: (3..6).map(|i| (i, block(i).hash)).collect(),
        finalized: Some(BlockNumHash::new(3, block(3).hash)),
        reads: Mutex::new(vec![]),
    };
    let mut reader = RethSnapshotReader::new(provider, 2026, B256::repeat_byte(7));
    let progress = reader.read_current_publication_progress(&body).unwrap();
    assert_eq!(progress.block(), block(3));
    assert_eq!(progress.body_hash(), body.receipt_hash());
    assert_eq!(progress.attesters(), [0, 1]);
    assert!(progress.receipt().is_none());
    assert_eq!(*reader.provider.reads.lock().unwrap(), [block(3).hash]);
    let mut other = body.clone();
    other.package_manifest_hash = B256::repeat_byte(41);
    assert!(reader
        .read_current_publication_progress(&other)
        .unwrap()
        .attesters()
        .is_empty());
    for height in [4, 5] {
        reader.provider.finalized = Some(BlockNumHash::new(height, block(height).hash));
        let progress = reader.read_current_publication_progress(&body).unwrap();
        assert_eq!(progress.block(), block(height));
        assert_eq!(progress.receipt().unwrap().receipt(), &accepted);
        assert!(progress.attesters().is_empty());
        assert!(reader.read_current_publication_progress(&other).is_err());
    }
    let before = reader.provider.reads.lock().unwrap().len();
    let mut wrong = body.clone();
    wrong.genesis_hash = B256::repeat_byte(99);
    assert!(reader.read_current_publication_progress(&wrong).is_err());
    reader.provider.finalized = None;
    assert!(reader.read_current_publication_progress(&body).is_err());
    reader.provider.finalized = Some(BlockNumHash::new(5, B256::repeat_byte(99)));
    assert!(reader.read_current_publication_progress(&body).is_err());
    assert_eq!(reader.provider.reads.lock().unwrap().len(), before);
    reader.provider.finalized = Some(BlockNumHash::new(5, block(5).hash));
    reader.provider.states.remove(&block(5).hash);
    assert!(reader.read_current_publication_progress(&body).is_err());
}
