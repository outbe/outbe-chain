# Rehearsal Network: план реализации публичной идентичности

Статус: пользователь подтвердил переименование wire identifiers; оно реализовано и прошло перечисленные ниже локальные проверки. Полная goal ещё не завершена из-за оставшихся проверок окружения. Изменения в рабочем дереве, без commit/deploy. Исходная точка исследования: `7e2430b2`, 2026-09-07. Ниже сохранён исходный план; актуальное выполнение и ограничения перечислены в конце.

## Целевой контракт

Сеть публично доступна, но без анонсов, продвижения, публикации материалов и добавления в публичные каталоги. Профиль исполнения остаётся `testnet`.

| Поверхность | Целевое значение |
| --- | --- |
| Отображаемое название сети | `Rehearsal Network` |
| Машинное имя сети | `rudis-rehearsal` |
| EVM chain ID | `70860602`, JSON-RPC quantity `0x4393f3a` |
| Нативный токен / публичное обозначение | `rudis` |
| Wrapped token symbol | `wrudis` |
| Wrapped token name | `Wrapped Rudis` |
| Native / wrapped decimals | `18`, без изменения сумм и точности |
| TEE policy | `GramineDirectDev` / `gramine-direct-dev` |
| Фактическое исполнение enclave | настоящий `gramine-sgx`, remote attestation `none` |
| Node–enclave session | `production-node-host` |

Уточнения пользователя: отображаемое имя сети — `Rehearsal Network`, машинное имя — `rudis-rehearsal`. ID `70860602`, токены `rudis` / `wrudis`, RPC namespace `rudis_*` и прочие параметры сохраняются. Имя этого файла также сохраняется. Машинное имя синхронизировано в Rust/Python, release schema/spec, MCP network keys и именах поддерживаемых сетей в vectors; ID и адресные/EIP-712 preimages не менялись. Изменение отображаемого имени проверено: MCP typecheck, 3 identity-теста, создание chain context через stub fetch transport и 13 Intex metadata-тестов прошли. Это не заменяет остающийся HTTP/hardware smoke.

Для строковых symbol в плане принят буквальный нижний регистр из требования: `rudis` / `wrudis`. Не вводить одновременно разные варианты регистра в Rust, Solidity и клиентах.

Файлы, директории, crates/packages, Rust-типы, Solidity contract names, CLI, бинарники и имена env-переменных сохраняются. Например, контракт `WCOEN` в файле `WCOEN.sol` возвращает новые `name()` / `symbol()`.

Внутренние единицы `unit`, коэффициенты 1e18/1e6, экономические параметры, storage layout, precompile addresses, технические CREATE3 salts и криптографические domain separators не являются брендингом. Их не заменять поиском по `outbe` / `coen`.

## Подтверждённые точки изменения

| Область | Что найдено | Исходники |
| --- | --- | --- |
| Идентичность сети | Testnet сейчас `54322345` / `outbe-testnet-1`; распознавание профиля зависит от ID | [chain.rs](crates/blockchain/primitives/src/chain.rs#L46), [валидация ChainSpec](bin/outbe-chain/src/main.rs#L877) |
| Native metadata | Общий symbol `COEN`, base denomination `unit`, decimals 18 | [units.rs](crates/blockchain/primitives/src/units.rs#L7) |
| Genesis и bundle | Таблица сетей продублирована в двух Python-генераторах; файл `testnet.yaml` фактически задаёт devnet ID `424242` и служит baseline | [create_genesis.py](scripts/create_genesis.py#L68), [prepare_network.py](scripts/prepare_network.py#L59), [testnet.yaml](scripts/testnet.yaml#L13) |
| SGX без DCAP | Основной launcher поддерживает SGX без attestation и production session; при отсутствии hardware/manifest есть fallback | [launch_bundle.py](scripts/launch_bundle.py#L251), [выбор session](scripts/launch_bundle.py#L355), [SgxNoAttest](bin/outbe-tee-enclave/src/gramine.rs#L33) |
| Альтернативный launcher | В `prepare_network.py` SGX devices добавляются в ветке `dcap-required`, но отсутствуют в dev-ветке | [prepare_network.py](scripts/prepare_network.py#L872) |
| Wrapped ERC-20 | Canonical wrapper и synthetic deployment возвращают `Wrapped COEN` / `WCOEN` | [WCOEN.sol](contracts/tokens/src/canonical/WCOEN.sol#L4), [Routes.sol](contracts/tokens/script/routes/Routes.sol#L113) |
| Deployment identity | `tokenLabel: WCOEN` участвует в salts; CREATE3 address не зависит от token metadata | [Routes.sol](contracts/tokens/script/routes/Routes.sol#L57), [BaseRoute.sol](contracts/tokens/script/routes/BaseRoute.sol#L21) |
| NFT metadata | `Outbe Gem`, `Outbe Nod`, `Outbe Tribute`, описание Outbe network у Intex, image URL `api.outbe.io` | [Gem](crates/core/gem/src/constants.rs), [Nod](crates/core/nod/src/constants.rs), [Tribute](crates/core/tribute/src/state.rs#L78), [Intex](contracts/intex/src/shared/libs/IntexMetadata.sol#L16) |
| MCP / кошельки | Новый ID попадёт в fallback `Ether/ETH`; название строится как `outbe-${id}` | [chain.ts](mcp/src/chain.ts#L28) |
| Клиентские реестры | Старые network names, ID, RPC URL, token aliases и bridge destination ID зашиты отдельно | [intent registry](mcp/src/intent/registry.ts#L27), [token registry](mcp/src/intent/tokens.ts#L14), [Intex registry](mcp/src/intex/registry.ts#L39), [chain definitions](contracts/intex/scripts/shared/chains.ts#L29) |
| Release validation | Имя/ID зафиксированы в xtask, schema и release metadata; существующий SGX release spec требует DCAP | [sgx.rs](xtask/src/release/sgx.rs#L56), [schema](release/release-manifest-v1.schema.json#L207), [bundle spec](release/testnet-sgx-bundle-v1.json) |
| Chain-bound данные | Stablecoin address vectors включают ID в preimage; есть EIP-712 vectors и genesis/network binding fixtures | [network vectors](crates/blockchain/primitives/testdata/stablecoin/v1/network-address-vectors.json), [EIP-712 vectors](crates/blockchain/primitives/testdata/stablecoin/v1/eip712-domain-vectors.json) |
| RPC namespace | Сервер объявлен как `outbe_*`; custom module отдельно разрешён при разборе конфигурации | [api.rs](crates/blockchain/rpc/src/api.rs#L327), [main.rs](bin/outbe-chain/src/main.rs#L829) |

## Порядок реализации

### 1. Установить новую testnet identity

- Заменить `TESTNET_CHAIN_ID` на `70_860_602`, `TESTNET_CHAIN_NAME` на `rudis-rehearsal`. Внутреннее перечисление `OutbeNetwork::Testnet` сохранить.
- Синхронно обновить таблицы `NETWORK_IDENTITIES` в Python и testnet metadata в release tooling. В xtask использовать общую Rust-константу имени вместо отдельного литерала.
- Убедиться, что новый ID проходит node startup, TEE policy, consensus identity и генераторы как testnet. Старый ID не должен продолжать обозначать новую сеть.
- Не менять ID devnet/mainnet. Не менять общий devnet default только потому, что файл называется `testnet.yaml`: новый запуск задаёт `network: testnet` и `chain_id: 70860602` явно.
- Сохранить имена файлов `testnet-genesis.json`, `testnet-sgx-bundle-v1.json` и прочих артефактов, обновляя их содержимое по назначению.

### 2. Зафиксировать запуск на SGX без DCAP

Использовать `create_genesis.py` → `launch_bundle.py` как основной путь подготовки этой сети:

```yaml
network: testnet
chain_id: 70860602
tee:
  mode: gramine-direct-dev
enclave_sgx: true
```

- Enclave запускается через `gramine-sgx` с подписанным manifest, в котором remote attestation отключена; node использует `production-node-host`.
- Для явного `enclave_sgx: true` отсутствие SGX device или подходящего manifest должно завершать запуск с ошибкой. Сейчас launcher может перейти к контейнеру без аппаратной изоляции; для требуемой сети такой fallback не подходит.
- Проверять режим manifest/runtime, а не выводить наличие SGX из одного имени `gramine-direct-dev` или значения attestation `none`.
- `prepare_network.py` синхронизировать по identity. Не объявлять его dev-ветку эквивалентным SGX-путём, пока она не поддерживает те же требования; основной план не требует переписывать оба launcher одновременно.
- Существующий release manifest с DCAP не превращать в unattested release простой заменой строки. Обновление его testnet identity и подготовка запуска без DCAP — разные изменения; mainnet DCAP-проверки сохраняются.

### 3. Заменить публичные token / NFT metadata

- Обновить native symbol; сохранить `BASE_DENOM = unit`, 18 native decimals и 6 protocol decimals.
- В `WCOEN.sol` и synthetic-конструкторе из `Routes.sol` установить `Wrapped Rudis` / `wrudis`.
- Сохранить contract/file names, `CANONICAL_WCOEN_TOKEN` и salt label `WCOEN`. Не смешивать token symbol с техническим ключом deployment.
- Заменить бренд в описаниях Gem, Nod, Tribute, Intex. Имена самих продуктов Gem/Nod/Tribute/Intex/Gratis/Promis остаются прежними.
- Для image URL использовать фактический адрес сервиса Rudis либо согласованный вариант metadata без внешнего image URL. Не придумывать домен и не оставлять ссылку на Outbe как скрытый fallback.
- Проверить публично возвращаемые ошибки и тексты MCP: они также содержат `COEN`, например ошибки цен/overflow и описания amount/staking.
- Добавить `rudis` как входное обозначение native asset в seed/oracle tooling, обновить генерируемые oracle pairs. Существующий `native` и технические адреса пар сохранить; поддержку старого input alias можно оставить для старых конфигураций, не выдавая его как canonical output.

### 4. Синхронизировать API-клиентов и сетевые реестры

- В MCP и chain definitions привязать ID `70860602` к `Rehearsal Network`, `rudis`, decimals 18. Не допустить fallback к ETH.
- Обновить публичные network keys, token registry и aliases, bridge destination mappings, описания инструментов и данные подключения в пределах нового профиля.
- Операторские имена CLI/env/config selectors сохранять там, где это технические средства запуска, а не возвращаемые пользователю сведения о сети.
- Старые RPC URL и deployment addresses не переносить на новый ID автоматически. Адреса подтвердить по новому deployment/bundle; внешний wrapped-токен должен действительно возвращать `wrudis`.
- Перед окончательным запуском отдельно согласовать фактические RPC/image endpoints. План не включает DNS-регистрацию, публикацию сайта или добавление сети в каталоги.

### 5. Граница RPC/API identifier rename

Подтверждено пользователем 2026-09-07: переименовать wire identifiers вместе с возвращаемыми значениями. RPC namespace становится `rudis_*`; старые публичные RPC aliases не сохраняются. Операторский selector `outbe` разрешено нормализовать в `rudis`, сохранив совместимость запуска.

Синхронно обновить:

- namespace в `crates/blockchain/rpc/src/api.rs`;
- регистрацию модуля и валидатор выбора RPC-модулей в `bin/outbe-chain/src/main.rs`;
- RPC-клиентов operator/follower, MCP, CLI, scripts и harness, вызывающих эти методы;
- HTTP/WS module selection и интеграционные проверки RPC.

При этом не переименовывать сами CLI/binary names. Возможность старого config selector `outbe` выбирать новый публичный namespace отделить от наличия старых RPC aliases.

ABI getter `wcoen()` становится `wrudis()`. Связанные публичные ABI methods/events и имена JSON-полей также переименовываются; ABI exports пересоздаются из Solidity. Это согласованное изменение wire API, включая затронутые selectors/topics. Криптографический префикс подписи `outbe/<ledger>/derive-keys/v1` не менять вместе с именем RPC: это отдельный signing contract.

### 6. Пересоздать данные, зависящие от chain ID

Рабочее предположение: это запуск новой сети с новым genesis, не переименование действующей цепочки с сохранением состояния.

- Сгенерировать genesis и весь связанный комплект registration / network binding / launch identity заново для `70860602`.
- Пересчитать адресные и EIP-712 vectors, где ID входит в preimage; обновить genesis-dependent hashes/signatures/fixtures через штатные генераторы. Не подменять число внутри подписанных или хешированных артефактов вручную.
- Проверить соответствие seed-конфигураций, oracle feeder, release schemas и manifest identity.
- CREATE3 wrapper addresses могут сохранять схему предсказания при неизменных factory/deployer/salt. Это не означает, что все адреса сети сохранятся: stablecoin namespace vectors явно зависят от chain ID.
- Уже развёрнутый `WCOEN` не получает новые name/symbol после изменения исходника: у показанного wrapper нет setter метаданных. Для нового запуска нужны новые корректно развёрнутые контракты; обновление внешних существующих deployments — отдельное действие.

## Проверки готовности реализации

1. Rust/Python: `70860602` распознаётся как testnet; генератор и node принимают его с GramineDirectDev; несовпадающие network/ID отклоняются; mainnet policy остаётся прежней.
2. Genesis/bundle: ID и имя согласованы во всех выходных артефактах; связанные hashes, signatures и independent vectors корректно пересчитаны.
3. ERC-20: реальные вызовы `name()`, `symbol()`, `decimals()` к canonical и synthetic token; deposit/withdraw, balances, supply и transfers работают с прежними величинами.
4. NFT: декодировать фактические `tokenURI()` / `uri()` для Gem/Nod/Tribute/Intex; проверить description/image и отсутствие старого бренда в этих полях.
5. MCP: stub RPC возвращает `70860602`; контекст, native transfers, token resolution и bridge destination mapping используют Rudis и правильные decimals. Обновить существующие denomination tests, а не только проверять текст исходника.
6. Локальный RPC smoke: `eth_chainId = 0x4393f3a`, `net_version = 70860602`; проверить фактические metadata/error responses. После решения по namespace проверить соответствующие HTTP/WS методы и follower/operator вызовы.
7. SGX launch tests: явный hardware-профиль не допускает fallback, использует production session и remote attestation `none`. Полный hardware smoke — отдельный запуск: подтверждение `SgxNoAttest`, bootstrap, финализация блоков, перезапуск и чтение публичных ответов.
8. Остаточный поиск старых строк классифицирует каждое совпадение: публичное значение исправляется; внутренний идентификатор, salt/domain или исторический fixture сохраняется по назначению. Требование не сводится к отсутствию слова `outbe` во всём репозитории.

На этапе подготовки плана тесты/build/deploy не запускались. В рамках последующей goal выполняются локальные изменения, сборки и проверки; серверы, действующие сети и публикации остаются вне объёма.

## Граница достоверности исследования

Проверены перечисленные источники локального checkout; runtime и SGX hardware не запускались. `list_projects` подтвердил наличие graph project `Users-sakor-outbe-io-outbe-chain`, но запросы `index_status`, `search_graph` и `check_index_coverage` не вернули результат за время исследования. Поэтому generation/freshness и coverage не подтверждены; выводы опираются на прямое чтение и строковый поиск, а таблица не заявляется исчерпывающим графовым аудитом всех call-sites. Перед реализацией расширенного RPC rename нужен отдельный полный inventory его потребителей.


## Выполнение goal: 2026-09-07

Изменения локальные, поверх указанного HEAD. После статуса blocked пользователь подтвердил пункт 5, и реализация RPC/ABI rename выполнена. HTTP smoke пока не завершён. Запрос повторного HTTP-прогона подтверждён как всё ещё выполняющийся; он не объявлен завершённым или отклонённым. Аппаратная готовность или готовность действующей публичной сети не заявляется.

### Реализовано

- Testnet identity `70860602` / `rudis-rehearsal` синхронизирована в primitives, Python, xtask, release schema/spec, тестах, скрытом testnet CI workflow и примерах конфигурации. Devnet/mainnet ID сохранены. `scripts/network.example.yaml` явно выбирает testnet + SGX; общий `testnet.yaml` сохраняет devnet baseline.
- Native symbol `rudis`, canonical и synthetic wrapper metadata `Wrapped Rudis` / `wrudis`. Имена `WCOEN`, env, salt label, precompile addresses, storage и числа не переименованы. CLI отображает `rudis`, сохраняя прежние команды и бинарники.
- Gem/Nod/Tribute/Intex descriptions используют Rudis. В Gem/Nod удалён внешний `image` из фактического JSON: реального адреса Rudis нет, поэтому после вопроса оператору принят обратимый вариант без image URL. Это рабочее допущение, не полученное подтверждение пользователя.
- Основной SGX launcher при `enclave_sgx: true` требует device и подписанный manifest, проверяет `sgx.remote_attestation = none` и запускает `gramine-sgx`. Отсутствие hardware не приводит к Docker/direct fallback. Node использует `production-node-host`. Для mock требуется явный `enclave_sgx: false`. Проверка manifest использует Python 3.11+ (`tomllib`); аппаратный запуск ещё необходим.
- Seed/oracle принимают `rudis` вместе с прежними aliases, нормализуя пары по адресам. Генерируемый native pair и вывод feeder/CLI используют `rudis`. Существующий upstream source ticker `COEN` сохранён: это входной инструмент внешнего поставщика котировок, не публичный denom сети. Он не заменяется несуществующим инструментом Rudis.
- MCP/клиенты знают новый ID, название и decimals. Старые RPC defaults для новой сети удалены. Сетевые selectors/env/CLI names сохранены там, где они операторские. Подключение к другому ID через заданный RPC отклоняется в intent/Intex resolvers.
- Старые application/token/router deployments не назначаются новому chain ID автоматически. MCP использует явные `OUTBE_INTENT_ROUTER`, `OUTBE_INTENT_TOKENS` и `OUTBE_INTEX_ADDRESSES`; фиксированные runtime precompiles остаются в реестре. Intent examples требуют адреса через прежние `ROUTER`, `INPUT_TOKEN`, `OUTPUT_TOKEN`.
- Stablecoin address/EIP-712 vectors пересчитаны через Foundry `cast` из новых preimages; Rust и Solidity проверки подтверждают результаты. Технические domain names сохранены.
- `release/testnet-genesis.json` перепривязан через canonical `TeePolicyScheduleV1` codec и schedule hash; helper — `crates/blockchain/primitives/examples/rebind_tee_fixture.rs`. Этот исторический release fixture сохраняет DCAP rules и не является launch genesis unattested Rudis.
- OCOMP final fixture пересоздан штатным `xtask ocomp final-artifacts`: новая chain binding и founder signatures, корректные fork-install/genesis-final. Frozen protocol bundle и capacity evidence не менялись. Повторный `--check` проходит.

### Подключение после фактического deployment

- MCP: `--rpc` или прежний `OUTBE_RPC`, без default домена.
- Foundry и новые Intex chain definitions: `OUTBE_RPC_URL`.
- Intent examples: прежний `OUTBE_TESTNET_RPC`.
- `OUTBE_INTENT_TOKENS`: JSON `symbol -> chain ID -> deployed token address`. Native `rudis` на `70860602` остаётся zero address; `wrudis` на внешней сети задаётся после её deployment.
- `OUTBE_INTEX_ADDRESSES`: JSON `network key -> contract key -> deployed address`; network keys `rudis-rehearsal` и `bsc-testnet`.
- Пока адреса не заданы, cross-chain операции сообщают об отсутствии конфигурации. Это не подтверждение наличия контрактов по какому-либо прежнему адресу.

### Пройденные локальные проверки

| Проверка | Результат |
| --- | --- |
| `cargo build --offline -p outbe-chain` | Сборка успешна после изменений metadata |
| `cargo test --offline -p outbe-primitives --lib` | 305 passed |
| Primitives: `stablecoin_namespace`, `stablecoin_vectors`, `stablecoin_fork_vectors` | 20 passed, включая chain-bound vectors |
| Gem/Nod/Tribute `--lib token_uri` | 3 passed: фактический JSON декодирован |
| Feeder `--bin outbe-feeder config` | 21 passed |
| CLI `commands::oracle`, `zerofee` | По 4 passed, включая native aliases/output |
| `python3 -m unittest scripts.test_create_genesis` | 77 passed, включая реальное завершение launcher на host без SGX |
| `scripts.tests.test_prepare_network`, `scripts.tests.test_mainnet_network_profile` | 14 passed с актуальным debug binary |
| `scripts.release.tests.test_release_manifest` | 15 passed в локальном cached Python 3.12 с jsonschema |
| OCOMP `--features ocomp-integration --test ocomp_final_fixture` | 2 passed: hashes/binding/node load |
| `xtask ocomp final-artifacts ... --check` | Выходные артефакты совпадают с deterministic generation |
| `contracts/tokens`: `forge test --offline` | 65 passed: metadata, deposit/withdraw, bridge и deployment guards |
| `contracts/precompiles`: Solidity vectors | 11 passed |
| Intex `IntexNFT1155MetadataTest` | 13 passed |
| MCP `npm run typecheck` | Успешно |
| MCP identity tests | 2 passed: native aliases, явные deployments, precompile/bridge mapping |
| `git diff --check`, targeted rustfmt | Успешно |

Повторная проверка OCOMP fixture:

```sh
target/debug/xtask ocomp final-artifacts \
  --capacity testing/e2e-harness/fixtures/ocomp-final-v1/artifacts/generated-capacity-v1.json \
  --base-genesis testing/e2e-harness/fixtures/ocomp-final-v1/base/genesis.json \
  --validators testing/e2e-harness/fixtures/ocomp-final-v1/base/validators.json \
  --release-artifacts-dir testing/e2e-harness/fixtures/ocomp-final-v1/artifacts \
  --output-dir testing/e2e-harness/fixtures/ocomp-final-v1/artifacts --check
```

### Незакрытые проверки и решения

1. Решение по пункту 5 закрыто ответом пользователя: RPC/ABI rename реализован; подробности и новые проверки ниже.
2. MCP полный локальный прогон: 17 passed / 3 failed; три HTTP tests падают на `listen EPERM 127.0.0.1` до RPC assertions. Запрошен повтор вне sandbox; результат пока не получен. Фактический node HTTP smoke `eth_chainId` / `net_version` не выполнен. Default fullnode mode в текущем node требует `--upstream`; запуск с искусственно отключёнными startup guards не проводился.
3. Расширенный primitives `--tests`: trybuild snapshot `storage_handle_thread_spawn.stderr` расходится с дополнительной диагностикой текущего rustc. Сам запрет Send работает; source, compile-fail case и expected stderr совпадают с HEAD. Это не успешный полный прогон.
4. `xtask --test sgx_release`: 16 passed / 1 failed. Исходный mainnet workflow уже содержит `runs-on: testnet-release-sgx`, тогда как тест запрещает любое `testnet`. Workflow не менялся; ошибка подтверждена чтением HEAD. Mainnet CI policy не исправлялась в рамках ребрендинга.
5. `scripts.test_seed_genesis_protocol_constants`: 13 passed / 1 failed. Уже в HEAD `seed-testnet-lowstake.json` содержит `metadosis`, а тест требует отсутствия этого ключа. Экономический seed не удалялся ради зелёного результата; новый тест равенства oracle storage для aliases проходит.
6. Crosschain Solidity suite не компилируется offline: в проекте отсутствуют pinned dependencies, включая OpenZeppelin, LayerZero и forge-std. Intent examples typecheck не запущен: локальный `node_modules/.bin/tsc` отсутствует. Соответствующие клиентские конфигурации проверены чтением, но не объявляются прошедшими эти suites.
7. Подписанный SGX manifest с attestation none, bootstrap/finalization/restart и внешний RPC на настоящем SGX-host ещё не проверялись. Это отдельный аппаратный запуск вне текущего локального scope. DNS, существующие сети, deployment, публикации, commit/push/PR не выполнялись.
8. Graph project найден, однако generation/coverage по-прежнему UNKNOWN: `index_status`, `search_graph`, `check_index_coverage` не ответили. Последний полный coverage запрос после wire rename включал 182 evidence paths и relevant scopes; результат остаётся UNKNOWN. Использованы прямое чтение и проверки поведения; исчерпывающий graph-аудит не заявляется.


## Подтверждённое переименование RPC/ABI

После ответа пользователя «да сделай» внесён общий набор изменений сервера и потребителей:

| Прежнее публичное имя | Новое имя |
| --- | --- |
| `outbe_*` JSON-RPC methods | `rudis_*` |
| `wcoen()` | `wrudis()` |
| `mineCoen(...)` | `mineRudis(...)` |
| `getCoenExchangeRateFor(uint16)` | `getRudisExchangeRateFor(uint16)` |
| `CoenMined` event | `RudisMined` |
| `relayBidsToOutbe(uint32)` | `relayBidsToRudis(uint32)` |
| TargetRouter public getter `OUTBE_CHAIN_ID()` | `RUDIS_CHAIN_ID()` |
| ABI/decoded JSON field `wcoen`, parameter `_wcoen` | `wrudis`, `_wrudis` |
| ABI event field `coenUsdRateMinor` | `rudisUsdRateMinor` |
| TargetRouter ABI constructor parameter `outbeChainId_` | `rudisChainId_` |

RPC clients обновлены в CLI, operator, follower/engine, OCOMP, MCP, harness и эксплуатационных scripts. Rust-типы `OutbeApi*`, внутренние snake_case методы клиентов, внутренние функции `mine_coen`, storage member `OriginRouterStorage.wcoen`, storage namespace `outbe.intex.*` и подпись `outbe/<ledger>/derive-keys/v1` сохраняются. Имена файлов, бинарников, contracts/types и env не изменялись. Публичный immutable getter TargetRouter входит в ABI и поэтому переименован; одноимённые env-переменные не затрагивались.

Node регистрирует только модуль `rudis`. `--http.api outbe` / `--ws.api outbe` остаются операторскими aliases, нормализуемыми в `rudis`; наличие этих selectors не включает старые методы. Генерируемые launch scripts используют canonical selector `rudis`.

Семь изменённых ABI exports пересозданы через `forge inspect --offline ... abi --json`: IOracle, IGratisFactory, IPromisFactory, IDesis, OriginRouter, IOriginRouter, TargetRouter. Текстовый поиск прежних hex selectors и event topic в проверенных исходниках и JSON fixtures не выявил потребителей с зашитым старым calldata/topic. Это не аудит произвольного бинарного bytecode. Изменение имён fields не меняет layout или ABI types; изменение имён методов/events намеренно меняет selectors/topics.

Новые проверки:

- `cargo build --offline -p outbe-chain`: успешная пересборка ноды после изменения namespace.
- `cargo test --offline -p outbe-chain --bin outbe-chain outbe_rpc_module_validator`: 3 passed (новое имя, нормализация старого selector, отклонение неизвестного).
- `cargo check --offline -p outbe-e2e-harness --features ocomp-integration --tests`: успешно, включая обновлённых RPC/ABI потребителей.
- Реальная JSON-RPC диспетчеризация в `outbe-rpc`: `rudis_radicleStatus` возвращает результат; `outbe_radicleStatus` возвращает `-32601`; все зарегистрированные custom methods имеют namespace `rudis`. Это проверка in-process RpcModule, не HTTP/hardware smoke.
- Rust Oracle/PromisFactory/GratisFactory/Desis: 276 tests passed с новыми ABI bindings и прежней арифметикой.
- Intex OriginRouterProceeds/TargetRouterProceeds/PatternADefer: 24 tests passed, включая новый getter и отклонение старого.
- MCP ABI encoding/decoded JSON, identity, market scales, crypto: 10 tests passed; TypeScript typecheck проходит.
- Python create_genesis/prepare_network/mainnet-profile: 91 tests passed с canonical RPC selector в выходных scripts.
- Все `contracts/*/abi-export/*.json` прочитаны как JSON: в ABI `name` fields не осталось `coen`/`outbe` (без учёта сохраняемых contract/type names).
- `git diff --check` и targeted rustfmt: успешно.

Один прогон был прерван `No space left on device`. Удалён только регенерируемый `target/debug/incremental` (44 GiB); прерванные проверки повторены. Данные сети и исходники при очистке не затронуты. Старые ограничения полного прогона и HTTP/hardware smoke, перечисленные выше, остаются актуальны; успешная in-process проверка их не заменяет.
