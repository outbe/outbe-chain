use super::*;

/// an unverifiable late-finalize credit carried in
/// `header.extra_data` is FATAL in **pre-exec** - the block is rejected
/// before any transaction executes (no receipts), not as a soft receipt.
/// Phase 1 is disabled (no CPA proof seeded); the late-finalize preflight is
/// the sole gate under test, enabled via the dedicated
/// `LATE_FINALIZE_VERIFY_DISABLED` opt-out staying off.
#[test]
fn bad_late_proof_pre_exec_fatal() {
    use outbe_primitives::reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit};

    let signer = test_evm_signer();
    let proposer = signer.address();
    let parent_hash = B256::with_last_byte(0xAA);
    let mut state = state_with_active_proposer(proposer);

    // Block-2 header artifact: an in-window credit (distance 2 - 1 = 1)
    // whose committee snapshot was never written -> verify cannot resolve it.
    let artifact = OutbeBlockArtifacts {
        execution_summary: None,
        consensus_header_artifact: None,
        timestamp_millis_part: 0,
        late_finalize_credits: Some(LateFinalizeCreditsArtifact {
            batches: vec![PerBlockCredit {
                fb_number: 1,
                fb_hash: B256::repeat_byte(0xCD),
                epoch: 0,
                view: 9,
                parent_view: 8,
                committee_set_hash: B256::repeat_byte(0xEF),
                signer_bitmap: vec![0x01],
                aggregate_signature: [0u8; 96],
            }],
        }),
        compressed_entities_root: None,
    };
    let extra_data = encode_outbe_block_artifacts(&artifact).unwrap();

    let config =
        OutbeEvmConfig::new_with_bridge(test_chain_spec(), ConsensusExecutionBridge::new())
            .with_evm_signer(signer);
    let evm_env = test_evm_env(2, REWARDS_ADDRESS);
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut ctx = execution_ctx(Some(0), extra_data);
    ctx.inner.parent_hash = parent_hash;
    let mut executor = config.create_executor(evm, ctx);

    // Phase 1 disabled; late-finalize verify stays ENABLED -> the
    // unverifiable credit aborts the block in pre-exec.
    let err = super::with_phase1_verify_disabled(|| executor.apply_pre_execution_changes())
        .expect_err("unverifiable late-finalize credit must be FATAL in pre-exec");
    assert!(
        err.to_string().contains("LateFinalizeCredits pre-exec"),
        "error must come from the late-finalize preflight (fatal): {err}"
    );
    assert!(
        executor.receipts().is_empty(),
        "no receipts may be emitted before a pre-exec FATAL"
    );
}

#[test]
fn verifier_rejects_finalization_parent_hash_mismatch() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let evm_env = test_evm_env(2, REWARDS_ADDRESS);
    let config = OutbeEvmConfig::new(test_chain_spec());
    let evm = config.evm_with_env(&mut state, evm_env);

    let parent_hash = B256::with_last_byte(0xAA);
    let wrong_parent_hash = B256::with_last_byte(0xBB);
    let mut metadata = test_metadata();
    metadata.finalized_block_number = 1;
    metadata.finalized_block_hash = wrong_parent_hash;

    let phase1_unsigned = build_unsigned_system_tx(
        SystemTxKind::CertifiedParentAccounting,
        0,
        2,
        CHAIN_ID,
        SystemTxInputV2::CertifiedParentAccounting { metadata }
            .encode()
            .unwrap(),
    )
    .unwrap();
    let cycle_unsigned = build_unsigned_system_tx(
        SystemTxKind::CycleTick,
        1,
        2,
        CHAIN_ID,
        SystemTxInputV2::CycleTick.encode().unwrap(),
    )
    .unwrap();
    let phase1_signed = signer.sign_unsigned(phase1_unsigned).unwrap();
    let cycle_signed = signer.sign_unsigned(cycle_unsigned).unwrap();
    let phase1_recovered =
        reth_primitives_traits::Recovered::new_unchecked(phase1_signed, proposer);
    let cycle_recovered = reth_primitives_traits::Recovered::new_unchecked(cycle_signed, proposer);

    let mut ctx = execution_ctx(Some(2), Bytes::new());
    ctx.inner.parent_hash = parent_hash;
    ctx.expected_begin_system_txs = vec![phase1_recovered.clone(), cycle_recovered];
    ctx.proposer_evm_address = Some(proposer);

    let mut executor = config.create_executor(evm, ctx);
    // the rejection now fires in `apply_pre_execution_changes`
    // (Phase 1 verifier preflight) rather than during the main tx loop -
    // `verify_v2_proof` reads the same `parent_hash` mismatch via
    // `begin_block_system_tx_inputs` BEFORE any begin-zone state change.
    let err = executor.apply_pre_execution_changes().expect_err(
        "verifier must reject CertifiedParentAccounting metadata for a non-parent hash",
    );
    assert!(err
        .to_string()
        .contains("CertifiedParentAccounting metadata hash must match block parent"));
    assert!(executor.receipts().is_empty());
    let _ = phase1_recovered;
}

/// Regression for the consensus stall at block 14402: finalized-parent
/// metadata committee can legitimately differ from the live active set
/// after a DKG/reshare. As long as every committee member is a registered
/// validator (historical participant), validation must succeed.
#[test]
fn validate_finalized_metadata_accepts_registered_historical_committee() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        let val_b = address!("0x2222222222222222222222222222222222222222");
        let val_c = address!("0x3333333333333333333333333333333333333333");
        let val_d = address!("0x4444444444444444444444444444444444444444");

        for (addr, seed) in [
            (val_a, 0xA1u8),
            (val_b, 0xB2u8),
            (val_c, 0xC3u8),
            (val_d, 0xD4u8),
        ] {
            if addr == val_c {
                test_register_waiting(&mut vs, addr, &dummy_pubkey(seed));
            } else {
                test_register_active(&mut vs, addr, &dummy_pubkey(seed));
            }
        }
        // Live active set is [A, B, D]; C is registered but no longer a
        // current consensus participant after a reshare.
        let live_active = vs.get_active_consensus_set().unwrap();
        let live_addrs: Vec<Address> = live_active.iter().map(|v| v.validator_address).collect();
        assert!(!live_addrs.contains(&val_c), "C must not be live-active");

        // Finalized-parent metadata still describes the previous committee [A, B, C].
        let metadata = metadata_with(vec![val_a, val_b, val_c], vec![1, 1, 0], vec![]);
        super::validate_finalized_metadata(storage.clone(), &metadata).unwrap();
    });
}

#[test]
fn validate_finalized_metadata_rejects_duplicate_committee_member() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        let val_b = address!("0x2222222222222222222222222222222222222222");
        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));
        test_register_active(&mut vs, val_b, &dummy_pubkey(0xB2));

        let metadata = metadata_with(vec![val_a, val_b, val_a], vec![1, 1, 1], vec![]);
        let err = super::validate_finalized_metadata(storage.clone(), &metadata).unwrap_err();
        assert!(
            err.to_string().contains("duplicate"),
            "expected duplicate error, got {err}"
        );
    });
}

#[test]
fn validate_finalized_metadata_rejects_unregistered_committee_member() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));

        let stranger = address!("0x9999999999999999999999999999999999999999");
        let metadata = metadata_with(vec![val_a, stranger], vec![1, 1], vec![]);
        let err = super::validate_finalized_metadata(storage.clone(), &metadata).unwrap_err();
        assert!(
            err.to_string().contains("not a registered validator"),
            "expected unregistered error, got {err}"
        );
    });
}

#[test]
fn validate_finalized_metadata_rejects_missed_proposer_outside_committee() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        let val_b = address!("0x2222222222222222222222222222222222222222");
        let val_c = address!("0x3333333333333333333333333333333333333333");
        for (addr, seed) in [(val_a, 0xA1u8), (val_b, 0xB2u8), (val_c, 0xC3u8)] {
            test_register_active(&mut vs, addr, &dummy_pubkey(seed));
        }

        let metadata = metadata_with(vec![val_a, val_b], vec![1, 1], vec![val_c]);
        let err = super::validate_finalized_metadata(storage.clone(), &metadata).unwrap_err();
        assert!(
            err.to_string().contains("not in finalized committee"),
            "expected missed-proposer-outside-committee error, got {err}"
        );
    });
}

#[test]
fn validate_finalized_metadata_rejects_signer_bitmap_length_mismatch() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));

        let metadata = metadata_with(vec![val_a], vec![1, 0], vec![]);
        let err = super::validate_finalized_metadata(storage.clone(), &metadata).unwrap_err();
        assert!(
            err.to_string().contains("bitmap length mismatch"),
            "expected bitmap length error, got {err}"
        );
    });
}

// -----------------------------------------------------------------------
// Runtime: hint acceptance guard.
//
// `accounted_parent_artifact_for_metadata` is `pub(crate)`, so a runtime
// test must live in this module (integration tests in
// `crates/blockchain/evm/tests/artifact_lookup.rs` cannot reach it). These
// tests close the audit gap by exercising the guard branch directly
// instead of relying on source-grep substring matches.
//
// Construction is minimal: an `OutbeBlockExecutor` with
// `accounted_parent_artifact_provider = None` (forces the lookup ladder
// straight to the hint), a synthetic `parent_hash`, an explicit
// `parent_artifact_hint`, and a `BlockEnv.number` whose `n - 1` matches
// the metadata's `finalized_block_number` on the happy path.
// -----------------------------------------------------------------------

fn hint_test_metadata(
    finalized_block_number: u64,
    finalized_block_hash: B256,
) -> CertifiedParentAccountingMetadata {
    CertifiedParentAccountingMetadata {
        finalized_block_number,
        finalized_block_hash,
        ..Default::default()
    }
}

fn hint_test_artifact() -> AccountedParentArtifact {
    AccountedParentArtifact {
        summary: ExecutionSummaryArtifact {
            validator_fee_sum: U256::from(777u64),
        },
        timestamp: 1_700_900_000,
        state_root: None,
    }
}

struct HeaderNotFoundArtifactProvider;

impl AccountedParentArtifactProvider for HeaderNotFoundArtifactProvider {
    fn execution_summary_by_hash(
        &self,
        _block_number: u64,
        block_hash: B256,
    ) -> Result<Option<AccountedParentArtifact>, ProviderError> {
        Err(ProviderError::HeaderNotFound(block_hash.into()))
    }
}

/// Build the EVM env + EthBlockExecutionCtx pair for tests. The
/// caller drives the `OutbeBlockExecutor::new(...)` construction inline
/// because its return type references the opaque concrete `Evm` produced
/// by `OutbeEvmConfig::evm_with_env`.
fn hint_test_env(block_number: u64, parent_hash: B256) -> (EvmEnv, EthBlockExecutionCtx<'static>) {
    let env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
        block_env: BlockEnv {
            number: U256::from(block_number),
            gas_limit: 30_000_000,
            basefee: 1_000_000_000,
            beneficiary: REWARDS_ADDRESS,
            timestamp: U256::from(block_number),
            ..Default::default()
        },
    };
    let ctx = EthBlockExecutionCtx {
        parent_hash,
        parent_beacon_block_root: None,
        ommers: &[],
        withdrawals: None,
        extra_data: Bytes::new(),
        tx_count_hint: Some(0),
        slot_number: None,
    };
    (env, ctx)
}

/// (a): hint accepted when `(metadata.finalized_block_hash,
/// metadata.finalized_block_number)` matches `(self.parent_hash,
/// block_number - 1)`.
#[test]
fn hint_accepted_when_metadata_matches_parent() {
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone());
    let mut state = State::builder()
        .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
        .with_bundle_update()
        .build();

    let block_number = 42u64;
    let parent_hash = B256::repeat_byte(0xA0);
    let hint = hint_test_artifact();
    let (evm_env, inner_ctx) = hint_test_env(block_number, parent_hash);
    let evm = config.evm_with_env(&mut state, evm_env);
    let executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, inner_ctx, &chain_spec, &receipt_builder),
        None,
        Bytes::new(),
        None, // accounted_parent_artifact_provider - None forces hint path
        false,
        None,
        parent_hash,
        None,
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
        true,
        None,
        Some(hint),
    );

    let metadata = hint_test_metadata(block_number - 1, parent_hash);
    let resolved = executor
        .accounted_parent_artifact_for_metadata(&metadata)
        .expect("hint must be accepted when parent identity matches");

    assert_eq!(
        resolved, hint,
        "executor must return the cached hint verbatim"
    );
}

/// FCU-Valid race-window: even if the provider leaks
/// `HeaderNotFound` instead of normalizing it to `Ok(None)`, the executor
/// must still reach the checked parent hint.
#[test]
fn provider_header_not_found_uses_matching_parent_hint() {
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone());
    let mut state = State::builder()
        .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
        .with_bundle_update()
        .build();

    let block_number = 42u64;
    let parent_hash = B256::repeat_byte(0xA0);
    let hint = hint_test_artifact();
    let (evm_env, inner_ctx) = hint_test_env(block_number, parent_hash);
    let evm = config.evm_with_env(&mut state, evm_env);
    let executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, inner_ctx, &chain_spec, &receipt_builder),
        None,
        Bytes::new(),
        Some(Arc::new(HeaderNotFoundArtifactProvider)),
        false,
        None,
        parent_hash,
        None,
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
        true,
        None,
        Some(hint),
    );

    let metadata = hint_test_metadata(block_number - 1, parent_hash);
    let resolved = executor
        .accounted_parent_artifact_for_metadata(&metadata)
        .expect("HeaderNotFound provider miss must fall back to matching parent hint");

    assert_eq!(
        resolved, hint,
        "executor must use the checked hint when provider visibility races"
    );
}

/// (b): hint rejected when `metadata.finalized_block_hash` does not
/// match `self.parent_hash`. Returns `BlockExecutionError::Internal` with
/// a `parent_artifact_hint mismatch` diagnostic (no silent fallback).
#[test]
fn hint_rejected_when_metadata_hash_mismatch() {
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone());
    let mut state = State::builder()
        .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
        .with_bundle_update()
        .build();

    let block_number = 42u64;
    let parent_hash = B256::repeat_byte(0xA0);
    let foreign_hash = B256::repeat_byte(0xFF);
    assert_ne!(parent_hash, foreign_hash);

    let (evm_env, inner_ctx) = hint_test_env(block_number, parent_hash);
    let evm = config.evm_with_env(&mut state, evm_env);
    let executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, inner_ctx, &chain_spec, &receipt_builder),
        None,
        Bytes::new(),
        None,
        false,
        None,
        parent_hash,
        None,
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
        true,
        None,
        Some(hint_test_artifact()),
    );

    let metadata = hint_test_metadata(block_number - 1, foreign_hash);
    let err = executor
        .accounted_parent_artifact_for_metadata(&metadata)
        .expect_err("metadata.finalized_block_hash mismatch must reject the hint");

    let message = err.to_string();
    assert!(
        message.contains("parent_artifact_hint mismatch"),
        "error must be the hint-mismatch diagnostic, got: {message}"
    );
}

/// (c): hint rejected when `metadata.finalized_block_number` does
/// not equal `block_number - 1`. Same error class as (b) - no silent
/// fallback.
#[test]
fn hint_rejected_when_metadata_number_mismatch() {
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone());
    let mut state = State::builder()
        .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
        .with_bundle_update()
        .build();

    let block_number = 42u64;
    let parent_hash = B256::repeat_byte(0xA0);

    let (evm_env, inner_ctx) = hint_test_env(block_number, parent_hash);
    let evm = config.evm_with_env(&mut state, evm_env);
    let executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, inner_ctx, &chain_spec, &receipt_builder),
        None,
        Bytes::new(),
        None,
        false,
        None,
        parent_hash,
        None,
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
        true,
        None,
        Some(hint_test_artifact()),
    );

    // Off-by-one: metadata claims to describe block (block_number - 2)
    // instead of (block_number - 1).
    let metadata = hint_test_metadata(block_number - 2, parent_hash);
    let err = executor
        .accounted_parent_artifact_for_metadata(&metadata)
        .expect_err("metadata.finalized_block_number mismatch must reject the hint");

    let message = err.to_string();
    assert!(
        message.contains("parent_artifact_hint mismatch"),
        "error must be the hint-mismatch diagnostic, got: {message}"
    );
}

/// negative-control: with NO provider AND NO hint, the lookup
/// returns a `missing execution summary artifact` error rather than
/// silently succeeding. Pins the third branch of the ladder.
#[test]
fn no_provider_no_hint_returns_missing_artifact_error() {
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone());
    let mut state = State::builder()
        .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
        .with_bundle_update()
        .build();

    let block_number = 42u64;
    let parent_hash = B256::repeat_byte(0xA0);

    let (evm_env, inner_ctx) = hint_test_env(block_number, parent_hash);
    let evm = config.evm_with_env(&mut state, evm_env);
    let executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, inner_ctx, &chain_spec, &receipt_builder),
        None,
        Bytes::new(),
        None,
        false,
        None,
        parent_hash,
        None,
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
        true,
        None,
        None, // no hint
    );

    let metadata = hint_test_metadata(block_number - 1, parent_hash);
    let err = executor
        .accounted_parent_artifact_for_metadata(&metadata)
        .expect_err("no provider + no hint must produce a hard error");

    let message = err.to_string();
    assert!(
        message.contains("missing execution summary artifact"),
        "error must be the missing-artifact diagnostic, got: {message}"
    );
}
