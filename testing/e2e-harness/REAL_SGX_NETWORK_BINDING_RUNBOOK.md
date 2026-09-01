# Real-SGX network-binding downgrade E2E

This scenario is intentionally compiled locally but executed only on a separate
Linux x86_64 machine with SGX, Gramine and DCAP configured. It launches a real
four-validator DcapRequired network, performs finalized Validator and FullNode
onboarding, and writes persistent scenario evidence.

## Signing-key boundary

- The key supplied to this harness is a disposable test key.
- Mainnet and public-testnet release keys must be separate offline trust roots.
- A release key must never sign a manifest with remote attestation disabled and
  must never be copied to a development or E2E host.
- A development/test signer must not appear in any `TeePolicyV1` measurement
  rule for mainnet or public testnet.
- Rotate `ISVSVN` when deploying this sealed-state schema and reject lower SVN
  in the network policy.

Signer separation is an operational trust-root requirement: possession of a
production SGX signing key permits an attacker to create another enclave with
the same `MRSIGNER`, so repository code cannot compensate for key compromise.
The downgrade scenario nevertheless uses one disposable signer for both DCAP
and no-attestation manifests. The release binary must reject no-attestation at
its compile-time production gate before sealed state is opened; unit tests also
exercise the independent `NetworkBindingV1` sealed-state rejection.

## Build

```sh
cargo build --release -p outbe-tee-enclave --features production-dcap-release
cargo build --release \
  -p outbe-chain \
  -p outbe-ocomp \
  -p outbe-feeder \
  -p outbe-cli \
  -p outbe-keygen
cargo build --release -p outbe-e2e-harness \
  --features ocomp-integration \
  --bin outbe-e2e
cargo build --release -p outbe-e2e-harness \
  --features release-sgx-e2e \
  --bin outbe-release-sgx-e2e
```

## Run on the SGX machine

```sh
cargo run --release -p outbe-e2e-harness \
  --features ocomp-integration \
  --bin outbe-e2e -- \
  --tee real \
  --validators 4 \
  --no-cleanup \
  --evidence-dir /secure/evidence/network-binding \
  --name 'Validator and FullNode survive restart after production onboarding'
```

The machine must provide `/dev/sgx_enclave`, `/dev/sgx_provision`, Gramine 1.9,
Intel DCAP quote generation/verification and network access for fresh Intel PCS
collateral capture.

After the network scenario passes, validate the exact published release image
and its signed bundle separately:

```sh
cargo run --release -p outbe-e2e-harness \
  --features release-sgx-e2e \
  --bin outbe-release-sgx-e2e -- \
  --network testnet \
  --image 'REGISTRY/IMAGE@sha256:DIGEST' \
  --bundle /secure/release/signed-sgx-bundle \
  --genesis /secure/release/testnet-genesis.json \
  --evidence /secure/evidence/release-sgx.json
```

## Required evidence

The retained `scenario-*.json` must report `result: passed`, `tee:
gramine-sgx`, the exact binary hashes, and a scenario data directory containing
the node/enclave logs. The scenario must prove:

- the approved seeded `genesis.json` determines the measured network identity
  and epoch-0 MinPk committee; the final release genesis is that exact seed plus
  the generated signed-enclave policy schedule;
- onboarding activation consumes a one-shot committee-transition/finality proof
  plus the exact `TeeRegistry` MPT opening;
- a same-test-`MRSIGNER` SGX no-attestation runtime cannot reopen the Validator's
  DCAP-bound sealed state or expose the permanent offer key; retain
  `enclave-no-attest-downgrade.log` as the negative evidence;
- Validator and FullNode onboarding, sealed restart and manual renewal preserve
  the exact permanent offer public key.

Also retain the private work directory named in stdout. The evidence
is valid only for the source commit, enclave binary, SGX signer and collateral
captured by that run; do not reuse it for another release.

## Source-surface gate

Run this in the checked-out source tree used for the build; it must print
nothing:

```sh
rg -n 'SealOfferKeyForRegistry|IngestSealedOfferKeyForRegistry|SealedOfferKeyForRegistry|seal_offer_key_for_registry' \
  bin crates testing --glob '*.rs'
```

The real-SGX harness does not expose a raw group-secret request. Onboarding is
purpose-bound to the recipient enclave, network, intent and key epochs.
