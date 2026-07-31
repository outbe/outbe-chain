# PFS-011: Role-scoped operational keys submit validator service transactions

- **Status:** Draft
- **Actors:** Validator account, ValidatorSet, Oracle feeder, OCOMP supervisor,
  ZeroFee, Oracle, OCOMP/Metadosis and public RPC
- **Trigger:** A validator delegates an operational EVM address for `ORACLE` or
  `OCOMP`, then that service submits its exact protocol transaction
- **Topology/services:** Four-validator localnet with four isolated OCOMP
  supervisor domains and public EIP-1559 transaction delivery
- **Referenced ADRs:** ADR-S-VAL-001, ADR-S-VAL-002, ADR-S-ORC-001,
  ADR-S-ORC-002, ADR-S-FEE-001, ADR-S-OCM-003 and ADR-S-OCM-004
- **Supersedes:** Oracle-local feeder delegation and node-signed OCOMP
  transaction delivery

## Outcome

Each validator can use distinct service keys for Oracle and OCOMP. A service key
represents exactly one active validator only for its delegated role, obtains the
matching narrow ZeroFee authorization and cannot exercise general validator
capabilities.

## Acceptance contract

- **Source:** The validator account is the only delegation author; the service
  owns the delegated private key.
- **Trigger:** Finalize `setDelegate(role, address)`, then submit the role's exact
  transaction envelope.
- **Environment:** Four active validators with live BLS shares, role-aware
  ValidatorSet, target precompile and ZeroFee hooks.
- **Canonical inputs:** Stable role id, validator address, delegate address,
  current Oracle period or finalized OCOMP job, exact calldata and fee envelope.
- **System under test:** ValidatorSet forward/reverse mappings, consumer
  principal resolution, ZeroFee authorization, local service signing, public
  transaction delivery and target execution.
- **Expected response:** The receipt signer is the delegate; ZeroFee names the
  represented validator. Oracle state uses that principal, while OCOMP state
  uses the independently verified validator index in the inner result vote.
- **Response measures:** Four distinct OCOMP delegates, no `ORACLE` resolution
  for them, successful receipts and one finalized quorum result.
- **Failure guarantee:** Wrong role, stale delegate, inactive/shareless validator,
  collision or malformed envelope cannot consume a vote slot or gain another
  validator capability.

## Preconditions and canonical inputs

- Every validator is registered, active and has a BLS share.
- Each service key is generated independently and stored only in its service
  custody domain.
- `ORACLE = 1` and `OCOMP = 2`.
- The validator account has enough balance to publish the delegation
  transaction; the operational transaction itself follows its exact ZeroFee
  hook.
- OCOMP has a finalized job and the node's separate attestation key is correctly
  bound to the job committee.

## Success sequence

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | service installer | generate a dedicated EVM key and derive its address | mode/owner checks and address |
| 2 | validator | call `setDelegate(role, address)` | successful receipt and delegation event |
| 3 | ValidatorSet | atomically write forward and reverse mappings | `getDelegate` and `resolveValidator` on all validators |
| 4 | service | obtain canonical data and build the exact role transaction | bounded typed calldata and fixed fee envelope |
| 5 | service key | sign the EIP-1559 envelope locally | recovered sender equals delegate |
| 6 | ZeroFee | classify envelope and resolve represented validator | canonical authorization subject |
| 7 | target module | Oracle re-resolves the principal; OCOMP verifies inner committee authority | successful receipt and role state |
| 8 | consensus | finalize inclusion and resulting Oracle/OCOMP state | finalized receipt and state parity |

For OCOMP, step 4 first requests only the inner sign-once `ResultVoteV1`
attestation from the node. The supervisor then constructs and signs the outer
transaction. The node never signs the EVM envelope.

## Boundaries and invariants

Delegation is one ordinary EVM transaction. Its writes and event commit
atomically. The subsequent role transaction must observe the finalized mapping;
txpool precheck is advisory and execution reauthorizes against canonical state.

```text
resolve(role, signer) = represented validator or none
represented validator is ACTIVE and has BLS share
one delegate -> at most one validator for one role
OCOMP resolution does not imply ORACLE resolution
```

The delegate is not substituted into registration, staking, governance, rewards
or delegation-authority calls.

## Observable completion contract

Completion requires:

- successful delegation receipts;
- `getDelegate` and `resolveValidator` parity on every validator RPC;
- recovered service transaction sender equal to the local dedicated key;
- role mismatch resolving to zero;
- successful role transaction receipt;
- Oracle state attributed to the represented validator or OCOMP state
  attributed to the verified inner vote member; and
- finality of the containing block and resulting quorum outcome.

A locally generated address, submitted hash or running process is insufficient.

## Rotation, revocation, retry and restart

Rotation atomically invalidates the old reverse mapping and installs the new one.
The old key cannot submit another role transaction after canonical rotation.
Revocation removes both mappings and restores validator-address fallback.

Exact transaction retry uses ordinary nonce/replay rules. OCOMP reorg retry
rebroadcasts the durable raw transaction. Supervisor restart reloads the same
key and journal; it does not regenerate the key or ask the node to sign an EVM
transaction.

## E2E scenario matrix

| Id | Scenario | Given | When | Then | Automation |
|---|---|---|---|---|---|
| PFS-011-01 | delegated OCOMP finalization | four active validators and four distinct OCOMP delegates | supervisors submit a real finalized-job vote | delegates resolve only for OCOMP and three matching votes atomically apply Lysis | `@pfs-011-01` in `ocomp_public_path.feature`; implemented, intentionally not run in this change |
| PFS-011-02 | role isolation | one address delegated for OCOMP | address is queried/used as Oracle feeder | Oracle resolution and authorization reject it | unit coverage; live negative scenario pending |
| PFS-011-03 | rotation and revocation | an existing role delegate | validator rotates and revokes it | old reverse mapping disappears and self-fallback returns only after revoke | ValidatorSet unit coverage |
| PFS-011-04 | same-role collision | two validators select one address for one role | second delegation executes | transaction reverts without changing either mapping | ValidatorSet unit coverage |
| PFS-011-05 | inactive validator | delegation exists, validator loses eligibility | service submits role transaction | ZeroFee and target fail closed without consuming a vote | unit coverage; live lifecycle scenario pending |
| PFS-011-06 | generic authority isolation | delegated service key | key calls registration, stake, governance, rewards or delegation methods | every call follows existing authority and rejects the service key | live negative scenario pending |
| PFS-011-07 | restart and sign-once | one OCOMP vote is prepared/submitted | supervisor and node restart, then retry | same inner vote and raw transaction are replayed; no equivocation | existing OCOMP restart coverage needs explicit delegated-key assertion |

## Current automation and gaps

The Rust harness installs four role mappings through public ValidatorSet
transactions before starting OCOMP roles. The dedicated scenario checks mapping
parity, distinct keys, role isolation and the real public OCOMP apply path.

The scenario was added but not executed as part of this change. Release CI must
run it on the production-feature harness. Live Oracle rotation, validator
lifecycle loss and generic-authority negative scenarios remain required before
this flow can be marked Automated.
