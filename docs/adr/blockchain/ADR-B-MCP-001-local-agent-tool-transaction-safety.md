# ADR-B-MCP-001: Local agent tools expose explicit read and transaction-intent boundaries

- **Status:** Proposed; current TypeScript implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `mcp` package, stdio tools, key/RPC/ABI/registry configuration
- **Depends on:** ADR-B-CLI-001, ADR-B-RPC-001, ADR-B-CRY-001

## Context

`@outbe/mcp` is a local MCP server exposing many chain queries and curated signed
transactions for Tribute, staking, rewards, Oracle, Intex and cross-chain intents. It
loads `OUTBE_PRIVATE_KEY`, constructs/approves/signs transactions and sometimes performs
multi-step workflows. A natural-language tool caller must not turn convenience into
ambiguous authority or invisible side effects.

## Decision

The server has two explicit modes: read-only without a signer and transaction-enabled
with a named account/chain policy. Every mutation tool produces a deterministic
transaction intent before signing: network/chain id, signer, target code/address, method,
decoded arguments, native value, allowance changes, gas/fee bounds, prerequisites and
expected postcondition. Multi-transaction operations expose each step and never describe
partial submission as complete success.

Keys are supplied by an OS/agent secret boundary, never logged, returned, persisted in
tool state or copied into error output. Tool schemas use exact integer/string units,
checksummed addresses, bounded arrays and explicit network. Registry addresses and ABIs
are versioned deployment manifests verified against chain id and code hash.

## Authoritative interfaces

- `createCtx` owns RPC chain probing and optional signer creation.
- `readView` and read tools are side-effect-free queries.
- `sendTx/sendRaw` and domain-specific signing tools are the only mutation boundary.
- `registry`, Intex/intent registries and token maps own address/ABI configuration.
- stdout is reserved for MCP framing; diagnostics use sanitized stderr.

The current tool registry is classified as follows:

| Class | Registered tools |
|---|---|
| Generic/RPC reads | `chain_info`, `block_get`, `transaction_get`, `transaction_receipt_get`, `contract_call` |
| Native protocol reads | `tribute_get`, `tributes_by_owner`, `tributes_by_day`, `worldwide_day_totals`, `nod_get`, `nods_by_owner`, `gem_get`, `gems_by_owner`, `gratis_balance`, `promis_balance`, `fidelity_index`, `agentreward_claimable`, `worldwide_days_offering`, `worldwide_day_get`, `currency_pairs`, `currency_rate`, `currency_rate_vwap`, `validators`, `validator_get`, `staking_info`, `rewards_claimable`, `metacanon_get`, `canon_get`, `oip_get`, `gip_get`, `oip_list`, `gip_list` |
| Native protocol mutations | `tribute_offer`, `staking_stake`, `staking_unstake`, `staking_unbonded_claim`, `rewards_claim`, `agentreward_claim`, `oracle_feeder_delegate`, `oracle_vote_submit` |
| Intex/auction reads | `intex_series_info`, `intex_series_list`, `intex_holdings_by_owner`, `intex_series_balance`, `auctions_active`, `auction_info`, `auction_bids_by_owner`, `intex_payment_allowance`, `intex_bridge_quote`, `intex_promis_balance` |
| Intex/auction mutations | `auction_bid_commit`, `auction_bid_reveal`, `auction_bid_cancel`, `intex_claim_commit_bond`, `intex_payment_approve`, `intex_bridge_send`, `auction_bid_settle`, `auction_settler_set`, `intex_promis_mine` |
| Intent saga | `intent_order_open`, `intent_order_track`, `intent_order_refund` |

`intent_order_track` is read-only; `intent_order_open` and `intent_order_refund` are
mutations. A new registered tool must be added to this table with its effect class in the
same change; inference from its name is not an authorization rule.

## Invariants

- Read-only mode cannot sign or send through any tool path.
- Signed intent targets the displayed chain, address, calldata, value and signer exactly.
- Amount units, decimal conversion and integer narrowing are unambiguous and checked.
- A tool never loses required recovery material after causing an irreversible first step.
- `success` means a successful receipt and verified postcondition; `submitted`, reverted,
  timed out and partially completed are distinct outcomes.

## Atomicity, replay and failure

On-chain atomicity is determined by each transaction. Tool-level workflows with approval
then action, source then destination, or commit then reveal are explicit sagas with a
stable local intent id, resumable state and compensation/recovery instructions. Retry
checks receipt/state before resubmitting. Nonce selection is delegated to the wallet/RPC
with concurrency serialization; business nonces are collision-resistant and durable.

## Determinism and bounds

Schemas bound strings, arrays, gas, fee, timeouts, polling and PoW computation. Wall-clock
time is advisory and never the sole uniqueness source. Auto gas overrides are explained
and capped. RPC responses are decoded/validated before driving a write.

## Security, compatibility and activation

Server version, tool schemas, dependency locks, chain manifest, ABI/address/code hashes
and allowed signer/network form one profile. Published npm artifacts are reproducible and
contain no deployment secrets. Adding a mutation tool requires security/effect review.

## Production-interface verification evidence

Inspected startup/stdio context, private-key loading, generic view/send helpers, Tribute
encryption/submission, staking/reward/Oracle tools, Intex commit/reveal/approval workflows
and intent open/refund flows. The package has build/typecheck scripts but no dedicated
test suite listed in `package.json`.

## Consequences

Agents and humans can inspect intent and partial progress without trusting prose. Tool
convenience cannot silently widen on-chain authority.

## Rejected alternatives

- Treating every local MCP caller as implicitly authorized for all writes is rejected.
- Reporting only a transaction hash as successful workflow completion is rejected.
- Keeping irreversible recovery secrets only in process memory is rejected.

## Open questions and technical debt

- **Critical:** Intex commit documentation says reveal inputs are remembered only for the
  session. Persist an encrypted recovery record or require/export a user-held artifact
  before submitting the commitment.
- **Critical:** intent `senderNonce` uses `Date.now()`, which can collide across concurrent
  opens/processes and is not a durable nonce allocator. Use on-chain/account-scoped
  monotonic or cryptographically random collision-checked identity.
- Auto-approval creates a separate irreversible transaction. Display/return partial
  state, prefer exact allowance, and resume safely if the following action fails.
- `OUTBE_PRIVATE_KEY` is a raw long-lived environment secret; support external signer/
  keystore hardware boundaries and document process/env exposure.
- Validate registry address code hashes and expected chain id rather than trusting local
  constants plus `eth_chainId` alone.
- Replace fixed gas constants with bounded simulation/profile evidence; Tribute's
  simulation exception must not become a generic bypass.
- Add tests for schema coercion, wrong chain/address, malicious RPC responses, concurrent
  nonces, receipt timeout/reorg, partial workflows, secret redaction and read-only mode.
