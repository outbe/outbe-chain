# PFS-007: ZeroFee sponsorship preserves quota and paid fallback

- **Status:** Draft
- **Actors:** EOA owner, sponsor policy, txpool, executor, ZeroFee, AgentReward and operator CLI
- **Trigger:** A funded EOA installs an EIP-7702 delegation and submits eligible or paid transactions
- **Topology/services:** Four-validator Pectra localnet with canonical ZeroFee genesis allocation
- **Referenced ADRs:** ADR-B-GEN-001, ADR-B-EVM-001, ADR-B-TXP-001, ADR-B-CLI-001, ADR-S-FEE-001, ADR-C-AGR-001
- **Supersedes:** None

## Outcome

An EIP-7702 delegated account receives exactly its daily sponsored quota, observes
receipt-visible soft failure after exhaustion, and remains able to transact through
the normal paid path without consuming another quota slot.

## Acceptance contract

- **Source:** Funded non-validator EOA and operator CLI.
- **Trigger:** Install the canonical delegation, then submit eligible zero-tip and ordinary tipped calls.
- **Environment:** Pectra active from genesis; four validators finalizing; ZeroFee schema/version and AgentReward predeploy available.
- **Canonical inputs:** Chain id, EOA nonce/key, canonical ZeroFee address, UTC day, fee envelope, `claimReward(0)` calldata and daily limit 8.
- **System under test:** EIP-7702 execution, txpool admission, ZeroFee policy/counter, executor failure receipts, fee accounting, AgentReward and CLI signing.
- **Expected response:** Delegation designator, eight successful sponsored receipts, one quota-exhausted failure receipt, one successful paid receipt and canonical CLI authorization JSON.
- **Response measures:** Sponsored balance delta is zero; counter reaches exactly 8; ninth receipt has status 0 and `OutbeFailure(110)`; paid receipt has status 1, positive fee debit, no sponsorship event and unchanged counter.
- **Failure guarantee:** Rejected/failed sponsorship never debits the signer or increments quota; delegation never prevents the paid path.

## Preconditions and canonical inputs

- Genesis has `pragueTime = 0`, marker bytecode `0xef` at ZeroFee and schema version 1 in slot 0.
- The signer is a freshly generated, funded, non-validator EOA.
- Authorization binds the RPC chain id, canonical delegate address and self-auth nonce rule.
- Block timestamp is the authority for the UTC quota day.

## Success sequence

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | genesis/node | expose Pectra and ZeroFee allocation | genesis/code/storage reads |
| 2 | EOA | submit EIP-7702 self-delegation | delegation designator on EOA |
| 3 | EOA/txpool/executor | execute eight eligible calls | receipts, events, counter, balances |
| 4 | EOA/txpool/executor | submit ninth eligible call | failed mined receipt and code 110 |
| 5 | EOA | submit a tipped call | successful paid receipt and fee debit |
| 6 | CLI | sign canonical authorization | JSON fields recover canonical intent |

## Boundaries and conservation

Every call is a separate transaction. Quota is consumed only by successful
sponsorship classification/execution. `sponsored_count + remaining_quota = 8` for
the active day; paid transactions do not enter this equation.

## Observable completion contract

Completion is proved by canonical receipts, ZeroFee events/views, EOA code and
balance deltas. A submitted hash alone is insufficient. Committee finality/parity
must be added when the shell flow is migrated to the Rust harness.

## Replay, retry, restart and failure

Authorization nonce replay is rejected by EIP-7702 rules. Retrying the ninth free
call produces no counter/balance change. Restart must preserve the delegation and
counter. A paid retry follows ordinary nonce and fee rules.

## E2E scenario matrix

| Id | Scenario | Given / canonical inputs | When / trigger | Then / outputs and postconditions | Verification |
|---|---|---|---|---|---|
| PFS-007-01 | Pectra and ZeroFee readiness | clean Pectra genesis | network finalizes first block | marker/schema/views are canonical | live Rust `zerofee.feature` |
| PFS-007-02 | install delegation | funded fresh EOA, chain id and nonce | submit native Alloy set-code authorization | exact `0xef0100 ++ ZeroFee` designator | live Rust |
| PFS-007-03 | consume sponsored quota | delegated EOA, count 0 | submit eight eligible calls | 8 successful receipts, zero fees, events and count 8 | live Rust |
| PFS-007-04 | quota exhaustion soft failure | delegated EOA, count 8 | submit ninth eligible call | mined status 0, code 110, no debit/increment | live Rust |
| PFS-007-05 | paid fallback remains available | delegated EOA, exhausted quota | submit tipped call | status 1, positive fee, count 8, no sponsorship event | live Rust |
| PFS-007-06 | CLI authorization | signer key and RPC chain id | run `zero-fee eip7702-authorize` | canonical target/chain/nonce/signature JSON | live Rust |
| PFS-007-07 | authorization replay | included raw delegation transaction and consumed nonce | resubmit the exact signed transaction before and after restart | rejected; delegation/quota/balance remain canonical on every validator | `@pfs-007-07` live-node |
| PFS-007-08 | restart with exhausted quota | finalized count 8 | restart one validator, then the full committee | delegation and quota remain identical; replay remains rejected; paid path works | `@pfs-007-08` live-node |
| PFS-007-09 | wrong-chain authorization | funded account and otherwise valid authorization | sign for a different chain id | no delegation or quota state is installed | `@pfs-007-09` live-node |
| PFS-007-10 | wrong delegation target | funded delegated account | install a non-ZeroFee target and send a sponsored-shaped call | no sponsorship; quota remains unchanged | `@pfs-007-10` live-node |
| PFS-007-11 | stale conflicting authorization | an existing wrong-target delegation | submit a stale conflicting authorization | prior delegation remains; quota remains unchanged | `@pfs-007-11` live-node |
| PFS-007-12 | worldwide-day lazy reset | exhausted quota immediately before the UTC day boundary | advance through the boundary and submit the first eligible call | quota resets lazily once and converges on every validator | `@pfs-007-12` live-node |

## Open questions and technical debt

- Port the six implemented shell phases into `crates/testing/e2e-harness` and add committee finality/state parity.
- Add a mise task and CI lane; the shell entrypoint is currently orphaned.
- Add authorization replay and restart persistence scenarios.
