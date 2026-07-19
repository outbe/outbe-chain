# ADR-S-GOV-002: Vote owns executable proposal tally and dispatch

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `crates/system/vote`; executable proposals, ballots, tally and
  target-handler dispatch
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-S-VAL-001
- **Supersedes:** The Vote-local portions of the deleted pre-space governance aggregate

## Context

Executable governance needs deterministic validator eligibility, a closed voting
window and target-specific payload validation. These concerns are independent of
the editorial OIP/GIP registry and of the state a successful target later owns.

## Decision

Vote is the sole owner of executable proposal and ballot state. A compile-time
`VoteTargetRegistry` defines which module addresses may receive proposals, validates
their JSON payloads before creation, and handles the terminal tally in begin-block.
Target modules own all resulting domain state; Vote retains only proposal history.

## Commands, authority and state

Only an `ACTIVE` ValidatorSet member may create a proposal. `ACTIVE` and `PENDING`
validators may cast one yes/no ballot per proposal through its inclusive deadline.
Creation is bounded globally and per proposer. Unknown or duplicate target handlers
and malformed/target-invalid payloads fail before proposal allocation.

State consists of a monotonic proposal counter, proposal records, a bounded pending
id list, a per-proposal dense ballot list, and a composite
`keccak256(proposal_id || voter)` map whose nonzero value is the ballot's one-based
position. Every map entry must resolve to the same voter in the dense list; every
pending id must name exactly one `Pending` proposal.

## State machine and tally snapshot

```text
Pending --after deadline, yes >= 2/3 of active set--> Approved
Pending --after deadline, quorum absent------------> Expired
Pending --approved target handler fails------------> Rejected
```

All terminal states are final. Tally occurs only when
`block_number > voting_deadline_height`. It re-reads the current active validator
set, ignores stored ballots from validators no longer active, and uses that same
active count as the denominator. `No` votes are recorded but the decision is a
yes-vote quorum, not a yes-versus-no majority.

## Ordering, atomicity and replay

Vote begin-block runs before Update activation under ADR-B-EVM-001. For an approved
proposal, target handling, Vote status/index mutation and finalization event execute
inside the containing system transaction's checkpoint. A target-handler error
converts the proposal to `Rejected`; errors while processing a non-approved outcome
remain block-fatal. A terminal proposal is skipped on replay and a second ballot is
rejected by the composite index.

Pending-list removal uses swap-remove, so enumeration order is explicitly unstable.
Proposal ids and ballot order remain stable. The proposal counter uses unchecked
`U256 + 1` semantics and requires an explicit exhaustion contract.

## Security and compatibility

The handler registry is compile-time consensus configuration: every validator
binary must expose the same unique address-to-handler mapping. Localnet alone may
override the voting window through `OUTBE_TEST_VOTING_WINDOW_BLOCKS`; production
uses the compiled constant. JSON payload interpretation and raw status/vote bytes
are consensus formats and require activation discipline when changed.

## Production-interface evidence

Evidence inspected in `crates/system/vote/src/{precompile,runtime,state,handlers,
schema,constants}.rs`, its guard/precompile tests, ValidatorSet reads, and the EVM
begin-block ordering. Required closure evidence includes changing-validator-set
tallies, injected handler failures, registry parity across binaries and corruption
tests for the dense-list/composite-map pair.

## Consequences and rejected alternatives

Vote can safely support multiple target modules without importing their storage or
creating dependency cycles. Runtime registration was rejected for now because it
would make handler availability mutable consensus state. Counting every historical
ballot was rejected: current active-set authority determines executable approval.
Using Governance editorial status as a ballot was rejected as an authority bypass.

## Open questions and technical debt

- Decide whether eligibility and quorum should be snapshotted at proposal creation;
  current membership changes can add, remove or invalidate voting weight mid-window.
- Clarify why `PENDING` validators may vote but their ballot is ignored until they
  are active at tally time, and prove this is resistant to boundary manipulation.
- The model exposes `Rejected`, but ordinary quorum failure produces `Expired`;
  specify whether handler failure is the only intended rejected path.
- Define counter exhaustion and bound total historical ballots/payload storage.
- Make canonical JSON/schema/version rules explicit; semantic payload changes must
  not depend on serde implementation drift.
- Add invariant checks for pending ids, ballot indexes and reserved packed-record
  bytes, including injected rollback failures.
- Prove all production binaries compile the identical unique target registry.
