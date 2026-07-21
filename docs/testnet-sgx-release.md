# Testnet SGX release and rollout

This guide releases the same enclave bytes that were independently reproduced, signs the
Gramine bundle with the protected testnet key, publishes an immutable OCI image, executes
that exact digest on hardware SGX and emits a signed `ReleaseManifest.json`.

It is a testnet process. The current Gramine contract uses local SGX measurements and
EGETKEY sealing with `sgx.remote_attestation = "none"`; it does not claim DCAP quote or
Intel collateral verification.

## One-time repository setup

Create the GitHub Environment `testnet-release`, apply the desired reviewer/deployment
protection, and store the base64-encoded PEM only as its environment secret:

```bash
gh secret set TESTNET_SGX_SIGNING_KEY_B64 \
  --repo outbe/outbe-chain \
  --env testnet-release < testnet-sgx-key.pem.b64

gh secret list --repo outbe/outbe-chain --env testnet-release
```

The private key is not needed on an SGX machine. SGX hardware is required to execute the
enclave, not to create its RSA SIGSTRUCT. Keep the original PEM outside the repository and
GitHub logs. The workflow's signing job is the only consumer.

Register a self-hosted x86_64 GitHub runner with labels `self-hosted` and `sgx`. It needs
Docker access and these device nodes (legacy `/dev/sgx/...` aliases are also accepted):

```bash
ls -l /dev/sgx_enclave /dev/sgx_provision
docker version
```

## Cut and run a release

The workflow is manual and must be dispatched from `main`. The input tag must already
exist, match `vX.Y.Z-testnet.N`, point to the same `main` commit and remain immutable:

```bash
git switch main
git pull --ff-only
git status --short                    # must be empty
git tag -s v0.2.0-testnet.1 -m 'testnet v0.2.0-testnet.1'
git push origin v0.2.0-testnet.1

gh workflow run testnet-release.yml \
  --repo outbe/outbe-chain \
  --ref main \
  -f release_tag=v0.2.0-testnet.1
```

Follow it with:

```bash
gh run list --repo outbe/outbe-chain --workflow testnet-release.yml --limit 5
gh run watch --repo outbe/outbe-chain <run-id> --exit-status
```

The ordered gates are two independent ELF builds, two unsigned SGX bundle builds,
protected SIGSTRUCT signing, exact-digest OCI publication and Cosign signing, SPDX SBOM,
hardware-SGX Rust/Gherkin acceptance, final schema validation and ReleaseManifest signing.
There is no successful release asset if the SGX runner does not pass.

## Verify before rollout

Download the release assets and verify the signed manifest bundle with Cosign. Verify the
OCI image separately by digest; do not deploy the mutable tag:

```bash
TAG=v0.2.0-testnet.1
mkdir -p "/tmp/outbe-${TAG}"
gh release download "$TAG" --repo outbe/outbe-chain --dir "/tmp/outbe-${TAG}"

cosign verify-blob \
  --bundle "/tmp/outbe-${TAG}/ReleaseManifest.sigstore.json" \
  --certificate-identity-regexp \
    '^https://github.com/outbe/outbe-chain/.github/workflows/testnet-release.yml@refs/heads/main$' \
  --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
  "/tmp/outbe-${TAG}/ReleaseManifest.json"

IMAGE_DIGEST=$(jq -r .image.digest.value "/tmp/outbe-${TAG}/oci-evidence.json")
IMAGE="ghcr.io/outbe/outbe-tee-enclave-testnet@sha256:${IMAGE_DIGEST}"
cosign verify \
  --certificate-identity-regexp \
    '^https://github.com/outbe/outbe-chain/.github/workflows/testnet-release.yml@refs/heads/main$' \
  --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
  "$IMAGE"
```

Inspect the `verified` lifecycle and compare the expected measurements before touching a
running node:

```bash
jq '{lifecycle:.release.lifecycle, artifacts:[.artifacts[] |
  select(.tee.stage == "signed") |
  {name, digest, tee}]}' "/tmp/outbe-${TAG}/ReleaseManifest.json"
```

## Upgrade policy

For a normal same-signer upgrade:

1. Create and pass the complete new release; never sign a locally modified bundle.
2. Require the same MRSIGNER, a strictly non-decreasing ISVSVN and the reviewed new
   MRENCLAVE.
3. Before rollout, authorize an overlap containing old and new MRENCLAVE in the network's
   measurement policy. Current testnet measurement enforcement is incomplete, so operators
   must also compare the manifest and hardware evidence explicitly; this guide does not
   turn the existing registration stub into a security claim.
4. Roll the exact image digest through full nodes and validators. Both roles require an
   enclave on an offer-bearing network because both re-execute Tribute transactions.
5. Verify same-signer sealed-state restoration and normal node readiness after each cohort.
6. Retire the old MRENCLAVE only after every required node has moved and rollback policy is
   closed.

A different MRSIGNER does not silently inherit old sealed identity. Treat signer rotation
as a separate governed migration: prepare an authenticated handoff or rebootstrap, approve
the new authority and release identity, and explicitly coordinate activation. Restarting
containers is not a signer-rotation procedure.

## Local tooling

`mise` is the human-facing command catalogue; `cargo xtask` owns typed release logic:

```bash
mise run release-sgx-prepare -- \
  --elf-output /tmp/outbe-elf-a --output /tmp/outbe-sgx-a
mise run release-sgx-compare -- \
  --first /tmp/outbe-sgx-a --second /tmp/outbe-sgx-b \
  --output /tmp/outbe-sgx-reproducibility.json
mise run release-sgx-sign -- \
  --unsigned /tmp/outbe-sgx-a --key-file /secure/testnet-sgx-key.pem \
  --output /tmp/outbe-sgx-signed
mise run release-sgx-verify -- --bundle /tmp/outbe-sgx-signed
mise run release-sgx-archive -- \
  --bundle /tmp/outbe-sgx-signed --output /tmp/outbe-tee-enclave-sgx.tar
```

Local signing is for controlled diagnosis only. The authoritative testnet release must use
the protected workflow so the key boundary, OCI signature, hardware evidence and final
manifest are all recorded together.

The ordinary `Dockerfile.test`/`mise run e2e-sgx` path deliberately uses a scenario-scoped
test key and runtime test signing. It cannot authorize a testnet release.

On an SGX host, the exact-artifact acceptance command is also exposed through `mise`:

```bash
mise run release-sgx-hardware-e2e -- \
  --image "$IMAGE" \
  --bundle /tmp/extracted-signed-sgx-bundle \
  --evidence /tmp/hardware-sgx.json
```
