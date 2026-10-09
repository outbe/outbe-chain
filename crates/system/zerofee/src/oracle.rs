use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
use alloy_primitives::Address;
use outbe_primitives::{addresses::ORACLE_ADDRESS, storage::StorageHandle};

use crate::envelope::{active_validator_for_role, ZeroFeeEnvelope};
use crate::hooks::ZeroFeePolicyError;
use outbe_validatorset::delegation::ValidatorDelegateRole;

/// Maximum calldata bytes accepted for a zero-fee oracle vote.
pub const MAX_ZERO_FEE_ORACLE_CALLDATA_BYTES: usize = 16 * 1024;

/// Maximum gas limit accepted for a zero-fee oracle vote.
pub const MAX_ZERO_FEE_ORACLE_GAS_LIMIT: u64 = 1_500_000;

/// Minimum EIP-1559 fee cap accepted by Reth's public txpool.
pub const MIN_ZERO_FEE_ORACLE_MAX_FEE_PER_GAS: u128 = MIN_PROTOCOL_BASE_FEE as u128;

/// Stateless envelope of a zero-fee oracle vote.
pub(crate) const VOTE_ENVELOPE: ZeroFeeEnvelope = ZeroFeeEnvelope {
    target: ORACLE_ADDRESS,
    min_max_fee_per_gas: MIN_ZERO_FEE_ORACLE_MAX_FEE_PER_GAS,
    max_calldata_bytes: MAX_ZERO_FEE_ORACLE_CALLDATA_BYTES,
    max_gas_limit: MAX_ZERO_FEE_ORACLE_GAS_LIMIT,
    malformed_reason: "submitVote(ExchangeRateTuple[]) decode failed",
};

/// The signer must be an active validator or its oracle delegate, and that
/// validator must not have voted in this period.
pub(crate) fn validate_oracle_submit_vote_state(
    storage: StorageHandle,
    signer: Address,
) -> Result<Address, ZeroFeePolicyError> {
    let validator =
        active_validator_for_role(storage.clone(), signer, ValidatorDelegateRole::Oracle)?;

    let oracle = outbe_oracle::schema::OracleContract::new(storage);
    if oracle.vote_exists.read(&validator)? {
        return Err(ZeroFeePolicyError::AlreadyVoted);
    }

    Ok(validator)
}
