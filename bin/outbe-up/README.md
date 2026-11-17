# outbe-up

`outbe-up` prepares and promotes a joining Outbe validator from a self-contained operator bundle.
The bundle carries the matching `outbe-up` binary; systemd units and service helper logic are
compiled into that binary so they cannot drift from the lifecycle implementation.

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
restores a verified snapshot before starting the validator target. Its single node process runs with
`--validator --validator.evm-key ... --upstream ...`, autonomously obtains its resident TEE key,
and synchronizes the chain. Fund the validator address before `sync` for registration gas and stake.
`promote` checks local TEE health through `outbe_consensusStatus.enclave`, verifies finalized
synchronization against the certified RPC, bonds the minimum stake, delegates oracle votes to the
feeder, and submits validator readiness. It never stops or restarts the node: its TEE key remains
in memory throughout `sync -> promote`. The node owns TEE admission and renewal; the operator
journal records only external economic/role transactions and snapshot progress, not key readiness.
DKG activation continues in the background and is visible through `status`.

The editable configuration example lives at `templates/validator/operator-config.example.yaml`.
Systemd policy comes from the shared `outbe-operator-systemd` crate used by both joining and
founding flows; network-specific values remain in the generated `/opt/outbe-chain/validator.env`.
