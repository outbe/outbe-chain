# Outbe software bundle

The software bundle is the deterministic, network-neutral input for Outbe network and validator
deployment flows. It contains the Linux binaries and canonical validator systemd assets built from
one exact source commit. It does not contain a genesis, validator keys, network addresses, or other
deployment-specific configuration.

## Build

Run the same command locally and in CI:

```bash
cargo xtask release build --profile production --output /tmp/outbe-release
cargo xtask release build --profile development --output /tmp/outbe-release-dev
```

`production` builds the DCAP-capable enclave and rejects mock TEE support. `development` builds the
mock enclave and enables the protocol overrides needed by hardware-free local networks. Both builds
run inside the pinned project-toolchain container and require a clean Git checkout.

The output name includes the profile and full source commit. `software-manifest.json` is the first
archive entry and records the release identity, target, profile, mode, size, and SHA-256 of every
file. The build verifies the completed archive before returning it.

## Contents

- `bin/`: node, operator, feeder, Radicle, Hyperlane validator, and profile-specific enclave
  binaries;
- `tools/outbe-genesis-cli`: the founding-network bundle generator;
- `systemd/`: canonical joining-validator services embedded from `outbe-up`.

The Hyperlane validator is payload only in this phase: no Hyperlane unit, configuration, or startup
automation is installed.

Founding-validator launch bundles are generated later from this exact software set plus one fresh
genesis and network configuration. Their non-templated units follow the same users, hardening,
restart policy, paths, and target layout as the joining-validator units.

## CI

`17. Software bundle — preflight` builds either profile and uploads the verified bundle without
publishing it. Standard version tags build the production profile, create `SHA256SUMS`, sign the
checksum file with Sigstore, and publish those artifacts to the GitHub release.

Protected testnet and mainnet SGX release workflows remain separate and unchanged.
