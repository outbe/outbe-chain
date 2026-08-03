# I7 checkpoint - update-bound enclave policy

Date: 2026-08-01

Status: `PASS` for I7. A successor enclave policy is governed and activated
through the existing protocol Update lifecycle. The V1 public route remains
inactive until I9, and this checkpoint does not claim accepted SGX hardware
evidence.

## Outcome

An existing validator-authored Update proposal may carry one exact canonical
successor `TeePolicyV1` alongside its protocol version and activation height.
Approval atomically schedules the software update and stages one bounded
`current`/`next` policy pair. There is no second policy proposal, relay,
executor, bond model or activation state machine.

Existing validator and full-node bindings may transition to the staged enclave
measurement before the update height through the bounded
`transitionEnclaveMeasurement(bytes,bytes,bytes)` ABI. The transition uses the
same quote-bound node and enclave proofs, fresh enclave/binding identities,
stable NodeHost authorization, counters, lease checks and native QVL boundary as
replacement. The durable replacement-candidate workflow accepts the transition
operation without adding a second local lifecycle.

At the Update activation height, begin-block execution promotes `next` to
`current` inside the same checkpoint as upgrade handlers, protocol-version
state, proposal status and activation events. Any handler, storage or policy
invariant failure rolls the complete activation back.

## Authority and state

The optional JSON field `teePolicy` is bounded lowercase hex of the complete
canonical policy. Unknown fields, empty/odd/uppercase/oversized encodings,
non-canonical policy bytes, wrong chain identity and a different activation
height reject. The exact proposal bytes remain visible through Vote state.

Approval additionally requires:

- initial V1 policy already installed;
- successor chain and genesis equal `current`;
- successor policy version equals `current + 1`;
- predecessor hash equals the exact current-policy hash;
- activation remains in the future;
- no different successor is already staged.

TeeRegistry appends slots 48 through 53 for staged length/chunks/hash, owning Update
proposal, activation anchor and last promoted proposal. Reads re-authenticate
canonical bytes against every anchor. Exact stage and promotion replay are
idempotent; a different owner cannot rotate or discard policy authority.

If another selected software update makes a staged Update stale, its exact
`next` policy is discarded inside the selected update's activation checkpoint
and the stale Update is canceled. A lower unrelated activation retains a
higher-version staged successor.

## Rolling cutoff

Before activation, ordinary register/renew/replace remains bound to `current`.
Only an existing binding may use `next`, and only through measurement
transition. The staged measurement is evaluated at its activation height, so a
node can pre-roll without making `next` ordinary admission authority early.

From activation:

- `next` is the sole current policy;
- old-policy register, renew and replace reject;
- the pre-activation transition selector is closed;
- an existing old-policy lease remains readiness-valid until its already
  committed expiry and is not retroactively revoked.

Registry admission requires exactly one height-active measurement-rule match.
Both zero matches and overlapping matches reject. Validator and full-node
transitions traverse the same ABI preflight, normative gas precharge and
post-verifier state machine in hardware-free acceptance tests.

## Acceptance audit

| I7 criterion | Authoritative evidence | Result |
|---|---|---|
| Existing governance authority only | optional exact policy is part of `ScheduleUpdatePayload`; Update remains validator-only with no new admission or bond override | `PASS` |
| Canonical and bounded payload | lowercase hex cap, canonical decoder, unknown-field denial and chain/height tests | `PASS` |
| One exact successor | chain/genesis, version, predecessor, future-height and one-staged-policy checks | `PASS` |
| Atomic approval | Vote target checkpoint rolls back both Update schedule and TeeRegistry staging on policy error | `PASS` |
| Deterministic activation | Update begin-block checkpoint promotes policy and version together; handler failure restores both | `PASS` |
| Replay and stale-state safety | exact stage/promotion replay is idempotent; superseded staged policy is discarded by exact proposal id | `PASS` |
| Rolling validator and full-node transition | both profiles use the bounded transition ABI and shared Registry mutation; NodeHost durable candidate accepts the transition operation | `PASS` |
| Exact measurement admission | Registry rejects zero or multiple active matches, including a reachable overlapping-rule test | `PASS` |
| Old-code cutoff | post-activation register, renew and replace under the old policy reject while the old lease remains ready to expiry | `PASS` |
| No hardware-controller scope | no PPID/platform-controller calldata, state, event, allowlist or API was added | `PASS` |
| Hardware evidence boundary | accepted transition tests begin after the private typed verifier capability and do not claim Intel hardware evidence | `PASS` scope boundary |

## Reachable verification

The checkpoint was closed with:

```bash
env CARGO_TARGET_DIR=/tmp/outbe-i7-replay-target \
  scripts/release/test_dcap_replay_ci.sh
cargo test -p outbe-primitives --features tee-attestation-v1 \
  --test tee_attestation_v1 --offline
cargo test -p outbe-teeregistry --features tee-attestation-v1 --offline
cargo test -p outbe-update --offline
cargo test -p outbe-tee --features native-dcap --offline
cargo check -p outbe-chain --offline
cargo fmt --all -- --check
git diff --check
```

Results:

- the deterministic replay gate passed ten artifact-contract checks, 21
  primitive tests, 55 native host tests, 25 public DCAP tests, five native-QVL
  tests, three finalized-session tests, two fixture-tool tests and all pinned
  corpus hashes;
- the focused primitive suite passed 21 tests;
- TeeRegistry passed 35 tests, including current/next, validator/full-node ABI
  transition, overlapping-rule rejection, activation rollback, stale-policy
  cleanup and full old-policy cutoff;
- Update passed 68 tests, including typed payload, approval staging, atomic
  promotion and handler rollback;
- the native DCAP/TEE package passed 55 unit, 25 public DCAP, five native-QVL
  and three remote-session tests;
- `outbe-chain` compiled with the V1 public route still inactive;
- format and whitespace checks passed.

## Review closure

The initial two-axis review found no behavior defect, scope creep or documented
standards violation. Its evidence-only gaps were Registry-level overlapping
match rejection, validator ABI transition coverage and complete
register/renew/replace cutoff coverage. All three were added to the reachable
test harness. Documentation wording and import grouping were also normalized.
Final staged standards and spec re-reviews reported no remaining findings.

## Deferred, not waived

I8 owns block-1 verification and bootstrap of the exact 32-validator committee,
including full block gas/RLP/size closure through production-shaped fixtures.
I9 owns exact-release `gramine-sgx` hardware evidence, fresh Processor and
multi-package Platform collateral, empirical budgets and activation of the V1
public route.

I7 adds no physical-machine allowlist, PPID/controller policy, offer-key
recovery, proof of deletion, DKG/BLS redesign, relay incentive or governance
replacement mechanism.
