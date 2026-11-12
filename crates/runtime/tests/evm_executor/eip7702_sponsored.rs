use super::eip7702_support::*;
use super::*;
use outbe_primitives::addresses::{AGENT_REWARD_ADDRESS, ZEROFEE_ADDRESS};
use outbe_zerofee::precompile::IZeroFee::SponsorshipAuthorized;

/// Happy path: the executor pre-fee hook admits a sponsored tx with
/// `value=0`, `priority_fee=0`, `to in whitelist`. The tx executes under
/// zero-fee cfg overrides and produces a receipt with a
/// `SponsorshipAuthorized` log. The signer's balance is untouched, and
/// ZEROFEE_ADDRESS' counter slot is bumped to `(today, 1)`.
#[test]
fn eip7702_sponsored_tx_burns_quota_and_emits_event() {
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
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
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
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
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
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
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
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
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
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
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
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
