# Metadosis code health refactor

Branch: `refactor/metadosis-code-health`, based on `main` at `b1149eb6`.
Fresh-chain storage changes are intentional; no migrations or legacy decoder.
Public commands, read APIs, Solidity ABI and protocol economics stay unchanged.

## Execution checklist

- [x] Unify terminal receipt storage and proof/genesis bindings.
- [x] Normalize the live job index to identity keys.
- [x] Decompose aggregate validation by index invariant.
- [x] Separate request, finality and voting.
- [x] Isolate completion and expiry.
- [x] Separate worldwide-day advancement and effects.
- [x] Decompose activation fixtures by domain responsibility.
- [x] Extract precompile queries; run final verification and static analysis.

## Baseline

`cargo test -p outbe-metadosis --all-targets --features test-utils --locked --offline`:
240 library tests, 2 compile-fail harness tests, 5 FSM model tests and 5 semantic
facade tests passed; one exhaustive capacity fault matrix is intentionally ignored
in the default run and must run separately.

LLVM line coverage of `crates/core/metadosis/src` files (excluding `/tests/` and
`test_support` paths): 7,077 / 8,669 lines, **81.64%**. Measured with
cargo-llvm-cov 0.8.5. Inline `cfg(test)` blocks remain in LLVM file summaries;
this is source-file coverage, not a production-only line classification.
Baseline SARIF and LLVM JSON are retained in `/tmp/metadosis-before.sarif` and
`/tmp/metadosis-coverage-before.json` during this work.

## Decisions and evidence boundaries

The analyzer's untested-hotspot claim does not describe the existing test suite:
there are behavioral command/API/ABI, replay and fault-injection tests. Missing
coverage ingestion and paired test-file detection are separate metadata issues.
New codec tests pin independent wire vectors and reject malformed input; tests
do not inspect implementation source text.

Terminal receipt common fields have one authority. Capacity evidence is a tagged
variant in the same byte record. Live scheduler keys identify the authoritative
per-day FSM rather than duplicating its snapshot. Layout changes require updating
the Fidelity opening codec registry from its TSV authority as well as genesis pins.

History-based churn, defect and coupling findings are background signals and are
not targets of this refactor. Test fixture `unwrap`s intentionally fail incomplete
setup; accessor/factory cohesion findings require behavioral justification before
changing interfaces.

Step 1: 245 library tests and all 12 integration/model/facade harness tests passed.
The fixed OMTR wire vectors first failed against the empty encoder, then passed
with canonical encoding. Registry and measurement shape were regenerated from
their authorities; the intermediate live codec remains OMLI1 until step 2.

Step 2: 249 library tests passed, one separately gated fault matrix ignored.
OMLI2 stores 36-byte identity keys instead of 148-byte FSM copies. Snapshot and
carrier admission byte caps follow the new key size; per-day FSM validation remains
authoritative. The fresh-chain layout hash now commits OMLI2.

Step 3: 249 library tests passed. Aggregate validation is split into profile
presence, pending/live membership, job deadlines, response windows and READY keys.
Both directions of index equivalence and completed quorum windows remain checked.

Step 4: 251 library tests passed. Request binding is named identity/limit/hash
policy, voting selects all due jobs before committing the unique candidate, and
finality pins inclusive admission heights plus checked open/deadline arithmetic.
Private tests preserve retained receipt nonce semantics and reject overflow.

Step 5: 252 library tests passed. Completion separates live pre-state, terminal
permit binding, conservation, applied evidence, persistence and ordered events.
The one-shot permit still commits last, and the generation write still precedes
the finalized-record check inside the existing rollback boundary. Failed expiry
checks preserve fallible read order and split identity/membership/evidence policy.

Step 6: 252 library tests and 60 lifecycle tests after the final context cleanup
passed. Advancement keeps a single retained-count snapshot and admission budget
for the tick. Effects preserve the distinct Tribute/Promis mutation order of
capacity forfeiture and missed offering. Only the existing VWAP overflow prefix
is tolerated; every other snapshot error still propagates.

Step 7: 252 library tests, 2 compile-fail harnesses (6 UI cases), 5 FSM model
tests and 5 semantic facade tests passed. Fixture artifacts, committee signing,
parent tree, raw WWD setup and activation stages now have separate modules.
Constructor stages retain real command transitions and deterministic vote bytes.

Step 8: precompile dispatch delegates four typed query projections to
`precompile/queries.rs`. Missing-record tuples, caller rejection, stored-tag
corruption and inactive Lysis submission retain their ABI/error boundaries.
Final review added a red/green assertion for the original capacity receipt
immutability Fatal message and unchanged persisted receipt after rejection.

## Diff summary and marker effects

Paths below are relative to `crates/core/metadosis/src` unless specified.

| File family | Concrete change | Expected marker effect |
| --- | --- | --- |
| `schema.rs`, `terminal.rs`, `terminal/{model,codec,store}.rs` | Replace generic/detail duplicated terminal records with one tagged immutable receipt; common membership/conservation and variant evidence are separate validators | Remove duplicate-field divergence and the former 7/12-operator conditions; independent malformed wire and policy tests cover the new representation |
| `ocomp/{codec,live_index,store,views,vote_admission}.rs` | Store only sorted `(WorldwideDay, IntentId)` live keys and resolve each key to its authoritative FSM; enforce key identity and canonical bounds | Remove duplicated FSM snapshots; keep both membership directions and admission resource limits explicit |
| `aggregate.rs`, `aggregate/ocomp_indexes.rs` | Separate profile presence, pending/live equivalence, deadline/status policy, response window equivalence and READY membership | Reduce the former 157-line/CCN32 method, nesting and mixed compound condition |
| `ocomp/transitions.rs`, `ocomp/transitions/{request,finality,voting}.rs` | Named request bindings; checked finality window; select the unique due job before committing | Reduce request length/complex conditions while preserving receipt nonce inequality, deadlines and public entry points |
| `ocomp/transitions/{completion,expiry}.rs`, `terminal/failure.rs` | Completion input groups private data; separate pre-state, permit, conservation, evidence, persistence and events; isolate expiry/failure validation | Reduce completion brain/large/complex-method and expiry boolean-policy findings; retain rollback, fallible-read and event order |
| `lifecycle.rs`, `lifecycle/{advance,effects}.rs`, `state.rs` | Advancement context carries the tick budget/snapshot; typed effect context binds a day and transition; shared day-limit binding | Reduce deeply nested advancement/effect branches and private positional parameters; preserve reason-specific cross-contract mutation order |
| `ocomp/test_support.rs`, `ocomp/test_support/{artifacts,committee,parent_tree,wwd,activation}.rs` | Private fixture facade delegates domain responsibilities; activation uses ordered seed/prepare/request/finality/vote stages | Reduce the monolithic fixture constructor and mixed file responsibilities; retain deterministic signing bytes and semantic fixture surface |
| `precompile.rs`, `precompile/queries.rs` | Dispatch is the selector/metadata table; query helpers return Solidity-generated return types | Reduce dispatch size/nesting/brain method without a new ABI or a shared cross-precompile abstraction |

### Fresh-chain storage and proof binding

- Removed the unused persisted `active_wwd_count` and the separate capacity
  detail mapping. WWD records remain 10 slots each.
- Closed WWD base slot is now 13; OCOMP job-record mapping base is 19
  (previously 20); league-snapshot mapping base is 28 (previously 29).
- Unified terminal receipt mapping starts at slot 30. OMTR1 encodes either
  116-byte common receipts or a 208-byte capacity receipt. Unknown tags,
  wrong keys, noncanonical lengths and unsupported versions are fatal.
- OMLI2 encodes an 8-byte header and 36-byte identity keys. Per-day OMJS FSM
  records remain 148 bytes. Empty storage is the sole empty-index encoding.
- Day-limit receipt mapping starts at 31, with 7-slot records; terminal counts
  start at 38. `tests/state.rs` pins macro-generated slots to the descriptor.
- `proof_layout.rs` commits the layout and both codec versions in hash
  `b929da1d5f7064f33f1cf515babe3f581aebe3c0ad8ae5baf7903af692001fe6`.
- Direct external bindings: `crates/system/ocomp-protocol/src/league_snapshot.rs`,
  `registry/input-codecs-v1.tsv`, generated registry and measurement shape
  artifacts, plus `testing/e2e-harness/src/world/ocomp/fixtures/genesis.rs`.
  Registry/shape outputs were regenerated from their authorities and both
  `xtask ocomp registry --check` and `xtask ocomp shape --check` pass.

There is no migration, legacy decoder or old-layout compatibility path. Existing
stored state must not be reused for this fresh-chain layout. External Solidity
selectors, events and public command signatures were preserved.

## Final validation

| Gate | Result |
| --- | --- |
| Metadosis all-targets with `test-utils` | 252 library + 12 integration/model/facade tests passed; 6 UI compile-fail cases checked by two harness tests |
| Exhaustive capacity mutation-failure matrix | Separately run ignored test passed |
| Metadosis doctests | 6 passed |
| Metadosis no-default-features check | Passed |
| Metadosis all-targets clippy, `-D warnings` | Passed |
| Full OCOMP protocol suite | Passed, including finality vectors |
| Node OCOMP tests | 59 passed |
| Chain OCOMP genesis tests | 4 passed |
| EVM `ocomp_request_lifecycle` | 8 passed |
| Harness OCOMP genesis tests | 10 passed |
| Harness `ocomp-integration` feature build | Passed |
| Generated registry/measurement shape checks | Passed |
| Workspace formatting and diff whitespace | Passed |

These are local tests/build checks; a live multinode E2E network was not run.

LLVM all-targets source-file line coverage using the same exclusions rose from
**81.64%** to **83.31%** (7,586 / 9,106 lines). JSON and LCOV are retained locally
in `/tmp/metadosis-coverage-after.json` and `/tmp/metadosis-after.lcov`.

Qlty used the same configuration and `--include-tests` across the whole
Metadosis directory, including newly extracted helpers: **75 → 58 findings**.
The analyzed file count changed from 74 to 93; comparing only the shortened
original files would hide findings moved into helpers.

| Qlty rule | Before | After |
| --- | ---: | ---: |
| Function parameters | 15 | 13 |
| Return statements | 15 | 13 |
| Function complexity | 10 | 7 |
| File complexity | 4 | 2 |
| Boolean logic | 11 | 6 |
| Similar code | 17 | 14 |
| Identical code | 3 | 3 |

## Remaining findings and interpretation

- Public completion/finality and the existing expiry adapter retain their
  parameter counts (11/6/7). Private contexts reduce positional coupling
  without changing exported signatures merely to satisfy a metric.
- Fixture committee registration and proof-input builders still contain real
  similar-code findings. They are not presented as false positives; a common
  helper would need a separate semantic justification and scope.
- Fixture `unwrap` error-handling warnings describe fail-fast test setup,
  not unchecked production input. They intentionally remain.
- Existing aggregate/accessor factories share one domain authority despite
  static LCOM group counts. Splitting the public surface solely for that score
  is not justified by current behavior.
- The original "untested" evidence was missing coverage/paired-file metadata,
  not absence of tests. Coverage is now measured; it does not imply exhaustive
  branch coverage or per-test reachability contexts.
- The refreshed helper analysis retains size/complexity markers:
  `aggregate/ocomp_indexes.rs::validate_response_windows` has nesting 4 and CCN9;
  advancement, capacity effect, expiry, finality, request preparation and expired
  failure retain 62–85-line large-method findings. These are remaining work,
  not findings cleared merely by moving functions into files.
- Historical churn, defect, ownership and co-change signals remain. Moving code
  to new files does not constitute remediation of those historical risks.

## Repowise post-commit measurement

Snapshot: implementation commit `ea691ee8`, refreshed index and health analysis
on 2026-10-02. The final documentation amendment changes no analyzed code.
Strict LCOV import mapped all **54 Metadosis source files**, with no unresolved
paths. Its all-file summary is 82.63%; the 83.31% comparison above uses the same
explicit test-path exclusions as the baseline. Branch coverage and per-test
contexts were not supplied by this report.

| Original target | Code health / 10 | Max CCN | Max nesting |
| --- | ---: | ---: | ---: |
| `ocomp/transitions.rs` | 5.91 | 3 | 1 |
| `lifecycle.rs` | 5.81 | 2 | 1 |
| `precompile.rs` | 3.82 | 12 | 2 |
| `ocomp/test_support.rs` | 6.50 | 7 | 2 |
| `aggregate.rs` | 5.21 | 5 | 2 |
| `terminal.rs` | 6.15 | 2 | 1 |

These are per-file scores of the shortened entry/facade files, not an aggregate
claim about the whole refactor. All 19 extracted helper files were also queried;
their code-shape scores range from 8.05 to 10, but lack the original files'
historical risk. The untested-hotspot markers disappeared after measured coverage
ingestion; uncovered-line gradient penalties now describe actual gaps.

Remaining entry-file shape findings are dispatch complexity (CCN12),
aggregate accessor cohesion/validated-map `expect`, and several clone warnings.
Helpers retain the method-size/nesting findings listed above, fixture cohesion
and fail-fast setup `unwrap`s, actual duplication, and parameter warnings.
Qlty excludes receivers when counting method arguments (completion/finality
11/6); Repowise includes them (12/7). No exported signature was changed to remove
either warning.

## Whole-module closure, second pass

Authorized scope: all Metadosis Repowise and Qlty findings, including extracted
helpers and tests. Baseline `8a1103ee`: 58 Qlty rows and 941 Repowise rows.
`metadosis-findings.json` accounts for every baseline row. 197 git-history rows
are retained as non-editable history signals; the remaining rows must be fixed
or receive a code-grounded disposition, without blanket analyzer suppression.

- [x] Response-window policy and ordered capacity effect decomposition.
- [x] ABI missing-receipt, missing-artifact and successful metadata characterization.
- [ ] FSM state invariants and persisted equivalences.
- [ ] Commit/reducer and genesis domain operations.
- [ ] OCOMP voting, admission, expiry, request and activation.
- [ ] Private argument contexts, shared fixture setup and duplicated test assertions.
- [ ] Coverage gaps, complete analyzer rerun and per-finding reconciliation.

First slice: 255 library tests passed. Missing terminal/capacity receipts return
canonical NONE tuples; missing OCOMP artifacts retain their record-specific
Revert identities. Response-window policy distinguishes voting from completed
quorum windows. Capacity preflight, ordered Tribute/Promis effects, receipt,
transition and event projection are separate responsibilities.
