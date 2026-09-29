use crate::sharding::aggregate_b256_shard_roots;
use alloy_primitives::B256;
use reth_db::database::Database;
use reth_db::transaction::DbTx;
use reth_db::transaction::DbTxMut;

use crate::staging::TreeChange;
use crate::CollectionKey;

use super::*;
use std::collections::BTreeMap;

use crate::{
    collection_root, sealed_root,
    staging::{
        CollectionBatch, CollectionOperation, ProvisionalCatalogBatch, ProvisionalShardBatch,
        ProvisionalShardSetBatch, ProvisionalTreeBatch,
    },
    CeDomain, CeTopologyV1, K_PROVISIONAL,
};

mod fixtures;
use fixtures::{b256, identity, marker, sharded_genesis, sharded_identity};

mod codecs;

mod mdbx;

mod restart;
