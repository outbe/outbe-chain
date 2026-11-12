//! Shared approval-reader fixtures. Each integration test binary uses a subset.
#![allow(unused_imports)]
pub use alloy_eips::{BlockNumHash, BlockNumberOrTag};
pub use alloy_primitives::{Address, B256, U256};
pub use commonware_cryptography::{bls12381, Signer as _};
pub use outbe_coderegistry::{vote_target::CodePatchVoteTarget, CodeRegistry};
pub use outbe_primitives::code_registry::{self as cr, PatchVotePayloadV1, RepoId};
pub use outbe_primitives::committee_snapshot as committee;
pub use outbe_primitives::{
    addresses::CODE_REGISTRY_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    storage::hashmap::HashMapStorageProvider,
};
pub use outbe_radicle::manager::{FinalizedBlock, ManagerError, RethSnapshotReader};
pub use outbe_radicle::protocol_state::RadicleProtocolState;
pub use outbe_runtime::OutbeRuntime;
pub use outbe_validatorset::contract::ValidatorSet;
pub use reth_chainspec::ChainInfo;
pub use reth_provider::{
    test_utils::{ExtendedAccount, MockEthProvider},
    BlockHashReader, BlockIdReader, BlockNumReader, ProviderError, ProviderResult, ReceiptProvider,
    StateProviderBox, StateProviderFactory,
};
pub use std::sync::Arc;
pub use std::{collections::BTreeMap, sync::Mutex};

pub use outbe_radicle::integration::publication::fixture::bootstrap_fixture;

pub fn protocol_state() -> Arc<dyn RadicleProtocolState> {
    Arc::new(OutbeRuntime::new())
}

/// Distinct immutable states per hash; a latest/pending fallback fails the test.
pub struct ApprovalProvider {
    pub states: BTreeMap<B256, MockEthProvider>,
    pub canonical: BTreeMap<u64, B256>,
    pub finalized: Option<BlockNumHash>,
    pub reads: Mutex<Vec<B256>>,
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

pub fn snapshot(provider: &HashMapStorageProvider) -> MockEthProvider {
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

pub fn publication_receipt_fixture() -> RethSnapshotReader<ApprovalProvider> {
    use cr::{PublicationReceiptRecord, PublicationReceiptRecordEntryExt};
    use outbe_primitives::storage::types::StorageKey as _;
    let (mut body, manifest) = bootstrap_fixture();
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
        protocol_state(),
    )
}

pub fn repository_authority_storage() -> (HashMapStorageProvider, committee::CommitteeSnapshot) {
    use commonware_codec::Encode as _;
    use commonware_cryptography::Signer as _;
    use commonware_utils::TryCollect as _;
    use outbe_consensus::{
        bls::bootstrap_dkg_for_participants, dkg_manager::public_polynomial_hash,
    };
    use outbe_validatorset::state::write_committee_snapshot;
    let mut keys: Vec<_> = (101..105).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let players = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let dkg = bootstrap_dkg_for_participants(players).unwrap();
    let committee = committee::CommitteeSnapshot {
        committee: keys
            .iter()
            .enumerate()
            .map(|(i, key)| committee::CommitteeEntry {
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

pub fn repository_authority_fixture() -> (
    RethSnapshotReader<ApprovalProvider>,
    committee::CommitteeSnapshot,
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
        RethSnapshotReader::new(provider, 2026, B256::repeat_byte(7), protocol_state()),
        committee,
    )
}

pub fn block(number: u64) -> FinalizedBlock {
    FinalizedBlock {
        number,
        hash: B256::repeat_byte(number as u8),
    }
}

pub fn signing_fixture() -> RethSnapshotReader<ApprovalProvider> {
    use cr::PatchApprovalRecordEntryExt;
    use cr::{PublicationReceiptRecord, PublicationReceiptRecordEntryExt};
    use outbe_vote::{
        handlers::{VoteTarget, VoteTargetRegistry},
        schema::ProposalRecordEntryExt,
        ProposalStatus, ProposalSubmission, Vote,
    };
    static TARGET: CodePatchVoteTarget = CodePatchVoteTarget;
    static HANDLERS: &[&dyn VoteTarget] = &[&TARGET];
    let targets = VoteTargetRegistry::new(HANDLERS);
    let (mut storage, committee) = repository_authority_storage();
    let (mut body, _) = bootstrap_fixture();
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
            .write(cr::repo_id_storage_key(body.repo_id))
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
        let context = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 4,
                timestamp: 1004,
                chain_id: 2026,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: 1004,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            s.clone(),
        );
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
            .write(&U256::ONE, cr::PATCH_EXECUTION_REVIEWED)
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
        protocol_state(),
    )
}

pub fn fixture() -> (RethSnapshotReader<ApprovalProvider>, String) {
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
            .write(cr::repo_id_storage_key(RepoId::repeat_byte(0xaa)))
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
        let ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 3,
                timestamp: 1003,
                chain_id: 1,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: 1003,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            s.clone(),
        );
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
    (
        RethSnapshotReader::new(provider, 1, B256::ZERO, protocol_state()),
        payload,
    )
}
