# ADR-C-TOK-004: A governed Rust Factory owns permanent stablecoin identity and creation

- **Status:** Proposed; design approved, not implemented
- **Date:** 2026-07-27
- **Owners/scope:** proposed `crates/core/stablecoinfactory`; creation admission,
  address derivation, pending reservations and permanent token registry
- **Depends on:** ADR-B-EVM-002, ADR-B-EVM-003, ADR-B-EVM-004,
  ADR-C-TOK-003, ADR-C-TOK-005, ADR-S-GOV-002
- **Related:** ADR-S-GOV-003, ADR-S-FEE-001

## Context

Outbe needs many independently addressed Rust-native stablecoins without allowing
issuers to deploy arbitrary implementations. Address identity must be predictable,
collision-checked and permanent. Creation must be validator-admitted while allowing
a prospective issuer who is not yet a validator to submit its own proposal.

The Factory is an admission and identity registry, not a reserve verifier, price
oracle, fee-asset registry or endorsement service.

## Decision

### Factory-only creation through Vote

`STABLECOIN_FACTORY_ADDRESS` is a fixed stateful precompile and a compile-time
`VoteTargetRegistry` target. Direct public `create` is not exposed. The only creation
path is:

```text
issuer creates bonded Vote proposal
  -> Factory validates and reserves identity
  -> current ACTIVE validators vote
  -> Vote tally reaches existing 2/3 yes quorum
  -> Factory initializes token and permanent registry
  -> Vote marks Approved and refunds the bond
```

The proposal creator must equal the payload issuer. That address becomes token
issuer, initial sole `ADMIN`, bond owner and initial holder of every operational
role. Creation always starts with zero supply.

Stablecoin proposals use the existing production voting window and current ACTIVE
validator-set denominator. Proposal-specific quorum or deadline settings are not
accepted.

### Canonical proposal payload

The V1 payload is canonical UTF-8 JSON with exactly these keys and this order:

```json
{"version":1,"kind":"StablecoinCreate","issuer":"0x0000000000000000000000000000000000000000","name":"Example Dollar","ticker":"EXUSD","iso4217":840,"decimals":6,"supplyCap":"1000000000000","policyId":"1"}
```

Rules are consensus format. Vote passes the original payload bytes plus proposer and
attached-value context to the target; a generic `serde_json::Value` is not the
canonical-format authority. Factory parses into a typed value, re-encodes it and
requires byte equality:

- no whitespace, unknown fields, duplicate fields or alternate key order;
- address is `0x` plus 40 lowercase hexadecimal characters and must be nonzero;
- `version`, `iso4217` and `decimals` use canonical JSON integers;
- `supplyCap` and `policyId` use nonempty base-10 strings with no sign or leading
  zero except the single string `"0"`;
- name uses valid UTF-8 and shortest required JSON escaping; the decoder re-encodes
  and requires byte equality; and
- every field also passes ADR-C-TOK-003 validation.

`policyId` must already exist at proposal admission. Tooling may default to
`ALLOW_ALL` (`1`), but the canonical payload always carries the id and creation
never creates a policy implicitly. `supplyCap` must be nonzero.

### Bonded public target admission

Stablecoin Factory is the only V1 exception to Vote's default
`ActiveValidatorOnly` proposal creation rule. Its compile-time admission policy is
`PublicBonded` with this fixed hard-fork constant:

```text
STABLECOIN_PROPOSAL_BOND = 1,000,000 COEN
                         = 1,000,000 × 10^18 native base units
                         = 1000000000000000000000000
```

The proposal call must attach exactly that `U256` amount; all other Vote targets
reject nonzero value.

The EVM value transfer escrows the bond on `VOTE_ADDRESS`, and Vote records amount
and unsettled status in proposal state. The account balance may contain forced or
historical surplus, so the invariant is `balance >= unsettled bond liabilities`, not
equality. Only recorded liabilities may be settled.

Settlement is atomic with terminal outcome:

- successful Approved execution uses
  `transfer_balance(VOTE_ADDRESS, proposer, bond)`;
- Expired, quorum-failed and deterministic recoverable target rejection use the
  standard native `decrease_balance(VOTE_ADDRESS, bond)` burn primitive; and
- each settlement emits one typed refund or burn event and becomes replay-final.

A typed target outcome distinguishes recoverable domain rejection from execution
failure. OOG, unsupported storage capability, provider/storage corruption, impossible
schema or invariant failure propagate as fatal, roll finalization back to Pending and
do not burn the issuer's bond. Metadosis is not involved. A missing escrow liability
or failed refund/burn is likewise fatal rather than a partially settled proposal.

### Reservation and terminal hooks

At proposal creation, Factory atomically records both
`pendingTokenId[tokenId] = proposalId` and `pendingTicker[ticker] = proposalId` after
validating that:

- no permanent token exists for the full id;
- no permanent token created by any issuer already owns the ticker;
- no pending proposal from any issuer reserves the id or ticker;
- the predicted address is not assigned to another full token id; and
- the referenced policy exists.

Only one pending proposal globally may reserve a ticker. The reservation is
consumed by successful creation and released on every Expired or Rejected outcome.
Vote allocates the proposal id and calls target-specific reserve/terminal hooks in
the same transaction; target runtime registration is forbidden.

Approved Factory execution runs under a nested checkpoint. On typed recoverable
rejection, all partial token code/storage/registry writes roll back before Vote
records Rejected, releases the reservation and burns the bond. Fatal execution
failure rolls back the containing pre-execution hook batch, retaining Pending,
reservation and bond liability for deterministic retry. Cleanup or settlement
failure is also fatal.

### Deterministic identity and dynamic address class

The full identity is:

```text
tokenId = keccak256(
    "OUTBE_STABLECOIN_V1" ||
    chainId_be ||
    STABLECOIN_FACTORY_ADDRESS ||
    issuer ||
    ticker_length || ticker_bytes
)
```

The EVM address uses a protocol-reserved two-byte prefix followed by the rightmost
144 bits of `tokenId`. The full 256-bit id remains in Factory state. A hash-tail
collision with a different full id is rejected deterministically; it never aliases
or overwrites an existing token.

The exact prefix and Factory address are selected only after a repository/genesis,
Ethereum-precompile, system-address and reserved-range collision scan. The complete
class is reserved from genesis, before any user transaction: contract creation into
the class is rejected and calls to an unregistered member fail closed. The class
cannot be activated late over arbitrary pre-existing code, nonce or storage. Native
COEN may still be forced to a future address and is treated as unrelated surplus,
not token state or backing.

Creation atomically initializes ADR-C-TOK-003 storage, registers both identity
indexes and writes exact nonempty marker bytecode through `StorageHandle::set_code`.
Because Vote finalization currently runs in the atomic pre-execution hook batch, its
provider must gain journaled code mutation and include code changes in the state-root
notification; moving this flow to a begin-zone EVM transaction would require a
separate ordering decision. The Factory address is added to the `HookEvents` receipt
whitelist so `StablecoinCreated` is receipt-visible.

Installed nonempty marker code itself prevents EIP-161 pruning. Prefix membership
alone is never proof of a stablecoin: token dispatch also requires Factory
registration, matching full id/schema and exact marker code. Failed initialization
rolls marker/storage/index changes back together.

### Permanent bounded registry

Factory is the sole writer of:

- monotonic `tokenCount` and `tokenAt(index)`;
- `tokenById(tokenId)`;
- global `tokenByTicker(ticker)`;
- reverse `tokenIdOf(token)`; and
- pending token-id and global-ticker proposal reservations.

Registration cannot be deleted or replaced. Operational shutdown uses token pause
and/or `DENY_ALL`; balances and history remain queryable. Runtime code never calls
an unbounded `readAll`; callers iterate one index or use an explicitly capped page.

Ticker uniqueness is global across the Factory: once a permanent token or pending
proposal owns a ticker, every other issuer is rejected for that exact canonical
ticker. Multiple tokens may still reference the same ISO currency code.

## Authoritative interface

The canonical ABI lives in
`contracts/precompiles/src/IStablecoinFactory.sol` and exposes only views:

```solidity
function tokenCount() external view returns (uint256);
function tokenAt(uint256 index) external view returns (address);
function tokenById(bytes32 tokenId) external view returns (address);
function tokenByTicker(string calldata ticker) external view returns (address);
function tokenIdOf(address token) external view returns (bytes32);
function isStablecoin(address token) external view returns (bool);
function predictTokenAddress(address issuer, string calldata ticker)
    external view returns (bytes32 tokenId, address token);
```

Successful creation emits one non-anonymous `StablecoinCreated` with indexed
`tokenId`, token and issuer, plus non-indexed proposal id, immutable metadata, cap and
policy id. Solidity permits at most three indexed parameters on a non-anonymous event;
keeping the signature topic is preferred over an anonymous four-index event. The ABI
and README must state that this is protocol admission only, not proof of backing,
redeemability, price stability, creditworthiness or fee eligibility.

Factory-internal `validateProposal`, `reserveProposal`, `executeApproved` and
`releaseTerminal` are typed Rust APIs available only to the compile-time Vote target
adapter. They are not EVM selectors.

## Invariants

- Factory is the only token initializer and registry writer.
- A token id, dynamic address and canonical ticker each resolve to at most one
  permanent token globally.
- Every permanent forward index agrees with every reverse index and marker/schema.
- Every pending id names exactly one Pending Vote proposal and is absent from the
  permanent indexes.
- `VOTE_ADDRESS` native balance is at least the sum of unsettled bond liabilities;
  surplus is never refundable as a proposal bond.
- Successful creation consumes exactly one reservation; every non-success terminal
  outcome releases it.
- A token is never visible as registered before complete initialization commits.
- Factory approval does not grant fee or payment-lane eligibility.

## Determinism, bounds and replay

JSON byte validation, ISO table, ticker validation, address derivation and index
writes are protocol-versioned deterministic code. Proposal size remains bounded by
Vote; name and ticker bounds cap parsing/allocation. One proposal performs O(1)
Factory and token initialization work. Duplicate proposal, execution or terminal
hooks are rejected or idempotently observe their already-terminal state without
settling value twice.

## Consequences

Wallets and protocols get predictable ERC-20 addresses and an on-chain discovery
registry. Issuers cannot select code, bypass Vote or overwrite identity. The two-byte
namespace gives 144 hash bits while enabling cheap dispatch, but permanently
reserves that address class and requires a collision scan before activation.

## Rejected alternatives

- Permissionless direct Factory creation was rejected in favor of validator protocol
  admission and a spam bond.
- Validator-only proposal creation was rejected because the prospective issuer must
  bind its own identity and bond.
- Initial minting, automatic policy creation, issuer-scoped ticker uniqueness,
  deletable registrations and implicit fee eligibility were rejected.
- Event-only discovery was rejected because runtime modules need a canonical registry.
- A 12-byte prefix was rejected because leaving only 64 hash bits is unnecessarily
  restrictive; no-prefix routing was rejected because current native dispatch
  requires a reserved class.

## Open questions and technical debt

- Choose the exact `STABLECOIN_FACTORY_ADDRESS`, two-byte prefix, marker bytecode and
  activation version after the mandatory collision scan.
- Before activation, verify that the fixed 1,000,000 COEN bond is representable and
  transferred/burned as exactly `10^24` native base units in ABI and balance vectors.
- Define and benchmark the Factory page cap and creation gas schedule.
- ADR-S-GOV-002 must add raw-payload compile-time target
  admission/reservation/terminal hooks, typed recoverable-versus-fatal outcomes,
  nested handler rollback and bond state before this design can activate.
- ADR-B-EVM-002 must pass the actual callee address into class dispatch, reserve the
  class from genesis and generate exact-address plus reserved-class conformance from
  one manifest.
- `DirectStorageProvider` currently rejects `set_code`; add journaled code mutation,
  state-root notification and checked balance credit, then publish Factory hook logs
  through the mandatory `HookEvents` receipt.
- Fee eligibility, payment-lane classification and reserve attestations require
  independent registries and ADRs; Factory approval must not be reused as a shortcut.
