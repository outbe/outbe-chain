//! NOD bodies use id-derived shards. Owner and bucket indexes use the shared store.
outbe_offchain_storage::body_partitioning!(
    outbe_compressed_entities::WwdEntityId,
    NOD_SHARD_COUNT,
    NOD_SHARD_FAMILY,
    "nod",
    "nod-shards",
    "nods",
    ["nods_by_owner", "nod_buckets"],
    "invalid NOD primary key width"
);
