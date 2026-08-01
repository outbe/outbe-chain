# Rust Code Assessment Report — `crates/core/metadosis`

Scope: the `outbe-metadosis` crate (WorldwideDay lifecycle + OCOMP off-chain compute FSM). ~5,100 production LOC across 22 top-level files + 18 `ocomp/` files; ~10,000 LOC of tests. Consensus-critical: precompile + block-lifecycle hooks + emission sink executed on every validator.

Assessed at HEAD `395c8fd8` on branch `refactor/metadosis-2`.

**Post-assessment corrections** (verified by three independent review panels
against `2f4674fe`, to which the branch was subsequently reset):

1. The "trybuild suite broken at HEAD" finding below is **orphaned**: the
   branch was reset from `395c8fd8` to `2f4674fe`, where all 12 `tests/ui/`
   files are present and the suite is green.
2. The claim that no test recomputes `keccak(METADOSIS_STORAGE_LAYOUT_V1_CANONICAL)`
   against the pinned hash was **false at the time of writing**: the assertion
   exists in `src/tests/state.rs` (`test_storage_dsl_layout_slots`).
3. R1's "at 366 entries every FSM read Fatals" sub-claim is **dead code**: all
   push sites guard `>=` at 365 while the read guards `>`, so the vector
   saturates at 365 and reads keep working. The real failure mode is a Fatal
   on every subsequent *terminal transition* — made permanent by the
   `vote.rs` exact-response-deadline coupling, which converts the first
   rolled-back begin-zone phase into an unrecoverable per-block Fatal.
4. R1 and R3's `submitLysisResult` reclassification are **closed** on this
   branch — see `audit_metadosis.md` at the repo root for the closure records.

**Verified-broken at `395c8fd8` (orphaned by the reset, kept for the record):**
commit `395c8fd8 "update"` deleted all 12 `tests/ui/` files (6 `.rs` + 6
`.stderr`, 175 lines) added by `2f4674fe`, but left
`tests/external_mutation_surface.rs` referencing them, turning
`cargo test -p outbe-metadosis` and the workspace `test` CI job red.

---

## 1) Architecture & Crate Structure: Scalability Challenges

### Workspace & Modularity

**The lifecycle/runtime inversion — the repo standard's explicitly named anti-pattern.** `.ruler/module_structure.md` defines `runtime.rs` as "main business logic and orchestration" and `lifecycle.rs` as "thin block hook entrypoints delegating into `runtime.rs`", and names the anti-pattern "lifecycle.rs becomes a second runtime.rs". This crate inverts the direction:

- `runtime.rs` (125 lines) is the thin forwarder — every public fn delegates into `lifecycle::` (`runtime.rs:103`, `:116`, `:124`, `:20`; `start_metadosis` at `:54–91` makes four more `lifecycle::` calls).
- `lifecycle.rs` (450 lines, zero delegation upward) owns all business logic and all cross-module calls: `advance_active_worldwide_days` (`:49–117`), `apply_capacity_forfeiture` (`:248–345`, Tribute + PromisLimit + receipt + event), `apply_missed_offering` (`:347–408`), oracle VWAP snapshot (`:410–423`), day-type policy (`:441–450`).
- Actual block hook entrypoints live in a third file, `commands.rs` (`init_genesis_day:17`, `start_metadosis:28`, `run_ocomp_lifecycle_begin:128`, `run_ocomp_terminal_request:139`) — which is the content the standard assigns to `lifecycle.rs`.

Three layers where the standard defines two, with the two named files swapped. Anyone navigating by the repo convention lands in the wrong file every time. This complicates onboarding and makes "add a new lifecycle phase" a three-file archaeology exercise.

**Seven files are outside the standard's file dictionary** (`commit.rs`, `aggregate.rs`, `reducer.rs`, `settlement.rs`, `terminal.rs`, `pre_admission.rs`, `proof_layout.rs`). Some are legitimate seams (`commit.rs`'s non-forgeable `CommitPermit`, `reducer.rs`'s pure FSM), but:
- `aggregate.rs` holds the `WwdStatus`/`WwdDayType` state enums (`:22–110`) that the standard's own worked example places in `schema.rs`.
- `settlement.rs` is `runtime.rs` content; `terminal.rs` is `state.rs` content; `pre_admission.rs` mixes both in one file.
- `proof_layout.rs` (2 constants) belongs in `constants.rs`; `config.rs` (4 lines) is pure indirection.

**`ocomp/schema.rs` is not a schema — it is a re-export shim** (`ocomp/schema.rs:10–18`) behind which five different real modules hide (`authority`, `codec`, `index`, `profile`, `transitions`). `super::schema::X` resolves to five source files. Meanwhile `src/schema.rs` (374 lines) is the actual `MetadosisContract` schema. Name collisions compound this: `ocomp/schema.rs` vs `src/schema.rs`, `ocomp/state.rs` vs `src/state.rs`, `ocomp/test_support.rs` vs `src/test_support.rs` — disjoint content, identical names.

**`poc_schema_limits` is importable via three live paths** (`crate::ocomp::` via `mod.rs:25`, `crate::ocomp::schema::` via `schema.rs:17`, `crate::config::` via `config.rs:4`) and all three are used by different files (`aggregate.rs:14`, `api.rs:6`, `commands.rs:117`). Same for `ResponseDeadlineKey`. This prevents a reader from establishing a single canonical import and makes refactoring the surface a grep exercise.

**`ocomp/test_support.rs` (1,483 lines) is physically inside `ocomp/` but is not a member of the `ocomp` module** — it is mounted as `crate::fixture_kernel` via a `#[path]` override (`lib.rs:49–51`). Invisible to anyone reading `ocomp/mod.rs`; its `HashMap` usage escapes module-level review.

**Over-fragmentation:** `ocomp/authority.rs` is 38 lines, one function, one caller, re-exported through the shim. By contrast `ocomp/snapshot.rs` and `ocomp/views.rs` are clean, justified seams.

**Oversized/low-cohesion files:** `ocomp/transitions.rs` (709 lines, six unrelated commit flows, no shared abstraction), `ocomp/request.rs:130–338` (a single 208-line function), `aggregate.rs::validate_ocomp_index_equivalence` (`:391–521`, 131 lines, 13 exit points, reaching into four modules — the densest and hardest-to-review function in the crate), `commit.rs` (1,123 lines of which **52% is inline test code**, `:537–1123`).

**Missing module README.** The crate has 5 entrypoint kinds (precompile, cycle-lifecycle hooks, emission sink, late-settlement sink, verified-vote command) — a "complex" module under the repo tier model, which requires a README listing entrypoints and routing. Absent.

### Dependency Injection

The crate correctly follows the repo's `StorageHandle` DI model — no global mutable state, no `lazy_static`/`OnceCell`, facades scoped per call. Two exceptions:

- **A fault-injection hook is compiled into the consensus apply path.** `ocomp/activation.rs:433` unconditionally calls `inject_test_receipt_fault`, which under `cfg(test)` reads a `thread_local!` `Cell` (`ocomp/test_support.rs:102–104`) and *mutates receipts* immediately before `verify_receipts` (`activation.rs:435`). The production build compiles it to `let _ = (...)` — dead but present in the source of the certified-result path. The same coverage is achievable by injecting the fault at the fixture's owner-contract boundary instead of inside the production function.
- Module privacy is enforced by `compile_fail` doctests (`lib.rs:9–39`) — genuinely stronger discipline than typical. But see §5: half of those guarantees just lost their trybuild backing.

---

## 2) Error Handling: Debugging & Reliability Risk

**Two parallel error regimes, and the good one loses.**

- `errors.rs` defines `MetadosisError`: `thiserror`, `#[non_exhaustive]`, 15 structured variants, exhaustive `From<MetadosisError> for PrecompileError` with no wildcard (`errors.rs:69–93`). Textbook.
- `ocomp/state.rs:217–276` defines `JobFsmError`: 23 variants carrying real context (`RequestNotDue { due_height }`, `InvalidDeadline { request_height, deadline_height }`, …). Also textbook.
- **Neither is what the crate actually speaks.** `ocomp/` uses **zero** variants of `MetadosisError` — despite `errors.rs` carrying six OCOMP-specific variants (`:18–:55`). `JobFsmError` is flattened to a string at *every* storage boundary (`.map_err(|e| fatal(e.to_string()))` at `transitions.rs:122, 263, 328, 482`; `store.rs:205`; `request.rs:257`; …). The dominant currency is `PrecompileError::Fatal(String)`: ~60 inline sites in the top-level files (`aggregate.rs`, `commit.rs`, `lifecycle.rs`, `terminal.rs`, `settlement.rs`) and ~200 in `ocomp/`, with the private helper `fn fatal(...)` **copy-pasted verbatim into 13 files** (`activation.rs:637`, `codec.rs:345`, `store.rs:420`, `transitions.rs:707`, …). Callers cannot discriminate "capacity exhausted" from "storage corruption" without string matching; the `#[non_exhaustive]` enums guard nothing.

**Context destruction.** `aggregate.rs:364–368` collapses three distinct terminal-receipt failure modes into one message via `.map_err(|_| fatal("…invalid terminal receipt"))` — the specific upstream reason is discarded. Debugging a halted validator from that string is not possible. Similarly `state.rs:275–285` propagates eviction-loop errors that identify nothing about *which* evicted day failed.

**`Fatal` on caller-reachable ingress contradicts the crate's own documented rationale.** `errors.rs:73–75` asserts "No current variant has a production caller-controlled ingress." But `precompile.rs:173–175` returns `Fatal` for the published `submitLysisResult` selector — any external account can call it — and `precompile.rs:88–90` returns `Fatal` from a public view. Everywhere else `Fatal` means halt-the-node (`terminal Metadosis failure is fatal to the block hook`). `precompile.rs:56–60` shows the crate knows the correct pattern (bad caller input → `Revert`). Confirm the dispatch semantics; if `Fatal` from precompile dispatch propagates as a block-execution failure, these two sites are caller-reachable halt triggers.

**Silent swallowing — the oracle fallback is the worst instance.** `lifecycle.rs:426–433`: `day_type_pair_vwap(...)?.unwrap_or(U256::ZERO)` (twice). A missing oracle price becomes zero, `determine_day_type` (`:441–444`) turns zero into `WwdDayType::Red`, and `settlement.rs:61–64` divides both gratis demand and supply by `RED_DAY_REDUCTION_COEF = 8`. **An oracle outage silently cuts the day's economic allocation by 87.5%** — no event, no receipt field, indistinguishable from a genuinely red market. CLAUDE.md explicitly requires oracle fallback paths (`unwrap_or_default`, zero-VWAP) to be tested; no test covers this. `settlement.rs:299–306` has the same shape: oracle `None`/zero falls through to stored `current_vwap`, which defaults to `U256::ZERO` (`schema.rs:81`), and a zero entry price reaches `dispatch_auction_brief` unguarded.

**Silent outcome discards.** `ocomp/request.rs:68` drops `TerminalRequestOutcome` — a day stuck in `Deferred` is indistinguishable from a healthy no-op; `ocomp/expiry.rs:83` drops the bool saying whether a voting window opened. Neither emits an event or trace.

**Documentation of failure modes: zero.** Not a single `# Errors` section exists in the crate. Every public fn in `api.rs` (9) and `commands.rs` (10) returns `Result` undocumented. No `# Panics` sections either — including on the two functions that *can* panic (§3).

**Vestigial IIFE closures.** Six sites wrap a body in `(|| { ... })()` and immediately return the result with no rollback, cleanup, or mapping (`lifecycle.rs:297/344`, `:374/407`, `state.rs:228/253`, `emission_sink.rs:68/98`, `pre_admission.rs:223/248`, `:274/315`). They imply a transactional boundary that does not exist.

---

## 3) Type Safety & Correctness: Maintenance Deficits

### Panic surfaces on the consensus path (repo rule: forbidden)

1. **`aggregate.rs:178` and `:529` — `.expect("validated active order/record")`.** Traced: the invariant *does* hold today (`load_and_validate` guarantees `active_set ⊆ records.keys()` — `aggregate.rs:268–283`), but it is a comment, not a type. `load_and_validate` runs on the hottest path in the crate — twice per mutation (`commit.rs:90, :92`) plus `lifecycle.rs:55/198`, `api.rs:46`. There is no `catch_unwind` anywhere in the executor path; a deterministic panic here **halts every validator simultaneously** — a network-wide liveness failure, strictly worse than the `fatal(...)` this same file uses for 12 comparable corruption classes. Structural fix: store owned `WwdProjection` values in `active_order` (they are `Copy`, `aggregate.rs:119`) — deletes both `expect`s.
2. **`ocomp/state.rs:673` — `unreachable!("validated OCOMP FSM phase cardinality")`** in `projection()`, called ~25 times across `transitions.rs`/`store.rs`/`expiry.rs`/`request.rs` — while the sibling `phase()` (`:577–583`) already returns `Result` for the *identical* match. The panic is gratuitous; converting it is mechanical.
3. `reducer.rs:153` `unreachable!` and `:187` `.expect` — both provably guarded within the same function; low risk but still letter-violations of the repo rule.

### Silent value clamping

`ocomp/state.rs:651` — `u16::try_from(self.terminal.len()).unwrap_or(u16::MAX)` on a consensus-visible projection field. Downstream, `transitions.rs:363` and `expiry.rs:103–111` classify retry-vs-exhausted by comparing this value against the cap; a saturated `u16::MAX` selects the *retry* branch instead of *attempts-exhausted*. Currently masked by `validate()`, but the clamp is the wrong failure mode for a value that steers terminal transitions. Related inconsistency: `pre_admission.rs:85` uses `.unwrap_or(u16::MAX)` on `max_oracle_openings` while `pre_admission.rs:99` handles the identical conversion of the identical field with an explicit `Deferred(ArithmeticOverflow)` — two policies for one value, 14 lines apart.

### Encoding integrity

- **`RetirementOutcome` is encoded three ways:** `terminal.rs:312–327` (`encode_retirement`/`decode_retirement`), `precompile.rs:123–130` (ABI mapping), and **bare literals `1`/`2`** in event construction (`lifecycle.rs:337–340`, `:399–402`) that silently duplicate `terminal_retirement::NOT_PRESENT/REQUESTED` (`schema.rs:45–46`) with no compile-time link. Adding a variant updates two sites and corrupts the other two.
- **`Some(0)`/`None` alias in the wire format:** `codec.rs:129` encodes `deadline_height.unwrap_or(0)`; `codec.rs:309` decodes `(x != 0).then_some(x)`. Injective only by upstream invariant, not by construction.
- **Encode/decode asymmetry:** `encode_live_scheduler_index(&[])` succeeds (`codec.rs:150`), producing bytes `decode_live_scheduler_index` rejects (`codec.rs:188`). No live caller trips it today; one careless caller away from a write-then-unreadable brick.
- `decode_scheduler` omits the canonical round-trip check (`encode(decode(x)) == x`) that `profile.rs:229` and `fork.rs:177` both perform — the strongest guard, applied inconsistently across six codecs.

### Duplicated / dead consensus surface

- **`constants.rs:76 UTC_PLUS_14_OFFSET = 50_400`** duplicates `outbe_primitives::time::UTC_PLUS_14_OFFSET`; the local copy is dead (`lifecycle.rs:44` uses the primitives one). Two sources of truth for a constant that partitions the calendar — divergence is a silent consensus bug.
- **`schema.rs:234 active_wwd_count: Value<u16>`** — a storage slot never read or written in production, permanently occupying slot 11 and baked into `METADOSIS_STORAGE_LAYOUT_V1_HASH` (`proof_layout.rs:13`). Unremovable without fresh genesis.
- Dead public API: `api.rs:53 is_offering_day` (its doc claims TributeFactory uses it — **factually wrong**, zero callers), `api.rs:78`, `:88`, `:128`; `constants.rs:68–69` (test-only constants in the production API); `commit.rs:102 commit_emergency_fail` (26 lines of consensus-mutating code reachable only from tests); `ocomp/activation.rs:60–79` `OcompFinalizedIntentAuthority` trait + its error enum — no implementor, no caller.
- `ocomp/state.rs:642` — `if effect.effect_nonce != 0 || effect.effect_nonce > pending_nonce`: the second disjunct is unreachable by short-circuit and contradicts `state.rs:361`, which treats non-zero effect nonces as legal.

### Pattern matching, casts, misc

- FSM matching is exhaustive — `JobFsmCommand` dispatch (`state.rs:407–527`) has no wildcard; wildcards that exist are fail-closed value-domain fallbacks. Good.
- `as` casts: only enum-`#[repr(u8)]` discriminant reads (`activation.rs:498`, `fork.rs:131` — safe) plus `constants.rs:42/:44` narrowing `u64 as usize` on compile-time constants without the justification comment the repo rule requires (values 26 and 5 — safe in fact, non-compliant in letter). Everywhere else the crate correctly uses `try_from` (14+ sites).
- Asymmetric `PartialEq<u8>` on `WwdStatus`/`WwdDayType` (`aggregate.rs:70–74`, `:99–103`): `status == 5u8` compiles, `5u8 == status` does not — and the impls weaken the type safety the enums exist to provide.
- **Ambiguous positional `bool`s:** `commit.rs:513 commit_status(..., emit_status_change: bool)` — called with bare `true`/`false` literals at 5 sites; the `false` at `:466` is a semantically significant event-suppression expressed as an unlabelled literal. Also `commands.rs:158 is_static`, `settlement.rs:266 is_green`. The crate already has the right pattern (`reducer.rs:70–74` named struct field).
- Triplicated capacity constants reconciled only at runtime: `codec.rs:19 MAX_LIVE_JOBS = 2` vs `profile.rs:139 max_pending_jobs != 2` vs `constants.rs:61 MAX_RETAINED_WWDS = 2`, tied together by a runtime equality check at `authority.rs:28` instead of derivation.
- Overlapping reject-code namespaces: `activation.rs:38–45` and `vote.rs:29–32` both define codes `2` and `3` with different meanings (different selectors, so not a bug — but undocumented, with numeric gaps implying an external registry that is never referenced).

**Clean:** zero `unsafe`, zero `f32`/`f64`, no primitive obsession in economic values (`U256` throughout), `#[repr(u8)]` enums with validated `TryFrom`.

---

## 4) Async & Concurrency: Safety & Performance Risk

Not an async crate — no tokio, no tasks, no channels, no locks in production. Assessment reduces to determinism (the consensus equivalent of concurrency safety):

**Determinism: clean, verified exhaustively.** No `HashMap`/`HashSet` in production (all hits are in test support); all consensus indexes are sorted `Vec`s with strict-order enforcement validated on **both** encode and decode (`codec.rs:151/206/225–230`, `index.rs:118/179`, `:239/300`) — a hostile decoder cannot smuggle a differently-ordered index. No wall clock, no randomness; all time flows from `BlockContext`/`storage.block_number()`. Big-endian fixed-width wire format, no `usize` on the wire. Iteration orders are *total*, not merely stable (composite tiebreakers at `aggregate.rs:528–531`, uniqueness checks at `snapshot.rs:46–50`).

Two liveness-shaped risks:

1. **Exact-height coupling.** `transitions.rs:239–242` fatals with "consensus skipped the exact voting-open height" if `at_height > open_height`, and `vote.rs:278–280` similarly on deadline height. Any block where `run_lifecycle_begin` does not execute — e.g. the profile transiently reading `None`, which `expiry.rs:79–81` treats as a *silent* `Ok(())` — permanently bricks the open window. The failure path is explicit (fatal), but the skip path that causes it is silent.
2. **Snapshot-vs-mutation staleness in the advance loop.** `lifecycle.rs:55–115` computes `retained_count` once, then mutates storage inside the loop while feeding the pre-loop count to `reduce_outer_wwd` and `apply_capacity_forfeiture` on every iteration. Traced correct — but the correctness rests on a four-step argument spanning two files (`BecomeReady` immediately sets `admission_consumed`, steering the reducer away from a second increment) with **no comment stating it**. Fragile-but-correct; deserves either the comment or recomputation.

Test-only concurrency: the `thread_local!` fault-injection cell (§1) is take-once and safe under libtest/nextest process isolation, but leaks across scenarios if two ever run on one thread.

---

## 5) Testing: Barrier to Expansion & Refactoring

### The suite is broken at HEAD

`tests/external_mutation_surface.rs` references six `tests/ui/*.rs` compile-fail cases; commit `395c8fd8` deleted the entire `tests/ui/` directory (12 files) while keeping the harness. `cargo test -p outbe-metadosis` fails (trybuild ENOENT → `panic!("N of N tests failed")`), which reds the workspace `test` CI job and the `ocomp-poc.yml` gate. Either restore the 12 files from `2f4674fe` (the `.stderr` files are rustc-version-sensitive and may need regeneration under 1.96) or delete the harness deliberately.

**What was lost is not cosmetic.** The six cases enforced compile-time security guarantees: `CommitPermit` non-forgeability, the `MetadosisMutationPurpose` sealed-trait (no crate can forge a `CycleLifecycle` mutation frame), the `MetadosisMutationLease` non-existence pin, and `fixture_kernel`/`test_support::kernel` privacy under `test-utils` (the raw kernel can seed arbitrary predecessor state and inject receipt corruption). The 7 `compile_fail` doctests in `lib.rs:9–39` still cover the raw module surface, but **those five guarantees are now unguarded.**

### Coverage profile

~178 test fns. The strong part is genuinely strong: the crate's dominant oracle is a transactional-atomicity sweep — ≥20 `*_rolls_back_every_mutation_and_retries_exactly*` tests that enumerate every mutation index via `fail_after_mutation_at(n)` and assert byte-identical state+events rollback plus retry convergence. Test naming is disciplined and behavioral (`capacity_forfeiture_preserves_retained_work_and_replays_without_effects`). Isolation is clean: fresh `HashMapStorageProvider` per test, zero wall-clock/env/randomness in assertions, no `#[ignore]`.

**Critical gaps:**

1. **`ocomp/codec.rs` (347 lines) and `ocomp/index.rs` (332 lines) have zero direct tests — and are structurally untestable.** These are the hand-rolled consensus byte codecs (six wire formats, ~700 lines of manual byte work). `mod codec`/`mod index` are private with `pub(super)` items, so `src/tests/` cannot reach them. No round-trip test, no malformed-input corpus, no fuzz target (no `arbitrary` dep, no `fuzz/`). The malformed-input paths — the entire reason the decoders validate so carefully — are unexercised. Highest-value gap in the crate.
2. ~~`proof_layout.rs:8–10` claims a test pins the layout hash; it does not exist.~~ **Correction: false.** `src/tests/state.rs::test_storage_dsl_layout_slots` asserts `keccak256(METADOSIS_STORAGE_LAYOUT_V1_CANONICAL) == METADOSIS_STORAGE_LAYOUT_V1_HASH` in addition to the slot pins.
3. `settlement.rs` (336) and `terminal.rs` (327): zero direct references; reached only through `run_begin_block`. No unit test for `resolve_auction_entry_price` — the fn with the zero-price fallthrough (§2).
4. **The oracle zero-VWAP → Red fallback (§2) is untested**, in direct violation of the repo rule requiring explicit tests for oracle fallback paths.
5. `api.rs`: 8 of 10 public fns never referenced by any test. `precompile.rs`: 8 of ~10 selectors untested, including the always-`Fatal` `submitLysisResult` guard.
6. `tests/semantic_test_support.rs` is `#![cfg(feature = "test-utils")]` — **never runs in CI** (CI compiles it under `--all-features` clippy but no test job enables the feature).

### Property testing

- The real proptest (`src/tests/ocomp_request/p5_models.rs`, 8 tests) is well built — coverage-accumulator oracles asserting every command class was exercised — but runs only 64 cases from a **fixed ChaCha seed** (corpus never varies run-to-run), and **`proptest-regressions/` is empty and untracked**: any shrunk counterexample CI finds is discarded.
- `tests/ocomp_fsm_model.rs` is a hand-written reference-model parity test — but it is **not proptest**: a fixed-seed LCG, 512 fixed steps, no shrinking, no seed sweep. Worse, command generation reads `production.projection()` to choose the next command (`:310–311`) — if the SUT's projection is wrong, the generator steers away from the bug. The workspace already pins `proptest-state-machine`; this file is the textbook use case for it.

### Test organization

Split-brain: 11 files in `src/tests/` **plus** four inline `#[cfg(test)]` mods (879 lines) — `commit.rs` alone carries 587 inline test lines (52% of the file), overlapping `src/tests/state.rs` coverage of aggregate validation. `src/tests/lifecycle.rs` is a 3,306-line monolith. Naming convention is mixed: modern behavioral names alongside legacy `test_*` prefixes in the older halves.

---

## 6) CI/CD & Tooling: Process Gaps

What actually gates a PR (`.github/workflows/ci.yml`):

| Gate | State |
|---|---|
| `cargo fmt --check` | ✅ blocking |
| `clippy --all-targets --all-features -D warnings` | ✅ blocking (skippable on drafts) |
| Tests | ⚠️ run via `cargo llvm-cov --workspace` — **plain `cargo test` semantics; `cargo nextest` is never run in CI** despite being the repo's documented default runner (zero hits for `nextest` in workflows) |
| Coverage | ❌ **informational only** — `CodeCoverageSummary` with no `thresholds:`, no `fail_below_min`. No ratchet, no per-crate floor. VGV-style enforcement is absent entirely |
| `cargo deny` | ❌ `continue-on-error: true` ("promote after first clean run" — never promoted) |
| `cargo machete` | ❌ `continue-on-error` |
| `cargo audit` | ❌ not in the PR path at all (local mise task only) |
| `cargo vet` | ❌ prerelease-only; `supply-chain/audits.toml` is **4 lines** — near-zero first-party audits, everything rides on imported audit sets + exemptions |
| miri / localnet | opt-in via commit-message tags, non-blocking |

**Lint posture is stock.** `[workspace.lints]` sets only `unsafe_code = "deny"`, `warnings = "deny"`, `clippy::undocumented_unsafe_blocks = "deny"`. No `pedantic`, no `nursery`, no `rustfmt.toml`, no `clippy.toml` (so `cognitive_complexity` is off; `too_many_arguments` fires at default 8 — metadosis has exactly 3 `#[allow(clippy::too_many_arguments)]`, all in `transitions.rs`, its only allow-escapes). For a consensus codebase this is a thin floor; enterprise Rust practice is pedantic+nursery with curated allows.

**Conventions:** recent history is mostly Conventional Commits, but HEAD itself is `"update"` — which would fail any conventional-commit gate (none is configured). No changelog. Versioning is workspace-wide, not semver-per-crate (acceptable for a single-binary chain).

**Toolchain pinned** (`rust-toolchain.toml` → 1.96.0) — good. `Cargo.lock` committed — good.

The net effect: the only hard gates are fmt/clippy/test — and the test gate is currently red (§5), which means either PRs are merging around a red gate or the branch has not been exercised through CI.

---

## 7) API Design & Documentation: Onboarding Cost

**Public surface is tight and doctest-enforced** (`lib.rs:9–39` compile-fail blocks; private `mod ocomp` with a narrow `model`/`config` escape) — better than most runtime modules. Deficits:

- **Zero `# Errors` sections crate-wide; zero `# Panics` sections** — including on the two panicking functions. No `#![deny(missing_docs)]`; 6 of 10 `commands.rs` public fns and `api.rs:128` are entirely undocumented. Where docs exist they are often excellent (`schema.rs:7–13` base-slot rationale, `constants.rs:46–57` pipeline-cap derivation with the arithmetic spelled out) — the crate can clearly write good docs; it just doesn't do so consistently.
- **One doc "test" is defective:** `lib.rs:33–39` (`compile_fail` with `todo!()` args) fails at the `use` line — which duplicates the block at `lib.rs:9–11` — so lines 35–38 are never type-checked. If the two initializer methods were made `pub` tomorrow, the test would still pass, giving false assurance on exactly the property it exists to guard.
- **Stale doc:** `api.rs:52` claims `is_offering_day` is "used by TributeFactory" — it has zero callers anywhere. **Changelog-as-doc:** `runtime.rs:39–53` is 15 lines of git history ("Renamed from `run_begin_block` (Phase 5.1)…") masquerading as API documentation.
- `pub` vs `pub(crate)` is inconsistent *within single files*: `state.rs:164/:256/:260/:318` are `pub` while adjacent siblings (`:32`, `:48`, `:211`, …) are `pub(crate)`. Harmless today (the parent mod is private) but would trip `unreachable_pub` and signals the wrong intent.
- `#[must_use]` applied well in places (`reducer.rs`, `aggregate.rs` enums) but missing on non-`Result` constructors: `WwdProjection::from_record` (`aggregate.rs:138`), `TerminalReceiptValidationContext::new` (`terminal.rs:21`), `RequestBudgetSplit::derive` (`ocomp_budget.rs:34`).
- No crate README (§1).

---

## 8) Dependencies: Supply Chain & Bloat Risk

- **`Cargo.toml:16` — `outbe-primitives = { workspace = true, features = ["test-utils"] }` in `[dependencies]`.** Verified: `outbe-primitives`' `test-utils` feature is an **empty feature** gating nothing (`crates/blockchain/primitives/Cargo.toml:56`; zero `cfg(feature = "test-utils")` in that crate). So nothing actually ships — but the declaration is misleading, repeated cargo-cult-style across ~35 crates, and becomes a real leak the day someone puts content behind the feature. Delete the feature or the declaration.
- **The real test-code gate is sound.** `outbe-metadosis/test-utils` is enabled only from `[dev-dependencies]` in all four consumer crates (evm, node, cycle, rewards — verified section placement); under resolver 2 dev-dep features do not unify into the release binary. `src/test_support.rs` and the 1,483-line fixture kernel do **not** ship.
- **`k256` is declared twice** (optional in `[dependencies]` via `test-utils = ["dep:k256"]`, plus `[dev-dependencies]`). Correct but fragile: plain `cargo test` passing does not prove `cargo check --features test-utils` builds; the standalone-feature build is the only thing exercising the `dep:` activation.
- Dependency breadth is high (14 workspace crates) but consistent with the module's role as an orchestrator; all internal, no version-duplication risk introduced here. `thiserror` is the only external error dep — correct choice.
- Supply chain: `cargo-vet` configured but effectively outsourced (4-line `audits.toml`, §6); `cargo audit`/`deny` non-blocking. `Cargo.lock` committed; toolchain pinned. The `no-test-deps` CI job (grep for `arbitrary` in the no-dev tree + `check-consensus-deps.sh`) is a good, unusual control.

---

## 9) Performance & Resource Management: Production Readiness

### The headline defect: a global push-only vector enforced against a per-day cap → deterministic chain halt

`ocomp_terminal_intents` (`src/schema.rs:286`) is a single **global** `StorageVec<B256>` shared across all WorldwideDays. It is pushed on Expired (`transitions.rs:361`), Conflicted (`:513`), **and Completed** (`:673`), and **never pruned** — no `clear`/`pop`/`remove` exists in production code. Yet `store.rs:157–159` enforces the **per-WWD** limit `max_terminal_records` (pinned to 365 by `profile.rs:144`) against the *global* length, on every `ocomp_fsm_state()` call — which sits on the mandatory block paths (`request.rs:89` via `run_terminal_request`, `expiry.rs:37/82` via `run_lifecycle_begin`).

Consequences, in order of arrival:
1. At global length 365, `transitions.rs:340/:494/:577` reject any new terminal record for *any* day — a brand-new WWD's first completion is refused.
2. ~~At 366, `store.rs:158` returns `Fatal` on every FSM read for every day.~~ **Correction: unreachable** — every push site guards `>=` at 365 while the read guards `>`, so the vector saturates at exactly 365 and reads keep working. The real failure is consequence 1: every subsequent terminal transition Fatals inside the mandatory begin-zone, and the `vote.rs:275-277` exact-response-deadline coupling makes the first rolled-back phase a permanent per-block Fatal (hard fork + state surgery to recover). Fuse: `min(~365 days healthy, ~24–26k blocks with one stuck day exhausting its own 365-retry budget)`.
3. With `MAX_LIVE_JOBS = 2`, two concurrent days share one 365 budget — neither can reach its documented per-day retry allowance (cross-WWD budget theft) long before the cliff.
4. Latent adjacent bug: `store.rs:177–189`'s `_ =>` arm fatals on `Completed` entries; masked today only because completion clears the per-day FSM state (`transitions.rs:676`), so a re-enqueued completed day would fatal on first read.

This is not a style finding; it is a liveness bug with a calendar. Fix requires either per-WWD scoping of the vector, pruning on completion, or checking the per-day filtered count — plus a regression test that drives >365 global terminal records across ≥2 days.

### Amplified decode cost on the hot path

`ocomp_fsm_state()` linearly scans and **fully decodes** up to 365 `OcompJobRecordV1` per call (`store.rs:166–201`); `live_ocomp_fsm_states` multiplies by live jobs; `run_terminal_request` calls it again at `request.rs:254`. Net ≈ **1,460 canonical record decodes per block**, growing with history, uncached. `store.rs:235` additionally re-encodes both sides for a byte-equality equivalence check on every read. Bounded, but the bound is the halt bug above.

### Expensive public views

- `getWorldwideDaysByStatus` (`precompile.rs:55–63` → `state.rs:318–339`): a caller-supplied terminal status forces `read_all()` over 365 closed days **plus 365 per-record status reads** — ~730 storage reads per external call.
- `getWorldwideDayTerminalReceipt` (`precompile.rs:82–104`): `terminal_receipt_validation_context` materializes both index vectors (394 keys) *twice*, ~800 keys per view call, to compute two counts answerable by `contains`.
- `api::worldwide_day` — the crate's most-called cross-module read (TributeFactory, executor ×4) — materializes up to 394 keys for a single-day membership question.

### Bound enforcement asymmetry

`active_wwd` (cap 29) is defended three ways including load-time validation (`aggregate.rs:248–252`). `closed_wwd` (cap 365) is defended **only** by the write-time eviction loop (`state.rs:275–285`, single production call site); `load_and_validate` — which checks 20+ other invariants — never validates it. Four production `read_all()` sites (`api.rs:26`, `aggregate.rs:247`, `state.rs:332`, `terminal.rs:129`) depend on a bound that nothing enforces at load. One check next to `aggregate.rs:248` closes it.

### The good news

Arithmetic is **exemplary** — zero unchecked ops in economic paths: quotient/remainder decomposition to avoid overflowing intermediates with a documented rationale (`settlement.rs:42–57`), conservation assertions (`allocation + remainder == limit`, `settlement.rs:70–77`), round-trip re-verification (`ocomp_budget.rs:48`), `checked_*` throughout with one justified exception (`state.rs:606`, loop-index bounded). Allocation discipline is also strong: `try_reserve_exact` everywhere in the codecs, length derived (not trusted) from counts with `checked_mul`, trailing-byte rejection. Only `snapshot.rs:39` uses `with_capacity` sized from an upstream collection.

**Observability:** no `tracing` in the crate — acceptable for deterministic runtime code where events are the audit trail, but the silent paths (§2: oracle fallback, `TerminalRequestOutcome` discard, voting-window skip) emit *neither* events *nor* traces, which is the actual gap.

---

## 10) Refactoring Estimation & Summary

### Top 3 Risks

1. **Deterministic liveness failure with a ~1-year fuse:** the global `ocomp_terminal_intents` vector enforced against a per-day cap of 365 (`store.rs:157–161`, pushes at `transitions.rs:361/513/673`, no pruning) bricks block execution for every validator once 366 terminal records accumulate chain-wide — with completions counting toward it. Compounded by two panic surfaces on the same paths (`aggregate.rs:178/:529`, `ocomp/state.rs:673`) that would halt the network on any invariant slip, and the silent oracle-zero→Red fallback that cuts a day's allocation 8× with no signal.
2. **The compile-time security harness is broken at HEAD:** `tests/ui/` deleted, harness retained — `cargo test -p outbe-metadosis` and two CI workflows are red, and five guarantees (permit non-forgeability, sealed mutation purposes, fixture-kernel privacy) are currently unguarded. Simultaneously, the crate's ~700 lines of hand-rolled consensus byte codecs have zero direct tests and are structurally untestable at current visibility.
3. **Error-handling monoculture defeats the crate's own design:** two well-designed typed error enums exist but ~260 sites speak `PrecompileError::Fatal(String)` (with `fn fatal` duplicated in 13 files), context is destroyed at boundaries, `Fatal` is returned on caller-reachable ABI ingress contradicting `errors.rs:73–75`, and no failure mode is documented anywhere (`# Errors`: zero).

### Refactoring Scope

- **Task 1 (correctness):** scope `ocomp_terminal_intents` per-WWD (or prune/filter-count), fix `store.rs:188` Completed-arm, add a >365-records regression test; guard the oracle zero-VWAP path with an event + tests; convert the four panic sites to `Result` (structural fix for `aggregate.rs`: own `WwdProjection` in `active_order`); add the `closed_wwd` load-time bound check.
- **Task 2 (test integrity):** restore or deliberately remove the trybuild suite; add round-trip + malformed-input tests for `codec.rs`/`index.rs` (requires `pub(crate)` visibility or an in-module test mod); pin `METADOSIS_STORAGE_LAYOUT_V1_HASH` to a recomputation; migrate `ocomp_fsm_model.rs` to `proptest-state-machine`; track `proptest-regressions/`.
- **Task 3 (error redesign):** single `fatal` helper; route `ocomp/` through `JobFsmError`/`MetadosisError` variants instead of flattening to strings; reclassify caller-reachable `Fatal` → `Revert`; stop discarding `TerminalRequestOutcome`.
- **Task 4 (structure):** swap `lifecycle.rs`/`runtime.rs` content to match the repo standard (or rename per reality); dissolve the `ocomp/schema.rs` shim; move inline tests out of `commit.rs`; collapse the three `RetirementOutcome` encodings to one; delete dead API/constants; add the module README.
- **Task 5 (CI):** enforce a coverage threshold, promote `cargo deny` to blocking, run nextest (or accept llvm-cov semantics explicitly), add `cargo audit` to PR path.

### Time Estimates

**Minimal (Critical Gaps): 4–6 days.** Task 1 + the trybuild restoration + codec round-trip tests. These close the chain-halt bug, the panic surfaces, the red CI, and the untested consensus byte layout — the items with network-level blast radius.

**Comprehensive (Full Compliance): 3–4 weeks.** All five tasks, including the error-regime consolidation (~260 call sites, mechanical but wide), the structure swap (touches every file that imports `lifecycle`/`runtime`/`commands`), documentation (`# Errors` across ~20 public fns), and CI hardening. The structure swap and error consolidation should each be their own PR with the existing atomicity-sweep tests as the safety net — that test infrastructure is the crate's best asset and makes this refactor far cheaper than it would be elsewhere.

### Justification

The crate's foundations are unusually strong — exemplary checked arithmetic, verified determinism, a transactional-atomicity test oracle, doctest-enforced privacy — which is precisely why the remaining defects matter: they are concentrated, high-blast-radius exceptions (a dated liveness bomb, panic sites on the hot path, an unguarded security harness) rather than diffuse debt. Fixing Task 1 and 2 converts "correct until the calendar or an invariant slips" into "correct by construction and by test"; Tasks 3–4 cut the navigation and debugging tax that currently makes every incident a string-grep and every onboarding a three-file detour. Deferring the minimal set is not an option: item 1 has a fuse, and item 2 means the branch cannot merge green today.
