# ADR-B-RPC-001: RPC makes read authority, finality and verification explicit

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `crates/blockchain/rpc`, Outbe namespace wiring and the custom
  RPC/precompile-read boundary
- **Depends on:** ADR-B-OCD-007, ADR-B-GEN-001, ADR-B-CNS-002, ADR-B-OCD-010, ADR-B-OCD-004, ADR-B-OCD-005
- **Supersedes:** The RPC portion of the former pre-space RPC/operator placeholder

## Context

RPC combines several fundamentally different read authorities: canonical EVM
state, finalized headers, process-local consensus telemetry, Mongo projection
health, compressed-entity proofs and archived finalization bytes. A method name or
JSON response must not imply finality, liveness or cryptographic verification that
its underlying source does not provide.

## Decision

Every Outbe RPC method has a declared consistency class:

- **canonical state view:** one immutable provider snapshot at an explicitly named
  block tag/hash; current convenience methods default to latest canonical state;
- **finalized authenticated read:** a selected finalized header plus CE proof and
  canonical body bytes independently verifiable by the client;
- **verified transport:** archived consensus certificate/block bytes that the
  recipient must decode and verify against the epoch committee;
- **local telemetry:** consensus bridge, sync and projection readiness snapshots,
  never presented as consensus state;
- **compiled configuration:** constants from the running binary, returned with
  enough protocol/version identity to interpret them.

Mongo business documents are not exposed as authenticated state merely because the
projection is caught up. A projection response must carry its exact finalized
checkpoint and distinguish unavailable, stale and corrupt state.

## Public interface and sources

The registered `outbe_*` namespace is the following compatibility surface:

| JSON-RPC method | Consistency class | Primary source/owner |
|---|---|---|
| `outbe_getCompressedEntity` | finalized authenticated read | CE tree, finalized header and typed body repositories; ADR-B-OCD-013 |
| `outbe_getValidators` | canonical state view | ValidatorSet plus Staking; ADR-S-VAL-001 and ADR-S-STK-001 |
| `outbe_getValidator` | canonical state view | ValidatorSet, Staking and counters; ADR-S-VAL-001 |
| `outbe_getEpochInfo` | canonical state view | Cycle/ValidatorSet/Staking; ADR-S-CYC-001 and ADR-S-VAL-001 |
| `outbe_getStake` | canonical state view | Staking; ADR-S-STK-001 |
| `outbe_getSlashInfo` | canonical state view | SlashIndicator; ADR-S-SLS-001 |
| `outbe_consensusStatus` | local finalized telemetry | consensus reporter plus projection readiness; ADR-B-CNS-001 and ADR-B-SUP-001 |
| `outbe_getVrfSeed` | committed header read | canonical/finalized header provider; ADR-B-WIR-001 and ADR-B-CNS-002 |
| `outbe_getEmissionInfo` | compiled configuration | running binary constants; ADR-S-EMI-001 and ADR-S-RWD-001 |
| `outbe_getSlashConfig` | compiled configuration | running binary constants; ADR-S-SLS-001 |
| `outbe_getParticipation` | canonical state view | Rewards participation state; ADR-S-RWD-001 |
| `outbe_syncStatus` | local telemetry | Reth/consensus node state; ADR-B-SUP-001 |
| `outbe_getFinalization` | verified transport | finalization archive/consensus bridge; ADR-B-CNS-001 and ADR-B-CNS-003 |

The namespace therefore exposes:

- latest-state validator registry/detail, epoch, stake, slash configuration and
  participation reads through a read-only storage adapter;
- latest-finalized CE point packages for Tribute, Nod item and Nod bucket through
  the compressed tree plus typed body repositories;
- consensus/finality telemetry and local projection readiness;
- committed header VRF seed by optional block number;
- compiled reward emission information;
- archived finalization certificate and block bytes for follower backfill;
- a coarse sync status.

Governance and other module queries use standard `eth_call` against their precompile
ABIs rather than duplicate custom methods. RPC must preserve normal block-tag
semantics and the execution read lifecycle for compressed-entity precompiles.

## Consistency and response invariants

For a state snapshot, every field in one response must derive from the same block
state or be explicitly labelled compiled/local. Read errors never become plausible
zero/default domain values.

A CE point result must bind chain id, domain/id, commitment-scheme version, exact
finalized height/hash/header root, inclusion/absence proof and canonical body state.
`Present` requires the authenticated leaf and body bytes to agree; body/backend
failure is `Unavailable` or corruption, never `Absent`. V1 deliberately selects the
service's latest finalized marker and accepts no caller block selector.

Finalization RPC is bytes transport only. The server must return certificate and
block for the same requested height; followers verify codec, digest, epoch,
committee and threshold before delivery. Consensus status fields describe the last
finalized reporter snapshot: certificate signer count is not live peer count and
current view is not the live voting view.

Projection readiness binds checkpoint number **and hash** to canonical finalized
history. `ready` requires the configured lag policy and no conflicting/ahead
checkpoint. Wall-clock outage duration is local observability only.

## Failure, concurrency and resource policy

Provider/storage/bridge/Mongo/tree errors map to stable machine-readable RPC error
classes with sanitized messages. Invalid domain/id/proof requests are invalid
params. Unsupported service mode and temporarily unavailable data are distinct
from not-found/cryptographic absence.

Blocking tree/body work runs off the async executor and is concurrency-, memory-
and deadline-bounded. Finalization archive requests, large validator lists and
`eth_call` body scans require server limits. Global HTTP/WS exposure, CORS, request
size, batch size, subscriptions, rate limiting and authentication remain node
configuration owned by the node boundary but must be documented with this API's
cost model.

## Compatibility and trust

JSON method names, camelCase fields, enum strings, numeric encodings, hex codecs,
error codes, CE request/result version and consensus archive codec are public
compatibility surfaces. New consistency semantics require a versioned method/result,
not silent reinterpretation.

Clients trust canonical state only to the degree they trust the selected node and
block tag. CE and finalization packages are independently verifiable when the
client has the chain/committee roots. Local status is advisory and must not be used
as cryptographic finality. Public RPC deployment must assume hostile inputs.

## Production evidence and module audit profile

Evidence inspected includes all RPC API/server code and tests, node mode wiring,
consensus bridge/finalization archive, CE point-read service, typed body readers,
projection readiness and precompile `eth_call` tests. Current serialization tests
pin field vocabulary; precompile tests do not exercise the actual network server or
historical/finalized selection.

The RPC adapter is a boundary module: each method should be a small typed query
whose constructor fixes authority and consistency class. It must not manufacture
defaults, mix snapshots or collapse corruption/unavailability into absence.
Structural closure requires source-tagged response types and production-interface
tests over JSON-RPC transport.

## Consequences and rejected alternatives

Explicit consistency classes let operators and E2E flows choose state view,
projection or proof deliberately. Returning raw finalization bytes is acceptable
because the consumer verifies them; calling them a verified proof at the server
would be misleading. Duplicating every precompile view in the custom namespace was
rejected. Treating “latest” as implicitly finalized was rejected because provider
head semantics can change by node mode and import stage.

## Open questions and technical debt

- `getValidators` swallows each Staking read error with `unwrap_or(U256::ZERO)`.
  Return an all-or-error snapshot or a typed per-record error; zero stake is valid
  domain data and cannot signal corruption/unavailability.
- Correct `getVrfSeed(None)`: documentation says latest finalized, implementation
  uses `best_block_number`. Select `finalized_block_num_hash` or rename/version the
  method as latest canonical.
- Add explicit block selector and response block number/hash to latest-state
  methods, or guarantee and test that the node exposes only finalized canonical
  heads in every validator/follower/full-node mode.
- `syncStatus` reports a bridge-less full node as not syncing with block heights
  zero regardless of DevP2P state. Integrate authoritative Reth sync telemetry or
  return `Unsupported`, not a healthy-looking fabricated status.
- Rename `connectedPeers` or version it as `lastCertificateSignerCount`; it is not
  a P2P connection count. Likewise distinguish finalized view from live view.
- Strengthen projection `ready`: compare checkpoint hash with canonical hash at the
  checkpoint height, reject projection-ahead/conflict, and do not let saturating
  subtraction report zero lag for an impossible ahead state.
- Audit CE body-read error handling. Repository errors are currently collapsed to
  `None` after reporting unavailable; prove `serve_point_read_v1` can never turn a
  present leaf plus missing/corrupt body into an absence response.
- Replace generic internal errors with stable codes for unsupported mode,
  unavailable, deadline, corruption, pruned finalization and invalid request;
  sanitize provider/error debug strings exposed to remote callers.
- Add per-method deadlines/concurrency/size/rate limits for blocking point reads,
  finalization bridge requests, validator enumeration and expensive `eth_call` CE
  scans. Define cancellation behavior after client disconnect.
- Bind compiled `EmissionInfo` to active protocol version/chain id or read the
  active state; a newer/older binary can otherwise describe constants not active
  at the queried block.
- Validate requested finalization height against local finalized range before the
  bridge request and return verifiable height/hash metadata rather than two
  untyped hex blobs alone.
- Document and test HTTP/WS namespace enablement, CORS/hosts, authentication,
  request/batch limits and safe public exposure for validator nodes.
- Add network-level conformance tests across validator, certified follower and
  bridge-less node modes; historical/reorg/finality boundaries; CE present/domain-
  absent/entity-absent/unavailable/corrupt cases; and JSON backward compatibility.
