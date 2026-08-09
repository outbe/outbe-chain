# Генеральная pre-testnet NPT/E2E-приёмка — 6 августа 2026

Статус: выполняется. Этот документ — живой журнал доказательств для
`outbe-chain-08n`, а не список задач. Состояние работ и зависимости хранятся в
Beads.

## Фиксированная точка

- Ветка: `test/end-to-end`.
- Исходный commit: `96e31331e86fe781362eb5298ae8be6bb15b11c4`.
- Исходный `origin/main`: тот же commit.
- Текущий проверяемый HEAD: `1a2c2930` (`fix(consensus): activate validator
  epoch on certified boundary`) в чистом worktree ветки `test/end-to-end`.
  Для каждой runtime-строки ниже отдельно указывается точный run directory.
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
| B02a | Non-native feature profiles | Точечные `cargo check` для mock, OCOMP integration и остальных поддерживаемых non-native features | Все входящие в testnet-подготовку feature-профили типизируются без `native-dcap` | NOT RUN | — |
| B03 | Workspace release | `mise run build-release` | Release-профиль собирается auditable | PASS | Exit 0 за 3m09s на исходниках, зафиксированных затем как `1a2c2930`; эта release-сборка использована точным SGX/no-DCAP прогоном H03 |
| B04 | Formatting | `mise run fmt-check` | Форматирование воспроизводимо | PASS | Исходный FAIL был только rustfmt в двух OCOMP result-attestation файлах; `25e593a2`, повторный check PASS |
| B05 | Clippy | `cargo clippy --all-targets -- -D warnings` и точечные non-native feature profiles | Все входящие в scope targets без warnings; native DCAP не запускается | PASS | Первый прогон нашёл stale test references на удалённый Unix control transport; `a157277d` перевёл тесты на HTTP/ZMQ-era canonical types, повторный all-target Clippy PASS |
| B06 | Dependency hygiene | `mise run audit-machete` | Нет неиспользуемых workspace dependencies | PASS | Exit 0 на `25e593a2`; unused dependencies не найдены |
| B07 | Supply-chain policy | `mise run audit-deny` | Advisory/license/ban/source policy проходит | FAIL | Advisories, bans и sources проходят; license policy отклоняет `etag 4.0.0`, `str-buf 3.0.3`, `xxhash-rust 0.8.18` с `BSL-1.0` |
| B08 | RustSec | `mise run audit-rustsec` | Lockfile не содержит неразобранных уязвимостей | FAIL | Актуальная advisory DB `1237bbe0`: `RUSTSEC-2026-0118` и `-0119` в `hickory-proto 0.25.2`, `RUSTSEC-2025-0055` в `tracing-subscriber 0.2.25`; дополнительно 5 unmaintained и 4 unsound warnings |
| B09 | Unused dependencies | `mise run audit-udeps` | Nightly udeps проходит для всех targets | NOT RUN | — |
| B10 | Consensus layering | `mise run audit-consensus-deps` | Consensus не зависит транзитивно от EVM | PASS | `consensus dependency boundary: OK` на `96e31331` |
| B11 | Generated OCOMP registry | `cargo xtask ocomp registry --check` | Generated registry совпадает с нормативными входами | PASS | Exit 0 на `96e31331` |
| B12 | OCOMP shape/capacity | `mise run ocomp-poc-capacity` | Shape/capacity и Final-arming ограничения воспроизводимы | NOT RUN | — |
| B13 | OCOMP shape | `cargo xtask ocomp shape --check` | Generated shape freeze совпадает с нормативными входами | PASS | Exit 0 на `96e31331` |
| B13a | Final fixtures | `cargo xtask ocomp final-artifacts ... --check` с checked-in Final inputs | Canonical Final OCOMP artifacts воспроизводимы | NOT RUN | — |
| B14 | Storage layouts | Репозиторный layout-hash gate | Consensus storage layout не изменился незаметно | NOT RUN | — |

## Матрица тестов

| ID | Граница | Команда | Проверяемый инвариант | Результат | Доказательство |
|---|---|---|---|---|---|
| T01 | Workspace unit/integration | `mise run test` | Все Rust unit/integration tests и doctests проходят | PASS | `cargo nextest run --workspace --no-fail-fast`: 4078/4078 PASS, 19 slow, 24 skipped, 794.483 s; `cargo test --doc --workspace`: 9 compile-fail doctests PASS, один документированный пример ignored, остальные crates без doctests; HEAD `86061f3d` |
| T02 | Non-native feature tests | Workspace tests плюс mock/OCOMP integration и другие используемые non-native feature profiles | Все feature combinations в scope исполняют tests | NOT RUN | — |
| T03 | Main contract aggregate | `mise --cd contracts run build`, `test`, `lint` | Intent, tokens, vault, smart-account, precompiles и Intex проходят | NOT RUN | — |
| T03a | Crosschain contracts | `mise --cd contracts/crosschain run build`, `test`, `lint` | Исключённый из aggregate crosschain workspace проверен отдельно | NOT RUN | — |
| T04 | GramineDirectDev live SGX-no-attest E2E | `outbe-e2e --tee sgx-no-attest --validators 4 --all --name "Entire committee recovers after all enclaves restart"` | Четыре validator nodes используют production enclave, реальный SGX/EGETKEY и NodeHost; DCAP/QVL не вызываются и не монтируются; sealed restart сохраняет ключ и финализацию | PASS | 1 scenario / 3 steps PASS; Tribute visible at block 6, all four enclaves and nodes restarted, sealed offer keys restored and finalization resumed. Scenario receipt SHA256 `0fe2ca2568f01a0216ee4f9619da621778111f4d14b0f348da0d78d778107757`; tested production binary SHA256 `ca60ada35470b87ec05e66339abe892462370c6305fc5502fb6a63d17d7f9906`; harness binary SHA256 `3a7843f71855f65f183588a55501263ca0dc17bf3c1481291931d3d374ee21ab` |
| T05 | Hardware SGX/DCAP E2E | `mise run e2e-sgx` | Исключено из текущей приёмки | N/A | Решение пользователя: native DCAP не проверять |
| T06 | Native-DCAP SGX release | Hardware/native-DCAP release gates | Исключено из текущей приёмки | N/A | Решение пользователя: native DCAP не проверять |
| T07 | OCOMP evidence closure | `mise run ocomp-poc-closure-run` | Exact-artifact OCOMP lanes создают проверяемый evidence bundle | NOT RUN | — |

## Обязательный production-shaped OCOMP E2E

Нормативная цепочка:

`finalized JobIntent → tentative pin → finality promotion → finalized export →
Supervisor lease/dispatch → Worker computation → signed validator result →
domain convergence → Lysis apply → NOD state and bodies`.

Прямой ввод `ResultVoteV1` допустим в узких component tests, но не закрывает ни
одну строку ниже.

| ID | Проверка | Критерий PASS | Результат | Доказательство |
|---|---|---|---|---|
| O01 | Настоящий JobIntent | Intent создаётся chain lifecycle после Tribute, а не тестовым setter | PASS | `@ocomp-public-apply`, `/tmp/outbe-e2e-public-release.wHuPFB`: chain-created intent `0xb3f6…98f6`, request block 30, finality 31, open 35; 10/10 steps PASS |
| O02 | Production coordinator | Используется тот же `open()`, что в ноде; test-only `open_with_retained_tributes` не подменяет путь | PASS | Тот же release process E2E запускает embedded node coordinator и внешние production roles; прямого test-only constructor/result injection нет |
| O03 | Arming race | В E2E исполняются `finalized_marker` и атомарный arm; advancing marker не создаёт вечный retry | PASS | Public-path job открылся на block 35 и дошёл до activation на block 38 через настоящий pin/arming/export путь |
| O04 | Export и lease | Finalized snapshot экспортирован и принят Supervisor с точной identity | PASS | `@ocomp-public-apply`: production SnapshotExporter/Supervisor/Worker; terminal evidence связано с job `0x37c1…fb34` |
| O05 | Worker result | Worker реально выполняет unit и возвращает результат, а не только остаётся жив | PASS | Четыре независимых result-vote tx с разными OCOMP signers, одинаковый result digest `0x77f3…6ed`, без прямого ввода результата |
| O06 | Validator submission | Каждый ожидаемый validator domain подписывает и отправляет результат своего вычисления | PASS | Четыре успешных canonical vote transactions в block 38; calldata 1604, gas_used 0 в system lane |
| O07 | Terminal outcome | Job достигает `Completed`; quorum, Lysis roots/manifest и on-chain result совпадают | PASS | Activation и certified generation в block 38; Tribute=1, NOD=1; atomic quorum apply и exact completed retry подтверждены. Evidence SHA256 `1bdd3079ee60c03bee04569a0cdf97551972d3d071b2d889a85dca6e96ec7de2` |
| O08 | NOD materialization | FullNode и validators имеют одинаковый NOD state, bodies и membership proofs | PARTIAL | Validator state/data и certified roots доказаны; отдельное полное доказательство FullNode NOD bodies + membership proofs остаётся незакрытым |
| O09 | Restart/replay | Supervisor, Worker, validator и FullNode restart сохраняют exact-retry/sign-once свойства | PASS | `@ocomp-e2e-008`, `/tmp/outbe-e2e-restart-release.YIgHvA`: 9/9 steps PASS, `restart_replay_verified=true`, clean log audit; SHA256 `d97ea3b1cbb044ad4b3cbd2a0403fb667978ce1cb45f62077ab235e288b6524e` |
| O10 | Исторический membership | Старый job сохраняет pinned ACTIVE snapshot, новый использует обновлённый ValidatorSet | PASS | `@ocomp-dynamic-overlap`, `/tmp/outbe-e2e-dynamic-release.KQm6Z3`: old 4/q3 и current 5/q4 одновременно; 13/13 steps PASS; SHA256 `fde92cfc8d61d0c3cb1585d814ab7c980e0f9a921eb804eb082de2aa72b099ee` |
| O11 | Deadline accountability | Все pinned validators голосуют в 1800-block window; missing current ACTIVE получает jail | PASS | Тот же dynamic-overlap E2E доказал deadline accountability/jail и FullNode compute-only роль без изменения исторического snapshot |

### Trust-аудит OCOMP harness на `c95ced29`

| ID | Сценарий/граница | Разрешённая подготовка | Запрещённая подмена | Фактическое покрытие | Статус/доказательство |
|---|---|---|---|---|---|
| H01 | `@ocomp-dynamic-overlap` | Два согласованных `WorldwideDay` и короткий vote window записаны в immutable genesis до запуска | Нельзя считать это доказательством Create/FORMING/phase transitions | Реальные Tribute tx, два chain-created JobIntent, pinned 4/q3 и 5/q4 snapshots, FullNode compute-only, Worker results и публичные validator votes | PARTIAL. `/tmp/outbe-e2e-harness-2112543`: путь дошёл до обоих jobs и затем обнаружил test-only deadline drift 1800 вместо genesis 120. `/tmp/outbe-e2e-harness-2249770`: после RED→GREEN regression дошёл до fifth FullNode state parity, после чего был сознательно остановлен для trust-аудита; PASS не заявляется |
| H02 | `@ocomp-public-apply` | Один OFFERING day, Oracle VWAP и funding подготовлены в genesis | Не доказывает runtime-создание WWD | После JobIntent harness наблюдает настоящие worker results, публичные result-vote tx, одинаковую activation/NOD state и distinct signers; direct result submit helper не вызывается | PASS. `/tmp/outbe-e2e-public-release.wHuPFB`, 10/10 steps, clean audit, SHA256 `1bdd3079ee60c03bee04569a0cdf97551972d3d071b2d889a85dca6e96ec7de2` |
| H03 | `@metadosis-fresh-devnet` | Ключи, funding, Oracle initial data, immutable timing constants и OCOMP install | Seeded active WWD очищается до старта; JobIntent/result/NOD и post-start storage injection запрещены | Block 1 создаёт WWD в FORMING; production Cycle формирует immutable day limit; реальные blocks проходят фазы; 257 CLI offers отправляются максимум по 2; затем production Supervisor/Worker/vote/Lysis/NOD | PASS. Release, 4 validators, настоящий SGX/Gramine, `sgx.remote_attestation=none`, `--tee sgx-no-attest`; 1/1 scenario, 15/15 steps. Два controlled-time coordinated restarts прошли; сеть пересекла certified boundary H300, где предыдущий код расходил epoch и snapshot; 257/257 публичных Tribute видны четырём validators; chain lifecycle создал finalized JobIntent; production OCOMP domains сформировали три matching results, атомарно применили Lysis и создали ровно 257 NOD; validator-0 восстановил certified generation из canonical history. Нет JobIntent/result/NOD/post-start state injection. Run: `/tmp/metadosis-boundary-retry.3ECYgm/r/run-1786286533-3079681/scenario-1`; исправление `1a2c2930` |
| H04 | Deadline source equivalence | `computeVoteWindowBlocks` материализуется в immutable genesis constants | Test-only builder не вправе использовать отдельный hardcoded default | Production `outbe-chain ocomp genesis` на том же profile сформировал 120; harness раньше сформировал 1800 | PASS focused regression: `dynamic_membership_fixture_schedules_two_distinct_public_jobs` сначала RED 1800/120, после локальной test-only правки GREEN; `cargo fmt --all -- --check` и build harness PASS |
| H05 | Direct-result injection audit | Direct submit helpers разрешены только в negative/retry/component steps | Happy-path `JobIntent → Worker → vote → Lysis → NOD` не может ими пользоваться | `production_ocomp_domains_process_job_intent` проверяет pre-state/liveness; terminal step читает finalized public tx/accountability/activation/NOD. Прямые submit helpers находятся в отдельных mutation/retry шагах | PASS статический source audit; всё ещё требуется H03 runtime PASS |
| H06 | Embedded OCOMP crash restart | Harness выполняет dirty restart с сохранением production datadir и одинаковых durable checkpoints; задержка старта не используется как обход | Нельзя удалять/откатывать checkpoint, ждать случайный дополнительный блок или ослаблять canonical hash validation | Restart/replay сценарий восстанавливает embedded OCOMP checkpoint, exact result и sign-once state | PASS. `@ocomp-e2e-008`, 9/9 steps, `restart_replay_verified=true`; `/tmp/outbe-e2e-restart-release.YIgHvA` |
| H07 | Consensus restart с асимметричным unfinalized head | Все nodes имеют один certified anchor, но часть успела сохранить следующий unfinalized head/view | Нельзя выравнивать heads перед остановкой, чистить WAL или сбрасывать safety lock ради зелёного теста | Controlled-time flow дважды останавливает и последовательно поднимает весь committee с сохранёнными datadir/WAL/CE; после каждого запуска требует общий следующий finalized block до старта OCOMP roles | PASS. Тот же H03 run прошёл оба coordinated restart без выравнивания heads, очистки WAL или отката CE, затем finalized boundary H300 и полный Lysis/NOD. Run: `/tmp/metadosis-boundary-retry.3ECYgm/r/run-1786286533-3079681/scenario-1` |

До PASS строк O01–O09 нужны одновременно H03 и отдельный canonical LocalNet row,
который использует production generators/construction path. H01/H02 сами по себе
не могут закрыть полный production-flow, даже если зелёные.

## Время, накопление и эксплуатационные границы

| ID | Сценарий | Критерий PASS | Результат | Доказательство |
|---|---|---|---|---|
| L01 | Переход суток | `OFFCHAIN_PENDING` и WorldwideDay FSM согласованы после истечения окна | MISSING | Нужны управляемые chain clocks/heights |
| L02 | Два живых дня | Терминальные записи и capacity считаются в правильной проекции, не глобально | MISSING | — |
| L03 | Длительное накопление | Pin/job registries освобождаются раньше потолка; нет halt/livelock | MISSING | — |
| L04 | Delayed finality | Reconciliation запускается от финальности и не теряет tentative pin после фиксированного числа попыток | MISSING | — |
| L05 | Non-grid epoch boundary | Follower берёт наблюдаемую границу из chain history, не вычисляет `(h-1)/L`, и live/restart проходят позднюю DKG boundary | PASS | RED: SGX/no-DCAP run `/tmp/outbe-e2e-follower-offgrid-fixed/run-1786195246-3325576` остановил FullNode после поздней самосертифицированной boundary. GREEN: `/tmp/outbe-e2e-follower-p0.ydHWpD/run-1786200688-3593462`, release binaries, `sudo`, настоящий SGX/Gramine, `sgx.remote_attestation=none`: 1/1 scenario и 11/11 steps PASS. Старый epoch остался активен после planned=60; DKG завершился на H=84, точный outgoing-finalized `CommitteePreAnnounce` разрешил activation anchor H=86, active set 4→5 применился в block 87. FullNode достиг точного finalized hash/state-root parity после boundary и повторно после restart. Исправление `outbe-chain-08n.4.1.3` не меняет follower trust, canonical/wire/EVM/genesis формы |
| L06 | Грязный socket restart | Осиротевшие control sockets не лишают валидатора OCOMP роли молча | MISSING | — |
| L07 | Prepared export restart | Частично подготовленный export корректно восстанавливается или отклоняется | MISSING | — |
| L08 | Registry pressure | Старт с заполненным на 75% pin registry чистит только допустимый мусор и сохраняет live pins | MISSING | — |
| L09 | Production UID/path | Разные role UIDs и production base-dir права совпадают с deployment contract | MISSING | — |
| L10 | Mongo projection outage | Внешняя проекция не блокирует consensus execution/finality | NOT RUN | Требуется отдельный live fault-injection сценарий |
| L11 | Coordinated restart около unfinalized proposal | Общий certified anchor восстанавливает liveness при разных локальных speculative heads/views | PASS | H03 дважды выполнил production-shaped coordinated restart на сохранённых состояниях, потребовал общий следующий finalized block и затем завершил certified boundary H300 и Lysis/NOD; `/tmp/metadosis-boundary-retry.3ECYgm/r/run-1786286533-3079681/scenario-1` |

Persistent `outbe-e2e localnet` на фиксированной точке жёстко создаёт
`TeeMode::Mock`. Live-прогон использует Cucumber runner с явным
`--tee sgx-no-attest`; запуск mock или non-SGX `gramine-direct` не будет
засчитан как требуемое доказательство.

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
8. T07 exact OCOMP evidence gates; native-DCAP/hardware строки T05–T06 не
   запускаются.
9. Полный повтор затронутых gates и итоговый go/no-go.

## Текущий вывод

Решение по обновлению testnet пока **NO-GO**, но прежний blocker полного
production-shaped пути закрыт. На `1a2c2930` release SGX/no-DCAP сценарий прошёл
настоящий block-1 Create, production Cycle, два controlled-time coordinated
restart, certified DKG boundary H300, 257/257 публичных Tribute, finalized
JobIntent, Supervisor/Worker computation, quorum, Lysis и 257 NOD без state или
result injection. Dynamic 4→5 membership и отдельный restart/exact-replay также
имеют прямые зелёные process evidence. NO-GO сохраняется только потому, что
матрица ещё содержит обязательные незапущенные quality/profile/contract и
time/accumulation/dirty-start строки B02a, B09, B12–B14, T02–T03a, T07 и
L01–L04, L06–L10; их нельзя считать пройденными по одному happy-path E2E.

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
