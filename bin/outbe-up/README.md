# outbe-up

`outbe-up` prepares and promotes a joining Outbe validator from a self-contained operator bundle.
The bundle carries the matching `outbe-up` binary; systemd units and service helper logic are
compiled into that binary so they cannot drift from the lifecycle implementation.
It also carries the pinned `crs/bn254_g1.dat`; `prepare` verifies its canonical SHA-256 before
publishing the bundle under `/opt/outbe-chain`.

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

The editable configuration example lives at
`templates/validator/operator-config.example.yaml`. Systemd templates are embedded at build time
from `templates/validator/systemd/`; network-specific values remain in the generated
`/opt/outbe-chain/validator.env` rather than being rendered into unit files.
