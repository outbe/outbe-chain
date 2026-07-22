# PFS-009: A worldwide day is auctioned across chains and creators are paid

- **Status:** Draft
- **Actors:** Cycle scheduler, Metadosis, Lysis, Desis, IntexFactory, Intex
  ledger, PromisLimit, OriginRouter, per-target auction stacks (TargetRouter,
  IntexAuction, EscrowAdapter, IntexNFT1155 + bridge), bidders, tribute
  creators
- **Trigger:** READY-day processing hands Desis a one-shot auction brief; the
  12h `auction_advance` Cycle trigger walks the schedule from there
- **Topology/services:** Outbe validators (origin), N registered target chains
  reached through the ERC-7786 hub (the origin itself is a loopback target),
  funded router relay floats
- **Referenced ADRs:** ADR-S-CYC-001, ADR-C-MET-001, ADR-C-LYS-001,
  ADR-C-DES-001, ADR-C-INX-001 through ADR-C-INX-007, ADR-C-PRM-003,
  ADR-B-XCH-001
- **Supersedes:** None

## Outcome

One worldwide day's Intex supply is auctioned once across every registered
target chain: winners are minted on their own chains, losers refunded, unsold
supply returns to PromisLimit, and the day's tribute creators receive the
proceeds of every winning chain exactly once.

## Acceptance contract

- **Source:** Metadosis READY-day processing (economic inputs) and bidders on
  the target chains (demand).
- **Trigger:** The one-shot brief (`dispatch_auction_brief`) recorded for the
  day; every later transition is driven by the `auction_advance` Cycle trigger
  and the Desis/Intex begin-block hooks.
- **Environment:** Finalized Oracle VWAP for day typing, a wired OriginRouter
  with at least one registered target, deployed target stacks, funded relay
  floats, the loopback gateway for the origin's own venue.
- **Canonical inputs:** Worldwide day key, brief supply and entry price, day
  type, the frozen target snapshot, per-chain revealed bids
  `(bidder, quantity, rate, timestamp)`, per-chain proceeds deliveries.
- **System under test:** Metadosis→Desis brief seam, the Desis schedule and
  clearing FSM, OriginRouter fan-out/fan-in, target auction stacks, IntexFactory
  issuance, the Lysis contributor map and the proceeds pot distribution.
- **Expected response:** Stage events per chain, `ChainBidsDone`/`ChainSkipped`,
  `AuctionCleared`/`AuctionClearedEmpty`/`AuctionCancelledRedDay`, series
  creation on every snapshot chain, per-chain mint/refund instructions,
  `UnusedSupplyReported`, creator payouts.
- **Response measures:** Issued count ≤ supply; issued × load + unused = brief
  supply; every bidder pays or is refunded exactly once; the creator payout sum
  equals the delivered pot exactly; replay of the same day is byte-identical.
- **Failure guarantee:** A failed dispatch rolls back all of its writes (a
  checkpointed brief leaves no partial day); a failed gate day retries next
  block; terminal stages never reopen; redelivered batches and messages are
  no-ops.

## Preconditions and canonical inputs

The day exists in Metadosis with a sealed tribute population; the Oracle holds
the previous day's finalized VWAP (else the day types RED); OriginRouter's
target registry is non-empty and each target's stack is wired with relay roles;
relay floats cover the stage fan-out; the loopback gateway is registered for
the origin chain.

## Success sequence

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | Metadosis | READY processing: Lysis transforms tributes, contributor map recorded per series | contributor list/total |
| 2 | Metadosis | `dispatch_auction_brief` (supply, entry price, day type) | Desis `Briefed`, `brief_green` |
| 3 | Desis | schedule tick at anchor: green starts, red cancels; STAGE_START to every snapshot chain | stage, `AuctionCreated`/`AuctionCancelledRedDay`, sends |
| 4 | Target stacks | commit window: bond-locked commitments; reveal window: revealed bids, bond release | escrow receipts, auction records |
| 5 | TargetRouter | relay bid batches + BIDS_DONE after clearing signal | relayed batches, marker |
| 6 | Desis | per-chain intake: batches, integrity-checked completeness | `ChainBidsDone`, per-chain counts |
| 7 | Desis | gate: all chains done or 12h deadline; uniform-rate clearing | `AuctionCleared`, `ChainSkipped` |
| 8 | Desis | unused supply back to PromisLimit | `UnusedSupplyReported` |
| 9 | IntexFactory | series creation + issuance instructions to every snapshot chain (winners grouped per chain, empty lists provision only); proceeds fan-in armed | series record, sends |
| 10 | Target stacks | winner mints, loser refunds, escrow finalization; proceeds routed to origin per chain | NFT balances, escrow state |
| 11 | IntexFactory | proceeds pot accumulates per source chain; round opens on fan-in completion or deadline | pot/arrived records |
| 12 | IntexFactory | begin-block drain pays contributors proportionally, dust to last | creator balances, cleared pot |

## Boundaries and conservation

Steps 1–2 share the READY-day system transaction; step 3 and each later Desis
transition run inside their own checkpoints (schedule tick per day, gate per
day). Clearing (7–9) commits terminal stage, PromisLimit return, issuance and
per-chain sends in one transaction. Cross-module equations: issued × load +
unused = brief supply; per-bidder paid + refunded = locked; Σ creator payouts
= Σ delivered proceeds per series.

## Observable completion contract

Authoritative reads: Desis `getAuctionStage`/per-chain views (precompile),
Intex series and contributor state (Rust API), target auction/escrow state and
NFT balances (contract ABIs), creator native balances. Events are evidence,
stage reads are authoritative when layers disagree.

## Replay, retry, restart and failure

Bid replay is keyed by `(day, chain, generation, batch)`: redelivered batches
are no-ops, higher generations supersede a chain's intake and reset its
completeness. Premature messages revert for transport redelivery; terminal
stages absorb late traffic silently. A silent chain is excluded at the fan-in
deadline and its bidders refund through the escrow's never-finalized path. A
failed clearing day stays gate-active and retries next block. Proceeds
deliveries are per-chain idempotent contributions to the pot; a pot with no
contributors sweeps to the reserve; a late delivery after a deadline-forced
round funds a supplementary round over the retained map.

## E2E scenario matrix

| Id | Scenario | Minimum topology | Required assertions | Automated by |
|---|---|---|---|---|
| PFS-009-01 | green day clears and pays creators | in-process, two-chain snapshot | stage walk, issued count, supply conservation, exact creator payout, replay determinism | `crates/core/e2e/tests/wwd_auction_clearing.rs` (green scenario) |
| PFS-009-02 | red day cancels before start | in-process | zero-supply brief, `Cancelled`, no PromisLimit spend | same test (red scenario) |
| PFS-009-03 | silent chain skipped at deadline | in-process, two-chain snapshot | gate waits inside window, clears without silent chain, reporting chain's bids issued | `test_runtime_e2e_auction_gate_deadline_skips_silent_chain` |
| PFS-009-04 | full multichain walk over live transport | origin + remote chain + hub | stage delivery, bids over transport, mints/refunds on both chains, proceeds both legs | GAP: exercised manually on testnet; no automated runner |
| PFS-009-05 | contract-side commit/reveal/escrow | Foundry, loopback pair | bond lifecycle, reveal validation, clearing execution, escrow finalization | `contracts/intex/test/foundry/cross-chain/LocalLoopback.t.sol` and suite |

## Open questions and technical debt

- PFS-009-04 needs an automated two-chain runner; today the live walk is a
  documented manual procedure.
- Skipped-chain refunds rely on the escrow finalize-timeout path; an explicit
  cancellation signal to the skipped chain is future work.
- A canonical bid tie-breaker (equal rate and timestamp) is still open in
  ADR-C-DES-001.
