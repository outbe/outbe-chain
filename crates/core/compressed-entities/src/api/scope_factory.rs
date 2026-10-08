//! Construct block execution scopes and finalized RPC scopes.

use super::*;

pub(super) fn empty_scope(
    parent_tree: Option<Arc<dyn AuthenticatedParentTree>>,
    ce_work_config: CeWorkConfig,
) -> ExecutionScope {
    ExecutionScope {
        phase: AtomicU8::new(PHASE_BEFORE_BEGIN),
        explicit_gas_charged: AtomicU64::new(0),
        explicit_gas_window_active: AtomicBool::new(false),
        explicit_gas_window_start: AtomicU64::new(0),
        explicit_gas_window_limit: AtomicU64::new(0),
        parent_tree: Mutex::new(parent_tree),
        parent_tree_factory: Mutex::new(None),
        parent_identity_without_root: Mutex::new(None),
        parent_binding_configured: AtomicBool::new(false),
        rpc_read_only: AtomicBool::new(false),
        provisional_seal: Mutex::new(None),
        completed_seal: Mutex::new(None),
        ce_work_config,
        ce_work: Mutex::new(CeWorkState {
            used: 0,
            seen_keys: BTreeSet::new(),
            transaction_start: None,
        }),
        ce_work_failure: AtomicU8::new(CE_WORK_FAILURE_NONE),
    }
}

#[must_use]
pub fn with_parent_tree(
    parent_tree: Arc<dyn AuthenticatedParentTree>,
    ce_work_config: CeWorkConfig,
) -> ExecutionScope {
    let mut scope = empty_scope(Some(parent_tree), ce_work_config);
    scope.parent_binding_configured = AtomicBool::new(true);
    scope
}

#[must_use]
pub fn with_parent_tree_factory(
    factory: Arc<dyn AuthenticatedParentTreeFactory>,
    commitment_scheme_version: u32,
    parent_block_number: u64,
    parent_block_hash: B256,
    ce_work_config: CeWorkConfig,
) -> ExecutionScope {
    let mut scope = empty_scope(None, ce_work_config);
    scope.parent_tree_factory = Mutex::new(Some(factory));
    scope.parent_identity_without_root = Mutex::new(Some((
        commitment_scheme_version,
        parent_block_number,
        parent_block_hash,
    )));
    scope.parent_binding_configured = AtomicBool::new(true);
    scope
}

/// Construct a finalized RPC scope. The executor binds its block parent before begin-block.
#[must_use]
pub fn for_finalized_rpc(
    factory: Arc<dyn AuthenticatedParentTreeFactory>,
    commitment_scheme_version: u32,
    block_number: u64,
    block_hash: B256,
) -> ExecutionScope {
    let mut scope = with_parent_tree_factory(
        factory,
        commitment_scheme_version,
        block_number,
        block_hash,
        CeWorkConfig::new(0, 0, u64::MAX),
    );
    scope.parent_binding_configured = AtomicBool::new(false);
    scope.rpc_read_only = AtomicBool::new(true);
    scope
}
