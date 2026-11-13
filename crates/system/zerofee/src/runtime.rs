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

use alloy_eips::eip7702::SignedAuthorization;
use alloy_primitives::{Address, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_primitives::{addresses::ZEROFEE_ADDRESS, storage::StorageHandle};
use outbe_system_common::time::timestamp_to_date_key;

use crate::{
    constants::{
        ELEVATED_SPONSORED_GAS, FREE_TX_BOOTSTRAP_GAS_LIMIT, FREE_TX_DAILY_CALLDATA_BYTES,
        FREE_TX_DAILY_GAS_LIMIT, FREE_TX_DAILY_LIMIT, MIN_FREE_TX_MAX_FEE_PER_GAS,
    },
    hooks::{ZeroFeePolicyError, ZeroFeeTransaction},
    precompile::IZeroFee,
    schema::ZeroFeeContract,
};

pub use outbe_primitives::zero_fee_types::{
    BootstrapAccountView, BootstrapCandidate, BootstrapTransactionView, SponsorshipAuthorization,
};

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

trait BootstrapEnvelope {
    fn has_bootstrap_envelope(&self) -> bool;
    fn targets_zerofee_on_network(&self) -> bool;
    fn has_bootstrap_fee_shape(&self) -> bool;
    fn is_bare_call(&self) -> bool;
    fn self_authorization(&self) -> Option<&SignedAuthorization>;
}

impl BootstrapEnvelope for BootstrapTransactionView<'_> {
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
/// policy reads [`outbe_system_common::zero_fee::SPONSORED_TARGET_WHITELIST`]
/// directly, so a future caller cannot drift the policy by passing a
/// broader list.
///
/// On `Ok(())`, the policy accepts the transaction shape for sponsorship.
/// `Err(_)` means sponsorship ineligibility, not unconditional transaction
/// rejection. Current txpool and executor callers retain normal paid fallback.
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
/// The `(target, selector)` selects an elevated ceiling, or the default when
/// no grant matches. Argument decoding remains the target's responsibility.
fn check_sponsored_budget(tx: &ZeroFeeTransaction<'_>) -> Result<(), ZeroFeePolicyError> {
    let selector = tx
        .call
        .input
        .get(..4)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok());
    let gas_limit = ELEVATED_SPONSORED_GAS
        .iter()
        .find(|(target, granted_selector, _)| {
            tx.call.to == Some(*target) && selector == Some(*granted_selector)
        })
        .map_or(FREE_TX_DAILY_GAS_LIMIT, |(_, _, limit)| *limit);
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

    if !outbe_system_common::zero_fee::SPONSORED_TARGET_WHITELIST.contains(&to) {
        return Err(ZeroFeePolicyError::FreeTxDailyTargetNotWhitelisted { to });
    }

    Ok(())
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
/// # Quota epoch and timestamp drift
///
/// The quota epoch is the UTC day key of `block_timestamp_secs`. The block
/// proposer supplies this header timestamp. The protocol bounds it to a
/// deterministic band relative to the parent timestamp:
/// - The lower bound is
///   [`MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS`](outbe_primitives::consensus::MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS)
///   (1 s).
/// - The upper bound is
///   [`MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS`](outbe_primitives::consensus::MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS)
///   (1 h).
///
/// Validators enforce this inclusive band with
/// `validate_against_parent_timestamp_millis` in
/// `crates/blockchain/execution/src/consensus/policy.rs` (`outbe-execution`). A parent at
/// height zero skips only the minimum advance. Strict monotonicity and the
/// maximum still apply. This function does not validate the timestamp. It
/// relies on the surrounding block-validation flow.
///
/// The maximum is parent-relative, not a wall-clock or cumulative bound. A
/// proposer can cross an upcoming UTC midnight within one permitted advance.
/// Consecutive blocks can accumulate larger advances. Along a valid monotonic
/// chain, each day boundary resets a signer's quota at most once. This policy
/// accepts that early rollover rather than preventing it.
///
/// The epoch intentionally stays on the UTC timestamp day key. Thus it matches
/// the `getCounter` view, the `day` field of the
/// [`SponsorshipAuthorized`](IZeroFee::SponsorshipAuthorized) event, and the
/// day-indexed emission schedule. A unit test guards that the drift bound
/// stays below one day.
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

    Ok(SponsorshipAuthorization::from_parts(
        signer,
        current_day,
        used.saturating_add(1),
    ))
}

/// Consumes an authorization and records its signer's sponsored transaction.
///
/// The executor calls this only after inclusion, including an included revert.
/// A failed authorization, execution error, or declined commit must not call it.
/// Callers must keep authorization and recording within the same sequential
/// transaction flow; this token does not reserve quota or isolate storage.
///
/// Emits a [`SponsorshipAuthorized`](IZeroFee::SponsorshipAuthorized) log at
/// [`ZEROFEE_ADDRESS`] with the post-write counter so off-chain tooling can
/// observe sponsorship grants via `eth_getLogs`.
///
/// An authorization cannot be reused for another quota increment:
///
/// ```compile_fail
/// use outbe_primitives::storage::StorageHandle;
/// use outbe_zerofee::{record_sponsorship_use, SponsorshipAuthorization};
///
/// fn reuse(storage: StorageHandle<'_>, auth: SponsorshipAuthorization) {
///     let _ = record_sponsorship_use(storage.clone(), auth);
///     let _ = record_sponsorship_use(storage, auth);
/// }
/// ```
pub fn record_sponsorship_use(
    storage: StorageHandle<'_>,
    authorization: SponsorshipAuthorization,
) -> Result<u32, ZeroFeePolicyError> {
    let signer = authorization.signer();
    let current_day = authorization.current_day();
    let new_count = {
        let mut contract = ZeroFeeContract::new(storage.clone());
        contract.record_use(&authorization)?
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
mod tests;
