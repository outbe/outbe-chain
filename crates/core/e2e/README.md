# outbe-e2e

Cross-module end-to-end tests for Outbe runtime flows live in this crate.

This crate exists so broad integration scenarios do not force feature/test-only dependencies back into individual runtime modules such as `outbe-metadosis`.

## Current coverage

### `tests/wwd_lysis_nod_gratis.rs`

One lifecycle-driven scenario covers two WWDs in sequence. Each tick runs the full Outbe pre-execution hook chain (`outbe_evm::executor::run_outbe_pre_execution_hooks`) in the same order as `OutbeBlockExecutor::apply_pre_execution_changes`: genesis validation (skipped), `EmissionLimitLifecycle`, validator-set epoch boundary, `MetadosisLifecycle`, staking unbonding, and `OracleLifecycle` tally/S-curve processing. Oracle slash-window penalties run after begin-zone system phases and before user transactions. Day-metadosis-limit is additionally pumped via an explicit `outbe_metadosis::daily_accumulation::apply` call per day so the tributes are funded deterministically. User mining goes through `outbe_nod::precompile::dispatch`.

1. **GREEN WWD**
   - pre-seed previous-day and current-day VWAP snapshots (so `day_type` is inferred, not set by hand);
   - tick through `FORMING -> LOOKBACK -> OFFERING -> WAITING -> READY`;
   - issue `Tribute` inside the OFFERING window while the status machine has unsealed the day;
   - `process_metadosis` auto-runs `distribute_agent_rewards`, `calculate_metadosis_details`, and `outbe_lysis::runtime::lysis`, marks the day `COMPLETED` and accumulates remainder into `PromisLimit::total_unallocated`;
   - `lysis(...)` issues NODs into fresh *unqualified* buckets; the test then seeds the COEN/0xUSD exchange rate above the bucket's `floor_price_minor` and runs one more tick so `NodLifecycle::begin_block` flips `bucket_is_qualified`;
   - user call: `INod::mineGratisCall` through `outbe_nod::precompile::dispatch` — the dispatcher runs PoW, qualification check, noop settlement, NOD burn, and `Gratis::mine` in one atomic handler.

2. **RED WWD**
   - previous VWAP > current -> `day_type = RED`;
   - two tributes issued (small and large); RED-day allocation (/16) only funds the small tribute;
   - skipped large tribute is preserved for a future day (asserted on `get_tributes_by_owner`);
   - `process_metadosis` marks `COMPLETED`, `PromisLimit` total grows again;
   - user call: same precompile dispatch as GREEN.

## Scope

The test drives:

`WWD -> Tribute -> Lysis -> NOD -> mine_gratis -> GRATIS`

through the production block-lifecycle entry point and the production NOD precompile dispatcher.

## What this test does NOT cover

- NOD payment settlement uses the noop hook (`settle_mine_payment_noop(...)`); no cost-of-gratis balance movement is asserted.
- An explicit `metadosis::daily_accumulation::apply(...)` call per day on top of `EmissionLimitLifecycle`'s per-block emission — day limits are large enough to fund the test's tributes deterministically; the full emission schedule path is not exercised end-to-end.
- No validator set / staking / oracle vote population is seeded, so the epoch-boundary branch, `process_unbonding`, and oracle tally/slash paths all no-op on empty state rather than asserting positive behavior.
- Reth payload building, state-root computation, and txpool admission (only the pre-execution hook phase runs, not the full `OutbeBlockExecutor`).

## Why this crate exists

`Metadosis` orchestrates day processing, but the full end-to-end story also needs knowledge of `Tribute`, `Lysis`, `NOD`, `GRATIS`, and `PromisLimit`. Keeping the broad lifecycle scenario here avoids coupling `outbe-metadosis`'s own test module structure to unrelated downstream runtime modules.

## Typical verification

```sh
cargo test -p outbe-e2e
```
