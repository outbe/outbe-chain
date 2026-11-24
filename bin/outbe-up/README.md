# outbe-up

`outbe-up` creates and operates a founding network, then prepares and promotes additional joining
validators. Bundles carry the matching `outbe-up` binary; systemd units and service helper logic
are compiled from the shared Rust implementation so they cannot drift from the lifecycle code.

## Founding network

Build the immutable software bundle once with `cargo xtask release build`. The network
commands consume that bundle; they never compile source code.

```text
outbe-up network config-example --output network.yaml
outbe-up network build \
  --software-bundle outbe-software-bundle-....tar.zst \
  --config network.yaml \
  --output network-artifacts

outbe-up network prepare --artifacts network-artifacts [--ssh-key ~/.ssh/operator]
outbe-up network start --artifacts network-artifacts [--ssh-key ~/.ssh/operator]
outbe-up network status --artifacts network-artifacts [--ssh-key ~/.ssh/operator] [--watch]
```

`build` produces one fresh genesis, one founder identity per configured host, host-specific
founding archives, and a joining-validator bundle/config from the same software and genesis.
`prepare` verifies and installs every host over SSH without starting it. `start` starts all
founders and waits for RPC, execution peers, consensus signer state, threshold shares, resident
TEE readiness, a healthy feeder, and a finalized block. It then delegates and funds each dedicated
Hyperlane signer, registers it with the controller, and waits for every Hyperlane validator to
become healthy.
`prepare` refuses to reinstall over an active validator target; `start` can be repeated for the
same artifacts.

`network.yaml` keeps SSH endpoints separate from node advertise/RPC addresses, supports four or
more founders, prefunded accounts and explicit genesis overrides. `genesis.base` may reference a
custom pre-TEE genesis JSON; `build` verifies its chain ID, copies it into the immutable artifacts,
then applies the fresh founder set and network policy. Private consensus topologies must opt into
`topology.use_local_defaults`; it is never inferred from the network name.

Each founder needs a public checkpoint URL in `hyperlane_public_url`; TCP port `9000` must be
reachable by Hyperlane relayers. The private health endpoint stays on `127.0.0.1:9003`. Shared
origin-chain RPC and reorg settings live in the top-level `hyperlane` section. Devnet uses the
self-contained deterministic feeder profile; testnet and mainnet keep the externally configured
market profile.

The built-in founding workflow uses `no-attestation-dev` on devnet and testnet only. It cannot
bootstrap mainnet, which requires attestation. TEE keys remain in the node process; this
workflow does not itself provide hardware isolation.

## Joining validator

```text
sudo outbe-up validator prepare --bundle <operator-bundle.tar.zst> --config <operator-config.yaml>
sudo outbe-up validator sync [--snapshot <snapshot.tar.zst>]
sudo outbe-up validator promote
sudo outbe-up validator status [--watch]
```

Run `outbe-up validator config-example` to print the embedded operator template, or add
`--output operator-config.yaml` to write it to a new file.

`prepare` verifies the config, network, manifest, every bundled file, and the bootstrap binary. It
then creates host-local identities and installs services without starting them. `sync` optionally
restores a verified snapshot before starting the validator target. Its single node process runs with
`--validator --validator.evm-key ... --upstream ...`, autonomously obtains its resident TEE key,
and synchronizes the chain. Fund the validator address before `sync` for registration gas, stake,
and Hyperlane signer funding. `promote` checks local TEE health through
`outbe_consensusStatus.enclave`, verifies finalized synchronization against the certified RPC,
bonds the minimum stake, delegates oracle votes to the feeder, delegates and funds its dedicated
Hyperlane signer, and submits validator readiness. It never stops or restarts the node: its TEE key
remains in memory throughout `sync -> promote`. The node owns TEE admission and renewal; the
operator journal records only external economic/role transactions and snapshot progress, not key
readiness. DKG activation continues in the background; once the validator becomes active, its
service registers the signer and starts the Hyperlane daemon automatically. Both consensus and
feeder and Hyperlane health are visible through `status`.

The editable configuration example lives at `templates/validator/operator-config.example.yaml`.
Systemd policy comes from the shared `outbe-operator-systemd` crate used by both joining and
founding flows; network-specific values remain in the generated `/opt/outbe-chain/validator.env`.
All persistent validator data, keys, lifecycle state, Hyperlane state, and canonical systemd unit
files live under `/opt/outbe-chain`, so one persistent mount covers the complete installation.
`/etc/systemd/system` contains only links to those units, and `/run` remains ephemeral.
