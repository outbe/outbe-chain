# PFS-010: Governed stablecoin creation and durable ledger lifecycle

- **Status:** Draft
- **Actors:** stablecoin issuer, second issuer, policy administrator, token users,
  active validator voters, StablecoinPolicy, StablecoinFactory, Vote and the
  per-token Stablecoin ledger
- **Trigger:** a funded issuer submits a bonded StablecoinFactory proposal that
  references an existing shared policy
- **Topology/services:** fresh-genesis validator network with the Factory, Policy
  Registry and stablecoin address class active from block 0; no off-chain service is
  authoritative
- **Referenced ADRs:** ADR-B-CNS-003, ADR-B-EVM-002 through ADR-B-EVM-005,
  ADR-B-GEN-001, ADR-B-RPC-001, ADR-B-TST-001, ADR-S-GOV-002, ADR-S-VAL-001,
  ADR-C-TOK-003 through ADR-C-TOK-005
- **Supersedes:** None

## Outcome

A policy-bound stablecoin receives one permanent deterministic identity after
validator approval, refunds its exact creation bond, executes the issuer-controlled
ledger contract, rejects conflicting global identity without allocation or debit,
and preserves finalized registry and ledger reads across a full committee restart.

## Acceptance contract

- **Source:** The issuer creates or selects a shared policy and submits the canonical
  StablecoinFactory payload through the production operator CLI; active validators
  originate their own ballots.
- **Trigger:** The funded issuer submits the exact creation bond with a proposal whose
  canonical name, ticker, ISO-4217 currency, supply cap, policy and predicted identity
  pass Factory admission.
- **Environment:** A finalizing network of at least four validators running one
  binary and an identical Factory/Vote handler registry. Factory, Policy and the
  stablecoin reserved address class are active in fresh genesis.
- **Canonical inputs:** Chain id, Factory address, issuer, canonical ticker and name,
  reference currency, supply cap, policy id, compiled voting window, current active
  validator set and fixed creation bond.
- **System under test:** StablecoinPolicy admission and membership, Factory identity
  reservation/creation/registry, Vote admission/tally/bond settlement, EVM dynamic
  token routing, Stablecoin ledger authorization and finalized RPC history.
- **Expected response:** A terminal approved proposal, one created marker account,
  one permanent token id/ticker/address registration, one receipt-visible
  `StablecoinCreated` event, exact bond refund, policy-bound ledger effects and
  byte-identical current/historical observations on every validator after restart.
- **Response measures:** `tokenCount` increases exactly once; token-id and ticker
  lookups return the predicted address; code is the exact native marker; the bond is
  `Refunded` and unsettled liability is zero; supply, balances, allowance, nonce,
  frozen amount and pause state equal the committed operations on every RPC.
- **Failure guarantee:** Rejected admission allocates no proposal, reservation,
  liability, marker, registry entry or event and debits no bond. A reverted ledger
  command commits no partial balance, allowance, supply, role, policy or event
  change. Restart never recreates or mutates an already finalized token.

## Preconditions and canonical inputs

- Stablecoin V1, Factory and Policy Registry are available from fresh genesis with
  the reserved address-class guard and marker namespaces active from block 0.
- Every validator exposes the same Factory admission/tally handler and protocol
  constants. At least three of four active validators can vote before the deadline.
- The issuer has enough native balance for the fixed bond and transaction fee.
- The referenced permanent policy exists, has the expected type and administrator,
  and authorizes every account used by the successful ledger path.
- `(chain_id, factory, issuer, ticker)` deterministically predicts the token id and
  address. The token id, canonical ticker and address are all unreserved and
  unregistered before admission.
- Metadata, ISO currency, cap and policy fields satisfy ADR-C-TOK-003 through
  ADR-C-TOK-005. No reserve, fee-asset or issuer-endorsement claim is inferred.

## Success sequence

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | issuer/StablecoinPolicy | create a permanent shared policy and add the authorized accounts | policy record, member index and ABI receipts |
| 2 | operator CLI/issuer | derive the expected identity and submit the canonical Factory proposal with the exact bond | successful proposal receipt, pending Vote record, reservation triple and bond liability |
| 3 | active validators/Vote | cast independent ballots before the compiled deadline | one accepted ballot per validator and unchanged canonical payload |
| 4 | Vote begin-block | tally against the current active set after the deadline | terminal `Approved` proposal |
| 5 | StablecoinFactory target | under one checkpoint, authenticate reservations, install the marker, initialize token state and publish permanent indexes | marker code, schema/identity state, token id/ticker/list entries and one `StablecoinCreated` log |
| 6 | Vote/Factory settlement | clear pending reservations and return the exact bond | `Refunded` settlement, zero unsettled liability and issuer balance net of transaction fees only |
| 7 | issuer and users/Stablecoin | mint, transfer, permit, transfer with memo, pause/unpause, freeze and forced-transfer through the token address | successful/reverted receipts, events, supply, balances, nonce, allowance, frozen and pause reads |
| 8 | second issuer/Factory | attempt the already registered global ticker | admission rejection with no proposal id, bond debit, liability or registry change |
| 9 | validator operators | record a finalized observation, restart the full committee with the same binary and datadirs, and resume finalization | recovered height plus identical historical/current Factory, marker and ledger reads on every RPC |

## Boundaries and conservation

Policy creation/member mutation, proposal admission and every ballot are separate
user transactions. Tally, target execution, marker installation, registry mutation,
event publication and bond settlement execute in the bounded begin-block system path.
Factory target effects share a nested checkpoint: any target error restores code,
storage, balances, events and registry state before Vote records its terminal
proposal outcome. Each ledger call is a separate atomic EVM transaction. Historical
evidence is sampled only at a finalized height before process restart.

```text
one approved proposal             -> exactly one permanent token identity
token id + canonical ticker + addr -> one registered token address
approved bond settlement          -> issuer bond returned exactly once
unsettled bond liabilities        -> zero after successful creation
minted supply                     -> committed holder balances plus any defined burn
rejected admission                -> zero proposal/reservation/liability/registry delta
```

The harness flow mints all observed supply to the issuer and only moves it among the
issuer and recipient. Its final supply therefore equals their final balances; the
spender receives allowance but no token balance.

## Observable completion contract

The proposal and three validator ballot receipts succeed. After the deadline the
proposal reads `Approved`; its bond amount equals the compiled Stablecoin creation
bond and settlement reads `Refunded`. The issuer's post-approval native balance plus
the proposal transaction fee equals its pre-proposal balance, and
`unsettledBondLiabilities()` is zero.

Exactly one Factory `StablecoinCreated` log appears in the successful HookEvents
receipt. `tokenCount()`, `tokenById()` and `tokenByTicker()` agree on the predicted
address on every validator, and `eth_getCode` returns the exact native token marker.
Token supply, balances, allowance, nonce, frozen amount and pause state match the
operation sequence on every RPC. A paused transfer demonstrably reverts.

After the committee restart, exact-block `eth_call` at the saved finalized height
and current reads return the saved state on every validator. Factory lookup and
marker code remain unchanged. Canonical EVM state is authoritative; no off-chain
projection participates in this flow.

## Replay, retry, restart and failure

- Repeating a registered global ticker fails during Factory admission before Vote
  allocation or bond debit. Permanent token identity and `tokenCount` remain
  unchanged.
- Duplicate ballots, insufficient quorum and post-deadline voting follow
  ADR-S-GOV-002. Expiry must release the reservation triple and burn the bond exactly
  once.
- A deterministic target execution error leaves the proposal in `Error` with its
  bond liability, reservation triple and pending-cap occupancy retained. It does
  not invalidate the containing block. Retry, close or any other later handling of
  an `Error` proposal belongs to Vote/governance and is outside Stablecoin Factory
  V1 and this flow.
- Fatal provider/schema/marker/reservation inconsistency cannot be translated into a
  proposal rejection. The block checkpoint rolls back and consensus must not commit
  divergent Factory state.
- Reverted ledger authorization, cap, pause, freeze, policy or signature checks
  restore all state and events. Successful grant/revoke no-ops retain their
  ADR-defined idempotent behavior.
- Full committee restart uses the same binary and durable datadirs. It does not model
  snapshot import, a protocol upgrade, mixed binaries or a new validator joining
  from empty state.

## E2E scenario matrix

| Id | Scenario | Minimum topology | Required assertions | Automated by |
|---|---|---|---|---|
| PFS-010-01 | policy, bonded approval and permanent creation | fresh genesis, 4 active validators | policy exists; three yes votes approve; predicted id/ticker/address and marker agree on every RPC; one HookEvents creation log; exact bond refund and zero liability | `@pfs-010-01` in `stablecoin_factory_v1.feature` |
| PFS-010-02 | complete issuer ledger exercise | created token and authorized issuer/recipient/spender | mint/transfer/permit/memo/pause rejection/unpause/freeze/forced transfer produce exact supply, balances, allowance, nonce, frozen and pause state on every RPC | `@pfs-010-02` in `stablecoin_factory_v1.feature` |
| PFS-010-03 | duplicate global ticker admission | registered ticker and funded second issuer | RPC preflight rejects; no second proposal, bond debit, liability, token or `tokenCount` change | `@pfs-010-03` in `stablecoin_factory_v1.feature` |
| PFS-010-04 | full committee restart parity | finalized created token and all original datadirs | all validators recover the saved height; exact historical/current ledger reads, marker and Factory lookup remain identical | `@pfs-010-04` in `stablecoin_factory_v1.feature` |
| PFS-010-05 | insufficient quorum expiry | pending bonded proposal with fewer than threshold yes votes | `Expired`; reservation triple released; bond burned once; pending cap restored; same identity can be proposed again; no token/event | `handlers::vote::tests::pfs_010_05_expiry_releases_identity_and_pending_cap_and_burns_once` |
| PFS-010-06 | target execution error retention | approved proposal with deterministic target failure | block commits; proposal is `Error`; bond, reservation triple and pending cap remain occupied; no partial token or settlement event | `handlers::vote::tests::pfs_010_06_execution_error_retains_reservation_bond_and_pending_cap` |
| PFS-010-07 | policy, role, signature and cap failures | created token with unauthorized/nonmember callers and boundary values | each call reverts with exact pre-state/event restoration; valid boundary calls still commit | `tests::pfs_010_07_*` in `outbe-stablecoin` |
| PFS-010-08 | fatal creation rollback | injected reservation inconsistency and provider failure at every initializer mutation | fatal escapes target execution; outer block checkpoint keeps proposal/bond/Factory at parent state; no partial marker, token storage, registry or event | `handlers::vote::tests::pfs_010_08_fatal_creation_rolls_back_the_containing_block`; `tests::pfs_010_08_every_initializer_failure_rolls_back_token_marker_registry_and_event` in `outbe-stablecoinfactory` |

## Open questions and technical debt

- Accept this reconstructed flow contract and decide which failure rows are mandatory
  before promoting PFS-010 from Draft.
- PFS-010-05 through -08 are deterministic in-process fault/rollback scenarios.
  Promote any of them to a live-node scenario only if that boundary adds a distinct
  observable guarantee; the current four-validator product feature must not be
  relabeled as their evidence.
- Add machine-readable verification-ledger entries and a required CI lane for the
  focused four-validator feature; a documented local command alone is not release
  evidence under ADR-B-TST-001.
- Add snapshot-import/new-node and mixed-binary coverage only through their owning
  PFS/ADR; the existing same-binary restart must not be relabeled as either.
