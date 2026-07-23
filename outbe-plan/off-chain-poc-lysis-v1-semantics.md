# Off-chain PoC: frozen Lysis V1 semantic baseline

Status: **resolved research asset for decision ticket #3**

Scope: intended PoC semantic compatibility with the current synchronous
Tribute -> Metadosis -> Lysis -> Nod path

Date: 2026-07-23

This asset freezes what `LysisProgramV1` must compute and how an independent
reference corpus must prove equivalence. It does not freeze consensus codecs,
hash domains, protocol IDs, generated caps or crate placement; decision ticket
#4 owns those bytes.

## 1. Decision

PoC Lysis V1 preserves the current successful economic transformation and the
current deterministic first-failure behavior, under the fork-pinned bounded
profile. It does not preserve the current physical execution topology.

The semantic program:

1. receives one complete authenticated, request-pinned input;
2. reconstructs Fidelity and Oracle observations at the frozen logical context;
3. executes the integer Lysis algorithm in raw Tribute-ID order;
4. derives Nod, contributor, Tribute, unused-Lysis carry-over and Metadosis
   completion actions without writing chain state;
5. returns a typed success result or one deterministic typed failure;
6. produces the same result for one, two or four workers and any completion or
   retry order.

The accepted corpus plus this freeze becomes the Lysis V1 semantic authority
bound by `lysis_program_semantics_hash`. The current Rust runtime is the
migration baseline, not the only oracle after the fork.

No product choice or grilling is required for this ticket: the current code,
PoC specification and ADRs determine the result.

## 2. Authority and conflict resolution

| Evidence | Decision for Lysis V1 |
|---|---|
| [`off-chain-poc.md` section 8.2](../off-chain-poc.md#82-required-lysis-equivalence) | Normative target: raw-ID order, two logical Fidelity observations, conditional Oracle reads, exact integer operations/division points, first-error ordinal, contributor order, one Nod per Tribute and conservation |
| [`ADR-C-LYS-001`](../docs/adr/core/ADR-C-LYS-001-lysis-tribute-to-nod-transformation.md) | Normative domain invariants: sealed complete input, positive one-to-one Nod output, typed result, no writes during pure execution and all-or-nothing apply |
| [`runtime.rs`](../crates/core/lysis/src/runtime.rs) and [`algorithm.rs`](../crates/core/lysis/src/algorithm.rs) | Authoritative migration behavior where it does not conflict with an explicit target correction |
| Metadosis [`calculate_metadosis` and `process_metadosis`](../crates/core/metadosis/src/runtime.rs) | Authoritative arithmetic and successful effect/event values around Lysis |
| Existing tests | Regression evidence only; a test comment or unused constant cannot override the executing path |
| Historical Cosmos comment | Non-authoritative provenance. The referenced source is not present in this repository and is not required to define PoC bytes |

Explicit conflict resolutions:

1. `F_FP_DEFAULT` and `F_MAX_FP` in
   [`constants.rs`](../crates/core/lysis/src/constants.rs) are used by tests, not
   by the production Lysis path. Their comments and the old “8%/16%” wording do
   not define Lysis V1.
2. Production `compute_fi_fraction_map` derives
   `f = gratis_allocation * 10^18 / total_nominal` and `fmax = 2 * f`.
   Regression cases at 5%, 30% and 32% confirm that there is no 8%/16% clamp.
3. Small-budget rounding is not “fixed” by skipping a Tribute or redistributing
   dust. The first zero or over-budget `gratis_load` invalidates the whole local
   execution and produces no signature. A successful result returns the exact
   remaining dust.
4. A valid WWD has one Tribute identity per `(owner, WWD)`. Duplicate owner/day
   input is rejected before semantic execution. Contributor “aggregation” in a
   valid Lysis V1 job is therefore one eligible nominal per unique owner.
5. The current attempted `FAILED` write/event on Lysis error is reverted by the
   outer transaction. It is not a durable economic outcome and is not part of
   Lysis V1. Local failure means “do not attest”; consensus expiry remains live.
6. The fork intentionally moves the snapshot to the terminal request phase.
   Exact legacy state/event comparison applies when no relevant same-block
   Fidelity/Oracle mutation separates old and new observation points. The new
   terminal-snapshot ordering is tested separately.
7. Current comments saying Lysis physically deletes each Tribute are stale.
   Semantic success consumes exact totals and requests logical generation
   retirement; synchronous physical enumeration/deletion is not preserved.

## 3. Frozen logical input

The logical input is one typed job-bound bundle. Exact field encoding is ticket
#4, but its semantic content is fixed here:

- `JobId`, attempt, bundle identity and WWD;
- raw Tribute records and authenticated raw 36-byte IDs;
- sealed Tribute count and nominal total;
- frozen Metadosis day type, `day_limit`, `lysis_budget`, `auction_base`,
  demand inputs and current VWAP;
- request `logical_evaluation_height` and `logical_evaluation_time`;
- complete authenticated Fidelity state required to calculate each owner’s
  league at the logical time;
- complete authenticated Oracle pair/VWAP/S-curve state required by the legacy
  observation sequence;
- exact Tribute binding and absence/pre-state facts needed to derive unique Nod
  and contributor targets.

Preconditions:

```text
0 < T <= active PoC Tribute cap
U = T
sealed_count = T
sealed_nominal = exact checked sum(input nominal)
all raw EntityId36 values are unique and strictly ascending after canonical sort
all bodies, IDs, owners and WWDs agree
day type is GREEN or RED
day_limit = lysis_budget + auction_base
all activation preconditions refer to the same intent/attempt
```

Mongo values, a precomputed league, a precomputed price or a worker-supplied
total is never accepted without its authenticated request-state opening.

## 4. Frozen observation and execution order

The observable semantic order is:

1. Canonically sort complete input by raw 36-byte Tribute ID.
2. Traverse all Tributes in raw order:
   - perform logical Fidelity observation `F1`;
   - append the league;
   - checked-add nominal to `total_interest`.
3. Reject zero or mismatching total.
4. Group by ascending `u16` league, derive group shares/fractions and normalize
   the fixed-point distribution.
5. Resolve the mandatory ISO 840 Oracle nominal once, before the per-Tribute
   output loop. This happens even if no Tribute references ISO 840.
6. Traverse Tributes again in raw order. For each ordinal:
   - obtain its group fraction;
   - calculate `gratis_load`;
   - fail at this ordinal if it is zero or exceeds remaining Gratis;
   - subtract it from remaining;
   - reuse the mandatory ISO 840 price or perform a logical Oracle observation
     for this non-840 reference currency;
   - calculate floor price;
   - perform logical Fidelity observation `F2`;
   - calculate cost;
   - derive the unique Nod identity and bucket;
   - fail at this ordinal if the pinned Nod target precondition is
     invalid/conflicting;
   - append exactly one Nod action;
   - append an owner contribution only when
     `exclude_from_intex_issuance == false`.
7. Require emitted Nod count to equal Tribute count.
8. Sort eligible contributor entries by raw 20-byte owner address.
9. Derive the exact Tribute consume/retirement action.
10. Derive the unused-Lysis carry-over and Metadosis completion actions and
    their semantic event summary. Desis is already committed by the request
    phase and is not a Lysis result action.

Physical reads may be deduplicated, and one authenticated Fidelity/Oracle
opening may serve repeated logical observations. Deduplication cannot alter the
logical transcript, value, error precedence or first-error ordinal. `F1` and
`F2` for one owner are evaluated from the same request snapshot and must agree.

Scheduler order, worker identity, page arrival order, hash-map iteration and
artifact completion order are not semantic inputs.

## 5. Frozen arithmetic

All unsigned values are `U256`; fixed-point scale `S = 10^18`. Every division
rounds down. Signed `I256` division in the distribution calculation truncates
toward zero.

### 5.1 Metadosis allocation

```text
green_demand = wrap_u256(total_nominal * 32) / 100

GREEN:
  demand = green_demand
  supply = metadosis_limit

RED:
  demand = green_demand / 8
  supply = metadosis_limit / 8

lysis_budget = min(demand, supply)
auction_base = day_limit - lysis_budget
```

The legacy implementation names `lysis_budget` as `gratis_allocation` and
`auction_base` as `metadosis_limit_remainder`. The PoC wire and planning
documents use the budget names because the split is committed before Lysis.

The active bounded profile must make successful economic totals exact and
conserved. Adversarial overflow vectors still exercise the current
wrap/saturate/error behavior in the pure corpus; they are not evidence that an
unsafe value is admissible as a live job.

### 5.2 League groups and deficit

For league `g` in ascending numeric order:

```text
group_nominal[g] = sum(nominal_i where F1_i = g)
y[g] = wrap_u256(group_nominal[g] * S) / total_nominal
p[g] = number of Tributes in g
```

The truncation delta `S - sum(y)` is added to the last entry, which is the
highest league. Then:

```text
f = wrap_u256(lysis_budget * S) / total_nominal
fmax = wrap_u256(f * 2)
```

One league returns exactly `f`.

For multiple leagues, Lysis V1 preserves the exact integer implementation in
[`calc_fraction_distribution_fp`](../crates/core/lysis/src/algorithm.rs):

- boundary weights use floor fixed-point fifth and tenth roots;
- endpoint weights are `0.2` and `0.8` of the middle-weight sum;
- mass and cumulative moments divide at their current code points;
- variance uses saturating subtraction;
- negative fractions clamp to zero;
- if weighted expenditure exceeds `f`, every fraction is scaled down
  proportionally with floor division;
- no later rounding-up redistributes unused Gratis.

The reference implementation must independently reproduce the operation order,
including `fp_root`’s floor root, wide intermediate and checked narrowing. A
mathematically similar real-number implementation is not an acceptable oracle.

### 5.3 Per-Tribute values

For Tribute `i` in raw-ID order:

```text
gratis_load_i = wrap_u256(nominal_i * fraction[F1_i]) / S

require gratis_load_i > 0
require gratis_load_i <= remaining_before_i
remaining_after_i = remaining_before_i - gratis_load_i

entry_price_i =
  max(worldwide_day_vwap(pair(reference_currency_i)) or 0,
      max_active_scurve(pair(reference_currency_i), WWD_UTC_timestamp))

require entry_price_i > 0

floor_price_i =
  wrap_u256(max(tribute_price_i, entry_price_i) * 108) / 100

require F2_i = F1_i

cost_amount_i = wrap_u256(entry_price_i * gratis_load_i) / S
nod_id_i = PoseidonEntityId(owner_i, WWD)
bucket_key_i = keccak256(WWD_BE4 || floor_price_i_BE32)
issued_at_i = request logical_evaluation_time
```

Oracle state is selected at the request snapshot. The S-curve evaluation
timestamp remains the current legacy `WorldwideDay::to_timestamp_utc()` value;
it is not replaced with activation wall time.

The mandatory ISO 840 observation and each non-840 per-Tribute observation
retain current error precedence. A missing pair, absent/zero VWAP plus zero
S-curve, malformed opening or arithmetic failure produces no partial result.

### 5.4 Operation classes

The corpus records the result of every consensus-relevant site, rather than
assuming all arithmetic has one overflow policy:

- checked: complete nominal sum, per-owner contributor accumulation, count
  conversions, remaining-budget underflow and owner-owned supply subtraction;
- saturating: current timestamp differences, moment variance, selected
  event/accounting differences and the current S-curve overflow-to-zero path;
- wrapping: current ordinary `U256` multiplication/addition sites and their
  exact division points;
- wide checked: fixed-point root intermediates;
- signed checked/narrowed: `U256`/`I256` boundaries, with negative output
  clamped to zero.

Ticket #4 must turn these sites into named golden-vector operations before
implementation starts. Replacing a wrapping operation with a checked operation,
even if safer in isolation, changes Lysis V1 unless pre-admission makes the
branch unreachable and the adversarial corpus retains the specified rejection.

## 6. Fidelity and Oracle semantics

### 6.1 Fidelity

The worker does not trust a supplied `league`. From authenticated request-state
openings it reproduces [`FidelityContract::league_at`](../crates/core/fidelity/src/runtime.rs):

- active cohorts contribute to numerator and denominator;
- sold cohorts contribute their decayed held duration to the denominator;
- time differences saturate;
- efficiency and RCFI use the current `10^18` fixed-point division points;
- no qualified account or zero global maximum yields league `1`;
- otherwise `slot = min(rcfi * 4096 / max_rcfi, 4095)` and
  `league = 1 + slot`.

The PoC profile bounds cohorts per owner. Corpus cases at `U256::MAX` still
exercise the legacy wrapping/saturating behavior and must agree between native
and independent implementations.

### 6.2 Oracle

For every logical price observation:

1. map ISO code through `settlement_iso_to_pair` and `pair_hash_to_id`;
2. reject an unregistered/zero pair ID;
3. read the WWD VWAP if present, otherwise use zero;
4. evaluate every applicable S-curve record for that pair at WWD UTC day;
5. use `max(VWAP, max_scurve)`;
6. reject zero.

The opening may be physically shared for repeated currencies, but the logical
observation order remains mandatory ISO 840 followed by non-840 observations in
raw Tribute order.

## 7. Frozen actions, effects and events

### 7.1 Per-Tribute Nod action

Exactly one action contains:

- owner, WWD and derived Nod ID;
- `F2` league;
- `gratis_load_minor`;
- entry and floor prices;
- cost amount;
- issuance and reference currencies;
- derived bucket key;
- request logical `issued_at`.

Nod actions are ordered by source raw Tribute ID. Bucket grouping is a derived
stable shuffle and cannot reorder the semantic Nod stream.

### 7.2 Contributor action

For each non-excluded Tribute, emit `(owner, nominal_amount_minor)`. Valid input
has unique owners, so the canonical list is the eligible entries sorted by raw
address. The contributor count and total exclude opted-out Tributes; those
Tributes still participate in all allocation totals and receive a Nod.

### 7.3 Budget split and carry-over

```text
day_limit = lysis_budget + auction_base

unused_lysis =
  lysis_budget - sum(gratis_load_i in raw order)
```

The request phase applies the part known before Lysis:

- GREEN: dispatch one Desis brief with supply `auction_base`;
- RED: dispatch no brief and credit `auction_base` to carry-over;
- entry price is the last closed UTC-day `COEN/0xUSD` VWAP selected at request
  logical time, falling back to the frozen Metadosis `current_vwap`;
- anchor/time is request logical time.

The request effect and split receipt commit atomically with `JobIntentV1`.
Retries reuse them and never dispatch or credit `auction_base` again.

Activation never changes a live auction. It credits only:

```text
activation carry-over credit = unused_lysis
```

`PromisLimit.total_unallocated` is consumed atomically when forming the next
not-yet-formed day limit. If a day limit already exists, the credit waits for
the following unformed day.

A terminal no-retry outcome credits the whole `lysis_budget` exactly once.
Normal PoC expiry preserves the budget for retry.

### 7.4 Completion and semantic events

Successful actions also commit:

- exact Tribute count/nominal consume and logical partition retirement;
- Metadosis `COMPLETED`;
- removal of expiry;
- `MetadosisExecuted` economic fields:
  total nominal, demand, supply, `lysis_budget`, `auction_base`,
  `unused_lysis` and consumed Lysis budget;
- owner events equivalent to Nod issuance, contributor state, Tribute
  retirement and carry-over credit;
- the new aggregate `LysisActivated` identity/commitments.

Economic/event time fields use request logical time. The activation receipt
location and `activated_at` use the actual activation block and are explicitly
not part of semantic equivalence.

## 8. Frozen failure semantics

Input/authentication failures occur before arithmetic. Once semantic execution
starts, error precedence follows section 4.

Required typed failure classes:

- incomplete, duplicate or non-canonical raw input;
- sealed count or nominal mismatch;
- total nominal overflow or zero total;
- malformed/missing Fidelity opening or Fidelity arithmetic error;
- malformed/missing mandatory ISO 840 Oracle opening;
- malformed/missing per-Tribute non-840 Oracle opening;
- zero nominal price;
- arithmetic conversion/root failure;
- first zero `gratis_load`;
- first `gratis_load` greater than remaining;
- duplicate/conflicting Nod target or failed live precondition;
- contributor checked-sum overflow;
- output count, root, total or conservation mismatch.

Every ordinal-bearing error contains the lowest raw Tribute ordinal that the
sequential baseline would fail on. Parallel prefix/reduction may discover
several errors, but must return the same lowest semantic error. No failure emits
a partial result, signature or chain effect.

## 9. Independent reference and corpus

### 9.1 Technology and independence

Use a separate test-only Rust reference crate:

```text
crates/testing/lysis-v1-reference/
  Cargo.toml
  src/lib.rs
  src/main.rs
crates/core/lysis/vectors/lysis-v1/
  manifest.json
  cases.jsonl
```

Why a separate Rust crate:

- one Rust toolchain covers local development and CI;
- `num-bigint` keeps explicit `mod 2^256`, signed bounds and division behavior
  auditable without using production `U256` code;
- a separate crate/process remains independent of the production Rust
  implementation by dependency and source boundaries;
- no new production crate or runtime dependency is introduced.

Independence rules:

- no import, FFI or subprocess call into `outbe-lysis`;
- no node, RPC, Mongo, CAS or on-chain Lysis call;
- no copying Rust-produced expected outputs into the corpus without independent
  reference reproduction and review;
- explicit helpers for wrapping, checked and saturating `U256`, `I256`
  conversion and truncating signed division;
- independent fixed-point root/distribution transcription;
- identity/hash primitives validated against separately sourced canonical
  vectors, including the existing Noble Poseidon vectors, then frozen by
  ticket #4;
- strict duplicate-rejecting JSON input, canonical JSON output and fixed seeds
  only.

The reference is test-only and must never be linked into the node, worker,
activation verifier or consensus build.

### 9.2 Case format

`manifest.json` records:

- corpus schema and semantic version;
- source revision and reviewed decision reference;
- reference implementation digest;
- case count and case-file digest;
- required arithmetic and identity vector set;
- later, the ticket-#4 protocol bundle/hash and canonical codec vector digest.

Each JSONL case records:

- stable case ID and requirement tags;
- `SUCCESS` or exact typed `FAILURE`;
- decimal-string `U256` values and fixed-width lowercase hex identities;
- logical time/day, Metadosis inputs, raw Tribute records, Fidelity state,
  Oracle state, budget split and activation preconditions;
- expected ordered observations;
- expected group table/fractions;
- expected request split/Desis-or-carry-over receipt,
  Nod/contributor/Tribute/carry-over/Metadosis actions and conservation totals,
  or error class plus first ordinal;
- later, canonical input/result/action bytes and hashes from ticket #4.

JSON is a review and test-vector envelope, not the consensus codec.

### 9.3 Minimum corpus classes

| Class | Mandatory cases |
|---|---|
| Basic allocation | single Tribute at 5%, 30% and 32%; GREEN and RED; exact remaining dust |
| Multiple leagues | uneven two/three/fifteen-league populations; highest-league share-dust absorption; zero variance; negative-beta clamp; weighted normalization |
| Ordering | unsorted/page-split source normalized to raw ID; 31/32/33 records; stable contributor address order; identical expected bytes for input/page/worker permutations |
| Fidelity | no qualified account, minimum and maximum league, active/sold cohorts, 64-cohort boundary, time saturation and two equal logical observations |
| Oracle | mandatory 840; VWAP wins; S-curve wins; equality; non-840 conditional read; repeated currencies; missing pair; absent/zero values |
| Nod/effects | entry versus Tribute floor source, exact 108% floor, cost scale, identity and bucket vectors, request split/Desis-or-carry-over plus activation carry-over, logical issued/anchor time |
| Contributors | all eligible, mixed eligible/excluded, all excluded and `T=1` excluded |
| First failure | zero/over-budget load and missing conditional Oracle at first/middle/last raw ordinal; Nod target collision |
| Integrity | body/ID/WWD mismatch, count mismatch, nominal mismatch, duplicate owner/day and output conservation mutation |
| Width/overflow | `0`, `1`, `S-1`, `S`, `S+1`, `2^255-1`, `2^255`, `U256::MAX-1`, `U256::MAX` at every wrapping/checked/saturating boundary |
| Maximum PoC shape | generated active cap with worst allowed leagues, currencies, owners, buckets and encoded sizes; final exact cap belongs to ticket #4 |

The maximum-shape case does not imply billion-record support.

### 9.4 Freeze gate

The implementation plan must put this gate before planner, worker, reducer or
activation work:

1. implement and review the independent Rust reference from this document;
2. generate the minimized corpus independently;
3. compare the current Rust migration baseline with every compatible case;
4. classify every mismatch as a Rust defect, stale test/comment, intentional
   fork ordering change or rejected corpus case;
5. freeze canonical bytes/hashes after ticket #4;
6. make reference `--mode check` verification and native corpus replay
   mandatory in the fast PR lane;
7. make randomized fixed-seed differential/property replay mandatory in the
   integration lane;
8. reject corpus drift unless the semantic version and protocol bundle change.

No test may regenerate expected output from the native implementation and then
compare the implementation with itself.

## 10. Existing evidence and missing evidence

Verified current commands:

```text
cargo test -p outbe-lysis
  34 passed; 0 failed; 0 ignored

cargo test -p outbe-metadosis
  37 passed; 0 failed; 0 ignored
```

Existing tests cover examples for fixed-point distribution, 5/30/32% deficit,
cost scale, sparse Gratis, contributor exclusion/order, rollback, dense 512-item
execution and Metadosis branches. They do not provide:

- an independent implementation;
- a reviewed corpus;
- exhaustive operation-boundary vectors;
- storage-independent native execution;
- planner/reducer equality;
- canonical action/result bytes;
- first-error equivalence under parallel completion;
- a production OCOMP caller or public activation path.

Therefore passing current tests proves the migration baseline is internally
green; it does not prove PoC semantic completion.

## 11. Decision #3 result

Ticket #3 is resolved:

- the exact successful computation and first-failure order are frozen;
- stale 8%/16% clamp language is excluded;
- zero allocation remains a whole-job local failure, not a skip;
- valid input has unique owner/day identities;
- two Fidelity and conditional Oracle observations are logical request-pinned
  observations, not live storage calls;
- request logical time replaces activation time for economics while WWD UTC
  remains the S-curve evaluation date;
- a separate Rust crate with arbitrary-precision arithmetic and no production
  dependencies is selected for the independent reference;
- corpus structure, independence rules, minimum cases and freeze gate are
  defined.

The next frontier is ticket #4: freeze protocol/fork identity, exact canonical
bytes, hash/signature domains, object schemas, deadlines and generated PoC caps.
