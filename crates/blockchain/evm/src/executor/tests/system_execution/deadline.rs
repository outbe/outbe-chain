use super::*;

const MINIMUM_STAKE: u64 = 1_000;
const OPEN_HEIGHT: u64 = 1;
const DEADLINE: u64 = OPEN_HEIGHT + outbe_validatorset::runtime::OCOMP_RECOVERY_WINDOW_BLOCKS;

struct DeadlineBlockInput<'a> {
    signer: Arc<OutbeEvmSigner>,
    committee: &'a [Address],
    number: u64,
    parent_hash: B256,
}

fn parent_metadata(
    committee: &[Address],
    number: u64,
    parent_hash: B256,
) -> CertifiedParentAccountingMetadata {
    let mut parent_metadata =
        metadata_with(committee.to_vec(), vec![1; committee.len()], Vec::new());
    parent_metadata.finalized_block_number = number - 1;
    parent_metadata.finalized_block_hash = parent_hash;

    parent_metadata
}

struct DeadlineBlockIdentity {
    number: u64,
    parent_hash: B256,
    proposer: Address,
}

fn deadline_context(
    system_txs: &[Recovered<TransactionSigned>],
    parent_metadata: CertifiedParentAccountingMetadata,
    identity: DeadlineBlockIdentity,
) -> OutbeBlockExecutionCtx<'static> {
    let DeadlineBlockIdentity {
        number,
        parent_hash,
        proposer,
    } = identity;
    let mut execution = execution_ctx(Some(system_txs.len()), Bytes::new());
    execution.inner.parent_hash = parent_hash;
    execution.parent_consensus_metadata = Some(parent_metadata);
    execution.parent_artifact_hint = Some(AccountedParentArtifact {
        summary: ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        },
        timestamp: TEST_BLOCK_TIMESTAMP_BASE.saturating_add(number - 1),
        state_root: Some(B256::repeat_byte(0x91)),
    });
    execution.proposer_evm_address = Some(proposer);
    execution.expected_begin_system_txs = system_txs.to_vec();

    execution
}

fn execute_and_finalize_block(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    tree: &Arc<CompressedTreeService>,
    input: DeadlineBlockInput<'_>,
) -> (B256, Vec<Receipt>) {
    let DeadlineBlockInput {
        signer,
        committee,
        number,
        parent_hash,
    } = input;
    let proposer = signer.address();
    let chain_spec = test_chain_spec();
    let config = OutbeEvmConfig::new(chain_spec)
        .with_evm_signer(signer)
        .with_compressed_tree_service(tree.clone());
    let parent_metadata = parent_metadata(committee, number, parent_hash);
    let system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: number,
            parent_hash,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: Some(parent_metadata.clone()),
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    );
    let evm = config.evm_with_env(state, test_evm_env(number, REWARDS_ADDRESS));
    let execution = deadline_context(
        &system_txs,
        parent_metadata,
        DeadlineBlockIdentity {
            number,
            parent_hash,
            proposer,
        },
    );
    let mut executor = config.create_executor(evm, execution);
    super::with_phase1_verify_disabled(|| {
        executor
            .apply_pre_execution_changes()
            .expect("deadline block pre-execution must succeed");
    });
    for transaction in system_txs {
        executor
            .execute_transaction(transaction)
            .expect("deadline block system transaction must execute");
    }
    executor
        .finalize_compressed_entities()
        .expect("deadline block CE lifecycle must seal");
    executor
        .prepare_final_header_artifacts(0)
        .expect("deadline block artifacts must encode");
    let sealed = executor
        .compressed_entities_seal_output()
        .expect("deadline block must produce a CE candidate");
    let block_hash = keccak256(number.to_be_bytes());
    tree.publish_candidate(block_hash, sealed.staged_tree_batch)
        .expect("deadline block CE candidate must publish");
    tree.apply_finalized(number, block_hash, sealed.new_root)
        .expect("deadline block CE candidate must finalize");
    let (evm, result) = executor.finish().expect("full block execution must finish");
    drop(evm);
    assert!(result.receipts.iter().all(|receipt| receipt.success));
    (block_hash, result.receipts)
}

fn seed_deadline_stake(storage: StorageHandle<'_>, committee: &[Address]) {
    let mut validator_set = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
    let mut staking = outbe_staking::contract::Staking::new(storage.clone());
    staking
        .config_min_stake
        .write(U256::from(MINIMUM_STAKE))
        .unwrap();
    staking
        .total_staked
        .write(U256::from(MINIMUM_STAKE * committee.len() as u64))
        .unwrap();
    storage
        .set_balance(
            STAKING_ADDRESS,
            U256::from(MINIMUM_STAKE * committee.len() as u64),
        )
        .unwrap();
    for validator in committee {
        staking
            .stake_amount
            .write(validator, U256::from(MINIMUM_STAKE))
            .unwrap();
        validator_set
            .test_set_stake_projection(
                *validator,
                outbe_validatorset::StakeProjection::new(U256::from(MINIMUM_STAKE), None),
            )
            .unwrap();
    }
    for validator in committee {
        let miss = staking.record_ocomp_miss(*validator).unwrap();
        assert!(miss.first_in_window);
        assert_eq!(miss.slashed_bonded, U256::from(100));
        assert_eq!(miss.recovery_deadline, DEADLINE);
    }
}

fn seed_deadline_bindings(storage: StorageHandle<'_>, committee: &[Address]) {
    let registry = outbe_teeregistry::TeeRegistry::new(storage.clone());
    for (index, validator) in committee.iter().enumerate() {
        let node_hash = keccak256((index as u64).to_be_bytes());
        registry
            .validator_v1_node_hash
            .write(validator, node_hash)
            .unwrap();
        registry
            .v1_node_enclave_id
            .write(&node_hash, B256::with_last_byte(0x11))
            .unwrap();
        registry
            .v1_node_binding_id
            .write(&node_hash, B256::with_last_byte(0x12))
            .unwrap();
        registry
            .v1_node_intent_hash
            .write(&node_hash, B256::with_last_byte(0x13))
            .unwrap();
        registry
            .v1_node_valid_until
            .write(
                &node_hash,
                TEST_BLOCK_TIMESTAMP_BASE.saturating_add(DEADLINE + 3_600),
            )
            .unwrap();
    }
}

fn prepare_deadline_state(
    validators: &[(Address, [u8; 48])],
    committee: &[Address],
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    let state = state_with_active_validators_seeded_at_block(validators, OPEN_HEIGHT, |storage| {
        seed_deadline_stake(storage.clone(), committee);
        seed_deadline_bindings(storage.clone(), committee);
        outbe_accounting::schema::Accounting::new(storage)
            .last_accounted_block_number
            .write(DEADLINE - 2)
            .unwrap();
    });

    state
}

fn assert_deadline_resolutions(deadline_receipts: &[Receipt], committee: &[Address]) {
    let resolutions = deadline_receipts
        .iter()
        .flat_map(|receipt| &receipt.logs)
        .filter_map(|log| {
            outbe_validatorset::precompile::IValidatorSet::OcompRecoveryResolved::decode_log(log)
                .ok()
        })
        .collect::<Vec<_>>();
    assert_eq!(resolutions.len(), committee.len());
    assert!(
        resolutions.iter().all(|resolution| {
            resolution.recoveryDeadline == DEADLINE && resolution.outcome == 2
        }),
        "unexpected OCOMP recovery resolutions: {resolutions:?}"
    );
}

fn assert_retained_committee(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    committee: &[Address],
) {
    let read_ctx = BlockContext::new(
        DEADLINE,
        TEST_BLOCK_TIMESTAMP_BASE + DEADLINE,
        CHAIN_ID,
        committee[0],
        committee.to_vec(),
    );
    let mut provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        let validator_set = outbe_validatorset::contract::ValidatorSet::new(storage);
        assert_eq!(
            validator_set
                .get_active_consensus_set()?
                .into_iter()
                .map(|record| record.validator_address)
                .collect::<Vec<_>>(),
            committee
        );
        assert!(validator_set.has_pending_set_change()?);
        for validator in committee {
            assert!(matches!(
                validator_set.validator_lifecycle(*validator)?,
                ValidatorLifecycle::JailRetained(_)
            ));
            assert!(validator_set.is_consensus_participant(*validator)?);
            assert!(validator_set.ocomp_recovery_window(*validator)?.is_none());
        }
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("deadline state must retain the current consensus committee");
}

pub(super) fn run() {
    let first_signer = Arc::new(OutbeEvmSigner::from_secret_bytes([0x21; 32]).unwrap());
    let second_signer = Arc::new(OutbeEvmSigner::from_secret_bytes([0x22; 32]).unwrap());
    let validators = vec![
        (first_signer.address(), dummy_pubkey(0x31)),
        (second_signer.address(), dummy_pubkey(0x32)),
        (numbered_test_address(0x33, 3), dummy_pubkey(0x33)),
        (numbered_test_address(0x34, 4), dummy_pubkey(0x34)),
    ];
    let committee = validators
        .iter()
        .map(|(validator, _)| *validator)
        .collect::<Vec<_>>();
    let mut state = prepare_deadline_state(&validators, &committee);
    let empty_root = outbe_compressed_entities::sealed_root(B256::ZERO).unwrap();
    let parent_hash = B256::repeat_byte(0x90);
    let (_tree_directory, tree) = persistent_test_tree_with_marker(
        test_chain_spec().genesis_hash(),
        FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: DEADLINE - 1,
            block_hash: parent_hash,
            parent_block_hash: B256::repeat_byte(0x8f),
            parent_root: empty_root,
            new_root: empty_root,
        },
    );

    let (deadline_hash, deadline_receipts) = execute_and_finalize_block(
        &mut state,
        &tree,
        DeadlineBlockInput {
            signer: first_signer,
            committee: &committee,
            number: DEADLINE,
            parent_hash,
        },
    );
    assert_deadline_resolutions(&deadline_receipts, &committee);
    assert_retained_committee(&mut state, &committee);
    let (_successor_hash, successor_receipts) = execute_and_finalize_block(
        &mut state,
        &tree,
        DeadlineBlockInput {
            signer: second_signer,
            committee: &committee,
            number: DEADLINE + 1,
            parent_hash: deadline_hash,
        },
    );
    assert!(!successor_receipts.is_empty());
    assert!(successor_receipts.iter().all(|receipt| receipt.success));
}
