# Reproducing the Outbe Linux x86_64 ELF set

The first reproducible-release slice builds the five current production ELF files from one
clean commit: `outbe-chain`, `outbe-cli`, `outbe-keygen`, `outbe-feeder` and
`outbe-tee-enclave` without `mock`. It does not yet prove native-package, Gramine, SIGSTRUCT,
OCI-image, SBOM, signature or published-release reproducibility.

## Prerequisites and trust boundary

Install Git, Docker with BuildKit, Python 3.11 (including `venv`), `tar` and `sha256sum`.
Create a verifier environment from the hash-pinned requirements before comparing outputs:

```bash
python3.11 -m venv /tmp/outbe-release-verifier
/tmp/outbe-release-verifier/bin/python -m pip install --require-hashes \
  -r release/reproducible-verifier-requirements.txt
```

The recipe itself pins Rust 1.96.0, the builder image digest, a Debian snapshot and direct
package versions in `release/reproducible-elf-build-v1.json`. The host validates those
constraints before Docker can select or execute the builder. It accepts only
`x86_64-unknown-linux-gnu` with the existing Cargo `release` profile.

Run from a clean checkout. Commit or deliberately discard every tracked and untracked
change first. The output directory must be empty and outside the repository; this prevents
an old or partially copied artifact from entering the proof.

The default release identity is `commit-<full-sha>` and does not consult local Git tags. An
explicit `--release-tag` is accepted only when that exact tag resolves to `HEAD`; the same
value supplies the embedded deterministic Git description and the manifest release tag.
The Docker context is extracted with `git archive HEAD`, so ignored host files cannot enter
the build even when they exist beside the clean checkout.

## One build

```bash
scripts/release/reproducible-build.sh \
  --output /tmp/outbe-rebuild-a \
  --release-tag "commit-$(git rev-parse HEAD)"
```

The output contains:

- `bin/` — the five production ELF files;
- `release-manifest.json` — canonical `build-candidate` ReleaseManifest v1;
- `metadata/builder-system-packages.txt` — the fully resolved Debian package inventory;
- `metadata/outbe-chain.version.txt` — automatically checked commit/time/profile/target output;
- the exact schema and build spec; and
- `SHA256SUMS` — checksums for all emitted files except the checksum file itself.

The command verifies `SHA256SUMS` before returning. A candidate manifest is not a signed or
publishable release: its independent-rebuild and schema gates remain `pending` until an
external comparison records evidence.

## Independent local comparison

Use two empty output directories and force both compilation steps to bypass Docker's build
cache:

```bash
scripts/release/reproducible-build.sh \
  --output /tmp/outbe-rebuild-a \
  --release-tag "commit-$(git rev-parse HEAD)" \
  --no-cache

scripts/release/reproducible-build.sh \
  --output /tmp/outbe-rebuild-b \
  --release-tag "commit-$(git rev-parse HEAD)" \
  --no-cache

/tmp/outbe-release-verifier/bin/python scripts/release/verify_reproducible_elf.py \
  --first /tmp/outbe-rebuild-a \
  --second /tmp/outbe-rebuild-b \
  --output /tmp/outbe-reproducibility-evidence.json
```

The verifier requires the exact hash-pinned Python package versions, validates both manifests
against the checked-in Draft 2020-12 schema, checks their canonical bytes, requires the exact
five-ELF matrix, and independently verifies the source-input, resolved-package, metadata and
ELF digest/size records. It also checks the exact output checksum matrix, saved version
identity, ELF magic and leaked builder paths before comparing each ELF byte for byte. It
writes evidence even on failure and exits non-zero when any difference exists.

Confirm the embedded build identity independently:

```bash
/tmp/outbe-rebuild-a/bin/outbe-chain --version
```

The output must name the manifest commit, `release` profile,
`x86_64-unknown-linux-gnu` target and a build timestamp derived from the manifest's
`SOURCE_DATE_EPOCH`, not the wall clock of either rebuild.

Recipe v1 explicitly records `cargo.auditable = false`: this matches the current GoReleaser
ELF semantics and prevents an undeclared metadata difference. Integrating cargo-auditable
into both the sole production recipe and the published release remains a later P0 decision;
it must not be added to only one of those paths.

## Mismatch diagnosis

Do not change product code from the first mismatch. Preserve both output directories and
the failed evidence, then classify the difference:

```bash
sha256sum /tmp/outbe-rebuild-{a,b}/bin/*
readelf -n /tmp/outbe-rebuild-a/bin/outbe-chain
readelf -n /tmp/outbe-rebuild-b/bin/outbe-chain
cmp -l /tmp/outbe-rebuild-a/bin/outbe-chain \
       /tmp/outbe-rebuild-b/bin/outbe-chain | head
```

If installed, run `diffoscope` on the named ELF. Compare source commit, release tag,
`SOURCE_DATE_EPOCH`, builder image, Debian snapshot, resolved package inventory, Rust flags
and both canonical manifests. Establish whether the state is reachable in the supported
recipe and identify the byte-producing input before proposing a fix.

## Current residual work

This local proof is deliberately narrower than a full release. Independent CI builders,
exact consumption by the release workflow, deterministic native packages, the self-contained
Gramine production bundle and signed enclave measurements, two signed test-only OCI images,
SBOM/provenance, publication verification and authorization remain separate P0 slices.
Docker signing, TUF, `outbeup` and the operator sidecar are not part of this slice.
