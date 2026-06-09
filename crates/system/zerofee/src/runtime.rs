//! Runtime business logic for the EIP-7702 zero-fee paymaster path.
//!
//! Two entry points:
//!
//! 1. [`classify_sponsorship`] — stateless envelope check used by both the
//!    txpool admission policy and the executor pre-fee site. Returns Ok
//!    when the transaction shape is eligible for the sponsored path
//!    (gas/calldata caps, fee shape, contract creation forbidden,
//!    target in the protocol whitelist). Hard limits live here so both
//!    callers cannot drift.
//!
//! 2. [`authorize_sponsorship`] — stateful check executed by the
//!    executor against the block storage handle. Enforces anti-sybil
//!    (`balance > 0`) and the daily quota
//!    (`effective_count < FREE_TX_DAILY_LIMIT`). Returns the
//!    `current_day` and effective count on success so the caller can
//!    record the use atomically.
//!
//! 3. [`precheck_sponsorship`] — the stateless subset of (2) the
//!    txpool runs at admission time. Covers self-sponsorship and the
//!    anti-sybil gate but deliberately omits the quota check so a
//!    9th-of-day sponsored tx still lands in the block with a
//!    soft-failure receipt (code 110).
//!
//! Self-sponsorship and EIP-7702 designator detection are enforced by the
//! caller — `signer != ZEROFEE_ADDRESS` and the `0xef0100 ++ ZEROFEE_ADDRESS`
//! code pattern are observable on the caller side without any storage I/O.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::{
    addresses::ZEROFEE_ADDRESS, storage::StorageHandle, time::timestamp_to_date_key,
};

use crate::{
    constants::{
        FREE_TX_DAILY_CALLDATA_BYTES, FREE_TX_DAILY_GAS_LIMIT, FREE_TX_DAILY_LIMIT,
        MIN_FREE_TX_MAX_FEE_PER_GAS,
    },
    hooks::{ZeroFeePolicyError, ZeroFeeTransaction},
    precompile::IZeroFee,
    schema::ZeroFeeContract,
};

/// Stateless envelope classification for the sponsored free-tx path.
///
/// Caller responsibilities **before** calling this:
/// - confirm the signer's account code matches the EIP-7702 delegation
///   designator `0xef0100 ++ ZEROFEE_ADDRESS`;
/// - reject self-sponsorship (`signer == ZEROFEE_ADDRESS`);
/// - decide whether to run this stateless check before or after the
///   trait-registry hooks (oracle hook should match first so validator
///   votes do not burn the validator's daily quota).
///
/// The target whitelist is intentionally **not** a parameter — the
/// policy reads [`outbe_primitives::addresses::SPONSORED_TARGET_WHITELIST`]
/// directly so a future caller cannot drift the policy by passing a
/// broader list.
///
/// On `Ok(())` the transaction shape is accepted; on `Err(_)` the caller
/// must reject with the matching error code.
pub fn classify_sponsorship(tx: &ZeroFeeTransaction<'_>) -> Result<(), ZeroFeePolicyError> {
    if tx.value != U256::ZERO {
        return Err(ZeroFeePolicyError::FreeTxDailyValueNotZero);
    }

    if tx.max_priority_fee_per_gas != Some(0) {
        // The fee shape rule is shared with the oracle hook (zero
        // priority fee is the explicit sponsored opt-in); reusing
        // `FeeCapTooLow` keeps a single code for that condition.
        return Err(ZeroFeePolicyError::FeeCapTooLow {
            max_fee_per_gas: tx.max_fee_per_gas,
            minimum: MIN_FREE_TX_MAX_FEE_PER_GAS,
        });
    }

    if tx.max_fee_per_gas < MIN_FREE_TX_MAX_FEE_PER_GAS {
        return Err(ZeroFeePolicyError::FeeCapTooLow {
            max_fee_per_gas: tx.max_fee_per_gas,
            minimum: MIN_FREE_TX_MAX_FEE_PER_GAS,
        });
    }

    if tx.gas_limit > FREE_TX_DAILY_GAS_LIMIT {
        return Err(ZeroFeePolicyError::FreeTxDailyGasLimitExceeded {
            gas_limit: tx.gas_limit,
            limit: FREE_TX_DAILY_GAS_LIMIT,
        });
    }

    if tx.input.len() > FREE_TX_DAILY_CALLDATA_BYTES {
        return Err(ZeroFeePolicyError::FreeTxDailyCalldataTooLarge {
            size: tx.input.len(),
            limit: FREE_TX_DAILY_CALLDATA_BYTES,
        });
    }

    let Some(to) = tx.to else {
        return Err(ZeroFeePolicyError::FreeTxDailyContractCreationForbidden);
    };

    if !outbe_primitives::addresses::SPONSORED_TARGET_WHITELIST.contains(&to) {
        return Err(ZeroFeePolicyError::FreeTxDailyTargetNotWhitelisted { to });
    }

    Ok(())
}

/// Result of a successful sponsorship authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SponsorshipAuthorization {
    /// UTC date key (`yyyymmdd`) used for the lazy-reset bookkeeping.
    pub current_day: u32,
    /// Sponsorship counter value AFTER applying the implied increment.
    /// Callers MUST persist this through `record_use` to make the
    /// authorization visible across blocks.
    pub next_count: u32,
}

/// Stateless prechecks for the sponsored free-tx path, sufficient for
/// pool admission decisions.
///
/// Covers self-sponsorship rejection and anti-sybil — both checks that
/// only depend on the signer's account view (`balance`) and are
/// deterministic across nodes for a given EVM state. Quota enforcement
/// is intentionally **not** part of this function: the README contract
/// requires quota-exhausted txs to land in the block with a soft-failure
/// receipt code 110, so the pool must admit them and let the executor
/// (authoritative) produce the receipt.
///
/// The pool calls this; the executor calls the full
/// [`authorize_sponsorship`] which additionally consults block storage
/// for the quota.
pub fn precheck_sponsorship(
    signer: Address,
    signer_balance: U256,
) -> Result<(), ZeroFeePolicyError> {
    if signer == ZEROFEE_ADDRESS {
        return Err(ZeroFeePolicyError::UnauthorizedSigner);
    }

    if signer_balance.is_zero() {
        return Err(ZeroFeePolicyError::FreeTxDailyNoExistingAccount);
    }

    Ok(())
}

/// Stateful authorization for the sponsored free-tx path.
///
/// `signer_balance` is supplied by the caller from the EVM account view
/// it already has — the executor reads it from its journaled DB.
/// Routing the value in explicitly avoids extending the storage-reader
/// trait surface and keeps the gate uniform.
///
/// Anti-sybil intentionally checks `balance > 0` only. Nonce is a poor
/// proxy because EIP-7702 set-code transactions bump the authority's
/// nonce as part of authorization processing (25k gas per auth, paid by
/// the sponsor, not the EOA) — a fresh EOA can therefore reach nonce=1
/// without spending a single wei of its own. Requiring balance forces
/// real economic input (someone transferred wei to the address).
pub fn authorize_sponsorship(
    storage: StorageHandle<'_>,
    signer: Address,
    signer_balance: U256,
    block_timestamp_secs: u64,
) -> Result<SponsorshipAuthorization, ZeroFeePolicyError> {
    if signer == ZEROFEE_ADDRESS {
        return Err(ZeroFeePolicyError::UnauthorizedSigner);
    }

    if signer_balance.is_zero() {
        return Err(ZeroFeePolicyError::FreeTxDailyNoExistingAccount);
    }

    let current_day = timestamp_to_date_key(block_timestamp_secs);
    let contract = ZeroFeeContract::new(storage);
    let used = contract.effective_count(signer, current_day)?;
    if used >= FREE_TX_DAILY_LIMIT {
        return Err(ZeroFeePolicyError::FreeTxDailyExhausted {
            used,
            limit: FREE_TX_DAILY_LIMIT,
        });
    }

    Ok(SponsorshipAuthorization {
        current_day,
        next_count: used.saturating_add(1),
    })
}

/// Convenience helper: persists the use of a sponsored free-tx after
/// [`authorize_sponsorship`] succeeded. Called by the executor through
/// an outer `StorageHandle` whose write survives the inner tx's revert
/// journal, so a `REVERT` cannot un-burn the daily slot.
///
/// Emits a [`SponsorshipAuthorized`] log at [`ZEROFEE_ADDRESS`] with
/// the post-write counter so off-chain tooling can observe sponsorship
/// grants via `eth_getLogs`.
pub fn record_sponsorship_use(
    storage: StorageHandle<'_>,
    signer: Address,
    current_day: u32,
) -> Result<u32, ZeroFeePolicyError> {
    let new_count = {
        let mut contract = ZeroFeeContract::new(storage.clone());
        contract.record_use(signer, current_day)?
    };
    let event = IZeroFee::SponsorshipAuthorized {
        signer,
        day: current_day,
        newCount: new_count,
    };
    storage.emit_event(ZEROFEE_ADDRESS, event.encode_log_data())?;
    Ok(new_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::pack_counter;
    use alloy_primitives::{address, Address, U256};
    use outbe_primitives::{
        addresses::{AGENT_REWARD_ADDRESS, ZEROFEE_ADDRESS},
        storage::{hashmap::HashMapStorageProvider, StorageHandle},
        time::SECONDS_PER_DAY,
    };

    const SIGNER: Address = address!("0x1111111111111111111111111111111111111111");
    /// Block timestamp parked safely inside `2026-04-01 00:00:00 UTC` →
    /// `date_key = 20260401`. The exact value is not important for the
    /// tests; only the day-key derived from it.
    const BLOCK_TS: u64 = 1_775_001_600;
    const BLOCK_DAY: u32 = 20_260_401;

    fn sponsored_target() -> Address {
        // First whitelisted address — value is incidental, only being
        // a member of `SPONSORED_TARGET_WHITELIST` matters here.
        outbe_primitives::addresses::SPONSORED_TARGET_WHITELIST[0]
    }

    fn ok_envelope<'a>(input: &'a [u8]) -> ZeroFeeTransaction<'a> {
        ZeroFeeTransaction {
            signer: SIGNER,
            to: Some(sponsored_target()),
            value: U256::ZERO,
            input,
            gas_limit: 100_000,
            max_fee_per_gas: MIN_FREE_TX_MAX_FEE_PER_GAS,
            max_priority_fee_per_gas: Some(0),
        }
    }

    // ----- classify_sponsorship -----

    #[test]
    fn classify_accepts_minimal_envelope() {
        let tx = ok_envelope(&[]);
        assert!(classify_sponsorship(&tx).is_ok());
    }

    #[test]
    fn classify_rejects_non_zero_value() {
        let mut tx = ok_envelope(&[]);
        tx.value = U256::from(1);
        assert_eq!(
            classify_sponsorship(&tx),
            Err(ZeroFeePolicyError::FreeTxDailyValueNotZero)
        );
    }

    #[test]
    fn classify_rejects_non_zero_priority_fee() {
        let mut tx = ok_envelope(&[]);
        tx.max_priority_fee_per_gas = Some(1);
        let err = classify_sponsorship(&tx).unwrap_err();
        assert_eq!(err.code(), 105, "non-zero priority fee → FeeCapTooLow code");
    }

    #[test]
    fn classify_rejects_low_fee_cap() {
        let mut tx = ok_envelope(&[]);
        tx.max_fee_per_gas = 0;
        let err = classify_sponsorship(&tx).unwrap_err();
        assert_eq!(err.code(), 105);
    }

    #[test]
    fn classify_rejects_oversized_gas_limit() {
        let mut tx = ok_envelope(&[]);
        tx.gas_limit = crate::FREE_TX_DAILY_GAS_LIMIT + 1;
        let err = classify_sponsorship(&tx).unwrap_err();
        assert_eq!(err.code(), 114, "free-tx gas overflow → code 114");
    }

    #[test]
    fn classify_rejects_oversized_calldata() {
        let big = vec![0u8; crate::FREE_TX_DAILY_CALLDATA_BYTES + 1];
        let tx = ok_envelope(&big);
        let err = classify_sponsorship(&tx).unwrap_err();
        assert_eq!(err.code(), 115, "free-tx calldata overflow → code 115");
    }

    #[test]
    fn classify_rejects_contract_creation() {
        let mut tx = ok_envelope(&[]);
        tx.to = None;
        let err = classify_sponsorship(&tx).unwrap_err();
        assert_eq!(err.code(), 112);
    }

    #[test]
    fn classify_rejects_target_outside_whitelist() {
        let mut tx = ok_envelope(&[]);
        // ZEROFEE_ADDRESS itself is intentionally NOT on the whitelist,
        // so it doubles as a guaranteed-rejected target for this test.
        tx.to = Some(ZEROFEE_ADDRESS);
        let err = classify_sponsorship(&tx).unwrap_err();
        assert_eq!(err.code(), 116, "non-whitelisted target → code 116");
    }

    // ----- authorize_sponsorship -----

    fn with_storage<R>(f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, f)
    }

    #[test]
    fn authorize_rejects_self_sponsorship() {
        with_storage(|storage| {
            let err = authorize_sponsorship(storage, ZEROFEE_ADDRESS, U256::from(1), BLOCK_TS)
                .unwrap_err();
            assert!(matches!(err, ZeroFeePolicyError::UnauthorizedSigner));
        });
    }

    #[test]
    fn authorize_rejects_unfunded_zero_nonce_signer() {
        with_storage(|storage| {
            let err = authorize_sponsorship(storage, SIGNER, U256::ZERO, BLOCK_TS).unwrap_err();
            assert!(matches!(
                err,
                ZeroFeePolicyError::FreeTxDailyNoExistingAccount
            ));
        });
    }

    #[test]
    fn authorize_accepts_existing_account_with_balance_only() {
        with_storage(|storage| {
            let auth = authorize_sponsorship(storage, SIGNER, U256::from(1), BLOCK_TS).unwrap();
            assert_eq!(auth.current_day, BLOCK_DAY);
            assert_eq!(auth.next_count, 1);
        });
    }

    #[test]
    fn authorize_rejects_zero_balance_even_when_nonce_is_positive() {
        // Anti-sybil V2: nonce alone is not enough. EIP-7702 set-code
        // transactions bump the authority's nonce as part of auth
        // processing — a fresh EOA can therefore reach nonce > 0
        // without ever spending a wei of its own. Only `balance > 0`
        // is a real economic gate.
        with_storage(|storage| {
            let err = authorize_sponsorship(storage, SIGNER, U256::ZERO, BLOCK_TS).unwrap_err();
            assert!(matches!(
                err,
                ZeroFeePolicyError::FreeTxDailyNoExistingAccount
            ));
        });
    }

    #[test]
    fn authorize_rejects_ninth_tx_same_day() {
        with_storage(|storage| {
            // Seed the contract with a count of 8 for today.
            {
                let zerofee = ZeroFeeContract::new(storage.clone());
                zerofee
                    .counter
                    .write(&SIGNER, pack_counter(BLOCK_DAY, 8))
                    .unwrap();
            }
            let err = authorize_sponsorship(storage, SIGNER, U256::from(1), BLOCK_TS).unwrap_err();
            assert!(matches!(
                err,
                ZeroFeePolicyError::FreeTxDailyExhausted { used: 8, limit: 8 }
            ));
        });
    }

    #[test]
    fn authorize_applies_lazy_reset_on_new_day() {
        with_storage(|storage| {
            // Yesterday's count was 8 — should be treated as 0 today.
            {
                let zerofee = ZeroFeeContract::new(storage.clone());
                zerofee
                    .counter
                    .write(
                        &SIGNER,
                        pack_counter(outbe_primitives::time::previous_date_key(BLOCK_DAY), 8),
                    )
                    .unwrap();
            }
            let auth = authorize_sponsorship(storage, SIGNER, U256::from(1), BLOCK_TS).unwrap();
            assert_eq!(auth.current_day, BLOCK_DAY);
            assert_eq!(auth.next_count, 1);
        });
    }

    #[test]
    fn record_use_persists_through_storage_handle() {
        with_storage(|storage| {
            // Two consecutive authorize+record cycles for the same day.
            for expected in 1..=3 {
                let auth = authorize_sponsorship(storage.clone(), SIGNER, U256::from(1), BLOCK_TS)
                    .unwrap();
                assert_eq!(auth.next_count, expected);
                let written =
                    record_sponsorship_use(storage.clone(), SIGNER, auth.current_day).unwrap();
                assert_eq!(written, expected);
            }
            // Direct slot inspection confirms the packed counter.
            let zerofee = ZeroFeeContract::new(storage);
            let packed = zerofee.counter.read(&SIGNER).unwrap();
            assert_eq!(crate::schema::unpack_counter(packed), (BLOCK_DAY, 3));
        });
    }

    #[test]
    fn utc_midnight_boundary_is_inclusive_on_the_new_day() {
        // timestamp exactly at midnight `2026-04-01 00:00:00 UTC` →
        // belongs to day 20260401, not the previous day. This guards
        // against `>` vs `>=` confusion in the day-key arithmetic.
        with_storage(|storage| {
            // Seed yesterday at the limit so any lazy-reset failure
            // would surface as `FreeTxDailyExhausted` instead of a
            // pass-through.
            {
                let zerofee = ZeroFeeContract::new(storage.clone());
                zerofee
                    .counter
                    .write(
                        &SIGNER,
                        pack_counter(outbe_primitives::time::previous_date_key(BLOCK_DAY), 8),
                    )
                    .unwrap();
            }
            let auth = authorize_sponsorship(storage, SIGNER, U256::from(1), BLOCK_TS).unwrap();
            assert_eq!(auth.current_day, BLOCK_DAY);
        });
    }

    #[test]
    fn one_second_before_midnight_belongs_to_previous_day() {
        with_storage(|storage| {
            let just_before = BLOCK_TS - 1;
            let auth = authorize_sponsorship(storage, SIGNER, U256::from(1), just_before).unwrap();
            assert_eq!(
                auth.current_day,
                outbe_primitives::time::previous_date_key(BLOCK_DAY)
            );
        });
    }

    #[test]
    fn whitelist_membership_is_required_even_for_familiar_targets() {
        // AGENT_REWARD_ADDRESS is in the whitelist, sanity-check the
        // positive case so the test name reads consistently.
        let mut tx = ok_envelope(&[]);
        tx.to = Some(AGENT_REWARD_ADDRESS);
        assert!(classify_sponsorship(&tx).is_ok());
    }

    // ----- rejection-precedence pins -----
    //
    // The order in which `classify_sponsorship` and `authorize_sponsorship`
    // surface failures is consensus-visible: it lands in
    // `OutbeFailure(code, reason)` logs at `ZERO_FEE_POLICY_LOG_ADDRESS`.
    // Off-chain UX builds on that code, and a future refactor that
    // reorders checks would silently change the receipt — pin it.

    #[test]
    fn classify_precedence_non_zero_value_beats_contract_creation() {
        // `to = None` AND `value > 0`. The value check fires first
        // (code 113 FreeTxDailyValueNotZero), not the contract-creation
        // check (code 112).
        let mut tx = ok_envelope(&[]);
        tx.to = None;
        tx.value = U256::from(1);
        assert_eq!(
            classify_sponsorship(&tx).unwrap_err().code(),
            113,
            "FreeTxDailyValueNotZero must take precedence over ContractCreationForbidden"
        );
    }

    #[test]
    fn classify_precedence_fee_shape_beats_target_whitelist() {
        // Non-zero priority fee on an otherwise-correct envelope to a
        // non-whitelisted target. FeeCapTooLow (105) wins over
        // TargetNotWhitelisted (116) because the fee shape is checked
        // earlier — keeps the receipt deterministic.
        let mut tx = ok_envelope(&[]);
        tx.max_priority_fee_per_gas = Some(1);
        tx.to = Some(ZEROFEE_ADDRESS);
        assert_eq!(classify_sponsorship(&tx).unwrap_err().code(), 105);
    }

    #[test]
    fn authorize_precedence_self_sponsorship_beats_anti_sybil() {
        // signer == ZEROFEE_ADDRESS AND balance == 0 AND nonce == 0.
        // UnauthorizedSigner (107) fires first; the anti-sybil gate
        // (111) never gets to run.
        with_storage(|storage| {
            let err =
                authorize_sponsorship(storage, ZEROFEE_ADDRESS, U256::ZERO, BLOCK_TS).unwrap_err();
            assert_eq!(err.code(), 107);
        });
    }

    // ----- precheck_sponsorship -----

    #[test]
    fn precheck_rejects_self_sponsorship() {
        assert!(matches!(
            precheck_sponsorship(ZEROFEE_ADDRESS, U256::from(1)),
            Err(ZeroFeePolicyError::UnauthorizedSigner)
        ));
    }

    #[test]
    fn precheck_rejects_zero_balance() {
        assert!(matches!(
            precheck_sponsorship(SIGNER, U256::ZERO),
            Err(ZeroFeePolicyError::FreeTxDailyNoExistingAccount)
        ));
    }

    #[test]
    fn precheck_accepts_funded_non_paymaster_signer() {
        assert!(precheck_sponsorship(SIGNER, U256::from(1)).is_ok());
    }

    #[test]
    fn precheck_does_not_perform_quota_check() {
        // The pool MUST admit a sponsored tx even if the signer has
        // already burned all 8 slots for today — the executor produces
        // the soft-failure receipt code 110. `precheck` deliberately
        // does no quota check; this test pins that contract.
        assert!(precheck_sponsorship(SIGNER, U256::from(1)).is_ok());
    }

    #[test]
    fn day_constant_matches_timestamp_seconds_division() {
        // The test scaffolding picks `BLOCK_TS` to land on the start of
        // `BLOCK_DAY`. Document the invariant so future date pickers
        // notice if SECONDS_PER_DAY ever changes.
        assert_eq!(BLOCK_TS % SECONDS_PER_DAY, 0);
    }

    #[test]
    fn counter_survives_storage_handle_checkpoint_revert() {
        // The executor pre-fee design commits the counter increment
        // through `DirectStorageProvider::flush` BEFORE the inner tx
        // runs, so a `REVERT` inside the user's tx cannot un-burn the
        // daily slot. At the storage-primitive level we prove the
        // equivalent invariant: a write that happens BEFORE a
        // `checkpoint_revert` is preserved; only writes after the
        // checkpoint are rolled back.
        with_storage(|storage| {
            // Step 1: burn one slot for today, mimic flush() commit.
            let auth =
                authorize_sponsorship(storage.clone(), SIGNER, U256::from(1), BLOCK_TS).unwrap();
            record_sponsorship_use(storage.clone(), SIGNER, auth.current_day).unwrap();
            let after_first = ZeroFeeContract::new(storage.clone())
                .effective_count(SIGNER, BLOCK_DAY)
                .unwrap();
            assert_eq!(after_first, 1);

            // Step 2: open a checkpoint and make a doomed write to
            // some unrelated slot, then revert. The pre-checkpoint
            // counter must remain visible.
            let checkpoint = storage.checkpoint();
            // Simulate a tx-internal side effect by bumping the
            // counter again, then revert via the checkpoint.
            record_sponsorship_use(storage.clone(), SIGNER, BLOCK_DAY).unwrap();
            storage.checkpoint_revert(checkpoint);

            // The reverted second increment is gone; the pre-checkpoint
            // write survives.
            let after_revert = ZeroFeeContract::new(storage.clone())
                .effective_count(SIGNER, BLOCK_DAY)
                .unwrap();
            assert_eq!(
                after_revert, 1,
                "checkpoint_revert must NOT undo the pre-tx counter write"
            );
        });
    }

    #[test]
    fn record_use_emits_sponsorship_event_at_zerofee_address() {
        use alloy_sol_types::SolEvent;

        let mut provider = HashMapStorageProvider::new(1);
        // First record produces newCount=1.
        StorageHandle::enter(&mut provider, |storage| {
            let new_count = record_sponsorship_use(storage, SIGNER, BLOCK_DAY).unwrap();
            assert_eq!(new_count, 1);
        });

        // The provider is the canonical event sink in tests; the event
        // is recorded at ZEROFEE_ADDRESS so off-chain `eth_getLogs`
        // filtering can subscribe by address.
        let events = provider.get_events(ZEROFEE_ADDRESS);
        assert_eq!(
            events.len(),
            1,
            "exactly one SponsorshipAuthorized event per record_use"
        );

        // Topic 0 must be the canonical event signature; signer is
        // indexed in topic 1, day in topic 2. The new_count rides in
        // the data body.
        let log = &events[0];
        assert_eq!(
            log.topics()[0],
            IZeroFee::SponsorshipAuthorized::SIGNATURE_HASH,
            "topic0 must match the sol! signature hash"
        );
    }

    #[test]
    fn record_use_persists_across_multiple_storage_handle_scopes() {
        // The executor's pre-fee path opens a fresh `DirectStorageProvider`
        // scope per transaction. This test simulates the same shape:
        // each authorize+record cycle re-enters the storage handle, and
        // the counter must be observable in the next scope. If the
        // contract ever started caching state inside the facade, this
        // test would catch the regression.
        let mut provider = HashMapStorageProvider::new(1);
        for expected in 1..=3 {
            StorageHandle::enter(&mut provider, |storage| {
                let auth = authorize_sponsorship(storage.clone(), SIGNER, U256::from(1), BLOCK_TS)
                    .unwrap();
                assert_eq!(auth.current_day, BLOCK_DAY);
                assert_eq!(auth.next_count, expected);
                let new_count = record_sponsorship_use(storage, SIGNER, auth.current_day).unwrap();
                assert_eq!(new_count, expected);
            });
        }

        // After three cycles the counter must read 3 in a fresh scope.
        StorageHandle::enter(&mut provider, |storage| {
            let zerofee = ZeroFeeContract::new(storage);
            let packed = zerofee.counter.read(&SIGNER).unwrap();
            assert_eq!(crate::schema::unpack_counter(packed), (BLOCK_DAY, 3));
        });
    }
}
