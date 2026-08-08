# Metadosis audit findings

Findings for `crates/core/metadosis`. Labels follow the documentation contract:
`bug` (confirmed bug), `tech_debt` (module-level debt), `arch_debt`
(architectural gap). Resolved entries stay recorded with their closure
reference.

## Resolved

### bug — OCOMP terminal-record cap enforced globally instead of per WorldwideDay

`ocomp_terminal_intents` was a single global push-only `StorageVec<B256>`
shared by every WorldwideDay, while the per-day profile cap
`max_terminal_job_records = 365` was compared against the global length on
every terminal transition and FSM read. Consequences: any day's terminal
events (including successful completions) consumed every other day's budget;
at global length 365 a fresh day's first Expire/Conflict/Complete was refused
with `Fatal("OCOMP terminal record cap exhausted")` inside the mandatory
begin-zone — a deterministic chain-wide halt with a fuse of
`min(~365 days healthy, ~24–26k blocks with one stuck day)`. The vector also
survived WorldwideDay retirement, so the growth was permanent.

**Closed** on `refactor/metadosis-2` (per-WWD terminal index): the index is a
sparse `Mapping<B256, B256>` keyed by
`keccak("OUTBE_OCOMP_TERMINAL_INDEX_V1" ‖ wwd_be ‖ index_be)` with a per-day
`Mapping<WorldwideDayKey, u16>` count as the sole length authority
(`src/ocomp/terminal_index.rs`); terminal transitions assert index↔FSM
lockstep before every push; `delete_worldwide_day_raw` deletes the day's index
with the day. Slot-neutral: the field keeps `order = 11` (slot 24), the count
is appended at slot 62, and `METADOSIS_STORAGE_LAYOUT_V1_HASH` is unchanged
(pinned by `src/tests/state.rs::test_storage_dsl_layout_slots`). Requires
fresh genesis: slot 24 bytes are reinterpreted. Regression proof:
`terminal_cap_is_per_worldwide_day_not_global` fails on pre-fix code
(`2f4674fe`) with exactly the cap-exhausted Fatal and passes after the fix.

### bug — `submitLysisResult` returned `Fatal` on caller-reachable ingress before OCOMP activation

With the OCOMP lifecycle inactive, the public `submitLysisResult(bytes)`
selector reached the Metadosis view dispatcher, which returned
`PrecompileError::Fatal`. A `Fatal` from precompile dispatch aborts the whole
payload build (`payload_builder`), and the transaction is neither
`mark_invalid`-ed nor filtered by the txpool — one cheap public transaction
produced a sustained network-wide proposer stall (the 2026-05-15 incident
class; this call site was missed by that fix).

**Closed** on `refactor/metadosis-2`: the arm returns the machine-readable
rejection `RevertBytes(OCOMP_RESULT_VOTE_REJECTED_SELECTOR ++ uint256(5))`
(`REJECT_LIFECYCLE_INACTIVE`) via the selector's existing `vote_reject` ABI.
Regression tests:
`submit_lysis_result_reverts_when_ocomp_lifecycle_is_inactive` (metadosis) and
`inactive_lysis_selector_does_not_abort_block_execution` (outbe-evm); e2e
scenario `metadosis_ingress.feature`.

## Open

### arch_debt — begin-zone exact-height coupling turns transient failures into permanent stalls

`src/ocomp/vote.rs` (`close_due_ocomp_response_window`) fatals with
`"OCOMP lifecycle skipped the exact response deadline"` whenever
`at_height > key.deadline_height`, and `src/ocomp/transitions.rs`
(`open_due_ocomp_voting`) does the same for the exact voting-open height. Any
transient begin-zone failure that rolls the phase back past one of these exact
heights therefore becomes a permanent deterministic failure on every
subsequent block — recoverable only by hard fork plus state surgery. This is
the amplifier that turned the global-cap bug from disruptive into
catastrophic.

**Resolved** on `fix/metadosis-fatal-recovery` (Beads `outbe-chain-e7p`):
begin-zone detects a missed
voting-open, response-deadline or awaiting-finality boundary before invoking
the exact-height transition. The affected WorldwideDay is atomically moved to
`FAILED`; its sealed Tribute partition is forfeited through the partition
retirement API; the full formed day limit (before request split) or retained
`lysis_budget` (after split) is credited once to PromiseLimit carry-over; and
only live OCOMP scheduler/index/FSM state is removed. Immutable request, job
and vote-accountability evidence remains queryable, with the live job closed
as `Canceled`. Storage and compressed-entity checkpoints make every mutation
failure retryable without partial effects. The executor now prepares
provisional CE roots, runs `OcompTerminalRequest` while the scope remains
active, and performs the sole committed CE seal after the terminal decision.
Persisted-state corruption remains fatal. Certified result activation uses the
same recovery boundary for deterministic owner/receipt failures: attempted
effects roll back, the exact retained `lysis_budget` is credited once, the live
job is retained as canceled evidence, and the WWD closes as `FAILED`. On the
successful path all owner receipts and the terminal capability are validated
before the final owner mutation, ordered `Nod -> Contributor -> CarryOver ->
Tribute`; only then is `COMPLETED` committed. Regression coverage is in
`src/tests/ocomp_request/fatal_recovery.rs`, the activation fault matrix, and
the production payload-builder OCOMP lifecycle test.
