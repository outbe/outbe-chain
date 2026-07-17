# ADR-S-ORC-002: Oracle feeder owns external price ingestion and vote delivery

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Oracle operations and protocol economics maintainers
- **Scope:** `bin/outbe-feeder`
- **Depends on:** ADR-S-ORC-001, ADR-S-FEE-001, ADR-B-EVM-005, ADR-B-TXP-001
- **Related:** PFS for Oracle price publication (planned)

## Context

The feeder is an untrusted off-chain operator process. It polls external market
APIs, aggregates observations, signs one validator vote and attempts to deliver
that vote to the chain. It does not own pair registration, authorization, tally,
final prices or validator penalties; those are canonical Oracle state in ADR-S-ORC-001.

This distinction matters because an HTTP response, a locally computed average and
a transaction hash are progressively stronger attempts, not evidence that a price
was accepted or finalized.

## Decision

### Configuration and identity

One feeder instance is configured for one chain id, RPC endpoint, validator
identity, signer, vote cadence and an explicit provider set per currency pair. At
startup it must validate that the signer is the validator or its currently
delegated feeder, the live chain id and on-chain vote period match configuration,
and every configured pair is an active Oracle target.

Production secrets must enter through a secret manager, protected file descriptor
or hardware/remote signer. They must not be committed, logged, passed on a command
line or stored as plaintext application configuration. Mock providers and arbitrary
provider endpoint overrides are test-only capabilities.

### Observation and aggregation

Every provider response is untrusted. A usable observation binds provider, pair,
price, volume, provider observation time and local receipt time. Values must be
finite, positive, within configured numeric bounds and fresh for the target vote
period. Providers have explicit deadlines and failure isolation.

Aggregation requires a configured minimum of independent fresh providers. It uses
a versioned deterministic fixed-point policy with explicit rounding, overflow and
outlier rules. Candle and ticker inputs are distinct products: fallback between
them is explicit and observable. Configuration order cannot change the result.
The exact accepted inputs and algorithm version are retained in an operator audit
record without secrets.

### Delivery state machine

For each on-chain vote period the feeder persists this state machine:

```text
unobserved -> collecting -> prepared -> signed -> broadcast
                                      \-> abandoned
broadcast -> included -> finalized -> verified
          \-> replaceable -> broadcast
          \-> reverted/expired
```

`prepared` includes period, validator, sorted pair ids, fixed-point values and a
content digest. Exactly one intent may be active for an identity and period.
Restart resumes the durable intent and reconciles nonce, tx hash, receipt,
canonicality and `getAggregateVote`; it does not infer completion from process
memory. A retry may rebroadcast the same signed transaction or replace it under an
explicit nonce policy, but cannot silently construct different prices for the same
period after an earlier vote may have landed.

Preflight distinguishes permanent skips (Oracle disabled, already voted, identity
not authorized) from transient read/transport failures. Transient failures remain
retryable inside the voting window. A period is complete only after a successful
receipt and canonical Oracle read agree; operational success requires the chosen
finality policy.

### Health and observability

Readiness means configuration, signer access, live-chain identity and on-chain
authorization are verified. Liveness reports the last observed head and loop
progress. Vote health separately exposes last attempted, broadcast, included,
finalized and verified periods plus permanent-skip and failure reasons.

Health endpoints bind to loopback by default, have bounded request handling and do
not expose provider payloads, signer material or sensitive topology. Metrics use
on-chain timing rather than a hard-coded block duration.

## Authoritative interfaces

| Responsibility | Owner/entrypoint |
|---|---|
| Pair, authorization, vote and tally semantics | Oracle, ADR-S-ORC-001 |
| Fee waiver and admission | ADR-S-FEE-001 and ADR-B-EVM-005 |
| External observations and aggregation | feeder provider/aggregator boundary |
| Intent, signer, nonce and delivery reconciliation | feeder delivery state machine |
| Canonical/finalized verification | Oracle ABI through ADR-B-TXP-001 read classes |

## Invariants

- A vote intent is bound to one chain, validator, period and algorithm version.
- Every submitted pair value derives from the persisted accepted observation set.
- No stale, non-finite, zero, overflowed or under-quorum aggregate is signed.
- At most one unresolved nonce/period intent exists per signer identity.
- Restart cannot turn unknown delivery into a fresh conflicting vote.
- Submitted, included, finalized and verified are never represented by one flag.
- Feeder failure cannot mutate canonical Oracle state except through an authorized
  transaction executed by the normal block lifecycle.

## Atomicity, replay and failure

Local persistence commits the prepared intent before signing/broadcast and commits
the tx identity before reporting it. RPC timeouts, provider failures and process
termination leave a recoverable state. Reorgs move included work back to broadcast
or replaceable unless the on-chain vote is independently present. An on-chain
revert is terminal for that transaction but not automatically for the period.

Multiple processes using one signer require an external lease or are rejected;
provider-side concurrency alone is safe, signer/nonce concurrency is not.

## Security and trust assumptions

TLS authenticates transport endpoints, not economic truth. Provider diversity,
freshness, quorum and outlier policy reduce but do not eliminate correlated market
or API failure. The validator remains accountable for its delegated feeder vote.
RPC may be stale or malicious, so completion uses canonical/finalized reads from a
trusted or cross-checked endpoint. Signer compromise is outside Oracle tally
correctness but inside feeder operational security and requires revocation and key
rotation procedures.

## Compatibility and activation

Pair normalization, fixed-point scale, aggregation version, rounding, vote-period
selection and calldata ordering affect signed intent identity. Changes require an
explicit version and staged rollout compatible with the active Oracle ABI. Durable
intent storage requires migration and downgrade behavior.

## Production-interface verification evidence

Inspected the production loop, configuration, provider trait/implementations,
aggregation, vote encoding, Alloy transaction submission, receipt wait and health
server. Current unit tests cover arithmetic examples, preflight decisions and local
health counters. There is no crash/restart delivery model, provider-adversary suite,
durable intent store or feeder-to-finality e2e evidence. Status remains Proposed.

## Consequences

The feeder becomes an independently auditable delivery module rather than part of
the Oracle state ADR. The end-to-end publication saga belongs in a Protocol Flow
Specification referencing this ADR, Oracle, admission and verifiable RPC reads.

## Rejected alternatives

- **Treat transaction hash as success:** it proves neither execution nor finality.
- **Retry by rebuilding current prices blindly:** an unknown first transaction can
  land and make the replacement a conflicting intent.
- **Let one provider satisfy a production vote:** transport availability becomes
  price authority.
- **Put feeder policy into ADR-S-ORC-001:** off-chain failure/restart concerns obscure the
  deterministic on-chain state owner.

## Open questions and technical debt

1. `AccountConfig.private_key` is plaintext TOML and the startup warning accepts
   that design. Replace it with secret/signer indirection and document rotation.
2. Startup parses the key but does not prove its address matches the configured
   validator or current delegated feeder before entering the loop.
3. `last_voted_period` is process-local and initialized to zero. Add a durable
   intent/transaction journal and restart reconciliation.
4. The loop advances `last_voted_period` before submission and after every
   aggregation or submission failure. Transient failures therefore permanently
   abandon a still-open period.
5. Every `PreflightResult::Skip`, including RPC failure or configuration mismatch,
   is treated as a completed period. Introduce typed permanent/transient outcomes.
6. Receipt success is reported against the head observed before submission; there
   is no confirmation/finality wait or postcondition read of the accepted vote.
7. A 30-second receipt timeout leaves transaction/nonce state unknown but the loop
   abandons the period. Reconcile the hash before any replacement.
8. There is no lease preventing two feeder instances from sharing a signer and
   racing Alloy nonce selection.
9. Configuration validation must reject zero polling intervals, empty/duplicate
   pairs, duplicate providers, invalid/non-finite deviation thresholds and unsafe
   production mock providers.
10. Provider observations have no common freshness contract. Candle timestamps are
    retained but ignored; ticker observations carry no timestamp at all.
11. Candle aggregation accepts any available candles and bypasses deviation
    filtering/minimum-provider quorum. One provider can therefore determine a pair.
12. The so-called TVWAP is volume-weighted over assumed equal-duration candles;
    name and economic specification must match, including mixed provider windows.
13. `f64_to_u256` converts through a saturating Rust float-to-`u128` cast, while
    U256 accumulation uses saturating addition. Enforce decimal parsing and checked
    magnitude bounds instead of silently clipping economic values.
14. Outlier filtering with one observation accepts it, and its median/stddev/tie
    behavior lacks independent golden vectors and a minimum surviving quorum.
15. Ticker fallback replaces zero volume with `1.0`, inventing economic weight.
    Define missing-volume semantics explicitly.
16. Provider requests have five-second operation timeouts, but total collection is
    sequential across ticker and candle calls. Bound whole-period latency and fetch
    independent providers concurrently with a deterministic result cutoff.
17. The provider audit trail does not preserve source observations, timestamps,
    exclusions or aggregate algorithm version for incident reconstruction.
18. Live chain id is not checked against configuration before signing. RPC trust,
    endpoint failover and wrong-network protection need an operator contract.
19. Gas limit is fixed at one million and fee/nonce/replacement policy is implicit
    in Alloy. Define bounds for maximum pairs and behavior under fee spikes.
20. Health is initially healthy without successful startup preflight, assumes
    twelve-second blocks, and conflates receipt success with vote success.
21. Health binds to `0.0.0.0:9002` by default and `/status` is unauthenticated.
    Default to loopback and define exposure policy.
22. Add adversarial provider tests, crash points at every delivery transition,
    duplicate-instance/nonce tests, reorg/replacement tests and an e2e flow through
    admission, Oracle tally, finality and a verified canonical read.
