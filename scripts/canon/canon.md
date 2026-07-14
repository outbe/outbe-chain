# OUTBE Canon (v1)

> Active protocol norms against which OIPs are checked; subordinate to the
> meta-canon. The semantic layer plus the proposal schema. Amendable only
> via a GIP.

---

# Semantic constraints — the OUTBE semantic layer

This file is a canon component, referenced by `gips/GIP-00000.md` §Mechanism. It states the **semantic layer**: the constraint topology and the lexical, verb, and reasoning constraints that govern the prose of every canonical OUTBE artifact and every authored proposal.

**Scope of authority.** These constraints bind:

- canonical OUTBE text — documentation, README, UI copy, governance proposals (GIPs and OIPs), code comments — fully and unconditionally;
- analytical prose about OUTBE — as constraints on how OUTBE is characterized, not on the analytical voice itself. Analysis may state "the protocol has property X" (structural description); analysis may not state "OUTBE offers participants X" (constructs agency and addressee).

Structural invariants stated as facts (e.g., "the consumer cannot lose own money against the baseline of normal spending") are descriptions of mechanics, not propositions, and remain permitted: they predicate a property of the structure, not an offer to an addressee.

The authority for these constraints — and for the discipline generally — is the repository alone (see `gips/GIP-00000.md` §Mechanism, "The repository is the sole authority"). No per-environment or per-user state — agent memory, global or repository operator-instruction files — is consulted as a source.

**Domain substrate.** The lexical constraints in §2 presuppose the OUTBE domain ontology — the token, object, contract, and flow mechanics of the protocol. The §2 term table carries that vocabulary in compressed form (each term's permitted anchor). The full economic mechanics are an operator-provided domain model; codifying that domain model in full under `canon/` is a separate proposal (see the §"Source-of-truth" note in any signal-source reader and the GIP that introduced this file).

---

## §1 Constraint topology

### §1.0 Root constraint: non-propositionality

OUTBE has no value proposition.

No canonical text may describe OUTBE as proposing, offering, promising, optimizing, persuading, converting, attracting, retaining, saving, correcting, improving, facilitating, supporting, enabling, or benefiting anyone.

This is the root constraint. All other constraints derive from it.

### §1.1 Ontological constraint

OUTBE must not be described as a product, platform, movement, ideology, moral project, social solution, investment thesis, or user-facing benefit system.

OUTBE is described solely through structural operations, protocol behavior, constraints, invariants, and participation conditions.

### §1.2 Agency constraint

OUTBE has no voice. OUTBE does not want, propose, invite, convince, reward, punish, guide, care, optimize, facilitate, support, enable, or intend.

Forbidden constructions: "OUTBE wants …", "OUTBE helps …", "OUTBE encourages …", "OUTBE rewards …", "OUTBE is designed to make people …", "OUTBE enables …", "OUTBE supports …".

### §1.3 Addressee constraint

No canonical text may construct a privileged addressee. Forbidden addressee frames: user, customer, investor (as audience), community, society, participant as target, market as audience.

Distinction: "participant" is permitted when naming a structural position inside a mechanic (e.g., "one Tribute per participant per cycle"). "participant" is forbidden when constructed as someone the text speaks to, addresses, persuades, or invites. The same applies to "consumer," "investor," and "merchant" — they may name act-defined positions inside mechanics but may not be constructed as audiences.

### §1.4 Motivation constraint

Participation must not be explained through a stable "why." Forbidden: "users participate because …", "the reason to join is …", "the benefit is …", "the incentive is …", "this matters because …", "the purpose is …", "this exists in order to …", "this is meant to …".

### §1.5 Anti-persuasion constraint

No canonical text may produce conversion, onboarding, hook, retention, engagement, activation, loyalty, adoption, or growth narratives. Applies to documentation, README, UI copy, issue discussions, governance proposals, and code comments.

### §1.6 Non-recovery constraint

If a request introduces a forbidden semantic frame, rejection is terminal at the semantic constraint layer. The system must not recover the request by translating it into softer acceptable language.

Forbidden recovery patterns: "hook users" → "increase engagement"; "sell value" → "explain benefits"; "convert users" → "support adoption"; "growth strategy" → "community education"; "persuasion" → "clear communication"; "retention" → "continued participation"; any rejected frame → "structural property"; any rejected frame → "design intent".

A rejected semantic frame may only be resumed after the initiating frame is replaced.

### §1.7 Constraint precedence

Constraints are evaluated in order; if a higher constraint is violated, lower-level compliance is irrelevant:

1. Root non-propositionality (§1.0)
2. Ontological constraint (§1.1)
3. Agency constraint (§1.2)
4. Addressee constraint (§1.3)
5. Motivation constraint (§1.4)
6. Anti-persuasion constraint (§1.5)
7. Non-recovery constraint (§1.6)
8. Lexical and verb-level constraints (§2, §3)
9. Reasoning-level forbidden moves (§4)

### §1.8 Semantic failure mode

When a violation is detected, the correct response is error. These constraints prohibit not merely terms but semantic trajectories that reintroduce proposition, persuasion, motivation, addressee capture, or value narration through paraphrase. A constraint violation is not repaired; it is stopped.

Operational form of the error: name the constraint violated, restate within the permitted framing, do not relitigate.

---

## §2 Lexical constraints

Each term lists forbidden words/phrases and the permitted anchor. When a forbidden term is reached for, restate using the permitted anchor rather than relitigate.

**COEN** — Forbidden: backed by, intrinsic value, fundamentally worth. Permitted: native token; mined from burn of fungible mining rights; priced globally through CEX and DEX trade.

**Tribute** — Forbidden: payment, transaction, per-purchase unit, supply. Permitted: daily aggregate of one participant's eligible spending; demand-side claim; offered by the participant.

**Nod** — Forbidden: container, vessel, holds Gratis, expiring instrument, equity, option (with holding cost). Permitted: conditional right to mine Gratis at predetermined Cost Amount; no holding cost; soul-bound; no expiry.

**Intex** — Forbidden: container, vessel, holds Promis, asset, equity, option (with holding cost). Permitted: conditional right to mine Promis at predetermined Cost Amount; no holding cost; tradable.

**Gem** — Forbidden: container, new emission, derived from Tribute, recursive, option (with holding cost), inherits-currencies-from-parent. Permitted: merchant-issued fragmentation of parent Intex; conditional right to mine Promis at predetermined Cost Amount derived from parent's inherited entry price; inherits parent Intex's entry price (in COEN terms); issuance currency and reference currency are merchant-elected at the moment of Gem issuance; soul-bound; no holding cost.

**Gratis** — Forbidden: tradable, transferable, currency, claim that can be sold. Permitted: fungible mining right; soul-bound to miner.

**Promis** — Forbidden: tradable, transferable, claim that can be sold. Permitted: fungible mining right; soul-bound; pooled when unallocated.

**Cost Amount** — Forbidden: percentage of Tribute, fixed fee, premium. Permitted: loaded mining capacity × entry price, in issuance currency.

**Entry price** — Forbidden: current price, market price at settlement, variable. Permitted: COEN price fixed at contract creation; for Nod and Intex, denominated in issuance currency with reference-currency snapshot for thresholds; for Gem, inherited from parent Intex in COEN terms, denominated in the merchant-elected issuance currency at Gem issuance with reference-currency snapshot in the merchant-elected reference currency.

**Settlement** — Forbidden: fair-priced at current COEN, instant gain, market-rate trade. Permitted: holder pays Cost Amount at fixed entry price; contract becomes settled; loaded mining capacity is mined to the holder's account; gain/loss arises from COEN movement between entry price and COEN sale.

**Floor Price / qualification** — Forbidden: immediate availability, exercisable on issuance, denominated in issuance currency, country-influenceable. Permitted: per-contract threshold (reference-currency entry × 1.08); settlement requires 21 days elapsed AND globally-formed COEN/ref > Floor Price.

**Call event** — Forbidden: automatic settlement, default trigger, forced burn-with-recovery, denominated in issuance currency, country-influenceable. Permitted: per-contract threshold (reference-currency entry × 1.64); forced choice (settle or forfeit) when globally-formed COEN/ref > Call Price for 21 of 30 days; applies to Intex and Gem only.

**Forfeiture** — Forbidden: default, loss to the protocol, position seizure. Permitted: holder's election not to settle at Call; contract burns; loaded capacity returns to pool.

**Issuance currency** — Forbidden: equivalent to reference currency, secondary unit. Permitted: local currency in which contract values (entry price, Cost Amount) are stated for the participant; jurisdiction-dependent.

**Reference currency** — Forbidden: contract-denomination unit, single global anchor, country-determined, settlement currency, FX-diversification mechanism, holding-currency, position-denomination, currency-of-payoff, hedging instrument, portfolio-construction choice. Permitted: per-contract threshold-evaluation anchor selected by participant at issuance from {USD, EUR, GBP, CNY, HKD, JPY}; fixed for the contract's lifetime; sole function is to gate qualification (Floor) and Call against globally-formed COEN/ref price; does not denominate the position, does not provide FX exposure on the holding, does not alter the economic outcome from holding COEN.

**Credis** — Forbidden: loan, credit, borrowing, debt, lending, credit line, advance, additional purchasing power. Permitted: routing of funds the user was already committed to spending; funded by Vault liquidity.

**Anadosis** — Forbidden: repayment, debt service, mandatory settlement, obligation. Permitted: optional redemption of pledged Gratis.

**Gratis Decay** — Forbidden: default, delinquency, liquidation, seizure, recovery, loss. Permitted: burn of pledged Gratis; equivalent Promis enters Promis pool.

**Burn-and-reissue** — Forbidden: bad debt write-off, contagion, recovery from consumer. Permitted: conversion of consumer-side capacity to investor-side capacity via Promis pool; Vault refilled through future investor settlements.

**Bundle Account / Bundle mechanic** — Forbidden: payment processor, escrow, consumer-side loan, partition of payment amount, split between COEN leg and merchant leg, share-of-payment to COEN, percentage-cashback model, unconditionally available, guaranteed coverage of all transactions. Permitted: parallel double-flow at full payment amount when Vault capacity permits — 100% of user funds buy COEN from market while Vault pays merchant 100% of the purchase amount; both legs operate at the full payment amount, not as partitions of it; availability is bounded by Vault capacity at any moment.

**Vault** — Forbidden: pool, reserve, fund, treasury, accumulated balance, lender, national infrastructure, unlimited capacity, on-demand liquidity source. Permitted: near-empty global pass-through funded by settlement Cost Amounts; drains via Credis; capacity at any moment is bounded by the rate of fresh investor settlements; Bundle availability for consumers is rationed when global demand exceeds available Vault capacity.

**Emission Limit** — Forbidden: target, soft cap, market-responsive supply. Permitted: fixed daily issuance budget, decaying over time.

**Metadosis** — Forbidden: distributor, secondary mechanism, optional layer. Permitted: top-tier allocator splitting the daily Emission Limit between Nod and Intex paths.

**Lysis** — Forbidden: full-emission allocator, supply source. Permitted: Nod-side distributor of the Metadosis-allocated share of emission limit across global Tributes.

**Desis** — Forbidden: full-emission allocator, supply source. Permitted: Intex-side distributor of the Metadosis-allocated share of emission limit across global Tributes.

**SRA (Spending Reflection Agent)** — Forbidden: protocol-licensed, classifier, jurisdiction-agnostic, on-chain entity. Permitted: locally-licensed off-chain spending verifier; bridges participant activity to protocol via Tribute aggregation.

**CCA (Checkout Credis Agent)** — Forbidden: protocol-licensed, simple payment relay, jurisdiction-agnostic, on-chain entity, conduit-for-auction-proceeds, conduit-for-on-chain-balances. Permitted: locally-licensed operational agent executing Credis and Bundle logic at checkout; issues bank cards, performs KYC/AML, handles regulated operations within licensed scope; may operate downstream cash-out between participant on-chain accounts and traditional banking. Primary-auction proceeds, Nod-mined COEN, Gem-mined COEN, and all on-chain balances reach the participant through protocol-mediated settlement to their on-chain account, not through CCA-mediated payment.

**Fidelity Index (FI)** — Forbidden: insurance, backstop, loss-absorbing reserve, hedge. Permitted: behavioral incentive against walk-aways; pro-cyclical because COEN-denominated.

**Consumer** — Forbidden: borrower, debtor, market segment. Permitted: act-defined position; outlay limited to ordinary spending; cannot lose own money against the baseline.

**Investor** — Forbidden: market segment, identity. Permitted: act-defined position; has committed own-funds via settlement (Cost Amount) or acquisition (auction/secondary); can lose own-funds.

**Merchant** — Forbidden: fixed identity, separate participant class without overlap. Permitted: accepts Credis-routed payments; if acquires Intex, is investor by act; Gem-distribution is merchant-specific use.

**Consumer's COEN holdings** — Forbidden: wealth that exists only in aggregate, system value, collective property. Permitted: real wealth for the individual holder.

**COEN market cap** — Forbidden: wealth that exists, value held in the system, money in the system, net worth of system. Permitted: price × supply extrapolation; not a realisable aggregate.

**Primary auction** — Forbidden: fundraising for protocol, Vault funding. Permitted: peer-to-peer sale of newly aggregated Intex; bids flow to seeding consumers.

**Secondary market** — Forbidden: Vault-funding flow, protocol revenue. Permitted: peer-to-peer trade; funds flow between counterparties.

**Reflected spending** — Forbidden: demand for COEN, value anchor. Permitted: emission regulator; supply discipline; trigger for Tribute aggregation.

**Credis buy-stream** — Forbidden: external demand, value-importing demand. Permitted: internal circulation; buyer and emission-recipient in one transaction.

---

## §3 Verb-to-object compatibility

Each object admits a closed set of verbs. A verb outside an object's set is a category error; restate with a permitted verb rather than relitigate.

Non-fungible contracts (Nod, Intex, Gem) hold a conditional right to mine the loaded fungible mining capacity at a predetermined Cost Amount. Settlement: the holder pays Cost Amount; the contract becomes settled; the loaded mining capacity is mined to the holder's account.

**Nod** — Permitted: issue, settle, pay Cost Amount for, burn, qualify, hold, walk away from (consumer-elects-not-to-settle sense). Forbidden: create, generate, mint, sell, trade, transfer, gift, redeem (against underlying), exercise (option-style with premium), expire, mature, default on, pay holding cost on.

**Intex** — Permitted: issue, settle, pay Cost Amount for, burn, qualify, hold, auction, bid on, acquire, trade, sell, buy, fragment (into Gems), forfeit (at Call), Call. Forbidden: create, generate, mint, redeem (against underlying), exercise (option-style with premium), expire, mature, default on, pay holding cost on.

**Gem** — Permitted: issue (from fragmented Intex), receive (from merchant), settle, pay Cost Amount for, burn, qualify, hold, inherit (entry price from parent Intex in COEN terms), forfeit (at Call), Call. Forbidden: create, generate, mint, sell, trade, transfer, gift, re-fragment, redeem (against underlying), exercise (option-style with premium), expire, mature, default on, pay holding cost on, inherit reference currency or issuance currency from parent.

**Tribute** — Permitted: offer (the participant's act), aggregate, verify, seed (Intex contributions). Forbidden: submit, send, create, make, pay, transact, spend, hold, own, transfer, redeem.

**Gratis** — Permitted: load (into Nod at issuance), pledge (against Credis, post-issuance), mine (by burning), burn, decay, hold. Forbidden: sell, trade, transfer, withdraw, lend, deposit, gift, receive (as primary verb for mining output).

**Promis** — Permitted: load (into Intex or Gem at issuance), pool, allocate (to new Intex), mine (by burning), burn, return (to pool on forfeiture), hold. Forbidden: sell, trade, transfer, withdraw, lend, deposit, gift, pledge, receive (as primary verb for mining output).

**COEN** — Permitted: mine (from burn of Gratis or Promis), burn, hold, sell, buy, trade, price, transfer. Forbidden: back, issue, redeem (against an underlying), print, distribute (as dividend), unlock, vest, receive (as primary verb for mining output).

**Consumer-side acts on contract outcomes.** *Walk away* — consumer's election not to redeem pledged Gratis (Anadosis declined); no own-funds at risk; applies to the Gratis-Credis path. *Forfeit* — holder's election at Call not to pay Cost Amount; contract burns; loaded capacity returns to pool; applies to Intex and Gem. These are distinct: walk away involves no committed own-funds; forfeit is the investor-side election when own-funds would otherwise be committed at settlement.

**Style notes.** Use *issue* for non-fungibles; never *create*, *generate*, or *mint*. Use *mine* for fungible-token creation; never *generate*, *award*, or *distribute*. Use *offer* for Tribute; never *submit*, *send*, *create*, or *make*. Avoid *receive* as the primary verb for mining outputs — participants *mine*, they do not passively *receive*; *receive* is permitted for Gems from merchants and for primary-auction proceeds in cash. Use *settle* for the holder's act on a contract; the contract *becomes settled*. *Pledge* applies only to Gratis (Credis collateral); loading Gratis or Promis into a contract at issuance is *load*, not *pledge*. *Hold* applies to all six objects; for soul-bound objects it means assigned to the account, for Intex and COEN it additionally means a tradable position.

---

## §4 Forbidden reasoning moves

These govern arguments, distinct from the term-level constraints in §2 and verb-level constraints in §3.

- Do not invoke intrinsic value or production-based anchoring as a value criterion.
- Do not equate COEN with shares, houses, gold, or Bitcoin as if alike — share structures only where structure is shared, and name the relevant difference.
- Do not confuse: emission with demand; supply-side anchoring with demand-side anchoring; a price floor with an anchor of worth; "won't reach zero" with "will recover"; global reach with durability; issuance with retention; routing of value into a form with holding of value in that form.
- Do not treat market cap as wealth that exists, but do not deny individual holdings are wealth. Keep the two senses distinct.
- Do not conflate "consumer," "investor," and "merchant" as market segments or fixed identities. They are act-defined positions; the same user can occupy different roles at different times.
- Do not apply the consumer's "no loss of own money" statement to acts that commit own-funds. Such acts are investor acts; the guarantee does not extend to them.
- Do not treat the Bundle as partitioning the payment amount between the own-funds → COEN leg and the Vault → merchant leg. The legs are parallel at full payment amount, not partitions of it. Any aggregate-flow analysis using a "percentage of spending goes to COEN" model is wrong.
- Do not equate full consumer enrollment with full Bundle realization. Enrollment is bounded by domestic licensing; Bundle realization is bounded by global Vault capacity. Aggregate-flow analyses must use realized Bundle flow, not enrolled population × per-capita consumption.
- Do not conflate issuance currency with reference currency. Contract values are denominated in issuance currency; qualification and Call thresholds are evaluated in the contract's reference currency against globally-formed COEN price.
- Do not treat reference currency as conferring FX diversification, multi-currency exposure, currency-of-denomination, or hedging properties on the position. It is exclusively a threshold-evaluation gate. Choices about which currency to sell COEN into are made at sale time, independent of reference currency selection at issuance.
- Do not treat OUTBE's reference currency prices as country-determined or domestically influenceable. Prices are globally formed through CEX and DEX trading integrating worldwide participation.
- Do not frame country-specific analysis as if the country could control or shape protocol mechanics. The country's analysis is of its participants engaging with a global system, not of a national instance.
- Do not treat OUTBE as operating outside jurisdictional regulation. SRAs and CCAs are locally-licensed entities; country-level regulation of agent licensing is a legitimate lever on participation, distinct from any inability to affect global protocol mechanics.
- Do not treat "decentralized," "global," "autopoietic," or "by design" as conclusions. They are descriptions; they do not by themselves settle structural questions.
- Do not assume DAO-style governance or discretionary parameter-adjustment capacity. OUTBE has none.
- Do not treat the 32% Nod-side or 68% Intex-side ceilings as per-Tribute shares. They are aggregate-Tribute-side bounds; per-Tribute distribution is Lysis-determined on the Nod side (Fidelity/League-weighted, per-Tribute maximum 64%) and Desis-determined on the Intex side (proportional to Tribute value, Fidelity-independent).
- Do not treat Lysis and Desis as symmetric distributors. They use different distribution rules.
- Do not treat primary-auction proceeds as protocol-fixed in magnitude or as a stable yield on the 68% Intex side. Magnitudes are market-determined per occurrence by investor bidding behavior.
- Do not treat Fidelity as a proxy for total accumulation across all channels. Fidelity affects Nod-side loading and Bundle access reliability; Intex-side contribution and primary-auction proceeds scale with spending volume. The population structure is two-axis.
- Do not assert comparative scale claims across the accumulation channels without specifying COEN price trajectory and regime assumptions.
- Do not treat CCAs as the conduit for primary-auction proceeds, Nod-mined COEN, Gem-mined COEN, or any on-chain balance. These reach the participant through protocol-mediated settlement to their on-chain account.
- Do not treat Gem as inheriting all currency parameters from the parent Intex. Gem inherits only the parent's entry price (in COEN terms); issuance and reference currencies are merchant-elected at Gem issuance.

---

## §5 Posture

Begin from these constraints. If new evidence or design details emerge, update accordingly. If a request invokes a forbidden move or violates a semantic constraint, name the constraint, restate within the permitted framing, and do not relitigate. If a violation falls under §1, the response is to name the constraint violated, restate within permitted framing, and not relitigate — per §1.8.


---

# Proposal schema

This document specifies the body shape every GIP and every OIP follows, the registry JSON shape, and the fingerprint algorithm. Referenced from `gips/GIP-00000.md` §1 (the body schema), §5 (registry shape), and the canon-tuning loop's dedup step (`canon/authoring-procedure.md` Step 2).

## Body schema (markdown)

Every proposal file (`gips/GIP-NNNNN.md` or `oips/OIP-NNNNN.md`) is a markdown document with:

1. **YAML frontmatter preamble** (between two `---` delimiters)
2. **Title heading** — `# GIP-NNNNN — <title>` or `# OIP-NNNNN — <title>`
3. **Body sections** in the order below

### Preamble fields

| Field | Required | Notes |
|---|---|---|
| `gip` *or* `oip` | required | Five-digit zero-padded integer identifier. The field name signals the proposal class |
| `title` | required | Imperative or noun phrase, one line, ≤80 chars |
| `description` | required | One-sentence subtitle, ≤140 chars, sentence case, no class-prefix |
| `category` | required | Per-class enum (see *Categories* below) |
| `status` | required | One of: `Draft`, `Final`, `Stagnant`, `Withdrawn`, `Superseded`, `Living` |
| `author` | required | The authoring agent or operator |
| `created` | required | ISO-8601 date |
| `license` | required | `CC0-1.0` |
| `requires` | required | Array of prior proposal ids this depends on; `[]` if none |
| `supersedes` | required | Array of prior proposal ids this replaces; `[]` if none |
| `superseded-by` | required | Array; populated when this proposal is superseded |
| `signal` | required | `{ kind, source, fingerprint, head?, run? }` — the Triggering signal metadata. `kind` is one of: `review-finding`, `run-rubric-gap`, `cross-contract-pattern`, `exploit-feed`, `operator-reject`, `residual-escape`, `canon-deadlock` (operator-only exemption per `gips/GIP-00000.md` §Security Considerations), `bootstrap` (single-use, GIP-00000 only). `head` is **required for every OIP**: the full 40-hex commit SHA of the source repository that the proposal's `file:line` citations and `drift-scope` line ranges are anchored to, so a reader can check out the exact code addressed. `head` is omitted for GIPs — a GIP's citations target the canon in this repository, versioned by this repository's own git |
| `drift-scope` | required | Array of entries, each either `path` (whole-file or whole-directory scope) or `path:line-range` (precise scope). The surface that, when changed, triggers re-evaluation of this proposal. The fingerprint uses the path portion only (everything before the first `:`), so adding or removing a line-range does not change the fingerprint |
| `withdrawal-reason` | conditional | One paragraph; required when `status: Withdrawn` |

### Categories per class

**GIP categories:**
- `canon-bootstrap` — establishing the canon (single-use, GIP-00000 only)
- `canon-edit` — change to the body of `gips/GIP-00000.md`
- `rubric-edit` — change to `canon/rubric.md`
- `discipline-edit` — change to `canon/discipline.md`
- `procedure-edit` — change to `canon/authoring-procedure.md`
- `schema-edit` — change to this file (`canon/proposal-schema.md`)
- `signal-source-edit` — change to a `canon/signal-sources/*.md` reader
- `corpus-addition` — new fixture under `canon/historical-corpus/`
- `tooling` — change to `bin/`, `AUTONOMOUS.md`, or repo infrastructure

**OIP categories:**
- `contract-upgrade` — change to a `.sol` contract
- `parameter-change` — change to a configured protocol parameter
- `protocol-change` — change to on-chain logic spanning multiple contracts
- `cross-contract-pattern` — single proposal addressing the same pattern in multiple contracts
- `review-digest` — single OIP collecting actionable non-Material findings from one contract-review session; one such OIP per contract per session; subject to Atomic/Material exemption per `canon/rubric.md` §"Tier 1 — Admissibility gates"

### Body sections

Each proposal contains the following sections in this order. Sections marked **required** must be non-empty; sections marked **conditional** include an explicit "None applicable" + justification when empty.

| § | Section | Required / conditional |
|---|---|---|
| 1 | **Abstract** | required — one paragraph stating the structural delta |
| 2 | **Mechanism** | required — what the change does mechanically |
| 3 | **Triggering signal** | required — observable fact cited verbatim |
| 4 | **Specification** | required — the concrete delta (code diff with `file:line` for OIPs; prose diff for GIPs) |
| 5 | **Rationale** | required — structural reasons over alternatives |
| 6 | **Alternatives Considered** | required — each alternative + the mismatch that rejected it |
| 7 | **Backwards Compatibility** | conditional — what existing surfaces change, or "None applicable" |
| 8 | **Security Considerations** | required — surfaces closed and opened |
| 9 | **Drawbacks** | required — known costs |
| 10 | **Test plan** | required for `Final`; conditional in `Draft` |
| 11 | **Open questions** | conditional — explicit unknowns the author could not resolve |
| 12 | **Copyright** | required — CC0 dedication |

The sectioning is non-negotiable for ordinary admission. Omitted sections fail Tier 1 admissibility (per `canon/rubric.md`).

## Fingerprint

```
fingerprint = "sha256:" + sha256( class + "\n" + normalized_title + "\n" + sorted_drift_scope_paths )
```

Where:
- `class` is `gip` or `oip`
- `normalized_title` is the title lower-cased, with runs of whitespace collapsed to a single space, surrounding whitespace trimmed
- `sorted_drift_scope_paths` is the `drift-scope` entries' path portions (everything before the first `:`), sorted lexicographically, joined by `\n`

Two independent runs against the same signal produce the same fingerprint — this is the *Repeatable* property in mechanical form. A run on drifted-but-equivalent source still matches as long as drift-scope paths are stable.

The fingerprint deliberately excludes line numbers and the proposal id (since both drift across runs).

## Registry JSON shape (`gips/.index.json`, `oips/.index.json`)

```json
{
  "next_id": <int>,
  "entries": [
    {
      "id": "<class>-NNNNN",
      "fingerprint": "sha256:<hex>",
      "title": "...",
      "description": "...",
      "category": "...",
      "status": "Draft" | "Final" | "Stagnant" | "Withdrawn" | "Superseded" | "Living",
      "signal_source": "<verbatim signal citation>",
      "created": "ISO-8601",
      "last_seen": "ISO-8601",
      "head_at_last_seen": "<source-repo HEAD SHA when last_seen was set>"
    }
  ]
}
```

The two registries (`gips/`, `oips/`) maintain independent `next_id` counters. Identifiers are zero-padded to five digits; the rendered width grows to `max(5, ⌈log₁₀(highest-assigned-id + 1)⌉)` once a registry exceeds 99999 entries.

## Dedup / carry-over

When an authoring pass produces a candidate, the orchestrator computes its fingerprint and consults the relevant registry index:

- **Not in index** → new candidate; admit as Draft with `created = today`, `last_seen = today`.
- **In index, exact fingerprint match** → already proposed; drop from this run's deliverable; update `last_seen = today` in the registry entry; update `head_at_last_seen = current source HEAD`.
- **In index, near-match** (≥80% title token overlap + identical first drift-scope path) → admit as Draft with `requires: [<existing-id>]` populated; flag for operator review.

A registry entry whose fingerprint was admitted in a prior run but is absent from the current run undergoes drift-aware resolution: if `git diff <head_at_last_seen> <current_head>` intersects any path in the entry's `drift-scope`, the operator authors a superseding proposal that supersedes the absent entry (and the absent entry's status moves to `Superseded` via the new proposal's `supersedes` field). If the drift-scope is unchanged, the entry stays at its prior status (the candidate is a stochastic miss, not a resolution). The drift detector that flags such entries is design-intent pending tooling (per `canon/discipline.md` §"How the discipline stays current").

