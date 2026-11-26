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

TEE logic runs inside `outbe-chain`; the bundle does not ship an enclave executable or a separate
enclave service. The node uses the explicit no-attestation policy supported by devnet and testnet;
mainnet remains unavailable until confidential-VM attestation is implemented. The build runs inside
the pinned project-toolchain container and requires a clean Git checkout. The builder stages the
vendored CRS from `release/crs/bn254_g1.dat` and verifies its pinned size and SHA-256.

Output names include the full source commit. The shared `software-manifest.json` is the first archive
entry and records the release identity, target, mode, size, and SHA-256 of every file. The build
verifies the completed archive with the same verifier that `outbe-up` uses.

## Contents

- `bin/`: node, operator CLI and installer, key generator, feeder, Radicle, and Hyperlane validator;
- `tools/outbe-genesis-cli`: the founding-network bundle generator;
- `.bb-crs/bn254_g1.dat`: the verified CRS required by every node;
- `systemd/`: canonical joining-validator services rendered by `outbe-operator-systemd`.

The lifecycle flow gives every validator a dedicated Hyperlane key, renders its public configuration,
keeps the private key in the runtime-only configuration, and installs the canonical Hyperlane service.
Network-specific origins and public checkpoint URLs are supplied later through `network.yaml` or the
joining-validator operator config.

Founding-validator launch bundles are generated later from this exact software set plus one fresh
genesis and network configuration. Joining and founding units use the same templates and policy;
their compact specifications contain only the commands, dependencies, and paths that must differ.
Installers keep every persistent artifact under `/opt/outbe-chain`, including service state and the
canonical unit files. They link those units into `/etc/systemd/system`; runtime-only configuration
continues to use `/run`.

## CI

`17. Software bundle — preflight` builds and uploads the verified bundle without publishing it.
Version tags create `SHA256SUMS`, sign the checksum file with Sigstore, and publish those artifacts
to the GitHub release.

The separate ELF build remains available through `release/scripts/reproducible-build.sh`. The
software release workflow does not publish ELF comparison evidence, enclave images, or a genesis.
