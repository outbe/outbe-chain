use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;

/// Protocol module for `SystemTxKind::EnclaveUpgradeDeadline` (`OSEU`).
pub struct EnclaveUpgradeDeadline;

impl EnclaveUpgradeDeadline {
    /// Jails validators that missed the enclave upgrade deadline.
    pub fn begin_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        enforce_enclave_upgrade_deadline(ctx)
    }
}

#[cfg(feature = "test-utils")]
pub(crate) fn run_validator_liveness(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::active_evm_runtime_host().run_validator_liveness(ctx)
}

/// Applies the canonical post-bootstrap TEE lease deadline to the bounded ACTIVE
/// validator set. The sweep is part of receipt-visible `ValidatorLiveness`, before any
/// user transaction in the block. Block 1 is excluded because its earlier
/// `TeeBootstrap` system transaction creates the founder bindings atomically.
#[cfg(test)]
pub(super) fn enforce_tee_lease_deadlines(ctx: &BlockRuntimeContext) -> Result<usize> {
    crate::active_evm_runtime_host().enforce_tee_lease_deadlines(ctx)
}

pub fn enforce_enclave_upgrade_deadline(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::active_evm_runtime_host().enforce_enclave_upgrade_deadline(ctx)
}
