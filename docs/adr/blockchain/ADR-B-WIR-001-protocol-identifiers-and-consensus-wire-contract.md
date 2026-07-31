# ADR-B-WIR-001: Protocol identifiers and consensus wire formats are one versioned registry

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Protocol, consensus and execution maintainers
- **Scope:** `outbe-primitives` addresses, header/block/payload aliases, protocol schedule, block artifacts and shared wire records
- **Depends on:** ADR-B-NOD-001, ADR-B-CNS-002, ADR-B-CNS-003, ADR-S-GOV-003
- **Related:** ADR-B-CRY-001, ADR-B-TXP-001, ADR-B-CLI-001, ADR-B-EVM-002

## Context

Every node component must agree on reserved accounts, block/header/transaction
types, system artifact tags, protocol constants and canonical codecs. These values
are not utility implementation details: a collision, duplicated constant or
dependency-driven codec change can split execution, consensus, RPC and tooling.

This ADR owns the registry and representation contract. Stateful behavior attached
to an address remains in that module's ADR; system transaction sequencing remains
ADR-B-EVM-001; storage capabilities are ADR-B-EVM-003.

## Decision

### Protocol identifier registry

One checked registry assigns every protocol address, domain id, artifact tag,
selector, codec version and retired value. Each entry names its owner ADR, public or
internal accessibility, genesis account requirement, dispatch kind and activation.
Generation produces Rust constants, genesis validation and documentation; duplicate
or unregistered literal identifiers fail CI.

Addresses distinguish:

- stateful/read-only/stateless precompile dispatch;
- system-owned storage accounts with no public dispatch;
- logical log-emitter namespaces;
- reserved system sender/recipient identities; and
- ordinary predeployed EVM bytecode accounts.

`SYSTEM_ADDRESS`/zero caller, `OUTBE_SYSTEM_TX_ADDRESS`, fee beneficiary and every
user-visible precompile are distinct authorities. Reserved addresses cannot be
created, called, funded or sent from contrary to their declared policy.

### Ethereum-compatible base envelope

`OutbeHeader` RLP is byte-for-byte the upstream Ethereum `Header`; block hash is
`keccak256(rlp(standard_header))`. Outbe-specific consensus/execution information is
carried only in the bounded canonical `extra_data` artifact envelope. Transactions
and receipts use explicitly pinned Ethereum envelope semantics, and payload/RPC
conversion preserves exact hashes/roots/gas fields.

Upstream type aliases are compatibility dependencies: an upgrade is accepted only
after frozen RLP/compact/database/RPC vectors and fork-field behavior pass. Wrapper
methods that parse protocol artifacts return typed errors for strict callers;
lossy convenience methods are named as such and forbidden in validation.

### Outbe block artifact envelope

The envelope has magic, one active version, canonical ascending unique TLV tags,
strict lengths/counts and a total `OUTBE_MAX_EXTRA_DATA_SIZE` bound. Active records
currently cover execution summary, DKG/boundary or dealer/preannounce artifact,
millisecond timestamp part, late-finalize credits and compressed-entity root.

The tag registry permanently reserves retired tags. Decoder consumes all bytes,
rejects duplicates, unknown mandatory tags, noncanonical order/count/length and
oversize input. Structural decoding is separated from contextual validation, but
every production admission/execution path must run the latter with block height,
committee, fork and expected system layout.

### Time and root semantics

Ethereum `header.timestamp` is whole Unix seconds. A validated artifact carries
`timestamp_millis_part` in `0..1000`; full consensus time uses checked
`seconds * 1000 + part`. Missing/malformed part is an error in consensus paths.

Compressed-entity artifact binds commitment scheme and sealed root; block 1+ requires
exactly one. DKG, committee, late-finalize and execution-summary artifacts bind all
fields needed for independent contextual verification, not process-local objects.

### Protocol schedule

One immutable schedule version contains activation heights, evidence/proof bounds,
retention, timeouts and benchmark-derived gas/performance limits. Production cannot
construct arbitrary schedules. Constants required at compile time are generated
from the same manifest and checked for equality; no module duplicates a numeric
protocol value under a local constant.

Changing registry, schedule, base envelope interpretation or artifact codec is a
named protocol version activated by ADR-S-GOV-003 with compatibility/migration vectors.

### Ownership map for remaining primitives

| Primitive family | Normative owner |
|---|---|
| Consensus metadata, participation, P2P and committee facts | ADR-B-NOD-001, ADR-B-WIR-001, ADR-B-GEN-001 and ADR-S-VAL-001 |
| Projection readiness/checkpoint | ADR-B-SUP-001, ADR-B-OCD-004 |
| Governance/slashing/accounting journals | corresponding module ADRs |
| System transaction codec/layout | ADR-B-EVM-001 and ADR-B-WIR-001 |
| Signer/key path | ADR-S-KEY-001 and execution wiring |
| TEE bootstrap payload | ADR-S-TEE-001 and ADR-S-TEE-002 |
| ZeroFee shared records | ADR-S-FEE-001 and ADR-B-EVM-005 |
| Units/time/economic math | ADR-C-LBM-001 |
| Storage provider/DSL/types | ADR-B-EVM-003 |

## Invariants

- Every protocol identifier is unique, registered and has one owning decision.
- Header RLP/hash is exactly Ethereum-compatible for the active upstream fork.
- Artifact encoding is canonical, bounded and wholly consumed by strict decoding.
- Structural success never substitutes for required contextual validation.
- Header artifacts, body system transactions and execution result describe the
  same block-scoped facts.
- Schedule/constants are single-source and activation-versioned.
- RPC/database/wire conversions preserve exact consensus identity.

## Security and trust assumptions

Consensus trusts frozen codec/hash algorithms and contextual validators, not serde,
debug output or upstream defaults. Unknown optional extension policy must be explicit;
silently ignoring a state-affecting tag is forbidden. Inputs are adversarial and
length/count bounded before allocation/cryptography.

## Compatibility and activation

Address assignments are permanent once history exists. Tags/selectors/versions are
never reused. An artifact change affects header hash and requires coordinated
activation. Upstream Reth/Alloy type upgrades are protocol changes unless golden
vectors prove byte-identical behavior for all active forks.

## Production-interface verification evidence

Inspected exported primitives modules, full address registry/comments, Ethereum
header wrapper/RLP/hash/RPC conversion, artifact TLV encoder/decoder and bounds,
protocol schedule, system transaction types and production consumers/tests.
Coverage is broad but registry generation, duplicate-constant prevention and strict
versus lossy API enforcement remain incomplete. Status remains Proposed.

## Consequences

Shared primitives become an explicit protocol language rather than an unowned
collection of constants. module audits can follow each identifier/type to its state
owner while still checking the cross-component wire invariant once.

## Rejected alternatives

- **One ADR for all code in the primitives crate:** state/module decisions are
  duplicated and the document becomes another aggregate.
- **Let modules define local constants:** drift is detected only after divergence.
- **Add fields to Outbe header RLP:** Ethereum tooling/light-client compatibility
  breaks.
- **Treat upstream aliases as automatically compatible:** dependency upgrades can
  silently change consensus bytes.

## Open questions and technical debt

1. Addresses are handwritten constants with prose ownership. Add a machine-readable
   registry and CI uniqueness/range/dispatch/genesis-allocation checks.
2. Search and eliminate raw address/tag/selector literals outside the registry;
   current comments cannot prove there are no shadow definitions.
3. `OutbeHeader::timestamp_millis_part` maps every malformed artifact to zero. Rename
   it as lossy and provide/require a strict typed result in all protocol consumers.
4. `OutbeHeader::timestamp_millis` uses saturating multiplication/addition, turning
   overflow into a valid maximum timestamp. Contextual validation must use checked
   arithmetic and reject overflow.
5. Timestamp range `< 1000` is deliberately not enforced by the artifact codec.
   Prove every block admission, replay, RPC simulation and consensus path invokes
   the contextual validator before using the value.
6. `GENESIS_BOOTSTRAP_BLOCK_NUMBER` exists in `system_tx.rs` while the protocol
   schedule has the same value. Generate one from the other or remove duplication.
7. `reshare_artifact` version/history comments contain overlapping version claims
   while active `VERSION` is private `0x0A`. Publish a precise version/tag changelog
   and export typed version metadata for tooling.
8. Artifact types mix structural and semantic validation in dispersed consumers.
   Introduce one typed `ValidatedBlockArtifacts` constructor for a block context.
9. The artifact root record is structurally optional but mandatory at block 1+.
   Add exhaustive height/fork tests proving no consumer accepts absence/duplicates.
10. Unknown-tag compatibility policy must distinguish ignorable telemetry from
    mandatory consensus semantics; current all-or-nothing behavior and future
    extension process need a normative rule.
11. Several artifact payload lengths are only limited by `u16/u32` before the final
    extra-data cap. Validate total size before large allocation and derive per-tag
    bounds from committee/protocol limits.
12. Committee preannounce/outcome and DKG payloads contain opaque `Bytes`. Their
    inner codec/version/canonicality belongs to consensus but must be validated
    before accepting the outer artifact.
13. `OutbeTxEnvelope`, receipt and tx-type are upstream type aliases. Pin Reth/Alloy
    versions and run frozen EIP-2718/RLP/receipt-root/database compact vectors on
    dependency upgrades.
14. Header derives `reth_codecs::Compact`; prove compact round-trip preserves every
    active Ethereum fork field and exact hash, including newly added upstream fields.
15. RPC conversion unwraps `OutbeHeader` to upstream `Header`. Add end-to-end vectors
    that Outbe artifacts remain visible in `extraData` and no custom time/root field
    is synthesized inconsistently.
16. `SYSTEM_ADDRESS = Address::ZERO` overlaps conventional burn/zero-address
    semantics. Audit all balance/code/caller checks and document why no user can gain
    system authority through zero address behavior.
17. The “test-only” debug subcall address is present in the shared registry. Prove
    dispatch/genesis allocation is compile-/chain-spec-gated off in production.
18. Marker-bytecode preservation for system-owned storage accounts is maintained in
    executor/chain spec allowlists. Generate and reconcile it from the address
    registry to prevent state loss under EIP-161.
19. Protocol schedule contains operational timeouts and consensus/economic limits in
    one struct. Classify which fields are consensus-validity, proposer policy or
    local SLO so operator tuning cannot accidentally become a fork.
20. Production paths can call `OutbeProtocolSchedule::default`; there is no encoded
    schedule/version commitment in chain spec/header. Bind active schedule to chain
    identity/activation and expose it through RPC.
21. Error enums and serde representations are shared APIs but not systematically
    versioned. Separate stable wire errors from internal Rust diagnostics.
22. Add a registry completeness tool mapping every exported primitive to an owner
    ADR and every active codec to independent fuzz/golden-vector evidence.
