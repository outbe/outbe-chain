# Testnet SGX/DCAP release and rollout

This guide defines the release mechanics for the same enclave bytes that were
independently reproduced, signed with the protected testnet key, published as an
immutable OCI image and executed on Intel SGX x86_64.

> **DCAP release gate.** The checked-in B1 candidate selects
> `sgx.remote_attestation = "dcap"`, exactly `native-dcap`, Intel QVL
> `1.26.100.1-noble1` and Gramine `1.9`. This source-level activation is not by
> itself authorization to roll out a production-mode genesis: the B1 checkpoint
> must bind the reproducible artifact set, and H1, P1 and E1 must prove fresh
> accepted Processor evidence, same-server exact-release QVL/full-block timing,
> and reachable real Validator/FullNode E2E. A 32-validator network is not a
> release gate. Missing Processor hardware or an accepted
> Processor result blocks release rather than skipping it. A real Platform node
> is verified fail-closed by the same testnet verifier when it joins; it is
> not a dedicated row in every release.

The commands below remain the intended protected publication workflow. Until the
I9 closure checkpoint names the exact signed commit and artifact digests, they are
release-mechanics documentation only, not authorization to publish a testnet
DCAP image.

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

Also configure a repository tag ruleset that blocks update and deletion of
`v*-testnet.*`. The workflow reads the tag through the GitHub Git API, requires an annotated
tag object whose `verification.verified` result is true, records that exact tag-object SHA,
requires the embedded signed tag name to equal the requested release tag, and checks the
same object before every privileged boundary and immediately before publication. The
ruleset closes the remaining check-to-use race at the repository boundary.

Enable GitHub immutable releases once with an administrator token, then confirm the
repository returns `enabled: true`:

```bash
gh api --method PUT \
  -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2026-03-10' \
  repos/outbe/outbe-chain/immutable-releases
gh api \
  -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2026-03-10' \
  repos/outbe/outbe-chain/immutable-releases
```

Release immutability protects the tag and assets after publication. The tag ruleset is
still required before publication because draft releases are intentionally mutable while
the workflow uploads and verifies their complete asset matrix.

Register one dedicated ephemeral x86_64 GitHub runner class. The Processor-CA
host uses only `testnet-release-sgx`; its one job performs both the existing
immutable SGX/sealing acceptance and the fresh Processor-CA DCAP run. Do not
give it the default `self-hosted`, `Linux`, `X64` or generic `sgx` labels: those
labels are shared by CI/nightly jobs and do not isolate the protected release
workload.

The runner needs Docker access and these device nodes (legacy `/dev/sgx/...` aliases are
also accepted):

```bash
ls -l /dev/sgx_enclave /dev/sgx_provision
docker version
```

Configure the pinned runner from its installation directory using a short-lived repository
registration token. `--ephemeral` limits it to one job and `--no-default-labels` prevents it
from consuming unrelated queued work:

```bash
./config.sh --unattended --ephemeral --disableupdate --no-default-labels \
  --url https://github.com/outbe/outbe-chain \
  --token "$RUNNER_REGISTRATION_TOKEN" \
  --name "$(hostname)-outbe-testnet-release-sgx" \
  --labels testnet-release-sgx \
  --work _work
```

The host must install `libsgx-dcap-default-qpl` at the exact version in
`release/project-toolchain-v1.json` and provide working QCNL/PCS configuration.
The PCS subscription key remains a host-only acquisition secret: never put it in
workflow arguments, repository files, artifacts or logs. QPL/QCNL is not installed
in the release build image and never participates in consensus verification.

Start this runner only after the protected workflow is ready to advance to hardware
acceptance. Confirm GitHub reports exactly the custom routing label before allowing the
job to start.

## Cut and run a release

Pushing the tag is the trigger: the `dispatch-testnet-release` job in `release.yml` runs on
every `vX.Y.Z-testnet.N` tag and dispatches this workflow with `--ref main`. goreleaser
deliberately skips those tags so this workflow alone creates the GitHub release. The
workflow ref must stay `main` because the Sigstore certificate identity is pinned to
`refs/heads/main` in `xtask/src/release/sgx.rs` and `release/release-manifest-v1.schema.json`.

The tag must be an annotated signed tag, already exist, match `vX.Y.Z-testnet.N`, point to
the same `main` commit and remain immutable:

```bash
git switch main
git pull --ff-only
git status --short                    # must be empty
git tag -s v0.2.0-testnet.1 -m 'testnet v0.2.0-testnet.1'
git push origin v0.2.0-testnet.1      # this starts the protected SGX release
```

Dispatching by hand stays available for a re-run of an existing tag:

```bash
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

The final DCAP-capable workflow must order two independent ELF builds, two
unsigned SGX bundle builds, protected SIGSTRUCT signing, exact-digest OCI
publication and Cosign signing, SPDX SBOM, fail-not-skip hardware SGX/DCAP
acceptance, final schema validation and ReleaseManifest signing. The workflow is
not production-authorized until B1 through E1 are closed.
The OCI job provisions pinned Buildx v0.35.0 and a digest-pinned BuildKit v0.31.2
`docker-container` builder because the default Docker driver cannot emit the required
BuildKit provenance and SBOM attestations.
The first job exports one verified commit SHA and the verified signed tag-object SHA; every
privileged job checks out the commit and rechecks that the authoritative GitHub ref still
names the same tag object. Publication first creates a draft containing the complete asset
matrix, downloads every draft asset and compares it byte-for-byte, then publishes the draft
only after another tag-object check. Publication fails if a GitHub
Release or failed draft already exists for the tag: reruns cannot replace assets, and
changed output requires a new tag.
There is no successful release asset if the Processor/SGX job does not pass.
The final manifest requires canonical `hardware-dcap-processor.json`; its
complete public evidence directory is published as a deterministic tar asset.
Platform admission evidence, when a real Platform node joins, belongs to that
node's admission record and is not substituted into the release manifest.

Besides the SGX evidence, the asset matrix carries the deployable payload:
`outbe-linux-x86_64.tar` (every artifact of `release/reproducible-elf-build-v1.json` —
`outbe-chain`, `outbe-cli`, `outbe-keygen`, `outbe-feeder`, `outbe-tee-enclave`,
`outbe-ocomp` — plus their `SHA256SUMS` and `release-manifest.json`) and
`outbe-deploy-assets.tar` (`deploy/systemd/`). Network-specific node, embedded
OCOMP, SnapshotExporter and Worker units are generated by the launch bundle.

If publication stops after draft creation, leave that draft as evidence and cut a new
testnet tag after diagnosing the failed run. Do not delete assets and reuse the same release
identity.

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
EXPECTED_SHA=$(jq -r .release.source.commit "/tmp/outbe-${TAG}/ReleaseManifest.json")
IMAGE="ghcr.io/outbe/outbe-tee-enclave-testnet@sha256:${IMAGE_DIGEST}"
cosign verify \
  --certificate-identity-regexp \
    '^https://github.com/outbe/outbe-chain/.github/workflows/testnet-release.yml@refs/heads/main$' \
  --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
  --certificate-github-workflow-sha "$EXPECTED_SHA" \
  "$IMAGE"
cosign verify-attestation --type spdxjson \
  --certificate-identity-regexp \
    '^https://github.com/outbe/outbe-chain/.github/workflows/testnet-release.yml@refs/heads/main$' \
  --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
  --certificate-github-workflow-sha "$EXPECTED_SHA" \
  "$IMAGE"
cosign verify-attestation --type slsaprovenance02 \
  --certificate-identity-regexp \
    '^https://github.com/outbe/outbe-chain/.github/workflows/testnet-release.yml@refs/heads/main$' \
  --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
  --certificate-github-workflow-sha "$EXPECTED_SHA" \
  "$IMAGE"
```

The Rust finalizer invokes Cosign again with the exact certificate identity, OIDC issuer and
verified workflow SHA; it does not trust uploaded JSON as proof. From that command output it
records the image signature payload and both verified DSSE attestations, requires their
subject to be the exact OCI digest, requires the attested SPDX predicate to equal the
published SBOM, and requires material-bearing BuildKit provenance. Digests of all four
verification documents are recorded by the OCI gate.

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
3. Before rollout, pass the governance proposal whose activation height authorizes
   an overlap containing old and new MRENCLAVE in the V1 policy schedule. Verify the
   canonical policy hash and exact release hardware evidence before that height.
4. Roll the exact image digest through full nodes and validators. Both roles require an
   enclave on an offer-bearing network because both re-execute Tribute transactions.
5. Verify same-signer sealed-state restoration and normal node readiness after each cohort.
6. Retire the old MRENCLAVE only after every required node has moved and rollback policy is
   closed.

A different MRSIGNER does not inherit the old sealed identity or permanent offer key.
There is no handoff, rebootstrap or governance replacement for that identity: it must
onboard as a new identity under an independently approved release policy. Restarting
containers is not a signer-rotation procedure.

## Local tooling

`mise` is the human-facing command catalogue; `cargo xtask` owns typed release logic:

```bash
mise run release-sgx-prepare -- \
  --network testnet \
  --elf-output /tmp/outbe-elf-a --output /tmp/outbe-sgx-a
mise run release-sgx-compare -- \
  --first /tmp/outbe-sgx-a --second /tmp/outbe-sgx-b \
  --output /tmp/outbe-sgx-reproducibility.json
mise run release-sgx-sign -- \
  --network testnet \
  --unsigned /tmp/outbe-sgx-a --key-file /secure/testnet-sgx-key.pem \
  --output /tmp/outbe-sgx-signed
mise run release-sgx-verify -- --network testnet --bundle /tmp/outbe-sgx-signed
mise run release-sgx-archive -- \
  --network testnet \
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
  --network testnet \
  --image "$IMAGE" \
  --bundle /tmp/extracted-signed-sgx-bundle \
  --evidence /tmp/hardware-sgx.json
```

That command is the SGX/sealing smoke only. For fresh accepted remote-attestation
evidence, run the public-verifier path after the image has been verified and pulled:

```bash
cargo run --locked -p outbe-e2e-harness --bin outbe-release-dcap-evidence -- \
  --network testnet \
  --image "$IMAGE" \
  --bundle /tmp/extracted-signed-sgx-bundle \
  --genesis release/testnet-genesis.json \
  --expected-pck-ca processor \
  --output-dir /tmp/hardware-dcap-processor
```

For an on-demand Platform compatibility check, the same command may use
`--expected-pck-ca platform`. Guest-visible topology is recorded only as
provenance; the enclave-verified Intel PCK issuer determines the CA. This
optional diagnostic never substitutes for the real Platform node's admission
and does not gate releases. For the mandatory Processor run, a missing runner,
stale collateral, rejected verdict or missing artifact is a release failure,
never a skip.
