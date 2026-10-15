//! EIP-7702 sponsorship admission. `classify_sponsorship` rejects shape violations with
//! the codes the executor uses; `precheck_sponsorship` rejects self-sponsorship and reads
//! no balance or quota.

use alloy_primitives::{address, Address, U256};
use outbe_primitives::addresses::ZEROFEE_ADDRESS;
use outbe_zerofee::{classify_sponsorship, fixtures::sponsored_envelope, precheck_sponsorship};

const NON_VALIDATOR_SIGNER: Address = address!("0x9999999999999999999999999999999999999999");

#[test]
fn pool_classify_accepts_well_formed_sponsored_envelope() {
    assert!(classify_sponsorship(&sponsored_envelope(NON_VALIDATOR_SIGNER, &[])).is_ok());
}

#[test]
fn pool_classify_rejects_non_zero_value_with_plan_code_113() {
    let mut tx = sponsored_envelope(NON_VALIDATOR_SIGNER, &[]);
    tx.call.value = U256::from(1);
    let err = classify_sponsorship(&tx).unwrap_err();
    assert_eq!(err.code(), 113);
}

#[test]
fn pool_classify_rejects_oversized_gas_with_plan_code_114() {
    let mut tx = sponsored_envelope(NON_VALIDATOR_SIGNER, &[]);
    tx.call.gas_limit = outbe_zerofee::FREE_TX_DAILY_GAS_LIMIT + 1;
    let err = classify_sponsorship(&tx).unwrap_err();
    assert_eq!(err.code(), 114);
}

#[test]
fn pool_classify_rejects_oversized_calldata_with_plan_code_115() {
    let big = vec![0u8; outbe_zerofee::FREE_TX_DAILY_CALLDATA_BYTES + 1];
    let tx = sponsored_envelope(NON_VALIDATOR_SIGNER, &big);
    let err = classify_sponsorship(&tx).unwrap_err();
    assert_eq!(err.code(), 115);
}

#[test]
fn pool_classify_rejects_target_outside_whitelist_with_plan_code_116() {
    let mut tx = sponsored_envelope(NON_VALIDATOR_SIGNER, &[]);
    // ZEROFEE_ADDRESS itself is not in the SPONSORED_TARGET_WHITELIST.
    tx.call.to = Some(ZEROFEE_ADDRESS);
    let err = classify_sponsorship(&tx).unwrap_err();
    assert_eq!(err.code(), 116);
}

#[test]
fn pool_precheck_rejects_self_sponsorship_with_code_107() {
    let err = precheck_sponsorship(ZEROFEE_ADDRESS).unwrap_err();
    assert_eq!(err.code(), 107);
}

#[test]
fn pool_precheck_accepts_non_paymaster_signer() {
    assert!(precheck_sponsorship(NON_VALIDATOR_SIGNER).is_ok());
}
