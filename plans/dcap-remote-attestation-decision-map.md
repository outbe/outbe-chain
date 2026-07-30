# DCAP and Remote Attestation Decision Map

Status: design and engineering choices complete; implementation planned
Source plan: `/home/ubuntu/piolium/remediation-plan.md`
Reference implementation: `/home/ubuntu/SecretNetwork`

This map is the boundary for the DCAP/remote-attestation work. A topic not
listed here does not enter the DCAP grilling session.

Classification:

- `RESOLVED` — already agreed in the design discussion.
- `ADOPTED` — supported by Secret Network and compatible with the Outbe model.
- `OPEN` — requires one focused discussion, research task, or prototype.
- `DEFERRED` — belongs to another subsystem.

Frontier:

- user decisions: none;
- implementation/release validation is assigned to the implementation plan;
- all other DCAP/RA design questions are resolved or explicitly deferred.

## #1: What is inside the DCAP/RA scope?

Blocked by: none
Type: Discuss

### Question

Which mechanisms are required to prove an enclave identity and establish an
authenticated remote session?

### Answer

`RESOLVED`.

In scope:

- quote creation and `REPORT_DATA`;
- canonical quote/collateral evidence;
- deterministic QVL and TCB policy;
- measurement and hardware admission;
- node-to-enclave registration, leases, renewal, and expiry;
- proof of possession of attested keys;
- authenticated remote sessions;
- genesis attestation and production hardware release evidence;
- evidence caps and consensus resource accounting.

`DEFERRED`:

- offer-key DKG, BLS, reshare, and VRF shares;
- entitlement, Tx2 delivery, and delivery cryptography beyond the rule that
  secrets are released only to an attested key;
- consensus readiness and execution gating;
- software migration, sealed-state continuity, and reshare interlocks;
- recovery of lost identity state;
- consensus timestamp authenticity, tracked as a separate dependency.

## #2: What is the production attestation mode?

Blocked by: #1
Type: Discuss

### Question

May production fall back to unattested or development evidence?

### Answer

`RESOLVED`: no.

- Production is `DcapRequired` and fails closed.
- `GramineDirectDev` has a different chain ID and genesis.
- Missing verifier, quote, collateral, or hardware support is rejection.
- V1 accepts SGX ECDSA Quote v3 only. TDX and quote v4/v5 require a later
  policy version.
- The only production V1 target is x86_64 with Intel SGX. ARM TEE and aarch64
  are not planned or supported by this protocol version; adding another TEE or
  architecture requires an explicit later protocol and release decision.

This closes the behavior in
`findings/M1-strict-tee-policy-accepts-unattested-sidecar/report.md`.

## #3: Where is the QVL trust boundary?

Blocked by: #2
Type: Research

### Question

What result is consensus authority: host/QvE output or deterministic
verification of the submitted evidence?

### Answer

`RESOLVED`: every node runs the same pure verifier over transaction bytes,
the active policy, and block timestamp. It performs no network, PCCS,
filesystem, environment, or wall-clock reads.

Secret Network's Go `VerifyCertDCAP` only extracts a key; its real path invokes
Intel QVL outside the enclave and verifies a QvE report inside the enclave:

- `/home/ubuntu/SecretNetwork/x/registration/remote_attestation/remote_attestation.go:22`
- `/home/ubuntu/SecretNetwork/cosmwasm/enclaves/execute/src/registration/onchain.rs:44`
- `/home/ubuntu/SecretNetwork/cosmwasm/enclaves/shared/crypto/src/dcap.rs:27`

That is suitable for an enclave-local decision, not as Outbe's
consensus-native verifier.

## #4: What evidence is canonical?

Blocked by: #3
Type: Discuss

### Question

Does registration depend on a collateral service or local verifier state?

### Answer

`ADOPTED`, with stricter Outbe encoding.

- Every evidence-bearing operation carries a self-contained quote and complete
  Intel-signed collateral.
- The untrusted host may fetch collateral but is not an authority.
- Decoding rejects unknown/duplicate components, trailing bytes,
  non-canonical DER or signed JSON, cross-platform collateral, and cap excess
  before allocation.
- Verified measurement, FMSPC, PCE ID, and PCK CA type are derived from the
  authenticated evidence, never caller labels.

Secret Network also packages quote and collateral together, but treats the
collateral as a more opaque QVL blob:
`/home/ubuntu/SecretNetwork/cosmwasm/enclaves/execute/src/registration/attestation.rs:492`.

## #5: Which TCB and measurement states are admitted?

Blocked by: #3
Type: Discuss

### Question

Which code identity and Intel TCB results are acceptable in production?

### Answer

`RESOLVED`.

- Platform TCB may be `UpToDate` or `SWHardeningNeeded`.
- QE TCB must be `UpToDate`.
- TCB Info schema v3 remains mandatory. A schema-v2 result is rejected even
  when its reported status is `UpToDate`.
- Intel-authenticated advisory IDs accompanying an admitted
  `SWHardeningNeeded` Platform result are preserved in the stable verdict.
- Configuration-needed, out-of-date and revoked Platform or QE results are
  rejected. `SWHardeningNeeded` is not admitted for QE.
- Admission requires exact `MRENCLAVE`, matching `MRSIGNER`, `ISVPRODID`, and
  minimum `ISVSVN`.
- `MRSIGNER` alone or a higher SVN never admits unknown code.

The initial policy follows Secret Network's practical acceptance of
`SWHardeningNeeded`, but makes the accepted Platform set explicit and keeps QE
strict. Governance may later activate a complete policy that tightens Platform
admission to `UpToDate` only; existing leases remain valid only until their
bounded expiry and cannot renew under a policy they no longer satisfy.

Do not copy Secret Network's undifferentiated warning-only handling of other
non-OK QVL outcomes:
`/home/ubuntu/SecretNetwork/cosmwasm/enclaves/execute/src/registration/onchain.rs:50`.

## #6: Who changes attestation policy and hardware admission?

Blocked by: #5
Type: Discuss

### Question

How does the network admit a new measurement or platform without a central
operator?

### Answer

`RESOLVED` and `ADOPTED` in strengthened form.

- Anyone may submit a governance proposal.
- Governance approves a complete canonical policy hash and activation data,
  not a privileged local override.
- Any relay may execute the accepted activation.
- Measurement updates include exact new rules and rolling overlap/cutoff.
- Platform admission binds chain-scoped `H(PPID)` and the previously agreed
  controller commitment; it is governance state, not a hard-coded list or
  vendor JWT service.

Secret Network establishes the useful governance precedent for a new
`MRENCLAVE`, but its bare 32-byte measurement proposal and built-in PPID/JWT
allowlist are not copied:

- `/home/ubuntu/SecretNetwork/x/compute/internal/keeper/ante.go:157`
- `/home/ubuntu/SecretNetwork/cosmwasm/enclaves/execute/src/registration/attestation.rs:891`

The hard-fork-only wording in the source remediation plan is stale.

## #7: What does `REPORT_DATA` bind?

Blocked by: #5, #6
Type: Discuss

### Question

Can a quote be replayed for another chain, node, operation, or lease?

### Answer

`RESOLVED`.

- A fresh quote is generated for each registration, renewal, or measurement
  intent.
- `REPORT_DATA` binds the canonical intent: chain, genesis, operation, node
  identity, profile, enclave keys, policy, version/nonces, and requested lease.
- The remaining commitment binds the genesis, active policy, and the single
  `NodeHost` authorization. V1 has no role-key set.
- There is no unbound-attestation mode.
- A quote may be cached or retried only for the exact same intent hash.
- Monotonic on-chain version/nonces and exact idempotent replay provide
  freshness; DCAP does not prove quote creation time.

Secret Network binds one registration key and an optional second key, without
chain/genesis/nonce/lease:
`/home/ubuntu/SecretNetwork/cosmwasm/enclaves/execute/src/registration/offchain.rs:383`.
Outbe keeps the stronger design.

## #8: Who is the registration principal?

Blocked by: #7
Type: Discuss

### Question

Does transaction sender authority substitute for attestation or node
ownership?

### Answer

`RESOLVED`.

- `msg.sender` is only a permissionless relay.
- Validator identity is its address plus consensus BLS public key.
- Full-node identity is its persistent Reth P2P public key.
- Both profiles use the same mandatory DCAP/lease flow.
- Node and enclave signatures cover the same canonical authorization.
- One node has at most one active enclave; one enclave cannot bind to two
  nodes.

Secret Network also lets the transaction sender act as a relay, but does not
bind a full Outbe-style node/profile lifecycle to the quote.

## #9: How are registration replay and renewal modeled?

Blocked by: #7, #8
Type: Discuss

### Question

Is attestation a permanent record or a renewable authorization?

### Answer

`RESOLVED` at the protocol level.

- Registration is a renewable lease.
- Renewal requires the exact next version/nonce and a fresh intent-bound quote.
- Exact current replay is idempotent; old or conflicting replay rejects.
- An expired current identity may renew; a superseded identity cannot.
- There is no standalone API that pretends to erase a secret already delivered
  to an enclave.

Secret Network has no lease/version/renewal lifecycle, so its permanent
registration record is not copied.

The numerical lease and outage policy is resolved in #12.

## #10: How does a remote session use attestation?

Blocked by: #7, #8, #9
Type: Discuss

### Question

Is a fresh DCAP quote required for every connection?

### Answer

`RESOLVED` at the architectural level.

- DCAP authenticates the registered identity and its stable Noise responder
  key.
- The remote party verifies an active, unexpired registration and then uses a
  fresh Noise handshake to prove live possession of that key.
- A new DCAP quote is not generated per application session.
- A quote alone is not host authorization and does not replace Noise initiator
  authorization.

Secret Network encrypts seed material to the quote-bound registration key,
which is the useful proof-of-possession precedent, but it does not define
Outbe's live Noise session model:
`/home/ubuntu/SecretNetwork/cosmwasm/enclaves/execute/src/registration/onchain.rs:147`.

The finalized-state authority used by a remote verifier is resolved in #13.

## #11: How are genesis and release attestation closed?

Blocked by: #3, #4, #5
Type: Research

### Question

May committee signatures or Gramine tests substitute for hardware
attestation?

### Answer

`ADOPTED`: no.

- Every block-1 enclave carries complete evidence and passes the same QVL.
- Committee signatures bind bootstrap facts but do not authenticate enclave
  code.
- Production release requires a fail-not-skip real SGX/DCAP end-to-end gate.
- Saved canonical quote/collateral vectors are required for deterministic
  regression tests.

## #12: What are the lease constants and collateral-outage policy?

Blocked by: #9, #15
Type: Discuss

### Question

Are the proposed one-hour minimum, seven-day maximum, one-hour collateral
margin, and one-third renewal window justified, and what happens during a
mass PCCS/PCS outage?

### Answer

`RESOLVED`.

- DCAP has no trusted quote timestamp. Collateral is evaluated at consensus
  block time; there is no synthetic `quote_age`.
- `valid_until` is capped by the earliest authenticated collateral deadline
  minus the protocol safety margin.
- A PCS/PCCS outage does not extend a lease and does not enable an
  emergency/fail-open renewal path. Existing leases remain valid until their
  original expiry; registrations and renewals without complete valid
  collateral reject.
- Operators may prefetch and submit self-contained collateral, so consensus
  verification never depends on live PCS/PCCS availability.
- The requested lease must be at least one hour and no more than seven days.
- `valid_until` is additionally capped at one hour before the earliest
  authenticated collateral deadline. Registration or renewal rejects if this
  cap leaves less than the one-hour minimum.
- Renewal opens when the final third of the actual lease begins. Before that
  point, only exact idempotent replay is accepted.

Secret Network has no renewable lease and therefore does not answer this
question.

## #13: What finalized proof does a remote verifier trust?

Blocked by: #9, #10
Type: Discuss

### Question

How does a local host, peer node, or external client prove that the
quote-bound key still has an active, unexpired registration without trusting
an RPC response?

### Answer

`RESOLVED`.

- A validator or full node checks the registration and lease against its own
  consensus-finalized state. An RPC response is not authority and no duplicate
  proof is required between components sharing that node-owned finalized view.
- An external verifier must have a trusted genesis/checkpoint and an Outbe
  light-client view. It verifies the registration storage proof only against a
  state root that this view already recognizes as finalized. A client without
  such an anchor explicitly trusts its RPC provider.
- The verifier checks the active binding, profile, quote-bound Noise responder
  key, and `valid_until` before the handshake. A session deadline may not exceed
  `valid_until`; the verifier rejects new traffic and closes the session at
  expiry.
- Session admission does not run a continuous consensus light client inside
  the enclave. The enclave proves live possession of the registered responder
  key through the fresh Noise handshake.
- Secret-bearing command subsystems may reuse an on-demand, enclave-side,
  genesis-rooted finality and MPT verifier. That verifier proves authorization
  at a particular finalized height but never claims that a hostile host
  supplied the latest head. Its detailed delivery/migration use remains outside
  the DCAP/RA scope.
- A committee opened only from the state root of the same untrusted header it
  certifies is circular and forbidden. Any enclave or external light-client
  path depends on the separately remediated H1 predecessor-carried,
  genesis-rooted committee chain.

Secret Network's continuously advancing enclave light client provides the
useful principle that host-supplied chain bytes are transport, not authority.
It is not copied for session admission because withholding or sealed-state
rollback can keep it at an old valid height, so it does not establish
latestness or current lease validity:

- `/home/ubuntu/SecretNetwork/cosmwasm/enclaves/shared/block-verifier/src/verify/header.rs:27`
- `/home/ubuntu/SecretNetwork/cosmwasm/enclaves/execute/src/registration/offchain.rs:1774`
- `/home/ubuntu/piolium/findings/H1-self-certified-follower-committee/report.md:10`

## #14: Who may request quotes and protected enclave commands?

Blocked by: #8, #10
Type: Discuss

### Question

After removing `OperatorRecovery`, what is the minimal Noise initiator and
command-capability model?

### Answer

`RESOLVED`.

- An uninitialized enclave accepts exactly one initialization manifest signed
  by the persistent node identity. The manifest binds chain/genesis, node,
  profile, initialization challenge, and one persistent `NodeHost` Noise
  initiator public key before the enclave seals its identity.
- Initial and renewal quote generation requires that signed initialization
  authorization or the authenticated current `NodeHost`; there is no public
  production `GetQuote` endpoint after initialization.
- The responder extracts the Noise initiator static key after message 1 and
  rejects an unknown key before application request decode, dispatch, or side
  effects.
- A deny-by-default, exhaustive profile-and-state command matrix is mandatory.
  Secret-bearing commands additionally verify their canonical/finalized
  protocol authorization inside the enclave.
- V1 has no `DkgCoordinator`, `DeliveryRelay`, `OperatorRecovery`, wildcard, or
  default role keys. Separate keys stored under the same hostile host OS add
  lifecycle complexity without creating another TEE trust boundary.
- Restrictive UDS permissions and peer credentials are defense in depth, not
  cryptographic authority.

This closes
`/home/ubuntu/piolium/findings/M2-sidecar-trusts-unauthorized-host-requests/report.md`.
Secret Network has no reusable initiator-authentication model; its useful
precedent is only that a sensitive enclave command verifies its own
attestation/protocol authorization rather than trusting the caller.

## #15: What does expiry or emergency policy rejection disable?

Blocked by: #6, #9, #10
Type: Discuss

### Question

Does a policy change affect only new registration/renewal, or may governance
immediately stop accepting outputs and sessions from an active lease?

### Answer

`RESOLVED`.

- An offer key already delivered to an enclave cannot be remotely deleted,
  revoked, or proven erased. V1 exposes no API or governance action claiming
  otherwise.
- A governance policy change stops new admission, key delivery, and renewal
  under the rejected policy. It does not retroactively shorten an already
  active lease.
- Until that lease expires, honest participants continue to accept the
  registered identity. At expiry they reject its new sessions and protocol
  outputs. This is protocol authorization, not evidence that the enclave
  stopped running or communicating privately.
- If the permanent genesis offer key is extracted or copied, it is
  irreversibly compromised for that genesis. Recovery requires a new key epoch,
  explicit migration, or a new genesis and is outside the V1 DCAP/RA scope.
- Specifications must not call lease expiry, policy rejection, or local enclave
  self-disablement “offer-key revocation”.

Secret Network has the same hard boundary. Its measured enclave can mark a
replaced machine as disallowed and refuse further block processing, and
governance/software upgrades can freeze new registrations or require patched
platforms. Neither mechanism proves deletion of a consensus seed already
received by that machine:

- `/home/ubuntu/SecretNetwork/cosmwasm/enclaves/shared/block-verifier/src/submit_block_signatures.rs:63`
- `/home/ubuntu/SecretNetwork/docs/proposals/v1.10.md:7`

## #16: Is public platform linkability acceptable?

Blocked by: #4, #6
Type: Discuss

### Question

Does publishing complete quote/collateral evidence and governance-approved
`H(PPID)` create acceptable hardware linkability?

### Answer

`RESOLVED`.

Public self-contained quote/PCK/collateral evidence and governance-approved
hardware admission make cross-registration platform correlation possible.
Hashing a PPID into
`H(domain || chain_id || genesis_hash || PPID)` prevents publishing it as a
convenient raw registry field but does not make the public PCK evidence
unlinkable.

V1 accepts and documents this limitation:

- use a full 32-byte, chain-scoped platform commitment derived only from
  authenticated evidence;
- never copy raw PPID into separate calldata, state, event, log, or API fields;
- make no anonymity or cross-registration unlinkability claim;
- treat privacy-preserving hardware admission as a separate V2 redesign, not a
  codec adjustment.

Secret Network makes the same tradeoff: it stores the registration certificate,
derives a PPID/CPU-certificate-based machine ID, and operates a machine
allowlist. It demonstrates the correlation cost rather than solving it.

## #17: Which QVL implementation is consensus-safe?

Blocked by: #3, #4, #5
Type: Research

### Question

Which pinned Rust verifier and parser subset produces deterministic results on
the supported x86_64 production target and exposes every required protocol
check?

### Answer

`RESOLVED`; implementation acceptance remains in the backlog.

The cryptographic core is exact-pinned `dcap-qvl = 0.5.2`, crates.io checksum
`92a14fb8954c867d6855e44d98eab18e769816357738406691ebe60d8fdd005d`,
upstream commit `31a32a44de4cf68cb50c079e5bfd5348e4e6f4d5`, wrapped by a
consensus-owned Outbe verifier. The current host-local call in
`crates/system/tee/src/quote.rs` is not reusable: it reads environment,
filesystem, and wall clock and the dependency is optional.

The production dependency uses `default-features = false` and exactly
`std`, `ring`, and `default-x509`. Ring is the sole crypto backend; HTTP,
report generation, RustCrypto, TCB overrides and language bindings are absent.

The production wrapper:

- accept only canonical evidence bytes, the active policy, and block timestamp;
- require full input consumption and reject trailing/unsupported quote data;
- enforce platform and QE status, minimum TCB evaluation data number, PCE ID,
  exact measurement policy, and the pinned Intel root outside permissive
  upstream defaults;
- provide a deterministic DER-to-QVL adapter and stable Outbe verdict codes;
- never expose upstream error strings as consensus output.

The public QVL API is sufficient for the adapter and authenticated PCK
extension checks, so a fork is not planned. An implementation-discovered API
gap requires a separate minimal source-diff review rather than an implicit
fork.

Implementation/release acceptance evidence:

- fixed positive and negative quote/collateral vectors;
- deterministic time-boundary results;
- canonical DER and signed-JSON behavior;
- dependency and Intel-root pinning;
- no optional verifier feature or fail-open fallback;
- byte-stable verdict vectors across supported x86_64 validator builds.

Secret Network's host Intel QVL plus enclave QvE-report path is not copied
because it is not a consensus-native deterministic verifier.

Local prototype evidence, including the demonstrated upstream trailing-byte
acceptance and Ring/RustCrypto differential timing, is recorded in
`plans/evidence/dcap-qvl-engineering-gates.md`.

## #18: What evidence caps and gas prices are normative?

Blocked by: #17
Type: Prototype

### Question

Which quote/component/aggregate caps and QVL gas coefficients fit a dense
bootstrap and steady-state block without enabling DoS?

### Answer

`RESOLVED`; protocol constants are fixed, with release calibration retained as
implementation acceptance.

V1 uses:

- `16 KiB` quote cap;
- `896 KiB` per collateral component and complete canonical evidence cap;
- `16 KiB` maximum non-evidence framing per evidence-bearing registry call;
- 64 active measurement rules;
- `1,310,720`-byte `TeeBootstrapV2`;
- `500,000,000` gas only at block 1 and `30,000,000` from block 2 onward.

The former 1-MiB aggregate candidate is rejected: with the normative gas
formula its all-non-zero calldata plus QVL precharge may exceed a 30-million
steady block. At 896 KiB plus maximum framing, `registerEnclave` costs at most
`28,768,784` gas and the more expensive binding replacement costs at most
`29,133,784`.

The exact QVL charge is
`1,500,000 + 6*evidence_len + 120,000*9 + 180,000*2 + 160,000*2 +
10,000*active_rule_count`. Caps are checked before allocation; all arithmetic
is checked `u64`; batch deduplication never removes per-participant logical QVL
work.

For 32 participants at the evidence cap and 64 rules, OST3 precharge is
`309,931,488`, maximum intrinsic gas is `20,992,520`, and combined OST3 leaves
`169,075,992` gas for the other mandatory block-1 work.

Secret Network's opaque host-call pricing and permissive section allocation are
not suitable precedent. Processor/Platform, large-CRL, cap-boundary and
slowest-supported-x86_64-validator benchmarks remain mandatory release tests
under I1/I8/I9, not open design questions.

## #19: Deferred continuity rule

Blocked by: none
Type: Discuss

### Question

Does software migration/reshare need further DCAP design now?

### Answer

`DEFERRED`.

For the DCAP baseline retain only the operational rule: do not software-migrate
an active validator during a frozen reshare; abort and retry later. All
generation-CAS, migration bundle, retry, and continuity mechanics belong to a
separate design effort.
