# P0 reproducible release ELF evidence — 2026-07-21

## Executive verdict

**PASS for the authorized narrow slice.** At source commit
`3f5e77a3265cfcd7a75b64b47fb2601a53347776`, two clean, no-cache local
container builds produced byte-for-byte identical Linux x86_64 release ELF files for all
five current release binaries. The independent verifier also confirmed canonical manifest
identity, exact inputs and resolved package inventory, embedded version identity, ELF
format, expected output matrix and absence of forbidden host/build paths.

This is not full P0 completion and is not release authorization. It does not prove native
packages, Gramine/SGX packaging, OCI images or signatures, SBOM/provenance integration,
independent CI builders, GoReleaser consumption, TUF, installer or operator sidecar work.
The generated manifest deliberately remains `build-candidate` with pending release gates.

## Fixed identities

| Subject | Identity |
| --- | --- |
| Outbe baseline before this slice | `01b638a878095ed60d4b77c1fb19125959b6ef8e` |
| Artifact-under-test commit | `3f5e77a3265cfcd7a75b64b47fb2601a53347776` |
| Tempo reference commit | `1930c44509cbb3b546b57f682bdbee763ffea910` |
| Builder | `rust:1.96.0-bookworm@sha256:64d9b7f60e3abb08d477cad983d0a3743acc53a19369ba4482510184c9c807e5` |
| Debian snapshot | `20260501T000000Z` |
| Target/profile | `x86_64-unknown-linux-gnu` / existing `release` profile |
| Source epoch | `1784646360` (`2026-07-21T15:06:00Z`) |
| Candidate identity | `commit-3f5e77a3265cfcd7a75b64b47fb2601a53347776` |

The evidence/report commit follows the tested commit and therefore is not itself the
artifact-under-test. It changes documentation only.

## Implemented slice

| Requirement | Implementation | Verdict |
| --- | --- | --- |
| Versioned release contract | `release/release-manifest-v1.schema.json` | PASS |
| One deterministic recipe | `scripts/release/reproducible-build.sh` plus `Dockerfile.reproducible` | PASS |
| Exact source tree | clean `git archive HEAD`; output forbidden inside repository | PASS |
| Immutable build environment | digest-pinned Rust image, Debian snapshot and exact direct package versions | PASS |
| Deterministic identity | commit-derived tag/description and commit `SOURCE_DATE_EPOCH` | PASS |
| Current release ELF matrix | chain, CLI, keygen, feeder and non-mock TEE enclave | PASS |
| Artifact inspection | checksums, ELF magic, version identity and forbidden path scan | PASS |
| Independent comparison | pinned verifier environment and byte comparison of two builds | PASS |

## Commands and results

Build A:

```text
BUILDKIT_PROGRESS=plain scripts/release/reproducible-build.sh \
  --output /tmp/outbe-repro-3f5e77a-a --no-cache
```

Result: PASS; BuildKit build stage `512.5s` (approximately 8m33s).

Build B:

```text
BUILDKIT_PROGRESS=plain scripts/release/reproducible-build.sh \
  --output /tmp/outbe-repro-3f5e77a-b --no-cache
```

Result: PASS; BuildKit build stage `525.9s` (approximately 8m46s).

Independent verification:

```text
python3 scripts/release/verify_reproducible_elf.py \
  --first /tmp/outbe-repro-3f5e77a-a \
  --second /tmp/outbe-repro-3f5e77a-b \
  --output /tmp/outbe-repro-3f5e77a-evidence.json
```

Result: PASS; `artifact_count=5`, `differences=[]`. `cmp` also confirmed identical
`SHA256SUMS`. Both canonical manifests hash to
`732106a03789fdab8a6d507697199717bcf65d6396363862bafb02c45bfc1a66`.
The verifier evidence hashes to
`9e62d23faa1210fc9fc9890795945f973233079ba07a31b43003df699cf09daf`.

## Artifact results

| ELF | Size | SHA-256 A = B |
| --- | ---: | --- |
| `outbe-chain` | 190,053,560 | `e82c01bf7f8ff00664b131378013af73df4bb118bcc413e1d32a1865934afe20` |
| `outbe-cli` | 10,987,928 | `713bd47ceb455f5e252dc5e235a8aae4ac8a55cc36af446a227bd95709f9f9a2` |
| `outbe-keygen` | 3,872,984 | `1b4ee328eea9cbc8aa1de163fe9ad1443f88a515b1c65fb51362c2855d063d75` |
| `outbe-feeder` | 17,148,176 | `75c17d1395ac020d5b4419bfd0848a2265aa5656329a63e51932505600fd39c6` |
| `outbe-tee-enclave` | 4,359,920 | `8707573f5fcdf559957e3f1a8ff93d8626ae41fe7ef5d258e2cfae1dab2fc472` |

## Defect triage retained as evidence

The first comparison at `cd0c0f87aa1631798d9f6392c594d4756fe05520` produced identical
ELFs but correctly failed inspection because `/usr/local/cargo` leaked into `outbe-chain`,
`outbe-keygen` and `outbe-tee-enclave`. The observed state was reachable and repeated in
both builders. Root cause was an incomplete remap contract: the recipe covered the registry
prefix but omitted Cargo git checkouts. Commit `b43af4209886ad5f9663d9a6f4659feebdc9ab9b`
added the git-source remap and a focused regression test. Counterfactual: without this fix,
builders using a different Cargo home could emit different debug/provenance strings even
when two identical container paths happened to compare equal.

A subsequent audit stopped the next expensive build before completion and found additional
release-tooling assumptions: ambient tags could affect `vergen`, builder validation happened
after Docker start, verifier dependencies were not fully pinned, material claims exceeded
the actual checks, version evidence was not asserted automatically and ignored files could
enter `COPY . .`. Commit `3f5e77a3265cfcd7a75b64b47fb2601a53347776` bound or removed those
inputs and added focused tests before the final two-build proof.

No runtime consensus, EVM, protocol or business-logic defect was inferred or changed during
this slice.

## Test and review evidence

```text
python3 -m unittest discover -s scripts/release/tests -p 'test_*.py'
# 17 tests passed

bash -n scripts/release/build-elfs-in-container.sh \
  scripts/release/reproducible-build.sh

python3 -m py_compile scripts/release/*.py scripts/release/tests/*.py
git diff --check
```

After the fixes, independent Standards and Spec reviews reported no actionable findings for
the authorized slice. Their explicit limitation was the same as this report: acceptance
evidence and reporting were still pending at review time and are supplied here.

## Tempo reference assessment

| Tempo observation | Outbe disposition in this slice |
| --- | --- |
| Reproducibility profile differs from the published max-performance profile | Avoided: Outbe preserves its existing release semantics |
| Canary is not an independently rebuilt, promoted release artifact | Partially addressed locally with two clean outputs; CI promotion remains open |
| Installer executes mutable bootstrap/self-update code | Out of scope; must be addressed before installer work |
| Transaction latency mixes clocks and lacks finality/reconnect semantics | Out of scope for P1 sidecar |
| Synthetic load has unsafe defaults and weak production guards | Out of scope for P1 sidecar |

## Residual risk and next slices

1. Make release CI consume one compared primary output from independent workers; do not
   rebuild after verification.
2. Decide and bind `cargo-auditable` metadata consistently with per-artifact SBOMs.
3. Extend the artifact matrix to native packages, Gramine manifests and SGX packages.
4. Build and sign reproducible OCI images, then verify signatures and provenance by digest.
5. Bind test/security gate evidence, contracts, genesis and network manifests before
   promoting `build-candidate` to an authorized release.
6. Only afterward proceed to TUF/verified installer and the separately designed operator
   sidecar for validator/full-node/price-oracle profiles.

## Preserved evidence

- `docs/reports/evidence/p0-reproducible-elf-3f5e77a.json`: final independent PASS.
- `docs/reports/evidence/p0-reproducible-elf-3f5e77a.SHA256SUMS`: final output matrix.
- `docs/reports/evidence/p0-reproducible-elf-3f5e77a.version.txt`: embedded version output.
- `docs/reports/evidence/p0-reproducible-elf-cd0c0f8-initial-failure.json`: retained initial
  path-leak failure.

The 226 MB binary directories remain outside Git under `/tmp`; their digests and all
material verification results needed for this verdict are preserved above.
