# ADR-C-AGR-001: AgentReward owns capped daily reward allocation and claims

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Protocol economics maintainers
- **Scope:** `crates/core/agentreward` and its WAA/SRA allocation and claim ledgers
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-S-CYC-001
- **Related:** ADR-S-RWD-001, ADR-C-TRB-002
- **Supersedes:** AgentReward sections of former broad pre-space Cycle/daily-orchestration document (previously numbered 029)

## Context

AgentReward converts a daily pool into per-address native COEN claims using Tribute
activity. It has its own input indexes, capped redistribution algorithm, escrowed
balance, claim lifecycle and residue. These are not Cycle scheduler concerns and
must be auditable without pulling Metadosis or Lysis into the same state boundary.

## Decision

AgentReward owns two role-specific daily count collections (WAA and SRA), per-role
claimable balances, allocation completion/clearing, and native claim payout.
Upstream emission code supplies an exact pool and day; AgentReward returns the exact
undistributed residue to the caller's named sink.

For each role/day, allocation is deterministic:

1. load the unique recipient/count set in canonical address order;
2. distribute proportionally to counts using integer floor division;
3. cap each address at 32% of the original pool;
4. iteratively redistribute excess among still-eligible recipients;
5. credit claimable balances;
6. burn/return all unallocated residue; and
7. clear the day's counts only after credits succeed.

No-recipient allocation returns the whole pool. Minted native value held at
`AGENT_REWARD_ADDRESS` must equal aggregate outstanding claims; returned/burned
residue is not claimable.

The public claim command identifies the caller, transfers no more than its stored
claim, and clears/decrements state in the same EVM rollback domain as the native
balance transfer.

## Interfaces and invariants

Mutation authority consists of the enumerated Tribute/factory activity recorder,
the daily distributor, and user claims. Arbitrary callers may not write counts or
claim for another address.

Required closure:

```text
input_pool = newly_credited_claims + returned_residue
contract_native_balance = aggregate_outstanding_claims
```

Additionally, recipient membership agrees with nonzero counts, a day/role is
distributed and cleared at most once, an address never exceeds its cap for that
allocation, and claims cannot replay.

## Atomicity, determinism and bounds

Count clearing, claim credits, residue result and completion guard are one
transaction. Allocation uses integer arithmetic and canonical address order; no map
iteration or caller-provided ordering may decide dust. Failure rolls back all
credits and keeps counts retryable.

Daily recipient cardinality and count sums need protocol bounds. A fixed iteration
limit is valid only with a proof that it always reaches the specified fixed point or
returns the exact unresolved residue.

## Security, compatibility and evidence

Role identity, cap percentage, count semantics, deduplication, rounding and residue
destination are consensus economics. Changes require activation and before/after
reference vectors.

Inspected tests exercise basic percentages, caps, redistribution, empty recipient
sets, burns/residue and clearing. They do not yet prove arbitrary-population fixed
point behavior, full balance closure, activity-source deduplication or all claim
rollback failures.

## Consequences

AgentReward becomes a deep accounting module with a small interface: record
eligible activity, allocate a supplied pool, claim. Cycle only schedules the
upstream daily command; Metadosis never owns these claims.

## Rejected alternatives

- **Allocate directly in Cycle:** it mixes scheduling with mutable claim state.
- **Drop cap excess/dust:** it violates pool conservation.
- **Use unordered maps:** iteration would affect recipients and consensus state.
- **Clear counts before crediting:** a failure would permanently lose rewards.

## Open questions and technical debt

1. Redistribution stops after ten iterations. Prove this bound for the maximum
   population or implement a mathematically terminating bounded algorithm.
2. Count summation uses unchecked arithmetic in observed paths. Add checked sums
   and explicit per-day/per-address limits.
3. WAA/SRA routing addresses originate in enclave-returned Tribute data. Define
   duplicate-address and cross-role counting semantics and enforce deduplication.
4. Add a structural caller test proving only the intended Tribute/factory path can
   record activity.
5. Prove `contract balance == aggregate claims` after arbitrary allocations,
   residue, partial claims, failed transfers and retries.
6. Define whether counts of zero are forbidden and guarantee membership/index map
   closure after updates and clearing.
7. Define exact dust assignment versus returned residue independently of address
   insertion order and add reference vectors.
8. Add an explicit per-day/role completion guard if clearing an empty index alone
   cannot distinguish “already paid” from “never populated”.
9. Audit native transfer/reentrancy behavior of claims and test rollback when the
   receiver cannot accept value.
10. Clarify whether unused WAA/SRA pool is burned, returned, or credited to
    Metadosis at the owning orchestration layer; use one canonical term and ledger.
11. Add pagination/bounds for recipient queries and a generated large-population
    allocation model.
12. This module needs an ABI-level production-interface test; unit-level direct API
    calls alone do not prove caller authority.
