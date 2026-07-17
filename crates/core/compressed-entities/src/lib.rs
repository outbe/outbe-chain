//! Canonical compressed-body identities, encodings, and commitments.

// Persistence errors intentionally retain both exact markers/identities so a
// startup or finality conflict can be diagnosed without a second database read.
#![allow(clippy::result_large_err)]

mod api;
mod collection;
mod commitment;
mod errors;
mod identity;
mod lifecycle;
mod persistence;
mod protobuf;
mod replay;
mod runtime;
mod schema;
mod sharding;
mod smt;
mod staging;
mod state;
mod tree_manager;
mod tree_service;

#[doc(hidden)]
pub mod bench_support;

pub use api::{
    begin_block, delete, end_block, list, mint, read, update, AuthenticatedParentTree,
    AuthenticatedParentTreeFactory, BodyInput, CeWorkCheckpoint, CeWorkConfig, EntityRef,
    ExecutionScope, ExplicitGasCheckpoint, ExplicitGasWindow, FinalLeafMutation, IdPage,
    IdPageRequest, ParentBodySource, ParentBodySourceRef, QueryRef, VerifiedBody, VerifiedBodyPage,
    VerifiedPayload, MAX_ID_PAGE_LIMIT,
};

pub use collection::{
    collection_key, collection_root, sealed_root, CeDomain, CeTopologyV1, CollectionKey,
    K_PROVISIONAL,
};
pub use commitment::{
    body_commitment, derive_poseidon_entity_id, identity_field, pbytes, Commitment,
    CommitmentError, ACTIVE_COMMITMENT_SCHEME, CES1_TAG_BASE, TAG_BODY, TAG_BYTES_ABSORB,
    TAG_BYTES_FINAL, TAG_BYTES_INIT, TAG_COLLECTION_KEY, TAG_COLLECTION_ROOT, TAG_ID, TAG_KEY,
    TAG_LEAF, TAG_SEALED_ROOT, TAG_SMT_BASE, TAG_SMT_NORMAL, TAG_SMT_ZERO, TAG_TOP_NODE,
};
pub use errors::ParentBodySourceError;
pub use identity::{EntityId36, EntityIdError};
pub use lifecycle::{CompressedEntitiesLifecycle, CompressedEntitiesLifecycleContext, SealOutput};
pub use persistence::{
    classify_restart, ApplyOutcome, CeMdbx, CeRetentionCursor, DurableFinalizedCheckpoint,
    EnvironmentIdentity, ExactParentIdentity, FinalizationStage, FinalizedMarker, PersistenceError,
    RestartClassification, TreeNamespace, CE_SMT_RELATIVE_PATH, LOCAL_STORAGE_SCHEMA_VERSION,
};
pub use protobuf::{
    decode_nod_bucket_v1, decode_nod_item_v1, decode_stored_nod_bucket_v1,
    decode_stored_nod_item_v1, decode_stored_tribute_v1, decode_tribute_v1, encode_nod_bucket_v1,
    encode_nod_item_v1, encode_tribute_v1, CanonicalBodyError, NodBucketBodyV1, NodItemBodyV1,
    StoredBody, TributeBodyV1, BODY_SCHEMA_V1,
};
pub use replay::{
    decode_canonical_body_event, reconstruct_effective_final_mutations, CanonicalBodyEvent,
    ReplayEventError,
};
pub use sharding::{empty_shard_top_root, ShardingError, K_CANDIDATES};
pub use staging::{
    CandidateCache, CandidateCacheLimits, CollectionBatch, ProvisionalCatalogBatch,
    ProvisionalShardBatch, ProvisionalShardSetBatch, ProvisionalTreeBatch, PublicationOutcome,
    StagedTreeBatch, StagingError, TreeChange,
};
pub use tree_manager::{CompressedTreeService, FinalizedCandidateOutcome, TreeServiceError};
pub use tree_service::MdbxAuthenticatedTree;

#[cfg(test)]
mod tests;
