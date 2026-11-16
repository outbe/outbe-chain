# outbe-up

`outbe-up` creates and operates a founding network, then prepares and promotes additional joining
validators. Bundles carry the matching `outbe-up` binary; systemd units and service helper logic
are compiled from the shared Rust implementation so they cannot drift from the lifecycle code.

## Founding network

Build the immutable software bundle once with `cargo xtask release software build`. The network
commands consume that bundle; they never compile source code.

```text
outbe-up network init --output network.yaml
outbe-up network build \
  --software-bundle outbe-software-bundle-....tar.zst \
  --config network.yaml \
  --output network-build

# Only tee.policy=dcap-required has this protected-signing boundary:
outbe-up network finalize \
  --build network-build \
  --signed-sgx signed-sgx-bundle \
  --output network-final

outbe-up network prepare --build network-build [--identity ~/.ssh/operator]
outbe-up network start --build network-build [--identity ~/.ssh/operator]
outbe-up network status --build network-build [--watch]
```

For DCAP, use `network-final` in the last three commands. `build` produces one fresh genesis, one
founder identity per configured host, host-specific founding archives, and a joining-validator
bundle/config from the same software and genesis. `prepare` verifies and installs every host over
SSH without starting it. `start` starts all founders and waits for RPC, execution peers, consensus
signer state, threshold shares, and a finalized block. Repeating `prepare` or `start` for the same
build is safe.

A DCAP build also writes `sgx-signing-request.json`. Upload its pinned seeded genesis to the
approved HTTPS location, run the existing protected testnet/mainnet SGX workflow with the emitted
release tag and SHA-256, extract its signed SGX artifact, and pass that directory to `finalize`.
`runtime: system-gramine` requires the pinned Gramine installation on every host;
`runtime: bundled-sgx` uses the signed self-contained runtime instead.

`network.yaml` keeps SSH endpoints separate from node advertise/RPC addresses, supports four or
more founders, prefunded accounts and explicit genesis overrides. Private consensus topologies
must opt into `topology.use_local_defaults`; it is never inferred from the network name.

## Joining validator

```text
sudo outbe-up validator prepare --bundle <operator-bundle.tar.zst> --config <operator-config.yaml>
sudo outbe-up validator sync [--snapshot <snapshot.tar.zst>]
sudo outbe-up validator promote
sudo outbe-up validator status [--watch]
```

Run `outbe-up validator config-example` to print the configuration template embedded in that exact
binary version.

`prepare` verifies the config, network, manifest, every bundled file, and the bootstrap binary. It
then creates host-local identities and installs services without starting them. `sync` optionally
restores a verified snapshot and synchronizes a keyless full node. `promote` performs TEE admission,
bonds the network minimum stake, switches the same data directory to validator mode, and submits
validator readiness. DKG activation continues in the background and is visible through `status`.

For hardware-free dev/test networks created with a `GramineDirectDev` genesis TEE policy, set
`tee.runtime: mock`. The operator bundle must then carry
`outbe-tee-enclave-mock`; no SGX hardware, AESM, DCAP, Gramine, or Docker is required. This mode is
unattested and must never be used for production or mainnet.

The editable configuration example lives at `templates/validator/operator-config.example.yaml`.
Systemd policy comes from the shared `outbe-operator-systemd` crate used by both joining and
founding flows; network-specific values remain in the generated `/opt/outbe-chain/validator.env`.
