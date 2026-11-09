use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;

/// Protocol module for `SystemTxKind::EnclaveUpgradeDeadline` (`OSEU`).
pub struct EnclaveUpgradeDeadline;

impl EnclaveUpgradeDeadline {
    /// Jails validators that missed the enclave upgrade deadline.
    pub fn begin_block(
        host: &dyn crate::EvmRuntimeHost,
        ctx: &BlockRuntimeContext<'_>,
    ) -> Result<()> {
        enforce_enclave_upgrade_deadline(host, ctx)
    }
}

/// Applies the canonical post-bootstrap TEE lease deadline to the bounded ACTIVE
/// validator set. The sweep is part of receipt-visible `ValidatorLiveness`, before any
/// user transaction in the block. Block 1 is excluded because its earlier
/// `TeeBootstrap` system transaction creates the founder bindings atomically.
#[cfg(any(test, feature = "test-hooks"))]
pub fn enforce_tee_lease_deadlines(
    host: &dyn crate::EvmRuntimeHost,
    ctx: &BlockRuntimeContext,
) -> Result<usize> {
    host.enforce_tee_lease_deadlines(ctx)
}

pub fn enforce_enclave_upgrade_deadline(
    host: &dyn crate::EvmRuntimeHost,
    ctx: &BlockRuntimeContext,
) -> Result<()> {
    host.enforce_enclave_upgrade_deadline(ctx)
}
