# ADR-B-TXP-001: Txpool owns admission policy and proposer ordering, not block validity

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `crates/blockchain/txpool` and its seam with the Outbe payload
  builder
- **Depends on:** ADR-B-CNS-002, ADR-B-CNS-003, ADR-S-FEE-001, ADR-B-EVM-001
- **Supersedes:** The txpool portion of the former pre-space admission placeholder

## Context

The transaction pool is a node-local, replaceable cache. It decides which signed
transactions a node stores, propagates and offers to its proposer, but cannot make
a transaction valid, free or mandatory. Outbe extends Reth admission to support
Oracle ZeroFee hooks and EIP-7702 sponsorship while excluding the reserved system-
transaction address and prioritizing time-sensitive Oracle votes.

## Decision

Outbe retains Reth's signature, chain, type, nonce, fee, size, blob, replacement,
capacity, eviction and maintenance behavior. The custom validator disables Reth's
early balance gate, applies all other Reth validation, then restores an explicit
cost-versus-balance rule for every transaction not authorized for an Outbe waiver.

The custom policy may admit or reject against latest committed state, but the EVM
executor is final authority and repeats all consensus/economic checks against the
in-block state. Validators must validate a proposed block without consulting their
txpool. Pool contents, arrival order, local configuration and eviction never enter
consensus validity.

## Admission classes

After standard validation:

- any user transaction targeting `OUTBE_SYSTEM_TX_ADDRESS` is permanently invalid
  for pool admission; system transactions are constructed by the payload/executor
  lifecycle and independently rejected in the user zone;
- a malformed transaction that resembles a registered ZeroFee hook is invalid,
  not silently treated as paid;
- a valid registered hook candidate is authorized through ADR-S-FEE-001 against latest
  state and receives a synthetic `U256::MAX` pool balance allowance;
- an account delegated by EIP-7702 to ZeroFee may request sponsorship; matching
  envelopes undergo classification and anti-sybil precheck, while nonmatching or
  explicitly paying envelopes fall back to the normal paid path;
- all other transactions must have `transaction.cost <= latest balance`.

Sponsorship quota is deliberately not checked at admission: the executor produces
the protocol-defined soft-failure when an earlier in-block transaction consumed the
quota. This choice must be capacity-safe because admitted stale/quota-exhausted
transactions can still occupy pool and proposal work.

## Ordering and payload selection

Priority is `(class, effective_tip)`. Ordinary and sponsored transactions use
class 0. A syntactically valid `OracleSubmitVote` ZeroFee candidate uses class 1
and therefore outranks every normal fee bid. Classification errors have no priority.
Adding a new ZeroFee hook requires an explicit exhaustive ordering decision.

Reth's best-transaction iterator still enforces nonce dependencies and replacement
semantics. The Outbe payload builder then enforces remaining block gas, protocol and
Outbe transport size, blob count/sidecar version, compressed-entity transaction and
block work budgets, and actual EVM execution. Invalid or permanently oversized
transactions are marked invalid; temporary CE block-capacity exhaustion defers the
transaction. Begin-zone system transactions are injected before this iterator and
do not compete in the pool priority market.

## State, concurrency and replay

The txpool owns no consensus state. Its durable blob store and in-memory indexes are
rebuildable node-local state. Admission reads one latest provider snapshot, while
head updates and concurrent transactions can immediately stale nonce, balance,
validator, vote or sponsorship state. Such staleness is expected and must resolve
at payload execution without affecting validator agreement.

Duplicate hashes, nonce replacements and reorg reinsertion follow the pinned Reth
pool contract. Outbe-specific errors distinguish permanently bad reserved-address
transactions from transient/state-sensitive ZeroFee rejection so peer reputation
does not punish a valid transaction merely because local state advanced.

## Determinism, limits and trust

Pool ordering should be deterministic for the same pool snapshot and base fee, but
different nodes need not have the same snapshot or proposal. Consensus chooses and
validates the resulting ordered block. Local txpool count/byte/blob limits,
replacement bump, local-transaction exemptions and fee floors affect liveness and
fairness and must be documented as operator policy, not protocol facts.

The ZeroFee registry, priority class, reserved address, maximum calldata/gas and
error classification are security surfaces. The payload builder's
`OUTBE_MAX_BLOCK_SIZE` and CE budgets are protocol-adjacent proposer safety limits
and must match validator rejection/execution rules.

## Compatibility and production evidence

Reth pool API/behavior, transaction cost semantics, EIP-7702 bytecode resolution,
ZeroFee classifier/error codes, priority tuple ordering and payload iterator error
handling are upgrade-sensitive. Dependency upgrades require conformance tests, not
assumption that upstream defaults stayed equivalent.

Evidence inspected includes the complete txpool crate/README/tests, node builder
wiring, payload-builder selection loop, ADR-S-FEE-001 registry policy and executor
reserved/waiver checks. Current tests pin priority, malformed markers, reserved
address and pure sponsorship decisions. Structural closure also requires live pool
tests against a provider, replacement/reorg behavior and proposer/validator
differential execution.

## module audit profile

Txpool is not a consensus-state module, but its policy adapter should be a deep,
closed decision module: `Admit(snapshot, tx) -> Admission`, `Prioritize(tx,
base_fee) -> Priority`, with every balance-bypass path explicit. The payload builder
must consume only validated pool transactions while remaining authoritative for
current-state execution and block resource budgets.

## Consequences and rejected alternatives

Disabling the upstream balance check enables legitimate gasless transactions; the
explicit post-validator rule prevents that implementation detail from granting all
transactions unlimited balance. Executor reauthorization tolerates stale pools and
keeps block validity independent. Making txpool admission consensus-authoritative
was rejected because nodes observe different arrival/state snapshots. Giving every
gasless hook high priority was rejected; priority is an explicit scarce policy.

## Open questions and technical debt

- Add a production provider-backed test proving every inner-validator `Valid`
  branch either receives an explicit authorized bypass or the restored overdraft
  check. Upstream `Valid` shape changes could otherwise reopen the globally disabled
  balance gate.
- Bound high-priority Oracle candidate amplification. Multiple nonce-sequenced
  votes can be admitted from an eligible validator before the first changes state,
  then consume proposal gas as soft failures or crowd paid transactions.
- Define fair ordering among equal class/tip transactions and show it cannot be
  manipulated through local arrival order to starve validators or sponsored users.
- Reconcile admission failure with execution soft-failure semantics for
  `AlreadyVoted`, exhausted sponsorship quota and head races; document which errors
  remain in pool, are dropped, or affect peer reputation.
- Add explicit pool policy/config documentation and tests for count/byte limits,
  eviction, replacement bump, local exemptions, blob persistence and reorg
  reinsertion. The crate delegates these to Reth without pinning current values.
- Verify the reserved-system-address rule across every ingress, including local
  insertion APIs used by bootstrap tooling, reorg reinsertion and direct payload
  attributes; pool rejection alone is not a complete authority boundary.
- Prove `Priority::None` candidates cannot remain selectable through an upstream
  iterator edge case and that classification used for ordering is byte-equivalent
  to admission/executor classification.
- Define behavior when latest-state reads fail. ZeroFee candidates are currently
  rejected as state-sensitive pool errors; readiness/health should distinguish
  provider outage from user-invalid input.
- Pin Reth version semantics for transaction `cost`, nonce dependency,
  replacement, propagation flags, EIP-7702 account code and `is_bad_transaction`.
- Replace wall-clock-dependent blob cache sizing at startup with an explicit
  chain-head/fork input or document why local time cannot select an incompatible
  blob-store capacity around a fork boundary.
- Add differential tests: same candidate set in randomized arrival order,
  proposer build versus independent block execution, head/reorg during async
  validation, CE budget defer versus permanent invalidation, and paid fallback from
  a delegated account after quota exhaustion.
