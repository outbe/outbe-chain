# rudis-gems

Отдельная Rust-утилита для Rehearsal/Rudis: список GEM и цепочка
WUSDC → PayNote settlement → PROMIS → native RUDIS (COEN).
PayNote proof, PoW, MAC и подписи вычисляются внутри процесса.
`outbe-cli` и Node.js для запуска не нужны.

## Сборка и запуск

Из корня репозитория:

```sh
cargo build --release -p rudis-gems

./target/release/rudis-gems list --private-key "$PK"

./target/release/rudis-gems settle --gem-id "$GEM_ID" --private-key "$PK"
```

`PK` — приватный ключ с `0x` или без него. `GEM_ID` — hex ID из списка
или десятичный uint256. Утилита не сохраняет приватный ключ.

По умолчанию RPC — `https://125.253.92.5`, chain ID — `70860602`,
WUSDC — `0xdD9eD2f161c4F9A642471BCF49331A95F5B2B1d3` (6 decimals).
Другой endpoint этой сети задаётся через `--rpc-url`.

Список показывает ID, сумму PROMIS, стоимость settlement в WUSDC, состояние GEM
и балансы WUSDC/native RUDIS. Для чтения можно использовать публичный адрес:

```sh
./target/release/rudis-gems list --address "$ADDRESS"
```

Проверить план без транзакций и ограничить стоимость settlement:

```sh
./target/release/rudis-gems settle --gem-id "$GEM_ID" --private-key "$PK" \
  --max-settlement 30 --dry-run
```

Для выполнения уберите `--dry-run`. Лимит указан в WUSDC и не включает gas.
`settle` выполняет необходимый approve, deposit, строит и локально проверяет
PayNote proof, вызывает settlement, рассчитывает PoW и mint MAC, получает
PROMIS, затем с новым opNonce и burn MAC конвертирует сумму этого GEM в RUDIS.
Уже имеющийся PROMIS сверх этой суммы остаётся на балансе.
Для транзакций нужен native RUDIS на gas.

## Перевод RUDIS

Посмотреть native-баланс любого адреса без приватного ключа:

```sh
./target/release/rudis-gems balance 0x89a12ac6dE30463278eB4A0eEeBF2DA16eA9637d
```

Отправить RUDIS:

```sh
./target/release/rudis-gems send 0x89a12ac6dE30463278eB4A0eEeBF2DA16eA9637d \
  --rudis 5000000 --private-key "$PK"
```

`--rudis` задаёт сумму в RUDIS, до 18 знаков после точки. Gas оплачивается
сверх этой суммы. `--dry-run` проверяет оценку gas и достаточность баланса,
не отправляя транзакцию. Успех выводится после успешного receipt.

При обрыве RPC повторите команду с теми же адресом, суммой и `--state-dir`:
незавершённый перевод восстановится из `transfers/pending.json` в каталоге
кошелька. После подтверждения файл переносится в историю по хэшу транзакции.
Повтор команды после сообщения `Sent` создаёт **новый** перевод.

## Повторный запуск settlement

После обрыва RPC повторите ту же команду с тем же `--state-dir`.
Подписанная транзакция сохраняется **до** отправки; при повторном запуске
проверяется её receipt и при необходимости отправляются те же подписанные байты.
Уже завершённые этапы не создают новых транзакций.

По умолчанию журнал и секреты PayNote лежат в
`.rudis-gems/<chain>/<address>/<gem>/` под корнем репозитория
(вне репозитория — в текущем каталоге). Можно задать `--state-dir /absolute/path`.
Не удаляйте этот каталог при незавершённой операции: PayNote содержит секрет
для расходования внесённых WUSDC и получения сдачи.
Файлы создаются с правами `0600`, каталоги — `0700`.
Поддерживается восстановление старого журнала этой операции из TypeScript-утилиты.

## Проверка

```sh
cargo test --release -p rudis-gems
cargo clippy --release -p rudis-gems --all-targets -- -D warnings
```

Тесты проверяют настоящий PayNote proof и отказ при его подмене, PoW/MAC,
подписанные mint/convert с разными opNonce, восстановление после потери ответа
RPC и отсутствие повторных транзакций. Финансовые операции проверяются на mock RPC;
обычный запуск тестов не отправляет транзакции в сеть.
