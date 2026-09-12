use super::*;

fn test_oracle_get_params_tx() -> reth_ethereum::TransactionSigned {
    let selector = keccak256("getParams()");
    TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: 100_000,
        max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(ORACLE_ADDRESS),
        value: U256::ZERO,
        input: Bytes::copy_from_slice(&selector[..4]),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

#[test]
fn oracle_tx_keeps_fee_envelope_for_basefee_validation() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let oracle_tx = test_oracle_get_params_tx()
        .try_into_recovered()
        .expect("oracle tx signer should recover");

    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    db.insert_account_info(
        oracle_tx.signer(),
        AccountInfo {
            balance: U256::from(1_000_000u64),
            ..Default::default()
        },
    );

    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();
    let evm_env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
        block_env: BlockEnv {
            number: U256::from(1u64),
            gas_limit: 30_000_000,
            basefee: MIN_PROTOCOL_BASE_FEE,
            beneficiary: OWNER,
            timestamp: U256::from(1u64),
            ..Default::default()
        },
    };
    let evm = config.evm_with_env(&mut state, evm_env);
    let ctx = execution_ctx(Some(1), Bytes::new());
    let mut executor = config.create_executor(evm, ctx);

    executor
        .execute_transaction(oracle_tx)
        .expect("oracle tx with fee cap at basefee must pass validation");

    assert_eq!(executor.receipts().len(), 1);
}

#[test]
fn gas_10_low_gas_zero_fee_policy_failure_must_not_mint_intrinsic_gas() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let low_gas_zero_fee_tx = test_oracle_submit_vote_tx_with_gas_limit(1)
        .try_into_recovered()
        .expect("oracle tx signer should recover");

    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let marker_code = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        ORACLE_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code),
            ..Default::default()
        },
    );
    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));

    let err = executor
        .execute_transaction(low_gas_zero_fee_tx)
        .expect_err("gas_limit < intrinsic gas must reject before synthetic receipt creation");
    assert!(
        err.to_string().contains("intrinsic") || err.to_string().contains("gas limit"),
        "GAS-10: low-gas zero-fee rejection must be an admission error, got {err}"
    );
    assert!(
        executor.receipts().is_empty(),
        "GAS-10: invalid low-gas zero-fee tx must not mint a 21k synthetic receipt"
    );
}

/// The per-block zero-fee soft-failure cap admits up to
/// `MAX_ZERO_FEE_SOFT_FAILURES_PER_BLOCK` soft-failures, then rejects further
/// ones with a tx-level `InvalidTx` - the variant the payload builder SKIPS
/// (mark_invalid + continue) and a validator REJECTS the block on, NOT a
/// fatal `Internal` error that would abort the build (the 2026-05-15 halt).
#[test]
fn zero_fee_soft_failure_cap_admits_then_rejects_with_invalid_tx() {
    use alloy_evm::block::{BlockExecutionError, BlockValidationError};
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));

    let mut admitted = 0u32;
    let rejected_err = loop {
        match executor.record_zero_fee_soft_failure(B256::ZERO) {
            Ok(()) => {
                admitted += 1;
                assert!(admitted <= 4096, "cap never enforced");
            }
            Err(err) => break err,
        }
    };
    assert_eq!(
        admitted, 64,
        "zero-fee soft-failure cap must admit exactly MAX_ZERO_FEE_SOFT_FAILURES_PER_BLOCK (64)"
    );
    assert!(
        matches!(
            rejected_err,
            BlockExecutionError::Validation(BlockValidationError::InvalidTx { .. })
        ),
        "over-cap zero-fee soft-failure must be a tx-level InvalidTx (skip-on-build / \
         reject-on-validate), got: {rejected_err:?}"
    );
}

#[test]
fn zero_fee_oracle_vote_from_delegated_feeder_keeps_zero_balance() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let validator = address!("0x1111111111111111111111111111111111111111");
    let pk = dummy_pubkey(0xA1);
    let zero_fee_tx = test_oracle_submit_vote_tx()
        .try_into_recovered()
        .expect("oracle submitVote tx signer should recover");
    let feeder = Address::from(*zero_fee_tx.signer());

    let mut seed_storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut seed_storage, |storage| {
        seed_registered_active_validator(storage.clone(), validator, &pk);

        // Feeder resolution moved to the role-scoped ValidatorSet
        // delegation registry; the legacy oracle-side mapping is no longer
        // consulted by `resolve_validator_for_feeder`.
        let mut validator_set = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        validator_set.set_delegate(
            validator,
            outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
            feeder,
        )?;
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("test genesis state must be seeded");

    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    for address in seed_storage
        .storage
        .keys()
        .map(|(address, _)| *address)
        .collect::<std::collections::HashSet<_>>()
    {
        db.insert_account_info(address, AccountInfo::default());
    }
    for ((address, slot), value) in seed_storage.storage {
        db.insert_account_storage(address, slot, value)
            .expect("seed storage insert should succeed");
    }
    let marker_code = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        ORACLE_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code),
            ..Default::default()
        },
    );

    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();

    let feeder_balance_before = state
        .basic(feeder)
        .expect("feeder account read should succeed")
        .map(|account| account.balance)
        .unwrap_or_default();
    assert_eq!(feeder_balance_before, U256::ZERO);

    let setup_read_ctx = BlockContext::new(1, 1, CHAIN_ID, OWNER, vec![validator]);
    let mut setup_provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, setup_read_ctx);
    StorageHandle::enter(&mut setup_provider, |storage| {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let record = vs
            .get_validator(validator)?
            .expect("validator should be registered");
        assert_eq!(record.status, outbe_validatorset::logic::status::ACTIVE);
        assert!(record.has_bls_share);

        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        assert_eq!(oracle.resolve_validator_for_feeder(feeder)?, validator);
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("seeded zero-fee authorization state should be readable");

    {
        let evm_env = EvmEnv {
            cfg_env: CfgEnv::new()
                .with_chain_id(CHAIN_ID)
                .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
            block_env: BlockEnv {
                number: U256::from(1u64),
                gas_limit: 30_000_000,
                basefee: 1_000_000_000,
                beneficiary: OWNER,
                timestamp: U256::from(1u64),
                ..Default::default()
            },
        };
        let evm = config.evm_with_env(&mut state, evm_env);
        let ctx = execution_ctx(Some(1), Bytes::new());
        let mut executor = config.create_executor(evm, ctx);

        executor
            .execute_transaction(zero_fee_tx)
            .expect("delegated zero-fee oracle vote should execute");

        assert_eq!(executor.receipts().len(), 1);
        assert!(executor.receipts()[0].success);
        assert!(executor.receipts()[0].cumulative_gas_used > 0);
        assert!(executor.receipts()[0]
            .logs
            .iter()
            .any(|log| log.address == ORACLE_ADDRESS));
    }
    state.merge_transitions(BundleRetention::Reverts);

    let mut slot_storage = HashMapStorageProvider::new(CHAIN_ID);
    let vote_slot = StorageHandle::enter(&mut slot_storage, |storage| {
        outbe_oracle::schema::OracleContract::new(storage.clone())
            .vote_exists
            .get(&validator)
            .slot()
    });
    assert_eq!(
        state
            .bundle_state
            .storage(&ORACLE_ADDRESS, vote_slot)
            .unwrap_or_default(),
        U256::from(1u64)
    );

    let feeder_balance_after = state
        .basic(feeder)
        .expect("feeder account read should succeed")
        .map(|account| account.balance)
        .unwrap_or_default();
    assert_eq!(feeder_balance_after, U256::ZERO);
}

/// / T6.2 parity: two executors with identical state and tx
/// produce byte-equal soft-fail receipts. This is the on-chain parity
/// invariant that keeps `receipts_root` deterministic across proposer
/// and validators when a zero-fee tx is soft-failed.
#[test]
fn parity_soft_failed_zero_fee_receipt_is_byte_equal_across_runs() {
    fn run() -> Vec<reth_ethereum::Receipt> {
        let config = OutbeEvmConfig::new(test_chain_spec());
        // No validator-set seeding, no feeder delegation: the oracle vote will
        // hit `authorize_fee_waiver` -> `UnauthorizedSigner` (code 107).
        let zero_fee_tx = test_oracle_submit_vote_tx()
            .try_into_recovered()
            .expect("oracle submitVote tx signer should recover");

        let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
        let marker_code = Bytecode::new_legacy([0xef].into());
        db.insert_account_info(
            ORACLE_ADDRESS,
            AccountInfo {
                code_hash: marker_code.hash_slow(),
                code: Some(marker_code),
                ..Default::default()
            },
        );

        let mut state = State::builder()
            .with_database(db)
            .with_bundle_update()
            .build();

        let evm_env = EvmEnv {
            cfg_env: CfgEnv::new()
                .with_chain_id(CHAIN_ID)
                .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
            block_env: BlockEnv {
                number: U256::from(1u64),
                gas_limit: 30_000_000,
                basefee: 1_000_000_000,
                beneficiary: OWNER,
                timestamp: U256::from(1u64),
                ..Default::default()
            },
        };
        let evm = config.evm_with_env(&mut state, evm_env);
        let ctx = execution_ctx(Some(1), Bytes::new());
        let mut executor = config.create_executor(evm, ctx);

        executor
            .execute_transaction(zero_fee_tx)
            .expect("soft-fail path must not abort the block build");

        executor.receipts().to_vec()
    }

    let receipts_a = run();
    let receipts_b = run();

    assert_eq!(receipts_a.len(), 1);
    assert_eq!(receipts_b.len(), 1);
    assert!(!receipts_a[0].success);
    assert_eq!(receipts_a[0].logs.len(), 1);
    // Soft-fail log must come from the zero-fee policy address with the
    // OutbeFailure topic0 - anything else is a parity drift.
    assert_eq!(
        receipts_a[0].logs[0].address,
        outbe_primitives::addresses::ZERO_FEE_POLICY_LOG_ADDRESS
    );
    assert_eq!(
        receipts_a[0].logs[0].data.topics()[0],
        crate::failure_receipt::OUTBE_FAILURE_TOPIC0
    );
    // Code 107 (UnauthorizedSigner) - padded to 32 bytes BE.
    let mut expected_topic1 = [0u8; 32];
    expected_topic1[30] = 0;
    expected_topic1[31] = 107;
    assert_eq!(
        receipts_a[0].logs[0].data.topics()[1].as_slice(),
        expected_topic1
    );

    // Byte parity: RLP-encode both runs' receipts and compare bytes.
    // EIP-2718 is the canonical encoding used by `receipts_root`, so byte
    // equality here means `receipts_root` will be equal on every node.
    use alloy_consensus::TxReceipt;
    use alloy_eips::eip2718::Encodable2718;
    let buf_a = receipts_a[0].with_bloom_ref().encoded_2718();
    let buf_b = receipts_b[0].with_bloom_ref().encoded_2718();
    assert_eq!(
        buf_a, buf_b,
        "soft-fail receipts must be byte-equal across runs"
    );
}

/// / T6.6 property: `validator_fee_sum` MUST NOT be perturbed
/// by soft-failed zero-fee transactions. Failed zero-fee txs never run
/// the EVM and never contribute miner fees; only successful user txs in
/// the priority-fee path increment `current_block_validator_fees`.
///
/// This is a focused invariance test (proptest-style over multiple
/// runs without the full `proptest` macro to keep the test fast and
/// dependency-free).
#[test]
fn property_soft_fail_does_not_perturb_validator_fee_sum() {
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone());
    for run in 0..5 {
        let zero_fee_tx = test_oracle_submit_vote_tx()
            .try_into_recovered()
            .expect("oracle submitVote tx signer should recover");

        let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
        let marker_code = Bytecode::new_legacy([0xef].into());
        db.insert_account_info(
            ORACLE_ADDRESS,
            AccountInfo {
                code_hash: marker_code.hash_slow(),
                code: Some(marker_code),
                ..Default::default()
            },
        );
        let mut state = State::builder()
            .with_database(db)
            .with_bundle_update()
            .build();

        let evm_env = EvmEnv {
            cfg_env: CfgEnv::new()
                .with_chain_id(CHAIN_ID)
                .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
            block_env: BlockEnv {
                number: U256::from(1u64 + run as u64),
                gas_limit: 30_000_000,
                basefee: 1_000_000_000,
                beneficiary: OWNER,
                timestamp: U256::from(1u64 + run as u64),
                ..Default::default()
            },
        };
        let evm = config.evm_with_env(&mut state, evm_env);
        let ctx = execution_ctx(Some(1), Bytes::new());
        // Construct OutbeBlockExecutor directly (instead of through
        // `config.create_executor`) to keep the concrete type so we can
        // call `current_execution_summary` - the method is private to
        // `OutbeBlockExecutor` and hidden behind the `BlockExecutorFor`
        // opaque return type otherwise.
        let mut executor = OutbeBlockExecutor::new(
            EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
            None,
            Bytes::new(),
            None,
            false,
            None,
            ctx.inner.parent_hash,
            None,
            ctx.expected_begin_system_txs.clone(),
            ctx.expected_end_system_txs.clone(),
            ctx.system_layout_error.clone(),
            ctx.parent_consensus_metadata.clone(),
            ctx.proposer_evm_address,
            ctx.execute_outbe_block_hooks,
            ctx.prebuilt_phase1_tx.clone(),
            ctx.parent_artifact_hint,
        );

        // Baseline: no txs.
        assert_eq!(
            executor.current_execution_summary().validator_fee_sum,
            U256::ZERO,
            "run {run}: baseline fee sum must be zero"
        );

        // Soft-fail one tx.
        executor
            .execute_transaction(zero_fee_tx)
            .expect("soft-fail must succeed");

        // Invariant: failed zero-fee tx contributes 0 to validator fee sum.
        assert_eq!(
            executor.current_execution_summary().validator_fee_sum,
            U256::ZERO,
            "run {run}: soft-failed zero-fee tx must not credit the validator"
        );
    }
}

/// / T6.4 mempool natural-eviction bridge: a soft-failed zero-fee
/// tx returns `Ok(non-zero gas)` from `execute_transaction`, which signals
/// the `BasicBlockBuilder` to append the tx to `block.body`. Reth's pool
/// then evicts the tx hash on canonical commit via the standard
/// `on_new_head_block` -> `pool.remove_transactions(block_hashes)` path.
///
/// This is the contract that lets (T4 Won't Do) skip any custom
/// `mark_invalid` plumbing - confirmation that the executor's `Ok` return
/// is enough for the natural-eviction flow downstream.
#[test]
fn soft_fail_returns_ok_so_tx_lands_in_block_body() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let zero_fee_tx = test_oracle_submit_vote_tx()
        .try_into_recovered()
        .expect("oracle submitVote tx signer should recover");

    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let marker_code = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        ORACLE_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code),
            ..Default::default()
        },
    );
    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();

    let evm_env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
        block_env: BlockEnv {
            number: U256::from(1u64),
            gas_limit: 30_000_000,
            basefee: 1_000_000_000,
            beneficiary: OWNER,
            timestamp: U256::from(1u64),
            ..Default::default()
        },
    };
    let evm = config.evm_with_env(&mut state, evm_env);
    let ctx = execution_ctx(Some(1), Bytes::new());
    let mut executor = config.create_executor(evm, ctx);

    // Soft-fail path returns `Ok` - the contract that lets the wrapping
    // `BasicBlockBuilder` append the tx to `block.body.transactions`.
    let gas_output = executor
        .execute_transaction(zero_fee_tx)
        .expect("soft-fail path must not abort the block build");

    // Non-zero gas: signals the tx was "executed and committed" from the
    // BlockBuilder's perspective, even though no EVM code ran.
    assert!(
        gas_output.tx_gas_used() > 0,
        "non-zero gas signals the tx is committed to the block body, \
         which is the prerequisite for Reth's standard pool eviction"
    );
    // Exactly one receipt was pushed.
    assert_eq!(executor.receipts().len(), 1);
    assert!(
        !executor.receipts()[0].success,
        "soft-fail receipt must have status=0"
    );
    // Receipt contains the synthetic failure log; eth_getTransactionReceipt
    // will surface this to external observers.
    assert_eq!(executor.receipts()[0].logs.len(), 1);
    assert_eq!(
        executor.receipts()[0].logs[0].address,
        outbe_primitives::addresses::ZERO_FEE_POLICY_LOG_ADDRESS
    );
}

// -----------------------------------------------------------------
// EIP-7702 sponsored free-tx integration tests
//
// These tests verify the executor pre-fee hook end-to-end against
// real `State<DB>` + revm - NOT just the storage-primitive level.
// They cover the four claims the unit tests do NOT prove:
//   1. Counter persists through revm tx revert (anti-drain).
//   2. `SponsorshipAuthorized` event lands on the inner tx receipt.
//   3. Signer balance is genuinely unchanged (no fee debit).
//   4. EIP-7702 delegation to a NON-paymaster address falls through
//      to the normal fee path.
// -----------------------------------------------------------------

use outbe_primitives::addresses::{AGENT_REWARD_ADDRESS, ZEROFEE_ADDRESS};
use outbe_zerofee::precompile::IZeroFee::SponsorshipAuthorized;

fn agent_reward_query_input() -> Vec<u8> {
    let selector = keccak256(b"getClaimableBalance(address)");
    let mut input = Vec::with_capacity(36);
    input.extend_from_slice(&selector[..4]);
    input.extend_from_slice(&[0u8; 32]);
    input
}

/// Sponsored signer derived from the alloy test-signature recovery.
/// We don't care WHICH address it is - only that it is stable across
/// runs and we attach delegation + balance + nonce to it.
fn sponsored_test_tx(input: Vec<u8>) -> reth_ethereum::TransactionSigned {
    TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: 200_000,
        max_fee_per_gas: alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(AGENT_REWARD_ADDRESS),
        value: U256::ZERO,
        input: input.into(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

/// CfgEnv configured for Pectra (EIP-7702-active). The default test
/// cfg uses SHANGHAI, which silently disables delegation re-load.
fn pectra_evm_env(block_number: u64) -> EvmEnv {
    EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE),
        block_env: BlockEnv {
            number: U256::from(block_number),
            gas_limit: 30_000_000,
            basefee: alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE,
            beneficiary: OWNER,
            // 2026-04-01 00:00:00 UTC - matches BLOCK_DAY constant
            // in the zerofee unit tests for cross-reference.
            timestamp: U256::from(1_775_001_600u64),
            ..Default::default()
        },
    }
}

fn sign_test_hash(key: &k256::ecdsa::SigningKey, hash: &B256) -> alloy_primitives::Signature {
    let (signature, recovery_id): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = key
        .sign_prehash(hash.as_slice())
        .expect("test prehash signing must succeed");
    alloy_primitives::Signature::from_bytes_and_parity(
        signature.to_bytes().as_slice(),
        recovery_id.to_byte() != 0,
    )
    .normalized_s()
}

fn bootstrap_test_tx() -> reth_primitives_traits::Recovered<reth_ethereum::TransactionSigned> {
    let key = k256::ecdsa::SigningKey::from_slice(&[0x11; 32]).unwrap();
    let signer = Address::from_public_key(key.verifying_key());
    let authorization = Authorization {
        chain_id: U256::from(CHAIN_ID),
        address: ZEROFEE_ADDRESS,
        nonce: 1,
    };
    let authorization_signature = sign_test_hash(&key, &authorization.signature_hash());
    let signed_authorization = authorization.into_signed(authorization_signature);
    let input = outbe_zerofee::precompile::IZeroFee::authorizeSponsorshipCall { signer }
        .abi_encode()
        .into();
    let tx = TxEip7702 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: outbe_zerofee::FREE_TX_BOOTSTRAP_GAS_LIMIT,
        max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: ZEROFEE_ADDRESS,
        value: U256::ZERO,
        access_list: Default::default(),
        authorization_list: vec![signed_authorization],
        input,
    };
    let tx_signature = sign_test_hash(&key, &tx.signature_hash());
    let recovered = reth_ethereum::TransactionSigned::from(tx.into_signed(tx_signature))
        .try_into_recovered()
        .expect("bootstrap signer must recover");
    assert_eq!(Address::from(*recovered.signer()), signer);
    recovered
}

#[test]
fn eip7702_bootstrap_accepts_one_atomic_unit_without_fee_or_quota() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let recovered = bootstrap_test_tx();
    let replay = recovered.clone();
    let signer = Address::from(*recovered.signer());
    let initial_balance = U256::from(1);

    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let marker = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        ZEROFEE_ADDRESS,
        AccountInfo {
            code_hash: marker.hash_slow(),
            code: Some(marker),
            ..Default::default()
        },
    );
    db.insert_account_info(
        signer,
        AccountInfo {
            balance: initial_balance,
            nonce: 0,
            ..Default::default()
        },
    );
    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();

    {
        let evm = config.evm_with_env(&mut state, pectra_evm_env(1));
        let ctx = execution_ctx(Some(1), Bytes::new());
        let mut executor = config.create_executor(evm, ctx);

        let gas_output = executor
            .execute_transaction(recovered)
            .expect("one-unit bootstrap must execute under the fee waiver");
        assert!(
            gas_output.tx_gas_used() > 0,
            "bootstrap gas must remain visible in block accounting"
        );
        assert!(
            gas_output.tx_gas_used() <= outbe_zerofee::FREE_TX_BOOTSTRAP_GAS_LIMIT,
            "bootstrap gas must fit its signed limit"
        );
        assert_eq!(
            executor.current_execution_summary().validator_fee_sum,
            U256::ZERO,
            "bootstrap must not credit a validator fee"
        );
        assert_eq!(executor.receipts().len(), 1);
        assert!(executor.receipts()[0].success);

        executor
            .execute_transaction(replay)
            .expect_err("same-block bootstrap replay must fail against current state");
        assert_eq!(
            executor.receipts().len(),
            1,
            "replay must not append a receipt"
        );
    }
    state.merge_transitions(BundleRetention::Reverts);

    let account = state
        .basic(signer)
        .expect("bootstrap account read")
        .expect("bootstrap account exists");
    assert_eq!(
        account.balance, initial_balance,
        "bootstrap must not charge COEN"
    );
    assert_eq!(
        account.nonce, 2,
        "outer tx plus self-authorization consume two nonces"
    );
    assert_eq!(
        account.code.and_then(|code| code.eip7702_address()),
        Some(ZEROFEE_ADDRESS),
        "bootstrap must install the canonical delegation"
    );
    assert_eq!(
        zerofee_counter_for(&mut state, signer),
        0,
        "bootstrap must leave all daily sponsored calls available"
    );
}

#[test]
fn eip7702_bootstrap_rejects_zero_balance_without_state_change() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let recovered = bootstrap_test_tx();
    let signer = Address::from(*recovered.signer());

    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let marker = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        ZEROFEE_ADDRESS,
        AccountInfo {
            code_hash: marker.hash_slow(),
            code: Some(marker),
            ..Default::default()
        },
    );
    db.insert_account_info(signer, AccountInfo::default());
    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();

    {
        let evm = config.evm_with_env(&mut state, pectra_evm_env(1));
        let ctx = execution_ctx(Some(1), Bytes::new());
        let mut executor = config.create_executor(evm, ctx);
        let _error = executor
            .execute_transaction(recovered)
            .expect_err("zero-balance bootstrap must not receive the waiver");
        assert!(executor.receipts().is_empty());
    }

    let account = state
        .basic(signer)
        .expect("bootstrap account read")
        .expect("bootstrap account exists");
    assert!(account.balance.is_zero());
    assert_eq!(account.nonce, 0);
    assert!(account.is_empty_code_hash());
    assert_eq!(zerofee_counter_for(&mut state, signer), 0);
}

fn cache_db_with_paymaster_account(
    signer: Address,
    signer_balance: U256,
) -> CacheDB<EmptyDBTyped<ProviderError>> {
    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();

    // ZEROFEE_ADDRESS: marker bytecode for EIP-161 preservation.
    let marker = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        ZEROFEE_ADDRESS,
        AccountInfo {
            code_hash: marker.hash_slow(),
            code: Some(marker.clone()),
            ..Default::default()
        },
    );
    // AGENT_REWARD_ADDRESS: same marker, it is a precompile target.
    db.insert_account_info(
        AGENT_REWARD_ADDRESS,
        AccountInfo {
            code_hash: marker.hash_slow(),
            code: Some(marker),
            ..Default::default()
        },
    );

    // signer: EIP-7702 delegated to ZEROFEE_ADDRESS, with the
    // requested balance so sponsored fee-debit invariants can be
    // exercised for both funded and exactly-zero accounts.
    let delegation = Bytecode::new_eip7702(ZEROFEE_ADDRESS);
    db.insert_account_info(
        signer,
        AccountInfo {
            balance: signer_balance,
            code_hash: delegation.hash_slow(),
            code: Some(delegation),
            ..Default::default()
        },
    );
    db
}

fn zerofee_counter_for(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    signer: Address,
) -> u64 {
    // Reconstruct the counter slot via the same Map<Address, u64>
    // the contract uses, then read it directly off the bundle
    // state as a raw U256 and narrow to u64.
    let mut slot_storage = HashMapStorageProvider::new(CHAIN_ID);
    let slot = StorageHandle::enter(&mut slot_storage, |storage| {
        outbe_zerofee::ZeroFeeContract::new(storage.clone())
            .counter
            .slot(&signer)
            .slot()
    });
    state
        .bundle_state
        .storage(&ZEROFEE_ADDRESS, slot)
        .unwrap_or_default()
        .saturating_to::<u64>()
}

/// Happy path: a sponsored tx with `value=0`, `priority_fee=0`,
/// `to in whitelist` is admitted by the
/// executor pre-fee hook, executed under zero-fee cfg overrides,
/// and produces a receipt with a `SponsorshipAuthorized` log. The
/// signer's balance is untouched and ZEROFEE_ADDRESS' counter slot
/// is bumped to `(today, 1)`.
#[test]
fn eip7702_sponsored_tx_burns_quota_and_emits_event() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let recovered = sponsored_test_tx(agent_reward_query_input())
        .try_into_recovered()
        .expect("test-signature must recover");
    let signer = Address::from(*recovered.signer());

    let initial_balance = U256::from(1u64);
    let mut state = State::builder()
        .with_database(cache_db_with_paymaster_account(signer, initial_balance))
        .with_bundle_update()
        .build();

    let before = signer_balance(&mut state, signer);
    assert_eq!(before, initial_balance);

    {
        let evm = config.evm_with_env(&mut state, pectra_evm_env(1));
        let ctx = execution_ctx(Some(1), Bytes::new());
        let mut executor = config.create_executor(evm, ctx);

        executor
            .execute_transaction(recovered)
            .expect("sponsored tx should execute");

        let receipts = executor.receipts();
        assert_eq!(receipts.len(), 1);
        assert!(receipts[0].success, "sponsored transaction must succeed");

        // Find the SponsorshipAuthorized log on the receipt - this
        // is the guarantee. Topic[0] must match the event sig
        // hash; signer is topic[1] indexed.
        let sig_hash = SponsorshipAuthorized::SIGNATURE_HASH;
        let sponsorship_log = receipts[0]
            .logs
            .iter()
            .find(|l| l.address == ZEROFEE_ADDRESS && l.topics().first() == Some(&sig_hash))
            .expect("SponsorshipAuthorized log must be attached to the receipt");
        // topic[1] = padded signer
        let signer_topic = sponsorship_log
            .topics()
            .get(1)
            .expect("signer topic present");
        assert_eq!(
            &signer_topic.as_slice()[12..],
            signer.as_slice(),
            "signer indexed in topic[1]"
        );
    }
    state.merge_transitions(BundleRetention::Reverts);

    // Balance must be exactly what we put in - no fee debit. This
    // is the consensus-visible guarantee the README promises.
    let after = signer_balance(&mut state, signer);
    assert_eq!(
        after, initial_balance,
        "sponsored tx must not debit signer balance"
    );

    // Counter slot for `signer` must read `(date_key, 1)`. The
    // expected day is 20260401 (matches BLOCK_DAY in unit tests).
    let counter = zerofee_counter_for(&mut state, signer);
    let (day, count) = outbe_zerofee::unpack_counter(counter);
    assert_eq!(
        count, 1,
        "counter must be exactly 1 after a single sponsored tx"
    );
    assert_eq!(day, 20_260_401, "day-key must come from block timestamp");
}

/// EIP-7702 delegation to a different address must NOT trigger the
/// sponsored path. The tx goes through the normal fee path; with
/// `priority_fee = 0` and signer's balance below the gas cost, the
/// EVM `disable_balance_check` would normally let it through - we
/// assert it does NOT.
#[test]
fn eip7702_delegation_to_non_paymaster_falls_through_to_fee_path() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let recovered = sponsored_test_tx(agent_reward_query_input())
        .try_into_recovered()
        .expect("test-signature must recover");
    let signer = Address::from(*recovered.signer());

    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let marker = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        AGENT_REWARD_ADDRESS,
        AccountInfo {
            code_hash: marker.hash_slow(),
            code: Some(marker.clone()),
            ..Default::default()
        },
    );
    // Delegate to ORACLE_ADDRESS, NOT ZEROFEE_ADDRESS.
    let foreign_delegation = Bytecode::new_eip7702(ORACLE_ADDRESS);
    db.insert_account_info(
        signer,
        AccountInfo {
            balance: U256::from(2_000_000u64),
            code_hash: foreign_delegation.hash_slow(),
            code: Some(foreign_delegation),
            ..Default::default()
        },
    );
    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();

    {
        let evm = config.evm_with_env(&mut state, pectra_evm_env(1));
        let ctx = execution_ctx(Some(1), Bytes::new());
        let mut executor = config.create_executor(evm, ctx);

        // The tx is shaped like a sponsored envelope (priority_fee=0,
        // small gas) - but because signer's code points to ORACLE,
        // the pre-fee hook leaves it to the normal path. The normal
        // path requires balance to cover `gas_limit * max_fee_per_gas`,
        // which 2 COEN (2_000_000 unit) covers at the protocol fee floor,
        // so this succeeds. The key assertion is that NO SponsorshipAuthorized
        // log is emitted and the counter stays at 0.
        executor
            .execute_transaction(recovered)
            .expect("non-sponsored tx should still execute through normal fee path");

        let receipts = executor.receipts();
        assert_eq!(receipts.len(), 1);
        let sig_hash = SponsorshipAuthorized::SIGNATURE_HASH;
        let has_event = receipts[0]
            .logs
            .iter()
            .any(|l| l.address == ZEROFEE_ADDRESS && l.topics().first() == Some(&sig_hash));
        assert!(
            !has_event,
            "non-sponsored tx must NOT emit SponsorshipAuthorized"
        );
    }
    state.merge_transitions(BundleRetention::Reverts);

    // Counter must remain at 0 - no quota burn for delegation to
    // foreign address.
    let counter = zerofee_counter_for(&mut state, signer);
    assert_eq!(counter, 0, "non-sponsored path must not burn quota");
}

/// Native balance is not an eligibility signal: a correctly delegated
/// zero-balance signer executes through the sponsored path, burns one
/// quota slot, and pays no native gas.
#[test]
fn eip7702_sponsored_tx_accepts_fresh_zero_balance_signer() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let recovered = sponsored_test_tx(agent_reward_query_input())
        .try_into_recovered()
        .expect("test-signature must recover");
    let signer = Address::from(*recovered.signer());

    let mut state = State::builder()
        .with_database(cache_db_with_paymaster_account(signer, U256::ZERO))
        .with_bundle_update()
        .build();

    {
        let evm = config.evm_with_env(&mut state, pectra_evm_env(1));
        let ctx = execution_ctx(Some(1), Bytes::new());
        let mut executor = config.create_executor(evm, ctx);

        executor
            .execute_transaction(recovered)
            .expect("zero-balance sponsored tx should execute");

        let receipts = executor.receipts();
        assert_eq!(receipts.len(), 1);
        assert!(receipts[0].success, "zero-balance sponsorship must succeed");
        let sponsorship_log = receipts[0]
            .logs
            .iter()
            .find(|log| {
                log.address == ZEROFEE_ADDRESS
                    && log.topics().first() == Some(&SponsorshipAuthorized::SIGNATURE_HASH)
            })
            .expect("successful zero-balance sponsorship must emit authorization");
        assert_eq!(
            &sponsorship_log.topics()[1].as_slice()[12..],
            signer.as_slice()
        );
    }
    state.merge_transitions(BundleRetention::Reverts);

    assert_eq!(signer_balance(&mut state, signer), U256::ZERO);
    let counter = zerofee_counter_for(&mut state, signer);
    let (day, count) = outbe_zerofee::unpack_counter(counter);
    assert_eq!(day, 20_260_401);
    assert_eq!(count, 1);
}

/// Computes the ZEROFEE counter storage slot for `signer` (the same
/// keccak-derived `Map<Address,u64>` slot the contract uses).
fn zerofee_counter_slot(signer: Address) -> U256 {
    let mut slot_storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut slot_storage, |storage| {
        outbe_zerofee::ZeroFeeContract::new(storage.clone())
            .counter
            .slot(&signer)
            .slot()
    })
}

/// F2/code-110 executor-level proof: when the signer has already
/// burned all 8 slots for today, a 9th sponsored tx is NOT rejected
/// by the pre-fee hook as a hard error - it lands in the block with
/// a `status=0` receipt carrying `OutbeFailure(110)`, the counter
/// stays at 8 (no over-burn), and no balance is debited. This is the
/// exact contract the README promises and the txpool relies on
/// (pool admits, executor produces the soft-failure).
#[test]
fn eip7702_ninth_sponsored_tx_soft_fails_with_code_110() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let recovered = sponsored_test_tx(agent_reward_query_input())
        .try_into_recovered()
        .expect("test-signature must recover");
    let signer = Address::from(*recovered.signer());

    // pectra_evm_env uses timestamp 1_775_001_600 -> UTC day 20260401.
    const TODAY: u32 = 20_260_401;
    let initial_balance = U256::from(1u64);

    let mut db = cache_db_with_paymaster_account(signer, initial_balance);
    // Seed the counter to the full daily limit for TODAY.
    db.insert_account_storage(
        ZEROFEE_ADDRESS,
        zerofee_counter_slot(signer),
        U256::from(outbe_zerofee::pack_counter(
            TODAY,
            outbe_zerofee::FREE_TX_DAILY_LIMIT,
        )),
    )
    .expect("seed counter storage");

    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();

    {
        let evm = config.evm_with_env(&mut state, pectra_evm_env(1));
        let ctx = execution_ctx(Some(1), Bytes::new());
        let mut executor = config.create_executor(evm, ctx);

        executor
            .execute_transaction(recovered)
            .expect("exhausted-quota tx must soft-fail, not hard-error");

        let receipts = executor.receipts();
        assert_eq!(receipts.len(), 1);
        assert!(
            !receipts[0].success,
            "exhausted-quota sponsored tx must produce a status=0 receipt"
        );
        let outbe_failure_addr = outbe_primitives::addresses::ZERO_FEE_POLICY_LOG_ADDRESS;
        let failure_log = receipts[0]
            .logs
            .iter()
            .find(|l| l.address == outbe_failure_addr)
            .expect("soft-failure receipt must carry an OutbeFailure log");
        let code_topic = failure_log.topics().get(1).expect("code topic present");
        let code = u16::from_be_bytes([code_topic.as_slice()[30], code_topic.as_slice()[31]]);
        assert_eq!(code, 110, "quota exhaustion must surface as code 110");

        // No SponsorshipAuthorized event on the failed path.
        let sig_hash = SponsorshipAuthorized::SIGNATURE_HASH;
        assert!(
            !receipts[0]
                .logs
                .iter()
                .any(|l| l.address == ZEROFEE_ADDRESS && l.topics().first() == Some(&sig_hash)),
            "rejected tx must not emit SponsorshipAuthorized"
        );
    }
    // Counter must stay at exactly the limit - no 9th increment.
    // Read LIVE storage (not bundle_state): the rejected tx makes no
    // counter change, so the seeded value only exists in the base
    // state, not in the post-execution change set.
    let slot = zerofee_counter_slot(signer);
    let packed = state
        .storage(ZEROFEE_ADDRESS, slot)
        .expect("counter storage read")
        .saturating_to::<u64>();
    let (day, count) = outbe_zerofee::unpack_counter(packed);
    assert_eq!(day, TODAY);
    assert_eq!(
        count,
        outbe_zerofee::FREE_TX_DAILY_LIMIT,
        "rejected 9th tx must not over-burn the counter"
    );
    // No fee debited on the rejected tx.
    assert_eq!(signer_balance(&mut state, signer), initial_balance);
}

/// F1 executor-level proof: the delegation probe's `code_by_hash`
/// fallback branch is the production steady-state path. For an
/// account whose code was set in a PRIOR block, revm's
/// `State::basic()` returns `info.code == None` (only `code_hash`),
/// so the pre-fee hook must resolve the delegation via
/// `db.code_by_hash(code_hash)`. The other integration tests insert
/// `code: Some(..)` and therefore only exercise the `maybe_code`
/// arm; this test forces the fallback by registering the delegation
/// bytecode in the contracts cache while leaving the account's
/// `code` field `None`.
#[test]
fn eip7702_delegation_detected_via_code_by_hash_fallback() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let recovered = sponsored_test_tx(agent_reward_query_input())
        .try_into_recovered()
        .expect("test-signature must recover");
    let signer = Address::from(*recovered.signer());

    let initial_balance = U256::from(1u64);
    let mut db = cache_db_with_paymaster_account(signer, initial_balance);

    // Recreate the signer account in the STEADY-STATE shape:
    // `code == None`, `code_hash` set, and the delegation bytecode
    // registered in the contracts cache (reachable only via
    // code_by_hash). The first insert (code: Some) registers the
    // contract; the second (code: None) replaces the account entry
    // while leaving the contract in the cache.
    let delegation = Bytecode::new_eip7702(ZEROFEE_ADDRESS);
    let delegation_hash = delegation.hash_slow();
    db.insert_account_info(
        signer,
        AccountInfo {
            balance: initial_balance,
            code_hash: delegation_hash,
            code: Some(delegation),
            ..Default::default()
        },
    );
    db.insert_account_info(
        signer,
        AccountInfo {
            balance: initial_balance,
            code_hash: delegation_hash,
            code: None, // forces the code_by_hash fallback in the probe
            ..Default::default()
        },
    );

    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();

    // Sanity: basic() really returns code == None for this account,
    // so the test genuinely exercises the fallback branch.
    let basic = state
        .basic(signer)
        .expect("basic read")
        .expect("signer account exists");
    assert!(
        basic.code.is_none(),
        "test precondition: signer.code must be None to exercise code_by_hash fallback"
    );

    {
        let evm = config.evm_with_env(&mut state, pectra_evm_env(1));
        let ctx = execution_ctx(Some(1), Bytes::new());
        let mut executor = config.create_executor(evm, ctx);

        executor
            .execute_transaction(recovered)
            .expect("sponsored tx via code_by_hash fallback should execute");

        let receipts = executor.receipts();
        assert_eq!(receipts.len(), 1);
        assert!(
            receipts[0].success,
            "delegation resolved via code_by_hash must take the sponsored path"
        );
        let sig_hash = SponsorshipAuthorized::SIGNATURE_HASH;
        assert!(
            receipts[0]
                .logs
                .iter()
                .any(|l| l.address == ZEROFEE_ADDRESS && l.topics().first() == Some(&sig_hash)),
            "sponsored path must emit SponsorshipAuthorized"
        );
    }
    state.merge_transitions(BundleRetention::Reverts);

    // Counter bumped to 1 and no fee debited - confirms the fallback
    // branch actually routed into the sponsored path.
    let counter = zerofee_counter_for(&mut state, signer);
    assert_eq!(
        outbe_zerofee::unpack_counter(counter).1,
        1,
        "fallback-detected delegation must burn exactly one slot"
    );
    assert_eq!(signer_balance(&mut state, signer), initial_balance);
}

/// Additive-delegation guarantee: a delegated account that sets a tip
/// (`priority_fee > 0`) is NOT requesting sponsorship - its tx must
/// run through the normal fee path (balance debited, no quota burn,
/// no SponsorshipAuthorized event), exactly as if the account were
/// not delegated. This is what lets a signer keep transacting and
/// paying after the daily free quota is exhausted; without the fix
/// the executor soft-failed every non-free-envelope tx from a
/// delegated account, jailing it into free-only mode.
#[test]
fn eip7702_delegated_account_with_priority_fee_pays_normally() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    // Same target/calldata as the sponsored happy path, but with a
    // non-zero priority fee - the "I am paying" signal.
    let paying_tx: reth_ethereum::TransactionSigned = TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: 200_000,
        max_fee_per_gas: 14,
        max_priority_fee_per_gas: 1, // tip > 0 => paying, not sponsored
        to: TxKind::Call(AGENT_REWARD_ADDRESS),
        value: U256::ZERO,
        input: agent_reward_query_input().into(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into();
    let recovered = paying_tx
        .try_into_recovered()
        .expect("test-signature must recover");
    let signer = Address::from(*recovered.signer());

    // Delegated to ZEROFEE, funded with 1 COEN so the normal fee
    // path has balance to debit.
    let initial_balance = U256::from(3_000_000u64);
    let mut state = State::builder()
        .with_database(cache_db_with_paymaster_account(signer, initial_balance))
        .with_bundle_update()
        .build();

    {
        let evm = config.evm_with_env(&mut state, pectra_evm_env(1));
        let ctx = execution_ctx(Some(1), Bytes::new());
        let mut executor = config.create_executor(evm, ctx);

        executor
            .execute_transaction(recovered)
            .expect("paying delegated tx must execute via the normal fee path");

        let receipts = executor.receipts();
        assert_eq!(receipts.len(), 1);
        assert!(
            receipts[0].success,
            "paying delegated tx must succeed as a normal tx"
        );
        // No SponsorshipAuthorized event - this was not a sponsored tx.
        let sig_hash = SponsorshipAuthorized::SIGNATURE_HASH;
        assert!(
            !receipts[0]
                .logs
                .iter()
                .any(|l| l.address == ZEROFEE_ADDRESS && l.topics().first() == Some(&sig_hash)),
            "paying tx must NOT emit SponsorshipAuthorized"
        );
    }
    state.merge_transitions(BundleRetention::Reverts);

    // Fee WAS debited (normal path), and the daily quota counter was
    // NOT touched - the tx never entered the sponsorship branch.
    assert!(
        signer_balance(&mut state, signer) < initial_balance,
        "normal fee path must debit the signer's balance"
    );
    assert_eq!(
        zerofee_counter_for(&mut state, signer),
        0,
        "paying tx must not burn a free-tx slot"
    );
}
