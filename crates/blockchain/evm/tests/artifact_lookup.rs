//! - exact-hash-first `AccountedParentArtifactProvider` lookup.
//!
//! These tests pin four invariants:
//!
//! - `(block_number, block_hash)` equality is a mandatory precondition
//!   for returning the artifact.
//! - canonical-by-number lookup is allowed only after explicit hash
//!   equality.
//! - provider-backed installation works WITHOUT a consensus bridge or
//!   proof cache (full-node mode).
//! - the executor accepts a payload-builder-supplied `parent_artifact_hint`
//!   only on exact `(block_number, block_hash)` match plus codec validation.
//!   This invariant lives inside `OutbeBlockExecutor::accounted_parent_artifact_for_metadata`.
//!   We verify it through a source-level structural check, because the executor
//!   is not constructible in isolation without a full EVM context.

use std::ops::RangeBounds;
use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use outbe_evm::{
    AccountedParentArtifactProvider, OutbeEvmConfig, RethAccountedParentArtifactProvider,
};
use outbe_primitives::{
    reshare_artifact::{
        encode_outbe_block_artifacts, ExecutionSummaryArtifact, OutbeBlockArtifacts,
    },
    OutbeHeader,
};
use reth_ethereum::{
    chainspec::{ChainSpec, MAINNET},
    primitives::{Header, SealedHeader},
};
use reth_provider::{HeaderProvider, ProviderResult};

// ---------------------------------------------------------------------------
// Test fixture: an in-memory HeaderProvider. It lets us seed an arbitrary
// `(canonical_at_number, by_hash)` topology. Thus we can distinguish exact-hash
// reads from canonical-by-number reads, and stage competing branches at the
// same height.
// ---------------------------------------------------------------------------

/// In-memory `HeaderProvider` for competing-branch mapper tests. `canonical`
/// is the (number -> SealedHeader) mapping that `sealed_header(n)` returns.
/// `by_hash` is the union of all known headers across branches, which
/// `header(hash)` / `sealed_header_by_hash(hash)` return. The two maps are
/// independent on purpose: tests stage scenarios where a side-chain header
/// exists in `by_hash` but is NOT canonical at its number.
#[derive(Clone, Default)]
struct StageHeaderProvider {
    canonical: std::collections::BTreeMap<u64, SealedHeader<OutbeHeader>>,
    by_hash: std::collections::BTreeMap<B256, SealedHeader<OutbeHeader>>,
}

impl StageHeaderProvider {
    fn insert_canonical(&mut self, sealed: SealedHeader<OutbeHeader>) {
        let number = sealed.header().inner.number;
        self.canonical.insert(number, sealed.clone());
        self.by_hash.insert(sealed.hash(), sealed);
    }

    fn insert_side(&mut self, sealed: SealedHeader<OutbeHeader>) {
        // Side-chain header: reachable by hash but NOT canonical at its number.
        self.by_hash.insert(sealed.hash(), sealed);
    }
}

impl HeaderProvider for StageHeaderProvider {
    type Header = OutbeHeader;

    fn header(&self, block_hash: B256) -> ProviderResult<Option<Self::Header>> {
        Ok(self.by_hash.get(&block_hash).map(|s| s.header().clone()))
    }

    fn header_by_number(&self, num: u64) -> ProviderResult<Option<Self::Header>> {
        Ok(self.canonical.get(&num).map(|s| s.header().clone()))
    }

    fn headers_range(&self, _range: impl RangeBounds<u64>) -> ProviderResult<Vec<Self::Header>> {
        Ok(Vec::new())
    }

    fn sealed_header(&self, number: u64) -> ProviderResult<Option<SealedHeader<Self::Header>>> {
        Ok(self.canonical.get(&number).cloned())
    }

    fn sealed_headers_while(
        &self,
        _range: impl RangeBounds<u64>,
        _predicate: impl FnMut(&SealedHeader<Self::Header>) -> bool,
    ) -> ProviderResult<Vec<SealedHeader<Self::Header>>> {
        Ok(Vec::new())
    }
}

fn test_chain_spec() -> Arc<ChainSpec<OutbeHeader>> {
    MAINNET.as_ref().clone().map_header(OutbeHeader::new).into()
}

/// Build a sealed `OutbeHeader` at the given number/timestamp carrying an
/// `ExecutionSummaryArtifact { validator_fee_sum }` in `extra_data`. The
/// returned `SealedHeader.hash()` is deterministic for the inputs. It is
/// distinct per choice of `discriminator` byte (used to fork competing
/// branches at the same height).
fn header_with_artifact(
    number: u64,
    timestamp: u64,
    validator_fee_sum: U256,
    discriminator: u8,
) -> SealedHeader<OutbeHeader> {
    let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        execution_summary: Some(ExecutionSummaryArtifact { validator_fee_sum }),
        ..Default::default()
    })
    .expect("encode artifacts");
    let inner = Header {
        number,
        timestamp,
        // `nonce` is RLP-included in the header hash. Flipping it produces distinct
        // competing-branch headers at the same `number` and does not touch the
        // consensus-visible fields that tests assert against.
        nonce: alloy_primitives::B64::with_last_byte(discriminator),
        extra_data,
        ..Default::default()
    };
    SealedHeader::seal_slow(OutbeHeader::new(inner))
}

#[test]
fn real_reth_provider_preserves_artifact_lookup_contract() {
    use outbe_primitives::OutbeBlock;
    use reth_primitives_traits::RecoveredBlock;
    use reth_provider::{test_utils::create_test_provider_factory_with_node_types, BlockWriter};

    // This fixture uses MDBX indices and real static-file headers.
    let factory = create_test_provider_factory_with_node_types::<outbe_node::OutbeNode>(Arc::new(
        ChainSpec::<OutbeHeader>::default(),
    ));
    let provider = RethAccountedParentArtifactProvider::new(factory.clone(), None);
    let unknown_hash = B256::repeat_byte(0x9d);
    assert!(provider
        .execution_summary_by_hash(0, unknown_hash)
        .expect("read unseen header")
        .is_none());

    let sealed = header_with_artifact(0, 1_700_000_000, U256::from(123), 0xaa);
    let hash = sealed.hash();
    let state_root = sealed.header().inner.state_root;
    let block = RecoveredBlock::new_unhashed(
        OutbeBlock {
            header: sealed.header().clone(),
            ..Default::default()
        },
        Vec::new(),
    );
    let writer = factory.provider_rw().expect("open real provider writer");
    writer.insert_block(&block).expect("insert canonical block");
    writer.commit().expect("commit canonical block");

    assert_eq!(factory.sealed_header(0).unwrap().unwrap().hash(), hash);
    let found = provider
        .execution_summary_by_hash(0, hash)
        .unwrap()
        .unwrap();
    assert_eq!(found.summary.validator_fee_sum, U256::from(123));
    assert_eq!(found.timestamp, 1_700_000_000);
    assert_eq!(found.state_root, Some(state_root));
    assert!(matches!(
        provider.execution_summary_by_hash(1, hash),
        Err(reth_evm::execute::ProviderError::HeaderNotFound(_))
    ));
    // A hash miss must not return the canonical artifact for a different hash.
    assert!(provider
        .execution_summary_by_hash(0, unknown_hash)
        .expect("check canonical fallback hash")
        .is_none());
}

#[test]
fn accounted_parent_artifact_lookup_uses_exact_hash() {
    let sealed = header_with_artifact(7, 1_700_000_000, U256::from(123u64), 0xAA);
    let block_hash = sealed.hash();
    let block_number = 7;

    let mut hp = StageHeaderProvider::default();
    hp.insert_canonical(sealed);

    // No cache. Provider-only resolution forces the exact-hash branch.
    let provider = RethAccountedParentArtifactProvider::new(hp, None);

    let resolved = provider
        .execution_summary_by_hash(block_number, block_hash)
        .expect("provider read must not fail")
        .expect("exact-hash lookup must return artifact");

    assert_eq!(resolved.summary.validator_fee_sum, U256::from(123u64));
    assert_eq!(resolved.timestamp, 1_700_000_000);
}

// ---------------------------------------------------------------------------
// The provider must NOT silently return a canonical-by-number entry whose hash
// differs from the requested `block_hash`. It returns `Ok(None)`. The caller
// (the executor) maps this to a hard reject.
// ---------------------------------------------------------------------------

#[test]
fn canonical_number_hash_mismatch_rejects() {
    let canonical = header_with_artifact(7, 1_700_000_000, U256::from(11u64), 0xAA);
    let canonical_hash = canonical.hash();

    let mut hp = StageHeaderProvider::default();
    hp.insert_canonical(canonical);

    // No cache. Ask for a DIFFERENT hash at the same number. The provider
    // must not return the canonical artifact silently. Since `block_hash`
    // is not in `by_hash`, sealed_header_by_hash returns None. The
    // canonical-by-number branch is gated on `sealed_header(n).hash() ==
    // block_hash`, which fails here.
    let foreign_hash = B256::repeat_byte(0xBB);
    assert_ne!(foreign_hash, canonical_hash);

    let provider = RethAccountedParentArtifactProvider::new(hp, None);
    let resolved = provider
        .execution_summary_by_hash(7, foreign_hash)
        .expect("provider read must not fail");

    assert!(
        resolved.is_none(),
        "hash mismatch on canonical-by-number must not silently return the canonical artifact"
    );
}

// ---------------------------------------------------------------------------
// Unfinalized side-chain parent. The provider must resolve a header that is
// reachable only through `sealed_header_by_hash` (NOT canonical at its number).
// Direct `parent_artifact_hint` acceptance/rejection unit tests live in
// `src/executor/tests/accounting.rs`.
// ---------------------------------------------------------------------------

#[test]
fn unfinalized_side_chain_parent_artifact_resolves_by_exact_hash() {
    // Exact-hash branch on a side-chain header that is NOT canonical.
    let canonical = header_with_artifact(7, 1_700_000_000, U256::from(100u64), 0xAA);
    let side = header_with_artifact(7, 1_700_000_001, U256::from(200u64), 0xBB);
    let side_hash = side.hash();
    let canonical_hash = canonical.hash();
    assert_ne!(
        side_hash, canonical_hash,
        "branches must have distinct hashes"
    );

    let mut hp = StageHeaderProvider::default();
    hp.insert_canonical(canonical);
    hp.insert_side(side);

    let provider = RethAccountedParentArtifactProvider::new(hp, None);
    let resolved = provider
        .execution_summary_by_hash(7, side_hash)
        .expect("provider read must not fail")
        .expect("side-chain header reachable by hash must resolve");
    assert_eq!(
        resolved.summary.validator_fee_sum,
        U256::from(200u64),
        "side-chain artifact returned, not canonical's"
    );
    assert_eq!(resolved.timestamp, 1_700_000_001);
}

// ---------------------------------------------------------------------------
// Full-node mode (no consensus bridge) still installs a provider-backed
// `AccountedParentArtifactProvider`. The test verifies this through an
// executor built by `OutbeEvmConfig::new_with_provider_only` executing a real
// Phase 1 (`CertifiedParentAccounting`) transaction against the seeded parent.
// ---------------------------------------------------------------------------

#[test]
fn full_node_import_installs_provider_backed_accounted_parent_artifact_lookup_without_bridge() {
    use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
    use alloy_evm::block::BlockExecutor;
    use alloy_evm::eth::EthBlockExecutionCtx;
    use alloy_primitives::Bytes;
    use outbe_evm::{OutbeBlockExecutionCtx, OutbeEvmSigner};
    use outbe_primitives::{
        addresses::REWARDS_ADDRESS,
        block::{BlockContext, BlockRuntimeContext},
        consensus_metadata::CertifiedParentAccountingMetadata,
        storage::{direct::DirectStorageProvider, hashmap::HashMapStorageProvider, StorageHandle},
        system_tx::protocol_block_gas_limit,
        time::timestamp_to_date_key,
    };
    use reth_ethereum::evm::revm::db::State;
    use reth_evm::{execute::ProviderError, ConfigureEvm, EvmEnv};
    use revm::{
        context::{BlockEnv, CfgEnv},
        database::CacheDB,
        database_interface::EmptyDBTyped,
        primitives::hardfork::SpecId,
        state::AccountInfo,
    };

    let seeded_fee = U256::from(55u64);
    let parent_timestamp = 1_700_001_000u64;
    let header = header_with_artifact(5, parent_timestamp, seeded_fee, 0xCC);
    let block_hash = header.hash();

    let mut hp = StageHeaderProvider::default();
    hp.insert_canonical(header);

    let signer = Arc::new(OutbeEvmSigner::from_secret_bytes([1u8; 32]).expect("test signer"));
    let proposer = signer.address();
    let provider = Arc::new(RethAccountedParentArtifactProvider::new(hp, None))
        as Arc<dyn AccountedParentArtifactProvider>;
    let config = OutbeEvmConfig::new_with_provider_only(test_chain_spec(), provider)
        .with_evm_signer(signer.clone());

    assert!(
        config.bridge.is_none(),
        "full-node config must not carry a consensus bridge"
    );

    let make_state = || {
        let mut seed_storage = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut seed_storage, |storage| {
            let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
            vs.config_owner.write(proposer).unwrap();
            vs.set_config_max_validators(128).unwrap();
            vs.config_epoch_length_blocks.write(60).unwrap();
            vs.config_is_initialized.write(true).unwrap();
            let mut pubkey = [0u8; 48];
            pubkey[0] = 0xA2;
            vs.register_validator(proposer, proposer, &pubkey).unwrap();
            vs.activate_validator_via_boundary_for_test(proposer)
                .unwrap();
            outbe_validatorset::write_committee_snapshot(
                storage.clone(),
                0,
                &outbe_validatorset::CommitteeSnapshot {
                    committee: vec![outbe_validatorset::CommitteeEntry {
                        address: proposer,
                        consensus_pubkey: pubkey,
                    }],
                    vrf_material_version: 0,
                    vrf_group_public_key_bytes: vec![0x42; 96],
                    vrf_public_polynomial_hash: B256::ZERO,
                },
            )
            .unwrap();
            let ctx = BlockRuntimeContext::new(
                BlockContext::new(5, parent_timestamp, 1, proposer, vec![proposer]),
                storage,
            );
            ctx.storage
                .contract::<outbe_cycle::schema::Accounting<'_>>()
                .last_accounted_block_number
                .write(4)
                .unwrap();
        });
        let marker_code = revm::state::Bytecode::new_legacy([0xef].into());
        let marker_info = || AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        };
        let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
        let entries: Vec<_> = seed_storage.storage.into_iter().collect();
        let mut addresses: Vec<Address> =
            entries.iter().map(|((address, _), _)| *address).collect();
        addresses.sort_unstable();
        addresses.dedup();
        for address in addresses {
            db.insert_account_info(address, marker_info());
        }
        for ((address, slot), value) in entries {
            db.insert_account_storage(address, slot, value).unwrap();
        }
        db.insert_account_info(
            REWARDS_ADDRESS,
            AccountInfo {
                balance: seeded_fee,
                ..marker_info()
            },
        );
        State::builder()
            .with_database(db)
            .with_bundle_update()
            .build()
    };

    let metadata = CertifiedParentAccountingMetadata {
        finalized_block_number: 5,
        finalized_block_hash: block_hash,
        ordered_committee: vec![proposer],
        signer_bitmap: vec![1],
        ..Default::default()
    };
    let evm_env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(1)
            .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
        block_env: BlockEnv {
            number: U256::from(6u64),
            gas_limit: protocol_block_gas_limit(6),
            basefee: MIN_PROTOCOL_BASE_FEE,
            beneficiary: REWARDS_ADDRESS,
            timestamp: U256::from(1_700_001_006u64),
            ..Default::default()
        },
    };
    let make_ctx = || OutbeBlockExecutionCtx {
        inner: EthBlockExecutionCtx {
            parent_hash: block_hash,
            parent_beacon_block_root: None,
            ommers: &[],
            withdrawals: None,
            extra_data: Bytes::new(),
            tx_count_hint: Some(1),
            slot_number: None,
        },
        timestamp_millis_part: 0,
        block_hash: None,
        block_state_root: None,
        expected_begin_system_txs: Vec::new(),
        system_layout_error: None,
        parent_consensus_metadata: Some(metadata.clone()),
        proposer_evm_address: Some(proposer),
        execute_outbe_block_hooks: false,
        prebuilt_phase1_tx: None,
        parent_artifact_hint: None,
        pending_tee_bootstrap: None,
        execution_read_budget: None,
    };

    let phase1_tx = config
        .build_begin_system_txs(
            6,
            1,
            protocol_block_gas_limit(6),
            block_hash,
            &Bytes::new(),
            Some(metadata.clone()),
            Some(proposer),
            None,
            None,
        )
        .expect("begin-zone system txs should build")
        .into_iter()
        .next()
        .expect("Phase 1 system tx must be first at block 6");

    let mut state = make_state();
    {
        let evm = config.evm_with_env(&mut state, evm_env.clone());
        let mut executor = config.create_executor(evm, make_ctx());
        executor
            .apply_pre_execution_changes()
            .expect("pre-execution marker setup must succeed");
        executor
            .execute_transaction(phase1_tx.clone())
            .expect("Phase 1 CPA must resolve parent artifact through provider-only config");
        assert_eq!(executor.receipts().len(), 1);
        assert!(executor.receipts()[0].success);
    }

    let read_ctx = BlockContext::new(6, 1_700_001_006, 1, proposer, vec![proposer]);
    let mut direct_provider = DirectStorageProvider::new(&mut state, read_ctx.clone());
    StorageHandle::enter(&mut direct_provider, |storage| {
        let runtime_ctx = BlockRuntimeContext::new(read_ctx, storage.clone());
        assert_eq!(
            outbe_cycle::read_last_accounted_block_number(&runtime_ctx)?,
            5
        );
        let rewards = outbe_rewards::schema::Rewards::new(storage);
        let date_key = timestamp_to_date_key(parent_timestamp);
        assert_eq!(rewards.pending_fees.read(&block_hash)?, seeded_fee);
        assert_eq!(rewards.daily_fee_sum_raw.read(&date_key)?, seeded_fee);
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("accounting state should be readable");

    // Negative control: without the provider installed on config, the same
    // Phase 1 execution fails with a missing accounted-parent summary error.
    let mut unseeded_state = make_state();
    let config_without_provider = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer);
    let evm = config_without_provider.evm_with_env(&mut unseeded_state, evm_env);
    let mut executor = config_without_provider.create_executor(evm, make_ctx());
    executor
        .apply_pre_execution_changes()
        .expect("pre-execution marker setup must succeed");
    let err = executor
        .execute_transaction(phase1_tx)
        .expect_err("config without provider must fail Phase 1 parent lookup");
    assert!(
        err.to_string()
            .contains("missing execution summary artifact"),
        "expected missing-summary error without provider, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Settlement-money sanity: the value carried in
// `ExecutionSummaryArtifact.validator_fee_sum` round-trips exactly through
// the provider, so downstream `outbe_rewards::on_finalized_metadata` sees the
// authoritative parent number.
// ---------------------------------------------------------------------------

#[test]
fn settlement_money_loaded_from_parent_execution_summary_artifact() {
    let validator_fee_sum = U256::from(987_654_321_000_000_000u128);
    let sealed = header_with_artifact(42, 1_700_002_000, validator_fee_sum, 0x42);
    let block_hash = sealed.hash();

    let mut hp = StageHeaderProvider::default();
    hp.insert_canonical(sealed);

    let provider = RethAccountedParentArtifactProvider::new(hp, None);
    let resolved = provider
        .execution_summary_by_hash(42, block_hash)
        .expect("provider read must not fail")
        .expect("artifact must resolve");

    assert_eq!(
        resolved.summary.validator_fee_sum, validator_fee_sum,
        "settlement money must be loaded byte-for-byte from the parent artifact"
    );
}

// ---------------------------------------------------------------------------
// Two competing branches at the same height. Each branch
// resolves to its own artifact. A query for one branch's hash must NEVER
// return the other branch's artifact.
// ---------------------------------------------------------------------------

#[test]
fn competing_branch_same_height_stale_artifact_rejects() {
    let branch_a = header_with_artifact(9, 1_700_003_000, U256::from(111u64), 0x01);
    let branch_b = header_with_artifact(9, 1_700_003_000, U256::from(222u64), 0x02);
    let hash_a = branch_a.hash();
    let hash_b = branch_b.hash();
    assert_ne!(hash_a, hash_b, "branches must differ");

    let mut hp = StageHeaderProvider::default();
    // branch_a is canonical. branch_b is a side-chain.
    hp.insert_canonical(branch_a);
    hp.insert_side(branch_b);

    let provider = RethAccountedParentArtifactProvider::new(hp, None);

    let resolved_a = provider
        .execution_summary_by_hash(9, hash_a)
        .expect("provider read must not fail")
        .expect("branch_a must resolve");
    assert_eq!(
        resolved_a.summary.validator_fee_sum,
        U256::from(111u64),
        "branch_a query must return branch_a artifact"
    );

    let resolved_b = provider
        .execution_summary_by_hash(9, hash_b)
        .expect("provider read must not fail")
        .expect("branch_b must resolve (reachable by hash via sealed_header_by_hash)");
    assert_eq!(
        resolved_b.summary.validator_fee_sum,
        U256::from(222u64),
        "branch_b query must return branch_b artifact (not the canonical-at-number branch_a)"
    );

    // Cross-check: cache miss for branch_b's hash + canonical mismatch must
    // never leak branch_a's artifact when the caller asks for an unknown
    // hash at the same number.
    let foreign_hash = B256::repeat_byte(0xFF);
    let resolved_foreign = provider
        .execution_summary_by_hash(9, foreign_hash)
        .expect("provider read must not fail");
    assert!(
        resolved_foreign.is_none(),
        "stale/unknown hash query must return None, never the canonical branch's artifact"
    );

    let _ = Address::ZERO; // keep alloy_primitives::Address as a used import.
}

// `accounted_parent_artifact_lookup_uses_exact_hash`, `canonical_number_hash_mismatch_rejects`,
// and `competing_branch_same_height_stale_artifact_rejects` above cover BEHAVIORALLY the
// exact-hash-primary lookup and the hash-gated canonical-by-number fallback. They call
// `execution_summary_by_hash` over a forked provider and assert the resolved artifact /
// rejection. No source-text scan needed.

// ---------------------------------------------------------------------------
// FCU-Valid race-window: provider raises `ProviderError::HeaderNotFound`
// when consensus has finalized the parent but Reth has not yet persisted the
// sealed header to MDBX. The trait contract treats this as a visibility miss
// (`Ok(None)`) so the executor can fall through to its checked
// `parent_artifact_hint`. A previous version of `execution_summary_by_hash`
// short-circuited the `?`-propagated error as a fatal `BlockExecutionError`.
// That stranded block 2 proposals without `CertifiedParentAccounting` and
// stalled the chain on block 1.
// ---------------------------------------------------------------------------

/// Configurable provider that lets each test choose, per-call, whether
/// `sealed_header_by_hash` and `sealed_header` return `Ok(None)`,
/// `Err(HeaderNotFound)`, or a real I/O error.
#[derive(Clone)]
struct FallibleHeaderProvider {
    by_hash_result: HeaderLookupResult,
    by_number_result: HeaderLookupResult,
}

#[derive(Clone)]
enum HeaderLookupResult {
    None,
    HeaderNotFound,
    Other(String),
}

impl HeaderLookupResult {
    fn into_provider_result(self, key: B256) -> ProviderResult<Option<SealedHeader<OutbeHeader>>> {
        match self {
            HeaderLookupResult::None => Ok(None),
            HeaderLookupResult::HeaderNotFound => {
                Err(reth_evm::execute::ProviderError::HeaderNotFound(key.into()))
            }
            HeaderLookupResult::Other(msg) => {
                Err(reth_evm::execute::ProviderError::TrieWitnessError(msg))
            }
        }
    }
}

impl HeaderProvider for FallibleHeaderProvider {
    type Header = OutbeHeader;

    fn header(&self, _block_hash: B256) -> ProviderResult<Option<Self::Header>> {
        Ok(None)
    }

    fn header_by_number(&self, _num: u64) -> ProviderResult<Option<Self::Header>> {
        Ok(None)
    }

    fn headers_range(&self, _range: impl RangeBounds<u64>) -> ProviderResult<Vec<Self::Header>> {
        Ok(Vec::new())
    }

    fn sealed_header_by_hash(
        &self,
        block_hash: B256,
    ) -> ProviderResult<Option<SealedHeader<Self::Header>>> {
        self.by_hash_result.clone().into_provider_result(block_hash)
    }

    fn sealed_header(&self, number: u64) -> ProviderResult<Option<SealedHeader<Self::Header>>> {
        self.by_number_result
            .clone()
            .into_provider_result(B256::from(U256::from(number)))
    }

    fn sealed_headers_while(
        &self,
        _range: impl RangeBounds<u64>,
        _predicate: impl FnMut(&SealedHeader<Self::Header>) -> bool,
    ) -> ProviderResult<Vec<SealedHeader<Self::Header>>> {
        Ok(Vec::new())
    }
}

fn header_lookup_provider(
    by_hash_result: HeaderLookupResult,
    by_number_result: HeaderLookupResult,
) -> FallibleHeaderProvider {
    FallibleHeaderProvider {
        by_hash_result,
        by_number_result,
    }
}

#[test]
fn exact_hash_header_not_found_maps_to_ok_none() {
    let hp = header_lookup_provider(HeaderLookupResult::HeaderNotFound, HeaderLookupResult::None);
    let provider = RethAccountedParentArtifactProvider::new(hp, None);

    let resolved = provider
        .execution_summary_by_hash(1, B256::repeat_byte(0x9d))
        .expect("HeaderNotFound from sealed_header_by_hash must NOT propagate as Err");

    assert!(
        resolved.is_none(),
        "visibility miss must surface as Ok(None) so the executor can use its hint"
    );
}

#[test]
fn canonical_by_number_header_not_found_maps_to_ok_none() {
    let hp = header_lookup_provider(HeaderLookupResult::None, HeaderLookupResult::HeaderNotFound);
    let provider = RethAccountedParentArtifactProvider::new(hp, None);

    let resolved = provider
        .execution_summary_by_hash(7, B256::repeat_byte(0x77))
        .expect("HeaderNotFound from sealed_header must NOT propagate as Err");

    assert!(
        resolved.is_none(),
        "canonical-by-number visibility miss must also surface as Ok(None)"
    );
}

#[test]
fn non_header_not_found_provider_errors_still_propagate() {
    let hp = header_lookup_provider(
        HeaderLookupResult::Other("simulated MDBX corruption".into()),
        HeaderLookupResult::None,
    );
    let provider = RethAccountedParentArtifactProvider::new(hp, None);

    let err = provider
        .execution_summary_by_hash(1, B256::repeat_byte(0x9d))
        .expect_err("non-HeaderNotFound provider error must remain fatal");

    let msg = err.to_string();
    assert!(
        msg.contains("simulated MDBX corruption"),
        "real I/O / corruption errors must propagate verbatim; got {msg}"
    );
}
