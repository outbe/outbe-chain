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
settings. The previous `projection.mongodb-*`, `projection.start-block`, and
`OUTBE_*PROJECTION_MONGODB_*` runtime settings have been removed.

To use MongoDB, replace the document with:

```toml
version = 1
backend = "mongodb"
start_block = 1

[mongodb]
uri = "mongodb://127.0.0.1:27017/?replicaSet=rs0"
database = "outbe_validator_0"
```

MongoDB requires a transaction-capable replica set or sharded cluster. Every node
needs its own database. The adapter retains primary/majority reads and the single
writer lease. Keep credential-bearing TOML files private. Changing backend does
not migrate existing data; this implementation does not include a migration tool.

## Launch generators

- `create_genesis.py`: optional `offchain_storage` section in the network YAML,
  with the same fields as the TOML. Without it, generate RocksDB defaults. Selecting
  MongoDB generates a local replica-set setup script; an overridden external URI
  remains operator-managed. A custom `mongodb.database` is a prefix; the generator
  appends `_validator_N`. Review each generated TOML before launch.
- `prepare_network.py --storage-template template.toml`: copy a validated template
  per node. For MongoDB, append `_validator_N` to the template's database name.
  Without a template, generate relative RocksDB paths.
- `bootstrap-testnet.sh 4 OUT_DIR SEED_FILE [STORAGE_TEMPLATE]`: forwards that
  template to the network preparer. Re-bootstrap deliberately replaces a network;
  use `run-testnet.sh start OUT_DIR` to resume existing data.
- `localnet-stack.sh start [STORAGE_TEMPLATE]`: initialize or resume a stack.
  The template applies only to a new stack. Mongo service management is enabled
  only when the selected URI matches the stack's dedicated local Mongo endpoint.
- Generated `run-node.sh` and `run-ocomp-exporter.sh` reference the same TOML.
  `run-storage.sh` starts Mongo only when the TOML selects the managed local URI.

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
primary and an independent secondary. Both providers load the shared TOML. The
Mongo lane requires an isolated replica set and compares directly against RocksDB:

```sh
cargo test -p outbe-ocomp --test exporter_storage
OUTBE_TEST_MONGODB_URI='mongodb://127.0.0.1:27017/?replicaSet=rs0' \
  cargo test -p outbe-ocomp --test exporter_storage -- --ignored
```

This test supplies fixed chain-opening proof fixtures; it does not verify a live
chain's proofs or execute the final OCOMP result. Those require the network suite.

The separate Linux network matrix is `mise run e2e-offchain-storage`. It builds
the GramineDirect binaries and runs `features/offchain_storage.feature` once per
backend, covering public Tribute, real OCOMP, FullNode proof reads, exporter crash
replay and committee restart. It requires a working Linux Docker/Gramine runtime.

Network E2E uses `outbe-e2e --projection-backend rocksdb` (default), or
`--projection-backend mongodb` for the Mongo regression lane. The Mongo URI option
belongs to fixture generation; it is not passed to runtime nodes as an environment
setting. `@mongodb` outage scenarios run only in that backend lane. Generic
projection checks compare namespace/key/value/metadata records, not BSON layout.
Process integration tests do not substitute for the full OCOMP network E2E run.

For a host without Gramine, `mise run e2e-offchain-storage-native` builds an exact
`mock-native` artifact manifest and runs four validators with RocksDB through
public Tribute, Lysis/Nod, exporter crash replay and committee restart. Only the
enclave is mocked; storage, node and OCOMP processes are real. This separate
scenario omits fifth-node onboarding and SGX verification, which remain covered
by the full network profile above.
