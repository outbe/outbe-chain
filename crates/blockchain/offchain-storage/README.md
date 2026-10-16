# Off-chain projection storage

This library provides memory and RocksDB storage adapters. Tribute, Nod and Gem
use native EVM state; their records and proofs do not depend on this library.

A RocksDB configuration uses separate primary and secondary directories:

```toml
version = 1
backend = "rocksdb"
start_block = 1

[rocksdb]
path = "data/offchain"
secondary_path = "observation/secondary"
```

Paths are relative to the TOML file. Primary and secondary directories must be
separate and must not contain each other. One process owns the primary database.
Readers can open independent secondary sessions and catch up before collecting
a stable observation.

## Storage contract

`StorageProvider` supplies reader/writer handles and a lifetime ownership guard.
Callers continue to use `StorageReader` and `StorageWriter`. A future adapter must
implement their complete semantics, including metadata, duplicate/order-preserving
MultiGet, ordered bounded prefix scans with exclusive cursors, and all-or-nothing
ordered batches. Backend errors distinguish unavailability from corruption.

RocksDB uses one database and the default column family per node. Namespace
prefixes partition keys. Stored values and metadata have a bounded, versioned
encoding; the database has a format marker. Every acknowledged write uses WAL
and `sync=true`. A projection frame publishes bodies, indexes, retained records,
and its checkpoint in one WriteBatch. GC shares the node's primary handle and
retains the projection retention fence.

## Verification

Unit and shared adapter conformance tests run with:

```sh
cargo test -p outbe-offchain-storage
```

The separate `rocks_process_e2e` integration target checks atomic checkpoint and
data recovery with a real writer child and independent secondary sessions.
It does not establish network or SGX acceptance.
