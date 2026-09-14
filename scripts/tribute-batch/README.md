# Rudis Tribute batch

Два Python-скрипта без `outbe-cli` или `cast` во время работы.

## Подготовка

Из корня репозитория:

```sh
python3 -m venv scripts/tribute-batch/.venv
scripts/tribute-batch/.venv/bin/pip install -r scripts/tribute-batch/requirements.txt
```

## Генерация

```sh
scripts/tribute-batch/.venv/bin/python scripts/tribute-batch/generate.py \
  --count 400 --total-usd 150000 --wwd 20260913
```

Создаёт ровно два JSON в `scripts/tribute-batch/output/20260913/`:

- `wallets.json`: 400 новых адресов и их `private_key`.
- `tributes.json`: 400 Tribute с разными положительными суммами,
  суммарно **150000.000000 USD**, без приватных ключей.

Другая папка задаётся через `--out-dir`. Существующая папка не перезаписывается.
Файлы имеют права `0600`, каталог — `0700`; стандартный `output/` исключён из Git.

Суммы вычисляются целыми числами. Например, consumption `375.123456 USD`:

```json
{"amount_base": "375", "amount_atto": "123456"}
```

`amount_atto` — шестизначный **остаток**, не сумма целиком и не масштаб 10^18:
`amount_minor = amount_base * 1000000 + amount_atto`, `0 <= amount_atto < 1000000`.
Валюта consumption и reference currency — USD (`840`). Все Tribute имеют
`exclude_from_intex_issuance=false`, отдельный owner, случайные draft ID и SU hashes.

## Проверка и отправка

Проверить JSON, TEE, окно offering и потребность адресов в gas без транзакций:

```sh
scripts/tribute-batch/.venv/bin/python scripts/tribute-batch/send.py \
  --wallets scripts/tribute-batch/output/20260913/wallets.json \
  --tributes scripts/tribute-batch/output/20260913/tributes.json \
  --dry-run
```

Отправить, пополняя новые адреса на gas с существующего кошелька:

```sh
scripts/tribute-batch/.venv/bin/python scripts/tribute-batch/send.py \
  --wallets scripts/tribute-batch/output/20260913/wallets.json \
  --tributes scripts/tribute-batch/output/20260913/tributes.json \
  --funding-private-key "$PK"
```

По умолчанию RPC — `https://125.253.92.5`, chain ID — `70860602`.
Для другого endpoint той же сети есть `--rpc-url`.
`--funding-private-key` нужен только для gas; сами Tribute подписываются
ключами соответствующих owner из `wallets.json`. Если адреса уже пополнены,
его можно не указывать. Параметр `--fund-rudis` задаёт минимальный целевой баланс
при пополнении (по умолчанию `0.001 RUDIS`); отправляется только недостающая сумма.
Если оценённого максимального gas больше, пополнение покрывает его.
Для 400 пустых адресов при текущем низком gas это около `0.4 RUDIS` плюс gas переводов.
Остатки остаются у созданных owner, их ключи находятся в первом JSON.

**150000 USD — заявленный consumption. Эта сумма не отправляется как платёж:**
у `offerTribute` всегда `msg.value=0`. Расходуются только native RUDIS на gas.
Газовый лимит Tribute по умолчанию `8000000`, как у канонического CLI:
`eth_estimateGas` не используется для enclave-decrypt. Есть `--gas-limit`.

При закрытом offering новые пополнения и offers не отправляются.
Скрипт не ожидает открытия автоматически: запустите его, когда WWD станет `OFFERING`.
GREEN, итоговый nominal и результат Desis зависят от протокольной обработки;
заданный consumption не является обещанием результата аукциона.

## Восстановление

Отправщик создаёт `tributes.state.json` рядом с входным файлом. И funding,
и offer сохраняются в нём подписанными **до** первого broadcast. После обрыва
запускайте ту же команду с теми же JSON и state: проверяются receipts, для
неподтверждённых отправляются те же байты, подтверждённые Tribute не повторяются.
Подтверждение включает проверку owner, WWD, consumption и валюты в `TributeIssued`.
После полного завершения повтор команды лишь проверяет уже сохранённые receipts.

Не удаляйте журнал незавершённой партии. `--state` позволяет задать другой путь.
При reverted-транзакции скрипт останавливается и сохраняет её хэш для разбора;
автоматическая замена или повтор с новым nonce не выполняется.
Один процесс держит файловую блокировку своего state (macOS/Linux).

## Тесты

```sh
scripts/tribute-batch/.venv/bin/python -m unittest discover -s scripts/tribute-batch -p 'test_*.py'
```

Проверяются точная сумма 400 записей, масштаб 10^6, соответствие адресов ключам,
права файлов, настоящее шифрование/расшифрование payload, подписанные EIP-155
транзакции, восстановление после потери RPC-ответа, закрытое offering и ошибочный receipt.
Тестовые транзакции идут только в mock RPC.
