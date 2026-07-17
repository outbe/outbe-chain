# ADR-S-RWD-001: Rewards owns finalized participation, fee escrow and validator top-ups

- **Status:** Proposed; current implementation profiled; not an architecture-conformance verdict
- **Date:** 2026-07-17
- **Owners/scope:** `crates/system/rewards`; certified-parent economic identity,
  participation, per-block fee escrow/settlement and daily validator Gem top-ups
- **Depends on:** ADR-B-GEN-001, ADR-B-CNS-002, ADR-B-CNS-003, ADR-B-EVM-004, ADR-S-CYC-001, ADR-C-AGR-001, ADR-S-VAL-001,
  ADR-S-EMI-001, ADR-S-ACC-001
- **Related:** ADR-C-GEM-001 and ADR-C-GEM-002 Gem issuance, ADR-S-SLS-001 SlashIndicator
- **Supersedes:** The Rewards-local portions of the deleted pre-space validator aggregate

## Context

Validator compensation has two clocks and two assets. Native transaction fees from
a finalized block are escrowed and distributed after the `K`-block late-inclusion
window. Daily emission top-ups are allocated from finalized participation and minted
as Gems during Cycle's UTC-day dispatch. Combining either with ValidatorSet's
membership FSM would hide their replay keys, conservation equations and finality
requirements.

## Decision

Rewards is the sole owner of the canonical economic fingerprint for finalized-parent
metadata, daily participation aggregates, per-finalized-block native fee escrow,
late-credit window state and per-day validator-top-up completion. It exposes no
external callable ABI; all mutations are system commands invoked through the
certified-parent, late-finalize and Cycle lifecycle seams.

The V3 fingerprint binds finalized hash/number, epoch/view, ordered committee,
canonical signer set, committee hash, VRF version/key/proof identity, proof kind,
missed proposers and validator fee sum. Identical `(fb_hash, fingerprint)` is a full
replay no-op; the same hash with different fingerprint is protocol corruption.

## Authoritative inputs and commands

After proof verification and exact-parent gating, the certified-parent accounting
command records a fresh fingerprint, raw daily fee total, canonical escrow binding,
base signers at inclusion distance zero and per-day participation. Late-finalize
commands may improve a credited validator to the smallest observed `k` only when
their proof matches that escrowed binding.

At block `N + K`, window close first exposes the full credited set for slashing,
then settles block `N` exactly once. At day close, Cycle obtains the canonical daily
fee/participation data, computes the allocation through EmissionLimit/AgentReward,
asks Rewards to mint the validator top-up, and finally marks the whole day settled.

These system commands need unforgeable phase capabilities. A raw schema/context or
public Rust function is not authority to escrow fees, add credit, choose top-up
recipients, settle a window or mark a day complete.

## Persistent state and invariants

The live model contains:

- immutable genesis UTC-day anchor and monotonic finalized-day progress;
- per-`fb_hash` fingerprint/first-seen/settled guards, bounded by a ring;
- per-day raw fees, voter/count indexes and total participation;
- per-block pending fee and canonical number/epoch/view/committee binding;
- per-block enumerable credited voters with smallest `k + 1`; and
- distinct `daily_topup_settled` and whole-dispatch `daily_settled` guards.

Required equivalences include:

```text
fingerprint[hash] != 0 <=> one canonical economic intent is bound to hash
pending_fb_hash_at[number] = hash => reverse escrow binding is unique
late_voter_k_plus1[hash][v] != 0 <=> v appears exactly once in late_voter_at
daily_participation[day][v] > 0 <=> v appears exactly once in daily_voter_at
daily_total_participation = sum(per-voter daily participation)
fee_settled[hash] => pending fee and enumerable window state are cleared
daily_settled[day] => the complete imported Cycle dispatch committed
```

Guard pruning is safe only while its retention exceeds every accepted replay,
late-finalization and restart horizon. The 64-entry ring cursor advances once per
fresh finalized hash, never per replay.

## Per-block fee settlement

For block `N`, the escrowed native amount is distributed at `N + K` using:

```text
payout(v) = pending_fee * weight(min_k(v)) /
            (committee_size * maximum_weight)
residue   = pending_fee - sum(payout(v))
```

The fixed denominator prevents exclusion from enriching remaining voters. Current
weights are `[100, 100, 100, 0]` for `K = 3`; a first credit at the settle slot earns
zero. Transfers pay voters from `REWARDS_ADDRESS`. Residue is burned from that
balance and atomically dispatched as Metadosis terminal emission headroom. Then the
settled tombstone is written and hash/number-indexed window state is cleared.

The conservation contract is exact:

```text
sum(native payouts) + burned residue = escrowed pending fee
```

## Daily validator top-up

Daily raw fees contribute to the emission-cap calculation but are not paid again.
The computed top-up is divided in proportion to finalized participation counts and
minted through GemFactory: Genesis Gems for day numbers 0–20, Validator Gems
thereafter. Integer-division remainder is returned implicitly as
`topup_total - distributed`; its downstream owner must be explicit in the imported
Cycle/EmissionLimit contract.

`daily_topup_settled` guards this one sub-effect; `daily_settled` guards the complete
cross-module day dispatch. Neither marker may be written before all effects it
claims are committed.

## Atomicity, ordering and failure

Fingerprint, participation, escrow and associated indexes execute in the Phase-1
system transaction after certificate verification. Late-credit/slashing inspection
must happen before settlement clears the voter set. All voter transfers, residue
burn, terminal dispatch, tombstone and cleanup must share one EVM checkpoint; an
error restores the full economic pre-state.

Daily top-up Gem mints and its guard must share the Cycle dispatch checkpoint. A
later failure must not retain Gems while rolling back the day marker, or vice versa.
Contradictory metadata, impossible indexes and conservation failure are fatal
protocol outcomes, not retryable user reverts. A missing dependency or temporary
execution failure may retry only from semantic pre-state.

## Determinism and bounds

Committee order and signer bitmap are proof-bound. Daily voters use deterministic
first-seen finalized order; late voters use deterministic first-credit order, while
payout is order-independent except for failure position. Committee size bounds the
late list, but daily voter iteration and retained per-day state require explicit
limits/cleanup policy.

All arithmetic affecting balances/counts uses checked operations except identified
debts. Fixed-denominator multiplication currently saturates; impossible overflow
must be rejected/fatal rather than silently changing economics. UTC-day derivation
uses consensus block timestamps and the locked genesis day.

## Replay, retry and pruning

Fingerprint equality is the primary same-key/same-intent guard. Per-block and
per-voter guards protect subordinate effects. Window settlement is a no-op after its
tombstone and clears the number lookup so `settle_matured` replay returns zero.
Daily top-up/day markers similarly make completed dispatch replay a no-op.

Pruning the fingerprint or settled tombstone deliberately makes an old key appear
fresh. Therefore the protocol must prove such input can never be accepted again;
“older than K” is insufficient unless every replay entrypoint enforces the same
canonical height horizon.

## Security and compatibility

Rewards trusts only executor-verified certified-parent artifacts and canonical
committee snapshots. It must not trust proposer-supplied fee, committee, view or
credit fields without the fingerprint/binding checks. `REWARDS_ADDRESS` solvency is
a consensus invariant, not an operator-funded best effort.

Fingerprint domain/encoding, `K`, decay weights, denominator, guard retention,
genesis-day handling, Gem type cutoff and rounding are hard-fork economics. Storage
layout changes require migration because several legacy slots remain allocated.

## Production-interface and architectural evidence

Inspected evidence includes `schema.rs`, `runtime.rs`,
`finalized_metadata_hook.rs`, `late_settlement.rs`, `api.rs`, `lifecycle.rs`, the
reject-all precompile, executor/Cycle call ordering, economics tests, fingerprint
tests and replay property tests. Existing evidence is strong on isolated replay and
fee parity but does not close the effective public Rust mutation surface or prove
the whole production system-transaction path under injected failures.

architectural closure requires one phase-authenticated command interface, intent-bound
typed receipts, internal canonical recipient reads, fail-closed state decoding,
module-owned checkpoints for settlement, and stateful reference-model testing of
multi-block/multi-day histories through production lifecycle dispatch.

## Consequences and rejected alternatives

Delayed fixed-denominator fee settlement removes the incentive to censor a late
voter. Eager signer-only fee payment was rejected because it cannot credit valid
late inclusion. Redistributing absentee shares was rejected because it rewards
exclusion. A claimable native reward balance was rejected; fees transfer at window
close and emission compensation is represented as Gems.

## Open questions and technical debt

- Close public mutation bypasses: raw schema construction and public functions can
  escrow/overwrite bindings, add arbitrary late credit, settle early, choose top-up
  recipients or mark a day settled without an authenticated lifecycle phase.
- `add_topup_for_voters` trusts a caller-supplied `(Address, count)` list and does
  not bind it to stored day participation. Move canonical selection and allocation
  inside Rewards or require an unforgeable intent receipt from the owner.
- `escrow_block_fee` is described as idempotent but, before settlement, overwrites
  fee and number/epoch/view/committee bindings. Enforce same-key/same-intent and
  reject same number/different hash or same hash/different metadata locally.
- `ensure_genesis_anchor` claims initialization must occur at block 0 but does not
  check the block number; a missing lifecycle can initialize from a later day.
  Prefer genesis allocation or a fatal exact-block guard.
- Several documented schema facts are stale: `daily_fees_paid`, `daily_fee_dust`
  and `fee_dust_counted_for_block` are not updated by the inspected production
  path, yet comments assert an equality involving them. Migrate/remove the legacy
  fields and rewrite the invariant around per-block escrow/residue.
- `last_settled_utc_day` is initialized but inspected production ownership appears
  split/stale after Cycle refactoring. Define its sole writer/consumer or remove it.
- `daily_settled` comments still claim late finalized metadata is rejected, while
  the hook explicitly accepts sync-phase ordering without that guard. Resolve the
  normative late-after-settle policy.
- `genesis_utc_day` and contradictory/corrupt metadata paths return `Revert` despite
  comments calling them fatal. Introduce typed fatal invariant errors and verify
  proposer/validator parity.
- Validate that credited addresses are unique members of the bound committee and
  `late_voter_count <= committee_size`; do not rely solely on upstream verification.
- Replace saturating denominator multiplication and unchecked `k + 1`/participation
  sums with explicit overflow/corruption handling. `total_count` in top-up currently
  uses ordinary iterator `sum`.
- Specify and account for daily top-up rounding dust; the returned distributed
  amount alone does not assign the remainder to a durable owner.
- Prove REWARDS native balance equals all unsettled escrow liabilities and cannot
  be underfunded by fee-routing/order drift.
- Prove the 64-block tombstone/fingerprint retention against every accepted replay,
  restart and late-finalization path; add boundary tests at 63/64/65 and multiple
  blocks sharing days.
- Bound daily voter/day retention and define cleanup after settlement. Current
  per-day aggregates and voter indexes grow without an inspected pruning path.
- Add failure injection after every voter transfer, residue burn, terminal dispatch,
  tombstone and cleanup write, comparing full semantic pre-state after rollback.
- Add production-path tests for same hash/different intent, same number/different
  hash, credit after settle, terminal replay, arbitrary recipient injection and
  insufficient REWARDS balance.
- Add an independent stateful multi-block/multi-day reference model with generated
  replay, contradictory proof, late-credit, settlement, day rollover and pruning
  histories plus distribution labels and retained seeds.
