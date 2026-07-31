# OUTBE Meta-Canon (v1)

> Constitutional layer: the norms that regulate the canon itself — how a
> signal becomes a proposal, how a proposal is scored, and how the canon is
> amended. Assembled from the governance discipline corpus. Amendable only
> via a GIP.

---

# Undisputed world champion: the discipline of OUTBE governance

The goal is to be the **undisputed world champion** of OUTBE governance — both governance of the system itself (GIPs) and governance of blockchain logic (OIPs). Everything below derives from that.

"Undisputed" is the operative word. A chess world champion isn't "claimed to be best" — the title is *exhibited*: every move on the public record, reproducible by anyone who studies it. Undisputed means no one contests the methodology, because anyone applying it independently arrives at the same proposals.

This matters for blockchain governance specifically because deployed contracts are irreversible — a contract that has already lost funds cannot lose them less, and a governance system whose proposals can be misread or re-litigated has not closed the loop. So governance can't just *be* rigorous; it has to be **shown** to be, by anyone, against the public record. That is the discipline this document states.

## The three artefacts

- **The canon** — the codified governance discipline: how a signal becomes a proposal, how a proposal is scored, how the canon itself is amended. Lives at `gips/GIP-00000.md` plus the files it references under `canon/`. Vocabulary-neutral by design; exhibited against Solidity and Rust signal sources through `canon/signal-sources/contract-review.md` (a source-code review reader carrying a shared chassis and a per-source-type probe registry) and `canon/signal-sources/contract-walkthrough.md` (Solidity lifecycle walkthroughs) — the apparatus extends to further source types by adding a probe registry to the reader.
- **The proposal** — the canon's output for one signal: a GIP or an OIP. Plain language throughout — a proposal that needs the reader to already speak governance shorthand has failed as an exhibit.
- **The registry entry** — every proposal as a self-contained admitted unit (preamble, body, fingerprint, status), readable cold by anyone with no prior context. The entry is the unit of work.

## Admissibility precedes quality

Before any of the properties below apply, a proposal has to earn its place on the record. A champion's annotated game does not include moves that weren't played, blunders that weren't blunders, or "interesting alternatives" with no consequence. A proposal is admitted only if its triggering signal is **true** (the cited fact is really there), **reachable** (a concrete path triggers it), **material** (the consequence is real — funds, access, liveness, correctness, governance integrity), and **atomic** (one change, one fix). A proposal that fails any of these is not a weak exhibit; it is not an exhibit. The six properties describe how an *admitted* proposal is made championship-grade — they never rescue an inadmissible one.

## Six properties that distinguish championship-level work

1. **Self-contained at every level.** An entry stands alone. The registry stands alone. The canon stands alone. Each layer survives being lifted out of context — because a champion's published game is meant to be studied independently, without the champion in the room.
2. **Plain-language load-bearing.** Clarity is part of the demonstration. A move only counts as understood if it can be re-derived from the record. Same here: a proposal only counts as a real exhibit if it parses without insider vocabulary.
3. **Evidenced before persuasive.** Every claim is checked against the source (code, residual, prior proposal) before it's written, and the proposal cites where — `file:line` for code, a registry id for prior proposals, a verbatim quote for external signals. A champion's published analysis cites the positions; the reader confirms the problem is real without taking the author's word for it.
4. **Mechanism is sound.** Naming the issue is half the annotation; the other half is the change that closes it — and it has to actually close it. For OIPs: the code change is the minimal one that closes the issue, names a concrete test or invariant that confirms it, and states what it could regress. For GIPs: the canon patch is the minimal change that closes the canon gap, names what subsequent proposals will look like under the patched canon, and states what existing rules it interacts with. A change that doesn't hold is a worse annotation than none.
5. **Repeatable under composition.** Two independent agents running the canon against the same signal converge on the same proposal (same fingerprint). This is what makes the title *undisputed* — the standard isn't held by one author; it's reproducible by any qualified party. Without this property, the work is one opinion among many.
6. **Self-closing feedback.** A champion who stops training stops being champion. Every canon-tuning session that edits the canon to lift a sub-master proposal records those edits as a single canon-delta GIP bundled with the proposal for operator approval — so the discipline doesn't decay between sessions and every change to it lands on the record as it happens.

## How the discipline stays current

Three feedback loops are defined; one is live, two are design-intent pending tooling:

- **Reproduction gap** (design-intent; tooling pending). When two independent agents disagree on a signal's proposals, the disagreement points at an ambiguous canon rule. The disagreement is the Triggering signal for a GIP that tightens the rule. The metric isn't overlap; it's the rate of overlap improvement per cycle. No cross-agent runner exists today; a future `tooling` GIP delivers it.
- **Residual escape** (live). When an external researcher finds an issue the canon missed, the escape becomes a Triggering signal for an OIP (if the issue is in blockchain logic) and a GIP (if the canon should have caught it). The escaped finding becomes a regression fixture under `canon/historical-corpus/` forever.
- **Source drift** (design-intent; tooling pending). When a signal source changes, the canon flags any proposal whose drift-scope intersects the change and re-evaluates it under the current source state. No drift detector exists today; a future `tooling` GIP delivers it.

## The operational record

This document states the discipline; it is exhibited in concrete artefacts under `canon/` and the two registries:

- **`historical-corpus/contract-findings/`** — pre-exploit Solidity source snapshots paired with `expected-finding.md`. The Solidity signal-source reader must catch every entry; this is the framework's standing regression suite for OIP authoring from contract sources.
- **`historical-corpus/proposals/`** — master-grade proposal fixtures (great EIPs as the seed corpus, OUTBE proposals as they accumulate). Any canon change must not cause a previously-passing fixture to fail re-authoring.
- **`gips/`** — admitted governance proposals — the canon's self-improvement record. External escapes (auditor findings, exploit post-mortems, missed patterns) land here as GIP candidates with `signal.kind: residual-escape`, not in a separate buffer.
- **`oips/`** — admitted blockchain proposals — the canon's external work product.

The canon (`gips/GIP-00000.md`) is the *discipline*; everything under `canon/`, `gips/`, and `oips/` is the *artefact trail* of applying it.


---

# Proposal scoring rubric

A proposal (GIP or OIP) is scored in two tiers, in order:

1. **Admissibility gates** — binary. A proposal that fails any gate is *rejected*, not scored. A wrong or immaterial proposal is not a low-scoring proposal; it is not a proposal.
2. **Quality dimensions** — the six championship properties, scored only for admitted proposals. These derive directly from the six properties named in `canon/discipline.md`.

The rubric is itself an iteration target. If an admitted proposal genuinely cannot reach the quality bar as the rubric stands, that fact becomes the Triggering signal for a follow-on GIP (category `rubric-edit`) — never a reason to lower the bar. The gates themselves do not bend.

## What this rubric does not see

The rubric scores **one proposal at a time**. It is blind to **false negatives** — proposals the canon should have surfaced from a signal and didn't. A registry full of admitted, high-quality proposals is *not* evidence of a complete read on the source. Coverage is established by the historical-corpus regression check: every fixture under `canon/historical-corpus/` must remain re-authorable at master grade under the current canon, and an external residual escape (auditor finding, exploit post-mortem) is itself the signal for a GIP that closes the gap. Do not read a strong rubric score as "the source is clean."

---

## Tier 1 — Admissibility gates

Each gate is a yes/no question answered against the source signal. Any "no" rejects the proposal.

- **True** — the fact the proposal's Triggering signal cites is actually present at the cited location (file:line, registry id, residual entry, halt log). Not a misread, not a pattern that superficially matches. A false positive fails here.
- **Reachable** — there is a concrete path by which the cited consequence occurs: who calls it, under what state, with what input. For OIPs, this is the contract execution path. For GIPs, this is the canon-authoring scenario where the gap surfaces. A purely theoretical concern with no reachable trigger fails here.
- **Material** — the consequence is real: funds, access, liveness, correctness, or governance integrity. A style preference, a redundant check, or a change whose worst case is cosmetic fails here. A champion's annotated game marks the moves that decided it — not every legal alternative.
- **Atomic** — the proposal is one change with one Mechanism. A compound proposal is split into separate proposals *before* either is scored; a proposal bundling two changes fails here.

**Exemption: `review-digest` OIPs.** For an OIP of category `review-digest` (per `canon/proposal-schema.md` §"Categories per class"), Atomic and Material apply at the OIP level, not per-item. Atomic: one digest per contract per review session. Material: the OIP's aggregate effect on the contract — does fixing this list improve correctness, observability, or operational integrity? Findings inside the digest individually fail the per-item Material test (they would be rejected as atomic OIPs), but the digest's existence as one developer-facing document is itself Material. Borderline-Material findings — those where a competent reviewer might judge Material either way — default to the digest; the atomic OIP route is reserved for clear-Material findings only.

A rejected proposal is logged with the gate it failed and discarded. If a rejected proposal points at a real defect that the gate definitions don't capture, that becomes a Triggering signal for a follow-on GIP refining the gate — but the proposal as written stays rejected.

---

## Tier 2 — Quality dimensions

Six dimensions, scored only for admitted proposals. Each is scored against a qualitative tier — what a master would say about a proposal at that level. Numbers, where used downstream, are illustrative of these tiers, not a target to optimize toward.

### 1. Self-contained — property "self-contained at every level"

A reader picks up the proposal cold — no knowledge of the source, the registry, the surrounding proposals — and can act on it. Every `file:line`, every cross-referenced concept (sibling proposal, canon section, vocabulary term, helper library) is named inline or quoted in place.

- **Master** — the reader needs nothing but the proposal and the source tree; at most one external pointer, navigable by `grep` or by registry id.
- **Adequate** — the reader must consult one or two other places in the registry or canon to act.
- **Failing** — the proposal only makes sense with the canon, prior GIPs, or signal-source documentation open alongside it.

### 2. Plain-language — property "plain-language load-bearing"

Every first use of an *insider term* in a proposal body must carry one of two markers: (a) an inline definition in the same sentence or the immediately following sentence, or (b) a backtick-wrapped reference to its defining canon file or section (e.g., `` `drift-scope` (see `canon/proposal-schema.md` §Preamble fields) ``).

The insider glossary (extended via subsequent `rubric-edit` GIPs as the canon grows) currently contains: `drift-scope`, `fingerprint`, `canon-tuning loop`, `sub-master`, `master-grade`, `Tier 1`, `Tier 2`, `signal.kind`, `Triggering signal`, `operator`, `agent`, `bundle`, `corpus regression`. A named external library (e.g., OpenZeppelin's `EIP712` base) substitutes for an inline definition when the proposal cites where the reader can find its docs.

Scoring is by violation count, where one violation is one first-use of a glossary term without (a) or (b):

- **Master** — zero violations.
- **Adequate** — one or two violations.
- **Failing** — three or more violations.

**Sub-rule: load-bearing prose.** Each paragraph in a proposal body must introduce at least one **structural element** the proposal hadn't yet named. Structural elements are: a new `file:line` (or canon-file + section heading) citation, a new constraint the change imposes, a new alternative considered, a new threat or mitigation, a new known cost, a verbatim quote of source signal or source code, or a new mechanical check the rubric will apply. A paragraph that paraphrases an earlier paragraph, hedges an already-stated claim, or comments on the change without introducing a structural element is a **load-bearing violation**.

Scoring (by load-bearing violation count):

- **Master** — zero violations.
- **Adequate** — one or two violations.
- **Failing** — three or more violations.

**Sub-rule: quote-size discipline.** A verbatim quote of source signal or source code that exceeds 5 lines counts each of its lines as **load-bearing** or **incidental**. A quoted line is load-bearing if any of the following hold: (a) the proposal cites the line by line number in body prose, (b) the line contains an identifier (function name, type, error, role, storage variable, parameter) referenced elsewhere in the proposal body, or (c) the line is syntactically necessary to make the load-bearing lines parse — the enclosing function signature, the opening or closing brace, the contract or interface declaration. All other lines (sibling branches not cited, unrelated logic, comment blocks not addressing the cited fact) are **incidental**. Quoted blocks of ≤5 lines bypass this sub-rule.

**Quote load-bearing ratio** is computed across all in-scope quoted blocks in the body: `(sum of load-bearing lines) / (sum of total lines)`. Each line counts once, regardless of which block it sits in.

Scoring (by ratio):

- **Master** — ratio ≥ 0.75, OR every quoted block in the body is ≤5 lines (sub-rule bypassed).
- **Adequate** — ratio 0.50–0.74.
- **Failing** — ratio < 0.50.

**Sub-rule: sentence density.** Each sentence in a body paragraph or top-level bullet item (excluding section headers, bold-label prefixes such as `**Drawback title.**`, table cells, and verbatim code blocks) must satisfy at least one of: (a) introduce a new structural element of one of the seven types named by the load-bearing-prose sub-rule (`file:line` citation, constraint, alternative, threat or mitigation, known cost, verbatim source quote, mechanical check); (b) introduce a new identifier — function, type, error, role, storage variable, or parameter — that the proposal references elsewhere in the body or names in the §Specification's patch; (c) be a connective of ≤6 words that sets up a list, citation, or quote that immediately follows. A sentence failing all three is a **sentence-restatement violation**.

**Sentence boundary.** A sentence is text terminated by `.`, `?`, or `!` followed by whitespace or paragraph break. Semicolons join clauses within one sentence; em-dashes do not create new sentences. Backtick-wrapped tokens (`` `path/to/file.sol:NN` ``) count as one word per backticked unit when measuring connective length.

Scoring (by violation count):

- **Master** — zero violations.
- **Adequate** — one or two violations.
- **Failing** — three or more violations.

**Dimension score** is `min(insider-term tier, load-bearing-prose tier, quote-size tier, sentence-density tier)`. A proposal Master on three sub-rules and Adequate on one scores Adequate on the dimension; the lowest sub-rule tier sets the dimension tier.

### 3. Evidenced — property "evidenced before persuasive"

Every truth claim the proposal makes is checkable: it cites `file:line`, a registry id, a named mechanism, or a verbatim quote — not assertion or rhetoric. The reader can confirm the *problem* is real from the proposal alone, without taking the author's word for it.

- **Master** — every claim is independently checkable against a citation.
- **Adequate** — the load-bearing claim is cited; one secondary claim leans on a documented cross-link.
- **Failing** — claims are asserted; the reader cannot tell from the proposal whether the issue is real.

**Sub-rule: behavior-assertion citation.** A sentence that attributes a behavior to a named function, contract, type, error, role, or storage variable must cite `file:line` for the assertion's verification. Behavior attributions are verb phrases of the form: called / invoked / called-by, reverts / fails / fires, returns / yields, reads / writes / modifies, transfers / locks / releases, emits, holds / contains, gates-on, interacts-with. The citation may be one of:

1. **Inline citation** in the same sentence — e.g., `` (`runs/<path>:NN`) `` or `` (`gips/GIP-NNNNN.md` §Mechanism) ``.
2. **Implicit citation** via a verbatim quote in the same section that contains the named identifier — the quote's leading file:line cite covers attributions about identifiers in the quote.
3. **Self-citation** when the named entity is in the proposal's own drift-scope and the behavior is named by §Specification's patch — §Specification's file:line is the citation.

A behavior attribution failing all three is a **behavior-citation violation**. Pure naming ("the `X` function", "uses `Y`", "imports `Z`") does not trigger; the rule fires only on verb phrases attributing behavior to a named entity.

Scoring (by violation count):

- **Master** — zero violations.
- **Adequate** — one violation.
- **Failing** — two or more violations.

**Dimension score** is `min(qualitative tier, behavior-citation tier)`. The qualitative scoring above remains the upper bound; the sub-rule sets a mechanical floor.

### 4. Mechanism is sound — property "mechanism is sound"

The change is concrete (no "subsumed by" hand-offs, no "document this" without naming the doc), minimal (no incidental rewrites), and ships with a verification an operator can run. Four mechanical bullets:

1. **Specification anchors.** Every affected surface is cited by `file:line` (OIPs) or by verbatim old text + verbatim new text (GIPs). For `review-digest` OIPs (per `canon/proposal-schema.md` §"Categories per class"), each task card additionally satisfies the Fix-paragraph patch-sketch requirement of `canon/signal-sources/contract-review.md` §7 Task definitions — when the change is more than a one-liner, the card's Fix paragraph contains a ≤ 10-line patch sketch showing the inserted code, not a prose paraphrase of it. A card whose Fix replaces a non-one-liner with prose-only description fails this bullet.
2. **Test plan runnability.** Each named test is either a runnable shell command (e.g., `forge test --match-test test_X`, `shasum -a 256 …`) or an `op-check:`-prefixed operator-runnable check (e.g., `op-check: grep -c "Tier 1" canon/rubric.md`).
3. **Test target existence.** Every file or directory named as a test target (`runs/<name>/source/<path>`, `canon/<file>`, etc.) exists in the repository at HEAD. A test naming a deleted or nonexistent artifact fails this bullet — this is the round-2 GIP-00001 reject failure mode encoded as a gate.
4. **Regression surface.** The Drawbacks section names what the change could regress (specific files / behaviors / invariants), not just generic costs.

Scoring is by bullet count:

- **Master** — all four bullets satisfied.
- **Adequate** — three of four satisfied; the missing bullet is regression-surface enumeration (specific items implicit in Drawbacks but not listed).
- **Failing** — two or fewer satisfied, OR any named test target doesn't exist at HEAD (this fails the dimension regardless of the other bullets).

### 5. Repeatable — property "repeatable under composition"

Two agents running the same canon snapshot on the same signal produce the same fingerprint. Five mechanical checks cover every fingerprint input and the proposal's traceability:

1. **Title length.** ≤80 chars (per `canon/proposal-schema.md` §Preamble fields).
2. **Category in enum.** `category` value present in `canon/proposal-schema.md` §Categories.
3. **Signal kind in enum.** `signal.kind` value present in `canon/proposal-schema.md` §Preamble fields.
4. **Drift-scope paths exist.** Every entry's path portion (everything before the first `:`) exists in the repository at HEAD.
5. **Canon citation.** The proposal body cites at least one canon section by file path + heading (e.g., `` `canon/rubric.md` §"Tier 1 — Admissibility gates" ``).

Scoring:

- **Master** — all five checks pass.
- **Adequate** — four pass; the missing one is the canon-citation check (the proposal references the canon but not by explicit section heading).
- **Failing** — two or more checks fail.

### 6. Feedback-ready — property "self-closing feedback"

The proposal carries the metadata that lets it participate in the canon's feedback loops — and only that; precision already scored under *Self-contained* and *Evidenced* is not re-scored here. If the proposal is challenged and turns out wrong, drafting a superseding proposal is mechanical. If the cited source changes, drift detection knows to revisit, because `drift-scope` is present and precise.

- **Master** — `drift-scope` is precise and mechanical; the proposal can enter the drift detector with no manual translation, and a superseding proposal can be authored against the cited source without rediscovery work.
- **Adequate** — drift scope is present but coarse; supersession needs light manual work to identify the affected surface.
- **Failing** — the proposal is static; every feedback path requires a human to translate it first.

---

## Master-grade

A proposal is **master-grade** when:

1. It passes **every Tier 1 gate**, AND
2. It scores at **Master** on *Evidenced* and *Mechanism is sound* — the two substance-facing dimensions — and no lower than **Adequate** on the other four.

A proposal at **Adequate** on a substance-facing dimension is shippable but flagged; a proposal **Failing** any dimension is not.

## Corpus regression detection

When a GIP candidate would patch the canon, every fixture under `canon/historical-corpus/` is re-authored under the would-be patched canon and re-scored. Each fixture's new scoring is compared against its stored expected scoring using tier movements only — never within-tier variation.

Three possible outcomes per fixture:

- **Improved** — at least one dimension tier moves up (Failing → Adequate, or Adequate → Master), and no dimension tier moves down. Informational; recorded but not blocking.
- **Stable** — no dimension tier changes. Informational; the expected case for canon changes that don't touch the rules driving this fixture.
- **Regressed** — any dimension tier moves down. **Blocks GIP admission** regardless of any other dimensions improving or any other fixtures Improving; tier drops are not traded off against tier gains.

Within-tier variation is invisible by design. A re-authored fixture that's "still Adequate but better" does not count as Improved — the bar is real tier crossings, not polishing.

The corpus check runs only for GIPs that touch the canon (per `canon/authoring-procedure.md` Step 5). OIPs do not change the canon, so the check is a no-op for OIP candidates.

## Stop condition (per signal)

A signal terminates with a bundle (per `canon/authoring-procedure.md`):

1. **Bundle at master-grade** — the canon-tuning loop converged: the proposal scores Master per §Master-grade and the corpus regression check passed. The bundle (proposal + canon-delta GIP if any canon edits were made) hands to the operator at `status: Draft`.
2. **Bundle at sub-master** — the loop exited because no valid canon edit was found to lift a remaining failed Tier 1 gate or sub-master Tier 2 dimension. The bundle hands to the operator anyway, flagged sub-master in the proposal's `Open questions`. The operator's approve / reject-with-reason action applies as usual.

There is no per-signal iteration cap. The loop's natural bound is the availability of valid lifting edits — once no edit lifts the score, the loop exits.


---

# Authoring procedure — operator-approved bundles

The procedure has two roles. The **agent** runs the canon on a signal, scores the output against the rubric, edits the canon when output is sub-master, re-runs, repeats — until the proposal reaches master-grade or no canon edit can be found that lifts a sub-master gate or dimension. The agent's output is a **bundle**: the canon delta as a GIP if any was needed, plus the proposal as Draft in its registry. The **operator** has one action: **approve** (everything in the bundle → Final, canon files patched, all in one commit) or **reject-with-reason** (bundle discarded; the reason becomes the next signal).

The proposal is the discipline's answer to the signal. The canon delta is the tuning that produced the answer. The operator ratifies both at once or rejects both at once.

## The loop

You are a fresh-context agent handling one signal (`<SIGNAL>`, named in the orchestrator's prompt). You have no memory of prior sessions; everything you need is on disk.

### Read first

1. `canon/discipline.md` — the championship framing.
2. `canon/rubric.md` — Tier 1 admissibility gates, Tier 2 quality dimensions, corpus regression definitions.
3. `canon/proposal-schema.md` — body shape, signal kinds, registry conventions.
4. `gips/GIP-00000.md` — the canon. Read; you may edit during this session (see §Constraints) but the edits are tentative until operator approval bundles them as a GIP.
5. `canon/signal-sources/<source-type>.md` — the reader for the signal's source type.
6. `canon/historical-corpus/` — fixtures, for the regression check (Step 3).
7. `runs/<name>/run.md` if a run is in play.

### Step 1 — Read the signal

Locate `<SIGNAL>`. The kind shapes how to read it:

- `review-finding` / `cross-contract-pattern` — read the finding card; cite `file:line` from the source contract.
- `residual-escape` — read the external finding (auditor report, exploit post-mortem); cite verbatim, source named.
- `operator-reject` — read the prior bundle and the rejection reason; cite both. The next pass must address the reason in addition to the original signal.
- Other kinds — per `canon/proposal-schema.md` §Preamble fields.

Carry the citation forward verbatim — the proposal's Triggering signal depends on it.

### Step 2 — Iterate the canon

Iteration `i = 0`. At each iteration:

1. **Author the proposal** against the current canon state (signal-source reader + proposal-schema body shape). Compute fingerprint per `canon/proposal-schema.md` §Fingerprint.
2. **Score against the rubric.** Tier 1 admissibility gates first. If any gate fails, the iteration target is to lift the failing gate, not to score the dimensions. If all gates pass, score the six Tier 2 dimensions to Master / Adequate / Failing.
3. **Decide.**
   - **Master-grade** (per `canon/rubric.md` §Master-grade) → exit the loop. Go to Step 3.
   - **Sub-master, valid edit available** → identify one canon edit that closes a specific failed Tier 1 gate or lifts a specific sub-master Tier 2 dimension. The edit must cite the gate or dimension it targets and be the minimal wording change that closes it. Apply the edit to the working tree (tentative — not committed). Increment `i`. Re-enter Step 2.
   - **Sub-master, no valid edit available** → exit the loop. Go to Step 3 with the bundle flagged sub-master.

The loop continues as long as edits keep lifting the score. There is no iteration cap; the bound is the availability of valid lifting edits. A canon edit that does not lift any specific gate or dimension is not a valid edit and is not applied.

Every canon edit during the loop is tentative — written to the working tree, never committed mid-loop. The accumulated edits become the canon-delta GIP at Step 4 and are committed together at operator approval (or discarded on reject).

### Step 3 — Corpus regression check

Before assembling the bundle, walk `canon/historical-corpus/` under the tentatively edited canon and confirm no fixture has **Regressed** (per `canon/rubric.md` §"Corpus regression detection"). For loops that made no canon edits this is a no-op.

A Regression invalidates the offending edit. Back it out, mark it as a tried-and-failed direction in the canon-delta GIP's Rationale, and either re-iterate from Step 2 (continuing the loop without that edit) or exit to Step 4 with the bundle flagged sub-master.

### Step 4 — Bundle assembly

Write the bundle files into the working tree, all at `status: Draft`:

| What | Where | When |
|---|---|---|
| The proposal | `gips/draft/GIP-NNNNN.md` (governance signal) or `oips/draft/OIP-NNNNN.md` (blockchain signal) | Always |
| The canon-delta GIP | `gips/draft/GIP-NNNNN.md`, category per the area touched (`canon-edit`, `rubric-edit`, `discipline-edit`, `procedure-edit`, `schema-edit`, `signal-source-edit`, `corpus-addition`) | Only if the loop made canon edits |
| The review-digest OIP | `oips/draft/OIP-NNNNN.md`, category `review-digest` | When a contract-review signal surfaced ≥1 non-Material-but-actionable finding per `canon/signal-sources/contract-review.md` §Output |

Both `gips/` and `oips/` have three siblings: top-level (admitted — `Final` or `Living`), `draft/` (awaiting operator approval), and `rejected/` (operator-reject audit records). The directory tells you a proposal's status at a glance.

If the original signal was itself a canon-change request and the loop's only output is canon edits, the proposal IS the canon-delta GIP — one bundle artifact, not two.

The canon-delta GIP's `Mechanism` is the canon patch; its `Rationale` is the iteration trace — for each edit, the gate or dimension it lifted and why it was the minimal change that did so. The trace is the explanation the operator reads when deciding to approve or reject.

Update registry indices (`gips/INDEX.md` + `.index.json`, `oips/INDEX.md` + `.index.json`).

If the loop exited at sub-master, flag it in the proposal's `Open questions`: *"Loop exited at sub-master on <gate-or-dimension>; no canon edit was found that lifts it. Operator review required."*

### Step 5 — Hand-off

Record the session in `runs/<name>/run.md` (if a run is in play): signal cited, iteration count, canon edits summary, final tier scoring, corpus result. Return a ≤ 250-word summary naming each bundle file by path.

**Tier scoring format.** The "final tier scoring" item records per-dimension tiers and, for dimensions with mechanical sub-rules, per-sub-rule violation counts so a second-reader audit can reproduce the score without re-walking the proposal. One line per dimension. Format:

- **Self-contained** — qualitative only: `Master | Adequate | Failing` + one-clause reason.
- **Plain-language** — four sub-rules: `insider-term N violations · load-bearing-prose N violations · quote-size <ratio or "bypassed (all blocks ≤5 lines)"> · sentence-density N violations · dimension = min(...) = <tier>`.
- **Evidenced** — qualitative + sub-rule: `qualitative <tier> · behavior-citation N violations · dimension = min(...) = <tier>`.
- **Mechanism is sound** — four bullets: `specification anchors <pass/fail> · test plan runnability <pass/fail> · test target existence <pass/fail> · regression surface <pass/fail> · dimension = <tier per bullet count>`.
- **Repeatable** — five checks: `title-length <pass/fail> · category-in-enum <pass/fail> · signal-kind-in-enum <pass/fail> · drift-scope-paths-exist <pass/fail> · canon-citation <pass/fail> · dimension = <tier per check count>`.
- **Feedback-ready** — qualitative only: `Master | Adequate | Failing` + one-clause reason on drift-scope precision.

The format applies whenever Step 5 records scoring — whether the loop exited at master-grade or sub-master.

The bundle sits in the working tree at status Draft until the operator's next action.

## Operator action

One action: **approve** or **reject-with-reason**.

**Approve** = (1) move each Draft proposal from its registry's `draft/` subdirectory to the registry's top level (`gips/draft/GIP-NNNNN.md` → `gips/GIP-NNNNN.md`, or `oips/draft/OIP-NNNNN.md` → `oips/OIP-NNNNN.md`); (2) bump preamble `status` from Draft to Final; (3) update the registry index link path; (4) commit the moved files + canon-file patches if any + registry updates in one commit citing the proposal id.

**Reject-with-reason** = revert the working tree (discard the proposal files and any canon edits in this session); record the rejection as a new signal at the registry's `rejected/` subdirectory (`gips/rejected/<bundle-id>.md` for GIP bundles, `oips/rejected/<bundle-id>.md` for OIP bundles) with `signal.kind: operator-reject`, `source: <rejected-bundle-id>`, body = the operator's reason verbatim. Reject records are meta-layer governance artifacts and live alongside the registries, not under run scopes — a signal that arrived from outside any run still produces a reject record at this path. The next session reading this signal must address the reason in addition to the original signal.

The operator does not edit the bundle. The operator does not iterate the bundle. Approve / reject-with-reason are the only operator-mediated transitions.

## Constraints

- **No silent canon drift.** The agent edits canon files only as part of producing a bundle. Direct canon edits outside this procedure are forbidden.
- **Every canon edit is justified.** Each edit cites the Tier 1 gate or Tier 2 dimension it lifts; the canon-delta GIP's Rationale enumerates all edits with their justifications.
- **No rubric loosening.** A canon edit that lowers the bar (weakens a gate definition without compensating tightening elsewhere) is a violation. The operator can reject such bundles; a follow-on signal closes the gap properly.
- **Corpus contract.** No bundled canon delta may cause a previously-passing historical-corpus fixture to **Regress**.
- **Repeatable at every canon snapshot.** Two agents reading the same canon snapshot on the same signal produce the same proposal. The iteration loop changes the canon between iterations, not within one — at every snapshot the *Repeatable* property is mechanically reproducible.
- **Proposals are generalizations of the signal.** A candidate that hard-codes source-specific identifiers in places where they are not load-bearing (e.g. in `Rationale`) fails the rubric's *Repeatable* dimension on the next agent's read.
- **One signal at a time per working tree.** Concurrent in-flight bundles in the same working tree would entangle canon edits. Multi-signal orchestration is the orchestrator's concern (see `AUTONOMOUS.md`), not this procedure's.
- **Canon edits are forward-only by default.** A canon edit (any GIP touching `gips/GIP-00000.md` or files under `canon/`) applies to proposals admitted at or after its Final commit. Admitted proposals are not re-scored under later canon edits. A canon edit that intends retroactive scope to admitted proposals must declare it explicitly in its `Mechanism` and name the registry IDs it re-scores.

## Tools

`Read`, `Edit`, `Write`, `Bash`, `Glob`, `Grep` (Claude Code names; the canon currently runs on Claude Code — runtime portability is a separate concern, flagged for a future GIP).
