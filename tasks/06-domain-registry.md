# T06 — Fork-governed domain registry with partition policy

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §4.1 (Q6, Q20, Q23), §16.1
Depends on: T02, T30 (genesis registry table values)
Blocks: T07, T09, T18, T23

## Summary

Implement the fork-active domain registry: per-domain-version entries fixing identity encoding, partition
policy, shard count, versions, gas profile, and activation height, resolved deterministically by block height.

## Context

Every consensus execution and proof verification at height H uses the registry entry active at H. Entries
fix: `domain_id: u16`, runtime identity, `id_encoding_kind_u8` (immutable per version), ID generation
version + registered generator, partition policy (`Singleton` | `Partitioned` with canonical partition-key
derivation), `collection_shard_count = 2^k`, partition retirement policy, active schema/hash versions,
lifecycle policy extensions, gas/quota profile, activation height. Changing the registry is fork-governed —
for v1 this is compiled-in registration, not on-chain governance.

## Scope

- Registry implemented AGAINST the descriptor types owned by T02 (no cycle: T02 defines the entry shape the
  derivation consumes; T06 adds registration + height resolution) + the compile-time registration
  MECHANISM. Single-owner rule (audit-final B-09): T06 registers only TEST-FIXTURE domains for its own
  resolution matrix; the CONCRETE Tribute/Nod genesis entries and generator bindings are owned by T23;
  production genesis-domain evidence lives in T23/T14/T25.
- Height-resolved lookup: `active_entry(domain_id, height) -> Entry | fail-closed`.
- Fail-closed paths: unknown domain, inactive at height, mismatched encoding kind, invalid partition input.
- Partition policy enforcement hooks used by T02 derivation and T07 core validation (`retire_partition`
  allowed only when policy permits).
- `K_domain` power-of-two validation at registration; changing it for existing state is
  unrepresentable without a new commitment scheme (compile-time assert per domain version).
- Version-axis surface per §16.1 (schema_version, hash_version resolution for leaf derivation).

## Out of scope

- On-chain governance of the registry; concrete domain adapters (T23); Q11 numeric gas values.
- §16.1 multi-version upgrade-transition policy (old-schema readable / migrate-on-update / bulk-migrate):
  deferred — no transitions exist at v1 greenfield genesis; the version-resolution seam this task builds is
  where a future transition rule attaches.

## Acceptance criteria

1. Unknown/inactive domain and mismatched encoding-kind calls fail closed with structured errors (§19.7).
1b. Schema downgrade (§19.7 named gate): resolution at height H refuses a schema/hash version not active at
   H; on the consensus path an obsolete version is unrepresentable (callers never select versions — the
   registry at H does), asserted by a direct test.
2. Height-boundary tests: entry inactive at H-1, active at H; test-fixture genesis domains active at 0
   (the concrete Tribute/Nod genesis entries are T23's acceptance — audit-final B-09).
3. Singleton domain rejects partition input; Partitioned domain rejects missing/malformed partition key.
4. Registry entry immutability: no API mutates a registered version at runtime.

## Invariants

- Callers never select versions, encoding kinds, generators, or partition modes; the registry at H does.

## Tests

- Unit resolution matrix; property test that every registered `K_domain` is a power of two ≤ documented cap.

## Files

- `crates/core/compressed_entities/src/registry.rs`
