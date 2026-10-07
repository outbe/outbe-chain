//! Independent Nod body stores and commitment namespaces yield identical full-block state, receipts and balances on both execution roles.

use super::*;
use outbe_primitives::projection::{ExecutionReadBudget, ExecutionReadCancelled};

#[test]
fn independent_body_stores_produce_identical_full_block_state_receipts_and_balances() {
    let proposer = test_evm_signer().address();
    let worldwide_day = WorldwideDay::new(20_241_220);
    let entry_price_minor = U256::from(450_000_000u64);
    let bucket_key = NodContract::bucket_key(worldwide_day, entry_price_minor, 840);
    let nod_item = || NodItemState {
        is_settled: false,
        nod_id: NodContract::generate_nod_id(proposer, worldwide_day).unwrap(),
        owner: proposer,
        gratis_load_minor: U256::from(1_000_000u64),
        worldwide_day,
        league_id: 1,
        bucket_key,
        issuance_currency: 840,
        reference_currency: 840,
        issued_at: 1,
    };
    let fixture = NodBodyFixture {
        proposer,
        worldwide_day,
        entry_price_minor,
        bucket_key,
        item: nod_item(),
    };
    let run = |expected_validator_body: bool,
               readers: RuntimeBodyReaders,
               budget: Option<ExecutionReadBudget>| {
        let signer = test_evm_signer();
        let (mut state, _tree_directory, tree_service, seed_hash) =
            seed_nod_body_state(&fixture).expect("seed Nod body fixture");
        let config = OutbeEvmConfig::new_with_runtime_body_readers(test_chain_spec(), readers)
            .with_evm_signer(signer)
            .with_compressed_tree_service(tree_service.clone());
        let mut parent_metadata = metadata_with(vec![proposer], vec![1], Vec::new());
        parent_metadata.finalized_block_number = 1;
        parent_metadata.finalized_block_hash = seed_hash;
        let system_txs = begin_system_txs_for_test(
            &config,
            BeginBlockFixture {
                block_number: 2,
                parent_hash: seed_hash,
                extra_data: &Bytes::new(),
                parent_consensus_metadata: Some(parent_metadata.clone()),
                proposer,
                bootstrap: BootstrapFixture::StandardForBlock,
            },
        );
        let visible_envelopes: Vec<u64> = system_txs.iter().map(|tx| tx.tx().gas_limit()).collect();
        let signed_body = system_txs.clone();
        let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
        let mut execution = execution_ctx(Some(1), Bytes::new());
        execution.inner.parent_hash = seed_hash;
        execution.parent_consensus_metadata = Some(parent_metadata);
        execution.parent_artifact_hint = Some(AccountedParentArtifact {
            summary: ExecutionSummaryArtifact {
                validator_fee_sum: U256::ZERO,
            },
            timestamp: 0,
            state_root: Some(B256::repeat_byte(0x91)),
        });
        execution.proposer_evm_address = Some(proposer);
        execution.execution_read_budget = budget;
        if expected_validator_body {
            execution.expected_begin_system_txs = system_txs.clone();
        }
        let mut executor = config.create_executor(evm, execution);
        super::with_phase1_verify_disabled(|| {
            executor
                .apply_pre_execution_changes()
                .expect("reader-backed pre-execution hook must succeed");
        });
        for tx in system_txs {
            let result = executor.execute_transaction(tx);
            if result.is_err() {
                assert!(
                    !executor.receipts().is_empty(),
                    "the cancelled body read must follow earlier block transactions"
                );
            }
            result?;
        }
        let receipts = executor.receipts().to_vec();
        assert_eq!(receipts.len(), signed_body.len());
        assert!(
            receipts
                .iter()
                .any(|receipt| receipt.logs.iter().any(|log| {
                    log.address == NOD_ADDRESS
                        && log.data.topics().first()
                            == Some(&INod::NodBucketBodyDeleted::SIGNATURE_HASH)
                })),
            "fixture must mutate a Nod bucket before testing CE cleanup"
        );
        let cleanup_hook_observation = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cleanup_hook_capture = cleanup_hook_observation.clone();
        executor
            .evm_mut()
            .db_mut()
            .set_state_hook(Some(Box::new(observe_ce_cleanup(cleanup_hook_capture))));
        // Match the production payload-builder ordering: finalize CE while
        // the parallel-root hook is attached, prove the zeroing diff was
        // observed, then detach the hook and freeze/finalize the root.
        executor
            .finalize_compressed_entities()
            .expect("pre-root compressed-entity cleanup must succeed");
        executor
            .prepare_final_header_artifacts(0)
            .expect("final extra_data should encode");
        let sealed = executor
            .compressed_entities_seal_output()
            .expect("block cleanup must produce a CE tree batch");
        let block_hash = B256::repeat_byte(0x42);
        let block_root = sealed.new_root;
        tree_service
            .publish_candidate(block_hash, sealed.staged_tree_batch)
            .expect("publish block CE candidate");
        tree_service
            .apply_finalized(2, block_hash, block_root)
            .expect("finalize block CE candidate");
        let cleanup_hook_cleared_slots =
            cleanup_hook_observation.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            cleanup_hook_cleared_slots > 0,
            "pre-root hook must expose at least one temporary CE slot changing to zero"
        );
        executor.evm_mut().db_mut().set_state_hook(None);
        let (evm, block_result) = executor.finish().expect("block finish must succeed");
        drop(evm);
        let bundle = state.bundle_state.clone();
        let root = post_state_root(&bundle);
        let proposer_balance = signer_balance(&mut state, proposer);
        let rewards_balance = signer_balance(&mut state, REWARDS_ADDRESS);

        // A new lifecycle can only open when every pending body/index record and
        // touched list from the finished block has been removed. This checks the
        // same committed bundle used for the state root above, not a mock store.
        assert_clean_ce_lifecycle(
            &mut state,
            &tree_service,
            (block_hash, block_root),
            proposer,
        )
        .expect("assert clean ce lifecycle fixture succeeds");
        Ok::<_, alloy_evm::block::BlockExecutionError>((
            root,
            bundle,
            receipts,
            proposer_balance,
            rewards_balance,
            block_result.gas_used,
            visible_envelopes,
            cleanup_hook_cleared_slots,
            signed_body,
        ))
    };

    let proposer_result = run(
        false,
        independent_nod_readers(&fixture).expect("independent nod readers fixture succeeds"),
        None,
    )
    .expect("proposer block execution succeeds");
    let validator_readers =
        independent_nod_readers(&fixture).expect("independent nod readers fixture succeeds");
    let cancelled_budget = ExecutionReadBudget::new();
    cancelled_budget.cancel();
    let aborted = run(
        true,
        validator_readers.clone(),
        Some(cancelled_budget.clone()),
    )
    .expect_err("cancelled CycleTick must abort the entire block");
    assert!(matches!(
        aborted,
        alloy_evm::block::BlockExecutionError::Internal(_)
    ));
    assert!(ExecutionReadCancelled::find(&aborted)
        .expect("block error must preserve the exact typed cancellation")
        .budget
        .same_request(&cancelled_budget));
    // Reopen the same parent and body backend with a fresh request. Compare the
    // entire signed body, receipts, root and balances; no transaction may be skipped.
    let validator_result = run(true, validator_readers, Some(ExecutionReadBudget::new()))
        .expect("fresh canonical replay after cancellation succeeds");
    assert_eq!(proposer_result, validator_result);
    assert_cycle_tick_receipt_gas(&proposer_result.2, proposer_result.5, &proposer_result.6);
}

fn assert_cycle_tick_receipt_gas(receipts: &[Receipt], gas_used: u64, envelopes: &[u64]) {
    let body_receipt_index = receipts
        .iter()
        .position(|receipt| {
            receipt.logs.iter().any(|log| {
                log.address == NOD_ADDRESS
                    && log.data.topics().first()
                        == Some(&INod::NodBucketBodyDeleted::SIGNATURE_HASH)
            })
        })
        .expect("CycleTick body mutation receipt");
    let previous_cumulative = body_receipt_index
        .checked_sub(1)
        .map_or(0, |index| receipts[index].cumulative_gas_used);
    let body_receipt_gas = receipts[body_receipt_index]
        .cumulative_gas_used
        .saturating_sub(previous_cumulative);
    let cycle_intrinsic_gas =
        system_tx_intrinsic_gas(SystemTxInputV2::CycleTick.encode().unwrap().as_ref()).unwrap();
    assert!(
        body_receipt_gas > cycle_intrinsic_gas,
        "receipt-visible CycleTick gas must add explicit CE work to intrinsic gas"
    );
    assert!(
        body_receipt_gas <= envelopes[body_receipt_index],
        "receipt-visible CycleTick gas must not exceed its signed gas limit"
    );
    assert_eq!(
        gas_used,
        receipts.last().unwrap().cumulative_gas_used,
        "header gas_used must equal the final receipt cumulative gas including CE work"
    );
}

#[test]
fn proposer_validator_body_mints_match_for_all_three_commitment_namespaces() {
    let _enclave = outbe_tribute::enclave_client::test_enclave::scope();
    let proposer = test_evm_signer().address();
    let day = WorldwideDay::new(20_260_716);
    let tribute_owner = Address::repeat_byte(0x31);
    let tribute_id =
        outbe_compressed_entities::derive_poseidon_entity_id(tribute_owner, day).unwrap();
    let nod_owner = Address::repeat_byte(0x32);
    let nod_id = outbe_compressed_entities::derive_poseidon_entity_id(nod_owner, day).unwrap();
    let bucket_key = NodContract::bucket_key(day, U256::from(16), 978);
    let ctx = BlockContext::new(1, 1, CHAIN_ID, proposer, vec![proposer]);

    let tribute_fixture = || TributeData {
        tribute_id,
        owner: tribute_owner,
        worldwide_day: day,
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
        exclude_from_intex_issuance: false,
    };
    let nod_fixture = || NodItemState {
        is_settled: false,
        nod_id,
        owner: nod_owner,
        gratis_load_minor: U256::from(1),
        worldwide_day: day,
        league_id: 2,
        bucket_key,
        issuance_currency: 840,
        reference_currency: 978,
        issued_at: 15,
    };

    let run = || {
        let bodies = Arc::new(MemoryStorage::new());
        let tribute_reader = TributeRepositoryReader::new(bodies.clone());
        let nod_reader = NodRepositoryReader::new(bodies);
        let scope = ExecutionScope::new();
        let mut state =
            state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |_| {});
        let (changes, events) =
            super::run_atomic_storage_hooks(&mut state, ctx.clone(), |hook_ctx| {
                outbe_compressed_entities::begin_block(hook_ctx.storage.clone(), &scope)?;
                let tribute = tribute_fixture();
                let mut tribute_contract = TributeContract::new(hook_ctx.storage.clone());
                tribute_contract.unseal_day(day)?;
                tribute_contract.issue(&scope, &tribute_reader, &tribute)?;
                outbe_nod::api::add_nod(
                    &hook_ctx.storage,
                    &scope,
                    &nod_reader,
                    &nod_fixture(),
                    U256::from(16),
                )?;
                outbe_compressed_entities::end_block(hook_ctx.storage.clone(), &scope).map(|_| ())
            })
            .expect("body mint execution must succeed");
        let compressed_root = {
            let mut provider = outbe_primitives::storage::direct::DirectStorageProvider::new(
                &mut state,
                ctx.clone(),
            );
            StorageHandle::enter(&mut provider, |storage| {
                storage
                    .sload(
                        outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                        U256::from(1),
                    )
                    .map(|root| B256::from(root.to_be_bytes::<32>()))
            })
            .unwrap()
        };
        let root = post_state_root(&state.bundle_state);
        let proposer_balance = signer_balance(&mut state, proposer);
        let rewards_balance = signer_balance(&mut state, REWARDS_ADDRESS);
        (
            changes,
            events,
            compressed_root,
            root,
            state.bundle_state,
            proposer_balance,
            rewards_balance,
        )
    };

    let proposer_result = run();
    let validator_result = run();
    assert_eq!(proposer_result, validator_result);
    assert!(proposer_result.1.iter().any(|event| {
        event.address == outbe_primitives::addresses::TRIBUTE_ADDRESS
            && event.data.topics()[0]
                == outbe_tribute::precompile::ITribute::TributeBodyStored::SIGNATURE_HASH
    }));
    assert!(proposer_result.1.iter().any(|event| {
        event.address == NOD_ADDRESS
            && event.data.topics()[0] == INod::NodBodyStored::SIGNATURE_HASH
    }));
    assert!(proposer_result.1.iter().any(|event| {
        event.address == NOD_ADDRESS
            && event.data.topics()[0] == INod::NodBucketBodyStored::SIGNATURE_HASH
    }));

    let bodies = Arc::new(MemoryStorage::new());
    let tribute_reader = TributeRepositoryReader::new(bodies.clone());
    let nod_reader = NodRepositoryReader::new(bodies);
    let scope = ExecutionScope::new();
    let mut failed_state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |_| {});
    let error = super::run_atomic_storage_hooks(&mut failed_state, ctx.clone(), |hook_ctx| {
        outbe_compressed_entities::begin_block(hook_ctx.storage.clone(), &scope)?;
        let tribute = tribute_fixture();
        let mut tribute_contract = TributeContract::new(hook_ctx.storage.clone());
        tribute_contract.unseal_day(day)?;
        tribute_contract.issue(&scope, &tribute_reader, &tribute)?;
        outbe_nod::api::add_nod(
            &hook_ctx.storage,
            &scope,
            &nod_reader,
            &nod_fixture(),
            U256::from(16),
        )?;
        Err(outbe_primitives::error::PrecompileError::Fatal(
            "later transaction stage failed".into(),
        ))
    })
    .expect_err("failed transaction must roll back every body namespace");
    assert!(error.to_string().contains("later transaction stage failed"));
    let mut read_provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut failed_state, ctx);
    StorageHandle::enter(&mut read_provider, |storage| {
        assert_eq!(
            B256::from(
                storage
                    .sload(
                        outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                        U256::from(1),
                    )?
                    .to_be_bytes::<32>(),
            ),
            outbe_compressed_entities::sealed_root(B256::ZERO).unwrap()
        );
        assert_eq!(TributeContract::new(storage.clone()).total_supply()?, 0);
        assert_eq!(NodContract::new(storage).total_supply()?, 0);
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .unwrap();
}

struct NodBodyFixture {
    proposer: Address,
    worldwide_day: WorldwideDay,
    entry_price_minor: U256,
    bucket_key: B256,
    item: NodItemState,
}
type SeededNodParent = (
    State<CacheDB<EmptyDBTyped<ProviderError>>>,
    tempfile::TempDir,
    Arc<CompressedTreeService>,
    B256,
);
fn seed_nod_body_state(fixture: &NodBodyFixture) -> eyre::Result<SeededNodParent> {
    let proposer = fixture.proposer;
    let (directory, tree_service) = persistent_test_tree(B256::ZERO);
    let empty_root = outbe_compressed_entities::sealed_root(B256::ZERO)?;
    let parent_tree = tree_service.open_parent(ExactParentIdentity {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        block_number: 0,
        block_hash: B256::ZERO,
        root: empty_root,
    })?;
    let scope = ExecutionScope::with_parent_tree(parent_tree, CeWorkConfig::new(0, 0, u64::MAX));
    let mut staged = None;
    let mut seeded = Ok(());
    let state = state_with_active_validators_seeded_at_block(
        &[(proposer, dummy_pubkey(0xA2))],
        1,
        |storage| {
            seeded = (|| -> eyre::Result<()> {
                seed_called_nod(storage.clone(), &scope, empty_root, fixture)?;
                staged =
                    Some(outbe_compressed_entities::end_block(storage, &scope)?.staged_tree_batch);

                Ok(())
            })();
        },
    );
    seeded?;
    let staged = staged.ok_or_else(|| eyre::eyre!("seed lifecycle must produce a tree batch"))?;
    let seed_hash = B256::repeat_byte(0x41);
    let seed_root = staged.new_root();
    tree_service.publish_candidate(seed_hash, staged)?;
    tree_service.apply_finalized(1, seed_hash, seed_root)?;
    Ok((state, directory, tree_service, seed_hash))
}

fn seed_called_nod(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    empty_root: B256,
    fixture: &NodBodyFixture,
) -> eyre::Result<()> {
    let bucket_key = fixture.bucket_key;
    storage.sstore(
        outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
        U256::ZERO,
        U256::from(2_u64),
    )?;
    storage.sstore(
        outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
        U256::from(1_u64),
        U256::from_be_bytes(empty_root.0),
    )?;
    outbe_compressed_entities::begin_block(storage.clone(), scope)?;
    let empty_reader = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    outbe_nod::api::add_nod(
        &storage,
        scope,
        &empty_reader,
        &fixture.item,
        fixture.entry_price_minor,
    )?;
    // The daily Nod trigger forfeits a lapsed called bucket, deleting both bodies.
    let nod = NodContract::new(storage.clone());
    nod.bucket_called_at.write(&bucket_key, 1)?;
    nod.called_bucket_index.write(&bucket_key, 0)?;
    nod.called_buckets.push(bucket_key)?;
    seed_nod_daily_trigger(storage, fixture)?;

    Ok(())
}

fn seed_nod_daily_trigger(
    storage: StorageHandle<'_>,
    _fixture: &NodBodyFixture,
) -> eyre::Result<()> {
    let (.., pair_index) = outbe_oracle::api::require_coen_pair(storage.clone(), 840)?;
    let previous_day = outbe_primitives::time::previous_date_key(
        outbe_primitives::time::timestamp_to_date_key(TEST_BLOCK_TIMESTAMP_BASE + 2),
    );
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    oracle.record_utc_day_vwap(previous_day, pair_index, U256::from(1_000_000u64))?;
    oracle.utc_day_vwap_last_finalized.write(previous_day)?;
    outbe_cycle::schema::Cycle::new(storage.clone())
        .last_executed_at
        .write(
            &outbe_cycle::triggers::TriggerId::NodCallDaily.as_u32(),
            TEST_BLOCK_TIMESTAMP_BASE - 86_400,
        )?;

    Ok(())
}

fn independent_nod_readers(fixture: &NodBodyFixture) -> eyre::Result<RuntimeBodyReaders> {
    let NodBodyFixture {
        worldwide_day,
        entry_price_minor,
        bucket_key,
        ..
    } = *fixture;
    let adapter = Arc::new(MemoryStorage::new());
    let reader: StorageReaderHandle = adapter.clone();
    let writer: StorageWriterHandle = adapter;
    let repository = NodRepositoryWriter::new(reader.clone(), writer);
    repository.put_nod(&fixture.item)?;
    repository.put_bucket(&NodBucketState {
        settled_nods: 0,
        bucket_key,
        worldwide_day,
        entry_price_minor,
        reference_currency: 840,
    })?;
    let readers = RuntimeBodyReaders::new(reader);
    assert!(readers
        .nod()
        .get_bucket(outbe_compressed_entities::WwdEntityId::from_day_and_digest(
            worldwide_day,
            bucket_key.0,
        ))
        .expect("independent bucket read")
        .is_some());
    Ok(readers)
}

fn observe_ce_cleanup(
    cleanup_hook_capture: Arc<std::sync::atomic::AtomicUsize>,
) -> impl Fn(revm::state::EvmState) + Send + Sync {
    move |changes: revm::state::EvmState| {
        let Some(compressed_entities) =
            changes.get(&outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS)
        else {
            return;
        };
        let cleared_slots = compressed_entities
            .storage
            .values()
            .filter(|slot| {
                slot.is_changed() && !slot.original_value.is_zero() && slot.present_value.is_zero()
            })
            .count();
        if cleared_slots > 0 {
            cleanup_hook_capture.store(cleared_slots, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

fn assert_clean_ce_lifecycle(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    tree_service: &CompressedTreeService,
    block: (B256, B256),
    proposer: Address,
) -> eyre::Result<()> {
    let (block_hash, block_root) = block;
    let clean_parent = tree_service.open_parent(ExactParentIdentity {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        block_number: 2,
        block_hash,
        root: block_root,
    })?;
    let clean_scope =
        ExecutionScope::with_parent_tree(clean_parent, CeWorkConfig::new(0, 0, u64::MAX));
    let clean_ctx = BlockContext::new(3, 2, CHAIN_ID, proposer, vec![proposer]);
    super::run_atomic_storage_hooks(state, clean_ctx, |hook_ctx| {
        outbe_compressed_entities::begin_block(hook_ctx.storage.clone(), &clean_scope)?;
        outbe_compressed_entities::end_block(hook_ctx.storage.clone(), &clean_scope).map(|_| ())
    })?;

    Ok(())
}
