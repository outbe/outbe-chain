# ADR-B-CAP-001: Resource metering and capacity closure

- **Status:** Proposed; current limits are fragmented and production capacity is not closed
- **Date:** 2026-07-17
- **Decision owners:** Blockchain Space, execution, consensus, persistence and node maintainers
- **Scope:** deterministic work, memory, bytes, queues and external-service budgets per transaction/block/request
- **Depends on:** ADR-B-WIR-001, ADR-B-GEN-001, ADR-B-CNS-003,
  ADR-B-EVM-005, ADR-B-TXP-001, ADR-B-CLI-001, ADR-B-MCP-001
- **Related:** ADR-B-OCD-007, ADR-B-OCD-008, ADR-B-OCD-010, ADR-B-OCD-011 and every module ADR with bounded iteration

## Context

Consensus safety requires more than an EVM block gas limit. Outbe execution includes
Rust precompiles, begin-zone system transactions, authenticated CE overlays and tree
proofs, synchronous body reads, cryptography, TEE calls, consensus codecs/queues and
asynchronous Mongo projection. Some work is charged through revm storage/subcall gas,
some is constrained by local constants, and some is currently unbounded.

A limit is meaningful only when it bounds the scarce resource before allocation or
expensive work, has one protocol-versioned authority, and is enforced identically by
admission, proposal, validation and replay. Independent “reasonable” constants do
not prove a block fits its execution/finality deadline or that a request cannot
exhaust a node.

This ADR owns the cross-subsystem capacity envelope. Module ADRs own their algorithms
and must export worst-case cost dimensions into this envelope.

## Decision

### Versioned capacity profile

Every network activates a `CapacityProfileV1` through its chain/protocol schedule.
The profile is consensus-visible where divergence could change block validity and
local-policy-only where it affects only service admission. It contains at least:

- block gas, bytes, transaction count, calldata, receipt/log and header-artifact
  limits;
- system-transaction reserved/visible/internal gas budgets;
- per-precompile selector base, per-byte, per-item, storage, cryptographic and
  external-read cost coefficients;
- CE mutations, touched collections/shards/keys, body bytes, proof nodes/bytes,
  provisional candidates and encoded cache bytes;
- validator/committee/DKG artifact and consensus message limits;
- actor mailbox count/byte capacities and backpressure policy;
- Mongo projection block/event/document/byte batch limits;
- RPC request, batch, response, scan/proof and concurrency limits; and
- TEE/feeder frame, concurrency, deadline and retry budgets.

Each value has a unit, enforcement owner, failure class and activation version.
Consensus-critical values are derived from one generated manifest; they are not
duplicated between genesis Python, Rust modules, CLI defaults and deployment files.

### Hierarchical transaction and block metering

The outer block meter reserves mandatory system work before admitting user
transactions. A transaction can consume only the remaining block envelope. Every
precompile call receives a child meter bounded by the caller gas and returns unused
gas/refunds using revm-compatible semantics. Nested calls cannot mint gas or charge
the same work twice.

For each selector, charged gas is a checked, monotonic function of all attacker-
controlled dimensions:

```text
base
+ calldata bytes and decoded element counts
+ storage cold/warm reads and writes
+ bounded loop/lookup steps
+ cryptographic operations and proof bytes
+ CE body/mutation/key/proof/tree work
+ external boundary work admitted by the call
```

The implementation validates lengths/counts and reserves worst-case gas before
decoding into large allocations, scanning, mutating or issuing an external call.
Unused conservative reservation may be refunded only by deterministic rules.
Wall-clock duration, cache hit/miss, Mongo latency and host CPU cannot change the
consensus result or charged amount.

System transactions use the same finite accounting vocabulary. Internal artifact
gas may be distinct from receipt-visible gas only if the profile reserves its full
worst case outside the user budget and proposer/validator apply byte-identical
rules. No “effectively infinite” internal gas limit proves capacity.

### CE work envelope

One transaction and one block have explicit upper bounds for:

- decoded entity/body bytes and canonical records;
- reads, creates, updates, deletes and retirement operations;
- unique keys, partitions, collections and shards touched;
- parent proofs and total proof/node bytes materialized;
- overlay/index/event/log entries and encoded bytes;
- sorting/hashing/SMT update work; and
- speculative candidates retained across payload attempts.

Limits are checked incrementally with checked arithmetic. Candidate eviction may
discard only non-authoritative speculative data and cannot invalidate an in-flight
sealed payload. Finalized state and the recovery marker are never evicted by a cache
policy. A rejected/OOG operation rolls back EVM journal, CE overlay, events and all
reservations together.

### Consensus and actor backpressure

Wire decoders reject oversized frames and collection counts before allocation. The
64-KiB `extra_data` ceiling is only one part of the block/message budget; encoded
block, transaction, certificate, DKG artifact and gossip frames each have explicit
limits compatible with network quotas and finality deadlines.

Every actor edge is either:

- a bounded count-and-byte queue with documented backpressure/fatal overflow; or
- a formally single-in-flight/acknowledged edge whose maximum accumulation is
  proven from upstream protocol state.

“Naturally low rate” is not sufficient for an unbounded queue. Consensus-critical
messages are never silently dropped; overload stops admission/proposal or fails the
node before memory exhaustion. Queue depth/bytes and oldest-message age feed
ADR-B-SUP-001 readiness.

### External and derived work

Consensus execution must not depend on an unbounded external service. If canonical
body bytes are read outside Reth/EVM state, input size and read count are bounded and
the bytes are authenticated before use. Availability failure produces the same typed
non-acceptance result on every path; a local timeout is not a valid-block predicate.

Mongo projection has a separate local work budget and backpressure path. It may lag
finality but cannot slow or alter canonical execution; exact-parent readiness gates
operations that require it. Projection processes one bounded canonical block unit or
sub-batches with a durable cursor that preserves block-atomic checkpoint semantics.

RPC, CLI, feeder and TEE boundaries enforce byte/count/concurrency/deadline limits
before work. Pagination limits cap both entry count and encoded response bytes.
Retries use bounded exponential backoff, cancellation and a total request/session
budget; they do not multiply unbounded queued work.

### Capacity derivation and release gate

For every activated profile, CI computes a worst-case block bill from reachable
module selectors and proves it fits CPU, memory, disk-write, network and finality
budgets on the minimum supported production machine. Benchmarks use adversarial
maximum-shape inputs, cold caches and real cryptographic/storage/external seams.

The release artifact publishes coefficients, benchmark environment, headroom and
the generated enforcement manifest. Adding a selector, persistent loop, proof shape,
system hook or wire field fails CI until its cost dimensions and boundary tests are
registered. Governance activation cannot increase limits beyond independently
verified implementation maxima.

## Authoritative interfaces

| Responsibility | Authority |
|---|---|
| Active consensus-critical limits | versioned `CapacityProfile` in protocol schedule |
| Selector cost formula | generated precompile/ABI gas manifest |
| EVM child-frame semantics | revm-compatible transaction/subcall meter |
| CE reservation and rollback | execution-scope CE work meter |
| Queue admission/backpressure | owning actor ingress contract |
| Local RPC/projection/TEE policy | node role profile bounded by protocol maxima |
| Production adequacy | reproducible worst-case benchmark and conformance suite |

## Invariants

- Every attacker-controlled loop, allocation, proof and response has a finite bound.
- Limits are checked before proportional allocation or expensive work.
- Gas/cost arithmetic is checked and cannot wrap or saturate into undercharging.
- Proposal, validation, import and replay use identical consensus-critical limits.
- User work cannot consume resources reserved for mandatory system/finality work.
- OOG/limit failure leaves no EVM, CE, event, cache-reservation or external partial
  consensus effect.
- No actor queue can grow without a proven finite protocol bound.
- Local overload cannot change validity or state root.
- A maximum valid block completes within the advertised finality budget with
  measured headroom on the minimum supported node profile.

## Atomicity, replay and failure

Resource reservation belongs to the same execution checkpoint as the operation it
funds. Deterministic exhaustion returns the selector's specified revert/halt or
invalid-block class and rolls back semantic writes. Fatal internal accounting
inconsistency aborts execution rather than converting to a user-visible soft failure.

Queue and external-service overload are local lifecycle failures unless a protocol
input itself violates a consensus limit. Replay charges the historical profile
active at that block, not current local configuration. Metrics and benchmarks may
observe actual work but cannot retroactively change charged gas.

## Compatibility and migration

Every cost table and consensus limit has a protocol version and activation height.
Changing a coefficient or maximum is a state-transition compatibility change even
when ABI bytes remain unchanged. Nodes retain historical profiles for re-execution.
Local service limits may be stricter only when they reject local requests/txpool
admission without causing validators to reject otherwise valid imported blocks.

## Production-interface verification evidence

Inspected revm subcall metering, precompile dispatch, storage gas plumbing, system
transaction gas planning/receipts, payload building, txpool policy, CE execution and
candidate cache, proof/scan limits, consensus header artifacts/network quotas and
actor ingress, TEE framing, Mongo projection and RPC pagination. Existing code has
useful isolated caps and gas parity tests, including a 64-KiB header-artifact limit,
one pending marshal acknowledgement, bounded scans and selected module loops.
However, default stateful-precompile CPU work is not comprehensively priced and key
production caches/queues remain unbounded. Status remains Proposed.

## Consequences

Capacity becomes part of protocol design rather than a late performance exercise.
Module ADRs can state precise bounded work, module audits can distinguish legal
maximum states from denial-of-service states, and operators can size nodes from a
published, reproducible envelope.

## Rejected alternatives

- **Rely only on EVM block gas:** Rust, CE, consensus, projection and external work
  are not automatically proportional to it.
- **Use wall-clock timeouts for consensus work:** heterogeneous machines can disagree
  on validity.
- **Assign every precompile a flat base charge:** equal calldata gas can trigger
  radically different loops, proofs and cryptography.
- **Keep unbounded queues because traffic is normally low:** failures and adversarial
  bursts are precisely when the assumption stops holding.
- **Tune production limits from average benchmarks:** safety depends on worst-case
  valid inputs and cold/recovery paths.

## Open questions and technical debt

1. **Critical:** `bin/outbe-chain` constructs production `CandidateCacheLimits` with
   both `max_candidates` and `max_encoded_bytes` set to `usize::MAX`. Replace these
   with finite profile values and prove safe eviction/in-flight behavior.
2. **Critical:** most stateful precompiles use the flat `PRECOMPILE_BASE_GAS = 200`.
   Storage/subcall gas alone does not price calldata decoding, Rust loops, CE proof/
   hashing work, cryptography or authenticated external body reads.
3. **Critical:** executor, finalization and peer-manager paths use unbounded actor
   channels justified by expected low rates. Establish count-and-byte bounds,
   backpressure/fatal semantics and depth/age metrics.
4. `SYSTEM_TX_ARTIFACT_GAS_LIMIT` is `10_000_000_000`, far above common 30-million
   block fixtures. Prove finite worst-case internal work and its reservation against
   block time rather than treating a large ceiling as metering.
5. Build one generated selector inventory. Current base-gas overrides cover selected
   heavy stateless functions, but no evidence proves every callable selector and
   dynamic dimension has a cost formula.
6. Audit every module loop against both an explicit count bound and proportional gas.
   Examples of local caps exist (staking compaction, Nod qualification, Oracle
   backfill, pending votes), but they are not tied to one block capacity proof.
7. `cleanup_inactive_validators(max_removals)` documents `0 = unlimited`; prohibit an
   unbounded consensus call or charge/prove the maximum validator-set traversal.
8. Define per-transaction/per-block CE limits for body bytes, events, mutations,
   touched keys/collections/shards, retirements, proof nodes and encoded overlay
   bytes. Page limit `1024` is a read API bound, not an execution-work envelope.
9. Prove CE reservations roll back with EVM journals on every revert, halt, panic,
   rejected payload and nested precompile call, including cache byte accounting.
10. `OUTBE_MAX_EXTRA_DATA_SIZE = 64 KiB` bounds only header artifacts. Define maximum
    encoded block, transaction count/size, receipt logs/bloom work, certificate and
    DKG/broadcast frames end to end.
11. Commonware channel quotas are configured separately per channel. Generate them
    from the capacity profile and prove they admit the maximum valid block/artifact
    while rejecting resource-amplifying traffic.
12. Add pre-allocation decoder limits throughout ABI, consensus, DKG, TEE, snapshot
    and RPC codecs; post-decode length checks are too late for memory exhaustion.
13. `MAX_FRAME_LEN = 64 KiB` protects the TEE codec, but concurrency, outstanding
    requests, enclave compute, socket buffering and end-to-end deadlines need bounds.
14. Define authenticated body-read count/bytes and behavior under Mongo/unavailable
    storage. Consensus must not use machine-speed timeout as a validity decision.
15. Mongo projection needs event/document/byte batch limits and a durable sub-batch
    design that preserves exact block checkpoint atomicity for maximum blocks.
16. Offchain scans cap entries at `1024` and values per page at 8 MiB; prove all
    callers enforce total response/proof/body bytes and pagination cursors cannot
    cause quadratic rescans.
17. Audit RPC batch size, request/response bytes, proof construction, concurrency and
    expensive `eth_call`/trace methods. Module-level page caps do not protect the
    server as a whole.
18. ZeroFee removes price-based spam pressure. Reconcile sponsored gas/calldata and
    per-block soft-failure caps with CPU, CE and external-work costs, not only signed
    gas limits.
19. Add checked arithmetic to every cost formula. Existing `saturating_add` in some
    block/system accounting paths can conceal an impossible bill; overflow should
    fail deterministically before state publication.
20. Define mandatory system-work reservation when dynamic committee size, DKG,
    Oracle, Cycle and hook event volume all reach their legal maxima in one block.
21. Build adversarial maximum-shape benchmarks for every selector/system hook and an
    aggregate maximum block using cold Reth/MDBX/Mongo/TEE paths.
22. Publish minimum hardware, finality deadline, CPU/memory/disk/network budgets and
    required headroom. Constants without an empirical service envelope do not prove
    production capacity.
23. Add CI that rejects new selectors, loops, collection decodes, queue edges and
    protocol fields without registered units, enforcement tests and benchmark rows.
24. Define activation/migration rules for gas schedule and limits and retain every
    historical profile required for block replay and snapshot verification.
