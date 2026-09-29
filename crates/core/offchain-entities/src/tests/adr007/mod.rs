use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use alloy_primitives::{address, b256, keccak256, Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::{COMPRESSED_ENTITIES_ADDRESS, NOD_ADDRESS, TRIBUTE_ADDRESS},
    error::{PrecompileError, Result},
    storage::{hashmap::HashMapStorageProvider, types::StorageKey, StorageHandle},
};

use crate::{
    begin_block, body_commitment, delete, encode_nod_bucket_v1, encode_nod_item_v1,
    encode_tribute_v1, end_block, list, mint, read, update, AuthenticatedParentTree, BodyInput,
    CeWorkConfig, EntityRef, ExecutionScope, FinalLeafMutation, IdPage, IdPageRequest,
    NodBucketBodyV1, NodItemBodyV1, ParentBodySource, ParentBodySourceError, PartitionRef,
    ProvisionalTreeBatch, QueryRef, StoredBody, TributeBodyV1, VerifiedBody, WwdEntityId,
    ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1, MAX_ID_PAGE_LIMIT,
};
use crate::{
    runtime::{
        NodBodyStored, TributeBodyDeleted, TributeBodyStored, INDEX_RECORD_SCAN_GAS, PARENT_ID_GAS,
        READ_FIXED_GAS, READ_GAS_PER_CANONICAL_BYTE,
    },
    schema::{
        body_locator, Collection, CompressedEntitiesSchema, DeltaStatus, IndexKind, IndexRecord,
        PendingWord,
    },
    state::{
        State, BODY_TOUCHED_LENGTH_CLEANUP_GAS, FIRST_BODY_TOUCH_CLEANUP_GAS,
        FIRST_INDEX_TOUCH_CLEANUP_GAS, INDEX_TOUCHED_LENGTH_CLEANUP_GAS, MAX_STORED_BODY_BYTES_V1,
    },
};

mod fixtures;
use fixtures::{
    entity, nod_item, overlay_leaf, scope_with_tree, seed_parent_tribute, stored_tribute, tribute,
    tribute_commitment, FixtureBody, MemoryParent, TestAuthenticatedTree,
    FIRST_TRIBUTE_CLEANUP_GAS,
};

mod cleanup;
mod encoding;
mod gas;
mod queries;
mod rollback;
mod transitions;

use rollback::{arm_fault, FaultPosition};
