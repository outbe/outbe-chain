//! GEM bodies use id-derived shards. Owner and bucket indexes use the shared store.
outbe_offchain_storage::body_partitioning!(
    outbe_compressed_entities::WwdEntityId,
    GEM_SHARD_COUNT,
    GEM_SHARD_FAMILY,
    "gem",
    "gem-shards",
    "gems",
    ["gems_by_owner", "gem_buckets"],
    "invalid gem primary key width"
);
