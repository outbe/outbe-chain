mod empty_parent;

use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex,
    },
};

use alloy_primitives::{Address, B256};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::{
    errors::ParentBodySourceError, lifecycle, runtime, NodBucketBodyV1, NodItemBodyV1, StoredBody,
    TributeBodyV1, WwdEntityId,
};

/// Fork-fixed upper bound shared by execution merge logic and parent adapters.
pub const MAX_ID_PAGE_LIMIT: u32 = 1_024;

/// One of the fork-fixed compressed-body namespaces.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EntityRef {
    Tribute(WwdEntityId),
    NodItem(WwdEntityId),
    NodBucket(WwdEntityId),
    Gem(WwdEntityId),
    GemBucket(WwdEntityId),
}

/// Closed collection-level lifecycle authority. ADR-011 intentionally exposes
/// only Tribute WWD retirement. Singleton domains have no representable request.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PartitionRef {
    TributeWwd(WorldwideDay),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetirementOutcome {
    NotPresent,
    Requested,
}

/// Opaque proof that one collection root came from the completed CE lifecycle
/// for the current block. Domain owners accept this capability instead of a
/// caller-provided hash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SealedCollectionRoot {
    partition: PartitionRef,
    root: B256,
}

impl SealedCollectionRoot {
    #[must_use]
    pub const fn partition(self) -> PartitionRef {
        self.partition
    }

    #[must_use]
    pub const fn root(self) -> B256 {
        self.root
    }
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

    /// Verifies collection membership through the exact-parent Root Catalog.
    /// Absence means never populated (or already retired) and is a no-op for
    /// the ADR-011 trusted request path.
    fn partition_present_verified(
        &self,
        partition: PartitionRef,
        expected_parent_root: B256,
    ) -> Result<bool>;

    /// Returns the exact collection root committed by the authenticated parent
    /// catalog. Implementations used only by unrelated unit tests may keep the
    /// default fail-closed behavior.
    fn partition_root_verified(
        &self,
        _partition: PartitionRef,
        _expected_parent_root: B256,
    ) -> Result<Option<B256>> {
        Err(fatal_scope(
            "authenticated parent tree does not expose collection roots",
        ))
    }

    /// Prepare an immutable, side-effect-free candidate batch against this
    /// exact parent. Implementations must authenticate the complete unique
    /// touched set and eliminate parent-equal final leaves.
    fn prepare_seal(
        &self,
        block_number: u64,
        mutations: &[FinalLeafMutation],
        retirements: &[PartitionRef],
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
        crate::collection::EMPTY_SEALED_ROOT
    }

    fn read_leaf_verified(
        &self,
        _entity: EntityRef,
        expected_parent_root: B256,
    ) -> Result<Option<crate::Commitment>> {
        if expected_parent_root != self.parent_root() {
            return Err(fatal_scope(
                "empty reference parent does not match the EVM root",
            ));
        }
        Ok(None)
    }

    fn partition_present_verified(
        &self,
        _partition: PartitionRef,
        expected_parent_root: B256,
    ) -> Result<bool> {
        if expected_parent_root != self.parent_root() {
            return Err(fatal_scope(
                "empty reference parent does not match the EVM root",
            ));
        }
        Ok(false)
    }

    fn partition_root_verified(
        &self,
        _partition: PartitionRef,
        expected_parent_root: B256,
    ) -> Result<Option<B256>> {
        if expected_parent_root != self.parent_root() {
            return Err(fatal_scope(
                "empty reference parent does not match the EVM root",
            ));
        }
        Ok(None)
    }

    fn prepare_seal(
        &self,
        block_number: u64,
        mutations: &[FinalLeafMutation],
        retirements: &[PartitionRef],
    ) -> Result<crate::ProvisionalTreeBatch> {
        if !retirements.is_empty() {
            return Err(fatal_scope("empty parent cannot retire a collection"));
        }
        empty_parent::prepare(block_number, self.parent_root(), mutations)
    }
}

impl EntityRef {
    #[must_use]
    pub const fn entity_id(self) -> WwdEntityId {
        match self {
            Self::Tribute(id)
            | Self::NodItem(id)
            | Self::NodBucket(id)
            | Self::Gem(id)
            | Self::GemBucket(id) => id,
        }
    }
}

/// A typed canonical body. The variant fixes collection, codec and emitter;
/// callers cannot pass those values independently.
#[derive(Clone, Copy, Debug)]
pub enum BodyInput<'a> {
    Tribute(&'a TributeBodyV1),
    EncryptedTribute(&'a outbe_primitives::tribute_encryption::EncryptedTributeV2),
    NodItem(&'a NodItemBodyV1),
    EncryptedNodItem(&'a crate::NodItemBodyV2),
    NodBucket(&'a NodBucketBodyV1),
    Gem(&'a crate::GemBodyV1),
    GemBucket(&'a crate::GemBucketBodyV1),
}

/// Fork-fixed query surfaces.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum QueryRef {
    TributeByOwner(Address),
    TributeByDay(WorldwideDay),
    NodByOwner(Address),
    NodAll,
    GemByOwner(Address),
}

/// Exclusive ID-only parent-page request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdPageRequest {
    pub after: Option<WwdEntityId>,
    pub limit: u32,
}

/// Strictly ascending ID-only finalized-parent page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdPage {
    pub ids: Vec<WwdEntityId>,
    pub next_after: Option<WwdEntityId>,
}

/// Bounded primary-body page, independent of secondary index membership.
/// Identities are strictly ascending after the request's exclusive cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredBodyPage {
    pub entries: Vec<(WwdEntityId, StoredBody)>,
    /// Last returned identity when another page exists.
    pub next_after: Option<WwdEntityId>,
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
    EncryptedTribute(outbe_primitives::tribute_encryption::EncryptedTributeV2),
    NodItem(NodItemBodyV1),
    EncryptedNodItem(crate::NodItemBodyV2),
    NodBucket(NodBucketBodyV1),
    Gem(crate::GemBodyV1),
    GemBucket(crate::GemBucketBodyV1),
}

/// Read-only typed semantic payload. Construction remains inside this crate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedPayload(PayloadInner);

impl VerifiedPayload {
    #[must_use]
    pub fn as_gem(&self) -> Option<&crate::GemBodyV1> {
        match &self.0 {
            PayloadInner::Gem(body) => Some(body),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_gem_bucket(&self) -> Option<&crate::GemBucketBodyV1> {
        match &self.0 {
            PayloadInner::GemBucket(body) => Some(body),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_encrypted_tribute(
        &self,
    ) -> Option<&outbe_primitives::tribute_encryption::EncryptedTributeV2> {
        match &self.0 {
            PayloadInner::EncryptedTribute(body) => Some(body),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_tribute(&self) -> Option<&TributeBodyV1> {
        match &self.0 {
            PayloadInner::Tribute(body) => Some(body),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_encrypted_nod_item(&self) -> Option<&crate::NodItemBodyV2> {
        match &self.0 {
            PayloadInner::EncryptedNodItem(body) => Some(body),
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
    pub const fn entity_id(&self) -> WwdEntityId {
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
    next_after: Option<WwdEntityId>,
}

impl VerifiedBodyPage {
    pub(crate) fn new(bodies: Vec<VerifiedBody>, next_after: Option<WwdEntityId>) -> Self {
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
    pub const fn next_after(&self) -> Option<WwdEntityId> {
        self.next_after
    }
}

const PHASE_BEFORE_BEGIN: u8 = 0;
const PHASE_ACTIVE: u8 = 1;
const PHASE_ENDED: u8 = 2;

/// Executor-owned phase capability for one block execution.
///
/// It is deliberately not stored in consensus state. The executor creates one
/// scope per block and calls [`begin_block`] once. It threads the same instance
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
    provisional_seal: Mutex<Option<crate::SealOutput>>,
    completed_seal: Mutex<Option<crate::SealOutput>>,
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

pub mod scope_factory;

mod scope;

fn fatal_scope(message: impl core::fmt::Display) -> outbe_primitives::error::PrecompileError {
    crate::errors::fatal_error(message)
}

impl Default for ExecutionScope {
    fn default() -> Self {
        scope_factory::empty_scope(
            Some(Arc::new(EmptyAuthenticatedTree)),
            CeWorkConfig::new(0, 0, u64::MAX),
        )
    }
}

pub fn begin_block(storage: StorageHandle<'_>, scope: &ExecutionScope) -> Result<()> {
    lifecycle::begin_block(storage, scope)
}

pub fn end_block(storage: StorageHandle<'_>, scope: &ExecutionScope) -> Result<crate::SealOutput> {
    lifecycle::end_block(storage, scope)
}

pub fn preview_end_block(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
) -> Result<crate::SealOutput> {
    lifecycle::preview_end_block_for_storage(storage, scope)
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

/// Trusted, closed ADR-011 partition retirement request. The caller supplies
/// only the typed partition identity; collection/domain derivation stays here.
pub fn retire_partition(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    partition: PartitionRef,
) -> Result<RetirementOutcome> {
    scope.require_active()?;
    crate::state::State::new(storage).request_retirement(scope, partition)
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

pub(crate) fn encrypted_tribute_payload(
    body: outbe_primitives::tribute_encryption::EncryptedTributeV2,
) -> VerifiedPayload {
    VerifiedPayload(PayloadInner::EncryptedTribute(body))
}

pub(crate) fn nod_item_payload(body: NodItemBodyV1) -> VerifiedPayload {
    VerifiedPayload(PayloadInner::NodItem(body))
}

pub(crate) fn nod_bucket_payload(body: NodBucketBodyV1) -> VerifiedPayload {
    VerifiedPayload(PayloadInner::NodBucket(body))
}

pub(crate) fn encrypted_nod_item_payload(body: crate::NodItemBodyV2) -> VerifiedPayload {
    VerifiedPayload(PayloadInner::EncryptedNodItem(body))
}

pub(crate) fn gem_payload(body: crate::GemBodyV1) -> VerifiedPayload {
    VerifiedPayload(PayloadInner::Gem(body))
}
pub(crate) fn gem_bucket_payload(body: crate::GemBucketBodyV1) -> VerifiedPayload {
    VerifiedPayload(PayloadInner::GemBucket(body))
}
