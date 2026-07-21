# ADR-B-RLS-001: Releases bind reproducible builds, reviewed dependencies and artifact provenance

- **Status:** Accepted; deterministic ELF slice implemented, package/SGX/OCI authorization remains open
- **Date:** 2026-07-17
- **Owners/scope:** Rust/Node/Solidity dependency resolution, CI actions, release binaries/packages/images, SBOM, signatures and provenance
- **Depends on:** ADR-B-CRY-001, ADR-B-GEN-001, ADR-B-TST-001, ADR-B-DEP-001, ADR-B-OPS-001

## Context

Validators do not execute source files; they execute artifacts produced by compilers,
package managers, GitHub Actions, GoReleaser and container builders. `Cargo.lock`, npm and
Foundry dependency locks, vendored code, the Rust toolchain, system libraries, CI actions,
release credentials and artifact publication are therefore part of the trusted computing
base. Tests against one build do not establish properties of a differently built release.

The repository currently pins Rust 1.96, keeps cargo-vet and cargo-deny policy, scans
advisories, builds native archives/packages and an image, emits SBOM/signature material and
publishes the MCP npm package. These controls are split across workflows and some security
jobs are advisory.

## Decision

Every release has one immutable `ReleaseManifest` binding:

- source commit/tag and clean-tree state;
- toolchains, targets, compiler/linker flags and build profile;
- Cargo/npm/Foundry/system dependency lock digests and vendored-source revisions;
- generated code, contract ABI/artifact and genesis/deployment-manifest digests;
- each binary, library, archive, package, container and npm artifact digest;
- test/security gates executed against those exact inputs/artifacts;
- per-artifact SBOM, vulnerability/license policy result and exception set; and
- builder identity, build provenance, signatures and publication coordinates.

Release construction uses locked dependencies and ephemeral least-privilege builders.
Third-party CI actions, tool installers, base images, git dependencies and imported
supply-chain attestations are digest/commit pinned and reviewed through an update process.
Release credentials cannot modify source or bypass required verification. Published
artifacts are promoted from verified build outputs; they are not rebuilt by an unrelated
job after gates pass.

The supported artifact matrix is explicit. Omitting a production binary, architecture,
enclave package, contract artifact or operator tool is a deliberate manifest decision, not
an accidental difference between Docker, GoReleaser and local builds. Mock/test features
and keys are forbidden from production artifacts and verified by artifact inspection.

### ReleaseManifest v1 and canonical identity

`release/release-manifest-v1.schema.json` owns the versioned machine contract. Its first
implemented slice binds the exact source commit and required clean tree, release tag,
`SOURCE_DATE_EPOCH`, target/profile/toolchain, digest-pinned builder, immutable Debian
snapshot and direct package versions, reproducibility flags, material input digests and the
five current production ELF subjects. Every artifact records its role, classification,
feature set, install-profile compatibility, media type, length and SHA-256 digest. Network
selection remains delegated to a future signed `NetworkManifest`; an ELF therefore declares
`network-manifest-required` rather than silently claiming compatibility with every network.

The v1 canonical signature subject is `outbe-canonical-json-v1`: UTF-8 JSON, object keys in
Unicode code-point order, no insignificant whitespace, RFC 8259 string escaping with all
non-ASCII code points emitted as lowercase hexadecimal Unicode escapes, integer numeric
fields only, and exactly one trailing LF. Host path, wall-clock build time and output
directory are not manifest identity. `SOURCE_DATE_EPOCH` is the source commit timestamp and
is also the deterministic `vergen` timestamp embedded by `outbe-chain`.

`build-candidate`, `verified` and `revoked` are distinct manifest lifecycle values. The local
ELF builder emits only `build-candidate`: pending verification gates cannot be mistaken for
an authorized release. A later release-gate slice must bind immutable evidence and promote
the exact compared primary output; it must not rebuild it.

### Deterministic Linux x86_64 ELF recipe

`scripts/release/reproducible-build.sh` is the single public recipe for the first slice. It
fails closed for a dirty checkout, an output directory inside the source tree, stale output,
unknown arguments, a mutable builder reference, unlocked dependency resolution, a changed
toolchain/profile/target contract, missing inputs or missing artifacts. It always builds:

1. `outbe-chain`;
2. `outbe-cli`;
3. `outbe-keygen`;
4. `outbe-feeder`; and
5. production `outbe-tee-enclave` without the `mock` feature.

The recipe uses Rust 1.96.0 on a digest-pinned Bookworm image, an immutable Debian snapshot
with exact direct package versions, `cargo build --locked --release`, `LC_ALL=C`, `TZ=UTC`,
fixed source identity supplied to `vergen`, Rust/C/C++ path remapping and an explicit SHA-1
GNU build-id policy. Host-side validation rejects mutable builders before Docker execution.
The source context is an exact `git archive HEAD`; ignored host files and ambient local tags
cannot affect it. An explicit release tag must resolve to `HEAD`, otherwise the exact
`commit-<SHA>` identity supplies both manifest tag and `vergen` description. The source path
inside the builder is fixed, and emitted ELFs are checked for leaked workspace, Cargo and
rustup paths. The builder executes and verifies `outbe-chain --version` against the exact
commit, commit timestamp, profile and target, then preserves that output as evidence. It
preserves the existing `release` profile and panic/LTO/feature semantics. Tempo's separate `panic=abort`
reproducibility profile and unproved `-C metadata=` flag are intentionally not copied.

The current GoReleaser path still builds separately, while `mise run build-release` uses
`cargo-auditable`; neither is evidence for this deterministic output. Recipe v1 therefore
records `cargo.auditable = false` instead of silently producing a third semantic variant.
Release consumption, auditable metadata inclusion,
auditable metadata/SBOM binding and independent CI builders are later P0 slices. Until they
land, this recipe proves local ELF reproducibility only and is not a complete release gate.

Independent verification uses Python 3.11 packages pinned by exact version and wheel hash in
`release/reproducible-verifier-requirements.txt`. Verification recomputes source input and
resolved system-package records, validates the exact output checksum matrix and saved binary
version identity, and compares the five ELF files byte for byte. A matching hash alone does
not excuse a repeated builder-path leak or a mismatched manifest material.

## Dependency and exception policy

Every dependency source is locked and belongs to a reviewed trust class: workspace,
vendored with recorded upstream/revision/diff, registry package, or pinned git source.
`cargo vet`, advisory, license, banned-source/duplicate and npm/Foundry checks are required
for release. An exception records advisory/license id, exact affected versions, reachability
argument, compensating controls, owner and expiry/review trigger. A comment that a path is
unreachable is not permanent proof; a regression test or binary reachability evidence must
support security-relevant exceptions.

## Build, verification and publication state machine

```text
tag candidate
  -> resolve immutable source and dependency graph
  -> build once in declared hermetic profile
  -> test and inspect exact outputs
  -> generate SBOM/provenance/checksums
  -> sign immutable digest set
  -> publish/promote all declared artifacts
  -> verify registry/release downloads against manifest
```

Any failed or missing required gate stops promotion. Retrying reuses the same source and
declared inputs and either reproduces the same digests or creates a new candidate with an
explained environment difference. Revocation publishes a signed statement naming affected
digests; replacement is a new release, never silent artifact mutation.

## Determinism, compatibility and activation

Two clean builders for the same supported target should reproduce bit-identical artifacts
or a documented normalization boundary. Until bit reproducibility is achieved, provenance
must enumerate every material input and a second build must establish semantic equivalence.
The release manifest links the protocol/schema compatibility and activation schedules from
ADR-B-GEN-001 and contract manifests from ADR-B-DEP-001. Operators verify manifest and
signature before ADR-B-OPS-001 rollout.

## Production-interface verification evidence

Inspected `rust-toolchain.toml`, `Cargo.lock`, npm lockfiles, Foundry packages,
`supply-chain/`, `deny.toml`, `audit.toml`, vendored-SMT checks, `Dockerfile`,
`.goreleaser.yaml`, release/prerelease/CVE/MCP/contract workflows and general CI. Current
release emits archives, Linux packages, source archive, SBOM and Sigstore bundle material;
prerelease runs cargo-vet, binary audit and image scanning. The workflows do not yet produce
one manifest proving that all results apply to the same promoted artifacts.

## Consequences

An operator can prove what source and dependency graph produced a running artifact and
which gates it passed. ADR implementation claims can cite an immutable release rather than
an unbound CI run.

## Rejected alternatives

- A signed tag without signed artifact provenance is rejected.
- Passing tests on a separately rebuilt binary is rejected as release evidence.
- Floating CI action tags or container tags are rejected for privileged release paths.
- Permanent unowned advisory ignores are rejected.
- Treating SBOM generation as equivalent to vulnerability/reachability review is rejected.

## Open questions and technical debt

- **Critical, partially closed:** ReleaseManifest v1 now binds source, deterministic builder
  inputs and the five ELF digests. Extend the same canonical manifest with native packages,
  Gramine/SGX subjects, OCI subjects, SBOMs, completed immutable gate evidence,
  deployment/genesis compatibility and signed provenance before release authorization.
- **Critical:** pin third-party GitHub Actions and scanner/tool inputs to immutable commit
  digests. In particular, prerelease currently invokes `aquasecurity/trivy-action@master`;
  major/version tags elsewhere are also mutable supply-chain inputs.
- Promote cargo-deny, cargo-vet and Cargo.lock CVE policy from advisory/partial workflow
  coverage to required release gates. Give every ignored advisory an owner, expiry and
  machine-tested reachability/mitigation claim.
- Align artifact matrices: Docker currently builds only chain/keygen/CLI, GoReleaser adds
  feeder and the enclave binary, and the enclave still needs a separately governed Gramine
  manifest/signing package. Declare and test the complete production set.
- Use `--locked`/frozen resolution in every Docker, CI and package build and pin base image
  digests plus apt/system-library versions or snapshot repositories.
- Add reproducibility builds on independent workers and compare binary, package, contract
  artifact and container digests; record known nondeterministic sections until eliminated.
- Generate per-artifact SPDX/CycloneDX SBOMs covering Rust, npm, Foundry, vendored code,
  system packages and enclave runtime, then verify the published downloads against them.
- Define release-key/OIDC trust, protected environment, approval, revocation and incident
  procedures; minimize PAT/GitHub/npm token scope and prove untrusted PR code cannot access
  publication credentials.
- Bind Solidity compiler/settings and generated precompile ABI/code artifacts to the Rust
  release so address/selector/storage evidence cannot drift between packages.
- Add post-publication verification for GitHub archives/packages, container registry and
  npm provenance, plus an operator command that verifies a downloaded release manifest.
