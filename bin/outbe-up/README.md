# outbe-up

`outbe-up` creates and operates a founding network, then prepares and promotes additional joining
validators. Bundles carry the matching `outbe-up` binary; systemd units and service helper logic
are compiled from the shared Rust implementation so they cannot drift from the lifecycle code.

## Founding network

Build the immutable software bundle once with `cargo xtask release build`. The network
commands consume that bundle; they never compile source code.

```text
outbe-up network init --output network.yaml
outbe-up network build \
  --software-bundle outbe-software-bundle-....tar.zst \
  --config network.yaml \
  --output network-build

outbe-up network prepare --artifacts network-build [--ssh-key ~/.ssh/operator]
outbe-up network start --artifacts network-build [--ssh-key ~/.ssh/operator]
outbe-up network status --artifacts network-build [--ssh-key ~/.ssh/operator] [--watch]
```

`build` produces one fresh genesis, one founder identity per configured host, host-specific
founding archives, and a joining-validator bundle/config from the same software and genesis.
`prepare` verifies and installs every host over SSH without starting it. `start` starts all
founders and waits for RPC, execution peers, consensus signer state, threshold shares, and a
finalized block. It then delegates and funds each dedicated Hyperlane signer, registers it with the
controller, and waits for every Hyperlane validator to become healthy. Repeating `prepare` or
`start` for the same artifacts is safe.

`network.yaml` keeps SSH endpoints separate from node advertise/RPC addresses, supports four or
more founders, prefunded accounts and explicit genesis overrides. `genesis.base` may reference a
custom pre-TEE genesis JSON; `build` verifies its chain ID, copies it into the immutable artifacts,
then applies the fresh founder set and network policy. Private consensus topologies must opt into
`topology.use_local_defaults`; it is never inferred from the network name.

Each founder needs a public checkpoint URL in `hyperlane_public_url`; TCP port `9000` must be
reachable by Hyperlane relayers. The private health endpoint stays on `127.0.0.1:9003`. Shared
origin-chain RPC and reorg settings live in the top-level `hyperlane` section.

## Joining validator

```text
sudo outbe-up validator prepare --bundle <operator-bundle.tar.zst> --config <operator-config.yaml>
sudo outbe-up validator sync [--snapshot <snapshot.tar.zst>]
sudo outbe-up validator promote
sudo outbe-up validator status [--watch]
```

Run `outbe-up validator config-example --output operator-config.yaml` to write the configuration template
embedded in that exact binary version.

`prepare` verifies the config, network, manifest, every bundled file, and the bootstrap binary. It
then creates host-local identities and installs services without starting them. `sync` optionally
restores a verified snapshot and synchronizes a keyless full node. `promote` performs network
admission, bonds the network minimum stake, switches the same data directory to validator mode,
delegates and funds its dedicated Hyperlane signer, and submits validator readiness. DKG activation
continues in the background; once the validator becomes active, its service registers the signer
and starts the Hyperlane daemon automatically. Both consensus and Hyperlane health are visible
through `status`.

The bundle carries `outbe-tee-enclave-confidential-vm`, an unattested signer service intended to
run inside an operator-provided Intel TDX or AMD SEV-SNP VM. Hardware attestation is not implemented
yet, so the deployment currently trusts the operator and confidential-computing infrastructure.

The editable configuration example lives at `templates/validator/operator-config.example.yaml`.
Systemd policy comes from the shared `outbe-operator-systemd` crate used by both joining and
founding flows; network-specific values remain in the generated `/opt/outbe-chain/validator.env`.
All persistent validator data, keys, lifecycle state, Hyperlane state, and canonical systemd unit
files live under `/opt/outbe-chain`, so one persistent mount covers the complete installation.
`/etc/systemd/system` contains only links to those units, `/run` remains ephemeral, and
`/var/lib/outbe/tee` is only the enclave service's runtime bind alias for the TEE data under `/opt`.
