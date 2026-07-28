# ADR-C-TOK-003: Rust-native stablecoins share one hard-fork-governed issuer ledger

- **Status:** Implemented
- **Date:** 2026-07-27
- **Owners/scope:** `crates/core/stablecoin`; per-token balances, allowances,
  supply, roles, pause, freeze, policy binding and signatures
- **Depends on:** ADR-B-EVM-002, ADR-B-EVM-003, ADR-B-EVM-004,
  ADR-B-EVM-005, ADR-C-TOK-004, ADR-C-TOK-005
- **Related:** ADR-C-TOK-001, ADR-C-TOK-002, PFS-010

## Context

Outbe needs separately addressed, ERC-20-compatible stablecoins without deploying
mutable Solidity implementations or token-level proxies. Each token represents an
issuer-managed ledger denominated by one ISO-4217 reference currency. The ledger
must expose mature compliance and enforcement controls while preserving the
single-binary, deterministic Rust execution model.

Factory admission is not proof of backing, redeemability, price stability, issuer
creditworthiness or protocol endorsement. Fee assets, payment lanes, reserve
proofs, ERC-3009 authorization and ERC-7802 bridging are separate future decisions.

## Decision

### One dynamic precompile implementation, one address-scoped ledger

Every registered stablecoin address is a stateful dynamic native precompile instance,
not deployed Solidity bytecode. The reserved address-class dispatcher passes the
actual callee address to one compiled Rust precompile implementation; that address
selects the token's isolated storage account. The exact marker code is the one-byte legacy sequence `0xef`. It exists for EVM
introspection and EIP-161 preservation, but execution is handled by the Rust
precompile rather than by that bytecode.

The Outbe binary determines behavior for every stablecoin; there are no
per-token implementation pointers, proxy admins, issuer-selected templates or
coexisting runtime versions.

Each token stores `schemaVersion` and `creationProtocolVersion`. The genesis binary
creates V1 tokens with `creationProtocolVersion = 0`. Token-local state never selects
the runtime version.

Stablecoin V1 reads and writes schema version 1 only. Any other schema version fails
closed without mutation. When a real V2 schema exists, its ADR must define the exact
V1-to-V2 compatibility and migration behavior; V1 contains no test-only second schema
or speculative migration framework. Retired selectors revert explicitly rather than
succeeding as no-ops.

### Immutable identity and mutable supply ceiling

Creation initializes:

- immutable `name`: valid UTF-8, 1 through 64 bytes, with no control characters;
- immutable `symbol`/ticker: 2 through 12 ASCII characters, first `A-Z`, remaining
  `A-Z0-9`, without normalization;
- immutable numeric code present in SIX ISO 4217 List One published 2026-01-01
  (source XML SHA-256
  `838dfb991648cf36df939edd5fe3811737962b75a32252847d239cedd1e291c9`);
- immutable `decimals` in `0..=18` (CLI default 6 only);
- immutable issuer identity and Factory `tokenId`;
- explicit nonzero `U256 supplyCap`, in smallest units;
- explicit existing Policy Registry `policyId`; and
- zero initial supply.

`CAP_MANAGER` may set a new cap when `newCap >= totalSupply`; zero therefore means
an issuance shutdown when supply is zero, never unlimited. `U256::MAX` is the
explicit effectively-unlimited value.

No logo, metadata URI, reserve claim or mutable presentation field belongs to the
ledger.

### Fixed authority topology

The fixed roles are `ADMIN`, `ISSUER`, `CAP_MANAGER`, `GUARDIAN`, `COMPLIANCE` and
`ENFORCER`. Their canonical `bytes32` ids are respectively `keccak256("ADMIN")`,
`keccak256("ISSUER")`, `keccak256("CAP_MANAGER")`, `keccak256("GUARDIAN")`,
`keccak256("COMPLIANCE")` and `keccak256("ENFORCER")`; the checked-in ABI vectors pin
the resulting bytes. The creation issuer is the sole initial `ADMIN` and initially
holds all five operational roles. `ADMIN` may grant or revoke operational role
memberships; `grantRole` and `revokeRole` reject the `ADMIN` id with
`UnsupportedRole`. ADMIN changes exclusively through the two-step transfer, so those
role selectors cannot create multiple admins or bypass acceptance.

Admin replacement is two-step:

```text
ADMIN --beginAdminTransfer(nonzero candidate)--> PendingAdmin
PendingAdmin --acceptAdminTransfer()-----------> ADMIN
ADMIN --cancelAdminTransfer()------------------> no pending candidate
```

V1 has no Vote-level recovery, recovery role or validator replacement of a lost
admin. Issuers are expected to use a multisig or recoverable smart account.

`GUARDIAN` may pause; only `ADMIN` may unpause. Pause blocks transfer, mint, burn and
forced transfer, including memo variants. It does not block allowance reduction,
role/admin recovery, cap changes, policy rebinding, or freeze management.

Repeating `grantRole(role, account)` when the account already has the operational
role, or `revokeRole(role, account)` when it does not, succeeds as an idempotent
no-op. It changes no state and emits no role event.

### Ledger, compliance and allowance behavior

The ledger implements ERC-20, EIP-2612 and ERC-165. EIP-2612 binds chain id, token
address, owner, spender, value, nonce and deadline through the standard domain
separator; nonces advance only on successful permit.

`approve` and `permit` remain available during pause. If the allowance owner has a
nonzero frozen amount, the new allowance may only be less than or equal to the
current allowance, including zero. Public zero-value `transfer` and `transferFrom`
remain valid and emit canonical events. Zero-value mint, burn, burnFrom and forced
transfer are rejected.

The active shared policy is the sole account-policy source of truth:

- transfer and transferFrom require allowed sender, allowed recipient and enough
  unfrozen balance;
- mint requires an allowed recipient;
- allowance-backed burnFrom requires allowed sender and enough unfrozen balance;
- issuer burn of the issuer's own redemption balance may bypass sender policy and
  frozen amount; and
- a policy denial reverts with no balance, supply, allowance, nonce, freeze or log
  effects.

`COMPLIANCE` may bind only an existing policy. There is no embedded whitelist,
legacy fallback or implicit policy creation. The `PolicyDenied` ABI error uses the
stable `uint8` operation encoding `Send = 0`, `Receive = 1`, `Mint = 2`; ordinary burn
uses Send, recipient checks use Receive and mint uses Mint.

### ERC-7943 enforcement

The token implements the
[Final ERC-7943](https://eips.ethereum.org/EIPS/eip-7943)
`IERC7943Fungible` surface and reports its ERC-165 interface id (`0x3edbb4c4`):
`canSend`, `canReceive`, `canTransfer`,
`getFrozenTokens`, `setFrozenTokens` and `forcedTransfer`.

The public Token ABI uses the ERC-7943 errors
`ERC7943CannotSend`, `ERC7943CannotReceive`, `ERC7943CannotTransfer` and
`ERC7943InsufficientUnfrozenBalance`. Internal Policy Registry denial is mapped to
the corresponding ERC-7943 error at the token boundary.

`setFrozenTokens` overwrites an absolute frozen amount and may set a value above the
current balance. `canSend`, `canReceive` and `canTransfer` never revert and never
mutate state. `canTransfer` applies pause, policy and unfrozen-amount checks, but not
ordinary balance or allowance sufficiency beyond the ERC-7943 frozen check.

`forcedTransfer` requires `ENFORCER`, is blocked by pause, rejects `from == to`,
bypasses source policy/source freeze and still requires an allowed nonzero recipient.
For pre-state balance `B`, frozen amount `F` and amount `A <= B`, define:

```text
U = B - min(B, F)          // ordinarily unfrozen
C = max(0, A - U)          // frozen amount consumed
F' = F - C
```

If `C > 0`, the runtime stores `F'` and emits `Frozen(from, F')` before canonical
`Transfer`; it then emits `ForcedTransfer`. Excess freeze above the old balance is
therefore reduced only by frozen units actually removed and may continue freezing
future inflows. Ordinary ERC-20 self-transfer remains valid, changes no balance or
freeze and still passes normal policy/unfrozen checks. Forced self-transfer is
rejected. Forced transfer preserves total supply. The same formula/event ordering
applies when privileged issuer burn consumes frozen issuer funds.

There is no separate force-burn entrypoint.

### Memo extension

The runtime adds opaque `bytes32` memo variants for transfer, transferFrom, mint,
burn, burnFrom and forcedTransfer. A successful memo operation
emits the same canonical ERC-20/ERC-7943 events as its non-memo form plus:

```solidity
event TransferWithMemo(
    address indexed from,
    address indexed to,
    uint256 amount,
    bytes32 indexed memo
);
```

Mint uses `from = address(0)` and burn uses `to = address(0)`. The memo is event-only:
it is not interpreted, stored or used for authorization.

## Authoritative interface

The canonical ABI lives in `contracts/precompiles/src/IStablecoin.sol`. It includes:

- ERC-20 metadata, balances, allowances, approve, transfer and transferFrom;
- EIP-2612 `nonces`, `DOMAIN_SEPARATOR` and `permit`;
- `currency`, `supplyCap`, `policyId`, `issuer`, `paused`, `hasRole`,
  `pendingAdmin` and `creationProtocolVersion` views;
- the six ERC-7943 methods above and `supportsInterface`;
- `mint`, `burn`, `burnFrom` and their memo variants;
- transfer and transferFrom memo variants;
- `forcedTransferWithMemo`;
- operational role grant/revoke, cap change, policy change, pause/unpause,
  freeze management and two-step admin transfer.

Every mutating function rejects unexpected native value before state access.
Creation is not exposed by this ABI; only ADR-C-TOK-004 may initialize a ledger.

## State and invariants

Each dynamic token account owns its own schema-versioned mappings and values:
metadata, total supply, cap, balances, allowances, permit nonces, role membership,
admin transfer, pause, frozen amounts and policy id.

For every committed transition:

- `sum(balances) == totalSupply` conceptually; mint and burn update both atomically;
- `totalSupply <= supplyCap` after cap-changing and minting transitions;
- no unauthorized role, admin, pause, freeze or policy transition commits;
- a failed call preserves balances, allowances, nonces, supply, freeze and logs;
- transferFrom/burnFrom consume allowance exactly once, except standard infinite
  allowance remains unchanged; and
- all arithmetic is checked `U256` arithmetic.

No production path materializes all holders, allowances, role members or frozen
accounts.

## Atomicity, replay and failure

The enclosing revm frame journal is the sole transaction atomicity boundary. Each
operation validates before mutation and emits logs only after the transition plan
is known to succeed. Permit replay is rejected by nonce/deadline/signature checks.
Admin acceptance is restricted to the current pending candidate. Creation replay is
owned by ADR-C-TOK-004.

Domain failures are typed ABI reverts. Unknown or impossible schema versions fail
closed.

## Consequences

Applications receive a separate, standard address per stablecoin while validators
execute one deterministic implementation. The authority surface is explicit and
compliance is shared without duplicating mutable policy state in every token.
Global hard-fork semantics simplify runtime selection but require disciplined
compatibility vectors. Stablecoin V1 implements schema version 1 only. A future
schema upgrade defines its concrete migration in a separate ADR rather than adding
speculative V1 migration code.

## Rejected alternatives

- Solidity deployments and per-token proxies were rejected because implementation
  replacement would become issuer-controlled consensus behavior.
- One token contract with token ids was rejected because it is not ordinary ERC-20
  address compatibility.
- Per-token major templates were rejected in favor of one global hard-fork-governed
  runtime.
- Embedded token-local allowlists were rejected because they duplicate policy state.
- Implicit zero-as-unlimited caps, initial minting, mutable metadata and protocol
  admin recovery were rejected as ambiguous or over-privileged.

## Protocol lock and implementation evidence

Stablecoin V1 writes `schemaVersion = 1` and is active from fresh genesis at chain
protocol version `0`. The EIP-712
domain version is the independent string `"1"`; it is not inferred from the schema
or chain protocol version. V1 tooling is owned by `bin/outbe-cli`; generated ABI
JSON is an integration artifact, not a maintained SDK promise.

The initial native gas contract is dispatch `200`, each persistent read `100`, each
persistent write `5,000`, no refund or additional native cold surcharge, ecrecover
`3,000`, and explicit dynamic hashing `30 + 6 * ceil(bytes/32)`. Normal EVM
CALL/account access remains revm-owned. Identical fixed/class work has identical
Outbe-native charge on first and repeated calls; the dynamic class is never enumerated
as warm. `outbe_primitives::stablecoin_fork` is the canonical source for these values;
`fork-manifest.json` is a machine-readable mirror whose full gas/budget parity is
tested. The margin rule is `ceil_to_10_000(ceil(measured * 125 / 100))`. SCF-047
reopens this protocol lock if a measured token path cannot fit its corresponding
ceiling.

- `xtask stablecoin abi-check` compares every compiled function, error and event entry
  with all four checked-in ABI exports; selected role, ERC-165 and EIP-712 semantic
  vectors add independent assertions. Any mismatch reopens protocol lock.
- Behavioral tests cover `F < B`, `F == B`, `F > B`, mixed unfrozen/frozen
  consumption, full movement, ordinary self-transfer and rejected forced
  self-transfer.
- PFS-010 live evidence enters through the production CLI/RPC/native ABI path and
  covers mint, transfer, permit, memo, pause rejection, freeze, forced transfer and
  exact ledger reads before and after full-committee restart in
  `crates/testing/e2e-harness/features/stablecoin_factory_v1.feature`.
- PFS-010-07 component scenarios inject policy, role, permit-signature and supply-cap
  failures and assert exact ledger and event rollback through executable contract
  seams.
- ERC-3009, ERC-7802, reserve proofs, fee eligibility and payment-lane classification
  are explicitly outside V1 and require separate ADRs.
- ERC-7943 compatibility vectors pin the Final standard's functions, errors, events
  and ERC-165 interface id.
