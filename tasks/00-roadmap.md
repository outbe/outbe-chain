# Compressed Entity Storage v6.1 — Implementation Roadmap

Source spec: `compressed_entities_concept_v6_proposed_10-07-2026.md` (rev 1700 lines, Q1–Q23; Q11 numerically provisional).
Review trail: `compressed_entities_v6_proposed_review_12-07-2026.md` (r1), `_r2_`, `_r3_` — all HIGH/MEDIUM findings closed in the spec.
Architecture diagram: `compressed_entities_v6_architecture.html` (note: predates the Q23 collection/Root-Catalog model; flat-256-shard picture is outdated).

The spec is a concept, not an implementation specification. Each task below is scoped so its acceptance
criteria are verifiable against the spec section it implements. §19 acceptance-evidence items are mapped
into the tasks that own them; cross-cutting evidence lives in T25.

## Model summary (what we are building)

```text
domain (registry: partition policy, K_domain = 2^k, versions)
  └─ collection (Singleton: 1 per domain | Partitioned: per partition_key, e.g. Tribute WWD)
       └─ K_domain shard SMTs (vendored CKB, Poseidon-BN254 merge codec)
            → collection_top (log2(K_domain) levels) → R_collection
                 → Root Catalog SMT (collection_key → R_collection)
                      → R_sealed(B) → EVM slot 0xEE0B.slot1 + header artifact tag 0x08
```

Mutations: `mint / update / delete / retire_partition`, journaled overlay at `0xEE0B` (slots 0–5),
canonical receipt events (`WriteV1` full-body / `DeleteV1` / `PartitionRetiredV1`), end-block seal as the
last `BlockLifecycle` module returning typed `SealOutput`. Persistence: CE-owned MDBX committed only after
the durable-Reth barrier, ACK-gated to Marshal (`MAX_PENDING_ACKS = 1`). Projections: finalized-gated ExEx
→ MongoDB (runs in Docker for dev/test). Reads: point-proof packages (shard + collection-top + catalog
proofs) verified against finalized headers.

## Phases and dependency graph

```text
Gates (audit_plan.md/v2; decision/spec tasks that unblock implementation)
  T29 Gate D0: Variant A body-dependent execution profile (testnet)   → blocks T09, T20b, T23, T27, T30, T33, T34, T35, T36
  T35 Gate: body/generator/aggregate feasibility preflight            ← T29; blocks T23, T30 (feasibility PRECEDES the schema freeze — audit-final B-07)
  T36 Gate: read-surface product decisions (port map)                 ← T29; blocks T26, T27, T30 (decision PRECEDES the list-RPC freeze — audit-final B-03)
  T30 Gate D1: CES1 wire & schema spec (PROVISIONAL_Q11 K values;
      final K is a T24B output — no final-K cycle)                    ← T29, T35, T36; blocks T02, T05, T06, T08, T13, T18, T19, T20, T22, T23, T24 (B re-baseline), T26, T31, T33, T34
  T31 Gate D2: provisional resource bounds (pre-Q11; incl. B-10
      read bounds: Lysis pages, point-read reservation)               ← T30; blocks T07, T10, T12, T20, T24 (B re-baseline)
  T32 Gate D3: Reth persistence feasibility spike (read-only)         → blocks T16, T17

Gate convention (audit-v2 P1-4): a gate is COMPLETE when its own artifact (spec/contract/report) is
published — "gate artifact complete". Downstream compliance tests belong to the implementing tasks'
acceptance criteria — "downstream compliance verified". Gates never wait for the tasks they unblock.

Phase 0  Foundations (parallel, no chain deps)
  T01 CES1 Poseidon primitives + tag registry
  T02 Identity & leaf derivation; owns registry-descriptor types       ← T01, T30
  T03 Vendored CKB SMT + typed merge codec                             ← T01   (parallel with T04)
  T04 Reference model: collection top, Root Catalog, R_sealed          ← T01, T02 (differential gate joins T03+T04)
  T05 Canonical body codec (strict DAG-CBOR)                           ← T30

Phase 1  Core store module
  T06 Domain registry (partition policy, K_domain, versions)           ← T02, T30
  T07 Store core: 0xEE0B layout, overlay, lifecycle ops                ← T02, T05, T06, T31
  T08 Canonical events (3 forms)                                       ← T07, T30
  T09 Entrypoint dispatch guard (CALL-only, system path)               ← T06, T07, T08, T29
  T10 Attempt/gas guard + provisional D2 bounds (Q15 + T31 values;
      read reservations, reserved-totals summary, outcome arbiter)     ← T07, T08, T09, T31

Phase 2  Seal & consensus integration
  T11 BlockLifecycle typed EndBlockResult refactor
  T12 End-block seal (run_end_block_seal → SealOutput)                 ← T03, T04, T07, T10 (reserved totals), T11, T15 (read-view API), T31
  T13 Header artifact tag 0x08 + validator equality checks             ← T12, T30
  T14 Genesis activation: A plumbing ← T04, T13, T15, T16; B testnet re-baseline ← T24B
  T15 CE-owned MDBX environment (namespaced shards + Root Catalog)     ← T03
  T16 Finalized persistence coordinator (barrier → commit → ACK)       ← T12, T15, T32
  T17 Restart matrix & crash recovery                                  ← T08, T16, T32

Phase 3  Reads & projections
  T18 Proof/read module (point-proof package, single-snapshot)         ← T04, T06, T13, T15, T30
  T19 outbe_getBody RPC (proof_encoding_version negotiation)           ← T18, T21, T30
  T20 ExEx→Mongo: A generic ledger/current+recent-version rows/cursor ← T08; B domain adapters ← T23, T29
      (Variant A: Part B is a testnet execution prerequisite — local liveness, never authority)
  T21 Body store: wait-only policy + RecoverySource (peer src post-T19) ← T15, T18, T20
  T22 Snapshot: A spec/reference-exporter ← T30; B implementation      ← T15, T17, T20 (baseline API + B adapters), T21, T23 (validator-path index seeding)
  T26 Secondary-index list RPC over MongoDB                            ← T18, T20 (B), T21, T30, T36
  T28 Snapshot serving transport + operator CLI (HTTP/object-store)    ← T22, T33 (validator-readiness scenario)

  T26/T28 are integration/operational support, not consensus primitives.

Phase 4  Domain wiring + Variant A runtime
  T23 Writers ONLY: schemas, generators, CES write/system-tx wiring
      for ALL Stage 1 producers (tribute/lysis/nod+factory;
      GEM DEFERRED to a future onboarding fork, §16.2)                 ← T05..T10, T29, T30, T35 (full E2E under T25)
  T33 Body-read execution adapters + post-finalization catch-up
      (readiness machine READY/DEGRADED_KEY/NOT_READY(+CATCHUP),
      bounded automatic recovery + honest manual fallback,
      Lysis ordering, non-catchable outcomes)                          ← T07, T09, T10, T18, T19, T20 (B), T21, T23, T29, T30, T34
  T27 Legacy storage removal + read-path port to CES                   ← T19, T23, T26, T29, T33, T36
      (T27 is integration/operational support; T33 is an EXECUTION prerequisite — audit v5 P1-9)

Phase 5  Activation evidence
  T34 Stage 1 release/soak plan: A hardware profile + benchmark/
      restore/rollout protocol (blocks T24); B soak plan               ← T29, T30
  T24 Q11 benchmark: A harness/candidate search; B final constants +
      T30 registry update + FULL re-baseline
      (T02,T04,T12,T14,T15,T17,T18,T20,T21,T22,T23 + e2e);
      pass = execution-only < 2 s, through-ACK reported;
      host verified against the approved T34 profile ID (fail-closed) ← T10, T12, T16, T18, T23, T30, T31, T34 (hardware/protocol)
  T25 Cross-cutting acceptance suite + testnet soak                    ← T01–T24, T26–T36
      Blocks: STAGE 1 TESTNET activation. The production/mainnet gate
      is a separate OPEN placeholder: it requires the future off-chain
      computation design + its own acceptance evidence (Variant A
      results never close it).
```

Convention: every task implicitly blocks T25 (release gate); task-file `Blocks:` headers list DIRECT
consumers only and are maintained as the strict inverse of the `Depends:` headers.

Stable gate/design artifact paths (audit v5 P1-12): `docs/ces1-wire-spec-v1.md` (T30),
`outbe-plan/ces-resource-bounds-provisional.md` (T31), `outbe-plan/ces-persistence-spike.md` (T32),
`docs/ces-mutation-producer-inventory.md` (T09), `docs/ces-body-source-matrix.md` (T35),
`docs/ces-read-surface-port-map.md` (T36), `docs/ces-stage1-testnet-release-plan.md` (T34).

## Definition of Ready (audit_plan.md §9 — required before a task moves from todo to implementation)

1. Normative inputs: exact versions, constants, formats, source-of-truth (T30 for wire surfaces).
2. State ownership: consensus / derived / projection-only / ephemeral classified.
3. Exact API: Rust/ABI/RPC types, errors, version negotiation.
4. Bounds: bytes, counts, memory, depth, concurrency, timeout/decompression (T31 until Q11).
5. Trust source: what is trusted and how the checkpoint/header is verified.
6. Failure semantics: revert/abort/retry/defer/halt/resync and the atomicity boundary.
7. Determinism: ordering, sorting, parallel error selection, proposer/validator parity.
8. Dependencies: real direct edges only; `Blocks` strict inverse.
9. Acceptance evidence: concrete tests, fixtures, commands, artifact paths.
10. Docs impact: README, module README, audit/debt, operator migration notes.

## Recommended start order (audit_plan.md §11)

1. Close D0 (T29) → 1b. Decision gates T35 (body/generator feasibility) + T36 (read-surface port map)
   + T34 Part A (hardware profile + benchmark protocol) → 2. Close D1 (T30) → 3. In parallel: T01 → T02 → T04, with T03 alongside (T04 needs T01+T02; differential gate joins T03+T04 —
   audit-v2 P1-8); T11 + T15 API design early →
4. Close D2 (T31) → 5. T02/T05/T06/T07/T08/T10 on approved schemas/bounds → 6. D3 spike (T32) →
7. Seal/artifact/persistence/restart (T12–T17) → 8. Domain writers + receipt-visible system mutation
paths (T09/T23) → 9. Generic projector, then domain index adapters (T20 A→B) → 10. Proof/body wire +
resolver + RPC (T18/T21/T19) → 11. Variant A runtime adapters (T33 — before any legacy removal) →
12. Snapshot spec then implementation/transport (T22 A→B, T28) → 13. Benchmark, freeze final K/limits,
regenerate all vectors/genesis (T24, T14 freeze) → 14. Legacy cutover (T27) → 15. Cross-arch/crash/fuzz/
soak release gates (T25). T34 (release plan) starts immediately after T29, in parallel with everything.

Stage discipline: everything proven under Variant A is TESTNET evidence; the production/mainnet gate
requires the future off-chain-computation design (tracked in T25).

## Milestones

| Milestone | Contents | Exit criterion |
|---|---|---|
| M0 Crypto proven | T01–T05 | M0-scoped golden vectors green (tags/keys/merges/body — §19.2's event/artifact/proof vectors close at M1–M3 with T08/T13/T18); reference model + differential SMT green (§19.1, §19.3) |
| M1 Store works | T06–T10 | mint/update/delete/retire + events + guards fully unit-tested incl. revert/OOG suites (§19.5, §19.7, §19.9; store-interface half of §19.6 — the seal-side half closes at M2 with T12; §19.8 generator vectors close at M4 with T23) |
| M2 Chain seals | T11–T17 | localnet seals blocks with R_sealed; local proposer≡validator equality (§19.4 cross-architecture leg closes at M5); genesis rehearsal (§19.15); crash-matrix tests — partial, Mongo-side crash points and the consolidated sweep close at M3/M5 (§19.11) |
| M3 Reads verify | T18–T22 | point proofs verify externally; ExEx rebuilds Mongo from scratch; snapshot bootstrap round-trip at the library level (§19.10, 12, 19; postfix PF-L03 — T26/T28 close in M4: T26 ← T36/T20B and T28 ← T33 depend on M4 inputs) |
| M4 Domains live | T23, T26, T27, T28, T33 | Tribute (partitioned) + Nod (singleton) mint/burn/retire end-to-end on localnet; body-read execution adapters live (T33 is an execution prerequisite, not integration support); Tribute/Nod fully ported to CES — their legacy body storage deleted, views/CLI/MCP on CES reads; Gem module untouched on legacy storage (§19.8) |
| M5 Stage 1 Testnet Activation | T24, T25, T34 | benchmark report < 2 s target; Q11 closed; soak evidence per T34 plan incl. finalized-candidate recovery (§19.13–14, 17–18); production gate remains OPEN (future off-chain computation design) |

## Standing constraints (apply to every task)

- No `unwrap/expect/panic/assert` in runtime paths; structured errors (repo Safety Rules).
- No `f32/f64`, no `HashMap/HashSet` on consensus paths; `BTreeMap`/`BTreeSet`; no narrowing `as` casts.
- All persistent access through explicit `StorageHandle`; facades short-lived (repo StorageHandle Rules).
- Determinism proposer ≡ validator for every consensus-visible computation.
- Module structure per the repo's Runtime Module Structure Standard (in CLAUDE.md; postfix PF-L01 —
  `.ruler/module_structure.md` does not exist in this checkout) — schema/state/runtime/precompile/
  lifecycle split.
- `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run` before PR.
- README.md updates ride in the same PR when user-visible surface changes (Documentation Contract).
- MongoDB for dev/test runs in Docker (`docker compose` service + ephemeral container harness in tests);
  production deployment choice stays operator-level and out of scope.

## Task index

| # | File | Title |
|---|---|---|
| T01 | `01-poseidon-ces1-primitives.md` | CES1 Poseidon primitives and normative tag registry |
| T02 | `02-identity-derivation.md` | Canonical identity: id_f, collection_key, tree_key, leaf_f |
| T03 | `03-vendored-ckb-smt.md` | Vendored panic-sanitized CKB SMT with typed Poseidon merge codec |
| T04 | `04-reference-model.md` | Independent reference model: collection tops, Root Catalog, R_sealed |
| T05 | `05-canonical-body-codec.md` | Strict DAG-CBOR canonical body codec |
| T06 | `06-domain-registry.md` | Fork-governed domain registry with partition policy |
| T07 | `07-store-core.md` | CompressedEntityStore: 0xEE0B overlay + generic lifecycle |
| T08 | `08-canonical-events.md` | Canonical mutation events (WriteV1/DeleteV1/PartitionRetiredV1) |
| T09 | `09-entrypoint-guard.md` | Entrypoint dispatch guard and system-tx mutation path |
| T10 | `10-attempt-gas-guard.md` | CE attempt counters, gas charge, payload-builder contract |
| T11 | `11-blocklifecycle-typed-result.md` | BlockLifecycle associated EndBlockResult refactor |
| T12 | `12-end-block-seal.md` | run_end_block_seal: staged batches, Root Catalog, SealOutput |
| T13 | `13-header-artifact-0x08.md` | Header artifact tag 0x08 and validator root equality |
| T14 | `14-genesis-activation.md` | Genesis alloc, R_sealed(0) derivation, height-0 CE marker |
| T15 | `15-ce-mdbx-environment.md` | CE-owned MDBX environment and atomic marker commit |
| T16 | `16-persistence-coordinator.md` | Durable-Reth barrier → SMT commit → Marshal ACK |
| T17 | `17-restart-recovery.md` | Restart matrix, crash recovery, parent-root verification |
| T18 | `18-proof-read-module.md` | Point-proof package assembly and verification |
| T19 | `19-outbe-getbody-rpc.md` | outbe_getBody RPC with absent/unavailable/unsupported |
| T20 | `20-exex-mongodb-projector.md` | Finalized ExEx → MongoDB projector (Docker harness) |
| T21 | `21-current-body-store.md` | Current-body store, per-key verification, recovery |
| T22 | `22-snapshot-format.md` | Snapshot format v1: profiles, manifests, staged import |
| T23 | `23-domain-adapters.md` | Tribute/Nod domain adapters and generators (Gem deferred) |
| T24 | `24-q11-benchmark.md` | Q11 worst-case benchmark harness and numerical closure |
| T25 | `25-acceptance-suite.md` | Cross-cutting acceptance evidence and testnet soak |
| T26 | `26-secondary-index-list-rpc.md` | Secondary-index list RPC over MongoDB (by_owner/by_wwd, unverified lists + optional proofs) |
| T27 | `27-legacy-read-surface-cutover.md` | Legacy storage removal + read-path port to CES (tribute/nod views, outbe-cli, MCP; Gem untouched) |
| T28 | `28-snapshot-transport-cli.md` | Snapshot serving transport and operator CLI (export/import/bootstrap, mirror failover) |
| T29 | `29-gate-d0-variant-a-testnet-profile.md` | Gate D0: Variant A body-dependent testnet execution profile (Stage 1) |
| T30 | `30-gate-d1-ces1-wire-spec.md` | Gate D1: CES1 normative wire and schema specification |
| T31 | `31-gate-d2-resource-guards.md` | Gate D2: conservative provisional resource bounds (pre-Q11) |
| T32 | `32-gate-d3-reth-persistence-spike.md` | Gate D3: Reth persistence feasibility spike (read-only) |
| T33 | `33-variant-a-runtime-adapters.md` | Variant A runtime adapters: all body-dependent Mongo reads, readiness state machine, typed unavailability outcomes |
| T34 | `34-stage1-release-plan.md` | Stage 1 testnet release/soak plan (Part A: hardware profile + benchmark/restore/rollout protocol; Part B: soak) |
| T35 | `35-body-feasibility-preflight.md` | Gate: body/generator/aggregate feasibility preflight (body-source matrix, generator uniqueness, ActiveTributePartitionsView) |
| T36 | `36-read-surface-decision-gate.md` | Gate: read-surface product decisions (port map: (a)/(b)/(c) classification, list-RPC surface) |
