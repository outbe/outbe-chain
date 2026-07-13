# T28 — Snapshot serving transport and operator CLI

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §12.1, §14.4, §14.5
Depends on: T22
Blocks: — (release gate T25 only)

## Summary

Provide the minimal transport and operator surface that §14's availability assumptions and T22/T25 test
requirements need: a deterministic export layout a node/operator can publish, a range-boundary fetch client
for bootstrapping nodes, and `outbe-cli snapshot` commands.

## Context

The spec assumes "at least one reachable source has the required bytes" (§14.4) and cross-producer failover
at logical-range boundaries, but T22 owns only the format/exporter/importer libraries and explicitly excludes
distribution infrastructure. Without a minimal transport, T22's own acceptance criteria (multi-source
failover, localnet bootstrap round-trip) and T25's soak items (snapshot bootstrap, cross-source resume)
cannot be exercised end-to-end. "Bulk snapshot service need not be publicly exposed by every signing host" —
serving can live on a separate host/interface from the signer.

## Scope

- Export layout: deterministic filesystem/object-store layout for a published snapshot (manifest + chunk
  files, content-ID naming), producible by the T22 exporter; servable by any dumb HTTP/object-store mirror —
  no custom p2p protocol in v1.
- Fetch client: a concrete HTTP/object-store implementation of T22's abstract range-source interface —
  manifest download, chunk fetch with checksum verification, byte-resume within one manifest, cross-producer
  failover at canonical logical-range/continuation-key boundaries (multiple source URLs), resource bounds
  shared with the T22 importer. T28 adds no failover logic of its own beyond the transport: range-boundary
  failover semantics live in T22's library.
- REQUIRED (release-gating — §19.12 multi-source failover, §19.18 soak bootstrap): the fetch client, the
  node bootstrap flag (`--bootstrap-snapshot <sources…>`) wiring import + T22 activation gates + event
  replay to head, and `snapshot import <sources…>`. Reth-first sequence (postfix PF-B07): the bootstrap
  flag documents and enforces that the node's durable Reth checkpoint must already hold H (exact hash)
  before CE import activates — a fresh datadir syncs/restores Reth first; the CE snapshot never
  substitutes for Reth state.
- REQUIRED documentation (§14.5 operational MUST — not optional): the clone/double-sign guard. A snapshot
  or datadir relocation must never transport validator keys, signer state, node identity, live locks, or
  ephemeral caches (structural half owned by T22), and operational controls must prevent the original and
  a clone from concurrently signing with the same validator key. This section of the operator docs is a
  release deliverable.
- OPTIONAL operator tooling (beyond the spec's normative surface; not release-gating): standalone
  `outbe-cli snapshot export --profile … --height …` and `snapshot verify <path>` (offline checkpoint/root
  check) — the soak's export need can be met by a mise/test-harness invocation of the T22 exporter.
- OPTIONAL serving guidance (non-gating): publishing a snapshot from a non-signing host; advertising
  bootstrap capability only while the §14.5 event-tail invariant holds (flag from T16/T22 config).
- Localnet/soak wiring: mise target exercising export on one node → bootstrap of a fresh node from two
  mirrors with an injected range corruption (failover proof).

## Out of scope

- Producer discovery, incentives, or a p2p snapshot protocol; permanent archive guarantees (non-goals).

## Acceptance criteria

1. End-to-end: node A exports; fresh node B FIRST syncs/restores Reth to hold H (postfix PF-B07), then
   bootstraps CE from a mirror of A via CLI, replays to head, serves
   verifying proofs (closes T22 AC4 transport dependency). Validator bootstrap scenario (minimal model per the
   2026-07-13 re-cut): a fresh Variant A validator bootstraps Reth-first, then `tree-with-bodies` per the
   T34 runbook and joins; a tree-only bootstrap runs as a proof-serving full node (missing bodies fail
   reads per T29 item 5 — no readiness gate exists) — both exercised on localnet.
2. Cross-source failover: corrupted/missing range on mirror 1 recovered from mirror 2 at a range boundary;
   final root check authoritative (closes T22 AC3 end-to-end).
3. (Conditional — applies only if the optional `snapshot verify` is built) it rejects checkpoint/root
   mismatches offline; the release-gating equivalent lives in T22's import/activation gate.
4. Resume: interrupted fetch continues byte-level within the same manifest.
5. The REQUIRED clone/double-sign guard documentation is merged (release deliverable); the optional serving
   runbook, if written, is referenced by the T25 soak plan.

## Invariants

- Mirrors and object stores remain untrusted byte sources; the reconstructed root is the only completeness
  authority; no transport metadata becomes a trust root.

## Tests

- Integration: localnet export/bootstrap scenario with fault injection; CLI unit tests for source failover.

## Files

- `crates/core/compressed_entities/src/snapshot/{layout.rs,fetch.rs}`
- `bin/outbe-cli/src/commands/snapshot.rs`, `bin/outbe-chain` (bootstrap flag), mise tasks, README/runbook
