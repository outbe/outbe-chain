# Off-chain PoC: consensus lifecycle, activation and atomic apply

Status: **resolved decision asset for implementation-planning ticket #8**

Scope: the PoC fork path from terminal Metadosis request creation through
expiry, public `activateLysis`, certified owner effects, terminal state and
public outcome reads.

Normative inputs:

- [`ADR-S-OCM-001`](../docs/adr/system/ADR-S-OCM-001-ocomp-kernel-and-typed-program-boundary.md);
- [`ADR-S-OCM-004`](../docs/adr/system/ADR-S-OCM-004-certified-activation-job-fsm-and-protocol-versioning.md);
- [`PFS-002`](../docs/flows/002-off-chain-poc-protocol-flow.md);
- [`off-chain-poc.md`](../off-chain-poc.md);
- the [protocol freeze](off-chain-poc-protocol-freeze.md);
- the [current-code map](off-chain-poc-current-code-map.md);
- the [Lysis V1 semantic baseline](off-chain-poc-lysis-v1-semantics.md).

This asset selects implementation seams; it does not implement production code,
add another program, or create a generic write/dispatch framework.

## 1. Decision in one flow

```text
begin-zone OcompLifecycleBegin
  no-op mode barrier -> expire due jobs -> preserve budget -> READY+1

ordinary public transactions
  including paid activateLysis(bytes) through RPC/txpool/P2P/import/replay

compressed-entity end_block
  seal the complete user-transaction CE overlay and write the final CE root

end-zone OcompTerminalRequest
  inspect one due READY WWD
  -> direct legacy empty/ineligible branch, or
  -> split day_limit
  -> GREEN: dispatch auction_base to Desis
     RED: credit auction_base to carry-over
  -> store JobIntent(lysis_budget)/OFFCHAIN_PENDING atomically

block commit/finality
  -> independent off-chain execution and q=3 certificate

later public activateLysis transaction
  cap/decode/finality/job/certificate/result checks
  -> exact retry: return stored receipt, no effects
  -> certified precondition conflict: CONFLICTED + preserve budget + READY+1
  -> live exact targets: private capability + four owner calls
  -> verify four activation receipts + request split receipt
  -> active generation + terminal receipt + COMPLETED
  -> one outer checkpoint commits everything or nothing
```

There is no consensus `RUNNING`, intermediate result state, synchronous Lysis
fallback, public result-accept transaction or direct executor injection.

## 2. Why the existing lifecycle must change

The current production path is:

```text
CycleLifecycle::begin_block
  -> process_metadosis
  -> outbe_lysis::runtime::lysis
  -> Nod/Intex/Tribute writes
  -> Desis/Promis
  -> Metadosis COMPLETED and CE retirement
```

`CycleTick` is a begin-zone system transaction. Current
`CompressedEntitiesLifecycle::end_block` seals the CE overlay only after the
ordinary transaction loop. Creating `JobIntentV1` in today's `CycleTick` would
therefore bind a snapshot before same-block user writes and is forbidden.

The existing system-transaction layout already models a contiguous begin zone,
user middle and end suffix, although every current kind is begin-zone. The PoC
adds exactly:

| Kind | Selector/body | Position | Work |
|---|---|---|---|
| `OcompLifecycleBegin` | `OSE2`, V2, empty | begin zone after `LateFinalizeCredits`, before `CycleTick` | reserved mode barrier no-op, then bounded expiry/reset |
| `OcompTerminalRequest` | `OSR2`, V2, empty | sole end-zone kind | one bounded READY inspection/request after CE seal |

On the active fork the builder includes both mandatory envelopes. Import and
replay validate the exact zone, order, calldata, proposer signature and
presence. Pre-fork blocks contain neither.

When the transaction loop reaches the first end-zone envelope, the executor:

1. proves all ordinary transactions are already consumed;
2. calls the existing idempotent `finalize_compressed_entities()`;
3. requires the CE scope to be closed and its final root written;
4. executes `OcompTerminalRequest` as a normal receipt-visible system call;
5. rejects any user transaction or second/unknown end kind after it.

The terminal handler performs no CE body mutation. `finish()` observes CE
already finalized and only validates header/receipt/state artifacts. This is the
smallest change that makes the request state root commit both the sealed CE root
and the intent without a second block-execution path.

The reserved mode/revocation slot is an explicit first function in
`OcompLifecycleBegin`; it is a no-op in the PoC. BoundedMVP can populate it
without moving expiry relative to ordinary transactions.

## 3. Consensus ownership and minimal code placement

No third new package is needed.

| Ownership | Placement | Contents |
|---|---|---|
| canonical bytes and pure verification | `crates/system/ocomp-protocol` | OCB1 types, hashes, limits and certificate/finality helpers; no runtime authority |
| execution-frame authority | existing `outbe-primitives` storage capability seam plus `CtxStorageProvider` | one Lysis-specific non-serializable frame token; no protocol types or business-state access |
| Lysis result/apply meaning | `crates/core/lysis/src/activation_v1/` | structural result verifier, typed apply plan, receipt-equation verifier; no Fidelity/Oracle/storage reads |
| Lysis job/FSM orchestration | `crates/core/metadosis/src/ocomp/` | state/schema, READY/request, expiry, activation entry, conflict/terminal transitions and public views |
| lifecycle ordering | existing `system_tx.rs`, executor and builder modules | two fork-gated system kinds and CE-before-end-zone handoff |
| owner effects | existing NodFactory, Intex, Tribute, Desis and PromisLimit crates | strict request-phase split effect plus four narrow activation methods/receipts |
| public ABI | existing Metadosis precompile/interface | one write selector and three bounded views |

Keeping the concrete state in Metadosis is intentional: the persisted intent,
budget split, preconditions, result and apply bytes are Lysis V1-specific, and
Metadosis owns
the trigger/day transition. The `ocomp` module is a deep internal lifecycle
boundary, not a one-entry `ProgramRegistry`. A future program must add its own
typed state and fork before any demonstrated common part is extracted.

`crates/system/ocomp-protocol` remains free of node, database, process, HTTP,
business-state and runtime capability access. Canonical `EffectBindingV1` and
`ActivationCallId` bytes live there; the non-serializable runtime token does
not.

## 4. Fork-pinned state model

### 4.1 WorldwideDay additions

Existing status values remain unchanged. The fork appends:

```text
OFFCHAIN_PENDING = 8
```

Each WWD record gains:

```text
pending_nonce: u64
state_version: u64
pre_admission_envelope: PreAdmissionEnvelopeV1
pre_admission_envelope_hash: Hash
```

The fresh devnet initializes `pending_nonce=0`. For an intent created with
nonce `N`:

```text
JobIntent.pending_nonce = N
JobIntent.attempt = checked_u32(N)
```

Expiry or conflict increments the nonce exactly once. If it no longer fits
`u32`, the day remains `READY` with `ATTEMPT_EXHAUSTED`; no truncated attempt is
created. `terminal_pending_nonce` in `ActivationCallCoreV1` is the old live
nonce.

### 4.2 Canonical job record

The fixed Metadosis storage path maps `IntentId` to canonical
`OcompJobRecordV1`:

```text
OcompCompletedBindingV1 {
  JobId: Hash,
  activation_call_id: Hash,
  ResultDigest: Hash,
  result_evidence_hash: Hash,
  terminal_receipt_hash: Hash,
  terminal_receipt: AggregateActivationReceiptV1
}

OcompJobTerminalV1 {
  outcome: COMPLETED(1) | EXPIRED(2) | CONFLICTED(3) | CANCELED(4),
  terminal_height: u64,
  terminal_time: u64,
  next_pending_nonce: Option<u64>,
  completed_binding: Option<OcompCompletedBindingV1>
}

OcompJobRecordV1 {
  intent: JobIntentV1,
  status: OFFCHAIN_PENDING(1) | COMPLETED(2) |
          EXPIRED(3) | CONFLICTED(4) | CANCELED(5),
  terminal: Option<OcompJobTerminalV1>
}
```

Shape rules are closed:

- pending has `terminal=none`;
- completed has `next_pending_nonce=none` and a completed binding whose receipt
  outcome is `APPLIED`;
- expired has next nonce and no completed binding;
- conflicted has next nonce and a completed binding whose receipt outcome is
  `CONFLICT_RESOLVED`;
- canceled is codec-only in PoC; no handler can produce it.

The intent itself remains in the terminal record so public reads and historical
proofs never depend on supervisor storage.

### 4.3 Indexes and bounded retention

Consensus indexes are:

```text
ReadyWorkKeyV1  = (next_check_height, wwd, pending_nonce)
ExpiryKeyV1     = (deadline_height, IntentId)
live_by_wwd     = wwd -> IntentId
active_by_wwd   = wwd -> ActiveGenerationV1
terminal_order  = bounded ordered IntentId queue
```

No READY or expiry scan is permitted. The PoC limits are:

```text
max_pending_jobs = 1
max_ready_inspections_per_block = 1
max_expirations_per_block = 1
retry_backoff_blocks = 1
max_terminal_job_records = 365
```

The terminal cap reuses the current bounded Metadosis horizon. At capacity,
new OCOMP work is deferred; terminal evidence is not silently evicted. This is
a disposable-devnet bound, not an MVP retention policy.

### 4.4 State invariants

```text
WWD OFFCHAIN_PENDING
  <=> one live_by_wwd entry for its current nonce
  <=> one pending job record
  <=> one expiry entry
  <=> one immutable budget split and activation-precondition set

terminal job
  <=> no expiry entry

COMPLETED
  <=> one APPLIED aggregate receipt
  <=> one active_by_wwd generation with matching JobId/result evidence

READY
  <=> one due-index entry
  <=> no live current-nonce intent
```

The state module exposes one invariant checker used after every lifecycle
transition and by model tests. It is not a repair routine.

## 5. Request creation without on-chain Lysis

### 5.1 READY production

At the active fork `CycleTick` may advance a day to `READY` and insert its
canonical due key, but it cannot call the legacy non-empty
`process_metadosis -> lysis` branch. The terminal OCOMP system phase owns the
one bounded inspection.

The terminal phase handles four cases:

1. empty day: the existing direct empty compatibility result;
2. zero limit, unknown day type or other legacy-ineligible case: its frozen
   compatibility branch;
3. eligible non-empty but unknown/over-limit/unapplicable: typed `Deferred`
   and reinsertion at the fixed next height;
4. eligible bounded non-empty: atomic split/effect/intent transition.

Only case 4 creates OCOMP state. No branch invokes Lysis.

### 5.2 O(1) authenticated pre-admission

The terminal handler reads only bounded consensus state:

- sealed `DayTotals` and CE catalog/root identity;
- maintained `PreAdmissionEnvelopeV1`;
- frozen Metadosis scalars;
- Desis/Promis request-effect pre-state and activation target preconditions;
- active bundle/profile/committee snapshot.

It does not enumerate Tribute bodies, owners, Fidelity or Oracle. The maintained
pre-admission envelope additionally pins:

```text
auction_entry_price: U256
auction_entry_price_source:
  LAST_CLOSED_DAY_VWAP(1) | CURRENT_VWAP_FALLBACK(2)
auction_entry_price_source_day: u32
oracle_state_version: u64
```

Relevant normal Oracle/Metadosis updates maintain this bounded authenticated
value before the terminal phase. `FrozenMetadosisValuesV1.auction_entry_price`
must equal it. This closes the PFS requirement that neither the request nor
activation block calls an Oracle calculation path; it is canonical state, not a
worker-supplied price.

### 5.3 Budget split and activation preconditions

The request derives:

```text
day_limit = lysis_budget + auction_base
lysis_budget = min(day demand, day supply)
```

For GREEN it strictly dispatches one Desis brief for `auction_base`. For RED it
dispatches no auction and checked-adds `auction_base` to
`PromisLimit.total_unallocated`.

The request records `RequestBudgetSplitReceiptV1`. It also freezes read-only
activation preconditions:

- the exact sealed Tribute binding/generation;
- absence of the target Nod generation;
- absence/version of the contributor series;
- exact Metadosis nonce, attempt and state version.

No reservation copy is written into Tribute, NodFactory, Intex, Desis or
PromisLimit. Desis is complete before the job. Promis is a commutative
accumulator whose actual before/after values are certified by its receipt.

The split effect, split receipt, `JobIntentV1`, expiry/live indexes,
`OFFCHAIN_PENDING` and `OffchainJobRequested` share one checkpoint. Any strict
request effect failure defers before writes or rolls back the transition.

### 5.4 Expiry and retry

At height `H`, `OcompLifecycleBegin` first runs the no-op mode barrier, then
processes every due expiry up to the frozen cap. For the only PoC job it:

1. requires exact pending job/WWD/index/budget invariants;
2. removes the expiry/live entry;
3. stores terminal `EXPIRED`;
4. increments pending nonce;
5. changes the WWD to `READY` with the same `lysis_budget`;
6. inserts `ReadyWorkKeyV1(H+1, wwd, next_nonce)`;
7. emits `OffchainJobExpired`.

The one-block retry backoff prevents the end-zone of height `H` from immediately
creating the next attempt. An activation at `H` observes the old terminal
record/current nonce mismatch and rejects. No compute process is needed for
expiry, and the request-phase Desis/carry-over effect is not repeated.

Missing/corrupt budget or index state is an invariant/fatal block error.

## 6. Public activation ingress

### 6.1 Existing address, normal transaction

The only public write is the frozen:

```solidity
function activateLysis(bytes calldata pocActivationV1)
  external
  returns (bytes32 activationCallId, bytes32 resultDigest, uint8 outcome);
```

It remains at the existing Metadosis address and is permissionless, nonpayable
and non-static. The relay submits a normally paid Ethereum transaction. There
is no ZeroFee change.

`outbe_ctx_dispatch` adds a Metadosis-with-readers arm so activation receives
the same current-block `ExecutionScope` and authenticated parent body authority
already used by Nod/Tribute. Existing Metadosis views remain on the simple
dispatch path. Missing body authority is fatal for activation, never replaced
with a direct reader.

### 6.2 Early bounded admission

Before ABI allocation or cryptography:

1. RPC/txpool classify exact address+selector and enforce calldata/RLP caps;
2. the executor block-scoped meter permits at most one activation attempt and
   the frozen bytes/work budget;
3. Metadosis preflight verifies the single ABI dynamic offset/length, zero
   value and non-static context;
4. OCB1 checks envelope length/kind/version before nested allocation;
5. each nested vector checks its byte/count cap before allocation.

Txpool shape admission is not semantic authority. Every proposer/import/replay
path repeats consensus caps. A second activation or cap+1 input rejects before
large decode/signature work. Proposer selection skips it; a received block must
produce the same deterministic outcome under the frozen block policy.

No activation-specific fee waiver or transaction class is introduced.

### 6.3 Verification order

After bounded canonical decode:

1. load the `IntentId` job record;
2. if `COMPLETED`, derive the claimed binding/digest and return the stored
   receipt only when they match exactly; run no signatures or owner calls;
3. reject `EXPIRED`, `CONFLICTED`, `CANCELED` and mismatching completed claims;
4. require current height `< deadline_height`, exact pending nonce/attempt and
   active fork/bundle;
5. verify `FinalizedIntentProofV1`, historical committee authority, canonical
   intent and derived `JobId`;
6. reconstruct `ActivationPayloadV1`, `ResultDigest` and
   `ResultEvidenceHash`;
7. verify the pinned OCOMP committee snapshot and exact 3-of-4 certificate;
8. call `outbe_lysis::activation_v1::verify_result`, which checks only typed
   structure, IDs, ordering, roots, counts, totals, arithmetic/event
   commitments and activation-precondition membership;
9. compare all current target pre-states with the frozen preconditions;
10. choose certified conflict or certified apply.

The Lysis verifier accepts no `StorageHandle`, `ExecutionScope`,
`ParentBodySource`, Fidelity or Oracle interface. A compile-time dependency
test and call-path trace enforce this.

## 7. Private capability and closed owner dispatch

### 7.1 Runtime-unforgeable frame

`CertifiedLysisActivation` lives under the existing
`outbe-primitives::storage` capability seam and is a public type only so
existing owner crates, which already depend on `outbe-primitives`, can name it.
This avoids both a new crate and the impossible dependency cycle that would
result if owner crates depended on `outbe-lysis` or `outbe-metadosis`. It has:

- private fields;
- no public value constructor;
- no `Clone`, `Copy`, `Serialize`, `Deserialize`, OCB1 tag or ABI form;
- one exact raw `ActivationCallId`/binding hash (`B256`), so
  `outbe-primitives` does not depend on `outbe-ocomp-protocol`;
- a monotonic owner-step cursor;
- a borrowed production execution-frame lease.

`StorageHandle::with_lysis_activation_frame(call_id, closure)` is the only
constructor. It constructs the private token inside `outbe-primitives` after
the provider grants a one-shot lease; no public factory returns an owned token.
The production `CtxStorageProvider` grants the lease only when:

- current precompile is Metadosis;
- current selector is exactly `activateLysis(bytes)`;
- call is non-static and value-free;
- no activation frame is active;
- the caller supplies the derived `ActivationCallId`.

`outbe_ctx_dispatch` records the exact selector/value/static context in the
per-call provider configuration before creating `StorageHandle`. The provider
trait's default implementation denies the lease. Hash-map/direct providers do
not grant it; narrowly scoped owner tests use an explicit `cfg(test)` provider
and cannot export a production success constructor.

The only production call site is after step 9 above. Direct/hashmap providers
do not grant it outside that explicit test-only support. CI checks that the
frame constructor has exactly the allowlisted production call path.

This is not a generic capability system: names, selector, binding and owner
sequence are Lysis V1-specific.

### 7.2 Exact owner order

Inside one outer `StorageHandle::with_checkpoint`, the verified apply plan and
capability permit exactly:

```text
1 NodFactory::issue_certified_batch
2 Intex::record_certified_contributors
3 Tribute::consume_certified_partition
4 PromisLimit::credit_certified_carry_over
5 aggregate receipt verification and terminal commit
```

Each owner method consumes its step permit, rechecks its live precondition and
constructs its own receipt. Calling an owner twice, out of order, with another
binding or after frame exit fails. Raw storage keys/calls are never inputs.

### 7.3 Owner-specific reuse

#### NodFactory

Refactor the existing private issuance core to accept explicit `issued_at`.
Legacy pre-fork `issue_nod` supplies `storage.timestamp()`; certified batch
supplies only request `logical_evaluation_time`.

For each ordered `NodActionV1`, the owner:

- rederives Nod ID and bucket key;
- checks the frozen WWD/generation precondition and current absence;
- reuses the existing Nod item/bucket CE mutation helpers and event order;
- checked-accumulates counts, amount, Gratis and root inputs;
- returns `NodBatchReceiptV1`.

There is still no public Nod issuance selector.

#### Intex

The current `write_contributors` can overwrite and uses unchecked aggregate
addition. The certified method is a new strict owner path:

- requires the exact frozen absent series/version;
- validates order/uniqueness and max count;
- checked-aggregates owner nominals;
- writes the existing dense contributor representation;
- records the owner transition event;
- returns `ContributorReceiptV1`.

The legacy helper remains reachable only from pre-fork Lysis/tests.

#### Tribute

The certified method combines the existing checked DayTotals/supply transition
with one CE catalog retirement request:

- exact sealed root/count/nominal/generation must match the intent binding;
- total supply is checked-subtracted;
- day totals become zero;
- the WWD catalog pointer is retired without enumerating physical bodies;
- the existing retirement event is emitted;
- receipt reports generation `1`.

Mongo/source bodies remain retained under the independent terminal-finality
retention rule. No synchronous per-record delete is added.

#### Desis

The existing `dispatch_auction_brief` is best-effort and swallows errors, so it
cannot be used for the request split. Refactor its state-writing core:

- legacy/empty compatibility path may keep the best-effort wrapper;
- OCOMP request calls a strict `apply_request_auction_base`;
- it uses request logical time and the frozen entry price;
- the stage must accept exactly one brief for `auction_base`;
- any error rolls back request creation;
- Desis is never called during activation.

#### PromisLimit

One checked primitive updates `total_unallocated` and returns a typed receipt.
Request uses it for RED `auction_base`; activation uses the capability-gated
form for `unused_lysis`.

Day-limit formation checked-takes the whole available carry-over into the next
not-yet-formed limit. An unrelated prior add does not conflict.

### 7.4 Raw-path closure

At and after the fork:

- eligible non-empty Metadosis cannot call `outbe_lysis::runtime::lysis`;
- public precompiles expose none of the raw certified owner methods;
- certified owner methods require the frame capability;
- legacy owner helpers remain only for pre-fork, empty/ineligible compatibility
  or existing non-OCOMP domain flows;
- static callgraph/allowlist tests fail if active activation reaches legacy
  Lysis, Fidelity league, Oracle calculation or a raw write bypass.

No production test helper can construct a success capability.

## 8. Receipts, conflict and atomic terminal commit

### 8.1 Receipt binding

Each owner receipt hash uses its frozen domain over canonical receipt bytes.
`state_event_digest` is not an opaque owner claim: it is recomputed from the
exact binding plus the owner-specific action/pre/post projection frozen in the
protocol manifest. ABI log parity is generated from the same projection and
tested separately.

After all four activation calls,
`outbe_lysis::activation_v1::verify_receipts` consumes those receipts and the
stored request split receipt. It checks:

- all `EffectBindingV1` values equal the private capability;
- exact activation preconditions/versions;
- Nod/Tribute/result counts and roots;
- contributor root/count/eligible nominal;
- `day_limit = lysis_budget + auction_base`;
- `lysis_budget = Nod Gratis consumed + unused_lysis`;
- the exact GREEN Desis or RED carry-over request receipt;
- the exact activation carry-over credit;
- owner pre/post event digests;
- fixed owner order and no missing/duplicate receipt.

Only then can the capability produce the terminal permit.

### 8.2 APPLIED

Using the terminal permit, Metadosis in the same checkpoint:

1. removes expiry/live indexes;
2. constructs `ActiveGenerationV1`;
3. constructs and stores `AggregateActivationReceiptV1(APPLIED)`;
4. writes the job terminal binding and `COMPLETED`;
5. changes WWD `OFFCHAIN_PENDING -> COMPLETED`;
6. removes it from active/READY indexes without deleting OCOMP evidence;
7. emits the frozen `MetadosisExecuted` fields and `LysisActivated`.

The complete result remains once in canonical transaction bytes. Consensus
state stores only the intent, binding, active generation, receipt and hashes.

### 8.3 Certified conflict

A target conflict is not an invalid-certificate shortcut. The verifier first
completes finality, certificate and result checks. If a live target differs from
its frozen precondition, the same checkpoint:

1. invokes no owner effect method and creates no capability;
2. removes expiry/live indexes;
3. stores `AggregateActivationReceiptV1(CONFLICT_RESOLVED)`;
4. marks the old job `CONFLICTED`;
5. increments nonce, returns WWD to `READY` with the same budget;
6. schedules `height+1`;
7. emits `OffchainJobConflicted`.

Junk evidence can never force conflict resolution.

### 8.4 Rejections and failures

| Condition | Result |
|---|---|
| malformed/over-limit/proof/certificate/result mutation | typed transaction revert; no state/event diff; job stays pending |
| completed exact binding/digest retry | return recorded call/digest/outcome; no owner call, event or second aggregate receipt |
| completed different binding/digest | typed revert |
| expired/conflicted/canceled | typed revert |
| declared activation-precondition conflict after valid evidence | successful `CONFLICT_RESOLVED` terminal transition |
| owner declared failure or receipt mismatch | outer checkpoint and transaction revert; job stays pending |
| storage/provider/invariant corruption | fatal block error; candidate rejected |

A focused owner-failure/receipt-mutation hook is test-only and can select a
named step after real preceding writes. It cannot create a success result or
bypass public activation.

### 8.5 Error surface

Activation failures use the frozen custom ABI error
`OcompActivationRejected(uint16)` and the protocol code registry. Internal fatal
errors are never converted into this user error. This keeps mutation evidence
stable without exposing unbounded strings as consensus behavior.

## 9. Public authority after completion

No custom RPC is required. Add three view methods to the existing Metadosis
precompile:

```solidity
function getOffchainJob(bytes32 intentId)
  external view returns (bytes memory ocompJobRecordV1);

function getActiveLysisGeneration(uint32 wwd)
  external view returns (bytes memory activeGenerationV1);

function getLysisTerminalReceipt(bytes32 intentId)
  external view returns (bytes memory aggregateActivationReceiptV1);
```

They return canonical OCB1 bytes with normal not-found errors and strict output
caps. Public `eth_call` reads, `eth_getProof` at an exact block, transaction
receipt/logs, existing Nod/Contributor/Desis/Promis/CE views and finalization
RPC are the outcome authority.

The active generation is selected only by canonical Metadosis state. Supervisor
journals, CAS and Mongo cannot select or prove a completed result.

## 10. Logical time

The verified apply plan enforces:

| Value | Source |
|---|---|
| Nod `issued_at` and semantic Metadosis/effect fields | request `logical_evaluation_time/height` |
| auction entry price | request-pinned pre-admission value |
| certificate | no wall-clock field |
| terminal receipt `activated_at_*` and EVM receipt location | actual activation block |

NodFactory certified issuance cannot call `storage.timestamp()` for semantic
fields. Desis ran in the request phase and is not activation-dependent.
Delaying a valid activation changes only explicit activation metadata.

## 11. Concrete file and symbol ownership

The implementation plan may refine filenames only without moving authority:

```text
crates/system/ocomp-protocol/src/
  state.rs                 OcompJobRecordV1 nested canonical state types
  activation.rs            hashes, error codes, certificate/finality/result evidence
  receipts.rs              owner/aggregate receipt canonical types and hashes

crates/blockchain/primitives/src/storage/
  lysis_activation.rs      CertifiedLysisActivation and one-shot frame lease
  handle.rs                with_lysis_activation_frame; provider default denies

crates/core/metadosis/src/ocomp/
  schema.rs                job/live/due/expiry/active/terminal storage
  request.rs               terminal READY inspection, split and early effect
  expiry.rs                begin-zone expiry/reset
  activation.rs            public live/terminal dispatch and outer checkpoint
  state.rs                 transitions and invariant checker
  views.rs                 three bounded OCB1 public reads

crates/core/lysis/src/activation_v1/
  verify.rs                storage-free structural/result verification
  apply_plan.rs            closed owner-specific typed plan
  receipts.rs              aggregate equations

crates/core/{nodfactory,intex,tribute,promislimit}/src/
  certified.rs             activation method and receipt construction

crates/core/{desis,promislimit}/src/
  ocomp_budget.rs           strict request split effect/carry-over primitive

crates/blockchain/primitives/src/system_tx.rs
crates/blockchain/evm/src/{executor.rs,begin_block_precompile.rs,precompiles.rs}
crates/blockchain/evm/src/storage/ctx_provider.rs
crates/blockchain/node/src/payload_builder.rs
crates/blockchain/txpool/src/lib.rs
contracts/precompiles/src/IMetadosis.sol
```

`outbe_ctx_dispatch` and the executor meter are existing integration seams. No
new executor, transaction type, RPC namespace, generic handler table or owner
adapter crate is added.

## 12. Blocking implementation evidence

Ticket #9 will assign final IDs and CI lanes. This decision cannot be considered
implemented without evidence for at least:

1. fork-1/active/fork+1 system-tx layout, including required end-zone order;
2. user transaction before request changes the sealed root the intent binds;
3. no semantic writer runs after `OcompTerminalRequest`;
4. request commits split, exact early effect, receipt, intent/indexes/event
   together or none;
5. expiry at the exclusive deadline precedes activation and leaves READY until
   the next height;
6. terminal exact retry performs zero owner calls and returns stored identity;
7. valid evidence plus CAS conflict produces only `CONFLICT_RESOLVED`;
8. every malformed/proof/certificate/result/receipt mutation leaves pending
   state byte-identical;
9. a failure after each of the four activation owner steps rolls back all
   earlier effects,
   CE overlay changes, events, active generation and terminal state;
10. Nod uses request logical time; delayed activation never repeats Desis;
11. Promis carry-over add/take is checked, and next-day consumption is atomic;
12. public RPC -> txpool -> P2P -> proposal -> import -> replay carries the exact
    activation; cap+1/second activation rejects before expensive work;
13. final public job/generation/receipt, Nod/effect reads and CE absence proof
    agree after finality and replay;
14. static and runtime traces contain no active-fork call to Lysis execution,
    Fidelity league or Oracle calculation;
15. callsite scanning finds one production capability-construction path and no
    public/raw effect bypass.

Direct handler invocation can be a focused unit/component test but cannot close
the public-path or four-validator E2E gate.

## 13. PoC-to-BoundedMVP seam

The following remain unchanged:

- begin barrier -> expiry -> transactions -> CE seal -> terminal request order;
- `JobIntentV1`, JobId/finality binding and exclusive deadline;
- public typed activation and private capability;
- request split receipt, four activation owner receipts and atomic terminal
  transition;
- active-generation authority and exact retry meaning.

BoundedMVP may harden terminal retention, governed cancellation, migration,
pause/revocation handlers, historical committee rotation, crash recovery and
operations under a new governed bundle. It does not replace this core with a
generic write set or synchronous calculation.

## 14. Decision result

Ticket #8 is resolved without grilling. The normative documents and current
executor/CE/owner seams determine the answer:

- JobIntent/FSM live in a concrete Metadosis-owned OCOMP consensus module;
- expiry is a mandatory begin-zone system phase;
- request creation is a mandatory end-zone phase after real CE sealing;
- activation is one normal paid public Metadosis transaction;
- on-chain work is bounded verification plus typed owner application, never
  Lysis;
- one runtime-unforgeable Lysis capability and four activation owner receipts
  close the
  write boundary;
- active generation and canonical receipts, not off-chain storage, are public
  truth.
