# Off-chain projection storage

The node and its OCOMP SnapshotExporter read the same per-node
`offchain-storage.toml`. Launch generators select RocksDB for a new network.
They preserve an existing TOML when restarting or regenerating launch scripts.

```toml
version = 1
backend = "rocksdb"
start_block = 1

[rocksdb]
path = "data/offchain"
secondary_path = "ocomp/domain-v1/exporter-v1/rocksdb-secondary"
```

Paths are relative to the TOML file, not the process working directory. The
primary and secondary directories must be separate and must not contain each
other. The node and exporter must run on the same host and have access to these
paths. One node process owns the primary database. Each exporter protocol lane
uses `secondary_path/<protocol_bundle_hash>` and exclusively owns that directory.

The node receives `--projection.storage-config /path/to/offchain-storage.toml`.
The exporter receives `OUTBE_OCOMP_STORAGE_CONFIG=/path/to/offchain-storage.toml`.
Both load configuration at startup. There is no environment fallback for backend
settings. RocksDB is the only supported persistent backend.

## Launch generators

- `create_genesis.py`: the optional `offchain_storage` section in network YAML
  has the same fields as the TOML. Defaults use local RocksDB paths.
- `prepare_network.py --storage-template template.toml`: copy a validated RocksDB
  template per node. Without a template, generate relative RocksDB paths.
- `bootstrap-testnet.sh 4 OUT_DIR SEED_FILE [STORAGE_TEMPLATE]`: forward the
  template to the network preparer. Use `run-testnet.sh start OUT_DIR` to resume.
- `localnet-stack.sh start [STORAGE_TEMPLATE]`: initialize or resume a stack.
  The template applies only to a new stack.
- Generated node and exporter scripts reference the same TOML. `run-storage.sh`
  validates that file. The node opens the RocksDB primary.

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

An exporter opens a secondary session and catches up before receiving its reader.
That reader cannot catch up while inventory is being built. The checkpoint and
current/retained reads use the same view. The exporter releases it after inventory
construction; a later attempt opens a new session. This is an input-storage change:
OCOMP canonical bodies, commitments, CAS artifacts, spool and ACK authority retain
their existing contracts.

## Tests

Unit and shared adapter conformance tests:

```sh
cargo test -p outbe-offchain-storage
cargo test -p outbe-node --test projection_startup
```

The separate `rocks_process_e2e` integration target starts a real writer child,
reads it from independent secondary sessions, kills the writer without running
its destructors, reopens its primary and checks atomic checkpoint/data recovery:

```sh
cargo test -p outbe-offchain-storage --test rocks_process_e2e
```

The exporter integration target compares the canonical current/retained union and
the exact published CAS chunk and manifest bytes. RocksDB reads through a reopened
primary and an independent secondary. Both providers load the shared TOML.

```sh
cargo test -p outbe-ocomp --test exporter_storage
```

This test supplies fixed chain-opening proof fixtures; it does not verify a live
chain's proofs or execute the final OCOMP result. Those require the network suite.

Network scenarios use `--projection-backend rocksdb` (the default). Generic
projection checks compare namespace, key, value and metadata records. Process
integration tests do not replace the full release SGX acceptance suite.
