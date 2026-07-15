use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};

use alloy_primitives::Address;
use outbe_common::WorldwideDay;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::{
    errors::ParentBodySourceError, lifecycle, runtime, EntityId36, NodBucketBodyV1, NodItemBodyV1,
    StoredBody, TributeBodyV1,
};

/// Fork-fixed upper bound shared by execution merge logic and parent adapters.
pub const MAX_ID_PAGE_LIMIT: u32 = 1_024;

/// One of the fork-fixed compressed-body namespaces.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EntityRef {
    Tribute(EntityId36),
    NodItem(EntityId36),
    NodBucket(EntityId36),
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
    pub const fn new() -> Self {
        Self {
            phase: AtomicU8::new(PHASE_BEFORE_BEGIN),
            explicit_gas_charged: AtomicU64::new(0),
            explicit_gas_window_active: AtomicBool::new(false),
            explicit_gas_window_start: AtomicU64::new(0),
            explicit_gas_window_limit: AtomicU64::new(0),
        }
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
        self.require_active()?;
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

    pub(crate) fn activate(&self) -> Result<()> {
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
            })
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

impl Default for ExecutionScope {
    fn default() -> Self {
        Self::new()
    }
}

pub fn begin_block(storage: StorageHandle<'_>, scope: &ExecutionScope) -> Result<()> {
    lifecycle::begin_block(storage, scope)
}

pub fn end_block(storage: StorageHandle<'_>, scope: &ExecutionScope) -> Result<()> {
    lifecycle::end_block(storage, scope)
}

pub fn read(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    entity: EntityRef,
) -> Result<Option<VerifiedBody>> {
    scope.require_active()?;
    runtime::read(storage, scope, parent, entity)
}

pub fn list(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    query: QueryRef,
    request: IdPageRequest,
) -> Result<VerifiedBodyPage> {
    scope.require_active()?;
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
