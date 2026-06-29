# `outbe-metadosis`

WorldwideDay lifecycle + Metadosis settlement: the daily clock that walks each
WorldwideDay through its phases and settles the day's metadosis limit (lysis →
auction clearing → terminal) once it is credited.

## Tier: Complex

This is a **complex** runtime module (per `.ruler/module_structure.md`), because it
has **multiple entrypoint kinds** *and* is the central orchestrator of one
use-case across 5+ neighbouring modules:

- `precompile` — the `IMetadosis` view ABI.
- `lifecycle`/runtime entrypoints — the daily orchestration + genesis bootstrap.
- `sink` — the daily emission-limit handoff (`daily_accumulation`).

## Entrypoint kinds and where they route

| Kind | Entry | Caller | Routes to |
|---|---|---|---|
| precompile | `precompile::dispatch` | EVM precompile router (`blockchain/evm`) | view reads of `MetadosisContract` (active days, by-status, bootstrap end) |
| lifecycle (daily) | `runtime::start_metadosis` | `outbe_cycle::handler` (UTC midnight) | bootstrap (block 1) → create today's day → `worldwideday::advance_worldwide_day` per active day → `worldwideday::process_metadosis` for the READY day |
| lifecycle (genesis) | `runtime::init_genesis_day` | `outbe_cycle::lifecycle` (block 1) | testnet/devnet bootstrap window + first day |
| sink | `daily_accumulation::apply` | `outbe_emissionlimit::block` | ensures the day exists, records the terminal allocation via `set_metadosis_limit`, emits `MetadosisAccumulation` |

## Cross-module dependencies

All cross-module calls go through neighbours' **public runtime calls / `api`**
(this module exposes no `api.rs` — its surface is the public modules below):

- **Outbound** (this module calls): `outbe_tribute` (seal/unseal/day totals),
  `outbe_desis` (auction stage dispatch/clearing), `outbe_oracle` (forming-window
  VWAP snapshots), `outbe_lysis` (gratis allocation), `outbe_promislimit`
  (terminal remainder routing).
- **Inbound** (callers): `outbe_cycle` (`start_metadosis`, `init_genesis_day`),
  `outbe_emissionlimit` (`daily_accumulation::apply`), `blockchain/evm`
  (`precompile::dispatch`, `constants`), `outbe_tributefactory` (`get_wwd_status`
  + `schema`), and the `e2e` crate.

## Structure (non-obvious bits)

- **Two composed `smlang` FSMs.** `worldwideday.rs` drives the clock walk
  (`Forming → LookbackDelay → Offering → Waiting → Ready`) plus the settlement
  coupling (`Ready → InProgress → Completed/Failed`); `metadosis.rs` drives
  settlement (`Pending → {Aborted | Cleared | Lysed} → Cleared → Settled`).
  Both machine contexts hold the scoped `BlockRuntimeContext<'storage>` **directly**
  — **no thread-local, no `unsafe`** — and the WorldwideDay machine is rebuilt
  each block from the persisted `u8` status via `new_with_state`.
- **Single-owner status/day-type types.** `schema.rs` owns the `Status` and
  `DayType` enums (with `label`/`TryFrom`); the `status`/`day_type` `u8` constant
  modules are *derived* from them and exist for storage defaults + the cross-crate
  comparison surface.
- **One status writer.** Every status transition (clock + settlement) routes its
  write through `MetadosisContract::write_status`.
- **Day artifacts vs settlement.** `daily_accumulation` only *records* the daily
  limit; it does not compute it (that is `outbe_emissionlimit`'s job).

## Files

| File | Role |
|---|---|
| `schema.rs` | storage schema/layout + the `Status`/`DayType` types (single owners) |
| `state.rs` | local storage CRUD/helpers on `MetadosisContract` (`write_status`, `mark_wwd_*`, getters) |
| `runtime.rs` | orchestration (`start_metadosis` phases, genesis, time helpers) |
| `worldwideday.rs` | WorldwideDayLifecycle FSM + clock driver + settlement composition + day-rate effects |
| `metadosis.rs` | Metadosis settlement FSM + the pure `compute_allocation` formula |
| `precompile.rs` | `IMetadosis` ABI decode/dispatch/encode |
| `daily_accumulation.rs` | the emission-limit sink (`apply`) |
| `constants.rs`, `errors.rs` | module constants / error types |

**Public surface** (re-exported from `lib.rs`): `schema`, `runtime`,
`daily_accumulation`, `precompile`, `constants`. The FSMs, local state helpers,
and errors (`metadosis`, `worldwideday`, `state`, `errors`) are crate-internal —
their `pub` methods on the `schema` type `MetadosisContract` stay reachable, but
the module paths are not part of the public API.
