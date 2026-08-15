# Instrument Pricing Alignment — Nod, Intex, Gem, Credis

**Status:** proposal / work order
**Repo:** `outbe/outbe-chain`
**Branch this was written on:** `claude/instrument-pricing-alignment-v2ugwg`
**Written:** 2026-08-15

---

## 0. What this document is

Nod, Intex, Gem and Credis are supposed to be **the same instrument** with different
parameters. Today they are not: only Intex implements the full price triple plus the
call trigger, Gem implements it with an inconsistent rate value, and Nod and Credis
implement no call concept at all.

This document is the complete work order to fix that. It is written so that an agent
starting **from an empty context** — no prior knowledge of this codebase — can execute
it end to end. Every claim about current behaviour carries a `path:line` citation so it
can be re-verified rather than trusted.

Read sections in order. Section 1 is normative (what must be true when the work is
done). Section 2 is the audit (what is true now). Section 3 is the defect register.
Section 4 is the executable plan. Section 7 lists the one decision that needs a human.

### 0.1 Environment bootstrap

```bash
# from the repo root
mise run check        # cargo check --workspace
mise run lint         # cargo clippy --all-targets -- -D warnings
mise run test         # cargo nextest run --workspace && cargo test --doc --workspace
mise run fmt          # cargo fmt --all
mise run export-abi   # regenerate contracts/*/abi-export/*.json from the .sol sources
```

Targeted test runs (much faster while iterating):

```bash
cargo nextest run -p outbe-gem -p outbe-gemfactory
cargo nextest run -p outbe-intex -p outbe-intexfactory
cargo nextest run -p outbe-nod -p outbe-nodfactory -p outbe-lysis
cargo nextest run -p outbe-credis -p outbe-credisfactory
```

### 0.2 Repository orientation

| Concern | Location |
|---|---|
| Instrument ledgers (records, indexes, state machine) | `crates/core/{nod,intex,gem,credis}` |
| Instrument orchestration (issuance policy, pricing, settlement) | `crates/core/{nodfactory,intexfactory,gemfactory,credisfactory}` |
| Solidity ABI surface for precompiles | `contracts/precompiles/src/I*.sol` |
| Generated ABI JSON (consumed by the MCP server) | `contracts/precompiles/abi-export/I*.json` |
| Daily trigger registry (drives the call scans) | `crates/system/cycle/src/triggers.rs` |
| Oracle rates / VWAP history | `crates/system/oracle` |
| Off-chain certified Nod computation | `crates/core/lysis`, `crates/system/ocomp-protocol` |
| Genesis storage seeder | `scripts/seed_genesis.py` |
| Architecture decision records | `docs/adr/core/ADR-C-{NOD,INX,GEM,CRD}-*.md` |

Two storage models are in play and they behave very differently when you add a field:

* **Flat EVM storage** (`#[storage_record]` / `#[storage_schema]` macros) — Gem, Intex,
  Credis. Field `order` indices map to storage slots. Appending a new field with the
  next free `order` is backward compatible; **inserting or renumbering is a breaking
  storage migration.**
* **Compressed entity bodies** — Nod items and Nod buckets. The body is canonically
  encoded, committed to, and reproduced off-chain by the Lysis program. Adding a field
  changes the canonical encoding **and therefore the Lysis program semantics hash**, so
  it is a coordinated change across `outbe-nod`, `outbe-lysis` and
  `outbe-ocomp-protocol`. See §5.2.

---

## 1. Target model (normative)

### 1.1 The three prices

Every instrument carries the same three prices, all denominated in the instrument's
**reference currency** (ISO 4217 numeric), all on the oracle's 1e18 scale, all frozen at
issuance:

| Price | Meaning | Derivation |
|---|---|---|
| **Entry Price** | The COEN/`<reference>` rate observed at issuance. The anchor from which the other two derive. | Read from the Oracle at issuance. |
| **Floor Price** | The level whose upward breach **qualifies** the instrument. | `entry × (100 + FLOOR_RATE) / 100` |
| **Call Price** | The level whose sustained upward breach **force-calls** the instrument. | `entry × (100 + CALL_RATE) / 100` |

`FLOOR_RATE` and `CALL_RATE` are **percentage-point markups over entry**, matching the
convention already in the code (`crates/core/intexfactory/src/constants.rs:40-45`,
`crates/core/gemfactory/src/constants.rs:1-7`). A rate of `8` yields `1.08×`; a rate of
`128` yields `2.28×`.

> **Read this before touching any number.** `128` does **not** mean "×1.28". It means
> "+128 percentage points", i.e. ×2.28. Confusing the two is the root cause of defect
> **D-01** below. See §7 for the one place this reading still needs human confirmation.

### 1.2 The call trigger

Identical mechanism for all four instruments:

* The instrument must be **Qualified** to be callable.
* Once daily, walk the trailing **`CALL_WINDOW` = 28 days** of *finalized* COEN/`<reference>`
  daily VWAPs.
* Count days where `vwap > call_price`. Days before the instrument's own issuance day
  are not counted and terminate the walk.
* If `breach_days >= CALL_THRESHOLD` — **21 days** — transition Qualified → **Called** and
  stamp `called_at`.
* A Called instrument that is not settled within **`CALL_NOTICE_PERIOD` = 7 days** of
  `called_at` is forfeited.

Window/threshold/notice are stored **in seconds** and divided by `86_400` at scan time;
they are **snapshotted onto each record at issuance** so a later constant change cannot
re-term a live instrument. `crates/core/gem/src/runtime.rs:47-85` is the reference
implementation of the trigger; `crates/core/intexfactory/src/called.rs:214-274` is the
second.

### 1.3 Per-instrument parameters

Only the call rate differs. Everything else is shared.

| Instrument | `CALL_RATE` | Call price | `FLOOR_RATE` | Floor price | Window | Threshold | Notice |
|---|---|---|---|---|---|---|---|
| **Credis** | **64** | 1.64 × entry | 8 | 1.08 × entry | 28 d | 21 d | 7 d |
| **Intex** | **128** | 2.28 × entry | 8 | 1.08 × entry | 28 d | 21 d | 7 d |
| **Gem** | **128** | 2.28 × entry | 8 | 1.08 × entry | 28 d | 21 d | 7 d |
| **Nod** | **256** | 3.56 × entry | 8 | 1.08 × entry | 28 d | 21 d | 7 d |

`FLOOR_RATE = 8` is not part of the requested change — it is already uniform across the
three instruments that have a floor (`crates/core/lysis/src/constants.rs:4`,
`crates/core/intexfactory/src/constants.rs:42`, `crates/core/gemfactory/src/constants.rs:3`).
Credis inherits it.

### 1.4 Invariants that must hold after this work

For every instrument record, in every currency:

1. `entry_price_minor > 0`
2. `floor_price_minor == entry_price_minor * (100 + FLOOR_RATE) / 100`
3. `call_price_minor == entry_price_minor * (100 + CALL_RATE) / 100`
4. `entry_price_minor < floor_price_minor < call_price_minor`
5. `call_rate` stored on the record equals the instrument's `CALL_RATE` constant at the
   time of issuance, and re-deriving the call price from the stored `entry_price_minor`
   and stored `call_rate` reproduces the stored `call_price_minor` exactly.
6. `call_window == 28 * 86_400`, `call_threshold == 21 * 86_400`,
   `call_notice_period == 7 * 86_400` (production profile).
7. `call_threshold <= call_window`.
8. Every price field present in the Rust record is also present on the corresponding
   Solidity view struct in `contracts/precompiles/src/`.

Invariant 5 is the one that is *silently violated today by stored data* — see D-01.

---

## 2. Where the repo stands today

### 2.1 Field-presence matrix

`Y` = present and populated. `bucket` = present only on the aggregate, not the item.
`—` = absent entirely.

| Field | Nod | Intex | Gem | Credis |
|---|---|---|---|---|
| `entry_price_minor` | **bucket only** ¹ | Y | Y | Y ² |
| `floor_price_minor` | Y | Y | Y | **—** |
| `call_price_minor` | **—** | Y | Y | **—** |
| `call_rate` (stored) | **—** | **—** ³ | Y (wrong value ⁴) | **—** |
| `call_window` | **—** | Y | Y | **—** |
| `call_threshold` | **—** | Y | Y | **—** |
| `call_notice_period` | **—** | Y | Y | **—** |
| `called_at` | **—** | Y | Y | **—** |
| Qualified state | `is_qualified` bool (on bucket) | `IntexState::Qualified` | `GemState::Qualified` | **—** |
| Called state | **—** | `IntexState::Called` | `GemState::Called` | **—** |
| Daily call scan | **—** | `TriggerId::IntexCallDaily` | `TriggerId::GemCallDaily` | **—** |
| Forfeit on notice lapse | **—** | (target-chain side) | `GemContract::forfeit` | **—** |
| Prices on the Solidity view struct | floor only | Y (no `callRate`) | entry+floor only | entry only |

¹ `NodItemState` (`crates/core/nod/src/schema.rs:33-71`) has no `entry_price_minor`.
`NodBucketState` (`:77-101`) has it at `order = 4`. `nod_api::add_nod` takes the entry
price as a **separate argument** and folds it into the bucket only
(`crates/core/nod/src/api.rs:45-57`, `crates/core/nod/src/repository.rs:623,659`).

² `Position.entry_price_minor` at `order = 13` (`crates/core/credis/src/schema.rs:67-68`),
populated from the Gratis pledge's `entry_rate`
(`crates/core/credis/src/runtime.rs:99`, `crates/core/credisfactory/src/runtime.rs:95`).

³ Intex derives the call price from `IntexParams::call_rate` at issuance
(`crates/core/intexfactory/src/runtime.rs:51`) but never writes the rate onto
`SeriesRecord`, so it cannot be audited from the record afterwards.

⁴ See D-01.

### 2.2 Per-instrument findings

#### Intex — the reference implementation, one gap

Everything is in place: `SeriesRecord` carries entry/floor/call plus the flat
`call_window` / `call_threshold` / `call_notice_period` group
(`crates/core/intex/src/schema.rs:205-254`). Prices are derived at issuance through the
shared `marked_up` helper (`crates/core/intexfactory/src/runtime.rs:50-51`, helper at
`:240-245`). Constants are correct: `CALL_RATE = 128`, `FLOOR_RATE = 8`,
`CALL_WINDOW = 28d`, `CALL_THRESHOLD = 21d`, `CALL_NOTICE_PERIOD = 7d`
(`crates/core/intexfactory/src/constants.rs:38-53`). The daily scan
(`crates/core/intexfactory/src/called.rs`) implements exactly the §1.2 mechanism.

Gap: **`call_rate` is not snapshotted on the record** (D-05), and Intex additionally
gates qualification on a 21-day `QUALIFICATION_PERIOD`
(`crates/core/intexfactory/src/constants.rs:38`, enforced at
`crates/core/intexfactory/src/qualified.rs:185-188`) that Gem and Nod do not apply (D-07).

#### Gem — complete mechanism, corrupt rate value

`GemData` carries all seven call fields (`crates/core/gem/src/schema.rs:33-105`).
`gemfactory` derives floor and call correctly from the oracle rate
(`crates/core/gemfactory/src/runtime.rs:452-468`) and writes `call_rate: CALL_RATE`
where `CALL_RATE = 128` (`crates/core/gemfactory/src/constants.rs:7`,
used at `runtime.rs:61` and `:224`).

But two other writers of the same field use **228**:

* `scripts/seed_genesis.py:770` — `gem.get("call_rate", 228)`, with the comment
  *"add_gem snapshots GEM_CALL_MARKUP_PERCENT (228)"*. No such constant exists anywhere
  in the repo.
* `crates/core/gem/src/tests.rs:33` — fixture sets `call_rate: 228` while setting
  `call_price_minor: 1.14e18` against `entry_price_minor: 0.5e18`. `1.14 / 0.5 = 2.28`,
  which is the **128** markup. The fixture stores a rate that does not reproduce its own
  price.

So genesis-seeded gems and the test corpus violate invariant 5. See D-01.

#### Nod — no call concept at all

`NodItemState` has `floor_price_minor` and `is_settled`, no entry price, no call price,
no call parameters, no `called_at`, and no Called state
(`crates/core/nod/src/schema.rs:33-71`). Qualification is a monotonic latch on the
**bucket** when the COEN rate strictly exceeds the bucket floor
(`crates/core/nod/src/hooks.rs:1-25`). There is no daily call scan registered for Nod
(`crates/system/cycle/src/triggers.rs:19-25` lists only `IntexCallDaily` and
`GemCallDaily`). Grepping `crates/core/nod` and `crates/core/nodfactory` for call-price
semantics returns nothing.

Mitigating factor for the work: **the entry price already reaches the Nod boundary.**
`NodActionV1` carries `entry_price_minor` in both the Lysis type
(`crates/core/lysis/src/program_v1/types.rs:101-115`) and the OCOMP wire struct
(`crates/system/ocomp-protocol/src/result.rs`), and it is encoded into the certified
artifact (`crates/core/lysis/src/program_v1/artifacts.rs:700-703`). Nod's floor price is
computed as `entry × 1.08` by `calc_floor_price` (`crates/core/lysis/src/constants.rs:4-8`).
The value is available at issuance; it is simply dropped on the floor for the item record.

#### Credis — a different shape entirely

`Position` (`crates/core/credis/src/schema.rs:16-69`) is a ten-installment debt schedule:
principal, outstanding amounts, a pinned `currency_rate`, and `entry_price_minor`.
There is **no floor price, no call price, no call parameters, no qualification, no
lifecycle state enum, and no daily call scan.** The only forced-termination path is a
begin-block *expiry* sweep that burns collateral once the schedule's end date passes with
an outstanding balance (`crates/core/credisfactory/src/lifecycle.rs:42-75`,
`crates/core/credisfactory/src/runtime.rs:202`). That is a time trigger, not a price
trigger; it is not the mechanism in §1.2.

Credis is the largest delta and the one carrying a genuine design question — see §7.

---

## 3. Defect register

Each defect has a stable ID used by the work plan in §4.

| ID | Severity | Defect |
|---|---|---|
| **D-01** | **High** | Gem `call_rate` is written as `228` by the genesis seeder (`scripts/seed_genesis.py:770`) and the test fixture (`crates/core/gem/src/tests.rs:33`), but as `128` by GemFactory (`crates/core/gemfactory/src/runtime.rs:61,224`). Under the field's own documented formula (`crates/core/gem/src/schema.rs:82-86`) `228` means ×3.28, so seeded gems carry a rate that contradicts their own stored call price. Invariant 5 violated in production genesis data. |
| **D-02** | **High** | Nod has no call price, no call rate, no call window/threshold/notice, no `called_at`, no Called state and no daily call scan. Required: `CALL_RATE = 256`. |
| **D-03** | **High** | Credis has no floor price, no call price, no call parameters, no qualification state and no daily call scan. Required: `CALL_RATE = 64`. |
| **D-04** | Medium | `NodItemState` has no `entry_price_minor`; the value exists at issuance and in the certified artifact but is folded only into `NodBucketState`. The item cannot re-derive its own floor or call price. |
| **D-05** | Medium | Intex derives the call price from `IntexParams::call_rate` but never snapshots the rate onto `SeriesRecord`, so a series' rate is not auditable from its record and a config change silently detaches history. |
| **D-06** | Medium | Solidity view structs are out of sync with the Rust records. `IGem.GemData` (`contracts/precompiles/src/IGem.sol:5-17`) omits every call field. `INod.NodData` (`contracts/precompiles/src/INod.sol:38-51`) omits `entryPriceMinor` and every call field. `ICredis.Position` (`contracts/precompiles/src/ICredis.sol:18-33`) omits floor and call. `IIntex.SeriesData` omits `callRate`. |
| **D-07** | Medium | Qualification preconditions diverge: Intex requires `issued_at + QUALIFICATION_PERIOD (21 d)` **and** `rate > floor` (`crates/core/intexfactory/src/qualified.rs:185-191`); Gem and Nod require only `rate > floor` (`crates/core/gem/src/runtime.rs:27-29`, `crates/core/nod/src/hooks.rs:6-8`). If these are one instrument, the precondition must be one rule. |
| **D-08** | Low | The markup helper is duplicated three times with three signatures: `intexfactory::runtime::marked_up` (`:240-245`, `u16` rate), `gemfactory::runtime::derived_floor` / `derived_call_price` (`:452-468`, `u64` rate), `lysis::constants::calc_floor_price` (`:6-8`, unchecked multiply). The Lysis one can overflow-panic where the other two return an error. |
| **D-09** | Low | `crates/core/intex/src/tests.rs:15` sets a local `CALL_NOTICE_PERIOD = 21 days` against the production constant of 7 days, so the Intex ledger tests assert against a notice period the chain never uses. |
| **D-10** | Low | Lifecycle-state representation diverges: Nod uses two independent booleans (`is_qualified` on the bucket, `is_settled` on the item), Credis has no state at all, Gem has a 4-state enum, Intex a 3-state enum (no `Settled`). |
| **D-11** | Low | `ADR-C-GEM-001` (`docs/adr/core/ADR-C-GEM-001-gem-ledger.md:26-36`) documents the lifecycle as `Issued → Qualified → Settled → Burned` and states *"Burn is allowed only from Settled"*. The implemented lifecycle includes `Qualified → Called → forfeit-burn` (`crates/core/gem/src/runtime.rs:90-106`). The ADR is stale and contradicts the code. |
| **D-12** | Low | `crates/core/nod/src/schema.rs:168` names `tests::nod_contract_slot_layout_is_pinned` as the tripwire protecting the Nod slot layout, but **no such test exists** anywhere in the workspace (`grep -rn nod_contract_slot_layout_is_pinned --include='*.rs' crates/` returns only that doc comment). WP-3 appends fields to Nod storage with no layout guard in place. Write the test before WP-3b, modelled on `crates/core/gem/src/tests.rs:517`. |

---

## 4. Work plan

Execute in the order given. WP-0 first (everything else depends on it); WP-1 and WP-2 are
small and independently landable; WP-3 and WP-4 are the large ones; WP-5 closes out the
surface.

Each work package states **files to touch**, **what to change**, and **how to verify**.

---

### WP-0 — Shared pricing primitives *(fixes D-08, unblocks everything)*

**Goal:** one definition of a markup rate, one helper, one place per instrument for its
call rate.

1. Create `crates/core/common/src/pricing.rs` and export it from
   `crates/core/common/src/lib.rs` (which currently exports only `pow` and
   `worldwideday`):

   ```rust
   //! Shared instrument pricing: entry -> floor / call derivation.
   //!
   //! Rates are percentage-point markups over the entry price:
   //! `price = entry * (PRICE_RATE_DEN + rate) / PRICE_RATE_DEN`.
   //! A rate of 8 yields 1.08x entry; a rate of 128 yields 2.28x entry.

   use alloy_primitives::U256;
   use outbe_primitives::error::{PrecompileError, Result};

   /// Denominator for all instrument markup rates.
   pub const PRICE_RATE_DEN: u16 = 100;

   /// Floor-price markup, shared by every instrument: floor = 1.08 x entry.
   pub const FLOOR_RATE: u16 = 8;

   /// Call-trigger evaluation window in seconds (28 days).
   pub const CALL_WINDOW: u32 = 28 * 24 * 3600;
   /// Breach threshold in seconds (21 days out of the 28-day window).
   pub const CALL_THRESHOLD: u32 = 21 * 24 * 3600;
   /// Settlement notice after `called_at`, in seconds (7 days).
   pub const CALL_NOTICE_PERIOD: u32 = 7 * 24 * 3600;

   /// Per-instrument call-price markup rates.
   pub const CREDIS_CALL_RATE: u16 = 64;
   pub const INTEX_CALL_RATE: u16 = 128;
   pub const GEM_CALL_RATE: u16 = 128;
   pub const NOD_CALL_RATE: u16 = 256;

   /// `entry * (100 + rate) / 100`, checked.
   pub fn marked_up(entry_price: U256, rate: u16) -> Result<U256> {
       entry_price
           .checked_mul(U256::from(PRICE_RATE_DEN + rate))
           .map(|v| v / U256::from(PRICE_RATE_DEN))
           .ok_or_else(|| PrecompileError::Revert("marked-up price overflow".into()))
   }

   /// Floor price for any instrument.
   pub fn floor_price(entry_price: U256) -> Result<U256> {
       marked_up(entry_price, FLOOR_RATE)
   }
   ```

   Add unit tests asserting `marked_up(1e18, 8) == 1.08e18`, `(_, 64) == 1.64e18`,
   `(_, 128) == 2.28e18`, `(_, 256) == 3.56e18`, and that `U256::MAX` returns `Err`
   rather than panicking.

2. Re-point the existing call sites at the shared helper, keeping the old names as
   deprecated re-exports where they are part of a crate's public API:
   * `crates/core/intexfactory/src/runtime.rs:240-245` — make `marked_up` delegate.
     `outbe_desis` calls it (`crates/core/desis/src/runtime.rs:192-193`), so keep the
     `pub` re-export.
   * `crates/core/gemfactory/src/runtime.rs:452-468` — replace `derived_floor` and
     `derived_call_price` bodies with `common::pricing` calls. Note their current rate
     constants are `u64`; the shared ones are `u16`.
   * `crates/core/lysis/src/constants.rs:6-8` — `calc_floor_price` currently uses an
     **unchecked** `*`, which panics on overflow where the other two return `Err`.
     Delegate to `pricing::floor_price`. This changes a panic into a `Result`; adjust the
     Lysis call site at `crates/core/lysis/src/program_v1/execute.rs:14` accordingly.
   * `crates/core/intexfactory/src/constants.rs:40-52` — keep the `CALL_RATE`,
     `FLOOR_RATE`, `CALL_WINDOW`, `CALL_THRESHOLD`, `CALL_NOTICE_PERIOD`,
     `PRICE_RATE_DEN` names as re-exports of the shared values so
     `crates/core/intexfactory/src/config.rs` and the tests keep compiling.
   * `crates/core/gem/src/constants.rs:12-23` — same treatment for `CALL_WINDOW`,
     `CALL_THRESHOLD`, `CALL_NOTICE_PERIOD`.

3. Add `outbe-common` to the `[dependencies]` of `outbe-gemfactory`, `outbe-nodfactory`
   and `outbe-credisfactory` in the workspace `Cargo.toml` / per-crate manifests if not
   already present.

**Verify:** `mise run check && mise run lint && cargo nextest run -p outbe-common`. All
existing instrument tests must still pass unchanged — this package is pure refactor.

---

### WP-1 — Gem: correct the stored call rate *(fixes D-01)*

The mechanism is already right; only the stored *number* is wrong in two writers.

1. `scripts/seed_genesis.py:769-770` — change the default from `228` to `128` and fix the
   comment, which references a nonexistent `GEM_CALL_MARKUP_PERCENT`:

   ```python
   # call_rate: add_gem snapshots gemfactory's CALL_RATE (128 percentage points
   # over entry, i.e. call price = 2.28x entry).
   storage.set_mapping(14, gem_id, parse_int(gem.get("call_rate", 128)))
   ```

2. `crates/core/gem/src/tests.rs:33` — change `call_rate: 228` to `call_rate: 128`. The
   fixture's `call_price_minor: 1.14e18` against `entry_price_minor: 0.5e18` is already
   the 128 markup, so this makes the fixture self-consistent rather than changing intent.

3. Add a regression test in `crates/core/gem/src/tests.rs` that closes the loop —
   this is the test whose absence let D-01 exist:

   ```rust
   #[test]
   fn stored_call_rate_reproduces_stored_call_price() {
       // For any gem, re-deriving the call price from the record's own
       // entry_price_minor and call_rate must reproduce call_price_minor exactly.
       let params = base_params(owner());
       let derived = outbe_common::pricing::marked_up(
           params.entry_price_minor,
           params.call_rate,
       ).unwrap();
       assert_eq!(derived, params.call_price_minor);
   }
   ```

4. Check for genesis JSON fixtures that pin `call_rate` explicitly:
   `grep -rn '"call_rate"' --include='*.json' --include='*.yaml' --include='*.toml' .`
   Update any hit to `128`.

**Migration note:** any chain already seeded from genesis carries `call_rate = 228` in
storage slot 14 of the gem record. Because the call *price* was seeded independently and
is correct, this is a metadata inconsistency, not a mispriced instrument — the daily scan
reads `call_price_minor`, not `call_rate` (`crates/core/gem/src/runtime.rs:71`). No
state migration is required for a chain that is re-genesised. If a live chain must be
corrected in place, that needs a separate migration ADR.

**Verify:** `cargo nextest run -p outbe-gem -p outbe-gemfactory`, then
`python3 -c "import ast,sys; ast.parse(open('scripts/seed_genesis.py').read())"` and
whatever genesis smoke target applies (`mise run localnet-bootstrap`).

---

### WP-2 — Intex: snapshot the call rate *(fixes D-05, D-09)*

1. `crates/core/intex/src/schema.rs` — append to `SeriesRecord` (current max order is
   `13`, so **use 14**; do not renumber anything):

   ```rust
   /// Call-price markup percent, snapshotted at issuance:
   /// `call_price_minor = entry_price_minor * (100 + call_rate) / 100`
   /// (128 => 2.28x). Frozen so a later config change cannot re-term a live series.
   #[attribute(order = 14, default = 0)]
   pub call_rate: u16,
   ```

   Also add `call_rate: u16` to `CreateSeriesParams` (`:181-199`).

2. `crates/core/intexfactory/src/runtime.rs:51` — pass `cfg.call_rate` into the
   `CreateSeriesParams` literal at `:57-72`.

3. `crates/core/intex/src/api.rs:43` (`create_series`) — persist the new field.

4. `contracts/precompiles/src/IIntex.sol` — append `uint16 callRate;` to `SeriesData`
   (append at the end of the struct; do not reorder existing members). Update the
   encoder in `crates/core/intex/src/precompile.rs`.

5. `crates/core/intex/src/tests.rs:15` — delete the local `CALL_NOTICE_PERIOD = 21 days`
   and use `outbe_common::pricing::CALL_NOTICE_PERIOD` (7 days). Update the assertions at
   `:95`, `:172` and `:352` that depend on it.

6. Add the invariant-5 regression test mirroring WP-1 step 3, for `SeriesRecord`.

**Verify:** `cargo nextest run -p outbe-intex -p outbe-intexfactory -p outbe-desis`, then
`mise run export-abi` and confirm `contracts/precompiles/abi-export/IIntex.json` gained
`callRate`.

---

### WP-3 — Nod: add the full price triple and the call lifecycle *(fixes D-02, D-04)*

This is the highest-risk package because Nod item bodies are **compressed entities whose
canonical encoding is reproduced off-chain by the Lysis program**. Read §5.2 before
starting. Do the work in the sub-order below; each sub-step keeps the tree compiling.

#### WP-3a — Carry the entry price onto the item

`crates/core/nod/src/schema.rs` — `NodItemState` currently ends at `order = 10`
(`is_settled`). Append:

```rust
/// COEN/<reference> rate observed at issuance; the anchor from which
/// floor and call prices derive. Already carried by `NodActionV1` and
/// folded into the bucket; now retained on the item too.
#[attribute(order = 11)]
pub entry_price_minor: U256,
```

Then remove the separate `entry_price_minor` argument threading through
`nod_api::add_nod` (`crates/core/nod/src/api.rs:45-57`) and read it off the item instead
— `crates/core/nod/src/repository.rs:623,659` already needs the value to build the
bucket, so it now reads `item.entry_price_minor`. Update the caller at
`crates/core/nodfactory/src/runtime.rs:41` and populate the field in the `NodItemState`
literal at `crates/core/nodfactory/src/runtime.rs:60-71` from `params.entry_price_minor`.

#### WP-3b — Add the call fields

Append to `NodItemState`, continuing the order sequence:

```rust
#[attribute(order = 12)]
pub call_price_minor: U256,
#[attribute(order = 13, default = 0)]
pub call_rate: u16,
#[attribute(order = 14, default = 0)]
pub call_window: u32,
#[attribute(order = 15, default = 0)]
pub call_threshold: u32,
#[attribute(order = 16, default = 0)]
pub call_notice_period: u32,
#[attribute(order = 17, default = 0)]
pub called_at: u64,
```

Add `call_price_minor: U256` to `NodIssueParams` (`crates/core/nod/src/schema.rs:17-29`)
or — preferred — derive it inside `issue_nod_inner` from `params.entry_price_minor` via
`outbe_common::pricing::marked_up(entry, NOD_CALL_RATE)` so the constant has exactly one
call site.

#### WP-3c — Replace the boolean lifecycle with a state enum *(also addresses D-10)*

Nod currently uses `is_qualified` (bucket) + `is_settled` (item). A Called state cannot
be expressed by those booleans. Introduce, mirroring `GemState`
(`crates/core/gem/src/schema.rs:5-12`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NodState {
    Issued = 0,
    Qualified = 1,
    Called = 2,
    Settled = 3,
}
```

Keep `is_settled` as a computed accessor during the transition if that reduces churn in
`crates/core/nodfactory` (the mining path reads it), but the persisted field should
become the state byte. Note that Nod qualification is currently a **bucket-level**
latch — decide whether Called is per-item (recommended: yes, since `called_at` and the
notice deadline are per-holder) while qualification stays per-bucket.

#### WP-3d — Propagate through the certified computation pipeline

Every one of these must change together or the semantics hash check will reject results:

| File | Change |
|---|---|
| `crates/core/lysis/src/program_v1/types.rs:101-115` | Add `call_price_minor` to `NodActionV1`. |
| `crates/core/lysis/src/program_v1/artifacts.rs:700-703` | Add `encoded.write_u256(nod.call_price_minor)?` in the encoder, **after** `entry_price_minor` to keep the existing prefix stable. |
| `crates/core/lysis/src/program_v1/artifacts.rs:747-777` | Matching read in the decoder, same position. |
| `crates/core/lysis/src/program_v1/artifacts.rs:1294-1299` | Extend the validation that rejects zero prices. |
| `crates/core/lysis/src/program_v1/execute.rs:110-125` | Compute the call price alongside the floor price. |
| `crates/system/ocomp-protocol/src/result.rs` | Add `call_price_minor` to the `NodActionV1` `wire_struct!`. |
| `crates/core/lysis/src/runtime.rs:114-116` | Pass the new field through to `NodIssueParams`. |
| `crates/core/nodfactory/src/materialization.rs` | Materialized certified Nods must receive the field. |

Then **bump the Lysis program semantics hash** and regenerate every pinned fixture:
`crates/system/ocomp-protocol/tests/finality_vectors.rs`,
`crates/system/ocomp-protocol/tests/nod_materialization.rs`,
`crates/system/ocomp-protocol/tests/nod_membership.rs`,
`crates/core/lysis/src/activation_v1/receipts.rs`, and
`protocol-bundle-v1.ocb1` if it pins the program identity.

#### WP-3e — Add the daily call scan

Create `crates/core/nod/src/call.rs` modelled on `crates/core/gem/src/runtime.rs:47-106`
and `crates/core/gem/src/hooks.rs:124-224`:

* a callable-Nod index (dense list of items in Qualified/Called), maintained on state
  transitions — copy `GemContract::insert_callable` / `remove_callable`
  (`crates/core/gem/src/state.rs:190-215`);
* `trigger_call(window, nod_id, now)` — identical breach-counting logic to
  `crates/core/gem/src/runtime.rs:47-85`;
* `forfeit(nod_id, now)` — identical deadline logic to `:90-106`;
* `run_call_daily(ctx)` — identical VWAP-window caching to
  `crates/core/gem/src/hooks.rs:133-224`, including the
  `utc_day_vwap_last_finalized` guard at `:142-146` and the per-item
  `with_checkpoint` isolation at `:182-190`.

Register the trigger in `crates/system/cycle/src/triggers.rs`. **`TriggerId` values are
consensus-persisted and must never be renumbered** (`:12-16`) — append only:

```rust
pub enum TriggerId {
    EmissionLimit1 = 0,
    IntexCallDaily = 1,
    WwdAdvanceNoon = 2,
    AuctionAdvance = 3,
    GemCallDaily = 4,
    NodCallDaily = 5,      // new
    CredisCallDaily = 6,   // new, WP-4
}
```

Add matching `TriggerHandler` variants, a `metadosis_mutation_lease_budget` of `0`
(`:72-79`), and `TriggerSpec` entries with `period_seconds: 86_400`,
`start_offset_seconds: 0`, `requires_accounting_window: false` — copying the
`gem_call_daily` spec at `:171-180`. **Widen the array types**: `active_triggers` returns
`[TriggerSpec; 5]` (`:113`) and `ACTIVE_TRIGGER_ARRAY` is `[TriggerSpec; 5]` (`:184`);
both become `7`. Update the test at `:211-229`, which indexes the array positionally.

#### WP-3f — Nod events and view surface

Add `NodCalled` / `NodForfeited` events alongside the existing `NodBucketQualified`
(`contracts/precompiles/src/INod.sol:27-35`), and extend `INod.NodData` (§WP-5).

**Verify:** `cargo nextest run -p outbe-nod -p outbe-nodfactory -p outbe-lysis -p outbe-ocomp-protocol -p outbe-cycle`,
then the OCOMP end-to-end targets (`mise run ocomp-poc-integration`) since the semantics
hash changed.

---

### WP-4 — Credis: add the price triple and the call lifecycle *(fixes D-03)*

**Read §7 before starting this package.** Credis is a debt schedule, not a Promis-bearing
NFT, so "what a Called Credis position *does*" is a product decision. The plan below
implements the mechanically-consistent default: **Called accelerates the position into
the existing expiry path.** If §7 is answered differently, only WP-4d changes.

#### WP-4a — Prices on the record

`crates/core/credis/src/schema.rs` — `Position` uses orders `0..=7` and `9..=13`;
**order 8 is an unused gap, leave it alone** and append from 14:

```rust
/// Price floor: `entry_price_minor * 1.08`. Its upward breach qualifies
/// the position for call evaluation.
#[attribute(order = 14, default = 0)]
pub floor_price_minor: U256,

/// Call price: `entry_price_minor * (100 + call_rate) / 100` (64 => 1.64x).
#[attribute(order = 15, default = 0)]
pub call_price_minor: U256,

/// Call-price markup percent, snapshotted at issuance (64 for Credis).
#[attribute(order = 16, default = 0)]
pub call_rate: u16,

#[attribute(order = 17, default = 0)]
pub call_window: u32,
#[attribute(order = 18, default = 0)]
pub call_threshold: u32,
#[attribute(order = 19, default = 0)]
pub call_notice_period: u32,
#[attribute(order = 20, default = 0)]
pub called_at: u64,
/// Lifecycle state; decode via `CredisState`.
#[attribute(order = 21, default = 0)]
pub state: u8,
```

Add a `CredisState { Issued = 0, Qualified = 1, Called = 2, Settled = 3 }` enum mirroring
`GemState`.

Populate in `CredisContract::create_position`
(`crates/core/credis/src/runtime.rs:63-102`), deriving from the existing
`entry_price_minor: rate` parameter:

```rust
let floor_price_minor = outbe_common::pricing::floor_price(rate)?;
let call_price_minor = outbe_common::pricing::marked_up(
    rate, outbe_common::pricing::CREDIS_CALL_RATE,
)?;
```

**Reference-currency caveat.** Credis stores `issuance_currency`
(`crates/core/credis/src/schema.rs:56-57`) derived from the disbursed asset's
`isoCode()`, but the other three instruments price against a **reference currency**.
The daily call scan needs a COEN/`<reference>` oracle pair. Add a
`reference_currency: u16` field and populate it, or document that Credis pins
reference == issuance. Do not silently reuse `issuance_currency` as if it were the
reference currency.

#### WP-4b — Qualification

Add a begin-block qualifier promoting `Issued → Qualified` when the COEN rate for the
position's reference currency strictly exceeds `floor_price_minor`. The cheapest correct
implementation reuses the existing bounded cursor sweep in
`crates/core/credisfactory/src/lifecycle.rs:42-75` (`MAX_CREDIS_EXPIRY_SCANS_PER_BLOCK`,
`expiry_scan_cursor`) rather than building a new bin trie. If the position population is
expected to grow large, port the LB bin index from
`crates/core/gem/src/state.rs:254-341` instead — the `ponytail` comment at
`crates/core/credisfactory/src/lifecycle.rs:35-37` already flags the scan as O(n).

#### WP-4c — Daily call scan

Add `scan_and_call` in `crates/core/credisfactory`, structurally identical to
`crates/core/gem/src/hooks.rs:133-193`. Register `TriggerId::CredisCallDaily = 6` per
WP-3e (both new triggers land in the same array widening).

#### WP-4d — What Called *means* for a position

Default design, pending §7:

* `Qualified → Called` stamps `called_at` and emits `PositionCalled`.
* The holder has `CALL_NOTICE_PERIOD` (7 days) to settle the **outstanding anadosis
  balance**, which is exactly the existing `pay_anadosis` path.
* If `now > called_at + call_notice_period` and `outstanding_anadosis_amount > 0`,
  invoke the **existing** `runtime::expire_position`
  (`crates/core/credisfactory/src/runtime.rs:202`) — the collateral burn already
  implemented for time-expiry. This reuses a tested path rather than inventing a second
  forfeit mechanism.
* A position that reaches zero outstanding balance transitions to `Settled` and leaves
  the callable index.

#### WP-4e — Events and view surface

Add `PositionCalled(uint256 indexed positionId, uint64 calledAt)` to
`contracts/precompiles/src/ICredis.sol` alongside the existing `CollateralBurned`
(`:10`), and extend `ICredis.Position` per WP-5.

**Verify:** `cargo nextest run -p outbe-credis -p outbe-credisfactory -p outbe-cycle`, and
run the flow example under `examples/credis-flow/`.

---

### WP-5 — ABI surface, clients, docs *(fixes D-06, D-07, D-11)*

1. **Solidity view structs.** Append (never reorder) so each mirrors its Rust record:

   * `contracts/precompiles/src/IGem.sol` — `GemData` gains
     `uint256 callPrice; uint16 callRate; uint32 callWindow; uint32 callThreshold;
     uint32 callNoticePeriod; uint64 calledAt; uint64 qualifiedAt; uint64 settledAt;`
     Update the encoder `to_abi_data` at `crates/core/gem/src/precompile.rs:65`.
   * `contracts/precompiles/src/INod.sol` — `NodData` gains `uint256 entryPriceMinor;`
     plus the call fields, and `bool isQualified` / `bool isSettled` are replaced by
     `uint8 state` if WP-3c lands. Update `crates/core/nod/src/precompile.rs`.
   * `contracts/precompiles/src/ICredis.sol` — `Position` gains
     `uint256 floorPriceMinor; uint256 callPriceMinor; uint16 callRate;
     uint32 callWindow; uint32 callThreshold; uint32 callNoticePeriod;
     uint64 calledAt; uint8 state;`. Update `crates/core/credis/src/precompile.rs`.
   * `contracts/precompiles/src/IIntex.sol` — `uint16 callRate;` (WP-2).

2. **Regenerate ABIs:** `mise run export-abi`, then commit the changed
   `contracts/precompiles/abi-export/*.json`.

3. **MCP server.** `mcp/src/tools/intex.ts:293-295,470-472,505-507` already surfaces
   entry/floor/call for Intex. Add the equivalent for the other three tools so all four
   instruments read the same in the agent tooling.

4. **Resolve D-07 — one qualification rule.** Decide and apply uniformly:
   * *Option A (recommended):* drop `QUALIFICATION_PERIOD` from Intex so all four
     qualify purely on `rate > floor`. Remove the gate at
     `crates/core/intexfactory/src/qualified.rs:185-188` and the constant at
     `crates/core/intexfactory/src/constants.rs:38`.
   * *Option B:* add a 21-day maturity gate to Gem, Nod and Credis.

   Option A is recommended because §1.2 already encodes the protocol's time dimension in
   the 21-of-28 call trigger; a second, differently-scoped 21-day period on one
   instrument only is the anomaly. Whichever is chosen, land it in one commit across all
   four so the divergence never widens.

5. **Documentation.**
   * `docs/adr/core/ADR-C-GEM-001-gem-ledger.md:26-36` — update the lifecycle diagram to
     include `Qualified → Called → forfeit-burn` and correct the *"Burn is allowed only
     from Settled"* claim (D-11).
   * `docs/adr/core/ADR-C-NOD-001-*.md` — document the new Nod call lifecycle.
   * `docs/adr/core/ADR-C-CRD-001-credis-position-ledger.md` — document the new Credis
     prices and call lifecycle.
   * `docs/genesis-protocol-constants.md` — add the §1.3 parameter table. This file
     currently contains no pricing constants at all, which is why D-01 went unnoticed.
   * Write a new `docs/adr/core/ADR-C-PRC-001-instrument-pricing-and-call-trigger.md`
     holding §1 of this document as the single normative source, and link the four
     instrument ADRs to it.

---

## 5. Compatibility rules

### 5.1 Flat EVM storage (Gem, Intex, Credis)

* `#[attribute(order = N)]` maps to a storage slot. **Append with the next free index;
  never renumber.** Renumbering silently reinterprets existing chain state.
* Give every appended field a `default` so records written before the change decode.
* `crates/core/gem/src/tests.rs:517` (`gem_storage_layout_matches_genesis_seeder`) pins
  the Gem layout against `scripts/seed_genesis.py:716-795`. **Any Gem field addition must
  update the seeder's slot map, the docstring slot table at `scripts/seed_genesis.py:729-733`,
  and this test together.**
* `crates/core/nod/src/schema.rs:166-169` documents that `NodContract` occupies slots
  `0..=14` and names `tests::nod_contract_slot_layout_is_pinned` as the tripwire — **but
  that test does not exist** (D-12). Write it before appending Nod fields, or WP-3 lands
  with no layout guard at all.
* Solidity struct members are ABI-positional. Appending is safe for `view` returns;
  reordering breaks every decoder including `mcp/`.

### 5.2 Compressed entity bodies (Nod)

Nod item and bucket bodies are **not** flat storage. They are canonically encoded,
committed to on-chain as a root, and independently reproduced by the off-chain Lysis
program. Adding a field means:

1. The canonical encoder and decoder change
   (`crates/core/lysis/src/program_v1/artifacts.rs`).
2. The OCOMP wire struct changes (`crates/system/ocomp-protocol/src/result.rs`).
3. **The Lysis program semantics hash changes.** Every certified result produced under
   the old hash is rejected under the new one, and vice versa.
4. Every pinned test vector must be regenerated (see WP-3d for the list).

Append new fields **at the end of the encoded body** so the existing byte prefix is
unchanged; this keeps the diff reviewable even though the hash still moves.

### 5.3 Consensus-persisted identifiers

`TriggerId` values are emitted as the indexed `id` in `ICycle::CycleTriggerExecuted` and
persisted in the Cycle mappings. `crates/system/cycle/src/triggers.rs:12-16` states they
must remain byte-equal forever. **Append only.**

---

## 6. Acceptance criteria

The work is complete when all of the following hold.

**Automated:**

```bash
mise run fmt-check && mise run lint && mise run test
mise run export-abi && git diff --exit-code contracts/    # ABI JSON is in sync
```

**Property tests** — add one per instrument, asserting §1.4 invariants 1-7 against a
freshly issued record:

```
crates/core/gem/src/tests.rs      :: gem_pricing_invariants
crates/core/intex/src/tests.rs    :: intex_pricing_invariants
crates/core/nod/tests/pricing.rs  :: nod_pricing_invariants      (new file; Nod's unit
                                     tests live in crates/core/nod/src/adr006_tests.rs
                                     and its integration tests in crates/core/nod/tests/)
crates/core/credis/src/tests.rs   :: credis_pricing_invariants
```

Each must assert, for its instrument:
`floor == entry * 108/100`, `call == entry * (100 + CALL_RATE)/100`,
`entry < floor < call`, `call_window == 28 * 86_400`,
`call_threshold == 21 * 86_400`, `call_notice_period == 7 * 86_400`,
and the stored `call_rate` re-derives the stored `call_price_minor`.

**Cross-instrument test** — add a single test (suggested home:
`crates/core/common/src/tests.rs`) asserting the §1.3 table verbatim:

```rust
assert_eq!(CREDIS_CALL_RATE, 64);
assert_eq!(INTEX_CALL_RATE, 128);
assert_eq!(GEM_CALL_RATE, 128);
assert_eq!(NOD_CALL_RATE, 256);
assert_eq!(CALL_WINDOW / 86_400, 28);
assert_eq!(CALL_THRESHOLD / 86_400, 21);
assert_eq!(CALL_NOTICE_PERIOD / 86_400, 7);
assert_eq!(FLOOR_RATE, 8);
```

**Manual:**

* `grep -rn "228" scripts/seed_genesis.py` returns nothing pricing-related.
* `grep -rn "call_rate" --include='*.rs' crates/` shows every write sourced from a
  `common::pricing` constant — no literals at call sites.
* `TriggerId` has exactly two new variants, appended, and no existing value changed.
* Each of the four `contracts/precompiles/src/I{Nod,Intex,Gem,Credis}.sol` view structs
  exposes entry, floor and call price.
* `mise run localnet-bootstrap && mise run localnet-smoke` passes against a
  freshly-seeded genesis.

---

## 7. Open decision — needs a human

**Everything else in this document is mechanical. This is not.**

### 7.1 Confirm the rate convention (blocks nothing, but verify before merge)

The requested rates were given as "64% for credis, 128% intex, 256% nod, 128% gem". This
document reads them as **percentage-point markups over entry**, so 64 → ×1.64,
128 → ×2.28, 256 → ×3.56. Rationale:

* It matches the existing `CALL_RATE = 128` semantics already implemented and documented
  for Intex and Gem (`crates/core/intexfactory/src/constants.rs:44-45`,
  `crates/core/gemfactory/src/constants.rs:5-7`), and the requested Intex/Gem values are
  *identical* to what is already there — strong evidence the same convention is meant.
* It matches `FLOOR_RATE = 8` → ×1.08.
* Under the literal reading (64% → ×0.64) the Credis call price sits **below** its own
  floor price (×1.08) and below entry. Since qualification requires the rate to exceed
  the floor, and qualification is a monotonic latch, the call condition
  (`vwap > 0.64 × entry`) would then be *strictly implied* by the condition that made the
  instrument callable in the first place. The price test stops discriminating: avoiding a
  call would require COEN to fall **36% below the issuance rate** and stay there for at
  least 8 of any 28 days. The trigger degenerates from a price trigger into a ~21-day
  timer.

> **Correction.** An earlier draft of this section claimed ×0.64 would make the call fire
> "immediately and permanently". That is wrong. `crates/core/gem/src/runtime.rs:66-68`
> breaks the breach walk at `day < issued_day`, so no instrument can accumulate 21 breach
> days until it is at least 21 days old, whatever the price does — and missing oracle days
> do not count as breaches (`:69-73`), delaying it further. The defect in the ×0.64
> reading is the broken `entry < floor < call` ordering (invariant 4), not the timing.

**But see §7.2 — there is a coherent reading of ×0.64 that this document initially
dismissed too quickly.** For a *collateralized loan*, a call priced **below** entry is a
margin call: the COEN-denominated collateral loses value, so the lender calls the loan.
That is standard finance and it is the natural direction for Credis specifically, whereas
Gem/Nod/Intex are Promis-bearing instruments called away on the *upside*. If that is the
intent, Credis's call is not the same mechanism with a different rate — it is a
comparison in the opposite direction (`vwap < call_price`), and WP-4c must invert its
breach test. Resolve this together with §7.2 before implementing the Credis call scan.

### 7.2 What does a **Called Credis position** actually do?

Credis is structurally unlike the other three. Gem, Nod and Intex are Promis-bearing
instruments where Called → unsettled → forfeit-burn of the load. Credis is a
ten-installment debt schedule with pledged Gratis collateral and an existing *time-based*
expiry burn (`crates/core/credisfactory/src/runtime.rs:202`).

Adding a price-triggered call to a debt instrument means: **a rise in the COEN price
accelerates the borrower's repayment obligation.** WP-4d proposes the conservative
reading — Called starts a 7-day notice, after which the *existing* expiry path runs. But
the alternatives are materially different products:

| Option | Behaviour on Called |
|---|---|
| **A (WP-4d default)** | 7-day notice to clear the outstanding balance; then the existing collateral burn. Reuses tested code. |
| **B** | Called accelerates *all* remaining installments to immediately due, then notice, then burn. |
| **C** | Called is informational only — recorded and emitted, no forced settlement. Prices align, mechanism does not. |

### 7.3 Which *direction* does the Credis call trigger compare?

Orthogonal to §7.2, and it must be settled first because it determines the rate's meaning.

Gem, Nod and Intex are Promis-bearing instruments called away when COEN **rises**: the
call price is above entry and the breach test is `vwap > call_price`
(`crates/core/gem/src/runtime.rs:71`, `crates/core/intexfactory/src/called.rs:245`).

Credis is a loan against COEN-denominated Gratis collateral. The standard trigger for
that shape is a **margin call on the downside** — the collateral falls, the lender calls
the loan — which would put the call price *below* entry and invert the test to
`vwap < call_price`.

| | Call price | Breach test | Rate reading |
|---|---|---|---|
| **Upside (matches the other three)** | 1.64 × entry | `vwap > call_price` | 64 = +64 pp markup |
| **Downside margin call** | 0.64 × entry | `vwap < call_price` | 64 = 64% of entry |

The request framed all four as *"same instruments … call rate is different"*, which points
at the upside reading — a different mechanism is not a different rate. This document is
written against that reading throughout. If the downside reading is intended, §1.1,
§1.2, §1.4 invariant 4 and WP-4c all need Credis-specific carve-outs, and the shared
`marked_up` helper from WP-0 does not apply to it.

This affects borrowers' obligations and cannot be inferred from the codebase. **Do not
ship WP-4d without an explicit answer.** WP-4a through WP-4c (the prices, the record
fields, the qualification) are safe to land first under any option — they are pure
additions with no behavioural consequence until the call scan acts on them.

---

## Appendix A — Complete citation index

Every factual claim in this document, in file order.

**Gem**
- `crates/core/gem/src/constants.rs:14,19,23` — `CALL_WINDOW` 28 d, `CALL_THRESHOLD` 21 d, `CALL_NOTICE_PERIOD` 7 d
- `crates/core/gem/src/schema.rs:14-30` — `GemAddParams`
- `crates/core/gem/src/schema.rs:33-105` — `GemData`, call fields at orders 10-17
- `crates/core/gem/src/schema.rs:82-86` — the `(100 + call_rate) / 100` formula doc
- `crates/core/gem/src/api.rs:25-45` — `add_gem` field population
- `crates/core/gem/src/runtime.rs:13-36` — `qualify`, `rate > floor` only
- `crates/core/gem/src/runtime.rs:47-85` — `trigger_call`, reference 21-of-28 implementation
- `crates/core/gem/src/runtime.rs:90-106` — `forfeit`
- `crates/core/gem/src/state.rs:190-215` — callable index insert/remove
- `crates/core/gem/src/hooks.rs:133-224` — daily call scan and VWAP window cache
- `crates/core/gem/src/tests.rs:33` — fixture `call_rate: 228` (**D-01**)
- `crates/core/gem/src/tests.rs:517` — `gem_storage_layout_matches_genesis_seeder`
- `crates/core/gemfactory/src/constants.rs:1-7` — `FLOOR_RATE` 8, `CALL_RATE` 128
- `crates/core/gemfactory/src/runtime.rs:61,224` — writes `call_rate: 128`
- `crates/core/gemfactory/src/runtime.rs:452-468` — `derived_floor`, `derived_call_price`
- `contracts/precompiles/src/IGem.sol:5-17` — `GemData`, no call fields (**D-06**)

**Intex**
- `crates/core/intex/src/schema.rs:34-41` — `IntexCallTrigger`
- `crates/core/intex/src/schema.rs:181-199` — `CreateSeriesParams`
- `crates/core/intex/src/schema.rs:205-254` — `SeriesRecord`, orders 0-13, no `call_rate` (**D-05**)
- `crates/core/intex/src/tests.rs:15` — local `CALL_NOTICE_PERIOD` 21 d vs prod 7 d (**D-09**)
- `crates/core/intexfactory/src/constants.rs:38` — `QUALIFICATION_PERIOD` 21 d (**D-07**)
- `crates/core/intexfactory/src/constants.rs:40-52` — `PRICE_RATE_DEN`, `FLOOR_RATE` 8, `CALL_RATE` 128, windows
- `crates/core/intexfactory/src/config.rs:18-53` — `IntexParams` PROD / DEV profiles
- `crates/core/intexfactory/src/runtime.rs:50-51` — floor and call derivation
- `crates/core/intexfactory/src/runtime.rs:240-245` — `marked_up` (**D-08**)
- `crates/core/intexfactory/src/qualified.rs:173-192` — `try_qualify`, period + floor gate
- `crates/core/intexfactory/src/called.rs:214-274` — `try_call`, second 21-of-28 implementation
- `contracts/precompiles/src/IIntex.sol:26-40` — `SeriesData`, no `callRate`

**Nod**
- `crates/core/nod/src/schema.rs:17-29` — `NodIssueParams`
- `crates/core/nod/src/schema.rs:33-71` — `NodItemState`, no entry/call fields (**D-02**, **D-04**)
- `crates/core/nod/src/schema.rs:77-101` — `NodBucketState`, `entry_price_minor` at order 4
- `crates/core/nod/src/schema.rs:166-169` — slot layout pin note
- `crates/core/nod/src/api.rs:45-57` — `add_nod` takes entry price as a side argument
- `crates/core/nod/src/hooks.rs:1-25` — bucket qualification, `floor < rate` only
- `crates/core/nod/src/repository.rs:623,659` — entry price folded into the bucket
- `crates/core/nodfactory/src/runtime.rs:41,60-71` — issuance, item built without entry price
- `crates/core/lysis/src/constants.rs:4-8` — `FLOOR_RATE_PERCENT` 8, unchecked `calc_floor_price` (**D-08**)
- `crates/core/lysis/src/program_v1/types.rs:101-115` — `NodActionV1` already carries entry price
- `crates/core/lysis/src/program_v1/artifacts.rs:700-703,747-777` — canonical encode/decode
- `crates/system/ocomp-protocol/src/result.rs` — `NodActionV1` wire struct
- `contracts/precompiles/src/INod.sol:38-51` — `NodData`, no entry/call fields (**D-06**)

**Credis**
- `crates/core/credis/src/schema.rs:16-69` — `Position`, orders 0-7 and 9-13, order 8 free (**D-03**)
- `crates/core/credis/src/schema.rs:8-11` — `NUMBER_OF_ANADOSIS` 10, `SECONDS_PER_MONTH`
- `crates/core/credis/src/runtime.rs:63-102` — `create_position`, `entry_price_minor: rate`
- `crates/core/credisfactory/src/runtime.rs:80-99` — currency and rate resolution from the pledge
- `crates/core/credisfactory/src/lifecycle.rs:35-75` — time-based expiry sweep, not a price trigger
- `crates/core/credisfactory/src/runtime.rs:202` — `expire_position` collateral burn
- `contracts/precompiles/src/ICredis.sol:18-33` — `Position`, entry price only (**D-06**)

**Shared infrastructure**
- `crates/system/cycle/src/triggers.rs:12-25` — `TriggerId`, append-only contract
- `crates/system/cycle/src/triggers.rs:60-99` — `TriggerHandler` dispatch
- `crates/system/cycle/src/triggers.rs:113-186` — `active_triggers`, `[TriggerSpec; 5]`
- `crates/system/cycle/src/triggers.rs:171-180` — `gem_call_daily` spec, the template
- `crates/system/cycle/src/triggers.rs:211-229` — positional array test
- `crates/core/common/src/lib.rs` — currently exports `pow` and `worldwideday` only
- `crates/core/desis/src/runtime.rs:192-193` — external consumer of `marked_up`
- `scripts/seed_genesis.py:716-795` — gem storage seeder, slot map and the `228` default (**D-01**)
- `mise.toml:51-53` — `export-abi` task
- `mcp/src/tools/intex.ts:293-295` — the only client surfacing all three prices
- `docs/adr/core/ADR-C-GEM-001-gem-ledger.md:26-36` — stale lifecycle (**D-11**)
