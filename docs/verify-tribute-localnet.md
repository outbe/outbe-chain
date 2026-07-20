# Verify Tribute creation and MongoDB projection locally

This guide creates an encrypted Tribute on a local network and verifies its
MongoDB projection. The offer goes to `TributeFactory`, the TEE sidecar decrypts
it, and the transaction executes in a block. After finalization, ExEx projects the
event into MongoDB. The flow never writes to MongoDB directly or bypasses the
compressed-entity lifecycle.

## Requirements

- Build `outbe-chain`, `outbe-cli`, and the development TEE mock:

  ```sh
  cargo build --bin outbe-chain --bin outbe-cli
  cargo build --release --bin outbe-tee-enclave-mock --features mock
  ```

- Install `cast`, `python3`, the Python `cryptography` package, `docker`, and
  `mise`.
- Run MongoDB as a replica set because the projection uses transactions. To start
  a clean local MongoDB:

  ```sh
  docker run -d --name outbe-local-mongodb -p 27017:27017 \
    mongo:7 --replSet rs0 --bind_ip_all
  docker exec outbe-local-mongodb mongosh --quiet --eval \
    'rs.initiate({_id:"rs0",members:[{_id:0,host:"127.0.0.1:27017"}]})'
  ```

## Start the localnet with mise

The `Managed localnet stack` section in the root
[`README.md`](../README.md) describes the complete setup. This check needs only
three commands:

```sh
mise run localnet-stack-start
mise run tribute-offer
mise run tribute-show-mongo
```

The first command starts the infrastructure, the second creates one Tribute, and
the third verifies its MongoDB projection on all four validators. Remove the
localnet when finished:

```sh
mise run localnet-stack-clean
```

For manual diagnosis, run the same steps separately.

Start the complete local environment:

```sh
mise run localnet-stack-start
```

The command:

- builds `outbe-chain`, `outbe-cli`, and the mock TEE;
- creates a clean `/tmp/outbe-localnet-stack`;
- starts a separate `mongo:7.0` replica set on port `27027`;
- starts four validators with mock TEEs and separate projection databases;
- waits for RPC and the first block;
- checks a MongoDB transaction, four live validator processes, and four projection
  databases;
- prints the RPC URL, MongoDB URI, database prefix, and data directory.

Export the printed values before running the commands below. The defaults are:

```sh
export OUT_DIR=/tmp/outbe-localnet-stack
export PORT_OFFSET=1000
export MONGO_CONTAINER=outbe-localnet-stack-mongodb
export MONGO_PORT=27027
export DB_PREFIX=outbe_localnet_stack
```

This localnet does not create a Tribute automatically. Create the offer and check
MongoDB as described in sections 2 and 3; skip section 1 after starting the network
through `mise`. When finished:

```sh
# Stop the network and MongoDB but keep chain data.
mise run localnet-stack-stop

# Or stop everything and delete the localnet data directory.
mise run localnet-stack-clean
```

To run a second localnet, choose unique values for `LOCALNET_STACK_DIR`,
`LOCALNET_STACK_MONGO_NAME`, `LOCALNET_STACK_MONGO_PORT`,
`LOCALNET_STACK_PORT_OFFSET`, and `LOCALNET_STACK_DATABASE_PREFIX`. The port
ranges must not overlap. The following sections describe the same setup manually.

## 1. Build genesis and start the network

Choose an unused `PORT_OFFSET`. Use the same value for bootstrap, start, status,
and stop.

```sh
export OUT_DIR=/tmp/outbe-tribute-check
export PORT_OFFSET=0
export OUTBE_CHAIN_BINARY="$PWD/target/debug/outbe-chain"
export OUTBE_PROJECTION_MONGODB_URI='mongodb://127.0.0.1:27017/?replicaSet=rs0&directConnection=true'
export OUTBE_PROJECTION_MONGODB_DATABASE_PREFIX=outbe_tribute_check
export MONGO_CONTAINER=outbe-local-mongodb
export MONGO_PORT=27017
export DB_PREFIX=outbe_tribute_check
export OUTBE_TEE_ENCLAVE=1
export OUTBE_TEE_ENCLAVE_MOCK=1
export OUTBE_TEE_ENCLAVE_BINARY="$PWD/target/release/outbe-tee-enclave-mock"

./scripts/bootstrap-testnet.sh 4 "$OUT_DIR"
./scripts/run-testnet.sh start "$OUT_DIR"
./scripts/run-testnet.sh status "$OUT_DIR"
```

Wait until the block number is greater than zero:

```sh
RPC_PORT=$((8545 + PORT_OFFSET))
cast block-number --rpc-url "http://127.0.0.1:$RPC_PORT"
```

If the same database prefix already contains a projection from another genesis,
startup fails with `projection identity does not match configured chain`. Use a
new prefix, or delete the old disposable test databases before a clean bootstrap.

## 2. Create a Tribute with the Python flow

The network calculates WorldwideDay in UTC+14. Read the public offer key from the
on-chain TEE registry and the private key from the local bootstrap output:

```sh
RPC_URL="http://127.0.0.1:$RPC_PORT"
V0=$(tr -d '[:space:]' < "$OUT_DIR/validator-0/evm-key.hex")
WWD=$(date -u -d "@$(($(date +%s) + 50400))" +%Y%m%d)
export TEE_PUBLIC_KEY=$(cast call \
  0x000000000000000000000000000000000000EE0A \
  'tributeOfferPublicKey()(bytes32)' \
  --rpc-url "$RPC_URL")

python3 scripts/tributefactory/offer_tribute.py \
  --wwd "$WWD" \
  --amount-base 100 \
  --currency 840 \
  --private-key "$V0" \
  --rpc-url "$RPC_URL"
```

The check passes when:

- the script prints `Receipt status:0x1`;
- `Total supply` increases by one;
- the receipt contains `TributeBodyStored` and `TributeIssued` events.

Check the result independently:

```sh
TX_HASH=<hash from the script output>
OWNER=$(cast wallet address --private-key "$V0")
cast receipt "$TX_HASH" --rpc-url "$RPC_URL"
cast call 0x0000000000000000000000000000000000001101 \
  'totalSupply()(uint256)' --rpc-url "$RPC_URL"
cast call 0x0000000000000000000000000000000000001101 \
  'getTributesByOwner(address)(bytes[])' "$OWNER" --rpc-url "$RPC_URL"
```

The final call reads finalized compressed bodies through an ordinary `eth_call`.
The MongoDB check below independently verifies the persisted projection on each
validator.

## 3. Verify MongoDB

`run-testnet.sh` creates one database per validator:
`<prefix>_validator_0` through `<prefix>_validator_3`.

```sh
docker exec "$MONGO_CONTAINER" mongosh --quiet --port "$MONGO_PORT" --eval "
for (let i = 0; i < 4; i++) {
  const d = db.getSiblingDB("${DB_PREFIX}_validator_" + i);
  print("DB=" + d.getName());
  print("tributes=" + d.tributes.countDocuments({})
    + " owner_index=" + d.tributes_by_owner.countDocuments({})
    + " day_index=" + d.tributes_by_day.countDocuments({}));
  print(EJSON.stringify(d.tributes.findOne(), {relaxed:false}));
}"
```

The created Tribute should produce:

- one new record in `tributes`;
- one record in `tributes_by_owner`;
- one record in `tributes_by_day`;
- the successful receipt's `TX_HASH` in `tributes._projection.tx_hash`;
- identical `_id` and binary `value` fields in all four validator databases.

## 4. Stop the network

```sh
./scripts/run-testnet.sh stop "$OUT_DIR"
```

The mock TEE supports functional localnet testing but provides no SGX
confidentiality or attestation. See `docs/launching-with-sgx.md` for the production
SGX flow.
