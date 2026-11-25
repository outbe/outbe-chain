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
outbe-up network stop --artifacts network-artifacts [--ssh-key ~/.ssh/operator]
outbe-up network restart --artifacts network-artifacts [--ssh-key ~/.ssh/operator]
outbe-up network upgrade --artifacts network-artifacts \
  --software-bundle outbe-software-bundle-....tar.zst [--ssh-key ~/.ssh/operator]
outbe-up network doctor --artifacts network-artifacts [--support-bundle support.tar.zst]
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

`stop` deliberately removes the persistent auto-start marker. `restart` and `upgrade` preserve it,
operate one founder at a time, verify quorum before the first interruption, avoid DKG freeze windows,
and wait for full health before continuing. Upgrade consumes a software bundle directly, replaces
regular binaries atomically while retaining the preceding set in `/opt/outbe-chain/.previous`,
regenerates systemd units from the new `outbe-up`, and stops the rollout instead of automatically
running old binaries against data opened by a newer release. The first upgrade from a node that does
not yet expose `maintenanceSafe` requires the explicit `--skip-dkg-guard` acknowledgement. A failed
upgrade also removes the auto-start marker so a VM reboot cannot silently restart the rejected
release; inspect the node and use `validator start` explicitly after fixing it. Founding systemd
units are refreshed, but the generated `validator-N/run-node.sh` launch arguments remain those from
the immutable network build; changing founder launch flags requires a new network build/prepare.

Create a founder snapshot on its VM with `network snapshot create --founder ... --output
/opt/outbe-chain/snapshots/<name>.tar.zst`; add `--download <local-path>` only when a local copy is
actually needed. `network snapshot validate` runs the existing signed snapshot validator. Both
commands check quorum, DKG safety, and recovery of the temporarily stopped founder.

`network.yaml` v2 declares bare-metal `machines` and assigns every founder a private VM address.
`build` groups founders by machine and assigns deterministic port offsets, so one public IP can
serve several VM validators. The first VM uses the standard Reth, consensus, Radicle, Hyperlane,
and optional RPC ports; later VMs increment them. Resolved endpoints and neutral DNAT mappings are
recorded in `network-manifest.json` and printed for the separately managed host firewall.

`prepare`, `start`, and `status` connect to each private VM through its bare-metal machine without
requiring local SSH aliases. The operator's default SSH identity is used unless `--ssh-key` is
provided. Host fingerprints are kept in `$XDG_STATE_HOME/outbe-up` (or
`~/.local/state/outbe-up`), not inside the immutable artifact directory. Bare-metal SSH must permit
forwarding to the configured VM port 22.

`genesis.base` may reference a custom pre-TEE genesis JSON; `build` verifies its chain ID, copies it
into the immutable artifacts, then applies the fresh founder set and network policy. Private
consensus topologies must opt into `topology.use_local_defaults`; it is never inferred from the
network name. Hyperlane public URLs are derived from the machine IP and resolved public port; its
private health endpoint stays on `127.0.0.1:9003`. Devnet uses the self-contained deterministic
feeder profile; testnet and mainnet keep the externally configured market profile.

The built-in founding workflow uses `no-attestation-dev` on devnet and testnet only. It cannot
bootstrap mainnet, which requires attestation. TEE keys remain in the node process; this
workflow does not itself provide hardware isolation.

## Joining validator

```text
sudo outbe-up validator prepare --bundle <operator-bundle.tar.zst> --config <operator-config.yaml>
sudo outbe-up validator sync [--snapshot <snapshot.tar.zst>]
sudo outbe-up validator promote
sudo outbe-up validator status [--watch]
sudo outbe-up validator stop
sudo outbe-up validator start
sudo outbe-up validator restart
sudo outbe-up validator upgrade --software-bundle <software-bundle.tar.zst>
sudo outbe-up validator doctor [--support-bundle support.tar.zst]
sudo outbe-up validator snapshot create --output /opt/outbe-chain/snapshots/node.tar.zst
sudo outbe-up validator snapshot validate --archive /opt/outbe-chain/snapshots/node.tar.zst
sudo outbe-up validator unjail
sudo outbe-up validator exit
sudo outbe-up validator exit-status
sudo outbe-up validator claim
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

`validator stop` disables automatic start across VM reboots; transient `restart`, `upgrade`, and
snapshot operations do not change that persistent policy. `validator snapshot create` checks free
space and uses the existing signed snapshot format; `snapshot validate` performs the existing native
validation. `doctor` is read-only. Its optional private support archive contains only a fixed list of
public configs, canonical unit files, status output, and bounded journald logs; key and operator-state
directories are never traversed.

For on-chain maintenance, `validator unjail` requires a synchronized resident validator, submits
unjail, and confirms readiness for the next DKG. `validator exit` deactivates an active validator,
waits until it is no longer a consensus participant, starts unbonding, then stops and disables the
services. It does not wait through the unbonding period; use `validator exit-status` and later
`validator claim`.
