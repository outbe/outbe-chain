# ADR-S-GOV-002: Vote owns executable proposal tally and dispatch

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `crates/system/vote`; executable proposals, ballots, tally and
  target-handler dispatch
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-S-VAL-001
- **Used by:** ADR-C-TOK-004
- **Supersedes:** The Vote-local portions of the deleted pre-space governance aggregate

## Context

Executable governance needs deterministic validator eligibility, a closed voting
window and target-specific payload validation. These concerns are independent of
the editorial OIP/GIP registry and of the state a successful target later owns.

## Decision

Vote is the sole owner of executable proposal, ballot and proposal-value escrow
state. A compile-time `VoteTargetRegistry` defines which module addresses may
receive proposals, their proposer/value admission class, canonical payload
validation, reservation hooks, Approved execution and terminal cleanup. Target
modules own reservations and resulting domain state; Vote retains proposal history
and value-settlement evidence.

## Commands, authority and state

The default admission class is `ActiveValidatorOnly` with zero attached native
value. A target may instead declare a compile-time exact `PublicBonded { amount }`
class; ADR-C-TOK-004 is the only V1 use. A public bonded proposal requires proposer
to equal the target payload identity and exact `msg.value`; it does not weaken ballot
or quorum authority. `ACTIVE` and `PENDING` validators may cast one yes/no ballot per
proposal through its inclusive deadline.

Creation is bounded globally, per proposer and by target admission policy. Unknown
or duplicate target handlers, unexpected value and malformed/target-invalid payloads
fail before proposal allocation. After id allocation, a target reservation hook and
proposal record commit atomically.

State consists of a monotonic proposal counter, proposal records including bond
amount/settlement state, a bounded pending id list, a per-proposal dense ballot list,
and a composite `keccak256(proposal_id || voter)` map whose nonzero value is the
ballot's one-based position. Native balance held by `VOTE_ADDRESS` must be at least
the sum of unsettled proposal-bond liabilities; forced or historical surplus is
tracked as non-liability and cannot be refunded as a bond. Every map entry must
resolve to the same voter in the dense list; every pending id must name exactly one
`Pending` proposal.

## State machine and tally snapshot

```text
Pending --after deadline, yes >= 2/3 of active set--> Approved
Pending --after deadline, quorum absent------------> Expired
Pending --approved target returns domain rejection-> Rejected
Pending --approved target execution fails----------> Pending (block retry)
```

All terminal states are final. Tally occurs only when
`block_number > voting_deadline_height`. It re-reads the current active validator
set, ignores stored ballots from validators no longer active, and uses that same
active count as the denominator. `No` votes are recorded but the decision is a
yes-vote quorum, not a yes-versus-no majority.

## Ordering, atomicity and replay

Vote begin-block runs before Update activation under ADR-B-EVM-001 inside the atomic
pre-execution hook batch. Approved target handling runs in a nested checkpoint so a
typed recoverable domain rejection can roll back every target effect before Vote
records `Rejected`. Target terminal cleanup, Vote status/index mutation, bond
settlement and finalization logs then commit in that hook-batch checkpoint and are
published through the mandatory `HookEvents` system-transaction receipt.

Successful Approved execution refunds an exact bond liability from `VOTE_ADDRESS`
to the proposer. Expired and Rejected outcomes burn it with the standard native
`decrease_balance` primitive. OOG, unsupported provider capability, storage/provider
corruption, impossible schema/invariant state, cleanup failure, refund failure or
burn failure is fatal: the outer hook batch rolls back, the proposal remains Pending
and its liability remains unsettled for deterministic retry. Terminal replay cannot
settle value twice. A second ballot is rejected by the composite index.

Pending-list removal uses swap-remove, so enumeration order is explicitly unstable.
Proposal ids and ballot order remain stable. The proposal counter uses unchecked
`U256 + 1` semantics and requires an explicit exhaustion contract.

## Security and compatibility

The handler registry and admission/bond table are compile-time consensus
configuration: every validator binary must expose the same unique
address-to-handler mapping and constants. Runtime registration and proposal-selected
quorum, deadline or bond are forbidden. Localnet alone may override the voting
window through `OUTBE_TEST_VOTING_WINDOW_BLOCKS`; production uses the compiled
constant. Target validation receives original payload bytes, proposer and attached
value; a lossy generic JSON value is not sufficient for a canonical-byte contract.
JSON payload interpretation, value admission and raw status/vote/bond bytes are
consensus formats and require activation discipline when changed.

## Production-interface evidence

Evidence inspected in `crates/system/vote/src/{precompile,runtime,state,handlers,
schema,constants}.rs`, its guard/precompile tests, ValidatorSet reads, and the EVM
begin-block ordering. Required closure evidence includes changing-validator-set
tallies, injected handler failures, registry parity across binaries and corruption
tests for the dense-list/composite-map pair.

## Consequences and rejected alternatives

Vote can safely support multiple target modules without importing their storage or
creating dependency cycles. Target-specific public admission is explicit and bonded,
while voting authority remains unchanged. Runtime registration was rejected because
it would make handler availability mutable consensus state. Counting every
historical ballot was rejected: current active-set authority determines executable
approval. Using Governance editorial status as a ballot was rejected as an authority
bypass.

## Open questions and technical debt

- Decide whether eligibility and quorum should be snapshotted at proposal creation;
  current membership changes can add, remove or invalidate voting weight mid-window.
- Clarify why `PENDING` validators may vote but their ballot is ignored until they
  are active at tally time, and prove this is resistant to boundary manipulation.
- `Rejected` is reserved for a typed recoverable target-domain rejection after
  quorum. Fatal execution/provider/invariant failure retains Pending and retries.
- Define counter exhaustion and bound total historical ballots/payload storage.
- Pass original payload bytes to target validation and make canonical
  JSON/schema/version rules explicit; semantic payload changes must not depend on a
  `serde_json::Value` round trip.
- Add invariant checks for pending ids, ballot indexes and reserved packed-record
  bytes, including injected rollback failures.
- Prove all production binaries compile the identical unique target registry.
- Implement the compile-time admission/reservation/terminal-hook contract and exact
  public-bonded exception required by ADR-C-TOK-004; current code remains
  `ACTIVE`-creator-only and rejects value.
- Add proposal bond schema, `VOTE_ADDRESS balance >= liabilities` accounting,
  forced-surplus handling, checked balance credit, typed refund/burn events and
  injected rollback tests at every target/cleanup/settlement step.
- Add the nested target checkpoint before any handler capable of returning a
  recoverable Rejected outcome; outer-handler rollback alone cannot both erase
  partial target effects and retain Rejected state.
