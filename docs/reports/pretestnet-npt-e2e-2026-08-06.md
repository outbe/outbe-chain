# Генеральная pre-testnet NPT/E2E-приёмка — 6 августа 2026

Статус: приёмка завершена с итогом **NO-GO**. Этот документ — журнал
доказательств для `outbe-chain-08n`, а не список задач. Состояние работ и
зависимости хранятся в Beads.

## Фиксированная точка

- Ветка финальной приёмки: `test/e2e` (исходная работа началась в
  `test/end-to-end`).
- Исходный commit: `96e31331e86fe781362eb5298ae8be6bb15b11c4`.
- Исходный `origin/main`: тот же commit.
- Финальный functional-capacity evidence собран на clean commit
  `018c8967c01a3c720233d196e2bc8c7ca4e5bca5`; последующий отчётный коммит
  меняет только этот журнал и Beads export. Для каждого runtime-сценария ниже
  указан собственный точный source SHA, run directory и SHA-256 evidence.
- Workspace: 67 Rust packages, 244 targets.
- Общее решение по выпуску: сеть обновляется через wipe; эта проверка не
  изменяет testnet.
- LocalNet, release-сборки и E2E запускаются вне sandbox.
- Live TEE-профиль этой приёмки: production enclave binary под реальным
  `gramine-sgx`, `sgx.remote_attestation=none`, production NodeHost session и
  chain policy `GramineDirectDev`. Test-only mock и non-SGX `gramine-direct` не
  закрывают этот live acceptance.

Среда на старте:

- `native-dcap`, hardware SGX E2E и связанные release lanes исключены из этой
  приёмки прямым решением пользователя от 6 августа 2026;
- Docker и Foundry присутствуют;
- `cargo-nextest`, `cargo-machete`, `cargo-deny`, `cargo-audit` и
  `cargo-udeps` присутствуют;
- `mongosh` отсутствует; LocalNet обязан проверять MongoDB через собственную
  managed-обвязку, а не считать наличие CLI условием работы;
- `contracts/crosschain/mise.toml` пока не trusted в локальном mise. Это
  environment precondition, а не дефект продукта.

Статусы строк:

- `NOT RUN` — команда или сценарий ещё не запускались на фиксированной точке;
- `PASS` — приложено актуальное воспроизводимое доказательство;
- `FAIL` — получено воспроизводимое противоречие ожидаемому инварианту;
- `BLOCKED` — проверка требует отсутствующей внешней возможности;
- `MISSING` — требуемого исполняемого сценария пока нет;
- `PARTIAL` — получено прямое runtime-доказательство части инварианта, но
  сценарий не достиг всего критерия PASS;
- `N/A` — проверка неприменима с записанным обоснованием.

## Правила triage

1. Падение сначала воспроизводится отдельно. До этого оно не называется
   дефектом продукта.
2. Для подтверждённого дефекта фиксируются владелец инварианта, причина,
   минимальный regression test и blast radius.
3. Небольшой или средний локальный дефект исправляется отдельным цельным
   коммитом без соседнего рефакторинга.
4. Blocker/critical, изменение consensus/canonical/security поведения,
   архитектурное изменение, новый широкий production path или большой
   многофайловый blast radius останавливают изменения. Находку независимо
   проверяют три агента, после чего требуется решение пользователя.
5. Зелёный component test не заменяет process E2E. Живые процессы без
   достигнутого результата также не считаются PASS.
6. Подготовленный в genesis `WorldwideDay` может использоваться для узкой
   проверки membership/snapshot/deadline, но не доказывает runtime-создание дня
   или полный Metadosis lifecycle.
7. Funding, ключи, Oracle initial data и consensus-параметры допустимы как
   genesis inputs. Готовые `JobIntent`, `ResultVoteV1`, `LysisResultV1`, NOD или
   post-start storage writes запрещены в production-path acceptance.
8. Каждый E2E обязан фиксировать отдельно: setup inputs, runtime cause,
   наблюдаемое chain evidence, terminal outcome и restart evidence. Сценарий не
   может доказывать более широкую гарантию, чем исполняемый им путь.

## Матрица сборки и статического контроля

| ID | Профиль | Команда | Проверяемый инвариант | Результат | Доказательство |
|---|---|---|---|---|---|
| B01 | Workspace debug | `mise run build` | Все штатные бинарники, production enclave и OCOMP integration harness собираются | PASS | Auditable workspace build, вспомогательный mock binary и `ocomp-integration` harness собраны вне sandbox на `a157277d`; mock binary не считается live TEE evidence |
| B02 | Workspace type-check | `mise run check` | Все workspace packages типизируются | PASS | Exit 0 на `25e593a2`; workspace check завершён без ошибок |
| B02a | Non-native feature profiles | Точечные `cargo check` для mock, OCOMP integration и остальных поддерживаемых non-native features | Все входящие в testnet-подготовку feature-профили типизируются без `native-dcap` | PASS | Пять профилей типизируются: enclave `mock`, harness `ocomp-finality-fixture`, harness `ocomp-integration`, consensus/chain `test-marshal-drop`, TEE `tee-attestation-v1`. Единственный RED был tests-only unused import вне `ocomp-integration`; после feature-gate correction тот же `ocomp-finality-fixture --all-targets` PASS |
| B03 | Workspace release | `mise run build-release` | Release-профиль собирается auditable | PASS | Актуальный повтор на базе `9b8de5591008` плюс finalized-RPC/retention/export-replay slices: exit 0 за 2m38s; `outbe-chain` SHA256 `f3efe7912b2356a661f07d5f4480f07bcd080c75cbe694dbf85fe9d39d5985c0`, остальные exact binaries записаны в evidence обоих SGX/no-DCAP сценариев |
| B04 | Formatting | `mise run fmt-check` | Форматирование воспроизводимо | PASS | Исходный FAIL был только rustfmt в двух OCOMP result-attestation файлах; `25e593a2`, повторный check PASS |
| B05 | Clippy | `cargo clippy --all-targets -- -D warnings` и точечные non-native feature profiles | Все входящие в scope targets без warnings; native DCAP не запускается | PASS | Актуальный повтор на рабочем дереве нашёл один `useless_conversion` в GemFactory; лишний `.into()` удалён без изменения семантики. Повторный full-workspace all-target Clippy exit 0; GemFactory 23/23 tests PASS |
| B06 | Dependency hygiene | `mise run audit-machete` | Нет неиспользуемых workspace dependencies | PASS | Exit 0 на `25e593a2`; unused dependencies не найдены |
| B07 | Supply-chain policy | `mise run audit-deny` | Advisory/license/ban/source policy проходит | PASS | После явного решения разрешить Boost Software License 1.0 в `deny.toml`: license-only gate и полный повтор на `260b9821` завершились с exit 0; `advisories ok, bans ok, licenses ok, sources ok` |
| B08 | RustSec | `mise run audit-rustsec` | Lockfile не содержит неразобранных применимых уязвимостей | PASS | Exit 0 на `260b9821` с отдельной актуальной cargo-audit DB. `RUSTSEC-2026-0118` и `-0119` для транзитивного `hickory-proto 0.25.2` признаны неприменимыми: Outbe явно отключает DNS discovery, DNSSEC feature не собран, уязвимые NSEC3/name-compression пути недостижимы. Ранее принятое исключение `RUSTSEC-2025-0055` также синхронизировано во всех gates. Пять unmaintained и четыре unsound warning не скрыты и вынесены в R02 |
| B09 | Unused dependencies | `mise run audit-udeps` | Nightly udeps проходит для всех targets | PASS | `cargo +nightly udeps --workspace --all-targets` вне sandbox: exit 0, `All deps seem to have been used`. До прогона исправлены только пять nightly-incompatible tail `bail!` expressions в harness/chain, без изменения runtime-семантики |
| B10 | Consensus layering | `mise run audit-consensus-deps` | Consensus не зависит транзитивно от EVM | PASS | `consensus dependency boundary: OK` на `96e31331` |
| B11 | Generated OCOMP registry | `cargo xtask ocomp registry --check` | Generated registry совпадает с нормативными входами | PASS | Exit 0 на `96e31331` |
| B12 | OCOMP functional capacity | `target/release/outbe-e2e --tags @ocomp-capacity --concurrency 1 --no-resolve-ports --tee sgx-no-attest --all --no-cleanup` | Один production-shaped прогон обрабатывает shard-cap-plus-one population без искусственного ограничения ресурсов | PASS | Clean revision `018c8967`: 1 scenario, 9/9 steps PASS за 1 602 504 ms. На всех четырёх валидаторах подтверждены 257 distinct Tribute, production Supervisor/Workers, q=3 Lysis apply, 257 NOD и historical replay. Запуск использовал все ресурсы хоста — без `CPUQuota` и `MemoryMax`. Evidence `/tmp/ocomp-b12-single-unlimited.ICK6Nj/evidence/scenario-001.json`, SHA-256 `7b0cff678ae29041a4ecf9b1c62576173617c4c8d3ecca30eeccfe520d01aa94` |
| B13 | OCOMP shape | `cargo xtask ocomp shape --check` | Generated shape freeze совпадает с нормативными входами | PASS | Exit 0 на `96e31331` |
| B13a | Final fixtures | `cargo xtask ocomp final-artifacts ... --check` с checked-in Final inputs | Canonical Final OCOMP artifacts воспроизводимы | PASS | Исходный RED подтвердил известный drift после normative system-gas изменений. Две независимые генерации дали побайтово одинаковый набор; checked-in downstream profile/bundle/fork/genesis artifacts перегенерированы, повторный `--check` exit 0; capacity manifest/profile не изменились |
| B14 | Storage layouts | Репозиторный layout-hash gate | Consensus storage layout не изменился незаметно | PASS | Workspace `storage_layout` filter exit 0; Metadosis `tests::state::test_storage_dsl_layout_slots` PASS и повторно связал exact slots с `METADOSIS_STORAGE_LAYOUT_V1_HASH` |

## Матрица тестов

| ID | Граница | Команда | Проверяемый инвариант | Результат | Доказательство |
|---|---|---|---|---|---|
| T01 | Workspace unit/integration | `mise run test` | Все Rust unit/integration tests и doctests проходят | PASS | `cargo nextest run --workspace --no-fail-fast`: 4078/4078 PASS, 19 slow, 24 skipped, 794.483 s; `cargo test --doc --workspace`: 9 compile-fail doctests PASS, один документированный пример ignored, остальные crates без doctests; HEAD `86061f3d` |
| T02 | Non-native feature tests | Workspace tests плюс mock/OCOMP integration и другие используемые non-native feature profiles | Все feature combinations в scope исполняют tests | PASS | Вне sandbox зелёны: enclave `mock` 137/137 и integration targets; harness `ocomp-finality-fixture` 126 tests (+ trybuild/ledger); harness `ocomp-integration` 150/150, 1 ignored, все integration targets; chain `test-marshal-drop` 45/45; primitives `tee-attestation-v1` 239 tests и 6/6 compile-fail cases. Первый sandbox-прогон mock дал ложные EPERM на local endpoints; тот же exact прогон вне sandbox PASS |
| T03 | Main contract aggregate | `mise --cd contracts run build`, `test`, `lint` | Intent, tokens, vault, smart-account, precompiles и Intex проходят | PASS | Node 22.19.0, Yarn 4.9.3, Forge 1.7.1; immutable install 1.45s; aggregate build PASS 1m56.65s; aggregate tests PASS 1m23.74s (intent 98/98, tokens 45/45, crosschain 67/67, smart-account 53/53, precompiles 11/11 и все Intex suites); CI-equivalent lint PASS 13.04s. Acceptance явно добавил Intex Solhint и сохранил CI exemption smart-account от Solar; текущий aggregate lint task с CI расходится и не использовался как подмена |
| T03a | Crosschain contracts | `mise --cd contracts/crosschain run build`, `test`, `lint` | Исключённый из aggregate crosschain workspace проверен отдельно | PASS | `contracts/crosschain/mise.toml` оставлен untrusted; эквивалентные прямые Forge-команды: build PASS 15.25s, tests 67/67 PASS 0.08s, high-severity lint PASS 0.53s. Contracts/CI/mise worktree после прогона чистый |
| T04 | GramineDirectDev live SGX-no-attest E2E | `outbe-e2e --tee sgx-no-attest --validators 4 --all --name "Entire committee recovers after all enclaves restart"` | Четыре validator nodes используют production enclave, реальный SGX/EGETKEY и NodeHost; DCAP/QVL не вызываются и не монтируются; sealed restart сохраняет ключ и финализацию | PASS | 1 scenario / 3 steps PASS; Tribute visible at block 6, all four enclaves and nodes restarted, sealed offer keys restored and finalization resumed. Scenario receipt SHA256 `0fe2ca2568f01a0216ee4f9619da621778111f4d14b0f348da0d78d778107757`; tested production binary SHA256 `ca60ada35470b87ec05e66339abe892462370c6305fc5502fb6a63d17d7f9906`; harness binary SHA256 `3a7843f71855f65f183588a55501263ca0dc17bf3c1481291931d3d374ee21ab` |
| T05 | Hardware SGX/DCAP E2E | `mise run e2e-sgx` | Исключено из текущей приёмки | N/A | Решение пользователя: native DCAP не проверять |
| T06 | Native-DCAP SGX release | Hardware/native-DCAP release gates | Исключено из текущей приёмки | N/A | Решение пользователя: native DCAP не проверять |
| T07 | OCOMP evidence closure | Офлайн-проверка evidence B12 | Проверяемый evidence связан с точным release SHA и профилем запуска | RETIRED AS E2E | Отдельный `ocomp-poc-closure-run` повторял уже консолидированные OCOMP workflows и исключён как дублирующий E2E. После единственного B12 проверяется его сохранённый evidence без запуска новой сети |

## Обязательный production-shaped OCOMP E2E

Нормативная цепочка:

`finalized JobIntent → tentative pin → finality promotion → finalized export →
Supervisor lease/dispatch → Worker computation → signed validator result →
domain convergence → Lysis apply → NOD state and bodies`.

Прямой ввод `ResultVoteV1` допустим в узких component tests, но не закрывает ни
одну строку ниже.

| ID | Проверка | Критерий PASS | Результат | Доказательство |
|---|---|---|---|---|
| O01 | Настоящий JobIntent | Intent создаётся chain lifecycle после Tribute, а не тестовым setter | PASS | `@ocomp-public-apply`, `/tmp/outbe-o08-cas-proof.wvOBJW`: chain-created intent `0x2654…64b2`, request block 32, finality 33, open 37; 13/13 steps PASS |
| O02 | Production coordinator | Используется тот же `open()`, что в ноде; test-only `open_with_retained_tributes` не подменяет путь | PASS | Тот же release process E2E запускает embedded node coordinator и внешние production roles; прямого test-only constructor/result injection нет |
| O03 | Arming race | В E2E исполняются `finalized_marker` и атомарный arm; advancing marker не создаёт вечный retry | PASS | Public-path job открылся на block 37 и дошёл до activation на block 40 через настоящий pin/arming/export путь |
| O04 | Export и lease | Finalized snapshot экспортирован и принят Supervisor с точной identity | PASS | Актуальный `@metadosis-fresh-devnet`: production SnapshotExporter прочитал typed JobIntent и request-bound openings по exact finalized block, Supervisor/Worker завершили job `0x697d…9319`; evidence `scenario-002.json`, SHA256 `45e2e63c7c3181ff3516fe95370b16bea722663bfca701b5169db0a6cbf38ffb` |
| O05 | Worker result | Worker реально выполняет unit и возвращает результат, а не только остаётся жив | PASS | Четыре независимых result-vote tx с разными OCOMP signers, одинаковый result digest `0xb065…ae19`, без прямого ввода результата |
| O06 | Validator submission | Каждый ожидаемый validator domain подписывает и отправляет результат своего вычисления | PASS | Четыре успешных canonical vote transactions в block 40; calldata 1604, gas_used 0 в system lane |
| O07 | Terminal outcome | Job достигает `Completed`; quorum, Lysis roots/manifest и on-chain result совпадают | PASS | Activation и certified generation в block 40; Tribute=1, NOD=1; atomic quorum apply и exact completed retry подтверждены. Evidence SHA256 `d528dab47cc7b0179bba4fc41e73d161d612a2d62d36ec04b84038f25970bf25` |
| O08 | NOD materialization | FullNode и validators имеют одинаковый NOD state, bodies и membership proofs | PASS | Validator-0 и keyless FullNode независимо сохранили побайтово одинаковый canonical `ResultChunkV1`/`NodActionV1` для job `0x68f5…fecf`; action несёт все canonical NOD body fields. `NodMembershipProofV1` для ordinal 0 проверен против finalized `ActiveNodSetV1` и `nod_root=0xa224…0721`; state root и certified generation FullNode совпали с validator. Тот же release SGX run, 13/13, evidence SHA256 `d528dab47cc7b0179bba4fc41e73d161d612a2d62d36ec04b84038f25970bf25` |
| O09 | Restart/replay | Supervisor, Worker, validator и FullNode restart сохраняют exact-retry/sign-once свойства | PASS | Актуальный `@ocomp-e2e-008`: `/tmp/outbe-npt-lifecycle.PYZ19I/run/run-1786298662-3468315/scenario-1`, exact completed generation/vote replay после restart, clean log audit; evidence SHA256 `bda799255e76c40a43a76c894225e52fec2f1fbc1baa0d1caf4fd73bcb03cfdf` |
| O10 | Исторический membership | Старый job сохраняет pinned ACTIVE snapshot, новый использует обновлённый ValidatorSet | PASS | `@ocomp-dynamic-overlap`, `/tmp/outbe-e2e-dynamic-release.KQm6Z3`: old 4/q3 и current 5/q4 одновременно; 13/13 steps PASS; SHA256 `fde92cfc8d61d0c3cb1585d814ab7c980e0f9a921eb804eb082de2aa72b099ee` |
| O11 | Deadline accountability | Все pinned validators голосуют в 1800-block window; missing current ACTIVE получает jail | PASS | Тот же dynamic-overlap E2E доказал deadline accountability/jail и FullNode compute-only роль без изменения исторического snapshot |

### Trust-аудит OCOMP harness на `c95ced29`

| ID | Сценарий/граница | Разрешённая подготовка | Запрещённая подмена | Фактическое покрытие | Статус/доказательство |
|---|---|---|---|---|---|
| H01 | `@ocomp-dynamic-overlap` | Два согласованных `WorldwideDay` и короткий vote window записаны в immutable genesis до запуска | Нельзя считать это доказательством Create/FORMING/phase transitions | Реальные Tribute tx, два chain-created JobIntent, pinned 4/q3 и 5/q4 snapshots, FullNode compute-only, Worker results и публичные validator votes | PARTIAL. `/tmp/outbe-e2e-harness-2112543`: путь дошёл до обоих jobs и затем обнаружил test-only deadline drift 1800 вместо genesis 120. `/tmp/outbe-e2e-harness-2249770`: после RED→GREEN regression дошёл до fifth FullNode state parity, после чего был сознательно остановлен для trust-аудита; PASS не заявляется |
| H02 | `@ocomp-public-apply` | Один OFFERING day, Oracle VWAP и funding подготовлены в genesis | Не доказывает runtime-создание WWD | После JobIntent harness наблюдает настоящие worker results, публичные result-vote tx, одинаковую activation/NOD state и distinct signers; FullNode CAS chunk и NOD membership proof проверяются против finalized root; direct result submit helper не вызывается | PASS. `/tmp/outbe-o08-cas-proof.wvOBJW`, 13/13 steps, clean audit, SHA256 `d528dab47cc7b0179bba4fc41e73d161d612a2d62d36ec04b84038f25970bf25` |
| H03 | `@metadosis-fresh-devnet` | Ключи, funding, Oracle initial data, immutable timing constants и OCOMP install | Seeded active WWD очищается до старта; JobIntent/result/NOD и post-start storage injection запрещены | Block 1 создаёт WWD в FORMING; production Cycle формирует immutable day limit; реальные blocks проходят фазы; 257 CLI offers отправляются максимум по 2; затем production Supervisor/Worker/vote/Lysis/NOD | PASS. Актуальный совместный release-прогон `@metadosis-fresh-devnet or @ocomp-e2e-008`: 2/2 сценария, 24/24 шага, настоящий SGX/Gramine, `sgx.remote_attestation=none`, `--tee sgx-no-attest`, clean log audit. Fresh-flow провёл 257 публичных Tribute через все Metadosis-фазы, создал finalized JobIntent, получил три matching validator results, атомарно применил Lysis и создал ровно 257 NOD; validator-0 реконструировал certified generation из canonical history. Run: `/tmp/outbe-npt-lifecycle.PYZ19I/run/run-1786298662-3468315/scenario-2`; evidence SHA256 `629c4c3bd92de4216de12941f2ff6252576d4abab0885f6471aea64c47496332`; `outbe-chain` SHA256 `f3efe7912b2356a661f07d5f4480f07bcd080c75cbe694dbf85fe9d39d5985c0` |
| H04 | Deadline source equivalence | `computeVoteWindowBlocks` материализуется в immutable genesis constants | Test-only builder не вправе использовать отдельный hardcoded default | Production `outbe-chain ocomp genesis` на том же profile сформировал 120; harness раньше сформировал 1800 | PASS focused regression: `dynamic_membership_fixture_schedules_two_distinct_public_jobs` сначала RED 1800/120, после локальной test-only правки GREEN; `cargo fmt --all -- --check` и build harness PASS |
| H05 | Direct-result injection audit | Direct submit helpers разрешены только в negative/retry/component steps | Happy-path `JobIntent → Worker → vote → Lysis → NOD` не может ими пользоваться | `production_ocomp_domains_process_job_intent` проверяет pre-state/liveness; terminal step читает finalized public tx/accountability/activation/NOD. Прямые submit helpers находятся в отдельных mutation/retry шагах | PASS: source audit дополнен прошедшими H02/H03 runtime-сценариями без direct-result injection |
| H06 | Embedded OCOMP crash restart | Harness выполняет dirty restart с сохранением production datadir и одинаковых durable checkpoints; задержка старта не используется как обход | Нельзя удалять/откатывать checkpoint, ждать случайный дополнительный блок или ослаблять canonical hash validation | Restart/replay сценарий восстанавливает embedded OCOMP checkpoint, exact result и sign-once state | PASS. `@ocomp-e2e-008`, 9/9 steps, `restart_replay_verified=true`; `/tmp/outbe-e2e-restart-release.YIgHvA` |
| H07 | Consensus restart с асимметричным unfinalized head | Все nodes имеют один certified anchor, но часть успела сохранить следующий unfinalized head/view | Нельзя выравнивать heads перед остановкой, чистить WAL или сбрасывать safety lock ради зелёного теста | Controlled-time flow дважды останавливает и последовательно поднимает весь committee с сохранёнными datadir/WAL/CE; после каждого запуска требует общий следующий finalized block до старта OCOMP roles | PASS. Тот же H03 run прошёл оба coordinated restart без выравнивания heads, очистки WAL или отката CE, затем finalized boundary H300 и полный Lysis/NOD. Run: `/tmp/metadosis-boundary-retry.3ECYgm/r/run-1786286533-3079681/scenario-1` |

До PASS строк O01–O09 нужны одновременно H03 и отдельный canonical LocalNet row,
который использует production generators/construction path. H01/H02 сами по себе
не могут закрыть полный production-flow, даже если зелёные.

## Время, накопление и эксплуатационные границы

| ID | Сценарий | Критерий PASS | Результат | Доказательство |
|---|---|---|---|---|
| L01 | Переход суток | `OFFCHAIN_PENDING` и WorldwideDay FSM согласованы после истечения окна | PASS | `reducer_state_time_region_table_covers_every_persisted_status`, `reducer_is_a_noop_for_processing_states_after_the_process_boundary`, `advance_active_worldwide_days_advances_status_without_creating_or_settling` и `test_cold_start_uses_materialized_short_genesis_schedule` PASS: reducer сохраняет `OFFCHAIN_PENDING` после process boundary, production advance-path остаётся согласованным, а границы берутся из materialized `GenesisProtocolParametersV1`. Полный release E2E остаётся агрегатным доказательством; отдельный core-дубликат с injected state не создавался |
| L02 | Два живых дня | Терминальные записи и capacity считаются в правильной проекции, не глобально | PASS | `terminal_cap_is_per_worldwide_day_not_global` PASS (`365 + 1` между двумя WWD через production lifecycle commands); `two_concurrent_live_days_do_not_share_terminal_budget` PASS (`364 + 364 = 728` одновременно). Полный release E2E остаётся агрегатным доказательством; отдельный injected core-дубликат не создавался |
| L03 | Длительное накопление | Pin/job registries освобождаются раньше потолка; нет halt/livelock | PASS | Production `open()` без retention writer автоматически переводит due `Terminal → Released` и сохраняет его после restart. При 75% pressure существующая compaction удаляет только `Released`, сохраняет старые `Tentative`, `Finalized` и future `Terminal`, пропускает новый candidate и точно восстанавливается. `OCM-PIN-001` 22/22, outbe-node Clippy PASS. Неустранимые 65 535 live pins по-прежнему fail-closed; бесконечная live capacity не заявляется |
| L04 | Delayed finality | Reconciliation запускается от финальности и не теряет tentative pin после фиксированного числа попыток | PASS | `ocm_pin_001_old_tentative_survives_repeated_finality_misses_and_restart`: девять последовательных недоступных finality observations сохраняют побайтово точный non-signable `Tentative`; после restart поздняя exact finality переводит ту же запись в `Finalized`. Весь `OCM-PIN-001` 21/21 PASS; age-only eviction не добавлялся |
| L05 | Non-grid epoch boundary | Follower берёт наблюдаемую границу из chain history, не вычисляет `(h-1)/L`, и live/restart проходят позднюю DKG boundary | PASS | RED: SGX/no-DCAP run `/tmp/outbe-e2e-follower-offgrid-fixed/run-1786195246-3325576` остановил FullNode после поздней самосертифицированной boundary. GREEN: `/tmp/outbe-e2e-follower-p0.ydHWpD/run-1786200688-3593462`, release binaries, `sudo`, настоящий SGX/Gramine, `sgx.remote_attestation=none`: 1/1 scenario и 11/11 steps PASS. Старый epoch остался активен после planned=60; DKG завершился на H=84, точный outgoing-finalized `CommitteePreAnnounce` разрешил activation anchor H=86, active set 4→5 применился в block 87. FullNode достиг точного finalized hash/state-root parity после boundary и повторно после restart. Исправление `outbe-chain-08n.4.1.3` не меняет follower trust, canonical/wire/EVM/genesis формы |
| L06 | Грязный process/endpoint restart | Жёсткая остановка и повторный bind тех же HTTP/ZeroMQ endpoint не лишают валидатора OCOMP роли молча | PASS | Release SGX/no-DCAP `@ocomp-e2e-008` жёстко останавливает и повторно запускает SnapshotExporter и Workers на тех же адресах с сохранённым basedir; затем перезапускает все validator nodes и node-facing OCOMP процессы. Exact vote replay и завершённая generation остаются идентичны. Устаревшие Unix control sockets в текущем транспорте отсутствуют |
| L07 | Prepared export restart | Частично подготовленный export корректно восстанавливается или отклоняется | PASS | Production-path regression покрывает empty directory и prepared-only crash: только `MissingPreparation`/`MissingReceipt` продолжают существующий exact `prepare → record_committed`, manifest/receipt остаются идентичны; прочие receipt/CAS/authority ошибки остаются fail-closed. Live SIGKILL обнаружил 5-секундный stale Mongo writer lease: exporter теперь повторяет только структурно выделенный transient startup-unavailable с существующим 1-секундным cadence; invalid config/corruption остаются fatal. Повторный release SGX/no-DCAP E2E восстановил побайтово тот же `prepared.ref`/`receipt.ref`; `outbe-ocomp` lib 47/47 и binary 9/9 PASS |
| L08 | Registry pressure | Старт с заполненным на 75% pin registry чистит только допустимый мусор и сохраняет live pins | PASS | `ocm_pin_001_pressure_watermark_compacts_only_released_records_and_survives_restart` засеивает canonical journal ровно на watermark 49 152, открывает его через production `open()`, выполняет due release и pressure insert. После compaction/restart остаются только три live records и новый `Tentative`; Released обоих reasons не воскресают |
| L09 | Production basedir | Все OCOMP роли используют canonical `validator-N/ocomp/domain-v1` внутри выбранного base path без захардкоженных внешних путей | PASS | Release SGX/no-DCAP `@ocomp-e2e-008` проверил четыре реальных domain root и обязательные bundle/result/EVM key artifacts. Harness не изобретает отдельные service UID: роли работают от пользователя запуска, как требует текущий deployment contract |
| L10 | Mongo projection outage | Внешняя проекция не блокирует consensus execution/finality | PASS | В том же release E2E managed Mongo replica set остановлен после завершённого OCOMP результата; consensus finality продвинулась минимум на два блока во время outage и ещё минимум на два после возврата writable PRIMARY |
| L11 | Coordinated restart около unfinalized proposal | Общий certified anchor восстанавливает liveness при разных локальных speculative heads/views | PASS | H03 дважды выполнил production-shaped coordinated restart на сохранённых состояниях, потребовал общий следующий finalized block и затем завершил certified boundary H300 и Lysis/NOD; `/tmp/metadosis-boundary-retry.3ECYgm/r/run-1786286533-3079681/scenario-1` |

Persistent `outbe-e2e localnet` на фиксированной точке жёстко создаёт
`TeeMode::Mock`. Live-прогон использует Cucumber runner с явным
`--tee sgx-no-attest`; запуск mock или non-SGX `gramine-direct` не будет
засчитан как требуемое доказательство.

## Известные отложенные риски

| ID | Риск | Решение текущего PR | Follow-up |
|---|---|---|---|
| R01 | `outbe_getOcompLysisOpeningsV1` выполняет историческое state/proof построение в обычном публичном RPC namespace; без отдельного admission/local-only транспорта удалённый клиент может расходовать node-local CPU и blocking pool | Не расширять текущий Tribute → NOD correctness-slice транспортом или аутентификацией; риск не считается закрытым зелёным E2E | `outbe-chain-9t3` |
| R02 | RustSec сообщает пять unmaintained и четыре unsound warning: `atomic-polyfill`, `bincode`, `derivative`, `paste`, `proc-macro-error2`, `anyhow`, два advisory для `git2` и `memmap2` | Предупреждения остаются видимыми в выводе `mise run audit-rustsec`; они не классифицированы как применимые vulnerability и не блокируют B08 | Отдельный dependency-hygiene slice при обновлении upstream dependency graph |
| R03 | Сертифицированная финальность OCOMP может прийти раньше exact local execution receipts; после восьми retries текущий retention path способен отбросить finalized boundary | Один зелёный B12 не опровергает сохранённый production-reachable RED. До durable ordered join testnet update остаётся NO-GO | `outbe-chain-ohz.2.1` |

Проверка loopback/auth для `OUTBE_OCOMP_RPC_URL` также сознательно отложена:
текущий операционный контракт предполагает, что Supervisor обращается к RPC
своей же ноды. Это допущение не устраняет R01 и не превращает его в PASS.

## Пакеты оставшейся приёмки

Исходные 18 строк `NOT RUN`/`MISSING` выполняются четырьмя функционально
завершёнными пакетами, а не 18 независимыми циклами
«изменение → сборка → E2E»:

1. Статические и OCOMP-profile gates: B02a, B09, B12, B13a, B14 и T02.
2. Контрактные workspace: T03 и T03a.
3. Controlled-time lifecycle и единый retention seam: L01–L04 и L08.
4. Dirty-start и эксплуатационные отказы: L06, L07, L09 и L10.

Текущий пакетный прогресс: `18/18` закрыты. T07 снят как дублирующий E2E,
а единственный B12 выполнен и проверен офлайн.

Внутри пакета сначала параллельно завершаются анализ, тестовый код,
документация и независимые production-правки. Затем выполняются один общий
quality batch, не более одного integration-correction pass и один release/E2E
прогон. Прогресс считается только как закрытые строки из этих 18; внутренние
подзадачи не уменьшают остаток сами по себе.

Read-only freeze перед третьим и четвёртым пакетами подтвердил два дефекта на
текущем production path:

- production вызывает `OcompRetentionCoordinator::open()` без retained-Tribute
  writer, а release-loop целиком выключен при его отсутствии. Поэтому due
  `Terminal` не становится `Released`. Текущий физический потолок журнала —
  65 535 записей (`u16`), а не старые 256; численная оценка testnet3 устарела,
  но eventual liveness failure остаётся. L03 и L08 принадлежат одному
  semantic pass `retention.rs`; age-only eviction unresolved `Tentative`
  запрещён.
- crash после durable `prepare` и до `record_committed` оставляет empty или
  prepared-only receipt directory. `RpcInputExporter` видит directory,
  получает `MissingPreparation`/`MissingReceipt` и не доходит до существующего
  exact replay. Исправление L07 должно продолжать штатный
  `prepare → record_committed` только для этих двух incomplete состояний; все
  ambiguity/conflict/CAS/authority ошибки остаются fail-closed.

Обе находки подтверждены тремя независимыми read-only review и явно разрешены
пользователем как два узких tests-first исправления. Они реализованы по одному
semantic pass в каждом production owner: retention release без writer и exact
export replay только для двух неполных crash-состояний. Новые TTL, лимиты,
wire/state/FSM формы и age-only eviction не добавлялись.

Первый live prepared-only SIGKILL дополнительно доказал integration-дефект:
MongoDB сохраняет sole-writer lease до пяти секунд, тогда как новый exporter
стартовал через две секунды и завершался до exact replay. Исправление не меняет
lease или storage семантику: только transient `StorageUnavailable` при открытии
projection повторяется с уже существующим секундным reconcile cadence; другие
startup errors остаются fatal. Повторный `@ocomp-e2e-008` на release binaries,
настоящем SGX/Gramine и без DCAP прошёл 13/13 шагов. Evidence:
`/tmp/outbe-npt-dirty-retry.0iQazs/evidence/scenario-001.json`, SHA-256
`ed5f0959ba75314f5892125378384f5220261a2c48a07976dc7740ef89467b1f`.

## Порядок выполнения

1. Быстрые read-only/static gates B04, B10, B11–B14.
2. B01, B02, B05 и T01; каждое падение проходит evidence-based triage.
3. Release и supply-chain gates B03, B06–B09.
4. T03 contract workspaces.
5. T04 canonical GramineDirectDev suite с production enclave под real SGX и
   remote attestation none; mock/non-SGX прогон используется только как
   вспомогательная component-проверка.
6. O01–O11 production-shaped OCOMP и dynamic membership acceptance.
7. L01–L10 controlled-time и dirty-start сценарии.
8. Офлайн-валидация evidence единственного B12; отдельный T07 E2E и
   native-DCAP/hardware строки T05–T06 не запускаются.
9. Полный повтор затронутых gates и итоговый go/no-go.

## Текущий вывод

Решение по обновлению testnet пока **NO-GO**. Production-shaped
путь и все 18 строк текущего пакетного прогона закрыты. Release SGX/no-DCAP сценарии
прошли настоящий block-1 Create, production Cycle, controlled-time coordinated
restart, certified DKG boundary, 257/257 публичных Tribute, finalized JobIntent,
Supervisor/Worker computation, quorum, Lysis и 257 NOD без state/result injection. Dynamic
4→5 membership, delayed finality, pressure compaction, prepared-only crash replay, Mongo outage и
full restart также имеют прямые зелёные evidence. Отдельный public-path run доказал совпадение
validator/FullNode canonical result chunks, NOD body fields и membership proof против finalized
`nod_root`.

Полный completion-аудит сохраняет один независимый NO-GO вне закрытого
18-строчного пакета: `outbe-chain-ohz.2.1`. Сертифицированная финальность может
опередить локальные execution receipts; существующая восьмишаговая retry-лестница
тогда способна отбросить точную finalized boundary. Текущий B12 прошёл, но один
зелёный функциональный прогон не опровергает уже сохранённое production-reachable
RED-доказательство этого race.

Quality task `outbe-chain-08n.2` и dynamic-OCOMP acceptance
`outbe-chain-8ui.7` закрыты после сверки каждого дочернего acceptance criterion.
`outbe-chain-08n.5` закрывает публикацию этого финального NO-GO. Задача
`outbe-chain-08n.6` по решению владельца является отдельной характеристикой
burst/certification timing и не блокирует этот go/no-go.

B12 выполнен на release artifacts, с `sudo`, настоящим SGX и
`sgx-no-attest`, без cgroup-ограничений. Офлайн-проверка подтвердила clean source
SHA, exact binary hashes, четыре успешных уникальных vote, q=3 membership,
257/257 Tribute/NOD и совпадение historical replay. Пять повторных cold-runs и
отдельный T07 E2E не являются функциональными acceptance gates.

### Оставшаяся работа

| Блокер | Владелец инварианта | Следующее действие |
|---|---|---|
| `outbe-chain-ohz.2.1` | OCOMP retention: join certified finality с exact local execution | После отдельного architecture freeze реализовать durable ordered join; текущая задача B12 этот production-дефект не скрывает и не исправляет |

Отдельный performance/capacity benchmark при необходимости может определять
собственный минимальный аппаратный профиль и повторяемость. Он не является B12:
функциональная приёмка не ограничивает CPU/RAM и не заявляет burst throughput.

### Явно пропущенные или частичные строки

- T05/T06 — `N/A`: native DCAP и hardware-DCAP lanes исключены прямым
  решением пользователя; live acceptance выполнен на настоящем SGX с
  `sgx.remote_attestation=none`.
- B12 — `PASS`: выполнен один clean-revision functional run без искусственного
  ограничения ресурсов; evidence проверен офлайн. T07 снят как дублирующий E2E.
- H01 — намеренно `PARTIAL`: seeded WorldwideDay доказывает dynamic overlap,
  но не runtime-создание дня; полный runtime lifecycle отдельно доказан H03.

## Исправленные дефекты тестового слоя

Полный workspace-прогон сначала дал 4054 PASS / 24 FAIL / 24 SKIP. Все 24
падения были отдельно воспроизведены и классифицированы как недостижимые или
устаревшие test fixtures после уже принятой production-семантики. Production
код этими исправлениями не менялся:

- `a52418c4` — system-OOG regression проверяет агрегатное исчерпание бюджета;
- `bbe74471` — CE fixture использует сертифицированную активацию;
- `4731a995` и `2af72a36` — canonical Lysis owner cursor order восстановлен в
  NodFactory и PromisLimit fixtures;
- `d949cea4` — Metadosis READY tick согласован с 50-часовым bootstrap window;
- `9364ac67` — Rewards fork fixture проходит достижимый ACTIVE/founder/Oracle
  порядок;
- `6f316961` — payload-builder fixture регистрирует Oracle pair до атомарной
  установки OCOMP profile;
- `86061f3d` — fee-history embedded-node fixture связывает founder registration
  с реально посеянным ACTIVE validator.

После этих отдельных коммитов полный T01 повторён с нуля и завершился GREEN.

Отдельный consensus-liveness defect, найденный только полным fresh-flow:

- `1a2c2930` — убран height-only переход `ValidatorSet.epoch_number`;
  активная эпоха, ACTIVE membership/hash и consensus+OCOMP snapshot теперь
  переключаются одной checkpointed-операцией только по сертифицированному
  `BoundaryOutcome`. Регрессии покрывают отсутствие boundary, delayed boundary,
  skipped epoch, rollback при ошибке snapshot и сохранение LateFinalize miss.
  Точный release SGX/no-DCAP H03 после исправления прошёл 15/15 шагов.

Остальные defect/acceptance commits этой ветки, которые входят в итоговое
доказательство:

- `82f184bc` — восстановлен production SGX/no-attestation network profile;
- `e55f94cb` — vote slot выводится из зарегистрированного OCOMP-ключа;
- `64e5dc2a` и `c95ced29` — единый WWD schedule и корректный UTC+14 genesis day;
- `88bca021` — OCOMP-роли используют identity пользователя запуска без
  выдуманных service UID;
- `53d110dc` — восстановлен full-path restart/replay;
- `0131a218` — execution read deadlines сохраняются через storage readers;
- `f9cd12cf` — follower restart якорится к genesis ValidatorSet, а не к
  текущему live set;
- `8b154fcb` — production `open()` release/pressure regressions, exporter
  prepared-only exact replay, finalized RPC path и соответствующие E2E
  evidence сведены в один acceptance slice.
- `bb328729` — authentic late OCOMP carrier даёт status-0 receipt после
  deadline, не прерывая блок и не меняя accountability;
- `1e988b8c` — release evidence tooling пересобирает release harness перед
  snapshot артефактов;
- `6446fb48`, `27416a9d` и `37a97bca` — evidence contract переведён на текущий
  HTTP/ZeroMQ transport, сохраняет точную launch identity и исполняет sign-once
  owner;
- `82ce04f4` — 53 пересекавшихся E2E-сценария сведены в 37 дополняющих
  сценариев по восьми владельцам поведения без теста на фиксированное число;
- `4cd6142f` — B12 ожидает полный committee result в одном общем 300-секундном
  окне и завершается сразу после достижения результата;
- `018c8967` — отдельный T07 network-run и пять повторных cold-runs удалены из
  функциональной приёмки как дублирование; сохранена офлайн-проверка B12.
