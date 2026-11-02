use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
use outbe_primitives::addresses::HYPERLANE_CONTROLLER_ADDRESS;

use crate::envelope::ZeroFeeEnvelope;

/// Maximum calldata bytes accepted for a zero-fee checkpoint submission.
pub const MAX_ZERO_FEE_CHECKPOINT_CALLDATA_BYTES: usize = 1024;

/// Maximum gas limit accepted for a zero-fee checkpoint submission.
pub const MAX_ZERO_FEE_CHECKPOINT_GAS_LIMIT: u64 = 1_500_000;

/// Minimum EIP-1559 fee cap accepted by Reth's public txpool.
pub const MIN_ZERO_FEE_CHECKPOINT_MAX_FEE_PER_GAS: u128 = MIN_PROTOCOL_BASE_FEE as u128;

/// Stateless envelope of a zero-fee checkpoint submission.
pub(crate) const CHECKPOINT_ENVELOPE: ZeroFeeEnvelope = ZeroFeeEnvelope {
    target: HYPERLANE_CONTROLLER_ADDRESS,
    min_max_fee_per_gas: MIN_ZERO_FEE_CHECKPOINT_MAX_FEE_PER_GAS,
    max_calldata_bytes: MAX_ZERO_FEE_CHECKPOINT_CALLDATA_BYTES,
    max_gas_limit: MAX_ZERO_FEE_CHECKPOINT_GAS_LIMIT,
    malformed_reason: "submitCheckpoint(uint32,bytes32,uint32,bytes32,bytes) decode failed",
};
