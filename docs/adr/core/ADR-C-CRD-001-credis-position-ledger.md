# ADR-C-CRD-001: Credis owns credit positions and the installment state machine

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Credis protocol maintainers
- **Scope:** `crates/core/credis`
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004
- **Related:** ADR-C-CRD-002, ADR-C-VLT-001
- **Supersedes:** Credis ledger portions of former broad pre-space Gratis/economic aggregate (previously numbered 030)

## Context

Credis records a borrower's debt after proof, pricing and liquidity delivery have
been approved by CredisFactory. It owns position identity, terms, account indexes,
ten repayment records and overdue queries. It does not verify shielded notes, read
Oracle rates, move ERC-20 assets or release Gratis.

## Decision

A position is created once from a globally unique commitment/nullifier-derived id.
It snapshots bundle account, settlement asset and currency, refinancing rate,
principal, original collateral, creation time, outstanding debt/collateral and an
ordered schedule of exactly ten `Anadosis` installments.

Total debt is derived at creation from principal and the pinned annual refinancing
rate for a ten-month term. Integer division remainder is assigned deterministically
to the final installment so installment debt sums exactly to recorded total debt.
Collateral is likewise partitioned so all installments close the recorded amount.
Due dates are monthly offsets from creation under the currently implemented fixed
30-day convention.

The implicit FSM is:

```text
Absent --create--> Open[next=0]
Open[next=i] --advance_next--> Open[next=i+1]  (i < 9)
Open[next=9] --advance_next--> Complete
Complete --advance_next--> error
```

Only the next installment may advance. Advancement records canonical payment time,
reduces outstanding debt and collateral by that installment, and increments the
cursor atomically. Early payment is currently accepted; this is implementation
evidence pending policy acceptance.

## Authority and interfaces

The public ABI exposes position, installment, next-installment, account-position
and overdue reads. Creation and advancement are privileged internal APIs intended
only for CredisFactory. The ledger trusts factory-supplied snapshotted inputs only
after validating local representational invariants.

Overdue means an unpaid next installment whose due time is earlier than canonical
block time. Account-level overdue checks traverse that account's positions and are
used by CredisFactory to gate new credit.

## Persistent state and invariants

- Every position id is unique and points to one nonzero bundle account.
- Every account index entry points to an existing position owned by that account;
  every position appears exactly once in its owner's dense index.
- Every position has exactly ten ordered installment records.
- Installment due dates are monotonic and terms are immutable after creation.
- Sum of installment debt/collateral equals original recorded totals.
- Outstanding debt/collateral equals the sum of unpaid installments.
- Cursor equals the first unpaid installment and lies in `0..=10`.
- Paid installments before the cursor have exactly one nonzero payment time; later
  installments are unpaid.
- Complete means cursor ten and both outstanding quantities zero.

Saturating subtraction is forbidden for invariant closure: an installment larger
than outstanding state must fail as corruption.

## Atomicity, replay and failure

Position creation writes record, ten installments and owner indexes in one EVM
frame. Advancement is rolled back with CredisFactory's token deposit and reclaim
commitment insertion. The cursor is the repayment replay guard; position identity
guards duplicate creation.

Missing record, completed position and illegal cursor are business/state errors.
Broken indexes, missing scheduled installments, arithmetic mismatch and underflow
are invariant failures. No getter may silently skip corrupt records and still report
a healthy position.

## Determinism, bounds and compatibility

Term length, 30-day duration, debt formula, rounding remainder, currency/rate scale,
field widths and position-id derivation are consensus formats. Changes require
migration and before/after vectors. Per-account scans require a maximum or pagination
before they may be used in transaction admission at unbounded size.

## Production-interface verification evidence

Inspected schema, creation arithmetic, position/account indexing, next-installment
advancement, early/due-date tests, outstanding updates, overdue scans and ABI reads.
Tests cover core examples but not generated closure, corruption, scale bounds or
all factory rollback points. Status remains Proposed.

## Consequences

Credis becomes a pure debt-state module. Proof privacy, rates, assets and liquidity
remain in CredisFactory/VaultProvider and can fail without weakening its FSM.

## Rejected alternatives

- **Infer debt from events:** events do not own repayment state.
- **Allow arbitrary installment selection:** repayment order and overdue status
  become ambiguous.
- **Re-read rates each payment:** recorded obligations would change over time.
- **Saturate outstanding amounts:** corruption would masquerade as completion.

## Open questions and technical debt

1. Replace saturating outstanding subtraction with checked invariant failures.
2. Decide whether payment before due date is allowed; current tests explicitly
   accept it, but no normative economic decision exists.
3. Add explicit typed position status (`Open`, `Complete`, and any future default or
   liquidation states) or prove cursor-derived status is sufficient at every API.
4. There is no default, acceleration, restructuring, liquidation or bad-debt FSM.
   Define these before credit is treated as production complete.
5. Check refinancing-rate multiplication and all timestamp/month arithmetic for
   overflow before writes.
6. Specify whether fixed 30-day installments or calendar months are intended.
7. Add a generated model proving installment sums, cursor, outstanding values,
   overdue results and account indexes over arbitrary terms/bounds.
8. Add corruption tests for missing installment slots, wrong owners, duplicate
   account entries and impossible cursor/payment combinations.
9. Add pagination and bound account-wide overdue scans; a borrower can otherwise
   make new-credit validation increasingly expensive.
10. Define stable position-id domain separation beyond raw nullifier uniqueness and
    add collision/reference vectors.
11. Prove creation/advancement APIs have no caller except CredisFactory.
12. Define historical retention after completion and whether closed positions may
    ever be pruned without breaking auditability.
