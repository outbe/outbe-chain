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

Both profiles run TEE logic inside `outbe-chain`; neither ships an enclave executable or a
separate enclave service. `production` enables the pinned native DCAP verifier and fails the build
if the required QVL is unavailable. A host running the production node must provide the
pinned Intel QVL shared library listed in `release/dcap-native-qvl-v1.json`.
`development` enables protocol overrides for hardware-free
local networks. Both builds run inside the pinned project-toolchain container and require a clean
Git checkout. The builder downloads the canonical CRS prefix and verifies its pinned SHA-256.

The output name includes the profile and full source commit. `software-manifest.json` is the first
archive entry and records the release identity, target, profile, mode, size, and SHA-256 of every
file. The build verifies the completed archive before returning it.

## Contents

- `bin/`: node, operator CLI and installer, key generator, feeder, Radicle, and Hyperlane validator;
- `tools/outbe-genesis-cli`: the founding-network bundle generator;
- `.bb-crs/bn254_g1.dat`: the verified CRS required by every node;
- `systemd/`: canonical joining-validator services rendered by `outbe-operator-systemd`.

The Hyperlane validator is payload only in this phase: no Hyperlane unit, configuration, or startup
automation is installed.

Founding-validator launch bundles are generated later from this exact software set plus one fresh
genesis and network configuration. Joining and founding units use the same templates and policy;
their compact specifications contain only the commands, dependencies, and paths that must differ.

## CI

`17. Software bundle — preflight` builds either profile and uploads the verified bundle without
publishing it. Standard version tags build the production profile, create `SHA256SUMS`, sign the
checksum file with Sigstore, and publish those artifacts to the GitHub release.

Protected testnet and mainnet tags publish separately. Two builders reproduce the ELF archive,
then the network's protected workflow signs the checksums for the archive and comparison evidence.
These releases do not publish an enclave image, SGX evidence, or a genesis.
