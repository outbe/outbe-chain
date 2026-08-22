# Radicle validator sidecar

Each validator runs one persistent `outbe-radicle` process before starting
`outbe-chain`. The operator or the existing network run script owns this process;
`outbe-chain` never starts, stops, or restarts it.

Generate the identity once:

```bash
outbe-keygen radicle --output-dir /opt/outbe-chain/validator-0/radicle
```

For a founder, generate this identity before finalizing genesis and put the
printed 32-byte NodeId into that validator's `radicle_node_id` field. The
private key remains only in the validator's persistent `RAD_HOME`; genesis
contains the NodeId and the unlimited testnet repository sentinel, never an IP,
DNS name, port, repository, or Radicle private key.

The release executable is built from the `outbe-chain` workspace:

```bash
cargo build --locked --release -p outbe-radicle-sidecar --bin outbe-radicle
```

It is emitted as `target/release/outbe-radicle`. The sibling Heartwood checkout
is used only for the native `rad` and `git-remote-rad` operator tools.

Start the release sidecar in the foreground:

```bash
OUTBE_RADICLE_BINARY=/opt/outbe-chain/outbe-radicle \
  ./scripts/run-radicle.sh \
  /opt/outbe-chain/validator-0/radicle \
  0.0.0.0:8776 \
  127.0.0.1:8876 \
  256 \
  rpc.n1.testnet.outbe.net:8776
```

Then start `outbe-chain` with the matching local control and status endpoints:

```text
--radicle.control-socket /opt/outbe-chain/validator-0/radicle/node/outbe-control.sock
--radicle.status-address 127.0.0.1:8876
```

The run script creates the non-secret Heartwood home directories, atomically
maintains a mode-`0600` `config.json` for the same seedless `Network::Outbe`
runtime profile, enforces strict permissions, holds an exclusive home lock, and
removes a Unix socket only after a connection probe proves it stale. The key,
storage, issues, patches, and repository data remain in the same `RAD_HOME`
across restarts.

Changing the advertised IP, DNS name, or port requires restarting only the
sidecar with the new `--advertise` value. The running validator rereads the native
Heartwood configuration on its reconciliation tick and publishes a newly signed
endpoint without changing genesis or on-chain state.

The Outbe profile has no public Radicle bootstrap seeds. Do not initialize a
validator home with the public-network `rad config init` defaults.

The release acceptance scenario reuses the existing OCOMP integration and
SGX-no-attest lanes; it does not introduce another feature profile:

```bash
mise run e2e-radicle-sgx
```

This command requires real SGX hardware and runs the production enclave through
`gramine-sgx` with remote attestation disabled.
