# Проверка создания Tribute и MongoDB projection локально

Эта инструкция проверяет полный protocol flow: encrypted offer отправляется в
`TributeFactory`, расшифровывается и вычисляется в TEE sidecar, исполняется в
блоке, после финализации событие проектируется ExEx в MongoDB. Прямых записей в
MongoDB и обхода compressed-entity lifecycle здесь нет.

## Требования

- Собраны `outbe-chain`, `outbe-cli` и dev TEE mock:

  ```sh
  cargo build --bin outbe-chain --bin outbe-cli
  cargo build --release --bin outbe-tee-enclave-mock --features mock
  ```

- Доступны `cast`, `python3`, Python package `cryptography`, `docker` и `mise`.
- MongoDB запущена как replica set, потому что projection использует
  транзакции. Пример для чистого локального MongoDB:

  ```sh
  docker run -d --name outbe-local-mongodb -p 27017:27017 \
    mongo:7 --replSet rs0 --bind_ip_all
  docker exec outbe-local-mongodb mongosh --quiet --eval \
    'rs.initiate({_id:"rs0",members:[{_id:0,host:"127.0.0.1:27017"}]})'
  ```

## 1. Собрать genesis и запустить сеть

Выберите свободный `PORT_OFFSET`. Он должен быть одинаковым при bootstrap,
start, status и stop.

```sh
export OUT_DIR=/tmp/outbe-tribute-check
export PORT_OFFSET=0
export OUTBE_CHAIN_BINARY="$PWD/target/debug/outbe-chain"
export OUTBE_PROJECTION_MONGODB_URI='mongodb://127.0.0.1:27017/?replicaSet=rs0&directConnection=true'
export OUTBE_PROJECTION_MONGODB_DATABASE_PREFIX=outbe_tribute_check
export OUTBE_TEE_ENCLAVE=1
export OUTBE_TEE_ENCLAVE_MOCK=1
export OUTBE_TEE_ENCLAVE_BINARY="$PWD/target/release/outbe-tee-enclave-mock"

./scripts/bootstrap-testnet.sh 4 "$OUT_DIR"
./scripts/run-testnet.sh start "$OUT_DIR"
./scripts/run-testnet.sh status "$OUT_DIR"
```

Дождитесь, пока высота станет больше нуля:

```sh
RPC_PORT=$((8545 + PORT_OFFSET))
cast block-number --rpc-url "http://127.0.0.1:$RPC_PORT"
```

Если для того же database prefix уже существует projection от другого
genesis, startup намеренно завершится ошибкой `projection identity does not
match configured chain`. Используйте новый prefix или удалите только старые
одноразовые test databases перед чистым bootstrap.

## 2. Создать Tribute через Python flow

Worldwide day сети вычисляется в UTC+14. Public offer key читается из on-chain
TEE registry, а private key берётся из локального bootstrap output:

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

Успех означает одновременно:

- скрипт напечатал `Receipt status:0x1`;
- `Total supply` увеличился на один;
- receipt содержит `TributeBodyStored` и `TributeIssued` logs.

Повторная независимая проверка:

```sh
TX_HASH=<hash из вывода скрипта>
cast receipt "$TX_HASH" --rpc-url "$RPC_URL"
cast call 0x0000000000000000000000000000000000001101 \
  'totalSupply()(uint256)' --rpc-url "$RPC_URL"
```

Не используйте `getTributesByOwner` через обычный `eth_call` как post-check:
compressed body reads требуют active block lifecycle. Каноническое persistent
body после финализации проверяется в projection DB.

## 3. Проверить MongoDB

`run-testnet.sh` создаёт отдельную базу для каждого валидатора:
`<prefix>_validator_0` … `<prefix>_validator_3`.

```sh
docker exec outbe-local-mongodb mongosh --quiet --eval '
for (let i = 0; i < 4; i++) {
  const d = db.getSiblingDB("outbe_tribute_check_validator_" + i);
  print("DB=" + d.getName());
  print("tributes=" + d.tributes.countDocuments({})
    + " owner_index=" + d.tributes_by_owner.countDocuments({})
    + " day_index=" + d.tributes_by_day.countDocuments({}));
  print(EJSON.stringify(d.tributes.findOne(), {relaxed:false}));
}'
```

Для созданного Tribute ожидается:

- одна новая запись в `tributes`;
- одна запись в `tributes_by_owner`;
- одна запись в `tributes_by_day`;
- в `tributes._projection.tx_hash` находится `TX_HASH` успешного receipt;
- `_id` и binary `value` одинаковы во всех четырёх validator databases.

## 4. Остановить сеть

```sh
./scripts/run-testnet.sh stop "$OUT_DIR"
```

Mock TEE подходит только для функциональной localnet-проверки и не даёт SGX
confidentiality/attestation. Production SGX flow описан в
`docs/launching-with-sgx.md`.
