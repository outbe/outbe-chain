# Mara's Rows

*The farmer's view.* One farmer, followed from claim to sale.

---

## I · The business

Mara farms. The model is easy to state and unusual in one respect: she does not buy seed. The valley grants it against what she consumed, and it prices a fresh allotment every day — a window opens each morning and closes each evening.

Hers is a rolling operation. She has hundreds of allotments outstanding at any time, each priced off the day it was claimed, and she claims again tomorrow.

The claim itself is a sealed record of what she put on the table. One per farmer per day, and once the window shuts the record cannot be revised by anyone.

## II · How the allotment is set

Her claim is checked before it is admitted. The party that witnessed the spending signs for it, and where the network requires it she proves the record in zero knowledge — the amount is confirmed without the detail being disclosed to anyone, the valley included. A claim that does not verify does not become an allotment.

What nobody does is *judge* it. There is no committee weighing her against her neighbours and no appeal: the formula that turns a verified claim into an allotment is published, fixed in advance, and she can run it herself. Two inputs, no discretion.

The first input is volume: what she consumed. The second is standing: how long she has held what she took, net of what she has sold.

> Two farmers with identical volume do not receive identical allotments. The one who has held longer and sold less takes materially more, and the gap compounds.

Her allotment lands about three and a half weeks after the day it covers. Her neighbour claimed larger volume this cycle and will take more. The valley's total scales with what was claimed, up to a cap that follows a published curve. It has not yet bound. When it does, every allotment is trimmed by the same proportion, together.

## III · The asset

What she receives is a fixed quantity with a floating value and two price levels attached.

The quantity never changes. What moves is what it is worth, and that tracks a daily reference price no farmer in the valley influences. The lower level is where the asset becomes worth exercising. The upper level is where the valley forces the decision. Between the two she has discretion; outside them she has none.

## IV · Waiting

Below the lower level she does nothing, and it costs her nothing. The asset does not decay, expire or require maintenance.

The lower level sits eight per cent above the price the allotment was priced at. Nothing counts toward crossing it but the price itself — an allotment is not maturing while it waits, and waiting neither helps nor costs. One claimed into a rising market clears immediately. One claimed at a peak clears when the market comes back past where it was, and until then it is simply below its level.

## V · The exercise

Above the lower level the decision opens. To take delivery she pays a cost fixed the day the allotment was priced. It does not reprice, and is the same figure if the market has doubled since.

> Fixed cost against a floating payoff. It is a call option struck at the grant price, and the band is its exercise window.

She pays with a sealed note. The note proves its amount without disclosing who funded it, and paying and taking delivery are one act — there is no paid-but-untaken state. Anyone may fund the note on her behalf; only she may spend it, and the note names its spender.

## VI · Forced exercise

If the reference price holds above the upper level on twenty-one of the trailing twenty-eight days, the valley calls the allotment. She then has seven days.

At the call the position is worth more than at any point since it was granted and the cost is unchanged, so every farmer who is paying attention exercises immediately and profitably.

Which is why the deadline exists at all. Without it a deeply in-the-money claim would sit outstanding forever. Forfeiture is an operations failure — somebody was not watching — rather than a commercial decision. Her neighbour lost an allotment that way, at the best price it ever printed.

## VII · Two revenue lines

The allotment is one. The other arrives without her doing anything at all.

Whatever the valley did not allot is taken to auction by an agent acting for the claimants. The agent has no discretion and takes no margin: it sells the residual at whatever clearing price the bidding produces, and distributes the proceeds.

That distribution is by volume alone, not by standing. So a farmer on her first claim collects a full pro-rata share of the cash while taking a minimal allotment. **Standing is priced into the asset line; the cash line treats everyone the same.**

## VIII · Working capital

Farmers need cash before their allotments mature, and the instrument for that is a forward sale with a repurchase right.

She sells the holding at today's price and takes the cash immediately, retaining the right to buy it back at that same price plus a charge for the time. The holding is pledged rather than disposed of, and her standing is unaffected — the count still treats it as hers.

If the price runs far enough above her sale price she is made to choose: repurchase now, or the sale is final. Seven days. Letting it stand is the sale completing on the terms she agreed to. She keeps the cash and the holding goes.

Last cycle she repurchased. The price had moved a third and the carry was a fraction of the appreciation.

## IX · Distribution

Nobody buys from the valley directly. The auction is a wholesale market — professional bidders in a sealed round, taking price risk on a clearing level they have to guess in advance. Their money is what funds the cash line back to the claimants.

Retailers buy from wholesalers afterwards, at a visible price, in the quantity they need. They warehouse it and issue it to their own customers, who then ripen and take delivery on terms built the same way as hers.

That is the only entry point for anyone who has never claimed.

One structural rule governs the whole chain: once an allotment is called it stops being transferable. Whoever holds it settles or forfeits. The obligation cannot be sold on.

## X · The long view

Standing compounds. It climbs steeply in the early years and then flattens toward a ceiling it approaches and does not cross. Mara reached the flat part some time ago and cannot out-hold a newer farmer indefinitely.

What she can still do is not sell. Every disposal is recorded and drags the ratio for years — and moving a holding out to the open market counts as a disposal whether or not she ever sells it there, because from that point nobody can tell the difference.

The cap rose through the valley's first years and has since settled onto its floor, where it stays. Her successor runs this business at that floor and will have to be more disciplined about it than she was in the fat years.

> It was never structured to be generous indefinitely. It only had to be generous while there was still a reason to claim.

---

## Addendum

Each step of the business, mapped to the call that performs it. Numerals match the sections above. Addresses are the precompiles on Outbe; argument lists are given as declared in `contracts/precompiles/src`. Verified against `main` at `296e8375`.

### What Mara does herself

Every one of these is a transaction she signs. Nothing above touches her holding without one of them, except what is in the next section.

#### I · TributeFactory.offerTribute

`offerTribute(cipherText, nonce, ephemeralPubkey, worldwideDay, tributeCurrency, referenceCurrency, excludeFromIntexIssuance, …)` · `0x…1100`

Seals her record for the day. The ciphertext is decrypted and validated inside the enclave; where the sender's network has ZK enabled, `check_zk_merkle_root_signature` requires a BLS signature over the root from the registered network key and `verify_full_proof` must pass against it. **One per owner per day** — identity derives from `(owner, worldwideDay)`, so a second offer for the same day is rejected.

#### V · PayNote.deposit

`deposit(address asset, uint128 amount, bytes32 noteSn)` · `0x…1019`

Funds the sealed note. Pulls the asset into the reserve vault and appends a commitment to a depth-32 Merkle tree. **Bearer** — spend authority is knowledge of the note key, not an address, so anyone may fund a note for her.

#### V · NodFactory.mineGratis

`mineGratis(uint256 nodId, uint64 nonce, bytes32 mac, uint64 opNonce, bytes payNoteProof)` · `0x…1007`

Paying and taking delivery in one transaction. Checks owner, proof-of-work, a qualified bucket and the call deadline, then `discharge_cost` consumes the note: its spender must equal the caller, its asset must be registered under the Nod's reference currency, and it must cover `entry × load`. Burns the Nod and mints exactly its load.

#### X · GratisFactory.mineCoen

`mineCoen(uint256 amount, bytes32 mac, uint64 opNonce)` · `0x…2003`

Moving it out of the barn into the public store. Burns Gratis, credits native COEN 1:1 — and **books a Fidelity disposal**, which is why the count stops.

#### VIII · GratisFactory.pledgeGratis

`pledgeGratis(uint256 amountStables, address asset, uint256 maxGratis, bytes32 mac, uint64 opNonce)` · `0x…2003`

Names the cash she wants, not the collateral. The rate is sealed into the pledge ticket; `maxGratis` is her slippage guard. **No cohort effect** — pledging costs no standing.

#### VIII · CredisFactory.settle

`settle(uint256 positionId, uint256 amount)` · `0x…1009`

The buy-back. Interest first, then principal; any amount that covers the interest due, at any time, from any payer. Collateral releases in proportion to the principal covered, always to the original pledger.

### What happens with nobody asking

No transaction, no signature, no discretion. These run on the protocol's own schedule and are why the farmer controls timing but not outcome.

#### II · Metadosis · WorldwideDay FSM

*Hourly `protocol_cycle`.*

FORMING → LOOKBACK → OFFERING → WAITING → READY. Closing OFFERING seals the day's records. READY splits the day's limit: the farmers' budget is `min(32% × nominal, limit)`, and the auction gets `min(nominal, limit)` less that. On a RED day — the day's COEN price closed no higher than the day before, or is missing — the farmers' budget is taken from an eighth of each input, and the auction's share carries over to the next day's limit.

#### II · Lysis · typed OCOMP program

*Off-chain, quorum-certified.*

The arithmetic everyone can run. Reads every sealed record and every farmer's league, and derives one share each. **Deterministic** — validators and full nodes compute it independently and must agree exactly.

#### II · NodFactory.materializeCertifiedNods

`materializeCertifiedNods(bytes canonicalBatch)` · `0x…1007`

Turns the certified result into real Nods in proof-backed batches. Submitted by any active OCOMP delegate; the content is fixed by the certified root, so the submitter chooses nothing.

#### III · Nod · bucket qualification

*Begin-block hook.*

The line being crossed. A monotonic false→true latch when the live COEN rate exceeds the bucket's floor. It never reverts, and no user can trigger or delay it.

#### VI · Nod · run_call_daily

*Daily Cycle trigger.*

Calls a bucket once the finalised daily VWAP sat above its call price on **21 of the trailing 28 days**, then forfeits members whose 7-day notice has lapsed and **credits their load back to PromisLimit**, the carry-over pool. Budgeted at 256 bodies per run and cursor-resumable.

#### VIII · CredisFactory · credis_call_daily

*Daily Cycle trigger.*

The same 21-of-28 count against a different threshold, `P₀ + 64%`, then a 7-day window. If it lapses unpaid, `void_position` burns the collateral still locked and credits PromisLimit 1:1. **No user transaction can arm a call or trigger the void.**

#### VII · Desis · auction_advance

*12-hourly Cycle trigger.*

Walks the auction schedule across every registered chain and clears at one uniform rate once all have reported. Metadosis briefs it once per day and never signals again.

### What other people do that reaches her

Calls signed by other parties whose effects land on Mara's cash line, her working capital, or the retail channel.

#### VII · IntexFactory.payContributorBatch

`payContributorBatch(uint32 worldwideDay, uint32 startIndex, ContributorLeaf[] leaves, bytes32[] proof)` · `0x…1015`

The cash line. Share is `round.amount × your nominal / eligible_nominal_total`, paid in native COEN. **Permissionless** — the Merkle proof is the authorisation, so anyone at all can deliver the valley's money to the farmers.

#### VIII · CredisFactory.requestCredis

`requestCredis(address smartAccount, bytes32 pledgeHandle, bytes32 spendAuth, uint16 referenceCurrency)` · `0x…1009`

Her credit agent, the originating CCA, opens the forward sale with the handle and spend authorisation from her pledge, and the cash goes to her account at the sealed price. Terms fix here and never move: principal, collateral, policy rate, entry price, call price. A called position she already holds does not block a new one.

#### IX · IntexFactory.settle

`settle(bytes14 seriesId, address intexHolder, uint256 amount, address paymentToken)` · `0x…1015`

A wholesaler settling their own holding into the reserve vault. Allowed from Qualified, or from Called while inside the notice period. Qualification is the floor alone.

#### IX · GemFactory.issueGemPosition

`issueGemPosition(bytes14 sourceIntexId, uint256 amount)` · `0x…2013`

The retailer's warehouse. Burns their Issued units and records capacity of `promis_load × units`, valid one year.

#### IX · GemFactory.issueGem

`issueGem(uint256 positionId, address owner, uint256 promisLoad)` · `0x…2013`

Issued to a retail customer. Drains the position's capacity. Entry price is `max(current rate, parked entry)`, so a retailer cannot issue against a stale cheap price.

#### IX · GemFactory.settleGem · GemFactory.minePromis

`settleGem(uint256 gemId, address asset)` · `minePromis(gemId, nonce, mac, opNonce)` · `0x…2013`

What the retail customer does next — a settle step into the reserve vault, then the mine, ending in Promis rather than Gratis. The cost is derived at settlement from entry price and load.

#### IX · IntexNFT1155._update

*ERC-1155 transfer hook.*

The one rule at the end of the beat. Issued units transfer freely while Issued or Qualified; **a Called series reverts every holder-to-holder transfer** — the settlement obligation stays with the holder. Settled units are soulbound.

### Two more doors, not in the story

Outside Mara's operation, but part of the same system.

#### PromisFactory.mineCoen · PromisFactory.mineGratis

`mineCoen(uint256 amount, bytes32 mac, uint64 opNonce)` · `mineGratis(amount, promisMac, promisOpNonce, gratisMac, gratisOpNonce)` · `0x…2337`

The Promis side's two exits: out to the public store, or across into the barn. The crossing **records a fresh Fidelity cohort at the current time** — the ABI says so in as many words — so acquisition age starts again from zero.

#### AgentReward.claimReward

`claimReward(uint8 pool, uint256 amount) → gemId` · `0x…100B`

How the agents who brought records to the window collect. The claim is issued **as a Gem** and the native COEN that backed it is burned, so the reward joins the same qualify → settle → mine track as everyone else's crop. Sizing the claim is the agent's own risk control — an unsettled Gem can be called and forfeited, while the unclaimed balance cannot.

---

Companion to [The Outbe Valley](https://claude.ai/code/artifact/42c2a9ff-a738-4850-8cb5-bcd69f6710b7), which gives the full correspondence and the constants, and to the Divergence Register (`docs/reports/divergence-register-2026-09-04.md`), which lists where the implementation and the stated design still disagree. Signatures read from `contracts/precompiles/src`; behaviour from `crates/core`.
