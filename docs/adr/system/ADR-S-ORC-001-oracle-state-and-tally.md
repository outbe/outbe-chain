# ADR-S-ORC-001: Oracle owns price-vote tally, rate history and currency registries

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Oracle and protocol economics maintainers
- **Scope:** `crates/system/oracle`
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-S-VAL-001, ADR-S-SLS-001, ADR-S-ACC-001
- **Related:** ADR-S-CYC-001, ADR-C-MET-001, ADR-C-CRD-002, ADR-C-INX-002, ADR-C-GEM-002, ADR-S-FEE-001, ADR-S-ORC-002
- **Supersedes:** Oracle module portion of the former pre-space Oracle subsystem placeholder

## Context

Oracle is a stateful consensus module, not merely a feeder API. It registers pairs
and currencies, accepts validator/delegated-feeder votes, tallies stake-weighted
prices at block boundaries, stores bounded raw snapshots and finalized daily/WWD
aggregates, computes VWAP/TWAP/S-curve values, tracks participation penalties and
provides refinancing rates to credit modules.

The feeder binary, ZeroFee policy and downstream consumers have separate authority
and ADRs. This record defines the on-chain Oracle state and deterministic lifecycle.

## Decision

### Registry and authority

Pairs are identified by `keccak256("BASE/QUOTE")`, assigned stable one-based ids and
stored bidirectionally with original strings for reversible genesis export. Base and
quote are nonempty and cannot contain `/`. A registered pair may be activated or
deactivated as a vote target only by the system authority.

Settlement currency records bind a nonzero ISO code and denom string/hash to a
registered pair. Reference currencies bind a unique ISO code to an annualized
1e18-scaled refinancing rate. These registries are initialized/migrated by
chain-spec/protocol activation, not mutable feeder input.

Direct `setExchangeRate` is a system-only bootstrap path and records zero
block/timestamp in the current ABI implementation. Normal rates are written only by
the tally lifecycle with canonical block number/time.

Each registered validator may self-feed or delegate one feeder address. Vote
submission resolves caller to exactly one validator, rejects an existing vote for
the period, validates unique active pair tuples and stores rate/volume under the
validator plus dense voter membership.

### Period tally

At each nonzero block divisible by `vote_period`, begin-block tally:

1. reads the current active validator set and stake-derived voting power;
2. constructs ballots for active vote-target pairs;
3. chooses the reference pair with greatest submitted voting power;
4. computes a deterministic weighted median and reward band;
5. derives other pairs through per-voter cross-rates;
6. writes nonzero rates with canonical block/time and one price snapshot;
7. increments exactly one success/abstain/miss result per active validator; and
8. clears the complete period vote state.

The current design intentionally uses stake/active membership at tally time rather
than a period-start snapshot. This remains an explicit policy requiring acceptance.

### Historical products

Price snapshots form a bounded circular history and update per-UTC-day
price-volume/volume aggregates. Read APIs derive VWAP/TWAP over explicit ranges.
WorldwideDay snapshots are written once for a typed UTC+14 day/window. At UTC day
boundaries, lifecycle finalizes closed calendar days in chronological order with a
bounded catch-up cap and monotonic watermark. A finalized empty day is distinguished
from an unfinalized day through that watermark.

Daily S-curve processing runs once per UTC day over active pairs and maintains its
own bounded active history.

### Penalty window

At each slash-window boundary, the receipt-visible system phase evaluates every
validator's success ratio, respects explicitly configured protected validators,
and atomically invokes validator jail/slash authority for underperformance before
resetting counters. Validator-set cardinality is capped for this mandatory phase.

## Authoritative interfaces

| Responsibility | Owner/entrypoint |
|---|---|
| Pair/currency/config initialization | `init_from_genesis` and activated migration |
| Feeder consent | validator-authenticated Oracle ABI |
| Vote submission | Oracle ABI plus ZeroFee admission policy in ADR-S-FEE-001 |
| Tally/snapshot/UTC finalization | `OracleLifecycle::begin_block` |
| Penalty execution | receipt-visible `OracleSlashWindow` system phase |
| Rates/VWAP/TWAP/S-curve/refinancing reads | Oracle ABI and typed read-only API |
| Feeder process/network data | ADR-S-ORC-002, never direct state authority |

## Persistent state and invariants

- Pair id/hash/string maps are bijective for ids `1..=pair_count`; no duplicate
  textual/hash identity exists.
- Settlement/reference ISO indexes are unique and resolve to valid metadata/rates.
- At most one complete vote exists per validator/period; voter list, existence flag,
  tuple count and tuple maps agree bidirectionally.
- Vote tuples contain unique active registered pairs and bounded canonical values.
- A successful tally clears all vote records and advances each active validator's
  one penalty classification exactly once.
- Rate, block and timestamp update atomically and describe the same tally result.
- Snapshot head/tail and every nested entry are dense, bounded and chronological.
- Daily aggregate sums equal accepted snapshot entries within checked arithmetic.
- UTC/WWD snapshot entries refer only to registered pair ids; watermarks and exists
  flags agree with stored values.
- Penalty reset commits with any jail/slash effects; failure leaves counters and
  validator state unchanged.

## Determinism, arithmetic and bounds

All prices, volumes, bands and refinancing rates are 1e18 fixed-point. Median
sorting, tie-breaking, reference-pair selection, cross-rate formula, integer square
root, rounding, snapshot retention, catch-up cap and S-curve math are consensus
algorithms. Iteration is by stable pair id and validator/index order.

Every sum/product/power conversion requires explicit bounds. Overflow cannot be
converted to zero because zero means “no usable price” and changes economic paths.
Mandatory lifecycle work is bounded by registered pairs, validators, snapshots and
backfill constants committed by chain configuration.

## Atomicity, replay and failure

Vote submission is one user transaction. Tally, UTC finalization/S-curve and
slash-window execution occur in block-lifecycle checkpoints defined by ADR-B-EVM-001.
Period vote existence, tally boundary/clearing, day watermarks and S-curve processed
day are durable replay guards.

Invalid feeder, duplicate vote, pair/tuple, absent price or unsupported currency is
a typed revert/no-data result. Broken bidirectional indexes, malformed dense vote or
snapshot entries, arithmetic overflow and impossible watermark/history are
invariant failures. Lifecycle must not emit-and-ignore errors that leave a partially
advanced period.

## Security and trust assumptions

Feeders supply untrusted rate/volume claims; validator authorization and
stake-weighted aggregation create the on-chain result. The feeder's external data
quality is not authenticated by transport alone. ZeroFee grants only fee waiver,
not Oracle authorization.

System caller paths, protected-validator configuration, pair/currency registries and
penalty parameters are governance/activation authority. Downstream consumers must
read canonical execution state, define freshness/finality requirements and snapshot
rates when future obligations depend on them.

## Compatibility and activation

Storage slots, pair hash/string normalization, ids, ISO/denom mapping, scales,
algorithms, config, retention, lifecycle placement and snapshot codecs are
consensus-critical. Genesis export/import must round-trip every field and reject
missing or inconsistent reversible metadata. Changes require activation height,
migration and independent golden vectors.

## Production-interface verification evidence

Inspected full storage layout, genesis import/export, pair/currency registry,
delegation and vote paths, tally/cross-rate/penalty algorithms, lifecycle ordering,
snapshot/daily/WWD/S-curve APIs and downstream typed reads. Existing tests cover
many formulas and basic lifecycle cases, but no generated state-machine model,
maximum-bound arithmetic proof, feeder-to-finality e2e or complete corruption suite
exists. Status remains Proposed.

## Consequences

Oracle becomes one independently auditable state owner. Feeder, fee policy,
ValidatorSet/SlashIndicator and economic consumers reference its explicit seams
instead of being folded into an “Oracle subsystem” ADR.

## Rejected alternatives

- **Trust feeder-supplied final prices:** validator aggregation and accountability
  disappear.
- **Use floating point:** execution would not be platform deterministic.
- **Let consumers inspect raw maps:** pair/date/currency interpretation fragments.
- **Use current price for old obligations:** debt/settlement economics become mutable.
- **Treat arithmetic overflow as zero/no-data:** corruption becomes a valid market
  signal.

## Open questions and technical debt

1. `standard_deviation` returns zero on multiplication/sum overflow, and several
   cross-rate paths use `unwrap_or(U256::ZERO)`. Replace semantic fallback with
   checked bounds/invariant errors.
2. Volume totals and voting-power sums use saturating/unchecked arithmetic. Define
   maximum validators, stake, pairs, rate and volume and enforce them on input.
3. Weighted median threshold uses `total_power / 2` with `>=`; specify exact even/
   tie behavior and add independent vectors.
4. Reference pair is chosen by submitted power at tally time; ties depend on
   iterator/max semantics. Make tie-breaking explicit and test pair permutations.
5. Validator set and stake are sampled at tally time, not period start. Accept this
   economically or persist an exact period committee/power snapshot.
6. `resolve_validator_for_feeder` scans all validators twice and permits one feeder
   to be delegated by multiple validators ambiguously. Add reverse mapping,
   uniqueness policy and bounded lookup.
7. Feeder delegation currently accepts zero/self and replacement without a pending
   consent/rotation delay. Define revocation, compromise and uniqueness semantics.
8. Direct system `setExchangeRate` stores block/time zero and emits while ignoring
   event-emission errors in observed code. Restrict it to genesis or supply canonical
   context; never ignore consensus event failure silently.
9. Several event emissions assign results to `_`. Decide whether events are
   consensus receipt obligations and propagate failures consistently.
10. Vote rate/volume zero and maximum bounds need explicit validation; malformed but
    authorized feeds can influence abstain/winner behavior.
11. Vote tuple and voter cleanup must prove all nested slots are cleared; add
    structural closure and replay tests after restart/export/import.
12. Snapshot circular-buffer retention versus maximum VWAP lookback and root/day
    backfill needs a formal capacity equation.
13. UTC backfill skips older days beyond its cap and advances watermark to the most
    recent closed day. Specify skipped-day semantics permanently and expose them to
    consumers distinctly from finalized-empty.
14. UTC date and WorldwideDay use different calendars but similar integer keys.
    Continue replacing raw integers with distinct types at every API/storage seam.
15. Daily price-volume accumulation requires checked arithmetic and proof that
    snapshot eviction cannot destroy data before daily finalization.
16. S-curve economics, peak detection, retention and bounds need their own reviewed
    mathematical specification and reference vectors.
17. Protected validators and slash thresholds are genesis/system configuration with
    no documented governance/timelock/activation workflow.
18. Oracle underperformance currently crosses directly into jail/slash. Prove
    offense idempotency, evidence attribution, event ordering and rollback with
    ADR-S-VAL-001 and ADR-S-SLS-001.
19. Penalty `success + abstain + miss` uses unchecked `u64` addition and reset window
    semantics need generated long-run tests.
20. Refinancing rates are described as genesis-pinned; define activated update and
    historical snapshot policy before rates can change.
21. Pair deactivation zeroes rates in a separate cleanup method. Define atomic
    activation/deactivation and what existing obligations/readers observe.
22. Full state export requires an externally supplied validator list to discover
    delegation/counters/protection. Add enumerable Oracle-owned indexes or prove the
    supplied list is complete and canonical.
23. Add production e2e from feeder submission through zero-fee admission, tally,
    finality, daily VWAP, consumer read and penalty/restart behavior.
24. Add a generated reference model covering votes, committee changes, periods,
    snapshots, UTC gaps, S-curve, export/import and every arithmetic boundary.
