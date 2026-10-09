//! Runtime business logic for the EIP-7702 zero-fee paymaster path.
//!
//! Three entry points:
//!
//! 1. [`classify_sponsorship`] - stateless envelope check used by both the
//!    txpool admission policy and the executor pre-fee site. Returns Ok
//!    when the transaction shape is eligible for the sponsored path
//!    (gas/calldata caps, fee shape, contract creation forbidden,
//!    target in the protocol whitelist). Hard limits live here so both
//!    callers cannot drift.
//!
//! 2. [`authorize_sponsorship`] - stateful check that the executor runs
//!    against the block storage handle. Enforces the daily quota
//!    (`effective_count < FREE_TX_DAILY_LIMIT`). Returns the
//!    `current_day` and effective count on success so the caller can
//!    record the use atomically.
//!
//! 3. [`precheck_sponsorship`] - the stateless subset of (2) the
//!    txpool runs at admission time. Covers self-sponsorship but
//!    deliberately omits the quota check so a
//!    9th-of-day sponsored tx still lands in the block with a
//!    soft-failure receipt (code 110).
//!
//! `precheck_sponsorship` and `authorize_sponsorship` reject self-sponsorship
//! inside this module. `classify_sponsorship` leaves that check to the caller.
//! The caller detects the EIP-7702 designator
//! `0xef0100 ++ ZEROFEE_ADDRESS` without storage I/O.

use core::fmt;

use alloy_eips::eip7702::SignedAuthorization;
use alloy_primitives::{Address, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_ocomp_protocol::transaction_call::TransactionCallFields;
use outbe_primitives::{
    addresses::{TRIBUTE_FACTORY_ADDRESS, ZEROFEE_ADDRESS},
    storage::StorageHandle,
    time::timestamp_to_date_key,
};

use crate::{
    constants::{
        FREE_TX_BOOTSTRAP_GAS_LIMIT, FREE_TX_DAILY_CALLDATA_BYTES, FREE_TX_DAILY_GAS_LIMIT,
        FREE_TX_DAILY_LIMIT, FREE_TX_TRIBUTE_FACTORY_GAS_LIMIT, MIN_FREE_TX_MAX_FEE_PER_GAS,
    },
    hooks::{ZeroFeePolicyError, ZeroFeeTransaction},
    precompile::IZeroFee,
    schema::ZeroFeeContract,
};

/// Execution-layer independent view of a possible first ZeroFee delegation.
#[derive(Clone, Copy)]
pub struct BootstrapTransactionView<'a> {
    /// Recovered signer of the outer EIP-7702 transaction.
    pub signer: Address,
    /// Chain ID carried by the outer transaction.
    pub tx_chain_id: Option<u64>,
    /// Chain ID of the block being validated.
    pub network_chain_id: u64,
    /// Outer transaction nonce before REVM increments the sender.
    pub nonce: u64,
    /// Call fields of the outer transaction.
    pub call: TransactionCallFields<'a>,
    /// Whether the EIP-2930 access list is empty.
    pub access_list_empty: bool,
    /// Signed EIP-7702 authorization list.
    pub authorization_list: &'a [SignedAuthorization],
}

/// The text form keeps the call fields flat, as before they moved to `call`.
impl fmt::Debug for BootstrapTransactionView<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = f.debug_struct("BootstrapTransactionView");
        out.field("signer", &self.signer)
            .field("tx_chain_id", &self.tx_chain_id)
            .field("network_chain_id", &self.network_chain_id)
            .field("nonce", &self.nonce);
        self.call.debug_fields(&mut out);
        out.field("access_list_empty", &self.access_list_empty)
            .field("authorization_list", &self.authorization_list)
            .finish()
    }
}

/// A transaction whose signed shape exactly matches the bootstrap protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapCandidate {
    /// Self-authorizing account.
    pub signer: Address,
    /// Expected pre-state account nonce.
    pub nonce: u64,
}

/// Minimal pre-state account view required to authorize a bootstrap waiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapAccountView {
    /// Native balance before transaction execution.
    pub balance: U256,
    /// Account nonce before transaction execution.
    pub nonce: u64,
    /// Whether the account has no deployed or delegated code.
    pub code_empty: bool,
}

/// Classifies the exact signed EIP-7702 bootstrap envelope.
///
/// This function deliberately returns `None` for a non-match. Callers must
/// then continue through normal paid-transaction validation. They must not
/// turn a merely similar transaction into a new consensus-visible ZeroFee
/// failure class.
pub fn classify_bootstrap(tx: &BootstrapTransactionView<'_>) -> Option<BootstrapCandidate> {
    if !tx.has_bootstrap_envelope() {
        return None;
    }
    tx.self_authorization()?;

    let expected_input = IZeroFee::authorizeSponsorshipCall { signer: tx.signer }.abi_encode();
    (tx.call.input == expected_input.as_slice()).then_some(BootstrapCandidate {
        signer: tx.signer,
        nonce: tx.nonce,
    })
}

impl BootstrapTransactionView<'_> {
    /// The outer transaction calls ZeroFee on this network, with the bootstrap
    /// fee shape and without value or access list.
    fn has_bootstrap_envelope(&self) -> bool {
        self.targets_zerofee_on_network() && self.has_bootstrap_fee_shape() && self.is_bare_call()
    }

    fn targets_zerofee_on_network(&self) -> bool {
        self.tx_chain_id == Some(self.network_chain_id) && self.call.to == Some(ZEROFEE_ADDRESS)
    }

    fn has_bootstrap_fee_shape(&self) -> bool {
        let fee_cap_ok = self.call.max_fee_per_gas >= MIN_FREE_TX_MAX_FEE_PER_GAS;
        self.call.gas_limit <= FREE_TX_BOOTSTRAP_GAS_LIMIT
            && fee_cap_ok
            && self.call.max_priority_fee_per_gas == Some(0)
    }

    fn is_bare_call(&self) -> bool {
        self.call.value == U256::ZERO && self.access_list_empty
    }

    /// Returns the only authorization when the signer uses it to delegate
    /// itself to ZeroFee on this network at the next account nonce.
    ///
    /// Signature recovery runs last because it is the most expensive check.
    fn self_authorization(&self) -> Option<&SignedAuthorization> {
        let [authorization] = self.authorization_list else {
            return None;
        };
        let authorization_nonce = self.nonce.checked_add(1)?;
        let delegates_to_zerofee = authorization.chain_id == U256::from(self.network_chain_id)
            && authorization.address == ZEROFEE_ADDRESS;
        if !delegates_to_zerofee || authorization.nonce != authorization_nonce {
            return None;
        }
        (authorization.recover_authority().ok()? == self.signer).then_some(authorization)
    }
}

/// Rechecks the stateful positive-balance, empty-code bootstrap requirements.
pub fn authorize_bootstrap(candidate: BootstrapCandidate, account: BootstrapAccountView) -> bool {
    !account.balance.is_zero() && account.nonce == candidate.nonce && account.code_empty
}

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
/// The target whitelist is intentionally **not** a parameter. The
/// policy reads [`outbe_primitives::zero_fee::SPONSORED_TARGET_WHITELIST`]
/// directly, so a future caller cannot drift the policy by passing a
/// broader list.
///
/// On `Ok(())`, the policy accepts the transaction shape. On `Err(_)`, the
/// caller must reject with the matching error code.
pub fn classify_sponsorship(tx: &ZeroFeeTransaction<'_>) -> Result<(), ZeroFeePolicyError> {
    if tx.call.value != U256::ZERO {
        return Err(ZeroFeePolicyError::FreeTxDailyValueNotZero);
    }

    // The oracle hook shares the fee shape rule (zero priority fee is the
    // explicit sponsored opt-in). A wrong priority fee and a low fee cap both
    // use `FeeCapTooLow`, so the condition keeps a single code.
    if tx.call.max_priority_fee_per_gas != Some(0)
        || tx.call.max_fee_per_gas < MIN_FREE_TX_MAX_FEE_PER_GAS
    {
        return Err(ZeroFeePolicyError::FeeCapTooLow {
            max_fee_per_gas: tx.call.max_fee_per_gas,
            minimum: MIN_FREE_TX_MAX_FEE_PER_GAS,
        });
    }

    check_sponsored_budget(tx)?;
    check_sponsored_target(tx.call.to)
}

/// Checks the gas limit and the calldata size of a sponsored transaction.
///
/// The TributeFactory has a larger gas limit than every other target.
fn check_sponsored_budget(tx: &ZeroFeeTransaction<'_>) -> Result<(), ZeroFeePolicyError> {
    let gas_limit = if tx.call.to == Some(TRIBUTE_FACTORY_ADDRESS) {
        FREE_TX_TRIBUTE_FACTORY_GAS_LIMIT
    } else {
        FREE_TX_DAILY_GAS_LIMIT
    };
    if tx.call.gas_limit > gas_limit {
        return Err(ZeroFeePolicyError::FreeTxDailyGasLimitExceeded {
            gas_limit: tx.call.gas_limit,
            limit: gas_limit,
        });
    }

    if tx.call.input.len() > FREE_TX_DAILY_CALLDATA_BYTES {
        return Err(ZeroFeePolicyError::FreeTxDailyCalldataTooLarge {
            size: tx.call.input.len(),
            limit: FREE_TX_DAILY_CALLDATA_BYTES,
        });
    }
    Ok(())
}

/// Rejects contract creation and every target outside the sponsored whitelist.
fn check_sponsored_target(to: Option<Address>) -> Result<(), ZeroFeePolicyError> {
    let Some(to) = to else {
        return Err(ZeroFeePolicyError::FreeTxDailyContractCreationForbidden);
    };

    if !outbe_primitives::zero_fee::SPONSORED_TARGET_WHITELIST.contains(&to) {
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
/// Covers self-sponsorship rejection. Native balance is deliberately not
/// an eligibility signal: ZeroFee exists so an otherwise valid address can
/// transact when its spendable COEN balance is exactly zero. Quota enforcement
/// is intentionally **not** part of this function. The protocol contract
/// requires quota-exhausted txs to land in the block with a soft-failure
/// receipt code 110. So the pool must admit them and let the executor
/// (authoritative) produce the receipt.
///
/// The pool calls this function. The executor calls the full
/// [`authorize_sponsorship`], which also reads block storage for the
/// quota.
pub fn precheck_sponsorship(signer: Address) -> Result<(), ZeroFeePolicyError> {
    if signer == ZEROFEE_ADDRESS {
        return Err(ZeroFeePolicyError::UnauthorizedSigner);
    }

    Ok(())
}

/// Stateful authorization for the sponsored free-tx path.
///
pub fn authorize_sponsorship(
    storage: StorageHandle<'_>,
    signer: Address,
    block_timestamp_secs: u64,
) -> Result<SponsorshipAuthorization, ZeroFeePolicyError> {
    if signer == ZEROFEE_ADDRESS {
        return Err(ZeroFeePolicyError::UnauthorizedSigner);
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
/// [`authorize_sponsorship`] succeeded. The executor calls it through
/// an outer `StorageHandle` whose write survives the inner tx's revert
/// journal. So a `REVERT` cannot un-burn the daily slot.
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
    use alloy_eips::eip7702::Authorization;
    use alloy_primitives::{address, Address, Signature, U256};
    use alloy_sol_types::SolCall;
    use outbe_primitives::{
        addresses::{AGENT_REWARD_ADDRESS, ZEROFEE_ADDRESS},
        storage::{hashmap::HashMapStorageProvider, StorageHandle},
        time::SECONDS_PER_DAY,
    };

    const SIGNER: Address = address!("0x1111111111111111111111111111111111111111");
    /// Block timestamp parked safely inside `2026-04-01 00:00:00 UTC` ->
    /// `date_key = 20260401`. The exact value is not important for the
    /// tests. Only the day-key derived from it is important.
    const BLOCK_TS: u64 = 1_775_001_600;
    const BLOCK_DAY: u32 = 20_260_401;

    /// The call fields of a valid self-authorization bootstrap transaction.
    fn bootstrap_call_fields(input: &[u8]) -> TransactionCallFields<'_> {
        TransactionCallFields {
            to: Some(ZEROFEE_ADDRESS),
            value: U256::ZERO,
            input,
            gas_limit: FREE_TX_BOOTSTRAP_GAS_LIMIT,
            max_fee_per_gas: MIN_FREE_TX_MAX_FEE_PER_GAS,
            max_priority_fee_per_gas: Some(0),
        }
    }

    #[test]
    fn classify_bootstrap_accepts_exact_self_authorization() {
        let nonce = 7;
        let chain_id = 31_337;
        let authorization = Authorization {
            chain_id: U256::from(chain_id),
            address: ZEROFEE_ADDRESS,
            nonce: nonce + 1,
        }
        .into_signed(Signature::test_signature());
        let signer = authorization.recover_authority().unwrap();
        let input = IZeroFee::authorizeSponsorshipCall { signer }.abi_encode();
        let authorizations = [authorization];
        let tx = BootstrapTransactionView {
            signer,
            tx_chain_id: Some(chain_id),
            network_chain_id: chain_id,
            nonce,
            call: bootstrap_call_fields(&input),
            access_list_empty: true,
            authorization_list: &authorizations,
        };

        assert_eq!(
            classify_bootstrap(&tx),
            Some(BootstrapCandidate { signer, nonce })
        );
    }

    #[test]
    fn classify_bootstrap_rejects_every_noncanonical_outer_field() {
        let nonce = 7;
        let chain_id = 31_337;
        let authorization = Authorization {
            chain_id: U256::from(chain_id),
            address: ZEROFEE_ADDRESS,
            nonce: nonce + 1,
        }
        .into_signed(Signature::test_signature());
        let signer = authorization.recover_authority().unwrap();
        let input = IZeroFee::authorizeSponsorshipCall { signer }.abi_encode();
        let authorizations = [authorization];
        let exact = BootstrapTransactionView {
            signer,
            tx_chain_id: Some(chain_id),
            network_chain_id: chain_id,
            nonce,
            call: bootstrap_call_fields(&input),
            access_list_empty: true,
            authorization_list: &authorizations,
        };

        let mut invalid = exact;
        invalid.tx_chain_id = None;
        assert_eq!(classify_bootstrap(&invalid), None);
        invalid = exact;
        invalid.call.to = Some(Address::ZERO);
        assert_eq!(classify_bootstrap(&invalid), None);
        invalid = exact;
        invalid.call.value = U256::from(1);
        assert_eq!(classify_bootstrap(&invalid), None);
        invalid = exact;
        invalid.call.input = &[];
        assert_eq!(classify_bootstrap(&invalid), None);
        invalid = exact;
        invalid.call.gas_limit = FREE_TX_BOOTSTRAP_GAS_LIMIT + 1;
        assert_eq!(classify_bootstrap(&invalid), None);
        invalid = exact;
        invalid.call.max_fee_per_gas = MIN_FREE_TX_MAX_FEE_PER_GAS - 1;
        assert_eq!(classify_bootstrap(&invalid), None);
        invalid = exact;
        invalid.call.max_priority_fee_per_gas = Some(1);
        assert_eq!(classify_bootstrap(&invalid), None);
        invalid = exact;
        invalid.access_list_empty = false;
        assert_eq!(classify_bootstrap(&invalid), None);
    }

    fn classify_test_authorizations(
        signer: Address,
        nonce: u64,
        chain_id: u64,
        authorizations: &[alloy_eips::eip7702::SignedAuthorization],
    ) -> Option<BootstrapCandidate> {
        let input = IZeroFee::authorizeSponsorshipCall { signer }.abi_encode();
        classify_bootstrap(&BootstrapTransactionView {
            signer,
            tx_chain_id: Some(chain_id),
            network_chain_id: chain_id,
            nonce,
            call: bootstrap_call_fields(&input),
            access_list_empty: true,
            authorization_list: authorizations,
        })
    }

    #[test]
    fn classify_bootstrap_rejects_noncanonical_authorization_list() {
        let nonce = 7;
        let chain_id = 31_337;

        let wildcard = Authorization {
            chain_id: U256::ZERO,
            address: ZEROFEE_ADDRESS,
            nonce: nonce + 1,
        }
        .into_signed(Signature::test_signature());
        let wildcard_signer = wildcard.recover_authority().unwrap();
        assert_eq!(
            classify_test_authorizations(wildcard_signer, nonce, chain_id, &[wildcard]),
            None
        );

        let stale = Authorization {
            chain_id: U256::from(chain_id),
            address: ZEROFEE_ADDRESS,
            nonce,
        }
        .into_signed(Signature::test_signature());
        let stale_signer = stale.recover_authority().unwrap();
        assert_eq!(
            classify_test_authorizations(stale_signer, nonce, chain_id, &[stale]),
            None
        );

        let wrong_target = Authorization {
            chain_id: U256::from(chain_id),
            address: Address::ZERO,
            nonce: nonce + 1,
        }
        .into_signed(Signature::test_signature());
        let wrong_target_signer = wrong_target.recover_authority().unwrap();
        assert_eq!(
            classify_test_authorizations(wrong_target_signer, nonce, chain_id, &[wrong_target]),
            None
        );

        let exact = Authorization {
            chain_id: U256::from(chain_id),
            address: ZEROFEE_ADDRESS,
            nonce: nonce + 1,
        }
        .into_signed(Signature::test_signature());
        let exact_signer = exact.recover_authority().unwrap();
        assert_eq!(
            classify_test_authorizations(
                exact_signer,
                nonce,
                chain_id,
                &[exact.clone(), exact.clone()],
            ),
            None
        );
        assert_eq!(
            classify_test_authorizations(Address::ZERO, nonce, chain_id, &[exact]),
            None
        );
    }

    #[test]
    fn authorize_bootstrap_accepts_one_atomic_unit_with_empty_code() {
        let nonce = 7;
        assert!(authorize_bootstrap(
            BootstrapCandidate {
                signer: SIGNER,
                nonce,
            },
            BootstrapAccountView {
                balance: U256::from(1),
                nonce,
                code_empty: true,
            },
        ));
    }

    #[test]
    fn authorize_bootstrap_rejects_zero_balance() {
        assert!(!authorize_bootstrap(
            BootstrapCandidate {
                signer: SIGNER,
                nonce: 7,
            },
            BootstrapAccountView {
                balance: U256::ZERO,
                nonce: 7,
                code_empty: true,
            },
        ));
    }

    #[test]
    fn authorize_bootstrap_rejects_stale_nonce_or_existing_code() {
        let candidate = BootstrapCandidate {
            signer: SIGNER,
            nonce: 7,
        };
        assert!(!authorize_bootstrap(
            candidate,
            BootstrapAccountView {
                balance: U256::from(1),
                nonce: 8,
                code_empty: true,
            },
        ));
        assert!(!authorize_bootstrap(
            candidate,
            BootstrapAccountView {
                balance: U256::from(1),
                nonce: 7,
                code_empty: false,
            },
        ));
    }

    fn sponsored_target() -> Address {
        // First whitelisted address. The value is incidental. Only
        // membership in `SPONSORED_TARGET_WHITELIST` matters here.
        outbe_primitives::zero_fee::SPONSORED_TARGET_WHITELIST[0]
    }

    fn ok_envelope<'a>(input: &'a [u8]) -> ZeroFeeTransaction<'a> {
        ZeroFeeTransaction {
            signer: SIGNER,
            call: TransactionCallFields {
                to: Some(sponsored_target()),
                value: U256::ZERO,
                input,
                gas_limit: 100_000,
                max_fee_per_gas: MIN_FREE_TX_MAX_FEE_PER_GAS,
                max_priority_fee_per_gas: Some(0),
            },
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
        tx.call.value = U256::from(1);
        assert_eq!(
            classify_sponsorship(&tx),
            Err(ZeroFeePolicyError::FreeTxDailyValueNotZero)
        );
    }

    #[test]
    fn classify_rejects_non_zero_priority_fee() {
        let mut tx = ok_envelope(&[]);
        tx.call.max_priority_fee_per_gas = Some(1);
        let err = classify_sponsorship(&tx).unwrap_err();
        assert_eq!(
            err.code(),
            105,
            "non-zero priority fee -> FeeCapTooLow code"
        );
    }

    #[test]
    fn classify_rejects_low_fee_cap() {
        let mut tx = ok_envelope(&[]);
        tx.call.max_fee_per_gas = 0;
        let err = classify_sponsorship(&tx).unwrap_err();
        assert_eq!(err.code(), 105);
    }

    #[test]
    fn classify_rejects_oversized_gas_limit() {
        let mut tx = ok_envelope(&[]);
        tx.call.gas_limit = crate::FREE_TX_DAILY_GAS_LIMIT + 1;
        let err = classify_sponsorship(&tx).unwrap_err();
        assert_eq!(err.code(), 114, "free-tx gas overflow -> code 114");
    }

    #[test]
    fn classify_accepts_tribute_factory_zk_gas_limit() {
        let mut tx = ok_envelope(&[]);
        tx.call.to = Some(TRIBUTE_FACTORY_ADDRESS);
        tx.call.gas_limit = crate::FREE_TX_TRIBUTE_FACTORY_GAS_LIMIT;

        assert!(classify_sponsorship(&tx).is_ok());
    }

    #[test]
    fn classify_rejects_tribute_factory_above_zk_gas_limit() {
        let mut tx = ok_envelope(&[]);
        tx.call.to = Some(TRIBUTE_FACTORY_ADDRESS);
        tx.call.gas_limit = crate::FREE_TX_TRIBUTE_FACTORY_GAS_LIMIT + 1;

        assert_eq!(
            classify_sponsorship(&tx),
            Err(ZeroFeePolicyError::FreeTxDailyGasLimitExceeded {
                gas_limit: crate::FREE_TX_TRIBUTE_FACTORY_GAS_LIMIT + 1,
                limit: crate::FREE_TX_TRIBUTE_FACTORY_GAS_LIMIT,
            })
        );
    }

    #[test]
    fn classify_rejects_oversized_calldata() {
        let big = vec![0u8; crate::FREE_TX_DAILY_CALLDATA_BYTES + 1];
        let tx = ok_envelope(&big);
        let err = classify_sponsorship(&tx).unwrap_err();
        assert_eq!(err.code(), 115, "free-tx calldata overflow -> code 115");
    }

    #[test]
    fn classify_rejects_contract_creation() {
        let mut tx = ok_envelope(&[]);
        tx.call.to = None;
        let err = classify_sponsorship(&tx).unwrap_err();
        assert_eq!(err.code(), 112);
    }

    #[test]
    fn classify_rejects_target_outside_whitelist() {
        let mut tx = ok_envelope(&[]);
        // ZEROFEE_ADDRESS itself is intentionally NOT on the whitelist,
        // so it doubles as a guaranteed-rejected target for this test.
        tx.call.to = Some(ZEROFEE_ADDRESS);
        let err = classify_sponsorship(&tx).unwrap_err();
        assert_eq!(err.code(), 116, "non-whitelisted target -> code 116");
    }

    // ----- authorize_sponsorship -----

    fn with_storage<R>(f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, f)
    }

    #[test]
    fn authorize_rejects_self_sponsorship() {
        with_storage(|storage| {
            let err = authorize_sponsorship(storage, ZEROFEE_ADDRESS, BLOCK_TS).unwrap_err();
            assert!(matches!(err, ZeroFeePolicyError::UnauthorizedSigner));
        });
    }

    #[test]
    fn authorize_accepts_non_paymaster_signer() {
        with_storage(|storage| {
            let auth = authorize_sponsorship(storage, SIGNER, BLOCK_TS).unwrap();
            assert_eq!(auth.current_day, BLOCK_DAY);
            assert_eq!(auth.next_count, 1);
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
            let err = authorize_sponsorship(storage, SIGNER, BLOCK_TS).unwrap_err();
            assert!(matches!(
                err,
                ZeroFeePolicyError::FreeTxDailyExhausted { used: 8, limit: 8 }
            ));
        });
    }

    #[test]
    fn authorize_applies_lazy_reset_on_new_day() {
        with_storage(|storage| {
            // Yesterday's count was 8. The policy should treat it as 0 today.
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
            let auth = authorize_sponsorship(storage, SIGNER, BLOCK_TS).unwrap();
            assert_eq!(auth.current_day, BLOCK_DAY);
            assert_eq!(auth.next_count, 1);
        });
    }

    #[test]
    fn record_use_persists_through_storage_handle() {
        with_storage(|storage| {
            // Two consecutive authorize+record cycles for the same day.
            for expected in 1..=3 {
                let auth = authorize_sponsorship(storage.clone(), SIGNER, BLOCK_TS).unwrap();
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
        // timestamp exactly at midnight `2026-04-01 00:00:00 UTC` ->
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
            let auth = authorize_sponsorship(storage, SIGNER, BLOCK_TS).unwrap();
            assert_eq!(auth.current_day, BLOCK_DAY);
        });
    }

    #[test]
    fn one_second_before_midnight_belongs_to_previous_day() {
        with_storage(|storage| {
            let just_before = BLOCK_TS - 1;
            let auth = authorize_sponsorship(storage, SIGNER, just_before).unwrap();
            assert_eq!(
                auth.current_day,
                outbe_primitives::time::previous_date_key(BLOCK_DAY)
            );
        });
    }

    #[test]
    fn whitelist_membership_is_required_even_for_familiar_targets() {
        // AGENT_REWARD_ADDRESS is in the whitelist. Sanity-check the
        // positive case so the test name reads consistently.
        let mut tx = ok_envelope(&[]);
        tx.call.to = Some(AGENT_REWARD_ADDRESS);
        assert!(classify_sponsorship(&tx).is_ok());
    }

    // ----- rejection-precedence pins -----
    //
    // The order in which `classify_sponsorship` and `authorize_sponsorship`
    // surface failures is consensus-visible. It lands in
    // `OutbeFailure(code, reason)` logs at `ZERO_FEE_POLICY_LOG_ADDRESS`.
    // Off-chain UX builds on that code. A future refactor that reorders
    // checks would silently change the receipt, so pin the order.

    #[test]
    fn classify_precedence_non_zero_value_beats_contract_creation() {
        // `to = None` AND `value > 0`. The value check fires first
        // (code 113 FreeTxDailyValueNotZero), not the contract-creation
        // check (code 112).
        let mut tx = ok_envelope(&[]);
        tx.call.to = None;
        tx.call.value = U256::from(1);
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
        // TargetNotWhitelisted (116) because the policy checks the fee
        // shape earlier. This order keeps the receipt deterministic.
        let mut tx = ok_envelope(&[]);
        tx.call.max_priority_fee_per_gas = Some(1);
        tx.call.to = Some(ZEROFEE_ADDRESS);
        assert_eq!(classify_sponsorship(&tx).unwrap_err().code(), 105);
    }

    #[test]
    fn authorize_rejects_paymaster_address_regardless_of_account_state() {
        with_storage(|storage| {
            let err = authorize_sponsorship(storage, ZEROFEE_ADDRESS, BLOCK_TS).unwrap_err();
            assert_eq!(err.code(), 107);
        });
    }

    // ----- precheck_sponsorship -----

    #[test]
    fn precheck_rejects_self_sponsorship() {
        assert!(matches!(
            precheck_sponsorship(ZEROFEE_ADDRESS),
            Err(ZeroFeePolicyError::UnauthorizedSigner)
        ));
    }

    #[test]
    fn precheck_accepts_non_paymaster_signer() {
        // This address-only policy has no account balance or quota input.
        // Account-state coverage belongs to the pool/executor integration tests.
        assert!(precheck_sponsorship(SIGNER).is_ok());
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
        // runs. So a `REVERT` inside the user's tx cannot un-burn the
        // daily slot. At the storage-primitive level, this test proves
        // the equivalent invariant: a write that happens BEFORE a
        // `checkpoint_revert` is preserved. Only writes after the
        // checkpoint are rolled back.
        with_storage(|storage| {
            // Step 1: burn one slot for today. This mimics the flush() commit.
            let auth = authorize_sponsorship(storage.clone(), SIGNER, BLOCK_TS).unwrap();
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

            // The reverted second increment is gone. The pre-checkpoint
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

        // The provider is the canonical event sink in tests. The event
        // is recorded at ZEROFEE_ADDRESS so off-chain `eth_getLogs`
        // filtering can subscribe by address.
        let events = provider.get_events(ZEROFEE_ADDRESS);
        assert_eq!(
            events.len(),
            1,
            "exactly one SponsorshipAuthorized event per record_use"
        );

        // Topic 0 must be the canonical event signature. Topic 1 holds
        // the indexed signer. Topic 2 holds the day. The new_count rides
        // in the data body.
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
        // scope per transaction. This test simulates the same shape.
        // Each authorize+record cycle re-enters the storage handle. The
        // counter must be observable in the next scope. If the
        // contract ever started caching state inside the facade, this
        // test would catch the regression.
        let mut provider = HashMapStorageProvider::new(1);
        for expected in 1..=3 {
            StorageHandle::enter(&mut provider, |storage| {
                let auth = authorize_sponsorship(storage.clone(), SIGNER, BLOCK_TS).unwrap();
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
