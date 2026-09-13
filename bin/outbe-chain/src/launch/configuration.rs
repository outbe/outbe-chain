pub(crate) fn validate_adr005_node_mode(
    is_validator: bool,
    has_certified_upstream: bool,
) -> eyre::Result<()> {
    if !is_validator && !has_certified_upstream {
        eyre::bail!(
            "ADR-005 plain EL full-node mode is disabled: use --upstream so historical execution is gated by exact finalized-parent projection readiness"
        );
    }
    Ok(())
}

/// Configure Reth's engine tree for Outbe's pre-finalization parent switches.
///
/// Ethereum's Engine API permits an execution client to skip payload building when
/// an FCU selects an already-canonical ancestor. Outbe leaders intentionally build
/// on certified, not-yet-finalized parents, so a later view may select such an
/// ancestor and still require a payload. Reth exposes both parts of that behavior
/// explicitly: process the attributes and unwind the canonical header to the
/// selected parent before starting the payload job.
pub(crate) fn configure_outbe_engine_args(engine: &mut reth_node_core::args::EngineArgs) {
    engine.always_process_payload_attributes_on_canonical_head = true;
    engine.allow_unwind_canonical_header = true;
}

/// Outbe's transaction-pool defaults, installed before CLI parsing so operator
/// `--txpool.*` flags still override them.
///
/// Rationale (2026-08-22 incident): a transaction that keeps landing in
/// proposals which fail to finalize is re-injected by the reorg path and stays
/// pending indefinitely. Two upstream defaults made that worse:
///
/// - `--txpool.lifetime` (parked sub-pools) defaults to 3 hours - far longer
///   than any legitimate parked transaction needs on a two-second chain.
/// - RPC-submitted transactions are treated as "local" and are exempt from
///   lifetime eviction. The incident transactions arrived over public RPC, so
///   the exemption applied to exactly the traffic that must be evictable.
///
/// The transactions backup journal is disabled for the same reason: a restart
/// must not resurrect transactions the node deliberately evicted.
pub(crate) fn outbe_default_txpool_values() -> reth_node_core::args::DefaultTxPoolValues {
    reth_node_core::args::DefaultTxPoolValues::default()
        .with_max_queued_lifetime(OUTBE_TXPOOL_QUEUED_LIFETIME)
        .with_no_locals(OUTBE_TXPOOL_NO_LOCALS)
        .with_disable_transactions_backup(OUTBE_TXPOOL_DISABLE_BACKUP)
}

/// Parked-transaction lifetime. Reth's own default is three hours - orders of
/// magnitude longer than a two-second chain needs.
const OUTBE_TXPOOL_QUEUED_LIFETIME: std::time::Duration = std::time::Duration::from_secs(120);

/// RPC-submitted transactions must NOT be exempt from lifetime eviction.
const OUTBE_TXPOOL_NO_LOCALS: bool = true;

/// A restart must not resurrect transactions the node deliberately evicted.
const OUTBE_TXPOOL_DISABLE_BACKUP: bool = true;
