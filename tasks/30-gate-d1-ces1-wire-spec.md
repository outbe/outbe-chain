# T30 — Gate D1: CES1 normative wire and schema specification

Status: todo
Source: `audit_plan.md` §4 P0-3/P0-8, §8 Gate D1; concept §1.2 (final ABI is a concept non-goal — so the
exact formats MUST be fixed here, before implementation)
Depends on: T29 (Stage 1 profile shapes some DTO/status vocabulary), T35 (body/generator feasibility precedes the schema/generator freeze — audit-final B-07), T36 (the approved port map fixes the list-RPC product surface — audit-final B-03)
Blocks: T02, T05, T06, T08, T13, T18, T19, T20, T22, T23, T24 (Part B re-baselines the PROVISIONAL_Q11 values), T26, T31, T33, T34

## Summary

Author and approve the versioned `CES1 Wire and Schema Specification` — the single normative source for
every protocol-critical byte format the concept deliberately leaves open. Golden vectors are not
normative until the format is defined independently of the first implementation.

## Contents (all normative, all versioned)

1. Genesis domain registry table: exact `domain_id` values, `id_encoding_kind_u8` per domain, generator
   versions and algorithms, schema/hash version widths and assigned values, runtime identity
   representation, `PROVISIONAL_Q11`-marked candidate testnet `K_domain` values (final K is a T24 Part B
   output — audit_plan_v2 P0-1: requiring final K here would cycle through T23→T24), body-size bounds,
   gas profiles. Numeric fields marked `PROVISIONAL_Q11` are explicitly replaceable by T24 Part B before
   the final genesis freeze; T02/T04/T06 build on the provisional values and are re-baselined by T24.
2. Canonical body schemas for Tribute/Nod (Gem deferred to its future onboarding fork): exact DAG-CBOR
   arrays, field types, optional encodings,
   string normalization, max lengths. Schemas and generator algorithms freeze only AFTER the T35
   feasibility artifact is approved (audit-final B-07). The normalization rule names its executable
   owners (audit-final M-09): T23 normalizes domain input, T05 rejects non-normalized encodings;
   Unicode/length/invalid vectors committed.
3. Event ABI: exact Solidity types, indexed fields, numeric discriminants for
   `WriteV1`/`DeleteV1`/`PartitionRetiredV1`.
4. Artifact envelope: the exact version-bump value for the tag-0x08 addition and compatibility rule.
5. Proof encoding v1: field widths, byte order, enum variants, discriminated present/absent forms, size
   limits, full proof byte grammar; `K_domain = 1` rule (zero top levels — `collection_top = shard_root`)
   defined explicitly.
6. JSON-RPC: request/response DTOs, error codes, exact `present/absent/unavailable/unsupported` shapes,
   `proof_encoding_version` negotiation, trusted-local-testnet-node vs independent-verification modes;
   the LIST RPC surface for T26 (method names for by-owner/by-day/by-WWD, request/response DTOs, stable
   ordering key, continuation/offset semantics, numeric codes, `with_proof` shape, max-response behavior)
   and the bounded RECOVERY request form `{checkpoint, canonical_identity}` with `RECOVERY_BODY_WINDOW`
   value and status codes (audit v6 P0-1/P1-5). The list-RPC surface implements the T36-approved port map
   (audit-final B-03). Recovery responses use INTERVAL version-selection semantics — the version current
   AT the requested checkpoint (audit-final H-15). Height-status split (audit-final M-12):
   `height < local` ⇒ historical `unsupported`; `height > local` ⇒ retryable `not_ready` carrying
   `{local, required}` checkpoints; version mismatch stays `unsupported`.
7. Snapshot format v1: canonical record/container encoding, inclusive/exclusive range semantics and
   continuation keys, checksum and content-ID algorithms, compression codecs, snapshot identity
   derivation, `body_coverage` representation, numerical parser/resource bounds, `canonical id bytes` =
   `BE32(id_f)` stated explicitly. Collection-descriptor record (postfix PF-B06): a versioned record
   `{domain_id, partition_key_or_none}` for EVERY present Root-Catalog leaf — `collection_key` is an
   irreversible hash, so without the preimage an importer can neither reconstruct an EMPTY-but-present
   collection (amendment #3: ZERO-top `R_collection` with zero entity leaves) nor resolve `K_domain` for
   tree-profile leaf records; validation: orphan descriptor (no catalog leaf), duplicate, and missing
   descriptor for a present leaf all reject; empty-present and retired-absent golden vectors included.
8. Versioning/reservation rules and the golden-vector generation procedure (reference generator, fixture
   layout, regeneration policy).
9. Local runtime outcome vocabulary (audit v3 P1-3, granularity per audit v4 P0-1):
   `ProjectionNotReady {local_checkpoint, required_checkpoint, reason}` — global readiness gate;
   `BodyDataUnavailable {canonical_identity, checkpoint, reason}` — per-operation; both are LOCAL
   executor outcomes, never wire/consensus errors and never in receipts;
   `SameBlockBodyUnavailable {canonical_identity, block_number}` — deterministic domain revert.
   Which of these surface in RPC/status/metrics, their numeric codes at process boundaries, and the
   bounded timeout/retry/queue budget values for the T33 I/O layer. Includes the `NOT_READY_CATCHUP`
   sub-state and the parent-checkpoint recovery status contract (audit v7): recovery target shape
   `{checkpoint, canonical_identity}`; on window/source exhaustion — typed recovery failure, node remains
   NOT_READY, operator manual paired-restore status (NO snapshot fallback exists); operator status
   surface for catch-up progress. T30 is the SINGLE numeric owner of the `RECOVERY_BODY_WINDOW` minimum
   (T34 validates it via SLO/soak). Also owns the shared `ProjectionBaselineCoverageReport` type/API
   (produced by T22 through T20's baseline-init, consumed by T33's readiness checker — audit v7 P1-5).
10. Staged-batch serialized encoding grammar (postfix PF-M07): the normative byte encoding of a staged
   tree batch — the canonical unit in which `max_staged_tree_bytes` is measured; T31 references it,
   T15 implements it with golden vectors, T10/T12 measure in it.
11. Provisional marking is GLOBAL: every benchmark-controlled numeric — K values, body/byte limits, gas
   profiles, staged bounds — carries `PROVISIONAL_Q11`, not only K (audit v3 P1-3). This includes the
   READ bounds of audit-final B-10: Lysis page max rows/bytes and the point-read count/bytes reservation
   values.

## Acceptance criteria

1. Spec document merged at `docs/ces1-wire-spec-v1.md` (stable artifact path, audit v5 P1-12), reviewed, and marked
   v1-frozen for testnet. "No placeholder" means NO UNKNOWN WIRE RULES; numeric fields explicitly marked
   `PROVISIONAL_Q11` are permitted and are replaced by T24 Part B.
2. Codec consumers T05/T08/T18/T19/T22/T23/T26 AND runtime/profile consumers T20/T33/T34 reference it as their normative input (their "codec crate is pinned, but the
   grammar is authoritative" clauses point here).
3. Fixture ownership (audit_plan_v2 P1-6): T30 defines the fixture SCHEMA, algorithms, and authoritative
   input tables; the implementing tasks (T01–T05, T07, T08) PRODUCE their own vectors from it; an
   independent reference generator cross-checks them — with an OWNER and schedule (audit-final M-03):
   T30 defines the per-format reference-generation procedure (events, artifact envelope, proofs;
   snapshots are covered by T22 Part A's reference exporter), and T25 gates the release on the
   cross-check having actually RUN for every format; T24 regenerates K-dependent fixtures.

## Invariants

- One normative source; implementations and vectors mirror it, never define it.
