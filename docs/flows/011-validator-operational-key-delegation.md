# PFS-011: Role-scoped operational keys submit validator service transactions

- **Status:** Draft
- **Actors:** Validator account, ValidatorSet, Oracle feeder, OCOMP supervisor,
  ZeroFee (Oracle only), Oracle, OCOMP/Metadosis and public RPC
- **Trigger:** A validator delegates an operational EVM address for `ORACLE` or
  `OCOMP`, then that service submits its exact protocol transaction
- **Topology/services:** Every ACTIVE validator has an isolated OCOMP supervisor
  domain; public Oracle transaction delivery and the canonical OCOMP system-vote
  carrier
- **Referenced ADRs:** ADR-S-VAL-001, ADR-S-VAL-002, ADR-S-ORC-001,
  ADR-S-ORC-002, ADR-S-FEE-001, ADR-S-OCM-003 and ADR-S-OCM-004
- **Supersedes:** Oracle-local feeder delegation and node-signed OCOMP
  transaction delivery

## Outcome

Each validator can use distinct service keys for Oracle and OCOMP. A service key
represents exactly one validator only for its delegated role and cannot exercise
general validator capabilities. Oracle obtains its narrow ZeroFee authorization.
The OCOMP delegate signs a validator-authenticated system carrier with visible
`gas_limit = 30_000`; it is not a sponsored ordinary EVM transaction. Oracle
resolves current ACTIVE membership. OCOMP resolves membership from the
specific job's pinned historical ValidatorSet snapshot, so an old job remains
signable after a later membership boundary.

## Acceptance contract

- **Source:** The validator account is the only delegation author; the service
  owns the delegated private key.
- **Trigger:** Finalize `setDelegate(role, address)`, then submit the role's exact
  transaction envelope.
- **Environment:** A non-empty ACTIVE ValidatorSet whose members have live BLS
  shares and OCOMP registrations, role-aware ValidatorSet, target precompile and
  ZeroFee hooks.
- **Canonical inputs:** Stable role id, validator address, delegate address,
  current Oracle period or finalized OCOMP job, exact calldata and canonical
  role-specific envelope.
- **System under test:** ValidatorSet forward/reverse mappings, consumer
  principal resolution, Oracle ZeroFee authorization, OCOMP system-carrier
  authorization, local service signing, public delivery and target execution.
- **Expected response:** The receipt signer is the delegate. Oracle ZeroFee names
  the represented validator; the OCOMP system classifier independently resolves
  the pinned participant. Oracle state uses that principal, while OCOMP state
  uses the independently verified validator index in the inner result vote.
- **Response measures:** One distinct OCOMP delegate per pinned participant, no
  `ORACLE` resolution for them, successful receipts and one finalized quorum
  result.
- **Failure guarantee:** Wrong role, stale delegate, missing pinned participant,
  binding collision or malformed envelope cannot consume a vote slot or gain
  another validator capability. Oracle additionally rejects an inactive/shareless
  represented validator.

## Preconditions and canonical inputs

- Every validator is registered, active and has a BLS share.
- Each service key is generated independently and stored only in its service
  custody domain.
- `ORACLE = 1` and `OCOMP = 2`.
- The validator account has enough balance to publish the delegation
  transaction. Oracle service transactions follow their exact ZeroFee hook;
  OCOMP votes use the exact fee-free system-carrier form.
- OCOMP has a finalized job and the node's separate attestation key is correctly
  bound to the job's historical ValidatorSet snapshot.

## Success sequence

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | service installer | generate a dedicated EVM key and derive its address | mode/owner checks and address |
| 2 | validator | call `setDelegate(role, address)` | successful receipt and delegation event |
| 3 | ValidatorSet | atomically write forward and reverse mappings | `getDelegate` and `resolveValidator` on all validators |
| 4 | service | obtain canonical data and build the exact role transaction | bounded typed calldata and role-specific canonical envelope |
| 5 | service key | sign the public envelope locally | recovered sender equals delegate |
| 6 | ingress | Oracle uses ZeroFee; OCOMP is classified before ordinary intrinsic-gas handling as a 30,000-gas system carrier | canonical authorization subject and bounded system work |
| 7 | target module | Oracle re-resolves current eligibility; OCOMP verifies the inner signature against the pinned historical participant | successful receipt and role state |
| 8 | consensus | finalize inclusion and resulting Oracle/OCOMP state | finalized receipt and state parity |

For OCOMP, step 4 first requests only the inner sign-once `ResultVoteV1`
attestation from the node. The supervisor then constructs and signs the outer
system carrier. The node never signs that carrier. Its execution charges no
validator fee and consumes no ordinary user block gas.

## Boundaries and invariants

Delegation is one ordinary EVM transaction. Its writes and event commit
atomically. The subsequent role transaction must observe the finalized mapping;
txpool precheck is advisory and execution reauthorizes against canonical state.

```text
resolve(role, signer) = represented validator or none
Oracle subject is current ACTIVE and has BLS share
OCOMP subject is present in the exact job-pinned historical snapshot
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

Exact Oracle retry uses ordinary nonce/replay rules. OCOMP reorg retry
rebroadcasts the durable raw system carrier. Supervisor restart reloads the same
key and journal; it does not regenerate the key or ask the node to sign an EVM
transaction.

## E2E scenario matrix

| Id | Scenario | Given | When | Then | Automation |
|---|---|---|---|---|---|
| PFS-011-01 | delegated OCOMP finalization | every pinned ACTIVE validator has a distinct OCOMP delegate | supervisors submit canonical system-vote carriers | delegates resolve only for OCOMP, carriers consume no user gas, and the snapshot-derived quorum atomically applies Lysis | `@pfs-011-01` in `ocomp_public_path.feature`; implementation evidence required |
| PFS-011-02 | role isolation | one address delegated for OCOMP | address is queried/used as Oracle feeder | Oracle resolution and authorization reject it | unit coverage; live negative scenario pending |
| PFS-011-03 | rotation and revocation | an existing role delegate | validator rotates and revokes it | old reverse mapping disappears and self-fallback returns only after revoke | ValidatorSet unit coverage |
| PFS-011-04 | same-role collision | two validators select one address for one role | second delegation executes | transaction reverts without changing either mapping | ValidatorSet unit coverage |
| PFS-011-05 | membership changes after job open | delegation exists and the validator later leaves ACTIVE | service submits an old-job OCOMP vote and a current Oracle vote | OCOMP resolves the retained historical participant; Oracle rejects current ineligibility | historical-snapshot and Oracle unit coverage; live lifecycle scenario required |
| PFS-011-06 | generic authority isolation | delegated service key | key calls registration, stake, governance, rewards or delegation methods | every call follows existing authority and rejects the service key | live negative scenario pending |
| PFS-011-07 | restart and sign-once | one OCOMP vote is prepared/submitted | supervisor and node restart, then retry | same inner vote and raw transaction are replayed; no equivocation | existing OCOMP restart coverage needs explicit delegated-key assertion |

## Current automation and gaps

The Rust harness installs one role mapping per participating validator through
public ValidatorSet transactions before starting OCOMP roles. The dedicated
scenario checks mapping parity, distinct keys, role isolation, historical-job
continuity and the real public OCOMP apply path.

The scenario was added but not executed as part of this change. Release CI must
run it on the production-feature harness. Live Oracle rotation, validator
lifecycle loss and generic-authority negative scenarios remain required before
this flow can be marked Automated.
