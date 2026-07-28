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
or quorum authority. Only validators that are `ACTIVE` when the ballot is cast may
cast one yes/no ballot per proposal through its inclusive deadline.

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
`Pending` or retained `Error` proposal. Begin-block tally processes only `Pending`;
`Error` remains indexed solely so its caps, bond and target reservations stay owned
until a future governance transition resolves it.

## State machine and tally

```text
Pending --after deadline, quorum absent---------------------> Expired
Pending --after deadline, quorum reached, target Applied----> Approved
Pending --after deadline, quorum reached, target Error------> Error
```

`Approved` and `Expired` settle the proposal in V1. `Error` records that approved
target execution failed; automatic retry, cancellation and settlement of an
`Error` proposal are outside V1 and require a new validator-approved governance
transition. Tally occurs only when
`block_number > voting_deadline_height`. It re-reads the current active validator
set, ignores stored ballots from validators no longer active, and uses that same
active count as the denominator. There is no proposal-creation validator snapshot.
`No` votes are recorded but the decision is a yes-vote quorum, not a yes-versus-no
majority.

## Ordering, atomicity and replay

Vote begin-block runs before Update activation under ADR-B-EVM-001 inside the atomic
pre-execution hook batch. Approved target handling runs in a nested checkpoint.
Successful target execution commits before Vote records `Approved`. A target-declared
`Error` outcome rolls back every target effect before Vote records `Error` and emits
its finalization event; the containing block continues. An outer execution
`Err`—including storage/provider failure, unsupported capability or fatal
inconsistency—is not a proposal outcome: it propagates to the hook caller and the
enclosing atomic block execution rolls back.

Successful Approved execution refunds an exact bond liability from `VOTE_ADDRESS`
to the proposer. Expired outcomes burn it with the standard native
`decrease_balance` primitive. `Error` leaves the recorded bond liability and all
target reservations unchanged for the future governance decision. Terminal replay
cannot settle value twice. A second ballot is rejected by the composite index.

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

- Eligibility is resolved without a proposal-creation snapshot: only `ACTIVE`
  validators may cast, and the current `ACTIVE` set at tally time defines both
  eligible ballots and the denominator.
- Define a future validator-approved transition for retrying or closing an `Error`
  proposal and settling its retained reservations and bond. It is outside
  Stablecoin Factory V1.
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
- Add the nested target checkpoint before executable target dispatch; it must erase
  partial target effects while allowing Vote to retain the `Error` status.
