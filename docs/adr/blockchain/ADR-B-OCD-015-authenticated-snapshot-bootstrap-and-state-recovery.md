# ADR-B-OCD-015: Authenticated snapshot bootstrap and state recovery

- **Status:** Proposed; certified genesis replay exists, snapshot transport is not implemented
- **Date:** 2026-07-17
- **Decision owners:** Blockchain Space, node, consensus, execution and persistence maintainers
- **Scope:** joining an existing chain, snapshot export/import, historical replay and disaster recovery
- **Depends on:** ADR-B-WIR-001, ADR-B-GEN-001, ADR-B-CNS-001,
  ADR-B-CLI-001, ADR-B-OCD-004 through ADR-B-OCD-014
- **Related:** ADR-B-OCD-010, ADR-B-OCD-011, ADR-S-KEY-001,
  ADR-S-TEE-001 through ADR-S-TEE-002

## Context

A fresh follower can currently join through `--upstream`: it anchors on the genesis
validator set, downloads finalized blocks and finalization proofs, verifies committee
transitions and executes history locally. This is the strongest implemented
bootstrap path, but its cost grows with chain history and depends on retained blocks,
receipts and consensus certificates.

Operational scripts named “bootstrap” generate a new local network: DKG keys,
validator files and genesis. DKG share export/import transfers validator secret
material. Neither operation imports the state of an existing chain. Copying Reth,
Commonware, CE MDBX or Mongo directories independently is also not a consistent
snapshot because their durable boundaries can differ.

This ADR defines two authenticated ways to acquire an existing chain state: replay
from genesis and import at a verified finalized checkpoint. ADR-B-OCD-007 owns restart of
an already initialized node after a crash.

## Decision

### Bootstrap modes

The node exposes explicit, non-overlapping modes:

1. **Certified genesis replay:** start from ADR-B-OCD-006 genesis identity, verify every
   finalization proof and committee transition, execute every finalized block, and
   build CE/Mongo state through normal production paths.
2. **Authenticated checkpoint import:** independently verify a finalized checkpoint,
   import versioned semantic snapshots for required state, then certified-replay
   every block after that checkpoint.
3. **Validator promotion:** after either mode reaches exact readiness, install
   separately protected current validator key/share material and prove it belongs to
   the active committee. State import never transports validator secrets.

Uncertified upstream sync, a producer signature, object-store credentials, archive
checksums or “trusted host” status cannot substitute for consensus verification.

### Bootstrap checkpoint

`BootstrapCheckpointV1` contains:

- chain id, genesis hash and ADR-B-OCD-006 chain-manifest identity;
- finalized height and block hash;
- finalization certificate plus the committee-transition proof chain from a locally
  configured trust anchor;
- execution header/state root and required fork/protocol schedule identity;
- CE commitment-scheme/topology version and sealed catalog root;
- consensus archive namespace/codec version;
- snapshot format/profile identities for every imported component; and
- the first post-checkpoint block required for continuity.

The receiver selects and verifies the checkpoint. Snapshot producers provide bytes,
not authority. A weak subjectivity/trust-anchor rotation, if introduced, is an
explicit operator decision with age and chain-identity limits; it is never silently
learned from the same snapshot server.

### Snapshot set and manifest

One `NodeSnapshotManifestV1` binds a mutually consistent logical snapshot set:

| Component     | Normative content                                                                                      |
| ------------- | ------------------------------------------------------------------------------------------------------ |
| Execution     | canonical state at the checkpoint plus headers/receipts/artifacts required for post-checkpoint replay  |
| Consensus     | certificate/block archive continuity and committee/DKG public history needed to verify later finality  |
| CE            | canonical logical leaves and optional canonical bodies, reconstructed roots and exact finalized marker |
| Projection    | no authoritative state; Mongo may be rebuilt, or imported only as a checked acceleration artifact      |
| Configuration | chain/schedule/schema/codec/artifact identities, never private keys or local endpoints                 |

The manifest declares format version, component versions, checkpoint identity,
logical range coverage, ordered chunks, decoded sizes and content digests. Physical
MDBX pages, Mongo files, Reth allocator layout and Commonware archive files are not a
portable protocol format. Implementations may offer a same-version physical fast
path, but must still validate its semantic roots and full environment identity.

CE `tree` coverage contains every current leaf needed to reconstruct every shard,
collection, catalog and sealed root. `tree-with-bodies` declares exact
body ranges; a validator-capable profile contains every body required for deterministic
post-checkpoint execution. Internal SMT nodes are acceleration data and may be
discarded and rebuilt. Mongo documents and indexes are always derived from finalized
canonical events and never authenticate the snapshot.

### Staged, resumable import

Import uses a new staging generation outside live store paths:

1. verify the manifest, checkpoint certificate and chain environment;
2. stream strict-decoded canonical records in declared order;
3. reject gaps, duplicates, overlaps, conflicting keys, unknown versions and size or
   resource-limit violations;
4. recompute execution/CE roots and all component checkpoints independently;
5. run ADR-B-OCD-007 reconciliation against the same checkpoint;
6. atomically publish the complete generation or a small activation pointer; and
7. certified-replay subsequent finalized blocks before readiness.

Chunk download and validation resume from durable content/range cursors. Repeated
chunks are idempotent. A failed import cannot modify the active generation. Staging
is quarantined with a diagnostic report or safely garbage-collected only after no
reader/writer lease can reference it.

### Multi-source transport and availability

Manifests and chunks may come from validators, archive peers, mirrors or object
stores. Checksums protect transport; reconstructed authenticated roots and verified
finality establish meaning. Byte resume may mix mirrors serving the identical
manifest. Different producers interoperate only at canonical logical-range
boundaries, not by assuming their physical chunk numbers match.

Download concurrency, decoded bytes, record counts, decompression ratio, range size,
open files and disk growth are bounded before allocation. Parsers are streaming and
strict. Import never executes snapshot-provided code, follows paths from the archive,
or writes outside staging.

### Retention and recovery service levels

The release profile declares:

- maximum certified-genesis replay duration;
- snapshot publication cadence and maximum accepted age;
- minimum retained block/certificate/receipt/CE-event window after each published
  checkpoint;
- number and geographic/administrative diversity of snapshot sources;
- recovery point and recovery time objectives; and
- when pruning may advance after successful snapshot verification by independent
  consumers.

Pruning is coordinated across Reth and consensus archives and cannot remove the only
bridge from an advertised snapshot to current finality. Mongo never determines the
retention floor. Private validator/DKG/TEE secrets use independently tested encrypted
backup, rotation and revocation procedures owned by ADR-S-KEY-001, ADR-S-TEE-001 and
ADR-S-TEE-002.

## Authoritative interfaces

| Responsibility                      | Authority                                                         |
| ----------------------------------- | ----------------------------------------------------------------- |
| Network identity and genesis anchor | ADR-B-GEN-001 genesis manifest and ADR-B-RLS-001 release manifest |
| Finalized checkpoint selection      | verified consensus finalization/committee proof                   |
| Portable import contract            | versioned semantic snapshot manifest and codecs                   |
| Execution and CE integrity          | independently reconstructed state/commitment roots                |
| Post-import convergence             | ADR-B-OCD-014 recovery coordinator                                |
| Validator secret restoration        | ADR-S-KEY-001 and TEE custody ADRs, outside snapshot              |

## Invariants

- Snapshot bytes and producer identity are never consensus trust roots.
- Every imported component names the same chain and exact finalized block hash.
- Unknown format, schema, fork, namespace or commitment versions fail closed.
- No live store is mutated before complete semantic verification.
- A failed or interrupted import leaves the prior active generation unchanged.
- Mongo can be discarded and rebuilt without changing canonical state.
- Validator, DKG and TEE secrets never appear in a node-state snapshot.
- Post-checkpoint replay is certified, contiguous and begins from the exact imported
  parent identity.
- Pruning never destroys all verified recovery paths to the current finalized tip.

## Atomicity, replay and failure

The manifest is one consistency envelope, but component import need not be one large
database transaction. Each component writes an immutable staging generation; one
activation record publishes only a completely verified set. Activation and the first
ADR-B-OCD-007 readback are crash-safe and idempotent.

Transport failures are resumable. Malformed records, root mismatch, certificate or
committee failure, incompatible identity, resource-limit breach and missing declared
coverage permanently reject that artifact. A source may be penalized without making
source reputation part of consensus. If no valid snapshot remains, certified genesis
replay is the safety fallback while its history is retained.

## Compatibility and migration

Snapshot compatibility is explicit by format, chain, fork/schedule, storage schema,
consensus codec/namespace and commitment versions. Readers support only declared
historical versions and never guess from record length. A format migration either
converts semantic records while re-verifying roots or regenerates a snapshot from a
canonical node. Database-directory copy compatibility is limited to an exact pinned
binary/vendor profile and is not a cross-version promise.

## Production-interface verification evidence

Inspected the `--upstream` certified-follower stack, upstream finalization/block RPC
transport, genesis committee anchor, full local execution path, marshal immutable
archives, CE startup replay, Reth pruning guards and localnet/DKG bootstrap tooling.
The node rejects plain execution-only full-node mode and requires recovery-critical
receipt/account/storage history pruning to remain disabled. Existing code provides a
certified replay foundation, but no inspected CLI or production module exports,
imports or atomically activates a complete semantic node snapshot. Status remains
Proposed.

## Consequences

The project gains a scalable join/disaster-recovery contract without weakening the
genesis-anchored certified path. “Bootstrap”, “backup”, “snapshot” and “key restore”
become distinct operator actions, and every accelerated import remains independently
checkable against finality and authenticated roots.

## Rejected alternatives

- **Copy every live data directory at approximately the same time:** component
  checkpoints can describe different blocks.
- **Trust a signed snapshot from one validator:** the signer is a transport source,
  not a replacement for quorum finality.
- **Treat Mongo dump as canonical state:** Mongo is a derived projection.
- **Ship raw MDBX/Reth files as the only format:** physical layouts are not stable,
  portable semantic contracts.
- **Include validator keys for convenience:** it couples public state distribution to
  secret custody and enables accidental validator cloning.
- **Enable pruning as soon as one producer uploads a snapshot:** an unverified or
  unavailable artifact is not a recovery path.

## Open questions and technical debt

1. **Critical:** no production snapshot exporter, verifier, importer, manifest codec
   or atomic generation activator was found. The current scalable recovery design is
   documentation only.
2. **Critical:** current fresh-node recovery depends on certified replay from genesis
   through a configured `--upstream`; prove availability from multiple sources and
   remove the single-upstream liveness dependency without weakening verification.
3. **Critical:** recovery-critical pruning is currently disabled wholesale. Implement
   a proven retention-floor coordinator before permitting bounded pruning.
4. The follower is described as anchored on genesis validators. Prove the complete
   committee/DKG transition chain, boundary artifacts and historical public material
   can always be reconstructed from served marshal/RPC history.
5. `--upstream.nocertify` is declared but deliberately unimplemented. Keep it
   unavailable outside an unmistakable dev profile; it must never become an
   operational shortcut.
6. Define `BootstrapCheckpointV1` and its proof bundle, including fork/schedule,
   chain-manifest, consensus namespace/codec and CE topology identities.
7. Define canonical semantic snapshot codecs and resource bounds for execution,
   consensus archive and CE tree/body records. The long-form CE concept is useful
   design input but is not an implemented or indexed source of truth.
8. Decide whether Reth state snapshot import can be independently root-reconstructed
   with upstream APIs or requires an Outbe-owned semantic exporter/importer.
9. Define how imported execution state supplies receipts and Outbe artifacts needed
   by CE and Mongo catch-up after height `H` without retaining all pre-`H` history.
10. Define validator-capable CE body coverage. A root proves leaf commitments, not
    local possession of every body required by later business execution.
11. Add snapshot identities to ADR-B-OCD-006 release artifacts and bind every component
    to the same genesis hash/chain-manifest digest.
12. Mongo import should be optional acceleration. If supported, validate every
    document/index against canonical replay or rebuild it before readiness.
13. Add immutable staging generations and an atomic activation pointer for all
    imported stores; directory rename alone has platform, open-handle and multi-volume
    failure semantics that need proof.
14. Add streaming parser limits, decompression-bomb defenses, canonical ordering,
    duplicate/range-gap detection and adversarial snapshot fuzzing.
15. Add resumable logical-range downloads, content-addressed chunks and safe
    multi-source failover with no path traversal or archive extraction surface.
16. Define snapshot producer consistency: all records must come from immutable views
    of the same finalized checkpoint even while live execution continues.
17. Define snapshot publication acknowledgement and coordinated Reth/marshal/CE
    pruning order under crashes and producer failure.
18. Localnet `bootstrap-testnet.sh` destructively recreates its output directory.
    Keep that behavior clearly scoped to new dev networks; it is not a recovery tool
    and must reject production-looking paths/identities.
19. DKG `export-share/import-share` can be confused with chain-state recovery. Rename
    or document it as secret-material transfer and require encrypted authenticated
    custody, permissions and active-committee validation.
20. Define recovery for sealed TEE offer-key material. Current follower startup needs
    an attested enclave holding the lifetime key when the chain is already bootstrapped;
    snapshot import cannot manufacture or distribute that secret.
21. Add end-to-end tests: genesis replay, snapshot import plus catch-up, interrupted
    download/import/activation, corrupt chunks, wrong chain/checkpoint/root, missing
    bodies, unavailable producer, mixed manifests and validator promotion.
22. Publish operator commands for snapshot inspect/export/verify/import/status/abort
    with dry-run disk estimates and machine-readable audit reports.
23. Establish production RPO/RTO, snapshot cadence, retention window and independent
    restore drills. A backup that has never been restored is not evidence.
24. Audit all mutable state outside the four principal stores (DKG boundary files,
    TEE seals, peer/identity material and future sidecars) and classify it as
    reconstructible public state, local identity or separately backed-up secret.
