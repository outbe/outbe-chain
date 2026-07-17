use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex,
    },
};

use alloy_primitives::{Address, B256};
use outbe_common::WorldwideDay;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::{
    errors::ParentBodySourceError, lifecycle, runtime, EntityId36, NodBucketBodyV1, NodItemBodyV1,
    StoredBody, TributeBodyV1,
};

/// Fork-fixed upper bound shared by execution merge logic and parent adapters.
pub const MAX_ID_PAGE_LIMIT: u32 = 1_024;

/// One of the fork-fixed compressed-body namespaces.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EntityRef {
    Tribute(EntityId36),
    NodItem(EntityId36),
    NodBucket(EntityId36),
}

/// Exact-parent authenticated leaf reader owned by one block execution.
/// Implementations may cache only successfully verified evidence for the
/// immutable `(parent_root, entity)` pair.
pub trait AuthenticatedParentTree: Send + Sync + core::fmt::Debug {
    fn parent_block_hash(&self) -> B256;

    fn parent_root(&self) -> B256;

    fn read_leaf_verified(
        &self,
        entity: EntityRef,
        expected_parent_root: B256,
    ) -> Result<Option<crate::Commitment>>;

    /// Prepare an immutable, side-effect-free candidate batch against this
    /// exact parent. Implementations must authenticate the complete unique
    /// touched set and eliminate parent-equal final leaves.
    fn prepare_seal(
        &self,
        block_number: u64,
        mutations: &[FinalLeafMutation],
    ) -> Result<crate::ProvisionalTreeBatch>;
}

/// Node-owned opener for one exact finalized parent. The lifecycle supplies
/// the authoritative EVM root only after slot 1 has been read in begin-block.
pub trait AuthenticatedParentTreeFactory: Send + Sync + core::fmt::Debug {
    fn open_parent(
        &self,
        identity: crate::ExactParentIdentity,
    ) -> Result<Arc<dyn AuthenticatedParentTree>>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalLeafMutation {
    pub entity: EntityRef,
    pub final_leaf: Option<crate::Commitment>,
}

/// Deterministic, benchmark-supplied CE work coefficients.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CeWorkConfig {
    pub seal_base_units: u64,
    pub unique_key_units: u64,
    pub work_limit: u64,
}

impl CeWorkConfig {
    pub const fn new(seal_base_units: u64, unique_key_units: u64, work_limit: u64) -> Self {
        Self {
            seal_base_units,
            unique_key_units,
            work_limit,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CeWorkState {
    used: u64,
    seen_keys: BTreeSet<EntityRef>,
    transaction_start: Option<CeWorkTransactionStart>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CeWorkTransactionStart {
    transaction_keys: BTreeSet<EntityRef>,
}

const CE_WORK_FAILURE_NONE: u8 = 0;
const CE_WORK_FAILURE_TRANSACTION: u8 = 1;
const CE_WORK_FAILURE_BLOCK: u8 = 2;

/// Payload-builder checkpoint. Restoring it is valid only when the entire
/// speculative transaction is excluded together with its EVM journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CeWorkCheckpoint(CeWorkState);

#[derive(Debug)]
struct EmptyAuthenticatedTree;

impl AuthenticatedParentTree for EmptyAuthenticatedTree {
    fn parent_block_hash(&self) -> B256 {
        B256::ZERO
    }

    fn parent_root(&self) -> B256 {
        B256::ZERO
    }

    fn read_leaf_verified(
        &self,
        _entity: EntityRef,
        expected_parent_root: B256,
    ) -> Result<Option<crate::Commitment>> {
        if expected_parent_root != B256::ZERO {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "empty reference parent does not match the EVM root".into(),
            ));
        }
        Ok(None)
    }

    fn prepare_seal(
        &self,
        block_number: u64,
        mutations: &[FinalLeafMutation],
    ) -> Result<crate::ProvisionalTreeBatch> {
        use crate::{
            schema::Collection,
            smt::{derive_tree_key, PoseidonSmt, TreeLeaf},
        };
        let mut tree = PoseidonSmt::empty();
        let mut updates = Vec::with_capacity(mutations.len());
        let mut leaf_changes = std::collections::BTreeMap::new();
        for mutation in mutations {
            let (collection, entity_id) = match mutation.entity {
                EntityRef::Tribute(id) => (Collection::Tribute, id),
                EntityRef::NodItem(id) => (Collection::NodItem, id),
                EntityRef::NodBucket(id) => (Collection::NodBucket, id),
            };
            let key = derive_tree_key(collection, entity_id)
                .map_err(|error| fatal_scope(error.to_string()))?;
            let leaf = match mutation.final_leaf {
                Some(commitment) => TreeLeaf::from_be_bytes(*commitment.as_bytes())
                    .map_err(|error| fatal_scope(error.to_string()))?,
                None => TreeLeaf::ZERO,
            };
            updates.push((key, leaf));
            let persisted_key = crate::persistence::TreeKey::try_from(B256::from(key.as_bytes()))
                .map_err(|error| fatal_scope(error.to_string()))?;
            let change = mutation
                .final_leaf
                .map(
                    |commitment| -> Result<crate::TreeChange<crate::persistence::LeafValue>> {
                        Ok(crate::TreeChange::Set(
                            crate::persistence::LeafValue::try_from(B256::from(
                                *commitment.as_bytes(),
                            ))
                            .map_err(|error| fatal_scope(error.to_string()))?,
                        ))
                    },
                )
                .transpose()?;
            if let Some(change) = change {
                if leaf_changes.insert(persisted_key, change).is_some() {
                    return Err(fatal_scope("compressed-entity tree-key collision"));
                }
            }
        }
        let new_root = tree
            .update_all(updates)
            .map_err(|error| fatal_scope(error.to_string()))?;
        crate::ProvisionalTreeBatch::new_unsharded(
            block_number,
            B256::ZERO,
            B256::ZERO,
            B256::from(new_root.as_bytes()),
            Default::default(),
            leaf_changes,
        )
        .map_err(|error| fatal_scope(error.to_string()))
    }
}

impl EntityRef {
    #[must_use]
    pub const fn entity_id(self) -> EntityId36 {
        match self {
            Self::Tribute(id) | Self::NodItem(id) | Self::NodBucket(id) => id,
        }
    }
}

/// A typed canonical body. The variant fixes collection, codec and emitter;
/// callers cannot pass those values independently.
#[derive(Clone, Copy, Debug)]
pub enum BodyInput<'a> {
    Tribute(&'a TributeBodyV1),
    NodItem(&'a NodItemBodyV1),
    NodBucket(&'a NodBucketBodyV1),
}

/// One of the four fork-fixed query surfaces.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum QueryRef {
    TributeByOwner(Address),
    TributeByDay(WorldwideDay),
    NodByOwner(Address),
    NodAll,
}

/// Exclusive ID-only parent-page request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdPageRequest {
    pub after: Option<EntityId36>,
    pub limit: u32,
}

/// Strictly ascending ID-only finalized-parent page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdPage {
    pub ids: Vec<EntityId36>,
    pub next_after: Option<EntityId36>,
}

/// Consumer-owned finalized-parent body/index seam.
pub trait ParentBodySource {
    fn get(
        &self,
        entity: EntityRef,
    ) -> core::result::Result<Option<StoredBody>, ParentBodySourceError>;

    fn list(
        &self,
        query: QueryRef,
        request: IdPageRequest,
    ) -> core::result::Result<IdPage, ParentBodySourceError>;
}

/// Sized borrowed adapter for carrying a parent source through typed lifecycle
/// contexts without weakening domain APIs to untyped extension registries.
#[derive(Clone, Copy)]
pub struct ParentBodySourceRef<'a>(&'a dyn ParentBodySource);

impl<'a> ParentBodySourceRef<'a> {
    #[must_use]
    pub const fn new(source: &'a dyn ParentBodySource) -> Self {
        Self(source)
    }
}

impl ParentBodySource for ParentBodySourceRef<'_> {
    fn get(
        &self,
        entity: EntityRef,
    ) -> core::result::Result<Option<StoredBody>, ParentBodySourceError> {
        self.0.get(entity)
    }

    fn list(
        &self,
        query: QueryRef,
        request: IdPageRequest,
    ) -> core::result::Result<IdPage, ParentBodySourceError> {
        self.0.list(query, request)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PayloadInner {
    Tribute(TributeBodyV1),
    NodItem(NodItemBodyV1),
    NodBucket(NodBucketBodyV1),
}

/// Read-only typed semantic payload. Construction remains inside this crate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedPayload(PayloadInner);

impl VerifiedPayload {
    #[must_use]
    pub fn as_tribute(&self) -> Option<&TributeBodyV1> {
        match &self.0 {
            PayloadInner::Tribute(body) => Some(body),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_nod_item(&self) -> Option<&NodItemBodyV1> {
        match &self.0 {
            PayloadInner::NodItem(body) => Some(body),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_nod_bucket(&self) -> Option<&NodBucketBodyV1> {
        match &self.0 {
            PayloadInner::NodBucket(body) => Some(body),
            _ => None,
        }
    }
}

/// Opaque proof that a body was decoded canonically and matched current
/// authenticated state. There is intentionally no public constructor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedBody {
    pub(crate) entity: EntityRef,
    pub(crate) commitment: crate::Commitment,
    pub(crate) stored_body: StoredBody,
    pub(crate) payload: VerifiedPayload,
}

impl VerifiedBody {
    #[must_use]
    pub const fn entity(&self) -> EntityRef {
        self.entity
    }

    #[must_use]
    pub const fn entity_id(&self) -> EntityId36 {
        self.entity.entity_id()
    }

    #[must_use]
    pub fn payload(&self) -> &VerifiedPayload {
        &self.payload
    }

    #[must_use]
    pub fn stored_body(&self) -> &StoredBody {
        &self.stored_body
    }
}

/// Resolved same-block page plus its exclusive continuation cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedBodyPage {
    bodies: Vec<VerifiedBody>,
    next_after: Option<EntityId36>,
}

impl VerifiedBodyPage {
    pub(crate) fn new(bodies: Vec<VerifiedBody>, next_after: Option<EntityId36>) -> Self {
        Self { bodies, next_after }
    }

    #[must_use]
    pub fn bodies(&self) -> &[VerifiedBody] {
        &self.bodies
    }

    #[must_use]
    pub fn into_bodies(self) -> Vec<VerifiedBody> {
        self.bodies
    }

    #[must_use]
    pub const fn next_after(&self) -> Option<EntityId36> {
        self.next_after
    }
}

const PHASE_BEFORE_BEGIN: u8 = 0;
const PHASE_ACTIVE: u8 = 1;
const PHASE_ENDED: u8 = 2;

/// Executor-owned phase capability for one block execution.
///
/// It is deliberately not stored in consensus state: the executor creates one
/// scope per block, calls [`begin_block`] once, threads the same instance
/// through every execution precompile, then calls [`end_block`] once. This
/// makes a post-cleanup read/mutation a deterministic ordering error without
/// adding a protocol storage slot. Finalized RPC readers do not receive this
/// capability.
#[derive(Debug)]
pub struct ExecutionScope {
    phase: AtomicU8,
    explicit_gas_charged: AtomicU64,
    explicit_gas_window_active: AtomicBool,
    explicit_gas_window_start: AtomicU64,
    explicit_gas_window_limit: AtomicU64,
    parent_tree: Mutex<Option<Arc<dyn AuthenticatedParentTree>>>,
    parent_tree_factory: Mutex<Option<Arc<dyn AuthenticatedParentTreeFactory>>>,
    parent_identity_without_root: Mutex<Option<(u32, u64, B256)>>,
    parent_binding_configured: AtomicBool,
    rpc_read_only: AtomicBool,
    ce_work_config: CeWorkConfig,
    ce_work: Mutex<CeWorkState>,
    ce_work_failure: AtomicU8,
}

/// Opaque snapshot of explicit compressed-entity gas charged in one execution
/// scope. The executor uses it only around receipt-visible system calls.
#[derive(Clone, Copy, Debug)]
pub struct ExplicitGasCheckpoint(u64);

/// Receipt-visible compressed-entity gas budget for one system transaction.
/// Dropping the guard closes the window even when execution returns an error.
pub struct ExplicitGasWindow<'scope> {
    scope: &'scope ExecutionScope,
    start: ExplicitGasCheckpoint,
}

impl ExplicitGasWindow<'_> {
    /// Successfully deducted CE gas in this system-transaction window.
    pub fn gas_used(&self) -> Result<u64> {
        self.scope.explicit_gas_since(self.start)
    }
}

impl Drop for ExplicitGasWindow<'_> {
    fn drop(&mut self) {
        self.scope
            .explicit_gas_window_active
            .store(false, Ordering::Release);
    }
}

impl ExecutionScope {
    #[must_use]
    pub fn new() -> Self {
        // This constructor is retained for empty-tree harnesses. Production
        // execution installs an exact finalized parent with `with_parent_tree`.
        Self {
            phase: AtomicU8::new(PHASE_BEFORE_BEGIN),
            explicit_gas_charged: AtomicU64::new(0),
            explicit_gas_window_active: AtomicBool::new(false),
            explicit_gas_window_start: AtomicU64::new(0),
            explicit_gas_window_limit: AtomicU64::new(0),
            parent_tree: Mutex::new(Some(Arc::new(EmptyAuthenticatedTree))),
            parent_tree_factory: Mutex::new(None),
            parent_identity_without_root: Mutex::new(None),
            parent_binding_configured: AtomicBool::new(false),
            rpc_read_only: AtomicBool::new(false),
            ce_work_config: CeWorkConfig::new(0, 0, u64::MAX),
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
    ) -> Self {
        Self {
            phase: AtomicU8::new(PHASE_BEFORE_BEGIN),
            explicit_gas_charged: AtomicU64::new(0),
            explicit_gas_window_active: AtomicBool::new(false),
            explicit_gas_window_start: AtomicU64::new(0),
            explicit_gas_window_limit: AtomicU64::new(0),
            parent_tree: Mutex::new(Some(parent_tree)),
            parent_tree_factory: Mutex::new(None),
            parent_identity_without_root: Mutex::new(None),
            parent_binding_configured: AtomicBool::new(true),
            rpc_read_only: AtomicBool::new(false),
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
    pub fn with_parent_tree_factory(
        factory: Arc<dyn AuthenticatedParentTreeFactory>,
        commitment_scheme_version: u32,
        parent_block_number: u64,
        parent_block_hash: B256,
        ce_work_config: CeWorkConfig,
    ) -> Self {
        Self {
            phase: AtomicU8::new(PHASE_BEFORE_BEGIN),
            explicit_gas_charged: AtomicU64::new(0),
            explicit_gas_window_active: AtomicBool::new(false),
            explicit_gas_window_start: AtomicU64::new(0),
            explicit_gas_window_limit: AtomicU64::new(0),
            parent_tree: Mutex::new(None),
            parent_tree_factory: Mutex::new(Some(factory)),
            parent_identity_without_root: Mutex::new(Some((
                commitment_scheme_version,
                parent_block_number,
                parent_block_hash,
            ))),
            parent_binding_configured: AtomicBool::new(true),
            rpc_read_only: AtomicBool::new(false),
            ce_work_config,
            ce_work: Mutex::new(CeWorkState {
                used: 0,
                seen_keys: BTreeSet::new(),
                transaction_start: None,
            }),
            ce_work_failure: AtomicU8::new(CE_WORK_FAILURE_NONE),
        }
    }

    /// Creates a finalized-state read scope for EVM instances used by RPC
    /// simulation. A real block executor replaces this fallback binding before
    /// begin-block and activates the normal mutation lifecycle.
    #[must_use]
    pub fn for_finalized_rpc(
        factory: Arc<dyn AuthenticatedParentTreeFactory>,
        commitment_scheme_version: u32,
        block_number: u64,
        block_hash: B256,
    ) -> Self {
        let mut scope = Self::with_parent_tree_factory(
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

    /// Binds the factory/identity to the scope already captured by this EVM's
    /// precompiles. Live wiring calls this after the block parent is known and
    /// before begin-block; lifecycle and every nested precompile therefore keep
    /// using the same `Arc<ExecutionScope>`.
    pub fn configure_parent_tree_factory(
        &self,
        factory: Arc<dyn AuthenticatedParentTreeFactory>,
        commitment_scheme_version: u32,
        parent_block_number: u64,
        parent_block_hash: B256,
    ) -> Result<()> {
        if self.phase.load(Ordering::Acquire) != PHASE_BEFORE_BEGIN {
            return Err(fatal_scope(
                "exact-parent tree factory configured after begin-block",
            ));
        }
        self.parent_binding_configured
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| fatal_scope("exact-parent tree factory configured more than once"))?;
        self.rpc_read_only.store(false, Ordering::Release);

        let mut tree = self
            .parent_tree
            .lock()
            .map_err(|_| fatal_scope("authenticated parent tree lock poisoned"))?;
        let mut installed_factory = self
            .parent_tree_factory
            .lock()
            .map_err(|_| fatal_scope("authenticated parent tree factory lock poisoned"))?;
        let mut identity = self
            .parent_identity_without_root
            .lock()
            .map_err(|_| fatal_scope("authenticated parent identity lock poisoned"))?;
        *tree = None;
        *installed_factory = Some(factory);
        *identity = Some((
            commitment_scheme_version,
            parent_block_number,
            parent_block_hash,
        ));
        Ok(())
    }

    pub(crate) fn open_exact_parent(&self, evm_root: B256) -> Result<()> {
        let mut tree = self
            .parent_tree
            .lock()
            .map_err(|_| fatal_scope("authenticated parent tree lock poisoned"))?;
        if let Some(opened) = tree.as_ref() {
            if opened.parent_root() != evm_root {
                return Err(outbe_primitives::error::PrecompileError::Fatal(
                    "opened parent tree root does not match EVM slot 1".into(),
                ));
            }
            return Ok(());
        }
        let factory = self
            .parent_tree_factory
            .lock()
            .map_err(|_| fatal_scope("authenticated parent tree factory lock poisoned"))?
            .clone()
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::TreeUnavailable(
                    "no exact-parent tree factory was installed".into(),
                )
            })?;
        let (commitment_scheme_version, block_number, block_hash) = self
            .parent_identity_without_root
            .lock()
            .map_err(|_| fatal_scope("authenticated parent identity lock poisoned"))?
            .ok_or_else(|| fatal_scope("missing exact-parent identity metadata"))?;
        let opened = factory.open_parent(crate::ExactParentIdentity {
            commitment_scheme_version,
            block_number,
            block_hash,
            root: evm_root,
        })?;
        if opened.parent_block_hash() != block_hash || opened.parent_root() != evm_root {
            return Err(fatal_scope(
                "exact-parent factory returned a tree with the wrong block hash or root",
            ));
        }
        *tree = Some(opened);
        Ok(())
    }

    fn opened_parent_tree(&self) -> Result<Arc<dyn AuthenticatedParentTree>> {
        self.parent_tree
            .lock()
            .map_err(|_| fatal_scope("authenticated parent tree lock poisoned"))?
            .clone()
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::TreeUnavailable(
                    "exact-parent tree has not been opened by begin-block".into(),
                )
            })
    }

    pub fn read_parent_leaf_verified(
        &self,
        entity: EntityRef,
        expected_parent_root: B256,
    ) -> Result<Option<crate::Commitment>> {
        self.require_readable()?;
        if self.phase.load(Ordering::Acquire) != PHASE_ACTIVE {
            self.open_exact_parent(expected_parent_root)?;
        }
        let parent_tree = self.opened_parent_tree()?;
        if parent_tree.parent_root() != expected_parent_root {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "authenticated parent tree root does not match EVM slot 1".into(),
            ));
        }
        parent_tree.read_leaf_verified(entity, expected_parent_root)
    }

    pub fn parent_root(&self) -> Result<B256> {
        self.opened_parent_tree().map(|tree| tree.parent_root())
    }

    pub(crate) fn prepare_tree_seal(
        &self,
        block_number: u64,
        mutations: &[FinalLeafMutation],
    ) -> Result<crate::ProvisionalTreeBatch> {
        self.require_active()?;
        self.opened_parent_tree()?
            .prepare_seal(block_number, mutations)
    }

    pub fn ce_work_checkpoint(&self) -> Result<CeWorkCheckpoint> {
        self.ce_work
            .lock()
            .map(|state| CeWorkCheckpoint(state.clone()))
            .map_err(|_| fatal_scope("compressed-entity work meter lock poisoned"))
    }

    /// Opens one executor transaction window so overflow can distinguish a
    /// transaction that cannot fit an empty block from ordinary remaining
    /// block-capacity exhaustion.
    pub fn begin_ce_work_transaction(&self) -> Result<()> {
        self.ce_work_failure
            .store(CE_WORK_FAILURE_NONE, Ordering::Release);
        let mut state = self
            .ce_work
            .lock()
            .map_err(|_| fatal_scope("compressed-entity work meter lock poisoned"))?;
        if state.transaction_start.is_some() {
            return Err(fatal_scope(
                "compressed-entity work transaction window is already active",
            ));
        }
        state.transaction_start = Some(CeWorkTransactionStart {
            transaction_keys: BTreeSet::new(),
        });
        Ok(())
    }

    /// Closes the current executor transaction window. Work remains reserved;
    /// excluded transactions are restored separately from their checkpoint.
    pub fn end_ce_work_transaction(&self) -> Result<()> {
        let mut state = self
            .ce_work
            .lock()
            .map_err(|_| fatal_scope("compressed-entity work meter lock poisoned"))?;
        if state.transaction_start.take().is_none() {
            return Err(fatal_scope(
                "compressed-entity work transaction window is not active",
            ));
        }
        Ok(())
    }

    /// Returns and clears the typed CE admission failure recorded through the
    /// infallible revm precompile boundary.
    pub fn take_ce_work_failure(&self) -> Option<outbe_primitives::error::PrecompileError> {
        match self
            .ce_work_failure
            .swap(CE_WORK_FAILURE_NONE, Ordering::AcqRel)
        {
            CE_WORK_FAILURE_TRANSACTION => {
                Some(outbe_primitives::error::PrecompileError::TransactionCeWorkLimitExceeded)
            }
            CE_WORK_FAILURE_BLOCK => {
                Some(outbe_primitives::error::PrecompileError::BlockCeWorkCapacityExhausted)
            }
            _ => None,
        }
    }

    pub fn restore_ce_work_checkpoint(&self, checkpoint: CeWorkCheckpoint) -> Result<()> {
        let mut state = self
            .ce_work
            .lock()
            .map_err(|_| fatal_scope("compressed-entity work meter lock poisoned"))?;
        if checkpoint.0.used > state.used || !checkpoint.0.seen_keys.is_subset(&state.seen_keys) {
            return Err(fatal_scope(
                "invalid compressed-entity work checkpoint restore",
            ));
        }
        *state = checkpoint.0;
        Ok(())
    }

    pub fn ce_work_used(&self) -> Result<u64> {
        self.ce_work
            .lock()
            .map(|state| state.used)
            .map_err(|_| fatal_scope("compressed-entity work meter lock poisoned"))
    }

    pub(crate) fn reserve_unique_key_work(&self, entity: EntityRef) -> Result<()> {
        self.require_active()?;
        let mut state = self
            .ce_work
            .lock()
            .map_err(|_| fatal_scope("compressed-entity work meter lock poisoned"))?;
        let transaction_unique_keys = state.transaction_start.as_ref().map_or(1_usize, |start| {
            if start.transaction_keys.contains(&entity) {
                0
            } else {
                start.transaction_keys.len().saturating_add(1)
            }
        });
        if transaction_unique_keys == 0 {
            return Ok(());
        }
        let transaction_units = u64::try_from(transaction_unique_keys)
            .ok()
            .and_then(|count| self.ce_work_config.unique_key_units.checked_mul(count))
            .and_then(|units| self.ce_work_config.seal_base_units.checked_add(units));
        if transaction_units.is_none_or(|units| units > self.ce_work_config.work_limit) {
            self.ce_work_failure
                .store(CE_WORK_FAILURE_TRANSACTION, Ordering::Release);
            return Err(outbe_primitives::error::PrecompileError::TransactionCeWorkLimitExceeded);
        }
        if state.seen_keys.contains(&entity) {
            if let Some(start) = state.transaction_start.as_mut() {
                start.transaction_keys.insert(entity);
            }
            return Ok(());
        }
        let next = state
            .used
            .checked_add(self.ce_work_config.unique_key_units)
            .ok_or(outbe_primitives::error::PrecompileError::BlockCeWorkCapacityExhausted)?;
        if next > self.ce_work_config.work_limit {
            self.ce_work_failure
                .store(CE_WORK_FAILURE_BLOCK, Ordering::Release);
            return Err(outbe_primitives::error::PrecompileError::BlockCeWorkCapacityExhausted);
        }
        state.seen_keys.insert(entity);
        if let Some(start) = state.transaction_start.as_mut() {
            start.transaction_keys.insert(entity);
        }
        state.used = next;
        Ok(())
    }

    /// Opens the receipt-visible CE gas budget for one system transaction.
    pub fn begin_explicit_gas_window(&self, gas_limit: u64) -> Result<ExplicitGasWindow<'_>> {
        self.require_active()?;
        self.explicit_gas_window_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                outbe_primitives::error::PrecompileError::Fatal(
                    "compressed-entity explicit gas window is already active".into(),
                )
            })?;
        let start = self.explicit_gas_checkpoint();
        self.explicit_gas_window_start
            .store(start.0, Ordering::Release);
        self.explicit_gas_window_limit
            .store(gas_limit, Ordering::Release);
        Ok(ExplicitGasWindow { scope: self, start })
    }

    /// Snapshots the monotonic explicit CE charge counter.
    ///
    /// User transactions do not consume this snapshot: their CE charges are
    /// already part of normal EVM gas. The system lane snapshots immediately
    /// before execution and publishes exactly [`Self::explicit_gas_since`].
    #[must_use]
    pub fn explicit_gas_checkpoint(&self) -> ExplicitGasCheckpoint {
        ExplicitGasCheckpoint(self.explicit_gas_charged.load(Ordering::Acquire))
    }

    /// Returns explicit CE gas successfully deducted since `checkpoint`.
    pub fn explicit_gas_since(&self, checkpoint: ExplicitGasCheckpoint) -> Result<u64> {
        self.explicit_gas_charged
            .load(Ordering::Acquire)
            .checked_sub(checkpoint.0)
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::Fatal(
                    "compressed-entity gas checkpoint exceeds current counter".into(),
                )
            })
    }

    pub(crate) fn deduct_explicit_gas(&self, storage: &StorageHandle<'_>, gas: u64) -> Result<()> {
        self.require_readable()?;
        if self.explicit_gas_window_active.load(Ordering::Acquire) {
            let charged = self.explicit_gas_charged.load(Ordering::Acquire);
            let start = self.explicit_gas_window_start.load(Ordering::Acquire);
            let limit = self.explicit_gas_window_limit.load(Ordering::Acquire);
            let used = charged.checked_sub(start).ok_or_else(|| {
                outbe_primitives::error::PrecompileError::Fatal(
                    "compressed-entity explicit gas window starts after current counter".into(),
                )
            })?;
            if used.checked_add(gas).is_none_or(|next| next > limit) {
                return Err(outbe_primitives::error::PrecompileError::OutOfGas);
            }
        }
        storage.deduct_gas(gas)?;
        self.explicit_gas_charged
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |charged| {
                charged.checked_add(gas)
            })
            .map(|_| ())
            .map_err(|_| {
                outbe_primitives::error::PrecompileError::Fatal(
                    "compressed-entity explicit gas counter overflow".into(),
                )
            })
    }

    pub(crate) fn require_active(&self) -> Result<()> {
        if self.phase.load(Ordering::Acquire) == PHASE_ACTIVE {
            return Ok(());
        }
        Err(outbe_primitives::error::PrecompileError::Fatal(
            "compressed-entity execution attempted outside active block lifecycle".into(),
        ))
    }

    fn require_readable(&self) -> Result<()> {
        if self.phase.load(Ordering::Acquire) == PHASE_ACTIVE
            || self.rpc_read_only.load(Ordering::Acquire)
        {
            return Ok(());
        }
        Err(outbe_primitives::error::PrecompileError::Fatal(
            "compressed-entity execution attempted outside active block lifecycle".into(),
        ))
    }

    pub(crate) fn activate(&self) -> Result<()> {
        if self.rpc_read_only.load(Ordering::Acquire) {
            return Err(fatal_scope(
                "finalized RPC read scope cannot enter block mutation lifecycle",
            ));
        }
        self.phase
            .compare_exchange(
                PHASE_BEFORE_BEGIN,
                PHASE_ACTIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| {
                outbe_primitives::error::PrecompileError::Fatal(
                    "compressed-entity begin_block called more than once".into(),
                )
            })?;
        let mut work = self
            .ce_work
            .lock()
            .map_err(|_| fatal_scope("compressed-entity work meter lock poisoned"))?;
        if self.ce_work_config.seal_base_units > self.ce_work_config.work_limit {
            return Err(outbe_primitives::error::PrecompileError::TransactionCeWorkLimitExceeded);
        }
        work.used = self.ce_work_config.seal_base_units;
        work.seen_keys.clear();
        work.transaction_start = None;
        self.ce_work_failure
            .store(CE_WORK_FAILURE_NONE, Ordering::Release);
        Ok(())
    }

    pub(crate) fn finish(&self) -> Result<()> {
        self.phase
            .compare_exchange(
                PHASE_ACTIVE,
                PHASE_ENDED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| {
                outbe_primitives::error::PrecompileError::Fatal(
                    "compressed-entity end_block called outside active lifecycle".into(),
                )
            })
    }
}

fn fatal_scope(message: impl Into<String>) -> outbe_primitives::error::PrecompileError {
    outbe_primitives::error::PrecompileError::Fatal(message.into())
}

impl Default for ExecutionScope {
    fn default() -> Self {
        Self::new()
    }
}

pub fn begin_block(storage: StorageHandle<'_>, scope: &ExecutionScope) -> Result<()> {
    lifecycle::begin_block(storage, scope)
}

pub fn end_block(storage: StorageHandle<'_>, scope: &ExecutionScope) -> Result<crate::SealOutput> {
    lifecycle::end_block(storage, scope)
}

pub fn read(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    entity: EntityRef,
) -> Result<Option<VerifiedBody>> {
    scope.require_readable()?;
    runtime::read(storage, scope, parent, entity)
}

pub fn list(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    query: QueryRef,
    request: IdPageRequest,
) -> Result<VerifiedBodyPage> {
    scope.require_readable()?;
    runtime::list(storage, scope, parent, query, request)
}

pub fn mint(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    new_body: BodyInput<'_>,
) -> Result<()> {
    scope.require_active()?;
    storage
        .clone()
        .with_checkpoint(|| runtime::mint(storage, scope, new_body))
}

pub fn update(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    current: VerifiedBody,
    new_body: BodyInput<'_>,
) -> Result<()> {
    scope.require_active()?;
    storage
        .clone()
        .with_checkpoint(|| runtime::update(storage, scope, current, new_body))
}

pub fn delete(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    current: VerifiedBody,
) -> Result<()> {
    scope.require_active()?;
    storage
        .clone()
        .with_checkpoint(|| runtime::delete(storage, scope, current))
}

pub(crate) fn tribute_payload(body: TributeBodyV1) -> VerifiedPayload {
    VerifiedPayload(PayloadInner::Tribute(body))
}

pub(crate) fn nod_item_payload(body: NodItemBodyV1) -> VerifiedPayload {
    VerifiedPayload(PayloadInner::NodItem(body))
}

pub(crate) fn nod_bucket_payload(body: NodBucketBodyV1) -> VerifiedPayload {
    VerifiedPayload(PayloadInner::NodBucket(body))
}
