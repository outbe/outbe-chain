#[allow(dead_code)]
#[path = "radicle_approval/support.rs"]
mod support;
use support::*;

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
    use outbe_radicle::manager::RepositoryReceiptReader as _;
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

#[test]
fn current_patch_signing_state_uses_one_exact_finalized_state_and_preserves_consumed_permission() {
    let mut reader = signing_fixture();
    let current = reader.read_current_patch_signing_state(U256::ONE).unwrap();
    assert_eq!(current.authority().block(), block(4));
    assert_eq!(current.approval().block, block(4));
    assert_eq!(current.approval().approval.approved_height, 4);
    assert_eq!(current.stage(), cr::PATCH_EXECUTION_PENDING);
    assert_eq!(current.anchor(), block(4));
    assert_eq!(current.checkpoint().receipt().body.sequence, 1);
    assert_eq!(*reader.provider.reads.lock().unwrap(), [block(4).hash]);
    reader.provider.finalized = Some(BlockNumHash::new(6, block(6).hash));
    let reviewed = reader.read_current_patch_signing_state(U256::ONE).unwrap();
    assert_eq!(reviewed.stage(), cr::PATCH_EXECUTION_REVIEWED);
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
    use alloy_sol_types::SolEvent as _;
    use outbe_radicle::manager::MAX_VOTE_PAGE;
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
            address: outbe_system_common::addresses::VOTE_ADDRESS,
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
                    address: outbe_system_common::addresses::VOTE_ADDRESS,
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
    use outbe_radicle::manager::RepositoryReceiptReader as _;
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
                    .write(cr::repo_id_storage_key(RepoId::repeat_byte(repo)))
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
            protocol_state(),
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
