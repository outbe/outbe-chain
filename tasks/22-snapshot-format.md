# T22 — Snapshot format v1: profiles, manifests, staged import

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §14 (Q10, Q16, Q17)
Depends on: T30 (Part A: normative encoding); T15, T16 (shared DB-only activation verification — R4), T17, T20 (baseline-init cursor API + Part B adapters for index seeding), T21, T23 (Part B: index memberships decode via the same projection adapters — audit-final H-04)
Blocks: T28

## Summary

Implement the semantic snapshot format v1: logical leaf/body records over collections, versioned profiles
(`tree`, `tree-with-bodies`, and an optional non-gating `full-current-body` extension), manifests/chunks
with multi-source recovery, staged import with activation gates, and the bootstrap path (snapshot @ H +
projection baseline + event replay to head).

## Context

The format is semantically deterministic, not a DB image: conforming producers represent the same logical
records for the same `{checkpoint, format_version, profile, body_coverage}`. Records: leaf
`{collection_key, shard_index, tree_key, leaf_value}` (shard_index must equal the derived index — mismatch
rejects); body `{domain_id, partition_key_or_none, schema_version, hash_version, id bytes, body bytes,
expected tree_key, expected leaf_value}`; ranges
`{profile, payload_kind, collection_key, shard_index, start_key, end_key}` with strict ordering/uniqueness.
Internal SMT nodes do not exist in format v1 (no acceleration section; always rebuilt from leaf records).
Import writes into staging; activation requires
checkpoint identity + reconstructed root to match the independently selected finalized header. Activation
runs THE SAME DB-only verification function T16 uses per block (R4, owner decision — one verification
path, not a hand-rolled subset): durable block at H with the EXACT snapshot hash, durable receipts, EVM
`0xEE0B.slot1` equal to the snapshot root, artifact/scheme binding, plus the T15 identity binding
(chain_id/genesis/scheme) — height alone cannot prove same-chain (postfix PF-H06). Manifests
are producer-local; failover happens at logical-range boundaries; parsers impose explicit resource bounds.

## Structure (audit P0-8): two parts

- **Part A — format spec + reference exporter** (Depends: T30 wire spec): the normative snapshot v1
  encoding lives in the T30 CES1 Wire Specification; Part A builds the REFERENCE exporter + fixture
  generator directly from that spec (independent of the production code paths).
- **Part B — production implementation**: production exporter over T15 snapshots, importer, staged
  activation, transport seam for T28. The §19.12 "two independent exporters" AC is satisfied by the
  named pair: reference exporter (Part A) vs production exporter (Part B). The VALIDATOR bootstrap path
  (`tree-with-bodies` with index seeding) additionally depends on T20 Part B + T23 (audit-final H-04):
  imported bodies build `by_owner`/`by_wwd` memberships through the SAME projection adapters used by
  event replay — no snapshot-only index builder exists.

## Scope

- Wire format v1 codec (records, ranges, header) with strict ordering/uniqueness/continuation validation,
  including the collection-descriptor record (postfix PF-B06): one versioned
  `{domain_id, partition_key_or_none}` descriptor per present Root-Catalog leaf, letting the importer
  derive `collection_key`, resolve `K_domain` via the registry, and reconstruct EMPTY-but-present
  collections (ZERO-top `R_collection`, zero entity leaves) that leaf/body records alone cannot
  represent; orphan/duplicate/missing-descriptor imports reject.
  Body-record identity is normatively `{domain_id, partition_key_or_none, schema_version, hash_version,
  id_bytes, body_bytes, expected tree_key, expected leaf_value}` — **spec amendment #2 (§14.2) — APPLIED**:
  the partition field lets the importer derive `collection_key`/`tree_key`, and the version fields let it
  recompute `leaf_value` under §16.1 multi-version readability (mirrors the §10.3 package; pairs with the
  T08 event amendment).
- Boundary with T28: T22 owns codec, exporter, importer, staged activation, and an ABSTRACT range-source
  interface (in-memory/local-dir sources for tests); T28 owns the concrete HTTP/object-store transport,
  fetch client, CLI, and end-to-end multi-source scenarios.
- Acceleration section (within §14.2's MAY latitude): DECIDED for format v1 — no acceleration section
  exists; internal SMT nodes
  are always rebuilt from normative leaf records, and the importer rejects unknown section kinds fail-closed
  (a future format version may add it under §14.2's discard-and-rebuild semantics). The §19.12
  `lazy missing-node` gate is accordingly interpreted post-import: a node-access failure in the rebuilt
  tree is local corruption → T15/T17 recovery, covered by an explicit test.
- Exporter over T15 snapshots for the `tree` and `tree-with-bodies` profiles (release-gating: these
  suffice for every bootstrap/soak gate) incl. `body_coverage` declaration. The `full-current-body`
  profile with its one-time streaming leaf-to-body merge is OPTIONAL, matching the concept's "an
  implementation MAY expose" (§14.3 item 3) — not release-gating. Partial/lazy body bundles (§14.3 item 4)
  use a distinct profile/coverage and artifact identity and make NO claim that the imported node can
  immediately serve every current body.
- Signing-material exclusion (§14.5): the portable format structurally CANNOT carry validator private keys,
  signer state, node identity, live locks, or ephemeral caches — no record kind exists for them; the
  exporter never reads those paths (the operational double-sign guard for clones is T28 runbook scope).
- Importer: staging area, per-record validation (incl. per-range exactly-one-body rule for covered ranges),
  root reconstruction, activation gate, rollback of failed staging.
- Manifest/chunk layer: checksums over canonical decoded payloads, byte resume within a manifest,
  cross-producer failover at range boundaries; resource bounds (manifest size/entries, chunk sizes, record
  length, decompression ratio, temp disk, concurrency, time); artifact IDs never used as filesystem paths.
- Reth prerequisite is EXPLICIT (postfix PF-B07): the CE snapshot does NOT carry Reth EVM state; import
  activates only on a node whose durable Reth checkpoint already holds H with the EXACT hash (PF-H06). A
  fresh datadir FIRST obtains Reth state — ordinary full sync, or a paired Reth datadir restore per the
  T34 runbook — THEN imports CE at the matching H; no fresh/no-prehistory bootstrap claim exists.
- Bootstrap orchestration: import @ H → replay retained canonical events `H+1..head` (T17 rebuild machinery);
  advertised-bootstrap-capability rule surfaced as operator metric/flag. Crash-safety: no separate bootstrap
  staging state exists — after activation at `H` the SMT `last_applied` marker is the durable progress
  cursor, and a crash mid-replay restarts into T17's behind-row and resumes contiguously (see T17). While
  catching up, the parent-root gate blocks proposing/validating and proofs serve at the marker height.
- Projection baseline (§6.2 "loads the current body set at H and applies finalized events after H"): the
  importer atomically creates the Mongo/ExEx projection baseline
  `{height: H, block_hash, profile, body_coverage}` through T20's baseline-init cursor API, seeds body rows
  and index memberships ONLY for the declared `body_coverage`, and sets the event high-water to `H` in the
  same durable commit. The projector then applies `H+1..head` normally. For the `tree` profile the baseline
  is legal without bodies/indexes — their incompleteness stays explicit (per-key `unavailable` + T21 lazy
  recovery). Without this baseline a fresh node's projector would attempt `genesis..head` over pruned
  receipts or fake its high-water — both violate T20's contiguity semantics.
- Cross-store activation protocol (audit-final H-02): CE-MDBX activation and the Mongo baseline CANNOT be
  one transaction — activation is an IDEMPOTENT state machine keyed by a stable `import_id` derived from
  the snapshot identity: (1) staged CE import verified → (2) CE activation commit (marker at H) →
  (3) Mongo baseline install (T20 API, records the `import_id`) → (4) readiness eligibility. Every step
  is idempotently re-runnable; a crash at any point resumes by `import_id` without re-import; staging is
  retained until step 3 is confirmed; a conflicting `import_id` at step 3 is corruption (fail-closed).
  The T16 retention lease is acquired BEFORE step 2 (audit-final M-06), is keyed by the same `import_id`,
  and survives crash: on restart it is re-verified/re-acquired BEFORE the state machine resumes and
  before the projector's `FinishedHeight` can release retention (crash-safe handoff — postfix PF-M02). Aligned two-store EXPORT (the
  paired-checkpoint identity for the T34 manual-restore contract) uses the same `import_id` scheme.
- No recovery-window claim (audit-final H-14): the snapshot format/manifest carries current bodies only
  and makes NO recovery-window claim; the imported node starts with an empty recovery-capability range
  (T20's durable `{oldest, newest}`).
- Raw datadir relocation documented as a separate mechanism (validation only: paired-checkpoint check).

## Out of scope

- Historical-root retention (future pruning design); producer discovery/distribution infrastructure.
- Bootstrap path 2 (§14.5): full canonical-history replay is ordinary node sync plus domain-runtime replay
  prerequisites — owned by the domain runtimes per §14.5's own assignment, not by the snapshot subsystem.

## Acceptance criteria

1. Semantic conformance: two independent exporters — the Part A reference exporter and the Part B
   production exporter (different MDBX layouts / insertion orders) — produce importable snapshots
   reconstructing identical roots (§19.12).
1b. Bootstrap profiles (scope re-cut 2026-07-13 — the former validator coverage/readiness gate is
   removed): `tree` bootstraps a proof-serving node without bodies (per-key `unavailable` + T21 lazy
   recovery on the RPC surface); `tree-with-bodies` additionally seeds current bodies/indexes for the
   declared `body_coverage`. An operator bootstrapping a VALIDATOR uses `tree-with-bodies` (runbook
   guidance, T34) — a validator with missing bodies simply diverges and falls out per T29 item 5; no
   protocol readiness gate exists.
2. Import rejection matrix: omission, duplicate, overlap, reorder, conflict, out-of-range, checksum, wrong
   checkpoint, root mismatch, unknown version/profile/scheme downgrade, decompression bomb, lazy
   missing-node (§19.12 — complete list, no sub-item dropped); descriptor matrix (postfix PF-B06):
   orphan/duplicate/missing collection descriptor rejected.
2b. Empty-present round-trip (postfix PF-B06): a collection emptied by deletes (catalog leaf present,
   ZERO-top `R_collection`, no entity leaves) survives export → import → identical root; a retired
   collection stays absent.
3. Logical-range failover at the LIBRARY level: corrupted range from source A recovered from source B via
   the abstract range-source interface (network transport E2E is T28's gate).
4. Bootstrap round-trip with local-dir sources: fresh node imports @ H, replays to head, serves verifying
   proofs (network-transport bootstrap is T28's gate); crash injected mid-replay resumes from the SMT marker
   and completes without re-import (pairs with T17's multi-height catch-up AC).
5. Activation blocked while local durable Reth checkpoint < H (no §13.3 impossible-state trip);
   activation calls the shared T16 DB-only verification (R4): matching height but wrong hash-at-H
   (fork/chain) rejected; wrong `0xEE0B.slot1` root rejected; missing receipts/state rejected; wrong
   artifact/scheme rejected; identity binding mismatch rejected — negative fixtures for each.
6. Projection-baseline crash matrix: crash between baseline commit and the first applied event block, and
   between subsequent event blocks — restart resumes from the baseline/high-water without gaps or
   double-apply; `tree`-profile baseline (no bodies) yields explicit `unavailable` + lazy recovery, never a
   fake-complete projection.
7. Cross-store crash matrix (audit-final H-02): crash between CE activation and the Mongo baseline
   install, a double-run of each state-machine step, and a conflicting-`import_id` fixture — each resumes
   or fails closed per the activation state machine; a concurrent export during catch-up yields a
   consistent paired checkpoint.

## Invariants

- The reconstructed root is the only completeness authority; no manifest/signature is a trust root.
- Staged import cannot corrupt an existing active tree.

## Tests

- Cross-exporter conformance, adversarial import fuzzing (§19.17), localnet bootstrap scenario (§19.18 input).

## Files

- `crates/core/compressed_entities/src/snapshot/{wire.rs,export.rs,import.rs,manifest.rs}`
