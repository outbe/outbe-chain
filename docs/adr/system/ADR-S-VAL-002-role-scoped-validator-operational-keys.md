# ADR-S-VAL-002: ValidatorSet owns role-scoped validator operational keys

- **Status:** Accepted
- **Date:** 2026-07-30
- **Decision owners:** ValidatorSet, Oracle, OCOMP and transaction-admission maintainers
- **Scope:** role-scoped EVM delegation for validator-operated services
- **Depends on:** ADR-S-VAL-001, ADR-S-ORC-001, ADR-S-ORC-002,
  ADR-S-OCM-003, ADR-S-OCM-004 and ADR-S-FEE-001
- **Related flow:** PFS-011
- **Supersedes:** Oracle-local feeder delegation and node-owned OCOMP EVM
  transaction signing

## Context

Oracle feeders and OCOMP supervisors must submit authenticated protocol carriers without
copying the validator account's private key into those processes. An operational
key represents one validator for one narrow protocol role; it must not thereby
become a general validator key.

In particular, an operational key cannot register or reconfigure a validator,
change stake, participate in governance, receive or withdraw validator rewards,
or appoint another operational key. Those capabilities continue to use their
existing owner, validator or system-command authority.

Oracle ZeroFee and the OCOMP system-carrier classifier both need to recognize
the represented validator, but neither fee nor execution policy may become the
owner of validator identity or delegation state.

## Decision

ValidatorSet owns a reusable role-scoped delegation registry. Stable role ids are:

| Id | Role | Authorized consumer |
|---:|---|---|
| 1 | `ORACLE` | Oracle `submitVote` and its exact ZeroFee hook |
| 2 | `OCOMP` | OCOMP `submitLysisResult` authenticated system carrier |

New roles may be appended with new ids. Existing ids are never reordered,
repurposed or deleted. Unknown ids fail closed.

The validator address calls `setDelegate(role, delegate)` and
`revokeDelegate(role)`. A delegate cannot set, rotate or revoke itself. For each
role ValidatorSet maintains both:

```text
(validator, role) -> delegate
(role, delegate) -> validator
```

The reverse mapping makes authorization bounded and rejects assigning one
delegate to two validators in the same role. The same address may deliberately
hold different roles because every lookup includes the role id.
An address already registered as a validator cannot be another validator's
delegate, and an address currently delegated for any known role cannot later
register as a validator. These checks preserve one stable principal in both
directions.

Without an explicit delegation, an eligible validator address represents itself
for that role. Setting a delegate disables that direct fallback until revocation;
the old delegate is removed atomically during rotation. Revocation restores the
validator-address fallback.

A registered validator may configure its future operational key while inactive,
but a signer resolves during use only when the represented validator is `ACTIVE`
and has a live BLS share. Consumers receive the represented validator principal,
not merely a boolean.

## Consumer and ZeroFee boundary

Oracle calls ValidatorSet's role resolver and enforces vote-period rules against
the represented validator during execution. OCOMP uses the role resolver only
for its exact ZeroFee envelope; its protocol authority is the independently
verified inner `ResultVoteV1` signature by the pinned historical OCOMP key, not
the outer EVM sender.

ZeroFee remains split into:

1. stateless exact-envelope classification;
2. stateful role resolution and duplicate-vote authorization; and
3. executor enforcement against canonical state.

ZeroFee does not store delegation, grant generic validator authority, or decide
Oracle/OCOMP protocol validity. A role lookup is available only to the named
consumer; no global "is validator" predicate treats a delegate as a validator.

## OCOMP key custody and signing boundary

OCOMP uses two separate keys:

- the node-owned OCOMP result key is registered with proof of possession during
  `confirmValidatorReady(registration)` and signs the canonical inner
  `ResultVoteV1` behind sign-once and finalized-job checks; and
- the supervisor-owned, role-delegated EVM key signs the outer fixed-shape
  `submitLysisResult` system carrier.

The first key is membership material: every validator admitted to an `ACTIVE`
ValidatorSet snapshot has one registered OCOMP key, and historical jobs verify its
inner signature against that pinned snapshot. The delegate is not membership
material; changing or revoking it changes only who may submit the outer carrier.

The authenticated node socket may return only the canonical inner attestation.
It does not expose an EVM transaction-signing method and production node startup
does not load an OCOMP EVM signer.

The supervisor constructs the exact Metadosis call locally, enforces zero value,
zero tip, canonical visible `gas_limit = 30_000` and bounded calldata, signs it
with its dedicated key, persists the raw carrier and submits it through public
RPC. OCOMP classification occurs before ordinary intrinsic-gas/fee handling;
actual work uses the separate system budget. Restart or reorg
rebroadcast reuses the persisted bytes rather than signing a different envelope.

All OCOMP paths are derived from one deployment base path. The default is
`/opt/outbe-chain`, overridable by `OUTBE_OCOMP_BASE_PATH`. OCOMP roles inherit
the invoking process identity; the protocol does not require fixed service
usernames. The dedicated key is:

```text
<base>/ocomp/data/keys/ocomp-evm-key.hex
```

It is a non-symlink regular file owned by the effective process UID with mode
`0600`, one hard link and canonical lowercase hex encoding. Runtime re-checks
those properties and the opened inode before reading it. Installation creates
it once from 32 random bytes and never overwrites an existing key.

## Persistent state and compatibility

ValidatorSet appends two storage fields after the existing layout:

- slot 48: delegate by validator and role;
- slot 49: validator by role and delegate.

Role ids, storage slots, fallback behavior and eligibility rules are consensus
formats. Any change requires an activated migration and mixed-version evidence.
Legacy Oracle feeder storage remains reserved for layout compatibility but is no
longer authoritative.

The OCOMP result-key registration and its reverse key reservation are distinct
ValidatorSet fields owned by ADR-S-VAL-001. They are consensus membership material,
not role delegation. Replacing an OCOMP registration never changes the OCOMP EVM
delegate, and changing the delegate never changes a pinned result-signing key.

## Invariants

- A delegate resolves to at most one validator per role.
- A registered validator address is never another validator's delegate.
- An operational delegate cannot register as a validator until every role is
  revoked.
- Rotation removes the old reverse mapping before installing the new one.
- Revocation removes both directions and restores only self-fallback.
- An inactive or shareless validator has no usable delegate.
- An OCOMP delegate does not resolve for `ORACLE` unless separately delegated.
- A delegate acquires no registration, stake, governance, reward or delegation
  capability.
- Oracle ZeroFee and Oracle execution resolve the same validator principal.
- OCOMP system-carrier classification resolves the outer sender while OCOMP
  execution derives vote authority only from the inner historical-snapshot signature.
- The node never holds or exposes the supervisor's OCOMP EVM private key.

## Consequences

Compromise of a feeder or OCOMP transaction key is contained to its explicit
role. Operators can rotate it without changing consensus, staking, governance or
reward custody keys. The OCOMP system classifier can admit only the correct
carrier while the target module retains final authorization.

The additional on-chain delegation transaction must be finalized before the
service submits role transactions. Deployment readiness must compare the local
key-derived address with ValidatorSet's role mapping.

## Rejected alternatives

- **Put the validator EVM key in each service:** compromise grants all authority
  attached to that account.
- **Keep OCOMP EVM signing in the node behind a socket:** it expands the node
  signing API and couples transaction construction to the consensus process.
- **Store delegation in ZeroFee:** fee policy would become an identity registry
  and non-zero-fee consumers could diverge.
- **Separate ad-hoc delegation mapping in every service:** role semantics,
  rotation and collision checks would drift.
- **Treat a delegate as a general validator address:** it grants precisely the
  registration, stake, governance and reward capabilities this decision forbids.

## Verification

Required unit coverage includes stable role ids, unknown-role rejection, role
isolation, same-role collision, rotation, revocation, inactive/shareless
fail-closed behavior, Oracle principal resolution, OCOMP system-carrier
resolution and local restricted transaction construction.

PFS-011 defines the live delegated-signing flow. Its release gate uses a distinct
OCOMP result key for every active validator, finalizes their delegations, proves
the keys do not resolve for `ORACLE`, submits canonical authenticated system
carriers and finalizes an OCOMP result vote without validator-account signing.
