# ADR-017: Close gas, work, concurrency, and local-capacity limits with measured evidence

- **Status:** Superseded; historical input only
- **Canonical mapping:** [`docs/adr/legacy-reconciliation.md`](../docs/adr/legacy-reconciliation.md)
- **Date:** 2026-07-16
- **Depends on:** ADR-009, ADR-016

## Context

ADR-009–016 use parameterized sharding with fork-fixed pre-production/testnet `K_PROVISIONAL = 16`. They do not claim that value is production-optimal and do not settle production execution, worker, reader, cache, or activation limits before the complete system exists.

ADR-017 owns the later performance closure over the complete Tribute/Nod path: canonical body handling, Mongo projection/read verification, sharded collections and Root Catalog, header carrier, proofs, persistence/recovery, and snapshot bootstrap. It converts measured costs on the minimum validator hardware into deterministic consensus CE work/gas limits and startup-validated local resource envelopes.

This ADR preserves the performance questions and preferred directions discovered while designing ADR-009 so they are not lost. They are not automatically implementation requirements for ADR-009; ADR-017 must revalidate them against the completed path and actual pinned Reth concurrency.

## Minimum validator benchmark profile

The accepted minimum validator class is:

```text
CPU:     Intel Xeon E-2388G, 8 physical cores / 16 threads,
         3.2 GHz base / 4.6 GHz boost
RAM:     64 GiB
Storage: local NVMe
TEE:     Intel SGX support
```

Every activation report additionally records exact OS/kernel, filesystem and mount options, NVMe model, CPU governor/turbo state, SGX mode, Rust toolchain/build flags, source revision/dirty declaration, background load, and warm/cold cache preparation. SGX support identifies the validator class; each scenario states whether the measured path actually enters an enclave.

## Production shard-count selection

ADR-017 selects `K_PRODUCTION` only on the completed ADR-001–016 node plus the actual co-located off-chain-computation workload/resource contention intended for production. `K` remains a power of two:

```text
K_CANDIDATES = [1, 2, 4, 8, 16, 32]
```

The full benchmark compares concentrated single-shard safety, uniform throughput/locality, collection/Root Catalog overhead, proof/persistence contention, RSS, staged records/bytes, MDBX growth, and the active validator workload. Sixteen is the baseline candidate, not a privileged winner. Counts above 32 are outside the implemented ADR-009 shard-layout contract and require a separate architecture/codec decision rather than silently expanding this benchmark.

The reviewed result becomes each initial domain's `K_PRODUCTION` unless evidence justifies domain-specific values. If it differs from the running pre-production/testnet value, the project performs a complete reset/rebuild with a newly derived genesis `R_sealed`, CE MDBX, Mongo projection, proofs, and topology identity before production. Because the old chain/state is discarded, the new chain may retain scheme 1 as its first active topology. Preserving existing state or changing K in place would instead require a new commitment-scheme version plus explicit recommitment/migration.

### Benefits

- K is chosen against the real complete workload rather than an intermediate tree microbenchmark;
- ADR-011–016 and off-chain resource contention participate in the decision;
- earlier protocol implementation proceeds with one simple value.

### Costs

- testnet may run with a suboptimal shard count;
- a different production value requires another complete pre-production reset/rebuild;
- performance conclusions about K are intentionally deferred until late in the roadmap.

## Accepted performance target

The selected conservative target is:

```text
measured samples >= 100 per accepted scenario/mode
failed samples    = 0
p99 latency       <= 1.0 second
maximum latency   <= 1.5 seconds
```

It applies to the gas/CE-work-saturated adversarial single-shard workload on the minimum validator profile. Uniform sharding speedup may rank already-safe configurations but never establishes the safety limit.

One issue intentionally remains open: whether the target gates execution/seal/state-root alone, the combined execution-to-finalized-MDBX-ready path, or two separately justified lifecycle budgets. ADR-017 must settle that from the real scheduling/finality path before using the numbers. It may not choose the easiest interval after seeing results.

If no candidate capacity passes, the project reduces capacity or optimizes and reruns. It does not silently relax the target, raise the hardware floor, or assume more shards fix concentrated work.

### Benefits

- substantial margin below the original two-second execution ceiling;
- tail and maximum latency are visible rather than hidden by a median;
- concentrated work, not ideal uniform distribution, sets safety capacity.

### Costs

- the gate is strict and may reduce usable CE capacity;
- cold-cache and contention sampling can take hours;
- maximum latency is sensitive to uncontrolled host noise, so run validity must be explicit.

## Reproducible benchmark contract

ADR-017 uses two layers.

Layer 1 is Criterion for narrow kernels: body/Poseidon hashing, shard aggregation, CKB update/proof/verify, worker scaling, staged encoding, and MDBX micro-costs.

Layer 2 is a deterministic optimized Rust full-path runner covering the selected measured lifecycle: exact-parent open, real body/key derivation, execution and overlays, shard/collection/catalog sealing, candidate publication, state-root notification, finalized persistence/marker advancement, proof contention, recovery/snapshot paths where relevant, and multi-node sustained load. Fixture construction and report generation stay outside measured intervals.

The intended operator command is:

```text
mise run bench-ce-performance
```

A narrow Linux helper may collect hardware state and perform privileged page-cache reset; benchmark logic and pass/fail classification remain in Rust, and the main benchmark process does not run as root.

Every run emits a versioned artifact set:

```text
benchmark-results/adr017/<run-id>/
  manifest.json
  hardware.json
  criterion/
  samples.jsonl
  summary.json
  report.md
```

The manifest records scenario IDs, deterministic dataset seed/checksums, body/state shapes, `K`, worker/concurrency settings, reader/cache envelopes, warmup/sample counts, expected roots/batch checksums, toolchain/source identity, and command arguments. Raw JSONL samples are the source for generated summaries/reports and include phase timings, CPU/RSS/I/O, proof and staged bytes/records, roots/checksums, and outcomes.

Uniform datasets derive real entity IDs/tree keys and select balanced ordered samples. Adversarial datasets deterministically scan valid derived keys until all selected keys map to one shard; synthetic suffix rewriting does not replace the full path. Cold evidence is valid only when the Linux cache reset is confirmed. The runner continues after a performance-rejected configuration but exits non-zero for correctness/infrastructure/incomplete artifacts or when no safe capacity exists.

### Benefits

- one reproducible command retains raw evidence and correctness checks;
- kernel and full-path regressions remain separately diagnosable;
- setup and fixture construction cannot silently contaminate measured intervals.

### Costs

- benchmark-only code and artifact schemas require maintenance;
- controlled cold runs need privileges and disrupt other host workloads;
- telemetry itself can perturb the result and must be calibrated.

## CE work model to validate

The preferred model carried forward from ADR-009 is:

```text
CE_WORK_USED =
    CE_SEAL_BASE_UNITS(active topology)
    + CE_TOUCHED_SHARD_UNITS * unique_touched_shards
    + CE_UNIQUE_KEY_UNITS * unique_keys

CE_WORK_USED <= CE_WORK_LIMIT
```

The base covers fixed parent/root/catalog/marker work. First-touched-shard units cover snapshot/job/proof/batch setup. Unique-key units cover worst-case CKB evidence, update/delete records, body/hash work assigned to CE, and cleanup.

Block and transaction meters keep deterministic shard/key sets. The empty-block transaction-fit check charges that transaction's unique shards/keys even if an earlier transaction already paid the block reservation. Reservation occurs before corresponding EVM/event writes with checked arithmetic. Included revert/net-no-op gives no refund; excluding the whole speculative transaction restores meter sets/usage with the EVM checkpoint. Worker count, completion order, cache state, and branch sharing never change consensus units.

### Benefits

- separates fixed, per-shard, and per-key cost;
- avoids charging shard setup once per key in concentrated workloads;
- maps naturally to benchmark decomposition without adding a mutation-count cap.

### Costs

- adds shard sets to transaction/block checkpoint state;
- coefficients remain conservative when keys share branches;
- proposer/validator/defer/revert parity testing becomes more complex.

The alternative `base + unique_keys` is simpler but overcharges concentrated work. Charging actual branch records after seal is rejected because capacity failure would be discovered after receipt-visible execution.

## Local worker/concurrency direction to validate

The carried-forward candidate design uses an explicitly owned bounded worker pool rather than a global Rayon pool:

```text
ShardPreparationPool {
    worker_count,
    max_inflight_seals,
    max_queued_jobs = max_inflight_seals * active_shards,
}
```

Sequential `worker_count = 1` is the differential oracle. Parallel and sequential roots, records, encoded size, CE units, and classifications must be identical. No `StorageHandle`, EVM journal, lifecycle context, or receipt crosses the worker seam.

The candidate concurrent-seal design acquires a seal permit before snapshots/allocation, isolates per-seal state/results, bounds the queue, drains admitted work at shutdown, and treats local saturation as proposer defer/forfeit, validator abstention, or import pause rather than invalid-block evidence. A single node-builder source should bind the Reth execution concurrency, in-flight memory, candidate cache, and MDBX reader envelope.

This remains a direction, not a fixed ADR-009 implementation. ADR-017 must first inspect the actual pinned Reth payload/import concurrency and compare always-sequential, serialized-seal, and bounded-concurrent-seal behavior. It must not introduce session IDs, fairness algorithms, or reader formulas without evidence they solve a real observed concurrency requirement.

### Benefits

- can exploit independent shard/collection jobs;
- explicit ownership and sequential equivalence preserve determinism;
- bounded concurrency can support competing payloads without global resources.

### Costs

- worker/session/queue/shutdown logic expands the local failure surface;
- resource needs can scale with `max_inflight_seals * active_shards`;
- concentrated workloads remain single-shard and gain no parallel speedup;
- wrong sizing can worsen p99 through CPU/cache contention.

## Candidate-cache envelope to validate

The carried-forward preferred local envelope is:

```text
CandidateCacheLimits {
    max_candidates,
    max_candidate_encoded_bytes,
    max_total_encoded_bytes,
    max_candidate_change_records,
    max_total_change_records,
}
```

Change records count changed-shard entries plus branch and leaf changes. Canonical bytes include root vectors, namespaces/prefixes, metadata, delete keys, and values. Bytes and records are separate because many small BTree entries can have higher resident overhead than their encoded form suggests.

These are local startup minima, not consensus mutation caps. ADR-017 derives them from `CE_WORK_LIMIT`, worst-case bytes/records per unit, the measured allocator/RSS multiplier, and the actual maximum candidate window. Production must be able to retain every protocol-valid candidate shape within that window; operators may provision more but not less. Insertion remains atomic and implicit eviction or speculative disk spill remains disallowed unless a later ADR changes recovery semantics.

### Benefits

- bounds both wire-like bytes and many-small-record memory shapes;
- prevents one candidate monopolizing the whole multi-candidate budget;
- makes CE capacity imply a testable minimum RAM envelope.

### Costs

- more counters/configuration and startup validation;
- conservative products may reserve excess RAM;
- allocator/collection changes can require remeasurement.

## Open decisions

ADR-017 must close all of the following from measured completed-system evidence:

1. the production `K` candidate matrix result and whether all existing domains retain one shared value;
2. the exact lifecycle interval(s) governed by the `p99 <= 1.0s` / `max <= 1.5s` target;
3. final body-byte, transaction, block, CE work, staged-byte/record, and local cache limits;
4. final worker count and whether parallel preparation is worth retaining;
5. whether concurrent seals exist in pinned Reth wiring and, if so, their exact bound/owner;
6. MDBX proof/finalization/recovery reader reserves;
7. candidate-window ownership and maximum count;
8. sustained multi-node throughput/soak duration and acceptance;
9. performance rerun triggers for hardware, hash/tree codec, persistence, allocator, or execution changes.

## Verification

- deterministic sequential/parallel root and batch equality;
- proposer/validator CE work and error equality;
- transaction fit, defer, revert, and no-refund boundaries;
- warm/cold concentrated and uniform workloads;
- concurrent proof/finalization/recovery and any real competing-seal workload;
- cache byte/record/RSS boundary tests with no implicit eviction;
- sustained multi-node testnet run at accepted limits;
- generated report reproducibility from the raw manifest/hardware/samples only.

## Reset and activation policy

Numerical limit changes are hard-fork/testnet-restart eligible before production but do not create a second commitment scheme unless they also change commitment semantics. No production activation may rely on the provisional values retained from earlier ADR stages.

## Next unlocked step

After the full ADR-001–016 path exists, implement/run this closure suite, review the generated evidence, and fix the final capacity/resource constants before production activation.
