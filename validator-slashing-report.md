# Основания и последствия наказаний валидатора

Проверено 12 сентября 2026 года по текущей рабочей копии `outbe-chain`, HEAD `177a72ddbea9f2e52eef094405481292ecd56046`. Это описание исполняемых правил в исходниках, а не проверка конфигурации или истории наказаний действующей сети. Рабочая копия содержит локальные изменения.

## 1. Полный перечень обнаруженных оснований

Под **обычным felony** далее понимается вызов общей политики SlashIndicator: для ACTIVE-валидатора — `JAILED`, списание настроенного процента bonded stake и ещё не выведенных unbonding-заявок, продление срока существующих unbonding-заявок. Процент по умолчанию — **5%**. Основания Oracle и OCOMP имеют собственные параметры и правила.

| Основание | Когда применяется | Последствия |
|---|---|---|
| Пропуск назначенного proposer-слота | Каждый подтверждённый пропуск увеличивает proposer miss counter. По умолчанию предупреждение на кратных 50 пропусках; felony на кратных 150 | До felony — счётчик и события; на felony — обычный штраф 5% и JAILED |
| Отсутствие засчитанного consensus-голоса | После закрытия окна позднего включения `N+3`, если участник исторического комитета отсутствует среди base и late voters. Предупреждение на кратных 150; felony на кратных 500 | До felony — счётчик и события; на felony — обычный штраф 5% и JAILED |
| Две разные notarize-подписи за один round | Один BLS signer, одинаковые epoch и view, разные proposals, обе подписи действительны. Доступно через `submitDoubleProposalEvidence` и `submitConflictingNotarizeEvidence` | Обычный felony; награда подателю evidence |
| Notarize и nullify за один round | Один signer подписал и принятие proposal, и пропуск того же epoch/view. `submitConflictingVoteEvidence` | Обычный felony; награда подателю |
| Две разные finalize-подписи за один round | Один signer подписал finalize разных proposals в одинаковых epoch/view. `submitConflictingFinalizeEvidence` | Обычный felony; награда подателю |
| Nullify и finalize за один round | Один signer подписал пропуск и финализацию одного epoch/view. `submitNullifyFinalizeEvidence` | Обычный felony; награда подателю |
| Невалидный threshold VRF proof в подписанной Phase 1 transaction | `submitInvalidVrfProofEvidence` повторно проверяет доказательство и принимает только предусмотренные VRF-классы ошибок | Обычный felony обвиняемому signer Phase 1 transaction; награда подателю |
| Два различных VRF seed partial за один round/version | Два разных partial, каждый подписан identity-ключом одного валидатора для одинаковых round и material version. `submitSeedPartialEquivocationEvidence` | Обычный felony; награда подателю |
| Один криптографически неверный VRF seed partial | Identity-подпись валидатора действительна, но partial не проходит проверку относительно публичного polynomial, привязанного к committee snapshot. `submitInvalidSeedPartialEvidence` | Обычный felony; награда подателю |
| Недостаточная доля успешных Oracle-голосований | В конце Oracle slash window: `success / (success + abstain + miss) < min_valid_per_window` | JAILED для ACTIVE-валидатора и, если настроен ненулевой целочисленный процент, обычный staking slash. По умолчанию денежный штраф Oracle — 0% |
| Нет принятого OCOMP result vote к deadline задания | При закрытии response window проверяются пустые vote slots исторического result committee; наказание применяется только если валидатор сейчас ACTIVE | При первом пропуске в recovery window — **10% только bonded stake**, окно **43 200 блоков**, сохранение ACTIVE. Повторные пропуски внутри окна без нового списания |
| После OCOMP recovery deadline bonded stake ниже min_stake | Обязательная проверка состояния при достижении фиксированной высоты deadline | JAILED; дополнительного списания при закрытии окна нет |
| Нет TEE binding либо его lease истёк | После bootstrap, в CycleTick до пользовательских транзакций: binding отсутствует либо `valid_until <= timestamp` блока | JAILED **без списания стейка и без увеличения slash_count** |

Источники: [SlashIndicator thresholds и downtime](crates/system/slashindicator/src/runtime.rs#L42), [evidence entrypoints](crates/system/slashindicator/src/runtime.rs#L412), [Oracle](crates/system/oracle/src/tally.rs#L722), [OCOMP deadline](crates/core/metadosis/src/ocomp/vote.rs#L532), [OCOMP economics](crates/system/staking/src/logic.rs#L393), [TEE deadline](crates/blockchain/evm/src/begin_block_precompile.rs#L652).

`submitDoubleProposalEvidence` и `submitConflictingNotarizeEvidence` проверяют одну и ту же пару notarize-подписей; название первого не означает отдельной проверки того, что signer был назначенным лидером. Общая order-independent дедупликация не даёт повторно применить ту же пару через второй selector. [Проверки и dedup](crates/system/slashindicator/src/runtime.rs#L412).

## 2. Пропуски consensus: что именно учитывается

- Proposer miss берётся из `missed_proposers` в подтверждённых metadata родительского блока. Одна и та же validator address может встретиться несколько раз за разные пропущенные views: каждое вхождение считается отдельным пропуском.
- Voter miss определяется по историческому committee snapshot и объединению base voters с поздними голосами. Само отсутствие в первом сертификате ещё не означает miss: поздний голос до закрытия окна может защитить от него.
- Окно позднего включения: `K = 3`. Голос, впервые включённый на `N+3`, засчитывается для отсутствия miss, хотя fee-выплата за него уже равна нулю.
- Счётчики proposer/voter miss раздельные и сбрасываются при переходе эпохи через boundary. Это не счётчик последовательных пропусков: успешный следующий голос сам по себе предыдущие пропуски не обнуляет.
- При совпадении misdemeanor- и felony-порогов в одном событии выполняется felony-ветка.
- Для уже JAILED или EXITING валидатора новые downtime misses продолжают учитываться, но повторный misdemeanor/felony по этим двум downtime-путям пропускается.
- Повторная обработка одного finalized block защищена отдельными proposer/voter guards.

Источники: [учёт proposer events](crates/system/slashindicator/src/hooks.rs#L42), [окно voter miss](crates/blockchain/evm/src/begin_block_precompile.rs#L919), [защита уже наказанных](crates/system/slashindicator/src/runtime.rs#L115), [сброс на boundary](crates/blockchain/evm/src/executor.rs#L350), [K](crates/blockchain/primitives/src/consensus.rs#L116).

## 3. Денежный штраф и задержка вывода

Обычная `Staking::slash_stake(validator, percent)`:

1. Списывает `floor(current_bonded × percent / 100)`.
2. Для каждой ненулевой unbonding-заявки списывает `floor(entry_amount × percent / 100)`. Проверки, что срок заявки ещё не наступил, в этом обходе нет: уже созревшая, но ещё не полученная через claim заявка тоже попадает под штраф.
3. Сдвигает срок каждой существующей ненулевой заявки до `max(старый срок, timestamp штрафа + slashed_withdrawal_delay)`.
4. Уменьшает реальный native balance staking-контракта на сумму списания — это burn, а не только изменение счётчиков.
5. Обновляет отражённый bonded stake в ValidatorSet.

Если `slashed_withdrawal_delay` не задан, применяется `2 × unbonding_period`. Генератор genesis по умолчанию использует **21 день** обычного unbonding и **42 дня** slashed withdrawal delay. Эти значения могут быть переопределены конфигурацией.

**42 дня — не универсальная блокировка всего оставшегося стейка после любого наказания.** Данная функция продлевает уже существующие unbonding-заявки. Новый `unstake` создаёт заявку по обычному `unbonding_period`. OCOMP-штраф этот механизм вообще не вызывает.

При evidence-felony податель получает по умолчанию **10% от списанной суммы**, а не 10% от исходного стейка. Сначала списанная сумма сжигается, затем награда создаётся на балансе подателя. Чистое уменьшение supply — штраф минус награда.

Пример: 1 000 bonded и 200 в unbonding, обычный штраф 5% → списывается 50 + 10 = **60**. При evidence-felony подателю достаётся **6**, чистый burn равен **54**. Без подателя доказательства вся сумма **60** сжигается.

Низкоуровневая функция принимает процент до **100%** включительно; больше 100% отклоняет. Это возможность конфигурации/внутреннего вызова, а не обнаруженная автоматическая эскалация «за N нарушений списать 100%». В текущих путях нет увеличения процента на основании `felony_count`.

Источники: [slash_stake](crates/system/staking/src/logic.rs#L329), [задержка](crates/system/staking/src/logic.rs#L141), [новый unstake](crates/system/staking/src/logic.rs#L210), [genesis defaults](scripts/seed_genesis.py#L232), [seeding задержки](scripts/seed_genesis.py#L1216), [награда за evidence](crates/system/slashindicator/src/runtime.rs#L665).

## 4. Что означает JAILED и как вернуться

Для ACTIVE-валидатора обычное наказание сначала создаёт внутреннее состояние `JailRetained`. Наружу оно отображается как JAILED: валидатор исключается из будущего целевого набора, но его доля текущего комитета сохраняется до фактического исключения на корректном DKG boundary. Затем он переходит в `Jail` без текущей доли.

Поэтому «JAILED» не означает мгновенное удаление из всех исторических/текущих committee snapshots. Если целевой набор уже был зафиксирован до наказания, фактическое исключение может потребовать более позднего boundary. До исключения `unjailValidator()` отклоняется.

Возвращение требует:

1. Фактического исключения из текущего комитета.
2. Bonded stake не ниже настроенного `min_stake`; при необходимости его нужно пополнить.
3. Наступления `jailed_at_height + config_unjail_cooldown_blocks`. Незаполненный cooldown равен **0 блоков**, но требование фактического исключения остаётся.
4. Собственной транзакции `Staking.unjailValidator()` → PENDING / `WaitingForReadiness`.
5. Нового подтверждения readiness и последующего включения через DKG boundary → ACTIVE.

Одного пополнения стейка недостаточно для автоматического выхода из JAILED. При возвращении очищаются missed counters ValidatorSet; история slash/deactivation сохраняется. Это не сброс накопленного `SlashIndicator.felony_count` и не универсальный сброс всех счётчиков разных модулей.

Альтернатива возвращению — полностью вывести bonded stake через обычный lifecycle: retained jailed → EXITING до boundary; уже исключённый jailed → UNBONDING непосредственно. Частичный `unstake` не выводит из JAILED. Окончательная выплата требует созревания и claim заявок.

Отдельного правила пожизненного запрета или tombstone «после N felony нельзя вернуться» в исследованных путях нет. Новое, отличное от ранее обработанного, допустимое evidence может повторно списать часть оставшегося стейка уже JAILED-валидатора: защита от повторного downtime felony не распространяется на все независимые evidence.

Источники: [punish/jail](crates/system/validatorset/src/runtime.rs#L1731), [unjail](crates/system/validatorset/src/runtime.rs#L1904), [проверка стейка](crates/system/staking/src/logic.rs#L247), [cooldown transition](crates/system/validatorset/src/state_machine/mod.rs#L238), [unstake lifecycle](crates/system/validatorset/src/runtime.rs#L1118), [проверка post-freeze сценария в тесте](crates/system/validatorset/src/tests.rs#L1166).

## 5. Oracle: наказывается валидатор, включая ошибки его feeder

Oracle считает результаты по validator address. За отдельный tally round:

- `success`: валидатор получил зачёт по всем активным целевым парам;
- `abstain`: не отправил учитываемого голоса;
- `miss`: участвовал, но не выполнил условие успеха по всем парам.

Причинами miss могут быть котировка вне допустимого диапазона, нулевая цена, отсутствующая часть необходимых пар, непредставимая/отфильтрованная volume или cross-rate строка. Для ценового сравнения границы включительны: `median ± max(std_dev, median × reward_band / (2 × 10^18))`. На парах без кворума предусмотрен зачёт корректного участия; само отсутствие кворума не делает каждый корректный голос нарушением.

В конце окна проверяется:

```text
valid_rate = floor(success × 10^18 / (success + abstain + miss))
наказание, если valid_rate < min_valid_per_window
```

Параметры генератора genesis по умолчанию: vote period **2 блока**, slash window **96 блоков**, минимальная доля успехов **5%**, денежный slash fraction **0**. Следовательно, default Oracle policy допускает **JAILED без денежного штрафа**. `penalties_enabled=false` в генераторе обнуляет minimum и fraction.

Если fraction задан, фактический процент равен `floor(slash_fraction × 100 / 10^18)`. Например, fraction 0,5% даёт **0 целых процентов** и денежного списания не вызывает. При ненулевом целом проценте используется обычный `slash_stake`, включая unbonding и задержку вывода.

Исключения: `total=0` пропускается; protected-валидаторы пропускаются, если включён `allow_protected`. Счётчики обнуляются после обработки окна. В отличие от downtime-пути SlashIndicator, Oracle не вызывает `validator_already_penalized`: общей гарантии «уже JAILED никогда повторно не списывается» для Oracle-окон нет.

Отдельного немедленного `slash_byzantine` за Oracle overflow в текущем tally нет: непригодные вклады обрабатываются алгоритмом tally и обычным учётом успехов/пропусков. Исторические планы такого немедленного felony не являются текущим правилом.

Источники: [ценовой диапазон](crates/system/oracle/src/tally.rs#L168), [below-quorum credit](crates/system/oracle/src/tally.rs#L523), [success/abstain/miss](crates/system/oracle/src/tally.rs#L695), [штраф и исключения](crates/system/oracle/src/tally.rs#L722), [планирование](crates/system/oracle/src/lifecycle.rs#L33), [seeder](scripts/seed_genesis.py#L1439).

## 6. OCOMP: отдельное окно восстановления

Штраф связан с отсутствием принятого result vote к deadline конкретного задания. Проверяются пустые slots исторического result committee. Уже достигнутый кворум задания не освобождает остальных участников от ответа: окно остаётся открытым для учёта голосов, а пропуски обрабатываются и у completed job.

- Если участник больше не ACTIVE к моменту обработки, новый OCOMP miss penalty ему не назначается.
- Первый miss открывает окно `current_height + 43 200` и сжигает `floor(bonded / 10)`.
- Статус остаётся ACTIVE даже при падении bonded ниже min_stake.
- Повторные misses увеличивают накопительный `ocomp_miss_count`, но не списывают повторно и не продлевают deadline.
- Unbonding-заявки, их суммы и сроки этим штрафом не меняются.
- На deadline проверяется именно bonded stake: если он не ниже минимума — окно закрывается с восстановлением; если ниже и валидатор ещё ACTIVE — JAILED без второго денежного штрафа. Новый успешный OCOMP vote не является отдельным условием этой проверки восстановления.
- Если lifecycle к deadline уже не ACTIVE, окно закрывается без повторного jail.
- После закрытия окна следующий miss может открыть новое окно и снова списать 10%.
- Обязательный recovery sweep находится в CycleTick каждого блока, а не зависит только от появления нового OCOMP задания. Проверки begin-zone проходят до пользовательских транзакций: пополнение следует включить **до** блока deadline.

43 200 — точное число блоков. Комментарий к константе описывает его как сутки при целевом интервале две секунды; фактическое календарное время зависит от скорости сети.

По обнаруженному пути санкция проверяет отсутствие slot, а не совпадение с большинством. Отклонённый невалидный result vote не создаёт принятого slot и потому может закончиться обычным miss к deadline; сам revert такого vote не вызывает немедленный staking slash.

Источники: [закрытие response window](crates/core/metadosis/src/ocomp/vote.rs#L532), [приём голосов](crates/core/metadosis/src/ocomp/vote.rs#L350), [окно и ACTIVE](crates/system/validatorset/src/runtime.rs#L1239), [10% и неизменность unbonding](crates/system/staking/src/logic.rs#L393), [решение на deadline](crates/system/staking/src/logic.rs#L444), [обязательный sweep](crates/blockchain/evm/src/begin_block_precompile.rs#L643).

## 7. TEE и потеря вознаграждения без слешинга

**TEE.** Отсутствующий binding или истёкший `valid_until` приводит к JAILED с сохранением bonded stake и `slash_count`. Старый committee share сохраняет ответственность до boundary. Для expired binding обычный renew уже не подходит: нужен `registerEnclave` rejoin; JAILED-валидатор сначала должен пройти обычный unjail. После этого следует восстановить TEE binding, readiness и включение в комитет. Окно своевременного renew — вторая половина срока lease, до самого deadline.

Источники: [проверка lease](crates/blockchain/evm/src/begin_block_precompile.rs#L652), [non-slashing jail](crates/system/validatorset/src/runtime.rs#L1757), [порядок rejoin](crates/system/teeregistry/src/v1.rs#L129), [renewal window](crates/system/teeregistry/src/v1.rs#L2028).

**Fee-выплата за голос.** У credited vote на расстоянии `k=0,1,2` полный вес; при `k=3` — нулевой. Не засчитавший голос участник также не получает эту выплату. Это потеря дохода за конкретный блок, а не списание ранее внесённого stake. Невыплаченная доля не перераспределяется остальным валидаторам: знаменатель зависит от полного размера комитета; остаток уходит по terminal remainder policy.

Источники: [веса 100/100/100/0](crates/system/rewards/src/constants.rs#L22), [выплата и остаток](crates/system/rewards/src/late_settlement.rs#L175).

**Административная деактивация.** Отдельно от автоматических наказаний `deactivateValidator(address)` доступен самому валидатору или `config_owner`. Он переводит ACTIVE → EXITING и инициирует изменение набора, но сам по себе не списывает stake. Это дополнительная возможность исключения валидатора, а не отдельный доказуемый offense.

Источники: [ACL и переход](crates/system/validatorset/src/runtime.rs#L1653), [публичный selector](contracts/precompiles/src/IValidatorSet.sol#L105).

## 8. Доказательства: доступность, дедупликация, автоматизация

Все восемь evidence selectors SlashIndicator доступны только **сейчас ACTIVE** подателю. Базовый gas для каждого — **200 000**, сверх чего учитывается дальнейшая работа вызова. Обычный EOA без ACTIVE validator status подать их не может.

Для consensus equivocation проверяются реальные BLS-подписи и committee эпохи. Нужен сохранившийся snapshot; ring рассчитан на восемь эпох. Отдельного max-age условия в этих пяти selectors нет, но отсутствие snapshot или недопустимый lifecycle обвиняемого может сделать применение невозможным.

Для invalid VRF proof действуют дополнительные ограничения: возраст child не более **2 048 блоков**, epoch lag не более **1**, evidence не более **256 KiB**, доступность соответствующих canonical hashes и committee snapshot. Наказуемые failure codes: malformed proof, неверная material version, неверный group key hash, namespace, seed round или VRF signature. Ошибка только BLS quorum/accounting не принимается этим selector как VRF offense.

Оба seed-partial selectors используют epoch lag **1**. Invalid-seed-partial также ограничен **256 KiB**. Он требует криптографически проверяемый, но неверный partial: malformed commitment/partial отклоняется, а не автоматически считается доказанным нарушением.

Одинаковое evidence повторно не применяется. Это дедупликация конкретного evidence, а не пожизненный иммунитет адреса: разные доказательства могут приводить к новым штрафам. После полного claim ранее выведенный баланс обычный staking slash не затрагивает.

**Consensus reporter сам не отправляет slashing-транзакцию.** При `ConflictingNotarize`, `ConflictingFinalize`, `NullifyFinalize` он публикует сигнал с виновником/round/class и метрику. Внешний watcher должен собрать подписанные votes, а ACTIVE-податель — включить соответствующее evidence в chain. Поэтому запись BYZANTINE в логе ещё не подтверждает фактический slash.

В коде остался внутренний `slash_byzantine` с обычным felony без награды подателю, однако production-вызов этого hook из consensus/execution/Oracle не обнаружен. Его наличие не доказывает автоматический слешинг при каждом обнаружении Byzantine поведения. Аналогично внутренний `force_exit_validator` не является текущим вызовом основного felony-пути: тот использует jail.

Источники: [ACL](crates/system/slashindicator/src/runtime.rs#L376), [gas/восемь selectors](crates/system/slashindicator/src/precompile.rs#L35), [VRF admissibility](crates/system/slashindicator/src/runtime.rs#L797), [seed partial evidence](crates/system/slashindicator/src/runtime.rs#L1032), [расписание ограничений](crates/blockchain/primitives/src/protocol_schedule.rs#L79), [snapshot retention](crates/system/validatorset/src/state.rs#L84), [reporter](crates/blockchain/consensus/src/reporter.rs#L344), [внутренний byzantine hook](crates/system/slashindicator/src/hooks.rs#L94).

## 9. Как читать состояние и не перепутать счётчики

| Что нужно узнать | Источник |
|---|---|
| Эффективные proposer/voter thresholds, обычный slash percent, reward percent | `outbe_getSlashConfig` |
| Proposer miss, voter miss, cumulative felony count SlashIndicator | `outbe_getSlashInfo(address)` |
| Текущий validator lifecycle/status и история ValidatorSet | `outbe_getValidator(address)` |
| Авторитетный bonded stake | `outbe_getStake(address)` |
| Факт обычного felony/evidence | События `ProposerFelony`, `VoterFelony`, `EvidenceFelonyApplied` и типизированные evidence events |
| OCOMP miss и реально списанная сумма | `OcompVoteMissed`: `slashedBonded`, `firstInWindow`, `recoveryDeadline` |
| Jail / возвращение | `ValidatorJailed`, `ValidatorUnjailed`; причину нужно сопоставлять с соседними событиями |

`felony_count` SlashIndicator, `slash_count` ValidatorSet и `ocomp_miss_count` — разные величины. OCOMP денежное списание не обязано увеличивать первые два; его последующий jail увеличивает историю наказаний ValidatorSet. TEE jail не увеличивает `slash_count`. Oracle не увеличивает `SlashIndicator.felony_count`. Поэтому один из этих счётчиков нельзя считать универсальным числом всех денежных штрафов.

`outbe_getSlashConfig` также не возвращает отдельные параметры Oracle/OCOMP. Для утверждений о конкретной сети нужно прочитать её состояние, а не подставлять defaults из этого отчёта.

Источники: [RPC surface](crates/blockchain/rpc/src/api.rs#L380), [эффективные defaults](crates/blockchain/rpc/src/server.rs#L43), [punishment history](crates/system/validatorset/src/runtime.rs#L1829), [OCOMP event](crates/core/metadosis/src/ocomp/vote.rs#L649).

## 10. Границы проверки и расхождения в пояснениях

- Исследованы публичные evidence selectors, вызываемые staking/jail пути, block lifecycle, Oracle tally, OCOMP deadline/recovery и TEE deadline. Графовый поиск по slashing/penalty/jail дополнен прямым поиском вызовов и чтением исходников.
- Использован граф `Users-sakor-outbe-io-outbe-chain`, Tier 2. В начале проверки generation была `2026-09-07T14:04:40Z`; изменённые относительно индекса файлы проверены напрямую. Во время работы индекс обновился: заключительный `check_index_coverage` на generation `2026-09-12T08:18:29Z` подтвердил совпадение metadata всех 29 основных evidence paths и отсутствие зарегистрированных gaps в девяти исследованных scopes. Это best-effort сигнал, не доказательство абсолютной полноты; неполные method-call edges дополнены чтением реальных call sites.
- Root `README.md` в текущей рабочей копии отсутствует. Поэтому сопоставление с полной внешней спецификацией README выполнить нельзя.
- Комментарии `SlashIndicator.schema` содержат старые voter defaults 500/150; исполняемые defaults в runtime — **150/500**. Комментарии и некоторые сообщения логов называют наказание `force-exit`, хотя фактический вызов — `jail_validator`. Приоритет в этом отчёте у исполняемого кода.
- Отдельные существующие тесты просмотрены как подтверждение намерения: первый OCOMP miss/повтор без повторного списания, untouched unbonding, восстановление на deadline, misdemeanor без наказания и jail после freeze. **Тесты в рамках этой задачи не запускались**; прохождение CI, исполнение в testnet и корректность всех криптографических проверок не заявляются.
- Не обнаружено самостоятельного общего штрафа «за любую плохую транзакцию», «за любой невалидный блок», «за отказ DKG», «за голос OCOMP в меньшинстве» или автоматического пожизненного запрета после N нарушений. Такое поведение может приводить к конкретным описанным последствиям — пропуску голоса/слота, отсутствию принятого OCOMP vote, отсутствию readiness либо подаче соответствующего evidence — но не следует приписывать ему дополнительный несуществующий slashing selector.
