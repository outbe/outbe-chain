# Reproducing the Outbe Linux x86_64 ELF set

The first reproducible-release slice builds the five current production ELF files from one
clean commit: `outbe-chain`, `outbe-cli`, `outbe-keygen`, `outbe-feeder` and
`outbe-tee-enclave` without `mock`. It does not yet prove native-package, Gramine, SIGSTRUCT,
OCI-image, SBOM, signature or published-release reproducibility.

## Prerequisites and trust boundary

Install Git, Docker with BuildKit, Python 3 and `sha256sum`. The recipe itself pins Rust
1.96.0, the builder image digest, a Debian snapshot and direct package versions in
`release/reproducible-elf-build-v1.json`. It accepts only
`x86_64-unknown-linux-gnu` with the existing Cargo `release` profile.

Run from a clean checkout. Commit or deliberately discard every tracked and untracked
change first. The output directory must be empty and outside the repository; this prevents
an old or partially copied artifact from entering the proof.

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

python3 scripts/release/verify_reproducible_elf.py \
  --first /tmp/outbe-rebuild-a \
  --second /tmp/outbe-rebuild-b \
  --output /tmp/outbe-reproducibility-evidence.json
```

The verifier validates both manifests against the checked-in Draft 2020-12 schema, checks
their canonical bytes, requires the exact five-ELF matrix, verifies every declared digest
and size, checks ELF magic, rejects leaked builder paths and compares each file byte for
byte. It writes evidence even on failure and exits non-zero when any difference exists.

Confirm the embedded build identity independently:

```bash
/tmp/outbe-rebuild-a/bin/outbe-chain --version
```

The output must name the manifest commit, `release` profile,
`x86_64-unknown-linux-gnu` target and a build timestamp derived from the manifest's
`SOURCE_DATE_EPOCH`, not the wall clock of either rebuild.

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
