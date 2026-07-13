# T19 — outbe_getBody RPC with absent/unavailable/unsupported

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §11.1 (Q20)
Depends on: T18, T21, T30 (RPC DTO/error model)
Blocks: T27

## Summary

Expose `outbe_getBody(domain_id, partition_key?, raw_id, height?)` in the `outbe_*` namespace returning the
point-proof package or the tri-state `absent | unavailable | unsupported`, with the cursor-skew rules.

## Context

`absent` requires a verifying non-membership proof for the tree_key the client independently derives —
a node lacking body bytes or holding stale projection data must surface `unavailable`, never `absent`.
`unavailable` = commitment/proof capability present, body bytes not currently available; if the body-store
cursor is ahead of the served tree checkpoint, a mismatch may be a newer body: first wait for tree catch-up
or fetch the body version for the served root; peer/event/snapshot recovery only after cursor alignment
still shows missing/invalid. `unsupported` = requested historical height/proof version cannot be served
(v1 serves only the latest persisted finalized state). For `Singleton`, `partition_key` is absent; for
Tribute it derives from `raw_id[0..4]` and any explicit echo must match.

## Scope

- RPC registration through Reth's extension surface (existing `outbe_*` namespace conventions, no parallel
  router); request validation incl. partition-echo match. The signature carries an explicit
  `proof_encoding_version?` negotiation parameter (defaults to the node's current version; unknown
  requested version → `unsupported`) — audit P0-3; exact DTOs/error codes from the T30 wire spec.
- Peer `RecoverySource` ownership (audit-v2 P1-3): this task ALSO delivers `OutbeGetBodyPeerSource` — the
  production client implementation of T21's `RecoverySource` interface over the server DTOs defined here
  (source rotation, mandatory proof/leaf verification), replacing T21's fake-server-only coverage; the
  live two-node E2E stays in T25.
- Trust modes documented (audit P1-4): `trusted-local-testnet-node` — the CLI/MCP client trusts its
  connected node and verifies package integrity only; INDEPENDENT verification (own finality/header trust
  per §10.2) is a distinct, explicitly-selected mode. Docs never present trusted-node verification as the
  §10.2 independent-verifier guarantee.
- Response assembly: T18 package on present; verified non-membership for absent; `unavailable` with retry
  metadata on body miss; status split per audit-final M-12: `height < proof_ready_height` ⇒ `unsupported`
  (historical — v1 cannot serve it); `height > proof_ready_height` ⇒ a RETRYABLE typed `not_ready` status
  carrying `{local, required}` checkpoints (future-height lag is not "unsupported"); unknown proof
  version ⇒ `unsupported`.
- Cursor-skew handling wired to T21 (body-store high-water vs `proof_ready_height` comparison, catch-up
  wait, historical-body-for-served-root retrieval, no futile refetch loop).
- Serve-side leaf check: every returned body recomputes to the current leaf before it leaves the node.

## Out of scope

- Secondary-index/list RPCs (projection features, non-goal for the core); auth policy (repo RPC rules apply).

## Acceptance criteria

1. Tri-state matrix tests: present / absent-in-shard / collection-absent / body-missing (`unavailable`) /
   historical height (`unsupported`); `unavailable` never masquerades as `absent`.
2. Cursor-skew tests: body ahead of tree → wait/serve-for-root, no repeated peer fetch (§19.19).
3. Partition echo mismatch rejected; singleton with explicit partition rejected.
4. Served-body leaf mismatch blocked server-side (corrupt row cannot leave the node as valid).
5. Peer client (`OutbeGetBodyPeerSource`, audit v3 P1-2): conformance suite over the T21 `RecoverySource`
   interface — source rotation on failure, `unsupported`/`unavailable` handling, malicious/tampered
   package rejection (leaf/proof mismatch), timeout behavior; runs against both the fake server and a
   live localnet node.
6. Status split (audit-final M-12): `height < local` ⇒ `unsupported`; `height > local` ⇒ typed retryable
   `not_ready` with both checkpoints; unknown proof version ⇒ `unsupported`. (The former recovery DTO and
   `RECOVERY_BODY_WINDOW` are removed — scope re-cut 2026-07-13: the RPC is current-only.)

## Invariants

- The node never converts a local availability failure into a consensus claim of absence.

## Tests

- RPC integration tests against a localnet node; adversarial stale-projection fixtures.

## Files

- `crates/core/compressed_entities/src/rpc.rs` (+ node RPC wiring in `bin/outbe-chain`)
- `crates/core/compressed_entities/src/recovery/peer.rs` (`OutbeGetBodyPeerSource`)
