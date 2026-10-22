use super::super::*;

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
/// We don't care WHICH address it is. We only need it to be stable across
/// runs, and we attach delegation + balance + nonce to it.
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

#[test]
fn bootstrap_execution_restores_existing_fee_flags_on_every_result_exit() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    for flags in 0..8u8 {
        for outcome in ["included", "declined", "error"] {
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
            db.insert_account_info(
                signer,
                AccountInfo {
                    balance: U256::from(1),
                    ..Default::default()
                },
            );
            let mut state = State::builder()
                .with_database(db)
                .with_bundle_update()
                .build();
            let expected = (flags & 1 != 0, flags & 2 != 0, flags & 4 != 0);
            let mut env = pectra_evm_env(1);
            env.cfg_env.disable_balance_check = expected.0;
            env.cfg_env.disable_base_fee = expected.1;
            env.cfg_env.disable_fee_charge = expected.2;
            if outcome == "error" {
                // The waiver is authorized, but the inner executor rejects
                // the transaction against the available block gas.
                env.block_env.gas_limit = 0;
            }
            {
                let evm = config.evm_with_env(&mut state, env);
                let mut executor =
                    config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));
                let result = executor.execute_transaction_with_commit_condition(recovered, |_| {
                    if outcome == "declined" {
                        CommitChanges::No
                    } else {
                        CommitChanges::Yes
                    }
                });
                match outcome {
                    "included" => {
                        assert!(result.unwrap().is_some());
                        assert_eq!(executor.receipts().len(), 1);
                        assert!(executor.receipts()[0].success);
                    }
                    "declined" => {
                        assert!(result.unwrap().is_none());
                        assert!(executor.receipts().is_empty());
                    }
                    "error" => {
                        assert!(result.is_err());
                        assert!(executor.receipts().is_empty());
                    }
                    _ => unreachable!(),
                }
                let cfg = &executor.inner.evm.ctx_mut().cfg;
                assert_eq!(
                    (
                        cfg.disable_balance_check,
                        cfg.disable_base_fee,
                        cfg.disable_fee_charge
                    ),
                    expected,
                    "flags={flags}, outcome={outcome}"
                );
                assert_eq!(
                    executor.current_execution_summary().validator_fee_sum,
                    U256::ZERO
                );
            }
            let account = state.basic(signer).unwrap().unwrap();
            assert_eq!(account.balance, U256::from(1));
            assert_eq!(account.nonce, if outcome == "included" { 2 } else { 0 });
            assert_eq!(
                account.code.and_then(|code| code.eip7702_address()),
                if outcome == "included" {
                    Some(ZEROFEE_ADDRESS)
                } else {
                    None
                }
            );
            assert_eq!(zerofee_counter_for(&mut state, signer), 0);
        }
    }
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

/// Happy path: the executor pre-fee hook admits a sponsored tx with
/// `value=0`, `priority_fee=0`, `to in whitelist`. The tx executes under
/// zero-fee cfg overrides and produces a receipt with a
/// `SponsorshipAuthorized` log. The signer's balance is untouched, and
/// ZEROFEE_ADDRESS' counter slot is bumped to `(today, 1)`.
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

        // Find the SponsorshipAuthorized log on the receipt. This
        // is the guarantee. Topic[0] must match the event sig
        // hash. The signer is topic[1] indexed.
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

    // Balance must be exactly what we put in, with no fee debit. This
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
/// sponsored path. The tx goes through the normal fee path. With
/// `priority_fee = 0` and signer's balance below the gas cost, the
/// EVM `disable_balance_check` would normally let it through. We
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
        // small gas). But signer's code points to ORACLE, so
        // the pre-fee hook leaves it to the normal path. The normal
        // path requires balance to cover `gas_limit * max_fee_per_gas`.
        // 2 COEN (2_000_000 unit) covers it at the protocol fee floor,
        // so this succeeds. The key assertion is that the executor emits NO
        // SponsorshipAuthorized log and the counter stays at 0.
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

    // Counter must remain at 0. Delegation to a foreign address burns
    // no quota.
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

/// F2/code-110 executor-level proof: the signer has already burned all
/// 8 slots for today. The pre-fee hook does NOT reject a 9th sponsored
/// tx as a hard error. Instead:
/// - the tx lands in the block with a `status=0` receipt carrying
///   `OutbeFailure(110)`.
/// - the counter stays at 8 (no over-burn).
/// - no balance is debited.
///
/// This is the exact contract the README promises and the txpool relies
/// on (pool admits, executor produces the soft-failure).
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
    // Counter must stay at exactly the limit, with no 9th increment.
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
/// arm. This test forces the fallback by registering the delegation
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
    // contract. The second (code: None) replaces the account entry
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

    // Counter bumped to 1 and no fee debited. This confirms that the
    // fallback branch actually routed into the sponsored path.
    let counter = zerofee_counter_for(&mut state, signer);
    assert_eq!(
        outbe_zerofee::unpack_counter(counter).1,
        1,
        "fallback-detected delegation must burn exactly one slot"
    );
    assert_eq!(signer_balance(&mut state, signer), initial_balance);
}

/// Additive-delegation guarantee: a delegated account that sets a tip
/// (`priority_fee > 0`) is NOT requesting sponsorship. Its tx must
/// run through the normal fee path (balance debited, no quota burn,
/// no SponsorshipAuthorized event), exactly as if the account were
/// not delegated. This lets a signer keep transacting and
/// paying after the daily free quota is exhausted. Without the fix,
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
        // No SponsorshipAuthorized event. This was not a sponsored tx.
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
    // NOT touched. The tx never entered the sponsorship branch.
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
