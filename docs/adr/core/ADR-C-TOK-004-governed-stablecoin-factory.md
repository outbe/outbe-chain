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

`STABLECOIN_FACTORY_ADDRESS` is the fixed stateful precompile
`0x000000000000000000000000000000000000EE0F` and a compile-time
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
- `iso4217` must appear in the protocol-pinned SIX List One snapshot published
  2026-01-01; updates require a hard-fork change to the pinned set and vectors;
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
- Expired/quorum-failed proposals use the standard native
  `decrease_balance(VOTE_ADDRESS, bond)` burn primitive;
- target execution failure records proposal status `Error` without settling the
  bond; and
- each settlement emits one typed refund or burn event and becomes replay-final.

Retrying, cancelling or settling an `Error` proposal requires a separate
validator-approved governance transition and is outside Stablecoin Factory V1.
Metadosis is not involved.

### Reservation and terminal hooks

At proposal creation, Factory atomically records:

```text
pendingTokenId[tokenId] = proposalId
pendingTicker[ticker] = proposalId
pendingAddress[predictedAddress] = proposalId
```

The reservation commits only after validating that:

- no permanent token exists for the full id;
- no permanent token created by any issuer already owns the ticker;
- no pending proposal from any issuer reserves the id or ticker;
- the predicted address is not assigned to another full token id; and
- the referenced policy exists.

Only one pending proposal globally may reserve a ticker, token id or predicted
address. All three reservations are consumed by successful creation and released
on `Expired`. Vote allocates the proposal id and calls target-specific
reserve/terminal hooks in the same transaction; target runtime registration is
forbidden.

Approved Factory execution runs under a nested checkpoint. On target execution
error, all partial token code/storage/registry writes roll back before Vote records
`Error`. The bond liability and all three reservations remain unchanged. Stablecoin
Factory V1 defines no automatic retry or `Error` cleanup.

### Deterministic identity and dynamic address class

The full identity is:

```text
tokenId = keccak256(
    "OUTBE_STABLECOIN_V1" ||
    chainId_u64_be ||
    STABLECOIN_FACTORY_ADDRESS ||
    issuer ||
    ticker_length_u8 || ticker_bytes
)
```

`chainId_u64_be` is exactly eight unsigned big-endian bytes, matching Outbe's
canonical `u64` chain-id type. `ticker_length_u8` is exactly one byte; the V1 ticker
bound is 2 through 12 bytes, so wider or variable-length encodings are noncanonical.
SCF-002 identity vectors parameterize the Factory address and prefix; SCF-003 selects
the network constants and regenerates the final network vectors without changing the
preimage codec.

The EVM address uses a protocol-reserved two-byte prefix followed by the rightmost
144 bits of `tokenId`. The full 256-bit id remains in Factory state. A hash-tail
collision with a different full id is rejected deterministically; it never aliases
or overwrites an existing token.

The reserved class prefix is exactly `0x53c0`, so token addresses are
`0x53c0 || tokenId[14..32]`. The exact marker bytecode is `0xef`. Repository-declared
addresses, Ethereum built-ins, every `scripts/*seed*.json` contract predeploy, tracked
`genesis.json`, explicit generated genesis and every class in the machine-owned
planned-range registry are collision-scanned by `xtask stablecoin namespace-check`.
The class is reserved from genesis, before
any user transaction: contract creation into
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

- monotonic token-address list and count used by `listTokens`;
- `tokenById(tokenId)`;
- global `tokenByTicker(ticker)`;
- reverse `tokenIdOf(token)`; and
- pending token-id, global-ticker and predicted-address proposal reservations.

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
function listTokens(uint256 offset, uint256 limit)
    external view returns (address[] memory);
function tokenById(bytes32 tokenId) external view returns (address);
function tokenByTicker(string calldata ticker) external view returns (address);
function tokenIdOf(address token) external view returns (bytes32);
function isStablecoin(address token) external view returns (bool);
function predictTokenAddress(address issuer, string calldata ticker)
    external view returns (bytes32 tokenId, address token);
```

`listTokens` accepts `1 <= limit <= 100`. It exposes the current registry list with
no sorting or ordering guarantee and no cursor/filter semantics.

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
- Every pending token id, ticker and predicted address names exactly one Pending or
  Error Vote proposal and is absent from the permanent indexes.
- `VOTE_ADDRESS` native balance is at least the sum of unsettled bond liabilities;
  surplus is never refundable as a proposal bond.
- Successful creation consumes exactly one matching reservation triple; Expired
  releases it; Error retains it.
- A token is never visible as registered before complete initialization commits.
- Factory approval does not grant fee or payment-lane eligibility.

## Determinism, bounds and replay

JSON byte validation, the SIX List One 2026-01-01 ISO table snapshot, ticker
validation, address derivation and index writes are protocol-versioned deterministic
code. Proposal size remains bounded by
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

### Genesis and activation classification

Stablecoin V1 may activate on devnet/testnet only after a coordinated destructive
pre-production reset that regenerates one identical genesis containing the Factory
and Policy marker accounts. A restart that preserves old chain state is insufficient:
the class must have been protected from CREATE/CREATE2 since block 1. Existing state
that executed without the class guard is unsupported. Mainnet remains unsupported
until its chain id, fresh genesis and activation manifest are separately frozen.

## Protocol lock and technical debt

Stablecoin Factory V1 activates at protocol version `0.2` (raw `2`). Namespace
reservation remains genesis-active independently of that runtime predicate. The
public bonded sub-cap is 16 of Vote's 64 total pending slots, with one pending public
bonded proposal per proposer. The bond is exactly
`1,000,000,000,000,000,000,000,000` base units (`10^24`). Invalid admission commits
no reservation, liability or log. An `Error` proposal retains its bond liability,
reservation triple and pending-cap occupancy until a future validator-approved
governance transition resolves it.

Factory V1 exposes `tokenCount()` and `listTokens(offset, limit)` with caller-selected
`1 <= limit <= 100`. The list has no sorting or ordering guarantee. Factory creation
runs in Vote's bounded begin-block path; V1 defines no separate Factory creation gas
ceiling or gas-benchmark activation gate.

- ADR-S-GOV-002 must add raw-payload compile-time target
  admission/reservation/terminal hooks, nested handler rollback, `Error` status and
  bond state before this design can activate.
- ADR-B-EVM-002 first consolidates the 35 current exact routes behind one compact
  declaration containing only dispatch adapter and base gas. The class-owning step
  then adds exact-first resolution, passes the actual callee into class dispatch and
  reserves the class from genesis. The static route table is not protocol-version,
  persistence, warming or authentication authority; SCF-025 supplies exact-state
  activation and Factory full-id/schema/marker checks authenticate each instance.
- `DirectStorageProvider` currently rejects `set_code`; add journaled code mutation,
  state-root notification and checked balance credit, then publish Factory hook logs
  through the mandatory `HookEvents` receipt.
- Fee eligibility, payment-lane classification and reserve attestations require
  independent registries and ADRs; Factory approval must not be reused as a shortcut.
