# OCOMP framework review

Date: 2026-07-23

Status: non-normative research/consolidation record; no production code changed.
The revision hashes below identify the historical inputs reviewed on that date;
later normative amendments are tracked by the PoC scope/continuity audit.
The proposed canonical decisions are
[`ADR-S-OCM-001`](../docs/adr/system/ADR-S-OCM-001-ocomp-kernel-and-typed-program-boundary.md)
through
[`ADR-S-OCM-004`](../docs/adr/system/ADR-S-OCM-004-certified-activation-job-fsm-and-protocol-versioning.md);
the canonical end-to-end test flow is
[`PFS-002`](../docs/flows/002-off-chain-poc-protocol-flow.md).

Reviewed revisions:

- [`off-chain-computation.md`](../off-chain-computation.md), SHA-256
  `5bc67987f36a0dd0e5da4c66ce44b1e11ab479f9921350a055dccc6cd0779834`;
- [`off-chain-poc.md`](../off-chain-poc.md), SHA-256
  `fe9d14a6a697a3b4acef1e013188a9a82f21d5665664de5b72e374dbfbd534ce`.

## Question

Does the current design create a reusable off-chain computation framework, or
does it only move Lysis out of the node?

## Evidence

The current code has no OCOMP framework. Metadosis calls Lysis synchronously,
and Lysis combines domain reads, computation and effects
([Metadosis runtime](../crates/core/metadosis/src/runtime.rs),
[Lysis runtime](../crates/core/lysis/src/runtime.rs)):

```text
Metadosis::process_metadosis
  -> lysis(StorageHandle, ...)
  -> NodFactory / Intex / Tribute / Desis / Promis mutations
```

The design document already describes reusable lifecycle, process, evidence and
activation mechanics. However, its V1 wire objects are concretely Lysis-shaped:
the intent contains WWD/Metadosis/Tribute data, units contain Lysis phases, and
the result/activation contains Nod/contributor/Lysis commitments. The concern
that the design reads as “Lysis only” was therefore valid.

## Independent reviews

Three groups reviewed the documents, current code and primary implementation
precedents from different positions:

| Review | Starting position | Consolidated finding |
|---|---|---|
| framework-first | find the minimum stable reusable kernel | separate lifecycle/evidence/process ownership from typed Lysis semantics and effects |
| anti-overgeneralization | try to disprove a generic framework | do not infer generic wire types, registry or write model from one program |
| evolution/integration | test PoC→MVP and a future Gem/Nod path | implement the internal kernel now; use a real second program as the extraction gate |

After cross-critique all three converge on the same boundary:

```text
architecture seam now
  OCOMP operational kernel | concrete typed domain protocol

PoC implementation
  internal kernel          | Lysis V1 only

future multi-program wire
  extracted only after a qualified second end-to-end program
```

## Decision

1. The PoC implements a concrete internal `OcompKernel` boundary now, so
   lifecycle, finality, evidence and process mechanics do not become Lysis
   orchestration.
2. V1 wire remains Lysis-specific. Generic-looking names do not mean
   program-neutral schemas.
3. There is no one-entry consensus registry, generic envelope, public
   `TaskAdapter`, arbitrary bytecode or generic storage-write capability in the
   PoC.
4. Lysis retains typed input, planner, units, result verifier, preconditions,
   effect capability, receipts, conservation and cleanup.
5. A future program uses new fork-pinned typed objects and signature domains; it
   never reinterprets Lysis V1 bytes.
6. Shared consensus/source abstractions are extracted only from the demonstrated
   intersection of two real programs.

## Second-program qualification gate

A second adapter is not enough. A candidate qualifies only if it has:

- an independent domain postcondition and typed intent/result;
- authenticated complete input enumeration and canonical ordering;
- its own caps, preconditions and conflict rules;
- deterministic execution and domain verifier;
- private typed apply authority and owner-controlled effects;
- conservation/witness/receipt/recovery rules;
- a complete finality, activation, expiry and replay path;
- contention tests with Lysis.

Gem qualification is the preferred destructive test because its
owner/global/bin indexes and state transition differ from Lysis. It remains
post-PoC design work until a domain ADR supplies those missing contracts.

## Rejected designs

| Rejected now | Reason |
|---|---|
| `ProgramId + opaque bytes` | creates type erasure and an arbitrary execution surface |
| generic write sets/calls/opcodes | bypasses domain-owned invariants and authority |
| one-entry consensus registry | duplicates `ProtocolBundleV1` without adding a capability |
| generic intent/unit/result envelopes | freezes guessed Lysis assumptions as framework rules |
| generic activation capability | risks cross-program effect authority |
| dummy Gem/Nod adapter | does not prove a reusable seam |

## Primary precedents

- [Cosmos SDK module manager](https://docs.cosmos.network/sdk/v0.53/build/building-modules/module-manager):
  shared lifecycle coordination with module-owned rules.
- [Cosmos SDK keepers](https://docs.cosmos.network/sdk/v0.53/build/building-modules/keeper):
  narrow capability boundaries around module state.
- [Hyperledger Fabric chaincode lifecycle](https://hyperledger-fabric.readthedocs.io/en/latest/chaincode_lifecycle.html):
  committed semantics are distinct from a local implementation package.
- [Hyperledger Fabric transaction flow](https://hyperledger-fabric.readthedocs.io/en/release-2.2/txflow.html):
  independent execution/endorsement before ordered validation.
- [Ethereum node architecture](https://ethereum.org/developers/docs/nodes-and-clients/node-architecture):
  process separation behind a narrow versioned interface.
- [EIP-2718 typed transaction envelopes](https://eips.ethereum.org/EIPS/eip-2718):
  closed typed extension without reinterpreting old bytes.
- [RISC Zero receipts](https://dev.risczero.com/api/zkvm/receipts):
  proof binds a program and output, but does not define domain input completeness
  or state-application authority.

These are mechanism precedents, not wholesale designs for Outbe.

## Scope effect

The decision changes framing and required source-module ownership only. It does
not change `JobIntentV1`, `UnitSpecV1`, `ActivationPayloadV1`,
`ProtocolBundleV1`, hash domains, `POC-01..POC-26`, the thirteen-step
demonstration or any PoC runtime acceptance condition.
