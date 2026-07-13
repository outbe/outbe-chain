# T23 — Tribute/Nod domain adapters and generators (Gem deferred)

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §4.1, §4.3, §6.2, §18 (Q3, Q6, Q23)
Depends on: T05, T06, T07, T08, T09, T10, T29 (write-path profile constraints), T30 (registry/schema values), T35 (approved body-source/feasibility artifact)
Blocks: T20 (Part B adapters), T22 (Part B validator-path index seeding), T24 (benchmark needs registered schemas/K_domain entries), T27, T33

## Summary

Register the two Stage 1 genesis domains and wire their runtimes into the store: Tribute (Partitioned by
WWD, retirement-capable) and Nod (Singleton), each with canonical DAG-CBOR schemas, deterministic ID
generators, and fork-designated entrypoints. GEM IS DEFERRED (owner decision): Gem/GemFactory/GemLifecycle
stay on their current legacy EVM storage, untouched by Stage 1; Gem onboards later as a fork-activated new
domain (§16.2 cheap extension) with its own task packet — this also removes the GemLifecycle raw-hook
conversion from Stage 1 scope.

## Context

Tribute v1: `partition_key = tribute_id[0..4]` as canonical BE `wwd_id: u32`; core rejects mismatch;
retired WWD partitions have permanent non-reuse enforced by Tribute's monotonic lifecycle in domain-owned
consensus state (core stores no retirement tombstone). `TributeFactory` processes the encrypted offer and
`Tribute` supplies canonical `TributeData` — no special storage mode. Domain burn maps to generic delete.
Domains complete all validation/computation before calling the store; the store treats bodies as opaque
canonical bytes. Generators: deterministic, consensus-visible inputs only, lifetime-unique, never reproduce
a prior raw_id (including deleted); counter/nonce writes share the mint journal.

## Design-first checkpoint (moved to gate T35 — audit-final B-07)

The consensus-state design decisions formerly approved inside this task — body-source tables, generator
persistent-state contracts (Nod lifetime tombstone/monotonic guard, Tribute enclave-ID uniqueness), and
the WWD retirement aggregate contract — are OWNED by gate
T35 and approved BEFORE T30 freezes body schemas and generator algorithms (feasibility precedes the
freeze). Implementation here begins only after the T35 artifact (`docs/ces-body-source-matrix.md`) is
approved; this task implements it without re-deciding.

## Scope

- Registry entries (T06) for tribute/nod v1: encoding kinds, partition policies, `K_domain` values
  (provisional until Q11 selects exact counts), schema/hash versions, gas profiles, activation height 0.
  Single owner (audit-final B-09): T06 provides the generic registration mechanism and registers only
  test fixtures; the CONCRETE Tribute/Nod genesis entries and generator bindings land HERE.
- Canonical schemas via T05 descriptors: `TributeData` and Nod body layouts (field order fixed; migration
  of current struct definitions into canonical arrays). Domain input text is NORMALIZED here per the T30
  rule before encoding (audit-final M-09; T05 validates canonicality).
- ID generators per domain with vectors: determinism, collision/repeat rejection, failed-mint rollback of
  counter/nonce writes (§19.8).
- Entrypoint wiring through T09 guard: mint/update/burn paths from the existing tribute/nod runtimes to
  `CompressedEntityStore`; Tribute WWD retirement calling `retire_partition` under its lifecycle rules.
- This is a PORT, not a parallel path: the write path moves onto the CES engine — record bodies live only
  in CES from genesis; the legacy per-record EVM body schemas gain no new writers. Greenfield genesis
  (§17.1) means no data migration, only a code migration. Deletion of the legacy schemas and the read-path
  port is T27 (same release; a binary shipping both engines as live storage is not an outcome).
- FULL producer scope (audit P0-2/§8): the port covers ALL CES mutation producers — tribute/tributefactory
  user paths, **Lysis** (bulk NOD mint + Tribute delete via receipt-visible system txs with a
  deterministic cross-block cursor, per the T09 inventory) and **NodFactory**. (GemFactory/GemLifecycle
  are out of Stage 1 — Gem deferred.)
- Body-dependent Mongo READS are NOT this task: they belong to T33 (Variant A runtime adapters) — this
  removes the former T23↔T20 Part B cycle (audit_plan_v2 P0-3). T23 owns schemas, generators, and CES
  WRITE/system-tx wiring only. Writer flows that need a prior body consume a NEUTRAL `VerifiedBodyInput`
  port defined HERE (audit-final B-04): pure body constructors take verified canonical bytes and are
  tested with fixtures/stubs; ACQUISITION (Mongo read, checkpoint rules, leaf verification) and the
  runtime integration `reader → constructor → writer` are T33's; Mongo-dependent producer E2E lives in
  T33/T25.
- Generator closure (audit P1-0): per-domain proof that the concept's lifetime-uniqueness invariant holds
  in the CONCRETE generator + domain state: Nod — `keccak(nod||owner||wwd)` repeats after burn unless
  domain state keeps a lifetime tombstone/monotonic guard → define it; Tribute — enclave-produced ID needs a
  consensus-visible uniqueness contract. Each with collision domain, rollback semantics (counter/nonce
  writes share the mint journal), and persistent non-reuse state named in acceptance evidence.
- Tribute retirement aggregates: how supply/day/owner domain aggregates update on atomic WWD retirement
  WITHOUT O(N) per-entity deletes; product/ERC721-surface consequences (ownerOf/balanceOf/totalSupply/
  enumeration/tokenURI) recorded in the port map shared with T27.
- Body-source table per mutating entrypoint (artifact owned by gate T35; this task implements it): core never reads an old body (§3.1) and has no patch operation,
  so every update must construct the COMPLETE new canonical body. For each existing mutating entrypoint,
  document `transaction inputs + domain-owned consensus state → every field of the new body`; where a legacy
  flow relied on reading the stored record to fill unchanged fields, name the replacement source: widened
  tx input, a retained domain-owned field, or — under Stage 1 Variant A (T29/T33) — the VERIFIED Mongo
  body read (testnet-only, per-operation, recorded in the T33 matrix). This is the go/no-go check that the
  port is possible per flow.

## Out of scope

- Domain economics/business rules changes; new domain features; media policies.

## Acceptance criteria

1. Execution-level tests through store + canonical events: Tribute mint→update→burn, Nod mint→burn,
   Tribute WWD retire — body-needing writer flows tested against the STUB `VerifiedBodyInput` port
   (audit-final B-04: no Mongo dependency in T23 acceptance). (Full E2E with proof verification and Mongo
   projection runs under T25 once T18/T20 land — not a T23 gate.)
1b. Body-source tables (T35 artifact) implemented for every mutating entrypoint; each update flow demonstrably constructs its
   complete body from inputs + domain state (per-flow test).
2. Partition rule tests: wrong `wwd_id` prefix rejected; retire of a live WWD follows domain policy; retired
   WWD non-reuse enforced by the Tribute lifecycle (attempted regeneration rejected in domain state).
3. Generator vector suites per domain (§19.8) incl. failed-mint rollback.
4. Schema golden vectors for all three body layouts (T05 grammar).
5. Never-populated partition lifecycle (postfix PF-H09, minimal form): a domain-opened partition with
   zero mints retires domain-state-only — no core call, no `PartitionRetiredV1`; the domain's monotonic
   WWD lifecycle still forbids its reuse (tested).

## Invariants

- Domain adapters tighten but never bypass generic mutation rules; all mutations flow through the store.

## Tests

- Execution-level e2e per domain; localnet smoke with all three domains active.

## Files

- `crates/core/{tributefactory,tribute,lysis,nod,nodfactory}/…` (adapter wiring — ALL Stage 1 mutation
  producers; gem/gemfactory deferred)
- executor/system-tx wiring for the Lysis receipt-visible paths (`crates/blockchain/evm`)
- `crates/core/compressed_entities/src/registry.rs` (genesis entries)
- `docs/ces-body-source-matrix.md` (approved T35 artifact — normative INPUT, not produced here)
