# ADR-B-RLS-001: Releases bind reproducible builds, reviewed dependencies and artifact provenance

- **Status:** Proposed; current CI, dependency and release surfaces profiled
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

- **Critical:** create and sign one `ReleaseManifest` joining source, dependency locks,
  exact artifact digests, SBOMs, gate results, deployment/genesis compatibility and builder
  provenance. Current evidence is distributed across workflow logs and release assets.
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
