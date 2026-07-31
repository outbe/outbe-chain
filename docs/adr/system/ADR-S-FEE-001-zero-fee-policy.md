# ADR-S-FEE-001: ZeroFee owns deterministic fee-waiver and daily sponsorship policy

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Transaction admission and protocol UX maintainers
- **Scope:** `crates/system/zerofee`
- **Depends on:** ADR-B-CNS-002, ADR-B-CNS-003, ADR-B-EVM-004, ADR-S-VAL-001, ADR-S-ORC-001, ADR-B-EVM-005
- **Related:** ADR-B-TXP-001 and ADR-B-RPC-001 and ADR-S-ORC-002
- **Supersedes:** ZeroFee portion of former pre-space admission placeholder

## Context

ZeroFee determines when native fee debit may be waived without weakening the
underlying command's authorization. It has two distinct policy paths in one module:

1. a stateless-plus-stateful hook for validator/delegated-feeder Oracle votes; and
2. a general sponsored free-transaction path with a per-signer UTC-day quota held by
   the ZeroFee paymaster.

Txpool admission, EVM pre-fee accounting and receipt soft-failure conversion must
use identical classification. ZeroFee owns policy and quota state, not transaction
ordering or Oracle vote execution.

## Decision

### Oracle vote hook

The static hook registry currently contains one stable hook id:
`OracleSubmitVote`. Stateless classification matches exactly:

- target Oracle address and `submitVote` selector/decodable ABI;
- zero native value and zero priority fee;
- max fee cap at least the public-pool protocol minimum;
- calldata at most 16 KiB; and
- gas limit at most 1,500,000.

Stateful authorization resolves signer to an active, BLS-share-bearing validator or
its delegated feeder and requires no vote already stored for that validator in the
current Oracle period. Authorization returns the represented validator as subject.
It grants only fee waiver; Oracle revalidates command authority during execution.

### Daily sponsorship

The sponsored path accepts no contract creation/value, requires zero priority fee,
minimum fee cap, gas at most 200,000, calldata at most 16 KiB and a target in the
fork-governed precompile whitelist. Signer must not be the paymaster and must have a
nonzero native balance as the current anti-Sybil gate.

Each signer has a packed `(UTC yyyymmdd, count)` counter and may consume at most
eight sponsored transactions per UTC day. Day reset is lazy: a prior-day slot reads
as zero for current day. Executor authorization verifies quota/balance and records
use plus receipt-visible authorization event in the pre-fee transaction path.
Public ABI exposes only effective read predicates/counter; it cannot mutate quota.

### Failure contract

Stable numeric policy-error codes `100..=199` appear in the receipt-visible
`OutbeFailure` log when executor converts a rejected zero-fee attempt into the
specified failed receipt. Codes are never reordered/reused. A paid transaction that
does not request the exact zero-fee envelope follows normal fee policy rather than
being owned by the hook.

## Persistent state and invariants

- Hook ids and registry order are unique and static for a protocol version.
- Classification is deterministic from signed envelope; authorization is
  deterministic from canonical pre-state.
- Oracle waiver subject is exactly one active validator and has not voted this
  period.
- Sponsored packed day decodes canonically; effective count is zero for any other
  day and at most the daily limit for admitted pre-state.
- Every executed/admitted sponsored transaction consumes quota exactly once under
  the accepted “attempt vs success” rule.
- Quota write and authorization receipt evidence commit/rollback with the executor
  fee/execution boundary defined by ADR-B-EVM-001 and ADR-B-EVM-005.
- ABI `authorizeSponsorship` mirrors executor gates or is explicitly only advisory.
- A waiver never bypasses target precompile authorization or changes command input.

## Atomicity, replay and admission consistency

Txpool may precheck policy, but execution reauthorizes against canonical state. Two
same-signer transactions racing the last quota/vote slot are ordered canonically;
only the valid pre-state winner succeeds. Reorg rolls counters/votes back with EVM
state. Restart reconstructs from state, not txpool caches.

Whether quota is burned on authorization, execution attempt or successful target
execution is normative and must match code/receipts. A failed soft receipt must not
allow infinite free retries unless that is deliberate policy.

## Security, compatibility and bounds

Limits, whitelist, selector, hook ids, minimum fee semantics, UTC date conversion,
packed counter encoding and failure codes are consensus/admission formats. Updates
require activation across txpool and executor simultaneously.

Nonzero balance is only a weak cost signal, not Sybil resistance. Whitelisted
precompiles must have bounded work under sponsored gas and cannot expose indirect
arbitrary calls/value extraction. EIP-7702 and account-abstraction semantics require
explicit signer/authority analysis.

## Production-interface verification evidence

Inspected hook types/registry, Oracle envelope and state authorization, general
sponsorship constants/runtime/state/precompile, packed counter/lazy reset and error
code uniqueness tests. Full txpool-to-executor consistency, concurrency, reorg,
EIP-7702 and target-call e2e are incomplete.

## Consequences

Fee waiver remains a narrow authorization result rather than a special transaction
type that can bypass target policy. Wallets have deterministic advisory reads and
stable failure codes, while canonical execution remains final authority.

## Rejected alternatives

- **Trust txpool-only checks:** private/block-builder or reorg execution can bypass
  stale admission state.
- **Use nonce alone as anti-Sybil:** EIP-7702/sponsored processing weakens that cost.
- **Expose `recordUse` ABI:** users could burn/race quota out of band.
- **Set max fee to zero:** public txpool protocol minima would reject the envelope.
- **Whitelist arbitrary contracts:** sponsored indirect execution becomes unbounded.

## Open questions and technical debt

1. Define exactly when daily quota is consumed: admission, pre-fee authorization,
   target execution attempt or successful receipt. Test all revert/soft-failure paths.
2. `record_use` uses saturating add. Exceeding the proven gate must be an invariant
   error, not silent `u32::MAX` saturation.
3. Public `authorizeSponsorship` duplicates executor checks inline but cannot check
   full transaction envelope/target. Rename/document it as partial advisory or share
   one policy function to prevent drift.
4. The generic sponsorship classifier/runtime is not represented in the static
   `ZeroFeeHook` registry shown for Oracle. Document the exact precedence and ensure
   one envelope cannot match two waiver paths.
5. Prove txpool and executor construct identical `ZeroFeeTransaction`, especially
   EIP-1559 optional priority fee, EIP-7702 signer and calldata bytes.
6. Nonzero native balance is weak anti-Sybil protection. Quantify attack cost and
   define minimum balance, funding provenance or a stronger identity rule.
7. Whitelist changes require per-target worst-case gas, reentrancy/indirect-call and
   value-extraction review; add a structural whitelist audit.
8. Daily counters grow one slot per signer forever. Define state-growth bounds,
   cleanup/rent or accept permanent storage explicitly.
9. UTC day reset at timestamp boundary permits eight calls immediately before and
   after midnight; confirm intended burst capacity.
10. Oracle hook scans validator/delegation state through Oracle's unbounded resolver;
    admission DoS bounds depend on ADR-S-ORC-001 reverse-index work.
11. Stable error reasons include internal storage strings. Ensure receipt/log size is
    bounded and does not leak nondeterministic implementation detail.
12. Define behavior when zero-fee state authorization passes but target Oracle vote
    later rejects due to changed same-block state.
13. Add concurrent last-quota/last-vote, reorg, restart and duplicate transaction
    tests at real txpool/executor interfaces.
14. Add EIP-7702/account-abstraction threat tests and document whether sponsor or
    authority address owns quota.
15. Prove paymaster native balance funding/debit/accounting and exhaustion behavior;
    this module's counter alone does not close the economic source of sponsored gas.
16. Add activation compatibility tests showing old/new nodes classify every boundary
    envelope identically at the fork height.
