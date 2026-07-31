# ADR-S-ZKP-001: ZK verification is bound to a versioned circuit registry and fail-closed CRS

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** protocol cryptography and circuit maintainers
- **Scope:** UltraHonk verifier precompile, canonical circuit/VK registry, CRS and Barretenberg FFI
- **Depends on:** ADR-S-GOV-003, ADR-B-WIR-001, ADR-B-EVM-001
- **Related:** ADR-C-GRT-003, ADR-S-ZKP-002

## Context

The verifier precompile accepts a circuit hash plus a combined UltraHonkKeccak proof,
selects embedded verification-key bytes from `outbe-zk-canonical`, and calls a
vendored Barretenberg backend using a global CRS. Proof acceptance can authorize
state changes in consumers, so registry membership, VK bytes, CRS availability, ABI
decoding and backend behavior are consensus inputs.

Despite the current Rust function name `dispatch_groth16`, this path verifies
UltraHonkKeccak, not Groth16. Poseidon hashing is a separate primitive in ADR-S-ZKP-002.

## Decision

### Versioned circuit authority

Maintain a protocol manifest whose entry binds:

- stable circuit id and semantic purpose;
- circuit bytecode/source commitment and compiler/backend versions;
- exact verification-key bytes and their cryptographic digest;
- proof-system/flavor and canonical public-input schema;
- maximum proof/public-input sizes and verification work class;
- activation and retirement protocol versions; and
- owning consumer ADR and compatibility policy.

The manifest is generated from reviewed, reproducible circuit artifacts and embedded
in the node. Duplicate ids/hashes, missing artifacts or registry/backend mismatches
fail the build. A transaction is verified against the registry active at its block's
protocol version, not whatever circuits happen to be compiled into the binary.

Circuit hash is not sufficient provenance by itself: startup reports and genesis/
chain-spec compatibility bind the complete manifest root and backend build identity.

### CRS is mandatory consensus configuration

Validators and block builders must initialize and authenticate the exact required CRS
before entering service. The CRS source, expected length and digest are pinned in
release/chain configuration. Network retrieval may stage artifacts during explicit
installation, but consensus execution never downloads them lazily.

Missing, short, corrupt or incompatible CRS is a startup-fatal readiness failure for
any role that can execute/propose/validate blocks containing verifier calls. It can
never be translated to “proof invalid”. All validators expose the same verified CRS
identity in diagnostics. Read-only nodes may run without it only if verifier calls
return explicit unavailable outside consensus simulation.

### Canonical request and outcome

Use a versioned typed ABI containing circuit id, canonical proof bytes and explicit
public inputs as required by the circuit profile. Decoder requires the standard
offset, exact padded length, zero padding, no trailing bytes and profile-specific
maximums before allocation or FFI.

Outcomes are distinct:

- `Valid` / `InvalidProof` are normal deterministic verification results;
- `UnknownOrInactiveCircuit` is a typed domain rejection;
- malformed/non-canonical input reverts;
- missing CRS, backend failure/panic, invalid VK artifact or impossible response is
  fatal execution infrastructure failure.

Consumers may deliberately expose invalid proof as boolean false, but operational
failure can never collapse into the same value.

### Bounded FFI and gas

Validate all lengths and registry metadata in Rust before FFI. The backend executes
synchronously in a bounded, panic-contained adapter with deterministic flags and no
network/filesystem/environment reads. Proof bytes cannot select backend options.

Gas is profile-specific and covers decoding, public inputs, circuit size and measured
worst-case verification. Maximum input size is enforced independently of gas. FFI
panics/errors reject execution consistently and surface operator-fatal diagnostics.

### Reproducible evidence

For every active circuit, CI reproduces circuit/VK hashes from pinned sources and
runs valid, invalid, malformed and cross-circuit vectors through the production EVM
precompile. Independent verifier vectors and negative mutation tests establish that
public-input binding is complete. Release CI verifies the CRS digest and backend
binary/source revision.

## Authoritative interfaces

| Responsibility | Authority |
|---|---|
| Active circuit/VK/profile set | protocol-versioned circuit manifest |
| CRS bytes and readiness | authenticated startup artifact manager |
| Canonical request decoding | typed verifier ABI |
| Cryptographic verification | pinned bounded backend adapter |
| Meaning of public inputs/result | owning consumer ADR |
| Address, gas and EVM mapping | ADR-B-WIR-001 and ADR-B-EVM-001 |

## Invariants

- Every validator executing a block uses identical manifest, VK, CRS and backend.
- No inactive/unknown circuit can be verified as active.
- Operational failure is never reported as an invalid proof.
- Canonically distinct requests cannot decode to the same proof invocation.
- Proof/public-input length and backend work are bounded before FFI.
- Verification has no network, filesystem, environment or mutable-global dependency
  after readiness.
- Circuit acceptance binds every public input required by its owner state transition.

## Atomicity, concurrency and replay

Verification is stateless and completes before consumer mutation. The manifest and
CRS are immutable after startup and safe for concurrent calls; initialization has an
explicit readiness result rather than a one-shot best-effort side effect. Replaying
the same request at the same protocol version produces the same outcome. Whether a
valid proof may be replayed is owned by the consumer's nullifier/state machine.

## Compatibility and migration

Circuit/VK bytes, proof system, ABI/public-input schema, manifest activation, CRS and
backend flags are protocol compatibility. Add a new manifest entry/version and
activate it; never replace VK bytes under an existing identity. Retirement defines
historical re-execution behavior. Nodes refuse a chain whose required manifest/CRS
identity they cannot supply.

## Production-interface verification evidence

Inspected startup `init_crs`, static circuit lookup, ABI decoder, Barretenberg FFI,
precompile dispatch/gas, Cargo git pin and current unit tests. The git dependency is
lock-pinned, but readiness is best-effort and operational failures collapse to false;
no valid production proof fixture is exercised by this crate. Status remains
Proposed.

## Consequences

Cryptographic readiness becomes part of node readiness rather than an optional
optimization. module audits can trace a consumer proof check to exact circuit, VK,
CRS, backend and public-input evidence.

## Rejected alternatives

- **Continue when CRS loading fails:** validators can disagree on the same proof.
- **Treat every backend error as false:** hides infrastructure/corruption as user
  invalidity.
- **Make every compiled circuit permanently active:** bypasses protocol activation.
- **Identify the verifier as Groth16:** creates incompatible tooling/security claims.
- **Rely only on a git tag:** does not prove reproducible VK/circuit artifacts.

## Open questions and technical debt

1. **Critical:** `init_crs` catches error/panic and lets the node start while
   `verify_inner` maps backend errors to `false`. Validators with and without a CRS
   can execute the same valid proof differently. Make verified CRS readiness fatal.
2. `Once` records a failed initialization forever and exposes no readiness result or
   identity. Replace it with an explicit immutable initialized service.
3. Startup may download CRS from `crs.aztec.network`; consensus nodes must not depend
   on live network content. Stage and verify a pinned digest before service.
4. `OUTBE_BB_SRS_PATH` is an environment-selected consensus artifact without visible
   digest/length validation at this boundary. Bind it to chain/release configuration.
5. `SRS_POINTS` is manually derived from the currently largest circuit. Generate the
   requirement from the active manifest and reject insufficient/excessively malformed
   artifacts.
6. The API/constant/address call the verifier “groth16” while implementation is
   UltraHonkKeccak. Rename/version the ABI and prevent clients from assuming the
   wrong proof system.
7. `find_canonical` accepts every compiled registry entry unconditionally and leaves
   activation/deprecation to consumers. Enforce block-version activity centrally.
8. Registry lookup is a linear scan. Bound registry size or generate a collision-free
   lookup; validate duplicate circuit hashes at build time.
9. The local ADR cannot yet prove how `circuit_hash` is derived or that it commits to
   VK, bytecode, compiler flags and public-input schema. Publish a reproducible
   manifest/root.
10. `decode_input` accepts arbitrary dynamic offset, overlapping head/tail, nonzero
    padding, omitted padding and trailing bytes. Require canonical ABI encoding.
11. `offset + 32` is unchecked before the bounds test and can overflow `usize`.
    Perform checked arithmetic for every offset/length calculation.
12. Proof length is unbounded before FFI while gas is flat. Add per-profile maximums
    before slicing/allocation/backend parsing.
13. Flat `ZK_VERIFY_GAS = 3_000_000` is explicitly a placeholder. Benchmark supported
    hardware/circuit classes and include decoding/input-size work with safety margin.
14. Unknown circuit, invalid proof, missing CRS and arbitrary backend parse/verify
    errors ultimately produce the same zero word. Introduce typed internal outcomes
    and deliberate ABI mappings.
15. `verify_combined(...).unwrap_or(false)` discards all backend diagnostics and
    corruption classes. Backend error/panic must be fatal and observable without
    leaking proof secrets.
16. The crate tests no known-valid registered proof; the `ownership.bin` fixture is
    not referenced. Add valid/invalid vectors for every active registry entry through
    the production EVM path.
17. Prove combined proof bytes bind the intended public inputs and consumer context
    (chain, owner, nullifier, amount, etc.); registry membership alone is insufficient.
18. Audit GratisPool's direct backend/VK use against this same manifest/readiness
    authority so two verification paths cannot diverge.
19. Pin and attest the C++ Barretenberg build, compiler flags, target CPU behavior and
    vendored FFI ABI; add cross-platform deterministic release vectors.
20. Define thread-safety/resource limits for concurrent FFI calls and protect node
    liveness from verifier saturation.
21. Add startup self-tests using one valid and one invalid proof per backend flavor,
    reporting manifest/VK/CRS/backend digests.
22. Specify historical replay after circuit retirement: old blocks must remain
    verifiable without making retired circuits usable in new transactions.
