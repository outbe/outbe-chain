# Сетевой репозиторий Radicle: BLS и голосование за patch

Статус: план реализации; ни один этап реализации этим документом не объявляется завершённым.
Дата: 2026-10-15. Проверенный checkout chain: `033501adf`.
Beads feature: `outbe-chain-pjz`, локальная база `outbe_chain`. Фактические IDs и связи: `beads-map.json`.

## Результат для пользователя

Genesis задаёт системный репозиторий `governments`, принадлежащий сети. Узлы автоматически
материализуют его без ручного `rad init` и без полного приватного ключа владельца у оператора.
Автор создаёт обычный Radicle patch. Активные валидаторы через Vote решают, принять ли
конкретную revision этого patch. После финализации разрешения узлы выпускают threshold
BLS подписи; распределённый collector в `outbe-radicle` собирает их, публикует нативное
Review Accept и исполняет merge одобренной revision. Другой узел независимо проверяет
identity, COB и signed refs. Merge conflict явно оставляет результат одобренным, но не merged.

## Зафиксированные границы

- Heartwood получает общую поддержку Ed25519 и BLS. Проверка Vote, finality и права
  текущего validator set находится в Outbe integration/signing policy, а не в generic
  Radicle crypto или patch evaluator.
- Ed25519 остаётся для авторов, обычных репозиториев и transport NodeId/Noise. BLS является
  полноценным ключом repository authority; Ed25519 узла не заменяет подпись сети.
- Выбрана threshold BLS подпись под group public key. Агрегат независимых validator
  signatures не является незаметно допустимой заменой. Radicle threshold delegates = 1
  для единственного сетевого delegate не равен криптографическому signing threshold.
- Предмет Vote: `(repo_id, patch_id, revision_id, action=accept)`. Кодовые commit IDs
  и ожидаемая target branch нужны для проверки/исполнения, но не заменяют patch отдельным
  голосованием за заранее подготовленный merge commit. Изменение revision требует нового Vote.
- Review Accept и Merge — разные нативные действия; отдельного `State::Approved` в patch нет.
  Нельзя выдать опубликованный review за завершённый merge или за chain approval.
- Работа с Git, сетью, диском и ожидание partial signatures выполняются после finality,
  вне обработки Vote/block-production. Proposer не является специальным координатором.
- Signer может исполнять финализированное Approved, даже если голосовал No. Авторизован
  текущий signing set, а не только адреса с голосом Yes.
- Текущий signing set берётся из точного finalized состояния и аутентифицированной
  DKG/material generation. ACTIVE lifecycle status и effective consensus/DKG membership
  нельзя считать эквивалентными без проверки. Изменение правил всего Vote вне scope:
  handler/policy должны явно обеспечить выбранное правило именно для patch.
- Удалённые участники, посторонние ключи, дубликаты индексов и partials разных generations
  не учитываются. Смена набора во время сбора аннулирует незавершённую попытку.
- Историческая проверка подписи и право совершить новое действие различаются. Простое
  добавление epoch в sidecar metadata или сертификата поверх неизменного group key не
  лишает старые shares криптографической силы. Нельзя заявлять revocation без доказанного
  правила проверки нового действия и ротации authority. Эта проблема закрывается в P0/P3.
- Частичный успех, недоступный тестовый стенд, scaffold, mock-only путь и успешная компиляция
  не доказывают полную реализацию. Необходимый, но не выполненный критерий оставляет задачу открытой.

## Проверенная база и обновление

`vendor/heartwood/UPSTREAM.md` фиксирует fork `outbe/outbe-heartwood`:
`b76a17801329291153585ed31db61ee3c658046e`, parent
`16de8f1950825672fe48b901dafa0d009f612b04`. Собственный fork commit меняет 14 файлов.
Sidecar уже вынесен в `crates/system/radicle` (`outbe-radicle-runtime`), binary остаётся
в `bin/outbe-radicle` (`outbe-radicle-sidecar`). Manager/integration находится в
`crates/blockchain/radicle` (`outbe-radicle`).

Цель обновления: stable Radicle 1.10.3, tag `releases/1.10.3`, peeled commit
`7e1cb406bb935c3de008d2171e8961f57b4336ea`. Mutable `master` не является pin:
upstream использует его для Radicle 2. Сравнивать нужно с реальным общим ancestor,
сохраняя Outbe изменения, а не заменять каталог поверх форка без учёта delta.

Источник выбора версии: https://radicle.dev/download,
https://radicle.dev/2026/09/08/radicle-1.10.3,
https://radicle.dev/2026/08/26/radicle-1.10.2,
https://radicle.dev/2026/08/05/radicle-1.10.0.

Предыдущая проверка vendoring: сборка зависимых crates и два harness regression tests
прошли; один sidecar test `status_server_enforces_connection_and_request_deadline_bounds`
падал также на неизменном fork. P1 обязан заново воспроизвести baseline и получить
проверяемое поведение лимитов на целевой платформе, а не считать это автоматическим waiver.

## Этапы и зависимости

| Код | Результат задачи | Зависит от |
| --- | --- | --- |
| P0 / `outbe-chain-pjz.1` | Проверенный и зафиксированный контракт BLS authority, patch lifecycle, genesis/RID и смены набора | — |
| P1 / `outbe-chain-pjz.2` | Heartwood 1.10.3 с сохранённой рабочей Outbe integration | P0 / `outbe-chain-pjz.1` |
| P2 / `outbe-chain-pjz.3` | BLS-owned repo и нативные подписанные операции проверяются вторым узлом; Ed25519 работает | P1 / `outbe-chain-pjz.2` |
| P3 / `outbe-chain-pjz.4` | Текущая threshold authority, signer и безопасная смена generations проверены через публичный interface | P2 / `outbe-chain-pjz.3` |
| P4 / `outbe-chain-pjz.5` | `governments` автоматически создаётся из genesis descriptor после initial DKG | P3 / `outbe-chain-pjz.4` |
| P5 / `outbe-chain-pjz.6` | Vote за конкретную patch revision сохраняет неизменяемое разрешение в chain state | P1, P0 |
| P6 / `outbe-chain-pjz.7` | После finality распределённый collector публикует BLS Review Accept | P3, P4, P5 |
| P7 / `outbe-chain-pjz.8` | Разрешённая revision merged; конфликт и рестарт не подменяют решение | P6 / `outbe-chain-pjz.7` |
| P8 / `outbe-chain-pjz.9` | Полный многовалидаторный сценарий, смена набора, восстановление и evidence всей фичи | P7 / `outbe-chain-pjz.8` |

Подробные задачи в Beads самодостаточны: контекст, зачем, изменения, существующие/новые
файлы, acceptance criteria, тесты и обязательный evidence. Пути — точки входа по
проверенному checkout; при изменении структуры исполнитель уточняет всю карту callers,
fixtures, форматов и wiring до изменения кода и прикладывает её к задаче.

P0 — явная исследовательская задача с конечным результатом, не разрешение придумывать
криптографические гарантии по ходу реализации. До её закрытия нельзя считать решёнными:
состав electorate; exact native signing bytes; способ авторизации нового group key;
защиту от старых shares; устойчивый RID при DKG после genesis; порядок публикации
Review/refs/branch; merge policy и правила повторной попытки при движении target branch.
Неразрешённая осуществимость блокирует следующие этапы; дизайн, меняющий требования
пользователя, не принимается как исполнение задачи.

## Evidence перед закрытием каждой задачи

Исполнитель оставляет задачу `in_progress` и прикладывает evidence bundle, например
`plans/radicle-patch-vote/evidence/<bead-id>/<revision>/`, либо устойчивую ссылку на CI artifact.
Сам путь не является evidence: файлы и результаты должны реально существовать.

```text
task_id, base_sha, reviewed_sha, spec_hash, timestamp_utc
changed_files + actual call-site/fixture/wiring map
criterion_id -> source file:line -> behavioral test/scenario -> artifact + result
commands: exact argv, cwd, toolchain, platform, exit code, log/artifact hash
negative scenarios and observed rejection/unchanged state
known failures: reproduced baseline, effect on acceptance, resolution
unverified requirements and remaining work (must be empty for PASS)
```

Успешный тест должен различать правильную реализацию и неправильное поведение. Требуемые
production paths нельзя подменять test-only bypass, фиктивными ключами вместо проверки
подписи, строковым поиском по исходникам или mock без хотя бы одного реального сценария.
Для P0 evidence — проверяемая карта исходников и решение по каждому design blocker,
а не несуществующие результаты implementation tests. Для остальных этапов — реальные
изменения и запуски на указанном SHA. Runtime failure воспроизводится в нужной среде;
отсутствующая Linux/SGX среда не превращает FAIL/NOT RUN в PASS.

## Три независимые проверки и закрытие

Каждая P0–P8 задача имеет три дочерние review-задачи R1/R2/R3, изначально `blocked`:
ожидают evidence родителя. Это не зависит от закрытия родителя и не создаёт dependency
cycle. Сам implementation task остаётся открыт и может быть взят в работу после своих
обычных blockers. Открытые review children служат препятствием обычному закрытию родителя.
Feature также получает три финальных review children для проверки результата целиком.

1. Исполнитель фиксирует exact reviewed SHA и полный evidence; добавляет `evidence-ready`
   и переводит только относящиеся к нему R1/R2/R3 в `open`.
2. Координатор запускает **три разных агента**, не являющихся автором реализации.
   Каждый получает полные acceptance criteria, исходники/diff и evidence одного SHA,
   самостоятельно проверяет **все** критерии, а не треть задачи. Они не читают отчёты
   друг друга до сдачи собственных заключений и не редактируют реализацию.
3. Каждый публикует свой отчёт: agent/session ID, task ID, SHA/spec hash, таблица всех
   критериев `PASS/FAIL/NOT VERIFIED`, проверенные artifacts, выполненные команды,
   findings с источником. Акценты R1: полнота/интеграция; R2: полномочия/негативные
   сценарии; R3: независимое воспроизведение/восстановление. Акцент не ограничивает охват.
4. Review task закрывается только с отчётом `PASS` по всем критериям и без открытых
   findings. Исполнитель не может закрыть review за агента собственной декларацией.
   Рецензенты закрывают только свою review-задачу, не implementation task.
5. Любое изменение implementation/spec после отчёта аннулирует три PASS для этой задачи:
   новый SHA/evidence, соответствующие reviews переоткрываются, три агента проверяют заново.
6. Implementation task закрывается только после валидного evidence и трёх закрытых
   review children с PASS на том же SHA/spec. Close reason содержит ссылки на bundle
   и все три отчёта. Нельзя обходить через `bd close --force`, `bd update --status closed
   --force` или удаление reviewer children.
7. Feature закрывается после P0–P8, полного финального evidence и трёх финальных независимых
   PASS. Review-задачи являются записями проверки и не требуют рекурсивных review-of-review.

Beads хранит hierarchy, blockers и отчёты. Семантику «три реальных независимых PASS на
этом SHA» проверяет координатор по evidence: эта фича не меняет глобальные hooks/CLI и
не выдаёт текстовые acceptance criteria за автоматический криптографический guard.
Сейчас reviews реализации остаются blocked: планирование не является implementation evidence.

## Минимальная финальная проверка

Сценарий с реальными ключами: initial genesis/DKG → одинаковый network-owned RID на
двух узлах → Ed25519 автор создаёт patch → Vote за зафиксированную revision → finalized
Approved → текущие валидаторы подписывают exact native operations → любой collector
восстанавливает BLS → Review Accept и Merge/refs → второй узел fetch и независимая проверка.

Отрицательные сценарии: outsider/inactive/pending и несовпадающий effective set;
недостаточный threshold; duplicate/wrong-index/bad partial; другая сеть/repo/patch/revision;
новая revision после Vote; proposal не Approved/не finalized; устаревшая generation,
смена набора во время сбора и перед публикацией; старые shares; конфликт merge;
падение сборщика до/после каждого durable перехода; повторная доставка и fetch;
недоступность sidecar без остановки consensus после успешного bootstrap.

Финальные артефакты связывают Vote ID, repo/patch/revision, finalized block, membership
commitment/material generation, signing subject hashes, verified BLS signatures,
native COB/refs и наблюдаемое состояние второго узла. Три агента должны иметь возможность
проверить эту цепочку, а не доверять итоговой строке «успешно».

## Созданные Beads records

Feature `outbe-chain-pjz` содержит 9 implementation/design tasks, у каждой по три review children.
Ещё три final feature reviews: `outbe-chain-pjz.10`, `outbe-chain-pjz.11`, `outbe-chain-pjz.12`.
Всего 40 records: 1 feature, 9 этапов, 30 независимых review tasks.
Выполнять первым: `bd show outbe-chain-pjz.1`. Все implementation reviews
сейчас `blocked` в ожидании реального evidence; ни одна implementation задача не закрыта.
`issues.jsonl` — переносимый snapshot только этой feature, не экспорт остальной базы.
