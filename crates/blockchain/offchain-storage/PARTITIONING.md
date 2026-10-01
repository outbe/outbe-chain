# Compressed entity partitioning

Entities own their placement and lookup rules. The storage engine receives `PartitionRouting`
and `PartitionDataSource` through trait objects; it imports neither entity types nor concrete
backends. An additional datasource implements these ports without changing the engine.
`StorageProvider` is the configuration composition root. Domain registration happens in
`outbe_offchain_data::entity_partition_routing`, shared by node, exporter and snapshot readers.

| Population | Rule | RocksDB directory | MongoDB collection prefix |
|---|---|---|---|
| Projection state | Shared | `system/shared/` | `system__shared__` |
| Tribute live bodies and indexes | WWD | `tribute/wwd/<wwd>/` | `tribute__wwd_<wwd>__` |
| Tribute lifecycle and retained bodies | Shared | `tribute/shared/` | `tribute__shared__` |
| Nod items and owner index | Unsigned 160-bit owner address modulo 32 | `nod/nod-shards/<0..31>/` | `nod__nod_shards_<0..31>__` |
| Nod buckets and locator | Shared | `nod/shared/` | `nod__shared__` |

`nod_locations` maps a complete Nod ID to a four-byte big-endian shard number. Changing an
owner deletes the old primary and owner index, writes the new primary and owner index, and
replaces this locator in the same finalized batch. Point reads resolve the locator. Owner
queries select one shard; global scans merge ascending IDs from independently enumerated
physical partitions. The locator audit compares both complete populations and verifies
physical placement against the body owner. A missing primary selected by a locator is corruption.

Partition scans keep the existing `StorageReader::scan_prefix` interface. A single routed
partition serves the requested page with one datasource scan. Multiple partitions use an
ordered heap and per-call cursors, with internal tuning of 64 records per refill and a shared
8 MiB read-ahead budget beyond the necessary partition heads. One adapter page may temporarily
add up to 8 MiB; the output page has its separate existing 8 MiB bound. These budgets count
values and metadata, not keys, allocations or total process RSS. Under byte pressure unread
prefetch tails are discarded and re-read after the last retained key, never skipped. The logical
continuation is always the last returned key. Buffers do not survive a scan call, and corruption
discovered during read-ahead fails the call immediately. Adapter pages are checked for ordering,
prefix, entry/byte bounds and valid continuation through the same logical read interface.

MongoDB implements logical partitions as collections in the configured database. A single
majority-acknowledged transaction contains bodies, indexes, locators, partition retirement and
checkpoint. This does not configure MongoDB cluster sharding. RocksDB owns a database per
partition and a checksummed prepared journal under `system/shared/`. It applies entity scopes,
retirements and finally the checkpoint. Startup presents prepared mutations as an immutable
inspection view; chain identity and canonical checkpoint validation precede replay and readiness.
Read-only sessions refuse an unfinished journal and never initialize missing partitions.
Writer handles share ownership and remain inactive until validated activation; scoped bootstrap
authority and acknowledged teardown are described in [SESSION_LIFECYCLE.md](SESSION_LIFECYCLE.md).

Tribute WWD retirement is a batch effect. Without a retention pin it does not enumerate bodies.
RocksDB removes the selected directory; MongoDB clears only that scope's collections inside the
transaction. With a pin the projector preserves exact bodies and metadata in `tribute/shared`
before retiring the live partition. Nod survives retirement of its original WWD.

Projection schema 3 requires fresh storage. Scoped adapters reject the former root/shared,
`tribute-days`, `nod-days`, and flat MongoDB entity collection layouts. They perform no migration.
The primitive flat adapters remain available for low-level storage tests and isolated consumers;
production entity consumers explicitly inject the domain routing registry.

Changed Rust files, including tests, must stay within 1000 lines and 40000 Unicode characters.
`scripts/check-rust-file-size.py` checks the working diff and new files; CI checks against the
change base. Split responsibilities and test behavior through the public ports.

The native storage E2E scenario accepts `--projection-backend rocksdb` (default) or `mongodb`.
The MongoDB lane requires a local `mongod` executable and starts a scenario-owned replica set
on loopback, with separate generated databases for validators. Node and exporter share each
validator's exact configuration. External MongoDB URIs are not accepted by the harness.
