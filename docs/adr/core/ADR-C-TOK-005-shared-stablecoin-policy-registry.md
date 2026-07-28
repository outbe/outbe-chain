# ADR-C-TOK-005: Stablecoins use one shared bounded policy registry

- **Status:** Proposed; design approved, not implemented
- **Date:** 2026-07-27
- **Owners/scope:** proposed `crates/core/stablecoinpolicy`; policy identity,
  administration, membership and directional authorization views
- **Depends on:** ADR-B-EVM-003, ADR-B-EVM-004, ADR-B-EVM-005
- **Used by:** ADR-C-TOK-003, ADR-C-TOK-004

## Context

Regulated stablecoins need allow/deny rules, but copying membership state into every
token creates multiple mutable sources of truth and expensive synchronization. A
shared policy may intentionally serve several tokens, so its identity, admin
transfer and update behavior must remain stable even while tokens rebind to another
policy.

The registry decides account eligibility only. It does not hold token balances,
freeze amounts, allowances, reserves, issuer credentials or fee-asset status.

## Decision

### Permanent policy identities

`STABLECOIN_POLICY_REGISTRY_ADDRESS` is the fixed Rust precompile
`0x000000000000000000000000000000000000EE10` and the sole policy state owner. Policy ids are monotonic `U256` values. Two protocol policies always
exist:

- `DENY_ALL = 0`; and
- `ALLOW_ALL = 1`.

Both are immutable and have no admin. Permissionless callers may create policy ids
starting at 2, provided they choose a nonzero admin and a valid type. Creation
rejects native value.

A policy id and its type are permanent. Policies cannot be deleted, transformed or
automatically redirect bound tokens. A token changes behavior only when its
`COMPLIANCE` role explicitly binds another existing id.

### Closed bounded policy types

V1 supports these stable `uint8` descriptors:

- `0`: `DenyAll` (`policyId = 0` only);
- `1`: `AllowAll` (`policyId = 1` only);
- `2`: `Whitelist`, where an account is allowed exactly when it is a member;
- `3`: `Blacklist`, where an account is allowed exactly when it is not a member; and
- `4`: `Directional`, which separately references a send, receive and mint-receive
  policy.

The three children of a Directional policy must be built-ins, Whitelist or Blacklist
policies. They cannot be Directional, which forbids recursion and makes every query
O(1) with a fixed maximum of three membership reads. Policy type and child ids are
immutable after creation.

For simple policies the same membership result applies to send, receive and mint
queries. Directional policy selects its named child. `DENY_ALL` always returns false
and `ALLOW_ALL` always returns true.

### Administration and membership

Each mutable policy has exactly one admin. Replacement is two-step:

```text
admin --beginPolicyAdminTransfer(policyId, nonzero candidate)--> pending admin
candidate --acceptPolicyAdminTransfer(policyId)----------------> admin
admin --cancelPolicyAdminTransfer(policyId)--------------------> no candidate
```

A policy admin may add or remove members from its own Whitelist or Blacklist.
Directional policies have no direct membership; their child admins remain
independent. Batch updates are allowed only up to one protocol constant, reject
duplicate accounts within a batch and apply atomically. Zero address membership is
rejected. No generic policy admin may edit ids 0 or 1.

Every creation, member change and admin transition emits a typed event carrying the
policy id and actor. The registry stores no free-form reason, memo or off-chain
identity data.

### Authorization queries

The canonical read surface is:

```solidity
function policyExists(uint256 policyId) external view returns (bool);
function policyType(uint256 policyId) external view returns (uint8);
function policyAdmin(uint256 policyId) external view returns (address);
function pendingPolicyAdmin(uint256 policyId) external view returns (address);
function isMember(uint256 policyId, address account) external view returns (bool);
function policyMemberCount(uint256 policyId) external view returns (uint256);
function listPolicyMembers(uint256 policyId, uint256 offset, uint256 limit)
    external view returns (address[] memory);
function canSend(uint256 policyId, address account) external view returns (bool);
function canReceive(uint256 policyId, address account) external view returns (bool);
function canMint(uint256 policyId, address account) external view returns (bool);
```

`isMember` exposes raw membership only: it returns stored membership for Whitelist
or Blacklist and false for built-ins, Directional and unknown ids. It does not mean
"authorized" for a Blacklist. `policyType` and descriptor/admin views revert for an
unknown id; authorization views return false. Authorization views never mutate or
revert for ordinary unknown/account input. Typed internal Rust query APIs expose the
same semantics to ADR-C-TOK-003 without ABI sub-calls.

`policyMemberCount` and `listPolicyMembers` are valid only for existing Whitelist and
Blacklist policies; built-in and Directional policies revert with
`PolicyMemberEnumerationUnsupported(policyId, policyType)`, while unknown ids revert
with `UnknownPolicy(policyId)`.
`listPolicyMembers` accepts `1 <= limit <= 100`. It returns the current member list
without sorting or ordering guarantees and has no cursor/filter semantics.
`offset >= policyMemberCount(policyId)` returns an empty array; the final page is
clamped to the current count. `policyMemberCount` supplies the current count required
for paging.

The canonical mutation surface lives in
`contracts/precompiles/src/IStablecoinPolicyRegistry.sol` and contains policy
creation, bounded member add/remove and two-step admin transfer. Token binding is not
a Registry command; it is owned by each token's `COMPLIANCE` authority.

## State and invariants

Registry state contains the next id, immutable policy descriptors, per-policy admin
and pending admin, membership mappings for simple policies, and a dense member index
kept atomically consistent with those mappings.

- ids 0 and 1 always exist with their fixed meanings;
- every id from 2 to `nextPolicyId - 1` names exactly one descriptor, and checked
  increment rejects id exhaustion;
- every mutable policy has one nonzero admin;
- a Directional policy references three existing non-Directional policies;
- membership mutation is authorized only by that policy's current admin; and
- every member mapping entry agrees with the dense index and its reverse position;
- authorization uses direct membership lookup and never scans the member index; and
- list materialization is bounded by the caller-selected page limit of at most 100.

## Atomicity, replay and failure

The revm frame journal owns atomicity. A batch is fully validated for authority,
bounds, duplicates and address validity before its first write. Failed creation,
member update or admin transfer leaves descriptor, membership, next id and logs
unchanged. Repeating an already-applied add/remove returns the typed `MembershipUnchanged`
revert. Silent idempotency and partial batches are forbidden.

Policy denial in ADR-C-TOK-003 is a typed token revert and creates no Registry
receipt or custody state.

## Determinism and bounds

Policy types and batch cap are hard-fork constants. All iteration is bounded by the
batch input cap; authorization is O(1). No wall-clock, randomness, external network,
process-local cache or dynamic plugin affects policy results. The active hard fork
may add policy types only with explicit old/new codec and authorization vectors.

## Consequences

Several tokens can intentionally share one compliance source without duplicating
membership. Directional composition supports common one-way restrictions while its
closed depth keeps gas and execution deterministic. Because policy updates can
change several tokens at once, two-step admin transfer, permanent types and complete
eventing are protocol requirements rather than UI conveniences.

## Rejected alternatives

- Token-local allowlists were rejected as duplicated state and administration.
- A legacy/fallback policy path was rejected; the registry is authoritative from the
  first token.
- Recursive arbitrary policy graphs were rejected because they complicate gas,
  cycle detection and auditability.
- Mutable type, deletion and automatic token fallback were rejected because they can
  silently change many ledgers.
- Validator-only policy creation was rejected; policy creation itself moves no funds
  and Factory admission separately governs token creation.

## Protocol lock and technical debt

The Registry shares Stablecoin V1 activation at protocol version `0.2` (raw `2`); its
fixed `0xEE10` marker remains genesis-active independently. One add/remove call accepts
at most 64 accounts and validates the entire batch before the first write.
`listPolicyMembers` has a caller-selected maximum page size of 100 and no sorting or
ordering contract. View pagination has no separate gas-budget or benchmark gate. The
initial policy-authorization ceiling is `10,000` gas and the 64-member mutation
ceiling is `750,000`, under the shared native schedule and benchmark reopen rule in
`fork-manifest.json`.

- Keep add/remove `MembershipUnchanged` behavior and the exact event/error ABI aligned
  with the checked-in golden vectors.
- Policy content does not prove KYC, sanctions screening or legal validity; those are
  issuer/admin responsibilities and must not be implied by Factory or README wording.
