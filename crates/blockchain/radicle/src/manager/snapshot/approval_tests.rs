use super::*;
use alloy_eips::{BlockNumHash, BlockNumberOrTag};
use outbe_primitives::{
    addresses::RADICLE_REGISTRY_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    storage::hashmap::HashMapStorageProvider,
};
use outbe_radicleregistry::{
    payload::PatchVotePayloadV1, vote_target::RadiclePatchVoteTarget, RepoId,
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
    fn latest(&self) -> ProviderResult<StateProviderBox> {
        panic!("must read exact hash, not latest")
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
    use outbe_primitives::storage::types::StorageKey as _;
    use outbe_radicleregistry::schema::{
        PublicationReceiptRecord, PublicationReceiptRecordEntryExt,
    };
    let (mut body, manifest) = crate::integration::publication::tests::bootstrap_fixture();
    body.package_manifest_hash = manifest.hash().unwrap();
    let hash = body.receipt_hash();
    let mut storage = HashMapStorageProvider::new_with_chain_identity(2026, B256::repeat_byte(7));
    storage.enter(|s| {
        let registry = RadicleRegistry::new(s);
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
        RadicleRegistry::new(s)
            .publication_receipt_hash
            .write(B256::ZERO)
            .unwrap()
    });
    let bad_hash = snapshot(&storage);
    storage.enter(|s| {
        let registry = RadicleRegistry::new(s);
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
        let registry = RadicleRegistry::new(s.clone());
        registry
            .publication_receipts
            .entry(1)
            .accepted_height()
            .write(1)
            .unwrap();
        let slot = 1u64.mapping_slot(registry.publication_receipts.base_slot());
        s.sstore(RADICLE_REGISTRY_ADDRESS, slot, U256::MAX).unwrap();
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

fn repository_authority_fixture() -> (
    RethSnapshotReader<ApprovalProvider>,
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
        for member in &committee.committee {
            validators
                .test_register_validator_without_pop(member.address, &member.consensus_pubkey)
                .unwrap();
            validators
                .activate_validator_via_boundary_for_test(member.address)
                .unwrap();
        }
        validators.advance_epoch(1001, 1).unwrap();
        validators.advance_epoch(1002, 2).unwrap();
        write_committee_snapshot(s, 2, &committee).unwrap();
    });
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

fn fixture() -> (RethSnapshotReader<ApprovalProvider>, String) {
    use outbe_vote::{
        handlers::{VoteTarget, VoteTargetRegistry},
        Vote,
    };
    static TARGET: RadiclePatchVoteTarget = RadiclePatchVoteTarget;
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
        let mut repos = RadicleRegistry::new(s.clone());
        repos.config_max_repositories.write(2).unwrap();
        repos
            .register_repository(RepoId::repeat_byte(0xaa), owner)
            .unwrap();
        let mut vote = Vote::new(s);
        let id = vote
            .create_proposal_with_voting_window(
                outbe_vote::ProposalSubmission::new(
                    voters[0],
                    RADICLE_REGISTRY_ADDRESS,
                    &payload,
                    1,
                ),
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
