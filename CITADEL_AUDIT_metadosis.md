> Auto-disclaimer: I verified every candidate against the actual source under `/Users/sakor/outbe-io/outbe-chain` (read `metadosis/{schema,runtime,metadosis,worldwideday,state,daily_accumulation,precompile}.rs`, `lysis/{runtime,algorithm,tests}.rs`, `desis/{runtime,api}.rs` + constants/schema, `rewards/late_settlement.rs`, `emissionlimit/block.rs`, `cycle/handler.rs`, `finalized_metadata_hook.rs`, the metadosis tests, and the module README; ran targeted greps). One candidate line-number was wrong — `desis-double-credit` writes are at `runtime.rs:228/231`, not 214/217 — corrected below. All 10 CONFIRMED findings hold. The 5 REJECTED are correctly rejected (spot-checked `genesis-utc14-key` and `lysis-hashmap-determinism`: both benign exactly as written).

# CITADEL Triage — `crates/core/metadosis`

**Verdict: `close-needs-fixes`.** The metadosis state machine, its own-code money conservation, determinism, and lifecycle retirement are solid and well-tested (42 tests; C1–C7/D1–D5/E1–E3 hardening landed). Metadosis-proper has **no money-conservation break in its own code** — its worst own findings are one *latent* state-inconsistency, two spec-mismatches, one structural dead slot, and one nit. The reason this is not a citadel: the settlement **surface it orchestrates** carries one **reachable-now chain-halt blocker** (`lysis` missing-oracle) and two latent money bugs (`desis`, `rewards`). The blocker is amplified by a metadosis design choice (treating *any* lysis `Err` as fatal corruption), so metadosis co-owns it. Every defect has a clean, local, minimal fix — no architectural rot — hence `close`, not `real-gaps`.

Confirmed: **10** (5 metadosis-scope, 5 neighbour-scope). Noise rate from the original loop matches the brief (~78% rejected).

---

## Part 1 — Ranked CONFIRMED findings

Ranking: blocker > logic-bug > state-inconsistency > determinism > invariant-gap > spec-mismatch > structural > nit.

### Neighbour-scope (lysis / desis / rewards — reached through metadosis settlement)

**N1 · `lysis-missing-oracle-fatal` — BLOCKER — reachable-now**
- **Where:** `lysis/src/runtime.rs:246-249` returns `Revert("nominal price is zero...")` when `vwap.max(max_scurve)==0`; called *unconditionally* for ISO 840 at `:77` and per-tribute at `:104`. The `?` propagates into `metadosis/src/metadosis.rs:203-214` (`on_run_lysis` treats **any** `Err` as "genuine corruption" and re-returns it) → `worldwideday.rs:300-306` fires `Fail` then propagates → begin-zone CycleTick system-tx reverts → the `FAILED` write **rolls back** → day stays `READY` → re-reverts every subsequent block.
- **Why reachable:** missing-price is *routine*, not corruption. The day-rate path treats missing VWAP gracefully (RED, `worldwideday.rs:340-369`); the entry-price path does **not**. A day with ≥1 tribute (user action) but no 840 WWD-VWAP and no active S-curve (early chain, feeder downtime, or a reference currency that got no quorum) reaches settlement and reverts. Day color is driven by a *different* pair (COEN/0xUSD), so even a RED day with tributes hits `:77`. Begin-zone runs before user txs, so an oracle vote cannot heal it → **permanent halt at that height.**
- **Untested/masked:** every passing lysis test pre-seeds 840 (`tests.rs:51-69`, `:483-489` write `worldwide_day_vwap_exists`+value); the missing-840 path is never exercised. The one "lysis failure propagates" lifecycle test forces a NOD-id collision, conflating genuine corruption with this routine case.
- **Repro:** open a WWD; offer 1 tribute (ref 840); provide no 840 oracle data; advance to READY; the CycleTick reverts and the day never leaves READY.
- **Fix:** `resolve_entry_price_minor -> Result<Option<U256>>` (`Ok(None)` on no-data, `Err` only on storage fault); drop the unconditional `:77` resolve; resolve lazily per tribute and `continue` on `None` (mirror `:95-98`). Corruption (issue_nod collision) stays fatal.
- **Test:** lysis unit `missing_reference_currency_price_skips_tribute_instead_of_reverting` (Ok/empty nods/tribute preserved) + metadosis liveness regression (`try_run_begin_block(..).is_ok()` && status `COMPLETED`).

**N2 · `desis-double-credit` — LOGIC-BUG — latent**
- **Where:** `desis/src/runtime.rs:228` writes `clearing_initiated=1` and `:230-231` writes `pending_supply_intex` **before** the fallible messenger `staticcall`/decode/`call` at `:233-260`; a no-code/reverting messenger fails the decode at `:245` *after* the writes. `api.rs:62-65,84-96` swallow the `Err` and return the **whole** supply; `metadosis.rs:224-226` keeps it via `set_total_unallocated`; no `CheckpointGuard` wraps the call (`worldwideday.rs:288`). Later `clear_auction` sees `clearing_initiated==1` (guard `:347` passes), issues intex, and re-credits unused at `:389-397`. Net: unused promis counted twice in PromisLimit; issued promis both spent and retained. **Money-conservation break on the failure branch only.**
- **Reachability:** latent — needs the messenger to fail on the clearing quote while succeeding on start/reveal, plus subsequent bid relay + clear. Success path conserves.
- **Fix:** move the two persistent writes to **after** the messenger calls succeed (no input dependency); additionally reset both fields in `api.rs::clearing_failed`.
- **Test:** desis unit asserting `clearing_initiated==0 && pending_supply_intex==0` after a failed begin_clearing; metadosis e2e asserting PromisLimit unchanged after `clear_auction` on a failed clearing; conservation proptest.

**N3 · `rewards-late-settlement-ts` — LOGIC-BUG — reachable-now**
- **Where:** `rewards/src/late_settlement.rs:219-223` calls `dispatch_terminal_remainder_at(ctx, residue, ctx.block.timestamp)` — block N+K's timestamp. The contract it violates is in `emissionlimit/src/block.rs:29-34` ("must use the finalized/previous-day timestamp"). The bucket is derived at `daily_accumulation.rs:21` (`WorldwideDay::from_timestamp(ctx.block.timestamp)`), so residue lands in **day N+K's** bucket. The sibling caller is correct (`cycle/handler.rs:165-168` passes `date_key_to_utc_timestamp(prev_day)`). The finalized timestamp is available but dropped — `finalized_metadata_hook.rs:96` computes `fb_day`, but `escrow_block_fee` (`:77-117`) never stores it.
- **Impact:** money conserved (parity `Σpayout+residue==pending` at `:226-235`), but **wrong UTC-day attribution** of recycled headroom whenever the K=3 span straddles UTC midnight and residue≠0 (residue is non-zero on essentially every settlement). `set_metadosis_limit` is an overwrite, so it can also clobber the same-day Cycle terminal value. Deterministic, so not a consensus-divergence bug — a recurring per-day misbucket.
- **Fix:** add `pending_fb_day_ts_at: Mapping<u64,u64>`; store the finalized UTC-day anchor at escrow (`finalized.timestamp` is in scope at `finalized_metadata_hook.rs:132`); forward it through `settle_matured -> settle_window -> dispatch_terminal_remainder_at`; clear on settle.
- **Test:** midnight-straddling regression (`T_fb=23:59:59` day D, `T_current=00:00:02` day D+1) asserting the bucket is day D; same-day control; `(T_fb,T_current)` proptest.

**N4 · `lysis-partial-coverage-strands` — STATE-INCONSISTENCY — reachable-now (dust)**
- **Where:** `lysis/src/runtime.rs:95-98` skips a tribute when `gratis_load.is_zero()`; `:142-150` burns only processed ids and clears the day-index only if *all* were processed; the comment at `:143` ("Skipped tributes are preserved for potential reprocessing") is **false**. `algorithm.rs:258-262` clamps group fractions to zero, and a dust tribute (`nominal * fraction / SCALE` floors to 0) deterministically yields `gratis_load==0`. The day retires terminal exactly once (`worldwideday.rs:144-145`; `state.rs:110-126` deletes only the metadosis record, never the tribute day-index), and a COMPLETED day is never READY again, so lysis never re-runs → the skipped tribute is stranded in the day-index forever.
- **Caveat (honest):** the live arm is `gratis_load.is_zero()`; the `> remaining` arm is effectively dead (normalization guarantees `Σloads<=allocation`). I did **not** pin an exact multi-group numeric input that clamps a fraction to zero; the dust path is the concrete reachable case.
- **Fix:** the day can't be re-lysed, so "preserve" is unimplementable. Either (A) burn *all* day tributes + always `clear_day_index`, or (B) real carry-forward; delete the false comment. (A) is minimal but **silently drops** a no-gratis tribute — confirm against the lysis spec.
- **Test:** dust/multi-group day → assert `nod_ids.len()<tribute_count` and `get_all_day_tributes(wwd).is_empty()` (fails today); proptest: index empty after any settled lysis.

**N5 · `promis-u128-u32-lockup` — INVARIANT-GAP — latent**
- **Where:** `desis/src/runtime.rs:223-225` `u32::try_from(supply_promis / promis_load_minor)` → `Err` on overflow; `api.rs:84-96` returns the **whole** supply (not a remainder). `promis_load_minor = PROMIS_LOAD(100_000, constants.rs:18) * 1e18 = 1e23` (schema.rs:83) → binding ceiling at `supply_promis > u32::MAX*1e23 ≈ 4.29e32` minor PROMIS. **Not silent** (emits `AuctionDispatchFailed`), **not fund-loss** (conserved back to PromisLimit). Residual: no cap/carry-forward, so once the un-auctioned accumulator crosses the ceiling, every future clearing fails identically — **permanent liveness stall**.
- **Fix:** cap at `u32::MAX` and carry the excess whole units back via the remainder (behavior-preserving below the ceiling). Re-check the BNB-side `IntexAuction` can accept the capped count; otherwise use a documented protocol cap.
- **Test:** `begin_clearing_caps_supply_at_u32_and_carries_remainder`; conservation proptest across the ceiling.

### Metadosis-scope

**M1 · `multiwindow-reveal-skip` — STATE-INCONSISTENCY — latent**
- **Where:** `worldwideday.rs:257-264` forward walk fires only `advance_event(state)`, and `advance_event(Offering)=CloseOffering` (`:171-181`) — never `RevealOffering`. Reveal exists only as the `target==start==Offering` self-loop (`:237-246`). A single tick that *enters and leaves* Offering (e.g. `Forming/LookbackDelay → Waiting/Ready` in one walk) never reveals. The GREEN `desis` auction stays `Started`; settlement's `begin_clearing` requires `Revealing` (`desis/runtime.rs:216`) → fails → `api.rs:84-96` returns the whole supply → `metadosis.rs:224-226` parks it in PROMIS — **yet the day reaches COMPLETED** (cross-module divergence).
- **Reachability (honest):** in steady state `advance_worldwide_day` runs once per UTC day; the 50h/48h Offering window always spans ≥2 daily ticks, so the middle same-state tick reveals (the codebase's own `intex_reveal_dispatched_on_mid_offering_tick`, `tests/lifecycle.rs:640+`). The skip manifests only on a **missed UTC day / chain halt / forward-ts jump >~50h** that crosses the whole window in one tick; `cycle` does not catch up missed days. The candidate's "single tick Forming→Waiting" overstates steady-state. **Uncaught:** `test_normal_lifecycle_never_leaves_ready_day_type_unknown` (`:302-337`) already walks the 62h jump but asserts only `day_type`.
- **Fix:** fire `RevealOffering` once when leaving Offering before `CloseOffering` (best-effort reveal on an already-revealed auction is a harmless no-op; no re-entry after `CloseOffering`).
- **Test:** single-tick-jump test asserting `clearing_initiated==1` and PROMIS < day_limit; proptest-state-machine over arbitrary tick gaps: no GREEN day reaches COMPLETED with its auction still in `Started`.

**M2 · `skipped-vs-failed` — SPEC-MISMATCH — reachable-now (cosmetic)**
- **Where:** `metadosis.rs:152-157` emits `MetadosisSkipped{status:"SKIPPED"}` in `on_reject_zero_limit` (→ `Aborted`), but `worldwideday.rs:293-296` maps `Aborted→Fail→mark_wwd_failed` and `state.rs:103` persists `Status::Failed`. There is no `SKIPPED` status in `schema.rs`. The sibling `on_reject_unknown` (`:160-165`) is FAILED-consistent, so zero-limit is the lone outlier. `tests/lifecycle.rs:395-414` asserts persisted `FAILED` but never the event `status` field, leaving the divergence unguarded. No money impact (`dispatch_auction_clearing` called with `ZERO`).
- **Fix:** `status: Status::Failed.label().into()` (one-line, no schema change), keeping the `MetadosisSkipped` event name + reason.
- **Test:** extend the zero-limit test to decode the log and assert `ev.status=="FAILED"`.

**M3 · `bootstrap-end-time-no-gate` — SPEC-MISMATCH — latent (devnet/testnet only)**
- **Where:** `runtime.rs:19-28` `effective_hours` selects bootstrap-vs-normal purely by `chain::is_devnet/is_testnet(chain_id)`; it never reads `bootstrap_end_time`. The field is written only on block 1 (`:147-148`, self-guarded `:145`) and read only by the RPC getter (`precompile.rs:52-53`). `effective_hours` is called per WWD creation (`:185`), so devnet/testnet days created *after* the 504h window still get bootstrap hours (lookback 0, offering 48h) — the accelerated schedule never expires; the field bounds nothing. Mainnet never sets it, so no mainnet economic impact.
- **Fix:** either gate `effective_hours` on the stored boundary (take `&BlockRuntimeContext`, compare `ctx.block.timestamp < end`) **or** delete the field + RPC. The current "neither" is the defect.
- **Test:** devnet WWD before/after the 504h boundary asserting bootstrap vs normal windows.

**M4 · `active-wwd-count-orphan` — STRUCTURAL — unreachable**
- **Where:** `schema.rs:185` declares `active_wwd_count` (order=2, slot 11); the only other repo hit is the layout assertion `tests/state.rs:279`. Membership is tracked solely by the `active_wwd` Set (`state.rs:130-137`), which carries its own length slot. A never-read counter cannot cause an observable inconsistency — **dead slot, not a behavioral bug**.
- **Fix (pre-mainnet):** delete the field, renumber `active_wwd 3→2`, `closed_wwd 4→3`, re-pin the layout test.

**M5 · `compute-allocation-raw-sub` — NIT — unreachable**
- **Where:** `metadosis.rs:67` `wwd_metadosis_limit - allocation` is raw `Sub`; `allocation=min(demand,supply)<=supply<=limit`, so underflow is structurally impossible, but the repo Economics rule wants checked/saturating-or-comment, and siblings comply (`:199-201`, `:237`). alloy `U256 Sub` panics on underflow — unreachable here.
- **Fix:** `saturating_sub` + why-impossible comment; allocation-invariant proptest.

---

## Part 2 — Invariant citadel status (I1–I7)

| Inv | Statement | Status | Reason |
|---|---|---|---|
| **I1** | **Money conservation** — every PROMIS/gratis/emission unit is allocated, returned to PromisLimit, or burned-with-parity; no create/destroy without a matching counter. | **VIOLATED (latent)** | Happy paths tested & conserve (no-tributes returns full limit; rewards parity `Σpayout+residue==pending` at `late_settlement.rs:226`). But the `desis-double-credit` failure branch (N2) double-credits PromisLimit, and that branch is **untested**. `compute-allocation-raw-sub` (M5) is a latent guard-style nit on the same surface. |
| **I2** | **Terminal finality & index drain** — every active day reaches exactly one terminal, leaves `active_wwd`, and its tribute day-index is fully drained. | **VIOLATED (reachable-now, dust)** | `lysis-partial-coverage-strands` (N4): a zero-gratis tribute is left unburned in the day-index, the day retires COMPLETED, and is never re-lysed. Day-record retirement itself holds; the *index* drain does not. |
| **I3** | **Liveness / no silent stall** — settlement of a READY day always completes the block; auction stages always progress (Started→…→Cleared) without stranding. | **VIOLATED (1 reachable-now + 2 latent)** | `lysis-missing-oracle-fatal` (N1, reachable-now permanent halt); `multiwindow-reveal-skip` (M1, latent auction strand on missed-day); `promis-u128-u32-lockup` (N5, latent permanent clearing stall). Directly contradicts the repo rule "silent stalls are not acceptable." |
| **I4** | **Determinism across proposer/validator** — all settlement transitions byte-for-byte deterministic (no wall-clock, no HashMap iteration order, no float). | **HOLDS** | All money math is fixed-point integer (`algorithm.rs` U256/I256; no f32/f64 in production paths). Timestamps come from `BlockContext`/`ctx.block.timestamp`, never `SystemTime`. FI grouping uses `BTreeMap` (`lysis/runtime.rs:173`). The one `HashMap` (`fi_fraction_map`) is **key-lookup-only** (`runtime.rs:90`), never iterated/encoded → benign (rejected `lysis-hashmap-determinism`); flag as a hygiene nit vs the "no HashMap on consensus paths" rule, not a determinism break. |
| **I5** | **Status/event & enforcer consistency** — persisted on-chain status equals what events report; FSM edge and storage guard agree. | **VIOLATED (cosmetic) + sub-invariant HOLDS** | `skipped-vs-failed` (M2): event string "SKIPPED" ≠ persisted FAILED (no state/money impact). The FSM-edge-vs-guard agreement sub-invariant **holds** (rejected `ready-inprogress-double-enforce` and `mark-wwd-failed-permissive` both confirm the FSM is the real gate; guards are redundant, not contradictory). |
| **I6** | **Day-bucket attribution** — emission/residue credited to a UTC day lands in *that* day's Metadosis bucket. | **VIOLATED (reachable-now, recurring)** | `rewards-late-settlement-ts` (N3): residue anchored at dispatching block N+K, not finalized day N; misbuckets on every midnight-straddling K-window with non-zero residue. |
| **I7** | **Schema integrity** — every declared slot is live and the pinned layout matches; no orphaned/dead slots. | **VIOLATED (structural, unreachable)** | `active-wwd-count-orphan` (M4): dead slot 11. Rest of the layout is pinned & tested; no off-by-one elsewhere. |

Net: **I4 holds; I5 effectively holds (cosmetic event-string only); I1/I2/I3/I6/I7 violated** — but I7 is dead-code, I5/I1(M5) are cosmetic/latent, I2/I3(M1)/N5 are latent, and only **N1 (I3) and N3 (I6) are reachable-now operational defects.**

---

## Part 3 — Prioritised fix + test execution plan

**P0 — reachable-now, fix immediately**
1. **N1 `lysis-missing-oracle-fatal`** (chain-halt). Fix `resolve_entry_price_minor -> Result<Option>` + lazy per-tribute skip. **cargo-mutants** on `resolve_entry_price_minor` and `on_run_lysis`'s error arm; **llvm-cov** to confirm the missing-840 branch (currently 0%) is now covered. This is the single change that moves I3 from reachable-broken to latent.
2. **N3 `rewards-late-settlement-ts`** (recurring money-misbucket). Add `pending_fb_day_ts_at`, thread the finalized day. **cargo-mutants** must confirm a mutant that swaps the finalized-ts back to `ctx.block.timestamp` is killed by the midnight-straddle test.
3. **M2 `skipped-vs-failed`** (one-line string; cheap, closes I5 cosmetic gap). Add the event-status assertion.

**P1 — latent money/liveness**
4. **N2 `desis-double-credit`** — reorder writes after messenger success + reset in `clearing_failed`. **proptest** conservation over messenger-failure-injected clearings; **llvm-cov** on the partial-begin_clearing failure branch (currently 0%).
5. **M1 `multiwindow-reveal-skip`** — fire `RevealOffering` on leaving Offering. **proptest-state-machine** over arbitrary tick-gap sequences: invariant "no GREEN day reaches COMPLETED with auction in Started."
6. **N5 `promis-u128-u32-lockup`** — cap + carry-forward. **proptest** money-conservation across the u32 ceiling.
7. **N4 `lysis-partial-coverage-strands`** — **product decision first** (consume-all vs carry-forward), then fix burn/clear + delete the false comment. **proptest**: day-index empty after any settled lysis.

**P2 — structural/nit, no behavioral impact**
8. **M4 `active-wwd-count-orphan`** — delete slot, renumber, re-pin layout test.
9. **M3 `bootstrap-end-time-no-gate`** — decide gate-or-delete; align test + README.
10. **M5 `compute-allocation-raw-sub`** — `saturating_sub` + comment + allocation-invariant **proptest**.

**Where heavier tooling earns its keep:** proptest/state-machine for M1, N4, N5, N2, N3, M5 (all have a clean algebraic or sequence invariant); `cargo-mutants` specifically on N1/N3 (to prove the new tests *kill* the regression, not just pass); `llvm-cov` to confirm the two currently-0% failure branches (N1 missing-840, N2 partial-clearing) become covered.

---

## Part 4 — Ready-to-file task specs (metadosis-scope)

### TASK M1 — Fire `RevealOffering` when a single tick jumps the whole Offering window
- **Source:** CITADEL triage `multiwindow-reveal-skip`; `worldwideday.rs:257-264`.
- **Problem:** the forward walk fires only `advance_event`, which maps `Offering→CloseOffering`, never `RevealOffering` (reveal exists only as the same-state self-loop `:237-246`). A tick crossing the entire Offering window (missed UTC day / halt / >~50h ts jump) closes Offering without revealing → GREEN `desis` auction stays `Started` → `begin_clearing` require_stage(Revealing) fails → whole supply parked in PROMIS while the day still reports COMPLETED.
- **Invariant:** I3 (no auction strand) + cross-module: a GREEN day reaching COMPLETED implies its auction was revealed-then-cleared.
- **Proposed fix:** in the walk loop, `match *day.state() { Offering => { day.process_event(RevealOffering)?; CloseOffering } s => advance_event(s).ok_or(stalled)? }`. Redundant reveal on an already-revealed auction is a harmless best-effort no-op; after `CloseOffering` the state is no longer Offering (no loop).
- **Acceptance:** a single `run_begin_block` jumping ≥62h from offering-entry reveals and clears the auction; no infinite loop; existing steady-state reveal test still green.
- **Tests:** `reveal_dispatched_when_single_tick_jumps_past_offering` (assert `clearing_initiated[series]==1`, PROMIS<day_limit); proptest-state-machine over tick gaps.
- **Files:** `crates/core/metadosis/src/worldwideday.rs`; `crates/core/metadosis/src/tests/lifecycle.rs`.

### TASK M2 — Align `MetadosisSkipped.status` with persisted FAILED on zero-limit
- **Source:** `skipped-vs-failed`; `metadosis.rs:152-157`.
- **Problem:** zero-limit emits `status:"SKIPPED"` but persists `Status::Failed` (no SKIPPED status exists); the sibling unknown-day branch is FAILED-consistent; the test asserts only the persisted status, not the event.
- **Invariant:** I5 (event status == persisted status).
- **Proposed fix:** `status: Status::Failed.label().into()` (keep event name + reason). No schema change. (A distinct SKIPPED enum is only warranted if product says zero-limit is a non-failure skip — current test name/assertion say otherwise.)
- **Acceptance:** for a zero-limit READY day, the `MetadosisSkipped` log `status=="FAILED"` and `get_wwd_status==FAILED`.
- **Tests:** extend `tests/lifecycle.rs::test_ready_processing_zero_limit_fails` to decode the log and assert `ev.status=="FAILED"`.
- **Files:** `crates/core/metadosis/src/metadosis.rs`; `crates/core/metadosis/src/tests/lifecycle.rs`.

### TASK M3 — Make `bootstrap_end_time` consistent with `effective_hours` (gate or delete)
- **Source:** `bootstrap-end-time-no-gate`; `runtime.rs:19-28`, `:145-148`, `precompile.rs:52-53`.
- **Problem:** the field is written + RPC-exposed but never gates schedule selection; `effective_hours` keys solely off `chain_id`, so the devnet/testnet accelerated schedule never expires.
- **Invariant:** spec/documentation contract — a temporally-named, RPC-surfaced field must bound the schedule it names, or not exist.
- **Proposed fix:** (A) gate `effective_hours` on `(is_devnet||is_testnet) && end!=0 && ctx.block.timestamp < end` (take `&BlockRuntimeContext`; update the `:185` caller, test, README), **or** (B) delete `bootstrap_end_time` + setter/getter + `getBootstrapEndTime` RPC. Pick per product intent.
- **Acceptance (option A):** devnet WWD before the boundary uses bootstrap hours; after the boundary uses normal hours.
- **Tests:** devnet before/after-boundary window test; update `tests/state.rs` selection test.
- **Files:** `crates/core/metadosis/src/{runtime.rs,precompile.rs,state.rs,schema.rs}`; tests; README + the matching `audit_*.md` deviation if A keeps README-ahead.

### TASK M4 — Remove orphaned `active_wwd_count` schema slot
- **Source:** `active-wwd-count-orphan`; `schema.rs:185`, `tests/state.rs:279`.
- **Problem:** declared at slot 11, never read/written; membership lives in the `active_wwd` Set. Dead slot occupying pinned layout.
- **Invariant:** I7 (every declared slot is live).
- **Proposed fix (pre-mainnet):** delete the field; renumber `active_wwd 3→2`, `closed_wwd 4→3`; re-pin the layout test (drop slot-11 assertion, shift subsequent slots down by one). If post-genesis compat were in force, instead `deprecated=true` to reserve slot 11.
- **Acceptance:** `test_storage_dsl_layout_slots` passes with the new contiguous layout; no other references break.
- **Tests:** re-pinned `tests/state.rs` layout assertions.
- **Files:** `crates/core/metadosis/src/schema.rs`; `crates/core/metadosis/src/tests/state.rs`.

### TASK M5 — Use `saturating_sub` in `compute_allocation`
- **Source:** `compute-allocation-raw-sub`; `metadosis.rs:67`.
- **Problem:** raw `U256 -` on the consensus money path with neither a saturating/checked op nor a why-impossible comment (siblings at `:199-201`/`:237` comply).
- **Invariant:** I1 hygiene — money math is checked/saturating or proven-safe; `allocation<=supply<=limit`.
- **Proposed fix:** `wwd_metadosis_limit.saturating_sub(allocation)` + one-line comment.
- **Acceptance:** behavior unchanged on existing unit tests; clippy clean.
- **Tests:** proptest over `(total, limit, day_type)` asserting `gratis_allocation<=limit` and `gratis_allocation+remainder==limit`.
- **Files:** `crates/core/metadosis/src/metadosis.rs`.

---

## Part 5 — Honest assessment

Metadosis-the-module is **close to a citadel and clearly better than its neighbours.** Its own code has no money-conservation break, its determinism holds (fixed-point throughout, `BlockContext` time, `BTreeMap` ordering — the lone `HashMap` is key-lookup-only and harmless), its FSMs are real gates with redundant-but-consistent storage guards, and terminal-day retirement is bounded and tested. Of its five own findings, four are cosmetic/structural/nit (event-string, dead slot, ungated field, raw-sub) and the fifth (reveal-skip) is a *latent* strand that only bites on a missed UTC day. The **real residual risk lives at the boundary, not the core**: a routine missing 840 oracle price causes a permanent chain halt — and metadosis is complicit because `on_run_lysis` collapses *every* lysis `Err` into "fatal corruption + propagate," so it cannot distinguish "no data" (degrade gracefully) from "corruption" (halt). That single design seam, plus the two latent neighbour money bugs (desis double-credit on messenger failure, rewards midnight-misbucket), is what stands between this surface and citadel status. None requires re-architecture; the blocker fix is one helper signature change, and the rest are local. Fix N1 + N3 first (the only reachable-now operational defects), harden the failure branches with mutation/coverage proof, and metadosis graduates from "well-built core with a sharp hole at the lysis boundary" to a defensible citadel.