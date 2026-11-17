# Outbe software bundle

The software bundle is the deterministic, network-neutral input for Outbe network and validator
deployment flows. It contains the Linux binaries and canonical validator systemd assets built from
one exact source commit. It does not contain a genesis, validator keys, network addresses, or other
deployment-specific configuration.

## Build

Run the same command locally and in CI:

```bash
cargo xtask release build --output /tmp/outbe-release
```

The build runs inside the pinned project-toolchain container and requires a clean Git checkout.
It produces one bundle for confidential-VM deployments and downloads the canonical CRS prefix,
verifying its pinned SHA-256.

The output name includes the full source commit. `software-manifest.json` is the first archive
entry and records the release identity, target, mode, size, and SHA-256 of every file. The build
verifies the completed archive before returning it.

## Contents

- `bin/`: node, operator, feeder, Radicle, Hyperlane validator, and confidential-VM signer
  binaries;
- `tools/outbe-genesis-cli`: the founding-network bundle generator;
- `.bb-crs/bn254_g1.dat`: the verified CRS required by every node;
- `systemd/`: canonical joining-validator services rendered by `outbe-operator-systemd`.

The Hyperlane validator is payload only in this phase: no Hyperlane unit, configuration, or startup
automation is installed.

Founding-validator launch bundles are generated later from this exact software set plus one fresh
genesis and network configuration. Joining and founding units use the same templates and policy;
their compact specifications contain only the commands, dependencies, and paths that must differ.

## CI

`17. Software bundle — preflight` builds and uploads the verified bundle without publishing it.
Standard version tags create `SHA256SUMS`, sign the checksum file with Sigstore, and publish those
artifacts to the GitHub release.
