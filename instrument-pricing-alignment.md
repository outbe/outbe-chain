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
Section 4 is the executable plan. Section 7 records two decisions that were taken outside
the codebase and cannot be re-derived from it — **read it before Section 4.** Section 8 is
the exhaustive alignment checklist: every axis on which the four rights must agree, 48
numbered items, each with its current status per instrument and the required action.
Section 9 is the **symbol-level** list underneath it — every type, attribute, constant,
function, storage column, event and error the four use for the same concept, what each
calls it today, the canonical name, and a mechanical rename map (R-01…R-17). If you want
the full list of what must be addressed rather than an execution order, start at Section 8
and read Section 9 alongside it.

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
**reference currency** (ISO 4217 numeric), all frozen at issuance:

| Price | Meaning | Derivation |
|---|---|---|
| **Entry Price** | The COEN/`<reference>` rate observed at issuance. The anchor from which the other two derive. | Read from the Oracle at issuance. |
| **Floor Price** | The level whose upward breach **qualifies** the instrument. | `entry × (100 + FLOOR_RATE) / 100` |
| **Call Price** | The level whose sustained upward breach **force-calls** the instrument. | `entry × (100 + CALL_RATE) / 100` |

**Scale is not global — see §1.5.** Each reference currency defines *two* denominations: a
minor-unit scale for amounts (USD → 2 digits) and a rate scale for the COEN/currency
quote (USD → 6 digits). The three prices above are *rates* and live at the rate scale;
`cost_amount` and settlement figures are *amounts* and live at the minor-unit scale.
Neither is defined anywhere in the repo today, which is defect **S-01**.

`FLOOR_RATE` and `CALL_RATE` are **percentage-point markups over entry**, matching the
convention already in the code (`crates/core/intexfactory/src/constants.rs:40-45`,
`crates/core/gemfactory/src/constants.rs:1-7`). A rate of `8` yields `1.08×`; a rate of
`128` yields `2.28×`.

> **Read this before touching any number.** `128` does **not** mean "×1.28". It means
> "+128 percentage points", i.e. ×2.28. Confusing the two is the root cause of defect
> **D-01** below. This reading is confirmed, not assumed — see §7.1.

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

#### "21 days in **any** 28-day window"

The requirement is *any* 28-day window, not only the one ending today. A daily scan over
the trailing 28 days satisfies this exactly, provided two properties hold — both already
true of the reference implementation, and both easy to break:

1. **Breach counts are recomputed from oracle history on every run, never accumulated.**
   `crates/core/gem/src/hooks.rs:129-133` and `crates/core/intexfactory/src/called.rs:1-6`
   both state this explicitly. A persisted counter would answer a different question.
2. **The scan runs every day.** The window ending on day `D` is evaluated exactly once,
   on day `D+1`. Run daily, the trailing window sweeps across every contiguous 28-day
   window in turn, so "≥21 breaches in some 28-day window" ≡ "on some day, the trailing
   window had ≥21 breaches".

A skipped run therefore drops one window from evaluation. Both scans skip when the oracle
watermark lags (`crates/core/gem/src/hooks.rs:142-146`,
`crates/core/intexfactory/src/called.rs:44-48`). In practice a breach pattern strong
enough to hit 21/28 persists into the next day's window too, so only the knife-edge case —
exactly 21 breaches in exactly one window — can be missed. Worth knowing; not worth
adding backfill machinery for.

**Minimum age before a call is possible: ~21 days.** The walk terminates at
`day < issued_day` (`crates/core/gem/src/runtime.rs:66-68`), so an instrument younger
than the threshold cannot accumulate 21 countable days whatever the price does. Days with
no finalized VWAP are not breaches either (`:69-73`), which delays it further. This is
correct behaviour, not a bug — but it means no test can observe a call without advancing
the clock at least 21 days past issuance.

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
2. `floor_price_minor == entry_price_minor * (100 + FLOOR_RATE) / 100`, **rounded per the
   rule fixed in §1.5.1** — not by whichever division happens to be written
3. `call_price_minor == entry_price_minor * (100 + CALL_RATE) / 100`, same rounding rule
4. `entry_price_minor < floor_price_minor < call_price_minor`
5. `call_rate` stored on the record equals the instrument's `CALL_RATE` constant at the
   time of issuance, and re-deriving the call price from the stored `entry_price_minor`
   and stored `call_rate` reproduces the stored `call_price_minor` exactly.
6. `call_window == 28 * 86_400`, `call_threshold == 21 * 86_400`,
   `call_notice_period == 7 * 86_400` (production profile).
7. `call_threshold <= call_window`.
8. Every price field present in the Rust record is also present on the corresponding
   Solidity view struct in `contracts/precompiles/src/`.
9. Every reference currency has **both** denominations registered — `d_amount` and
   `d_rate` (§1.5) — and no instrument can be issued in a currency missing either.
10. Every field's scale is the one §1.5 assigns to its kind, and the field's name says so:
    `_minor` only for genuine minor units, `_scaled` for fixed-point.

Invariant 5 is the one that is *silently violated today by stored data* — see D-01.

### 1.5 Scale and denomination model (normative)

Two different quantities are being measured and they do **not** share a scale. Every
reference currency must define both.

| | Symbol | What it scales | USD | Example others |
|---|---|---|---|---|
| **Amount denomination** | `amount_scale(iso)` = 10^`d_amount(iso)` | Quantities *of the currency* — cost, settlement, payout | **2** (cents) | JPY 0, KWD 3, BHD 3 |
| **Rate denomination** | `rate_scale(iso)` = 10^`d_rate(iso)` | The COEN/`<iso>` **quote** — entry, floor, call, VWAP | **6** | per-currency, registered |

`d_amount` follows the ISO 4217 minor-unit exponent. `d_rate` is a protocol parameter per
currency, not an ISO value — it is the precision at which the protocol quotes and
compares COEN against that currency.

#### Which field is which

| Field | Kind | Scale |
|---|---|---|
| `entry_price_minor`, `floor_price_minor`, `call_price_minor` | rate | `rate_scale(reference_currency)` |
| Oracle daily VWAP, current COEN rate | rate | `rate_scale(reference_currency)` |
| `cost_amount_minor` | amount | `amount_scale(issuance_currency)` |
| Settlement / payout figures | amount | the **payment token's** `decimals()` |
| `<x>_load_minor` (Gem, Gratis, Promis loads) | protocol-token amount | that token's decimals (18) — **not** a fiat scale |

Three distinct scales therefore meet in one formula, and mixing them is the failure mode
this section exists to prevent:

```
cost_amount = entry_price × load × amount_scale(issuance)
              ÷ ( rate_scale(reference) × token_scale(load) )
```

#### Rules

1. **Markups are dimensionless.** `floor = entry × 108/100` and
   `call = entry × (100 + CALL_RATE)/100` preserve whatever scale `entry` is in, so the
   WP-0 `marked_up` helper needs no scale parameter. This is the one piece of the
   pricing arithmetic that is already scale-safe.
2. **Never compare across scales.** A VWAP and a call price may only be compared after
   both are at `rate_scale` of the *same* currency. The existing per-ISO namespacing of
   the bin tries (`crates/core/gem/src/state.rs:270-272`) is the same invariant one level
   down; extend it, do not weaken it.
3. **Convert at boundaries, once.** Every conversion happens at a named boundary — oracle
   ingest, ABI read, cross-chain wire, settlement in a payment token — and nowhere else.
   No implicit rescaling inside pricing arithmetic.
4. **Rounding direction is declared, not incidental.** See the decision in §1.5.1.
5. **`_minor` means the currency's minor unit.** Today it does not (S-02). Any field that
   is fixed-point rather than minor-unit must be named `_scaled`, not `_minor`.

#### 1.5.1 Internal representation — decide before implementing

The repo currently hardcodes `SCALE_1E18` (`crates/blockchain/primitives/src/units.rs:14`)
for every price everywhere. Two ways to introduce per-currency scale:

| | **Option A — store at per-currency scale** | **Option B — register scales, keep 1e18 internal (recommended)** |
|---|---|---|
| Stored value | `entry_price` at `rate_scale(iso)` | `entry_price` at 1e18 |
| Registry role | defines storage | defines every I/O boundary + rounding |
| Blast radius | every price field, all bin math, Lysis canonical encoding (**semantics hash**), genesis, all four ABIs | a new registry + conversion at ~4 named boundaries |
| Precision in the 21-of-28 comparison | limited to `d_rate` | full |

**Option B is recommended, and one finding makes it close to forced.**
`price_to_bin` → `convert_decimal_price_to_128x128(price_18dec)`
(`crates/blockchain/primitives/src/math/price_helper.rs:76-78`) hardcodes `PRECISION = 1e18`
— the parameter is *named* `price_18dec`. Every qualification scan and Intex's call scan
route prices through it (`gem/state.rs:256-262`, `nod/src/hooks.rs:127`,
`intexfactory/called.rs:99`). Feed it a USD price at 6 dp under Option A and it lands ~12
orders of magnitude off in the bin ladder, **silently mis-qualifying every instrument** —
no error, just wrong bins. Option A therefore additionally requires making `price_to_bin`
scale-aware and rebuilding every existing bin index. Option B leaves the ladder untouched.

Under Option B the registry is still mandatory — it is the only place that knows USD is
2/6 — it simply governs conversion and rounding rather than storage.

**Open decision — rounding of derived prices.** A call price derived as `entry × 2.28`
carries digits below `d_rate`. Rounding it **down** makes calls marginally easier to
trigger; **up** makes them harder; not rounding leaves the threshold more precise than
any quotable rate. This is economically meaningful at the margin and must be chosen
deliberately, per price:

* `floor_price` — round **down** (easier to qualify) or **up** (harder)?
* `call_price` — round **down** (easier to call) or **up** (harder)?
* `cost_amount` — `derived_cost_amount` already rounds **up** via `div_ceil`
  (`crates/core/intexfactory/src/runtime.rs:259`), i.e. in the protocol's favour. Keep
  and apply uniformly.

Recommendation: round derived *thresholds* (floor, call) **up** — an instrument is never
called on a price movement smaller than the currency can quote — and keep amounts
rounding **up** as they already do. State the choice in the ADR; do not leave it to
whichever `/` happens to be written.

---


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

#### Credis — a different shape today, being rewritten as a right

`Position` (`crates/core/credis/src/schema.rs:16-69`) is a ten-installment debt schedule:
principal, outstanding amounts, a pinned `currency_rate`, and `entry_price_minor`.
There is **no floor price, no call price, no call parameters, no qualification, no
lifecycle state enum, and no daily call scan.** The only forced-termination path is a
begin-block *expiry* sweep that burns collateral once the schedule's end date passes with
an outstanding balance (`crates/core/credisfactory/src/lifecycle.rs:42-75`,
`crates/core/credisfactory/src/runtime.rs:202`). That is a time trigger, not a price
trigger; it is not the mechanism in §1.2.

**This shape is going away.** Credis is scheduled to be rewritten as a *right* of the same
kind as Nod, Intex and Gem — not a loan (§7.2). The audit above therefore describes code
with a limited remaining life. WP-4 is written as a conformance specification for the
rewritten instrument rather than a patch to the schema above; do not implement pricing
against today's `Position`.

---

## 3. Defect register

Each defect has a stable ID used by the work plan in §4.

| ID | Severity | Defect |
|---|---|---|
| **D-01** | **High** | Gem `call_rate` is written as `228` by the genesis seeder (`scripts/seed_genesis.py:770`) and the test fixture (`crates/core/gem/src/tests.rs:33`), but as `128` by GemFactory (`crates/core/gemfactory/src/runtime.rs:61,224`). Under the field's own documented formula (`crates/core/gem/src/schema.rs:82-86`) `228` means ×3.28, so seeded gems carry a rate that contradicts their own stored call price. Invariant 5 violated in production genesis data. |
| **D-02** | **High** | Nod has no call price, no call rate, no call window/threshold/notice, no `called_at`, no Called state and no daily call scan. Required: `CALL_RATE = 256`. |
| **D-03** | **High** | Credis has no floor price, no call price, no call parameters, no qualification state and no daily call scan. Required: `CALL_RATE = 64` (call price 1.64 × entry, upside). Closed by the Credis rewrite (§7.2); WP-4 is that rewrite's conformance spec, not a patch to today's schema. |
| **D-04** | Medium | `NodItemState` has no `entry_price_minor`; the value exists at issuance and in the certified artifact but is folded only into `NodBucketState`. The item cannot re-derive its own floor or call price. |
| **D-05** | Medium | Intex derives the call price from `IntexParams::call_rate` but never snapshots the rate onto `SeriesRecord`, so a series' rate is not auditable from its record and a config change silently detaches history. |
| **D-06** | Medium | Solidity view structs are out of sync with the Rust records. `IGem.GemData` (`contracts/precompiles/src/IGem.sol:5-17`) omits every call field. `INod.NodData` (`contracts/precompiles/src/INod.sol:38-51`) omits `entryPriceMinor` and every call field. `ICredis.Position` (`contracts/precompiles/src/ICredis.sol:18-33`) omits floor and call. `IIntex.SeriesData` omits `callRate`. |
| **D-07** | Medium | Qualification preconditions diverge: Intex requires `issued_at + QUALIFICATION_PERIOD (21 d)` **and** `rate > floor` (`crates/core/intexfactory/src/qualified.rs:185-191`); Gem and Nod require only `rate > floor` (`crates/core/gem/src/runtime.rs:27-29`, `crates/core/nod/src/hooks.rs:6-8`). If these are one instrument, the precondition must be one rule. |
| **D-08** | Low | The markup helper is duplicated three times with three signatures: `intexfactory::runtime::marked_up` (`:240-245`, `u16` rate), `gemfactory::runtime::derived_floor` / `derived_call_price` (`:452-468`, `u64` rate), `lysis::constants::calc_floor_price` (`:6-8`, unchecked multiply). The Lysis one can overflow-panic where the other two return an error. |
| **D-09** | Low | `crates/core/intex/src/tests.rs:15` sets a local `CALL_NOTICE_PERIOD = 21 days` against the production constant of 7 days, so the Intex ledger tests assert against a notice period the chain never uses. |
| **D-10** | Low | Lifecycle-state representation diverges: Nod uses two independent booleans (`is_qualified` on the bucket, `is_settled` on the item), Credis has no state at all, Gem has a 4-state enum, Intex a 3-state enum (no `Settled`). |
| **D-11** | Low | `ADR-C-GEM-001` (`docs/adr/core/ADR-C-GEM-001-gem-ledger.md:26-36`) documents the lifecycle as `Issued → Qualified → Settled → Burned` and states *"Burn is allowed only from Settled"*. The implemented lifecycle includes `Qualified → Called → forfeit-burn` (`crates/core/gem/src/runtime.rs:90-106`). The ADR is stale and contradicts the code. |
| **D-12** | Low | `crates/core/nod/src/schema.rs:168` names `tests::nod_contract_slot_layout_is_pinned` as the tripwire protecting the Nod slot layout, but **no such test exists** anywhere in the workspace (`grep -rn nod_contract_slot_layout_is_pinned --include='*.rs' crates/` returns only that doc comment). WP-3 appends fields to Nod storage with no layout guard in place. Write the test before WP-3b, modelled on `crates/core/gem/src/tests.rs:517`. |
| **D-13** | **Medium** | The Gem **daily call scan is unbounded**. `crates/core/gem/src/hooks.rs:150-157` materializes every id in `callable_gems` into a `Vec` and iterates all of them with no per-block budget and no resumable cursor. Gem's *qualify* scan is bounded (`MAX_GEM_QUALIFICATIONS_PER_BLOCK = 256`, `hooks.rs:40`) and Intex bounds **both** its scans (`MAX_SERIES_PER_BLOCK = 256`, `call_scan_cursor`, `call_currency_cursor`). The daily trigger's system transaction therefore grows without limit as the callable-gem population grows. Independent of this alignment work; fix it here so the pattern copied into the new Nod and Credis scans is the bounded one. See §8.3 A-20b. |
| **S-01** | **High** | **No per-currency scale is defined anywhere in the repo.** The oracle's reference-currency registry is a bare `StorageVec<u16>` of ISO codes plus `Mapping<u16, U256>` of rates (`crates/system/oracle/src/schema.rs:202,226`) — no amount denomination, no rate denomination. `outbe_primitives::stablecoin` carries `ISO_4217_NUMERIC_CODES` and `ISO_4217_ALPHA` but **no minor-unit exponent table** (`crates/blockchain/primitives/src/stablecoin.rs:36-72`). Every price is hardcoded to `SCALE_1E18` (`crates/blockchain/primitives/src/units.rs:14`). There is nowhere to look up "USD is 2 for amounts, 6 for the COEN rate". See §1.5. |
| **S-02** | **High** | **The `_minor` suffix is inaccurate on every price field.** `_minor` should mean "in the currency's minor unit" — USD cents, 2 digits. In practice `entry_price_minor`, `floor_price_minor`, `call_price_minor` and `cost_amount_minor` are all 1e18 fixed-point. The field names assert a denomination the values do not have, across all four instruments and all four ABIs. Either rename to `_scaled`, or convert the values to genuine minor units; do not leave the name contradicting the value. |
| **S-03** | Medium | **Scale conversion happens at exactly one site and only for Intex.** `derived_cost_amount(entry, load, payment_decimals)` computes `exp = 36 - payment_decimals` and divides (`crates/core/intexfactory/src/runtime.rs:249-261`), reading the token's own `decimals()` via `erc20_decimals` (`:636`). Gem divides by a flat `SCALE_1E18` (`gemfactory/src/runtime.rs:443-450`), Nod takes `cost_amount_minor` from Lysis at 1e18, and Credis pins a 1e18 `currency_rate`. So three of the four never convert into the issuance currency's actual denomination at all, and the one that does derives it from the payment token rather than from a currency registry. |
| **S-04** | Medium | **The bin ladder hardcodes 1e18 and would silently mis-qualify under a per-currency price scale.** `convert_decimal_price_to_128x128(price_18dec)` uses `PRECISION = 1e18` (`crates/blockchain/primitives/src/math/price_helper.rs:76-78`; the parameter is named `price_18dec`). Every qualification scan and Intex's call scan feed prices through it (`gem/state.rs:256-262`, `nod/src/hooks.rs:127`, `intexfactory/called.rs:99`). A 6-dp price would land ~12 orders of magnitude off in the ladder with no error raised. This is the constraint that makes §1.5.1 Option B the recommended route. |
| **S-05** | Medium | **Rounding direction for derived prices is undeclared.** `floor` and `call` derive from `entry` by a markup and carry digits below any plausible rate denomination. `marked_up` truncates (`intexfactory/runtime.rs:240-245`), `derived_cost_amount` rounds up via `div_ceil` (`:259`), and `lysis::calc_floor_price` truncates with an unchecked multiply (`lysis/constants.rs:6-8`). Rounding a call price down makes calls marginally easier to trigger and up makes them harder — an economically meaningful choice currently decided by whichever `/` was written. See §1.5.1. |
| **N-01** | Medium | `IGem.GemData` drops the `Minor` suffix every other interface uses — bare `entryPrice` / `floorPrice` / `costAmount` / `gemLoad` (`contracts/precompiles/src/IGem.sol:5-17`) against `entryPriceMinor` etc. in `IIntex`, `INod` and `ICredis`. Breaking ABI rename; see §9.3 and R-01. |
| **N-02** | Medium | `INod.NodData` exposes the entry price as **`costOfGratisMinor`** — `crates/core/nod/src/precompile.rs:145` reads `costOfGratisMinor: bucket.entry_price_minor`. The same quantity is emitted as `entryPriceMinor` by `INodFactory.NodIssued` (`crates/core/nodfactory/src/runtime.rs:85`), so one value is spelled two ways across Nod's own ABI. `cost_of_gratis_minor` is not a field on `NodItemState` at all; it survives only in Lysis as the oracle VWAP input to `cost_amount_minor`. See §9.3 and R-02. |
| **L-01** | **Medium** | The callable set is built two structurally different ways, and this — not the missing budget alone — is the root of D-13. Intex indexes Qualified series in a **price-keyed LB bin trie** and its daily scan walks only bins at or below the day's VWAP bin (`crates/core/intexfactory/src/called.rs:119-129`), so a series whose call price is above today's VWAP is never read. Gem keeps a **dense list** and visits every callable gem daily (`crates/core/gem/src/hooks.rs:150-157`). The two differ in complexity class: work proportional to the *breached* population versus the *entire callable* population. Port Intex's structure to Gem; build the new Nod and Credis callable sets the same way. See §9.7. |
| **L-02** | Low | Scan-cursor granularity differs. Nod's `unqualified_bin_scan_cursor` resumes *within* a bin; Gem's and Intex's `qualify_scan_cursor` resume at bin boundaries and process whole bins atomically, overshooting the per-block budget when a bin exceeds the remaining allowance (`crates/core/gem/src/hooks.rs:58-60`). Nod's is finer-grained and cannot starve a large tail bin. Pick one and record the rationale. See §9.7. |
| **N-03** | Low | `GemBurned` is declared on **two** interfaces (`contracts/precompiles/src/IGem.sol:47`, `IGemFactory.sol:50`) and carries two meanings — forfeit-burn after the notice period lapses (`crates/core/gem/src/runtime.rs:100-104`) and post-mining burn. A consumer cannot distinguish a forfeit from a settled burn. Split into `GemForfeited` and `GemBurned`. `IIntexFactory.Settled` is likewise the only unprefixed lifecycle event beside `SeriesIssued` / `SeriesQualified` / `SeriesCalled`. See §9.8, R-03, R-04. |
| **D-14** | Medium | **Intex has no origin-chain forfeit.** Gem forfeit-burns a Called record once the notice period lapses (`crates/core/gem/src/runtime.rs:90-106`). Intex marks Called, notifies the target chain (`crates/core/intexfactory/src/called.rs:264,276-293`) and stops — the only burns in `intexfactory` are `burn_ownerless_proceeds`, unrelated creator-reward dust. Forfeit is computed and applied target-side (`contracts/intex/src/shared/libs/IntexMetadata.sol:41`, IntexNFT1155 `burnSettled`). This may be correct by construction, since Intex holders are ERC-1155 balances on other chains, but it is currently undocumented and undecided. See §8.2 A-17b. |

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

### WP-0b — Per-currency scale registry *(fixes S-01…S-05)*

**Read §1.5 first, and settle §1.5.1 (Option A vs B, and rounding) before writing code.**
This package assumes **Option B**: register the scales, keep 1e18 internal, convert at
named boundaries. Under Option A the storage and Lysis-encoding work from WP-3d expands
substantially and every bin index must be rebuilt.

Do this **before WP-1…WP-4**: the invariant tests in §6 assert exact price equalities, and
their expected values depend on the rounding rule chosen here.

#### WP-0b.1 — ISO 4217 minor-unit table

`crates/blockchain/primitives/src/stablecoin.rs` already carries `ISO_4217_NUMERIC_CODES`
and `ISO_4217_ALPHA` as positionally aligned slices, pinned to a dated snapshot
(`ISO_4217_SNAPSHOT_PUBLISHED`, `ISO_4217_SNAPSHOT_SHA256`). Add a third aligned slice:

```rust
/// ISO 4217 minor-unit exponents, positionally aligned with
/// [`ISO_4217_NUMERIC_CODES`]. USD => 2, JPY => 0, KWD => 3.
pub const ISO_4217_MINOR_UNITS: &[u8] = &[ /* … */ ];

const _: () = assert!(ISO_4217_MINOR_UNITS.len() == ISO_4217_NUMERIC_CODES.len());

/// Minor-unit exponent for an ISO 4217 numeric code.
pub fn iso_4217_minor_units(iso_code: u16) -> Option<u8> { /* binary_search, as iso_4217_alpha */ }
```

Mirror the existing `const _: () = assert!` length check at `:72` and extend whatever
regenerates the snapshot so the three slices cannot drift apart.

#### WP-0b.2 — Rate denomination in the oracle registry

`d_rate` is a **protocol parameter, not an ISO value**, so it belongs with the
reference-currency registry rather than the static table. The registry today is
`reference_currencies: StorageVec<u16>` plus `reference_currency_rate: Mapping<u16, U256>`
(`crates/system/oracle/src/schema.rs:202,226`). Add:

```rust
/// Quote precision for the COEN/<iso> pair, in decimal digits. USD => 6.
/// Registered per reference currency; distinct from the currency's ISO
/// minor-unit exponent, which scales amounts rather than the quote.
#[attribute(order = /* next free */)]
pub reference_currency_rate_decimals: Mapping<u16, u8>,
```

Append-only per §5.1. Seed it wherever reference currencies are registered
(`crates/system/oracle/src/genesis.rs`) and reject registration of a currency without a
rate denomination — an unset entry reading as `0` would mean "integer COEN rate", which is
never intended and would round every price to a whole unit.

#### WP-0b.3 — Accessors in `outbe_common::pricing`

Alongside the WP-0 helpers:

```rust
/// Amount denomination: 10^d_amount(iso). USD => 100.
pub fn amount_scale(iso_code: u16) -> Result<U256>;
/// Rate denomination: 10^d_rate(iso). USD => 1_000_000.
pub fn rate_scale(storage: &StorageHandle<'_>, iso_code: u16) -> Result<U256>;

/// Internal fixed-point scale. Every stored price is at this scale under Option B.
pub const INTERNAL_PRICE_SCALE: U256 = SCALE_1E18;

/// Rate at `rate_scale(iso)` -> internal 1e18.
pub fn rate_to_internal(storage: &StorageHandle<'_>, iso: u16, v: U256) -> Result<U256>;
/// Internal 1e18 -> rate at `rate_scale(iso)`, rounding per §1.5.1.
pub fn internal_to_rate(storage: &StorageHandle<'_>, iso: u16, v: U256) -> Result<U256>;
/// Internal 1e18 amount -> the currency's minor units.
pub fn internal_to_amount(iso: u16, v: U256) -> Result<U256>;
/// Internal 1e18 amount -> a payment token's minor units (token decimals, not ISO).
pub fn internal_to_token(v: U256, token_decimals: u8) -> Result<U256>;
```

`amount_scale` is pure (static ISO table); `rate_scale` needs storage (registry). Keep
that asymmetry visible in the signatures rather than forcing both through storage.

#### WP-0b.4 — Apply the rounding decision

Once §1.5.1 is settled, make it explicit at each derivation and delete the incidental
behaviour:

* `pricing::marked_up` — thresholds; currently truncates.
* `pricing::floor_price` — currently truncates (`lysis::calc_floor_price`, and unchecked).
* `derived_cost_amount` — already `div_ceil`; keep, and document that amounts round up in
  the protocol's favour.

Add a test per rule asserting the direction on a value that does not divide evenly. A
truncating threshold and a ceiling threshold differ by one minor unit, which is exactly
the margin a call turns on.

#### WP-0b.5 — Convert at the boundaries, and only there

Four named boundaries. Each gets one conversion call and a comment naming the scale on
both sides:

| Boundary | Direction | Helper |
|---|---|---|
| Oracle ingest (rate published) | `rate_scale(iso)` → internal | `rate_to_internal` |
| ABI / RPC read (`getGemStatus`, `seriesData`, `nodData`, `getPosition`) | internal → `rate_scale(iso)` / `amount_scale(iso)` | `internal_to_rate` / `internal_to_amount` |
| Cross-chain wire (Intex) | internal → wire | existing `to_wire_price`, `ORACLE_TO_WIRE_SCALE = 1e9`, `PRICE_DECIMALS = 9` |
| Settlement in a payment token | internal → token minor units | `internal_to_token` (today's `derived_cost_amount`) |

Intex's wire scale (1e9) is a **third** scale, independent of both denominations, fixed by
the cross-chain codec (`contracts/intex/src/shared/libs/IntexMetadata.sol:21`,
`crates/core/intexfactory/src/constants.rs:54-56`). Leave it alone — but note it now has
to survive a round trip through the rate denomination without loss, so
`d_rate(iso) <= 9` must hold for any currency Intex issues in. Assert it at registration.

`PRICE_PRECISION = 6` in `IntexMetadata.sol:22` is a **display** precision, coincidentally
equal to the USD rate denomination. Do not conflate them — one is a UI concern, the other
is consensus.

#### WP-0b.6 — Naming (S-02)

Decide and apply uniformly: either rename every fixed-point field `_scaled`
(`entry_price_scaled`), or convert the stored values to genuine minor units. Under
Option B the values stay fixed-point, so the rename is the honest fix — and it touches
every record, every ABI struct and the MCP decoders, so it belongs with the R-01/R-02
ABI renames in §9.11 rather than as a separate pass.

**Verify:** `cargo nextest run -p outbe-primitives -p outbe-common -p outbe-oracle`, plus
a test asserting `iso_4217_minor_units(840) == Some(2)`, `iso_4217_minor_units(392) == Some(0)`
(JPY), `iso_4217_minor_units(414) == Some(3)` (KWD), and that a reference currency cannot
be registered without a rate denomination.

---


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

### WP-4 — Credis: conform the rewritten instrument to the shared model *(fixes D-03)*

**Read §7.2 first.** Credis is being rewritten from a ten-installment debt schedule into a
*right* of the same kind as Nod, Intex and Gem. This work package is **not** a plan to
bolt prices onto the existing `Position` / `Anadosis` model — that model is superseded by
the rewrite. It is the **conformance specification** for the pricing and call surface the
rewritten Credis must expose.

Sequencing: if the rewrite has not started, hand this section to whoever does it. If the
rewrite has landed, use this section as a checklist against the result. Do not implement
WP-4 against today's `crates/core/credis` — the work would be discarded.

#### WP-4a — Record fields

The rewritten Credis record carries the same pricing and call group as `GemData`
(`crates/core/gem/src/schema.rs:33-105`), which is the cleanest reference:

| Field | Type | Value at issuance |
|---|---|---|
| `entry_price_minor` | `U256` | COEN/`<reference>` rate at issuance |
| `floor_price_minor` | `U256` | `pricing::floor_price(entry)` → 1.08 × entry |
| `call_price_minor` | `U256` | `pricing::marked_up(entry, CREDIS_CALL_RATE)` → **1.64 × entry** |
| `call_rate` | `u16` | `64` |
| `call_window` | `u32` | `28 * 86_400` |
| `call_threshold` | `u32` | `21 * 86_400` |
| `call_notice_period` | `u32` | `7 * 86_400` |
| `called_at` | `u64` | `0` until Called |
| `state` | `u8` | `CredisState::Issued` |

All derived through the WP-0 helpers — no literal rates at the call site.

`CredisState { Issued = 0, Qualified = 1, Called = 2, Settled = 3 }`, mirroring `GemState`
(`crates/core/gem/src/schema.rs:5-12`).

**Reference currency.** Today's Credis stores only `issuance_currency`
(`crates/core/credis/src/schema.rs:56-57`), derived from the disbursed asset's `isoCode()`.
The call scan needs a COEN/`<reference>` oracle pair, so the rewritten record must carry
`reference_currency: u16` as a first-class field like the other three
(`crates/core/gem/src/schema.rs:58-59`, `crates/core/nod/src/schema.rs:61-62`). Do not
reuse `issuance_currency` as if it were the reference currency.

**Storage layout.** A fresh contract is not bound by the append-only rule in §5.1 —
declare the fields in whatever order reads best. If the rewrite instead migrates the
existing schema in place, §5.1 binds: `Position` uses orders `0..=7` and `9..=13`, order 8
is an unused gap, append from 14.

#### WP-4b — Qualification

`Issued → Qualified` when the COEN rate for the record's `reference_currency` **strictly
exceeds** `floor_price_minor`. Strict comparison, matching
`crates/core/gem/src/runtime.rs:27-29` and the note at `crates/core/nod/src/hooks.rs:6-8`
(a record priced exactly at the rate stays unqualified). Monotonic latch — no path back
to Issued.

Subject to the D-07 resolution in WP-5: if Option B is chosen, Credis also gates on a
21-day maturity period. Under the recommended Option A there is no time gate.

Implementation: reuse the bounded-cursor sweep pattern already in
`crates/core/credisfactory/src/lifecycle.rs:42-75`, or port the LB bin index from
`crates/core/gem/src/state.rs:254-341` if the population is expected to be large. The
`ponytail` comment at `crates/core/credisfactory/src/lifecycle.rs:35-37` already flags the
existing sweep as O(n).

#### WP-4c — Daily call scan

Structurally identical to `crates/core/gem/src/hooks.rs:133-193` and
`crates/core/gem/src/runtime.rs:47-106`. Copy the shape rather than inventing one:

* Visit only the callable index (records in Qualified or Called).
* Guard on `utc_day_vwap_last_finalized` and skip the run if the watermark lags
  (`crates/core/gem/src/hooks.rs:142-146`).
* Cache one VWAP window per reference currency across the pass (`:163`, `:201-224`).
* Recompute breach counts from oracle history every run — **never accumulate a counter**
  (§1.2, "any 28-day window").
* Breach test is `vwap > call_price_minor` — **upside**, like the other three (§7.2).
* Isolate each record in a `with_checkpoint` so one bad record cannot halt the scan
  (`:182-190`).

Register `TriggerId::CredisCallDaily = 6` per WP-3e; both new triggers land in the same
array widening.

#### WP-4d — Called and forfeit

Same terminal path as the other rights, now that Credis is not debt:

* `Qualified → Called` stamps `called_at` and emits `PositionCalled`.
* The holder has `CALL_NOTICE_PERIOD` (7 days) from `called_at` to settle.
* Past `called_at + call_notice_period` unsettled, the record is forfeited — mirroring
  `GemContract::forfeit` (`crates/core/gem/src/runtime.rs:90-106`).
* Settlement transitions to `Settled` and leaves the callable index.

What "settle" and "forfeit" move — which balances, which vaults, whether the existing
collateral-burn path at `crates/core/credisfactory/src/runtime.rs:202` survives the
rewrite — is part of the rewrite's design, not this document (§7.2 scope boundary). The
*trigger* and its *timing* are specified here; the *effect* is specified there.

#### WP-4e — Events and view surface

`contracts/precompiles/src/ICredis.sol` gains `PositionCalled(uint256 indexed positionId,
uint64 calledAt)` and a forfeit event alongside the existing `CollateralBurned` (`:10`),
and its record struct gains the full price triple and call group per WP-5.

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
crates/core/credis/src/tests.rs   :: credis_pricing_invariants  (or wherever the
                                     rewritten Credis lands — see §7.2)
```

Each must assert, for its instrument:
`floor == entry * 108/100`, `call == entry * (100 + CALL_RATE)/100`,
`entry < floor < call`, `call_window == 28 * 86_400`,
`call_threshold == 21 * 86_400`, `call_notice_period == 7 * 86_400`,
and the stored `call_rate` re-derives the stored `call_price_minor`.

**Call-trigger behaviour tests** — one per instrument, and note the timing floor from
§1.2: a call cannot be observed until the clock advances **at least 21 days past
issuance**, because the breach walk terminates at `day < issued_day`. Each must cover:

* 20 breach days in the window → **not** called (below threshold).
* 21 breach days → called; `called_at` stamped; leaves the qualified set.
* 21 breach days spread across a window that is *not* the most recent one, with the scan
  run daily throughout → called on the day after that window closed. This is the test
  that pins "any 28-day window" (§1.2) and the one that fails if a future refactor
  replaces the recompute-from-history design with an accumulated counter.
* Called + notice period elapsed, unsettled → forfeited.
* Called + settled inside the notice period → `Settled`, not forfeited.

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

## 7. Resolved decisions

Both questions this document originally left open have now been answered. They are
recorded here because **the answers are not derivable from the codebase** — an agent that
re-derives them from first principles will get them wrong, as an earlier draft of this
document did.

### 7.1 Rate convention — percentage-point markup over entry ✅ **decided**

`64 / 128 / 256` are **markups**, not multipliers: 64 → ×1.64, 128 → ×2.28, 256 → ×3.56.
The same convention as the existing `FLOOR_RATE = 8` → ×1.08.

Requirement as stated: *"price of coen must be higher than entry 64% for 21 days in any 28
window."* So for Credis: `call_price = entry × 1.64`, breach test `vwap > call_price`,
threshold 21 days in any 28-day window — §1.2 applies unmodified, including the
"any window" semantics spelled out there.

This confirms the reading for all four instruments and matches the `CALL_RATE = 128`
already implemented for Intex and Gem (`crates/core/intexfactory/src/constants.rs:44-45`,
`crates/core/gemfactory/src/constants.rs:5-7`). Nothing in §1 changes. Invariant 4
(`entry < floor < call`) holds for every instrument, Credis included.

### 7.2 Credis is a right, not debt ✅ **decided**

**Credis is scheduled to be rewritten.** After the rewrite it is the same kind of
instrument as Nod, Intex and Gem — a *right* — not a loan. Everything in this document
applies to it unchanged: the same three prices, qualification on a floor breach, the same
21-of-28 call trigger, and the same notice-then-forfeit terminal path. Only the rate
differs (64).

Three things from the earlier draft are now void:

* The question "what does a Called *debt position* do?" dissolves — there is no borrower
  obligation to accelerate, and no lender.
* The downside margin-call reading (former §7.3) is **rejected**. Credis calls on the
  upside, `vwap > call_price`, exactly like the other three.
* WP-4 no longer bolts price fields onto the `Position` / `Anadosis` installment
  schedule. It is now a conformance specification for the rewritten Credis.

**Scope boundary.** This document does *not* specify the Credis rewrite — its scope,
sequencing and the fate of the existing debt schedule are decided elsewhere. It specifies
the pricing and call surface the rewritten Credis **must** expose, so the rewrite can be
checked against it. If you are the agent doing the rewrite, WP-4 is your acceptance
criteria for the pricing half of the job, not your design.

## 8. Full alignment checklist

§2.1 compares the four on *price fields alone*. This section is the exhaustive list: every
axis on which Nod, Intex, Gem and Credis must agree to be one instrument, with each one's
current position and the required action.

**The rights set is exactly these four.** Verified by inspection — `gratis`, `promis`,
`tribute` and `fidelity` carry no entry/floor/call price and no Qualified/Called
lifecycle in their schemas, so they are not rights and are out of scope. Promis is the
payload a right yields on mining; Gratis and Tribute are inputs; Fidelity is a cohort
ledger.

Legend: ✅ aligned · ⚠️ partial or divergent · ❌ absent · — not applicable

---

### 8.1 Economic terms

| # | Axis | Nod | Intex | Gem | Credis | Action |
|---|---|---|---|---|---|---|
| **A-01** | `entry_price_minor` on the record | ⚠️ bucket only (`nod/schema.rs:94`) | ✅ `:222` | ✅ `:47` | ✅ `:68` | WP-3a: add to `NodItemState` |
| **A-02** | `floor_price_minor` on the record | ✅ `:50` | ✅ `:225` | ✅ `:53` | ❌ | WP-4a |
| **A-03** | `call_price_minor` on the record | ❌ | ✅ `:228` | ✅ `:70` | ❌ | WP-3b, WP-4a |
| **A-04** | Floor rate = **8** (1.08×) | ✅ `lysis/constants.rs:4` | ✅ `intexfactory/constants.rs:42` | ✅ `gemfactory/constants.rs:3` | ❌ | WP-0 shared constant |
| **A-05** | Call rate constant | ❌ needs **256** | ⚠️ 128 correct but **not stored** | ⚠️ 128 in factory, **228** in seeder + fixture | ❌ needs **64** | WP-1, WP-2, WP-3b, WP-4a |
| **A-06** | `call_window` = 28 d | ❌ | ✅ | ✅ | ❌ | WP-3b, WP-4a |
| **A-07** | `call_threshold` = 21 d | ❌ | ✅ | ✅ | ❌ | WP-3b, WP-4a |
| **A-08** | `call_notice_period` = 7 d | ❌ | ✅ | ✅ | ❌ | WP-3b, WP-4a |
| **A-09** | All terms **snapshotted at issuance** | — | ⚠️ window/threshold/notice yes, `call_rate` no | ✅ all seven | — | WP-2 |
| **A-10** | One markup helper | ⚠️ `lysis::calc_floor_price`, unchecked multiply | ⚠️ `marked_up`, `u16` | ⚠️ `derived_floor`/`derived_call_price`, `u64` | — | WP-0 (D-08) |

### 8.2 Lifecycle model

| # | Axis | Nod | Intex | Gem | Credis | Action |
|---|---|---|---|---|---|---|
| **A-11** | State enum `Issued/Qualified/Called/Settled` | ❌ two bools: `is_qualified` (bucket), `is_settled` (item) | ⚠️ 3 states, **no `Settled`** | ✅ all 4 | ❌ none | WP-3c, WP-4a; decide Intex `Settled` |
| **A-12** | Qualify on **strict** `rate > floor` | ✅ `nod/hooks.rs:6-8` | ✅ `qualified.rs:190` | ✅ `gem/runtime.rs:27` | ❌ | WP-4b |
| **A-13** | Maturity gate before qualifying | none | ⚠️ **21 d** `QUALIFICATION_PERIOD` | none | — | **D-07 — pick one rule** (WP-5.4) |
| **A-14** | Qualification is a monotonic latch | ✅ | ✅ | ✅ | — | WP-4b preserve |
| **A-15** | Call on 21 breach days in **any** 28-day window | ❌ | ✅ | ✅ | ❌ | WP-3e, WP-4c |
| **A-16** | Breaches **recomputed** each run, never accumulated | — | ✅ | ✅ | — | WP-3e, WP-4c preserve (§1.2) |
| **A-17** | Notice lapse → forfeit | ❌ | ⚠️ **no origin-chain forfeit** — delegated to the target chain (`IntexMetadata.sol:41`, IntexNFT1155 `burnSettled`) | ✅ `gem/runtime.rs:90-106` | ❌ | WP-3e, WP-4d; **decide Intex** (A-17b) |
| **A-18** | Lifecycle timestamps | ⚠️ `issued_at` only | ⚠️ `issued_at`, `called_at` | ✅ all four | ⚠️ `created_at` only, and misnamed | Add `qualified_at`/`settled_at`; rename Credis to `issued_at` |

**A-17b — Intex forfeit is a real asymmetry, not an oversight to paper over.** Gem
forfeit-burns on the origin chain. Intex marks Called, notifies the target chain
(`intexfactory/called.rs:264,276-293`) and stops; the only burns in `intexfactory` are
`burn_ownerless_proceeds`, which is unrelated creator-reward dust. Because Intex holders
live in an ERC-1155 on target chains (A-25), the origin ledger has nothing to burn.
**Decide explicitly:** either accept that Intex's terminal step is cross-chain by
construction and document it as a permitted divergence, or add an origin-side terminal
state. Do not silently leave it undecided.

### 8.3 Scan infrastructure

| # | Axis | Nod | Intex | Gem | Credis | Action |
|---|---|---|---|---|---|---|
| **A-19** | Daily call trigger registered | ❌ | ✅ `TriggerId=1` | ✅ `TriggerId=4` | ❌ | WP-3e: append `=5`, `=6` |
| **A-20** | Call scan has per-block budget + resumable cursor | ❌ | ✅ `MAX_SERIES_PER_BLOCK=256`, `call_scan_cursor`, `call_currency_cursor` | ❌ **unbounded** — see A-20b | ❌ | **New: bound the Gem call scan** |
| **A-21** | Oracle watermark guard, skip if VWAP unfinalized | — | ✅ `called.rs:44-48` | ✅ `hooks.rs:142-146` | — | WP-3e, WP-4c copy |
| **A-22** | Per-record `with_checkpoint` isolation | — | ✅ `called.rs:146` | ✅ `hooks.rs:182` | — | WP-3e, WP-4c copy |
| **A-23** | Callable index (Qualified ∪ Called) | ❌ | ✅ qualified bin tree | ✅ `callable_gems` + index | ❌ | WP-3e, WP-4c |
| **A-24** | Unqualified bin index (LB radix-256 trie) | ✅ | ✅ | ✅ | ❌ linear sweep | WP-4b |

**A-20b — the Gem daily call scan is unbounded.** `gem/hooks.rs:150-157` reads
`callable_gems.len()` and materializes **every** id into a `Vec`, then iterates all of
them with no budget and no cursor. Gem's *qualify* scan is bounded
(`MAX_GEM_QUALIFICATIONS_PER_BLOCK = 256`); its *call* scan is not. Intex bounds both.
This is a liveness risk that grows with the gem population, independent of this
alignment work — as the callable set grows the daily trigger's system transaction grows
without limit. Port Intex's budget-plus-cursor pattern, and do **not** copy the unbounded
shape into the new Nod and Credis scans.

### 8.4 Identity, ownership and granularity — the structural axis

| # | Axis | Nod | Intex | Gem | Credis |
|---|---|---|---|---|---|
| **A-25** | What the record *is* | item per holder, but **qualification is per bucket** `(wwd, floor, ref_ccy)` | **a series** — per `(day, issuance, reference)`; **no owner field at all** | item per holder | position per holder |
| **A-26** | Owner field + dense enumeration | ✅ in the compressed-entity store | ❌ none — holders live in IntexNFT1155 on target chains | ✅ `owner_gem_counts` / `owner_gem_ids` / `all_gem_ids` / `gem_index` | ✅ `address_position_counts` / `address_position_ids` |
| **A-27** | ID derivation | Poseidon(owner, wwd) → `EntityId36` | 14 ASCII bytes, `20260212-TRY-U` | keccak(`"gem"` ‖ owner ‖ load ‖ **block number**) | keccak(commitment ‖ smart_account) |
| **A-28** | Transferable | no transfer surface | ✅ ERC-1155, cross-chain | ❌ explicitly `NonTransferable` (`gem/precompile.rs:47-49`) | no transfer surface |

**A-25 is the deepest divergence in this document and it is not a defect to fix — it is a
boundary to decide.** Gem and Credis are per-holder records. Nod is a per-holder item
whose *qualification* is decided at bucket granularity. Intex is a per-day series with no
owner, whose holders are ERC-1155 balances on other chains. So "call the instrument"
means three different blast radii:

* **Gem, Credis** — call one holder's record.
* **Nod** — qualification lands on a whole bucket; a call must be decided per item
  (`called_at` and the notice deadline are per holder) even though qualification is not.
  WP-3c already recommends this split; it is the correct resolution.
* **Intex** — calling a series calls **every holder of that series at once**, across every
  target chain.

Prices and trigger parameters align across all four regardless. Granularity does not, and
forcing it to would mean redesigning Intex's cross-chain model. **Recommendation: declare
A-25 a permitted divergence, document it in the new pricing ADR, and require only that the
*price terms and trigger arithmetic* be identical.** What differs is the set of holders a
single call resolves, not the condition that fires it.

A-27: four different id schemes, each keyed on that instrument's natural identity. Leave
them. One thing to note rather than change — Gem's id mixes in the issuing **block
number**, so it is not reproducible from `(owner, load)` alone; the genesis seeder
substitutes an index instead (`seed_genesis.py:700-708`).

### 8.5 Currency and scale

| # | Axis | Nod | Intex | Gem | Credis | Action |
|---|---|---|---|---|---|---|
| **A-29** | `reference_currency` on the record | ✅ | ✅ | ✅ | ❌ **only `issuance_currency`** | WP-4a — required for the oracle pair |
| **A-30** | `issuance_currency` on the record | ✅ | ✅ | ✅ | ✅ | — |
| **A-31** | Prices on the 1e18 oracle scale | ✅ | ✅ | ✅ | ✅ | — |
| **A-32** | Cross-chain wire scale | — | 1e9 via `ORACLE_TO_WIRE_SCALE` | — | — | Intex-only, correct |
| **A-33** | Prices comparable only within one reference currency; bin columns namespaced by ISO | ✅ | ✅ | ✅ | ❌ | WP-4b |
| **A-49** | Amount denomination `d_amount(iso)` registered (USD → 2) | ❌ | ❌ | ❌ | ❌ | **WP-0b.1** — nowhere in the repo (S-01) |
| **A-50** | Rate denomination `d_rate(iso)` registered (USD → 6) | ❌ | ❌ | ❌ | ❌ | **WP-0b.2** — nowhere in the repo (S-01) |
| **A-51** | Field name matches actual denomination | ❌ `_minor` on 1e18 values | ❌ | ❌ | ❌ | WP-0b.6 (S-02) |
| **A-52** | Conversion happens only at named boundaries | ❌ none | ⚠️ one site, `derived_cost_amount`, keyed off the **token**'s decimals not the currency's | ❌ flat `/SCALE_1E18` | ❌ 1e18 `currency_rate` | WP-0b.5 (S-03) |
| **A-53** | Rounding direction declared per derived price | ⚠️ truncates, unchecked | ⚠️ truncates; amounts `div_ceil` | ⚠️ truncates | — | WP-0b.4 (S-05) |

### 8.6 Naming and types

| # | Axis | Nod | Intex | Gem | Credis | Action |
|---|---|---|---|---|---|---|
| **A-34** | Load field | `gratis_load_minor` | `promis_load_minor` (`u128` in params, `U256` in storage) | `gem_load_minor` | `credis_principal` | Four names for one role. Converge on `<x>_load_minor`, or document the naming rule. |
| **A-35** | Cost field | ✅ stored `cost_amount_minor` | ⚠️ **derived**, `fn cost_amount_minor()` | ✅ stored | ❌ absent | Decide stored vs derived, apply uniformly |
| **A-36** | Timestamp width | `u64` | ⚠️ **`u32`** — `issued_at`, `called_at`; reverts past 2106 (`intexfactory/runtime.rs:43-44`, `called.rs:256-257`) | `u64` | `u64` | Widen Intex to `u64`, or document the 2106 bound as accepted |

### 8.7 External surfaces

| # | Axis | Nod | Intex | Gem | Credis | Action |
|---|---|---|---|---|---|---|
| **A-37** | Solidity view struct mirrors the Rust record | ❌ no entry, no call fields | ⚠️ missing `callRate` | ❌ no call fields | ❌ no floor, no call | WP-5.1 (D-06) |
| **A-38** | Lifecycle events | ⚠️ `NodBucketQualified` only | ⚠️ `SeriesCalled`, `SeriesIssued` | ✅ `GemQualified` / `GemCalled` / `GemBurned` | ⚠️ `PositionCreated` / `AnadosisPaid` / `CollateralBurned` | Uniform `<X>Qualified` / `<X>Called` / `<X>Forfeited` / `<X>Settled` |
| **A-39** | ABI JSON regenerated and committed | — | — | — | — | `mise run export-abi`, WP-5.2 |
| **A-40** | MCP tool surfaces all three prices | ❌ | ✅ `mcp/src/tools/intex.ts:293-295` | ❌ | ❌ | WP-5.3 |
| **A-41** | Genesis seeder support | ❌ not seeded | ❌ not seeded | ✅ `seed_gems` | ❌ not seeded | Only Gem needs the D-01 fix now; add others if genesis seeding is extended |
| **A-42** | Factory write surface shape | `settleNod` / `mineGratis` | `settle` / `minePromis` | `settleGem` / `mineGemPromis` | `requestCredis` / `anadosis` | Credis's will change with the rewrite; converge naming then |

### 8.8 Assurance

| # | Axis | Status | Action |
|---|---|---|---|
| **A-43** | Storage-layout pin test | Gem ✅ (`gem/tests.rs:517`); Nod ❌ — `nod/schema.rs:168` names a test that **does not exist** (D-12); Intex, Credis: none found | Write the Nod test **before** WP-3b; add Intex and Credis pins |
| **A-44** | Pricing-invariant test per instrument | none exist | §6 — one per instrument, asserting §1.4 invariants 1-7 |
| **A-45** | Call-behaviour test per instrument | Gem ✅ (`gem/tests.rs:425,535-589`); Intex partial; Nod, Credis ❌ | §6 — the five-case battery, including the not-most-recent-window case |
| **A-46** | Cross-instrument constants test | none | §6 — pin the §1.3 table verbatim |
| **A-47** | ADR accuracy | `ADR-C-GEM-001` stale (D-11); no ADR covers pricing as a cross-instrument concern | WP-5.5 — write `ADR-C-PRC-001` as the single normative source, link all four |
| **A-48** | Protocol constants documented | `docs/genesis-protocol-constants.md` contains **no pricing constants at all** — which is why D-01 survived | WP-5.5 — add the §1.3 table |

---

### 8.9 Summary — what must be addressed

**Must change to satisfy the stated requirement (blocking):**
A-01, A-02, A-03, A-05, A-06, A-07, A-08, A-11, A-12, A-15, A-17, A-19, A-23, A-29.

**Must change for the four to be genuinely one instrument (strongly recommended):**
A-09, A-10, A-13, A-18, A-20, A-24, A-33, A-37, A-38, A-43, A-44, A-45, A-46.

**Must be decided rather than fixed:**
A-13 (one qualification rule), A-17b (Intex terminal state), A-25 (call granularity —
recommend accepting the divergence), A-35 (cost stored vs derived), A-36 (Intex `u32`
timestamps), **§1.5.1 internal representation (Option A vs B) and the rounding direction
for derived prices — decide these first, they change the expected values in every pricing
test.**

**Scale — blocking, and upstream of everything else:**
A-49, A-50, A-51, A-52, A-53 (defects S-01…S-05). No per-currency denomination exists
anywhere in the repo today; WP-0b lands before WP-1…WP-4.

**Cosmetic, worth doing while the code is open:**
A-34, A-42, A-47, A-48.

**Found while compiling this list, not part of the alignment ask, fix anyway:**
A-20b (the Gem call scan is unbounded) and A-43/D-12 (the Nod layout pin test is
referenced but absent). Both are latent risks that this work would otherwise walk past.

## 9. Symbol-level conformance — names and logic

§8 lists the *axes*. This section lists the **actual identifiers**: every type, attribute,
constant, function, storage column, event and error the four rights use for the same
concept, what each calls it today, and the canonical name to converge on.

Rule of thumb used throughout: **Gem's record attributes** are the naming reference (they
are complete and already `_minor`-suffixed); **Intex's scan structure** is the logic
reference (it is the only one that bounds and prunes both scans).

---

### 9.1 Rust type names

| Role | Nod | Intex | Gem | Credis | Canonical |
|---|---|---|---|---|---|
| Ledger contract | `NodContract` | `IntexContract` | `GemContract` | `CredisContract` | ✅ consistent |
| Record | `NodItemState` | `SeriesRecord` | `GemData` | `Position` | **`<X>Data`** (Gem) |
| Aggregate record | `NodBucketState` | — | — | `Anadosis` | n/a |
| Issuance params | `NodIssueParams` | `CreateSeriesParams` | `GemAddParams` | *(9 positional args)* | **`<X>IssueParams`** |
| Lifecycle state | ❌ none | `IntexState` | `GemState` | ❌ none | **`<X>State`** |
| Call-trigger group | ❌ | `IntexCallTrigger` | *(flat fields)* | ❌ | `<X>CallTrigger` or flat — pick one |
| Identifier | `EntityId36` | `SeriesId` | `U256` | `U256` | leave (natural keys differ) |

Credis creating a position through nine positional arguments
(`crates/core/credis/src/runtime.rs:64-75`) rather than a params struct is the outlier.
The rewrite should adopt `CredisIssueParams`.

### 9.2 Record attribute names

Canonical column = the name all four should carry after alignment.

| Canonical attribute | Nod | Intex | Gem | Credis |
|---|---|---|---|---|
| `entry_price_minor` | ⚠️ on bucket only | ✅ | ✅ | ✅ |
| `floor_price_minor` | ✅ | ✅ | ✅ | ❌ add |
| `call_price_minor` | ❌ add | ✅ | ✅ | ❌ add |
| `call_rate` | ❌ add | ❌ add | ✅ | ❌ add |
| `call_window` | ❌ add | ✅ | ✅ | ❌ add |
| `call_threshold` | ❌ add | ✅ | ✅ | ❌ add |
| `call_notice_period` | ❌ add | ✅ | ✅ | ❌ add |
| `issued_at` | ✅ | ✅ | ✅ | ⚠️ **`created_at`** — rename |
| `qualified_at` | ❌ add | ❌ add | ✅ | ❌ add |
| `called_at` | ❌ add | ✅ | ✅ | ❌ add |
| `settled_at` | ❌ add | ❌ add | ✅ | ❌ add |
| `state` | ❌ `is_qualified` (bucket) + `is_settled` (item) | ✅ | ✅ | ❌ add |
| `issuance_currency` | ✅ | ✅ | ✅ | ✅ |
| `reference_currency` | ✅ | ✅ | ✅ | ❌ add |
| `owner` | ✅ | ❌ ownerless series (A-25) | ✅ | ⚠️ **`smart_account`** |
| `cost_amount_minor` | ✅ | ⚠️ derived, `fn cost_amount_minor()` | ✅ | ❌ |
| `<x>_load_minor` | `gratis_load_minor` | `promis_load_minor` | `gem_load_minor` | ⚠️ **`credis_principal`** |

**Load naming.** Nod / Intex name the load after *what it yields* (gratis, promis); Gem
names it after *itself*. Credis calls it a principal. Pick one rule and state it in the
ADR — the mechanical choice is `<instrument>_load_minor` everywhere, matching Gem.

**Owner naming.** Credis's `smart_account` is the owner slot under a different name.
Rename to `owner` in the rewrite, or document why the account abstraction makes it
distinct.

### 9.3 Solidity struct and field names

| Interface | Struct | Price field spelling |
|---|---|---|
| `IGem` | `GemData` | ⚠️ **`entryPrice`, `floorPrice`, `costAmount`, `gemLoad`** — no `Minor` suffix |
| `IIntex` | `SeriesData` | ✅ `entryPriceMinor`, `floorPriceMinor`, `callPriceMinor`, `costAmountMinor`, `promisLoadMinor` |
| `INod` | `NodData` | ⚠️ `floorPriceMinor`, `gratisLoadMinor`, `costAmountMinor` — but see below |
| `ICredis` | `Position` | ⚠️ mixed: `entryPriceMinor` but `totalGratisAmount`, `credisPrincipal` |

**Two concrete naming defects here.**

**N-01 — Gem's ABI drops the `Minor` suffix.** Three of four interfaces use
`<field>Minor`; `IGem.GemData` uses bare `entryPrice` / `floorPrice` / `costAmount`
(`contracts/precompiles/src/IGem.sol:5-17`). Renaming to `entryPriceMinor` etc. is a
**breaking ABI change** for anything decoding `getGemStatus`. Land it in the same commit
as the call-field additions from WP-5.1, since that struct is changing anyway, and
regenerate `abi-export/IGem.json`.

**N-02 — Nod's ABI calls the entry price `costOfGratisMinor`.**
`crates/core/nod/src/precompile.rs:145` reads `costOfGratisMinor: bucket.entry_price_minor`
— it is literally the entry price under another name. Meanwhile `INodFactory.NodIssued`
emits the *same quantity* as `entryPriceMinor`
(`crates/core/nodfactory/src/runtime.rs:85`). So Nod's own ABI surface spells one value
two ways across two interfaces. `cost_of_gratis_minor` exists nowhere on `NodItemState`;
it survives only in Lysis as the oracle VWAP input to `cost_amount_minor`. Rename the
ABI field to `entryPriceMinor` as part of WP-3a.

### 9.4 Constants — names and homes

| Concept | Nod | Intex | Gem | Credis |
|---|---|---|---|---|
| Floor rate | `lysis::FLOOR_RATE_PERCENT` | `intexfactory::FLOOR_RATE` | `gemfactory::FLOOR_RATE` | ❌ |
| Call rate | ❌ | `intexfactory::CALL_RATE` | `gemfactory::CALL_RATE` | ❌ |
| Call window | ❌ | `intexfactory::CALL_WINDOW` | **`gem::CALL_WINDOW`** | ❌ |
| Call threshold | ❌ | `intexfactory::CALL_THRESHOLD` | **`gem::CALL_THRESHOLD`** | ❌ |
| Notice period | ❌ | `intexfactory::CALL_NOTICE_PERIOD` | **`gem::CALL_NOTICE_PERIOD`** | ❌ |
| Rate denominator | ❌ | `intexfactory::PRICE_RATE_DEN` | *(inline `100`)* | ❌ |
| Per-block budget | `MAX_BUCKET_QUALIFICATIONS_PER_BLOCK` | `MAX_SERIES_PER_BLOCK` | `MAX_GEM_QUALIFICATIONS_PER_BLOCK` | `MAX_CREDIS_EXPIRY_SCANS_PER_BLOCK` |

Three naming problems:

* **`FLOOR_RATE_PERCENT` vs `FLOOR_RATE`** — same value, two names, and Lysis's is the one
  with the unchecked multiply (D-08).
* **Constants live in different crates.** Gem splits them: window / threshold / notice in
  the **ledger** crate (`outbe_gem::constants`), rates in the **factory**
  (`outbe_gemfactory::constants`). Intex puts all six in the **factory**. WP-0 resolves
  this by hoisting every shared value into `outbe_common::pricing` and leaving thin
  re-exports.
* **Four spellings of the per-block budget.** Converge on
  `MAX_<INSTRUMENT>_<SCAN>_PER_BLOCK`, e.g. `MAX_GEM_QUALIFICATIONS_PER_BLOCK` /
  `MAX_GEM_CALLS_PER_BLOCK`. Gem currently has no call budget at all (D-13).

Gem also uses `u64` for `FLOOR_RATE` / `CALL_RATE` while Intex uses `u16`; the shared
constants are `u16`.

### 9.5 Lifecycle function names

| Operation | Nod | Intex | Gem | Credis | Canonical |
|---|---|---|---|---|---|
| Create | `api::add_nod` | `api::create_series` | `api::add_gem` | `create_position` | **`api::issue_<x>`** |
| Qualify one | `qualify_bucket` | `try_qualify` | `qualify` | ❌ | **`try_qualify`** |
| Mark qualified | *(implicit)* | `api::mark_qualified` | `set_state(Qualified)` | ❌ | **`mark_qualified`** |
| Evaluate call | ❌ | `try_call` | `trigger_call` | ❌ | **`try_call`** |
| Mark called | ❌ | `api::mark_called` | `mark_called` | ❌ | ✅ `mark_called` |
| Forfeit | ❌ | ❌ (target-chain, D-14) | `forfeit` | ❌ | **`forfeit`** |
| Settle | `api::settle_nod` | *(factory `settle`)* | `set_state(Settled)` | `pay_anadosis` | **`api::settle_<x>`** |
| Burn | `api::remove_nod` | ❌ | `api::burn` | `expire_position` | **`api::burn_<x>`** |
| Read one | `api::get_item` | `api::read_series` / `get_series` | `api::get_gem` | `get_position` | **`api::get_<x>`** |

Intex exposing **both** `read_series` (reverts if absent) and `get_series` (returns
`Option`) is the one distinction worth keeping — propagate it as `read_<x>` / `get_<x>`
rather than flattening it.

Gem drives Qualified and Settled through the generic `set_state` while Intex has named
`mark_qualified` / `mark_called`. Named transitions are safer — `set_state` accepts any
enum value and `ADR-C-GEM-001:41-43` already flags that it "must enforce the transition
graph rather than accept arbitrary enum movement".

### 9.6 Scan and hook function names

| Role | Nod | Intex | Gem | Credis | Canonical |
|---|---|---|---|---|---|
| Qualify sweep | `qualify_nods` | `scan_and_qualify` | `scan_and_qualify` | ❌ | **`scan_and_qualify`** |
| Per-currency qualify | `qualify_buckets_with_rate` | `qualify_currency` | `qualify_with_rate` | ❌ | **`qualify_currency`** |
| Call sweep | ❌ | `scan_and_call` | `scan_and_call` | ❌ | **`scan_and_call`** |
| Per-currency call | ❌ | `call_currency` | *(none — see 9.7)* | ❌ | **`call_currency`** |
| Daily trigger entry | ❌ | `called::run_daily` | `hooks::run_call_daily` | ❌ | **`run_call_daily`** |
| Expiry sweep | ❌ | ❌ | ❌ | `scan_and_expire` | superseded by rewrite |

`qualify_nods` is the odd one out; `run_daily` is too generic once a crate has more than
one daily trigger.

### 9.7 Storage columns — and the logic divergence behind them

| Role | Nod | Intex | Gem |
|---|---|---|---|
| Unqualified bin trie | `bin_tree_root/mid/leaf` | `bin_tree_root/mid/leaf` | `bin_tree_root/mid/leaf` |
| Unqualified bin members | `unqualified_bin_buckets` | `unqualified_bin_series` | `unqualified_bin_gems` |
| Unqualified bin count | `unqualified_bin_count` | `unqualified_bin_count` | `unqualified_bin_count` |
| Qualify cursor | `unqualified_bin_scan_cursor` *(within-bin)* | `qualify_scan_cursor` *(bin-level)* | `qualify_scan_cursor` *(bin-level)* |
| **Callable set** | ❌ | **`qualified_bin_tree_root/mid/leaf` + `qualified_bin_count` + `qualified_bin_series`** — a second LB trie keyed by **call price** | **`callable_gems` (dense `List`) + `callable_gem_index`** |
| Call cursor | ❌ | `call_scan_cursor` + `call_currency_cursor` | ❌ **none** |

**L-01 — the callable set is represented two structurally different ways, and this is
the root cause of D-13.** Intex indexes Qualified series in a **price-keyed bin trie**, so
its daily scan walks only bins at or below the day's VWAP bin
(`crates/core/intexfactory/src/called.rs:119-129`, `Some(b) if b <= v_bin`) — a series
whose call price is above today's VWAP is never even read. Gem keeps a **dense list** and
visits every callable gem every day (`crates/core/gem/src/hooks.rs:150-157`), regardless
of price.

So the two implementations differ in **complexity class**, not just in naming: Intex does
work proportional to the *breached* population, Gem to the *entire callable* population.
Port Intex's structure to Gem, and build Nod's and Credis's new callable sets the same
way. Do not copy `callable_gems`.

**L-02 — cursor granularity differs.** Nod's `unqualified_bin_scan_cursor` resumes
*within* a bin; Gem's and Intex's `qualify_scan_cursor` resume *at bin boundaries* and
process whole bins atomically. Nod's is finer-grained and cannot starve a large tail bin;
Gem and Intex overshoot their budget when a bin is larger than the remaining allowance
(documented at `crates/core/gem/src/hooks.rs:58-60`). Pick one and say why in the ADR.

### 9.8 Event names

| Transition | Nod | Intex | Gem | Credis | Canonical |
|---|---|---|---|---|---|
| Issued | `NodIssued` (factory) | `SeriesIssued` (factory) | `GemIssued` (factory) | `PositionCreated` / `CredisRequested` | **`<X>Issued`** |
| Qualified | ⚠️ `NodBucketQualified` (bucket-level) | `SeriesQualified` | `GemQualified` | ❌ | **`<X>Qualified`** |
| Called | ❌ | `SeriesCalled` | `GemCalled` | ❌ | **`<X>Called`** |
| Settled | `NodSettled` | ⚠️ **`Settled`** — unprefixed | `GemSettled` | `AnadosisPaid` | **`<X>Settled`** |
| Forfeited | ❌ | ❌ | ⚠️ `GemBurned` — reused for forfeit *and* mining burn | `CollateralBurned` | **`<X>Forfeited`**, distinct from `<X>Burned` |
| Burned | `NodBurned` | — | `GemBurned` | — | `<X>Burned` |

Three defects:

* **`IIntexFactory.Settled`** is the only unprefixed lifecycle event, sitting beside
  `SeriesIssued` / `SeriesQualified` / `SeriesCalled`.
* **`GemBurned` is declared on two interfaces** — `IGem.sol:47` and `IGemFactory.sol:50` —
  and carries two meanings: forfeit-burn after notice lapse
  (`crates/core/gem/src/runtime.rs:100-104`) and post-mining burn. A consumer cannot tell
  a forfeit from a settled burn. Split into `GemForfeited` and `GemBurned`.
* **Nod emits qualification at bucket granularity only**, so no per-holder event exists
  to observe. WP-3c's per-item Called must come with a per-item `NodQualified`.

### 9.9 Error variant names

| Condition | Nod | Intex | Gem | Credis | Canonical |
|---|---|---|---|---|---|
| Not found | `NodNotFound` | `SeriesNotFound` | `GemNotFound` | `PositionNotFound` | ✅ `<X>NotFound` |
| Already exists | `NodFactoryError::NodAlreadyExists` | ❌ | ⚠️ `AlreadyExists` | `PositionAlreadyExists` | **`<X>AlreadyExists`** |
| Wrong state | ❌ | ✅ `InvalidState { expected, actual }` | ⚠️ `InvalidState` *(unit)* | ⚠️ `PositionCompleted` | **`InvalidState { expected, actual }`** |
| Bad state byte | ❌ | `InvalidStateValue(u8)` | ❌ | ❌ | **`InvalidStateValue(u8)`** |
| Floor not met | ❌ | ❌ | `FloorPriceNotMet` | ❌ | `FloorPriceNotMet` |
| Index OOB | `IndexOutOfBounds` | ❌ | `IndexOutOfBounds` | ❌ | ✅ |
| Oracle down | ❌ | ❌ | `OracleUnavailable` | ❌ | `OracleUnavailable` |
| Overflow | ❌ | `CostAmountOverflow` | ❌ *(factory `Overflow`)* | ❌ | `<Field>Overflow` |

Intex's `InvalidState { expected, actual }` is strictly the best of these — it tells the
caller *what* was expected. Gem's unit `InvalidState` should adopt it. Credis encodes a
state error as `PositionCompleted`, which is a state name rather than a violation.

---

### 9.10 Canonical naming rules

State these in `ADR-C-PRC-001` (WP-5.5) so future instruments inherit them:

1. **A suffix names the denomination, and must be true of the value.** `_minor` / `Minor`
   only for values in the currency's minor unit (§1.5); `_scaled` / `Scaled` for
   fixed-point values at `INTERNAL_PRICE_SCALE`. Applying the suffix consistently but
   wrongly is what produced S-02 — every price today is `_minor` and none of them are.
   Fix the denomination question first (WP-0b.6), then apply the suffix rule uniformly
   (fixes N-01).
2. **Prices** are `entry_price_minor`, `floor_price_minor`, `call_price_minor`. No
   instrument-specific synonym for a shared concept (fixes N-02).
3. **Call terms** are `call_rate`, `call_window`, `call_threshold`,
   `call_notice_period` — all snapshotted onto the record at issuance.
4. **Lifecycle timestamps** are `issued_at`, `qualified_at`, `called_at`, `settled_at`;
   `u64` seconds.
5. **State** is a single `state: u8` decoded through `<X>State`, never a pair of booleans.
6. **Transitions** are named — `mark_qualified`, `mark_called`, `forfeit` — not a generic
   `set_state`.
7. **Scans** are `scan_and_<verb>` at the top, `<verb>_currency` per currency, and the
   Cycle entry point is `run_<verb>_daily`.
8. **Events** are `<Instrument><Transition>` with the instrument prefix always present.
9. **Errors** are `<Instrument><Condition>`; wrong-state carries `{ expected, actual }`.
10. **Shared constants** live in `outbe_common::pricing`; crate-local names are
    re-exports, never independent definitions.

### 9.11 Mechanical rename map

Pure renames, no behaviour change. Each is independently landable.

| # | From | To | Scope |
|---|---|---|---|
| R-01 | `IGem.GemData.entryPrice` / `.floorPrice` / `.costAmount` / `.gemLoad` | `…Minor` | **breaking ABI**; bundle with WP-5.1 |
| R-02 | `INod.NodData.costOfGratisMinor` | `entryPriceMinor` | **breaking ABI**; bundle with WP-3a |
| R-03 | `IIntexFactory.Settled` | `SeriesSettled` | breaking ABI |
| R-04 | `GemBurned` (forfeit path) | `GemForfeited` | breaking ABI; disambiguates two meanings |
| R-05 | `Position.created_at` | `issued_at` | Credis rewrite |
| R-06 | `Position.smart_account` | `owner` | Credis rewrite |
| R-07 | `credis_principal` | `credis_load_minor` | Credis rewrite |
| R-08 | `lysis::FLOOR_RATE_PERCENT` | `common::pricing::FLOOR_RATE` | WP-0 |
| R-09 | `nod::hooks::qualify_nods` | `scan_and_qualify` | WP-3 |
| R-10 | `nod::qualify_buckets_with_rate` | `qualify_currency` | WP-3 |
| R-11 | `gem::qualify_with_rate` | `qualify_currency` | WP-1 |
| R-12 | `gem::trigger_call` | `try_call` | WP-1 |
| R-13 | `intexfactory::called::run_daily` | `run_call_daily` | WP-2 |
| R-14 | `GemError::AlreadyExists` | `GemAlreadyExists` | WP-1 |
| R-15 | `GemError::InvalidState` (unit) | `InvalidState { expected, actual }` | WP-1 |
| R-16 | `gemfactory` `FLOOR_RATE`/`CALL_RATE` `u64` | `u16` | WP-0 |
| R-17 | `api::add_gem` / `add_nod` / `create_series` | `api::issue_<x>` | all WPs |

R-01 through R-04 change published ABIs. Land each with `mise run export-abi` in the same
commit, and update `mcp/src/tools/*.ts` decoders together.

---


---


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
