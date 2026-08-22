# OCOMP Supervisor ExEx внутри Node

Статус: architecture freeze v8. Три независимые проверки завершены; их обязательные
замечания о реальном FullNode progression gate, durable checkpoint, нескольких live jobs,
`NoQuorum`, canonical result и restart/NOD recovery включены ниже. Интеграционный прогон
добавил обязательную непрерывность authenticated follower через DKG boundary: поиск
`CommitteePreAnnounce` не может предполагать, что carrier всегда является последним блоком
предыдущей эпохи. Прогон также выявил недостижимую producer-ветку: current
activation boundary (`epoch == E`) и next pre-announce (`epoch == E + 1`) должны
получаться разными typed lookup, иначе pre-announce никогда не публикуется.
Повторный E2E уточнил время готовности: artifact уже строится и durable сохраняется
при DKG completion, но в process-local `DkgManager` попадает только на activation.
Нормативный порядок: build once → durable save → publish в `DkgManager` на completion →
pre-announce → активация того же object.
Третий E2E прошёл authenticated boundary и выявил последний fingerprint gap:
follower создавал verifier новой эпохи с `vrf_material_version=0`. Поскольку genesis
якорь имеет `epoch=0/version=0`, а каждая успешная DKG activation увеличивает оба
счётчика ровно на один, authenticated epoch однозначно задаёт version для follower
verifier без изменения carrier wire.
Четвёртый production-shaped E2E подтвердил boundary continuity, но restart при переводе
FullNode в Validator выявил отдельный replay gap: immutable локальный результат уже лежал
в `LocalLysisResultStore`, а новый ExEx state восстанавливал только block checkpoint и
повторно запускал arming старого snapshot. Нормативное восстановление: canonical scan
сначала открывает и валидирует durable local result по exact `JobId`, восстанавливает его
в общей state machine и только при отсутствии результата запускает compute. Wire/state,
membership и алгоритм вычисления не меняются.
Пятый E2E уточнил restart checkpoint: между `OffchainJobRequested` и появлением finalized
job request уже является незавершённой работой, хотя `live_job_count` ещё равен нулю.
Checkpoint не может пройти такой request. При restart старые Reth ExEx notifications могут
быть неприменимы к уже продвинутому in-place CE tree; первая ошибка закрывает только этот
вспомогательный stream, а canonical provider scan от durable watermark остаётся authority.
Шестой E2E подтвердил параллельное вычисление двух live jobs, но выявил
локальную гонку EVM nonce: два vote-thread одного Validator signer могли
подписать разные votes с одним nonce. Compute runners остаются независимыми,
а существующая vote submission для одного EVM signer сериализуется до finalized
receipt. Это локальная signer-capability, а не новый scheduler и не изменение протокола.
Тот же E2E уточнил admission после FullNode→Validator promotion: durable result
исторического Job A восстанавливается, но новый Validator не член его pinned
snapshot. До signer gate ExEx проверяет свой OCOMP key hash в exact request-block
snapshot. Отсутствующий член abstain'ит без транзакции и не блокирует
голос настоящего члена в новом job.

## 1. Цель

Сохранить одну реализацию Off-chain Computation для Validator и FullNode:

- Node сама поднимает ZeroMQ endpoint и управляет подключёнными Workers;
- OCOMP Supervisor работает внутри `outbe-chain` как ExEx;
- Validator через тот же Supervisor ExEx вычисляет результат, подписывает и отправляет vote;
- FullNode через тот же Supervisor ExEx независимо вычисляет результат, не голосует и
  сравнивает локальный результат с canonical результатом кворума;
- FullNode, у которой к deadline локальный результат ещё не готов, удерживает локальное
  продвижение блоков на deadline и ждёт завершения вычисления; exact result снимает barrier,
  а mismatch или невосстановимая локальная ошибка останавливают только эту FullNode.

Отдельного Supervisor-процесса и отдельного OCOMP replay engine нет.

## 2. Scope

Входит в задачу:

1. Встроить существующую Supervisor orchestration в Node как OCOMP ExEx.
2. Перенести владение существующим ZeroMQ Worker endpoint в Node.
3. Переиспользовать существующие Worker registration, heartbeat, lease, dispatch и
   completion сообщения без изменения wire-формата.
4. Переиспользовать существующие CAS, snapshot/export adoption, planner, scheduler,
   reducer, result finalization и vote submission.
5. Выбирать политику OCOMP по режиму Node: `Validator` или `FullNode`.
6. Для FullNode добавить exact comparison с finalized canonical quorum result, локальный
   block-progress barrier на deadline при незавершённом вычислении и typed shutdown при
   mismatch или невосстановимой локальной ошибке.
7. Использовать тот же ExEx и тот же compute pipeline при live execution, restart и
   historical block replay.
8. Обновить runtime wiring, wrappers, E2E, ADR/flow/runbook и тесты этой топологии.
9. Обеспечить переход FullNode follower через certified epoch boundary: verifier следующей
   эпохи восстанавливается из matching `CommitteePreAnnounce`, финализированного доверенным
   предыдущим комитетом, даже если carrier находится раньше последнего блока эпохи.

## 3. Explicit non-goals

Не входят в задачу:

- изменение ValidatorSet, OCOMP membership или quorum;
- изменение Metadosis, Lysis semantics, deadline value, jail или accountability;
- изменение `JobIntentV1`, `ResultVoteV1`, `LysisResultV1`, ABI, events или storage layout;
- новый Node-to-Supervisor сетевой/control-протокол;
- новый Worker protocol или новый транспорт помимо существующего ZeroMQ;
- generic OCOMP VM/framework, validity proofs или fraud proofs;
- отдельный replay actor, distributed queue, replicated archive или DA protocol;
- новый multi-job scheduler, новая capacity policy или новый GC;
- изменение canonical block validity из-за локального состояния FullNode;
- изменение алгоритма вычисления или голосования Validator.
- изменение DKG schedule, Validator consensus engine, формата header artifacts или модели
  доверия committee chaining.

## 4. Runtime topology

```text
outbe-chain Node
├── Node mode: Validator | FullNode
├── Node-owned ZeroMQ endpoint
├── Worker registry / heartbeat / leases
├── existing OCOMP CAS and artifacts
└── OCOMP Supervisor ExEx
    ├── observes finalized OCOMP jobs and outcomes
    ├── freezes/adopts exact inputs
    ├── dispatches deterministic units to Workers
    ├── verifies Worker artifacts
    ├── produces local LysisResultV1
    └── applies Validator or FullNode policy
```

Workers всегда подключаются по ZeroMQ непосредственно к Node. Supervisor ExEx получает
доступ к Worker dispatcher как к внутренней capability Node. Отдельный Supervisor service
не запускается.

## 5. Общие инварианты

1. Существует один production compute path для Validator и FullNode.
2. Node, а не Supervisor, владеет ZeroMQ listener и знает текущий Node mode.
3. Worker не является authority итогового результата: ExEx проверяет artifacts и сам
   формирует canonical local `LysisResultV1`.
4. Job исполняется только по exact finalized `JobIntentV1`, frozen inputs, bundle и
   logical height/time.
5. Порядок Workers, retry, heartbeat, completion time и число Workers не входят в
   семантику результата.
6. Canonical authority сети — quorum Validator votes. Локальный FullNode result никогда
   не меняет canonical state и не делает canonical block невалидным.
7. До сравнения локальный результат и необходимые NOD/output chunks публикуются durable,
   no-clobber и проверяются по commitments.
8. FullNode никогда не отправляет OCOMP vote и не загружает vote signing keys.
9. Validator сохраняет существующие vote, deadline, accountability и jail semantics.
10. Deadline без local result переводит только FullNode в `WaitAtDeadline` и не является
    fatal; mismatch или невосстановимая ошибка сначала сохраняют evidence, затем
    останавливают только эту Node.
11. Несколько live jobs исполняются независимо по `JobId`; обнаружение нового job никогда
    не отменяет и не подменяет уже открытый job.
12. Worker endpoint является локальной доверенной capability Node под её `base-dir`.
    Remote/untrusted Worker transport и новый authentication protocol не вводятся.
13. FullNode не может остановиться на epoch boundary из-за предположения о единственной
    высоте carrier: она ищет exact pre-announce новой эпохи в bounded истории предыдущей
    эпохи, проверяя certificate каждого кандидата уже доверенным предыдущим комитетом.
    Self-finalized `BoundaryOutcome` новой эпохи не заменяет этот trust chain.
14. Producer использует два разных exact lookup: current boundary только для `epoch == E`,
    next pre-announce только для `epoch == E + 1`. Next artifact не может активироваться
    раньше, а current artifact не может выдаваться за pre-announce.
15. Completed DKG boundary artifact строится ровно один раз. До публикации в
    process-local manager он и threshold material durable сохранены. Activation использует
    этот же artifact; повторная сборка или повторная publication запрещены.
16. Follower committee verifier обязан восстанавливать не только participants и
    polynomial, но и exact `vrf_material_version`. Для цепочки от genesis anchor это
    authenticated epoch number; `committee_set_hash_v2` follower и historical state должны
    совпадать побайтово.
17. После restart canonical job discovery сначала проверяет immutable
    `LocalLysisResultStore` по exact `JobId`. Найденный canonical result восстанавливает
    `local_result_digest` и прежнюю policy state machine; повторное snapshot arming и
    повторный compute запрещены. При отсутствии записи используется обычный compute path.
18. Успешный `OffchainJobRequested`, для которого finalized job ещё не материализован,
    является checkpoint barrier. Watermark не может пройти request block в промежутке
    `request observed → finalized JobId observed`.
19. Reth ExEx notification stream не является источником OCOMP authority. Если historical
    notification нельзя применить к текущему in-place CE tree, stream закрывается после
    первой ошибки; provider-based canonical scan и `FinishedHeight` продолжаются без busy loop.
20. Два live jobs могут вычисляться параллельно, но транзакции, подписываемые одним
    Validator EVM signer, проходят существующую durable vote submission последовательно.
    Следующий job получает nonce только после finalized receipt предыдущего; два
    journaled votes одного signer не могут зафиксировать один account nonce.
21. Validator приобретает signer-capability только если его existing OCOMP key hash
    есть в exact pinned snapshot job. Повторный вход или FullNode→Validator promotion
    не дают права голосовать за старый job; локальный result сохраняется,
    а vote path делает abstain без RPC submission.
22. Durable OCOMP checkpoint и Reth `LastFinalizedBlock` не являются одной атомарной
    записью. После crash/restart проверенный checkpoint `C` может временно опережать
    восстановленный finalized watermark provider. Это состояние означает
    `StartupRecovery`, а не finality regression: ExEx не сканирует новые блоки, не
    откатывает и не переписывает `C`, не публикует readiness выше provider и ждёт
    exact catch-up. После первого `target == C` с тем же hash включается прежняя строгая
    runtime-проверка, и любая последующая регрессия остаётся fatal.
23. Typed fatal обязан остановить Node через существующий lifecycle exit channel, но сам
    ExEx future не завершается до teardown. Возврат `Ok`/`Err` из ExEx не используется
    как shutdown API, потому что Reth считает любое завершение ExEx panic-condition.

## 6. Режим Validator

```text
Finalized JobIntent
→ exact input adoption
→ Worker execution over Node-owned ZeroMQ
→ verified local LysisResultV1
→ existing result signing
→ existing ResultVoteV1 submission
→ existing quorum/deadline/accountability/jail
```

Для Validator меняется deployment topology, но не протокольное поведение. Существующие
ключи, sign-once и vote submission используются Supervisor ExEx внутри Node.

## 7. Режим FullNode

```text
Finalized JobIntent
→ exact input adoption
→ Worker execution over Node-owned ZeroMQ
→ durable local LysisResultV1
→ finalized canonical quorum result
→ exact comparison
```

Результат:

- exact local/canonical match: `Proceed`, локальные NOD/output данные готовы;
- local result ещё считается до deadline: `Wait`;
- local result отсутствует на deadline: `WaitAtDeadline`, не продвигать FullNode дальше
  deadline block и ждать завершения вычисления;
- local/canonical mismatch: `FatalMismatch`, остановить FullNode;
- невосстановимое повреждение CAS/input/program: `FatalLocal`, остановить FullNode.

Deadline определяется block height. Он не делает поздний exact result недействительным:
он устанавливает локальный barrier. После durable exact result и проверки против canonical
outcome barrier снимается, и FullNode продолжает live sync либо historical catch-up.

## 8. Минимальная state machine

```text
Idle
  └── finalized JobIntent → Computing

Computing
  ├── local result ready → LocalReady
  ├── temporary Worker/input unready before deadline → Wait/Computing
  ├── deadline without durable local result → WaitAtDeadline
  ├── finalized NoQuorum → ClosedNoQuorum
  └── unrecoverable local corruption → FatalLocal

WaitAtDeadline
  ├── local result ready → LocalReady
  ├── temporary Worker/input unready → remain WaitAtDeadline
  ├── finalized NoQuorum → ClosedNoQuorum
  └── unrecoverable local corruption → FatalLocal

LocalReady
  ├── canonical result pending → wait for canonical outcome
  ├── canonical exact → Verified
  ├── canonical mismatch → FatalMismatch
  └── finalized NoQuorum → close local job without vote/readiness claim

Verified
  └── publish OCOMP/NOD readiness and continue

ClosedNoQuorum
  └── cancel local work, release inputs and progression barrier; Lysis/NOD не активируются

FatalMismatch | FatalLocal
  └── persist evidence → typed Node shutdown
```

Для Validator отсутствие vote/result обрабатывается существующим protocol deadline и
accountability. FullNode-specific wait/fatal policy не изменяет Validator flow.

### 8.1. Полная transition matrix

| Local state | Finalized event / condition | Node mode | Effect | Replay/restart |
|---|---|---|---|---|
| `Idle` | exact `JobIntent` | оба | создать state по `JobId`, принять exact inputs, начать compute | повтор идемпотентен |
| `Computing` | новый другой `JobId` | оба | начать второй независимый runner; старый не отменять | оба восстанавливаются из canonical scan |
| `Computing` | local exact result | оба | durable publish result + immutable CAS refs, затем `LocalReady` | переиспользовать запись |
| `Computing` | deadline, result отсутствует | Validator | существующий protocol deadline/accountability | без новой локальной политики |
| `Computing` | deadline, result отсутствует | FullNode | `WaitAtDeadline`, readiness остаётся на `D-1` | после restart барьер восстанавливается |
| `Computing` / `WaitAtDeadline` / `LocalReady` | finalized `NoQuorum` | FullNode | `ClosedNoQuorum`, отменить work, снять barrier, без Lysis/NOD | terminal |
| `LocalReady` | canonical exact | FullNode | `Verified`, разрешить progression и NOD readiness | terminal record переоткрывает CAS |
| `LocalReady` | canonical mismatch | FullNode | evidence, `FatalMismatch`, shutdown этой Node | повторный startup снова fail-closed |
| любое nonterminal | retryable Worker/input unavailable | оба | сохранить state, retry существующим lease protocol | продолжить после restart |
| любое nonterminal | доказанная порча input/CAS/program | FullNode | evidence, `FatalLocal`, shutdown этой Node | не превращать в retry |
| follower epoch `E-1` | matching pre-announce `E` находится до последнего блока эпохи | FullNode | проверить carrier комитетом `E-1`, зарегистрировать verifier `E`, продолжить replay | тот же bounded scan после restart |
| follower epoch `E-1` | matching pre-announce `E` ещё отсутствует | FullNode | retry без self-certified fallback и без продвижения в `E` | повторить после новых finalized данных |
| producer epoch `E` | DKG artifact `E+1` готов, activation ещё не нужна | Validator | выбрать artifact через next-preannounce lookup и нести `CommitteePreAnnounce(E+1)` | тот же artifact может повторяться до activation |
| producer epoch `E` | наступил activation boundary `E` | Validator | current-boundary lookup возвращает только artifact `E`; next artifact не активируется | chain ancestry определяет duplicate/emission |
| DKG epoch `E` | ceremony `E+1` completed до activation | Validator | build artifact once, durable save, publish в manager, сохранить в pending activation | restart восстанавливает тот же durable artifact |
| follower registers epoch `E` | authenticated preannounce outcome decoded | FullNode | создать verifier с participants, polynomial и `vrf_material_version=E`; historical fingerprint обязан совпасть | restart повторяет то же восстановление |
| request observed, finalized job ещё отсутствует | checkpoint advance | оба | не продвигать watermark за request block | restart повторно сканирует request |
| historical ExEx notification возвращает ошибку CE parent | auxiliary stream error | оба | один warning, закрыть stream, продолжить provider scan | тот же durable watermark определяет replay |
| два `LocalReady` jobs | Validator vote submission | Validator | сериализовать nonce/sign/submit/finalize на общей signer-capability; compute не блокировать | durable per-job journals повторяются в том же однопоточном signer path |
| `LocalReady` historical job | локальный OCOMP key нет в pinned snapshot | Validator | abstain до signer gate; не создавать vote journal/transaction | exact request-block snapshot даёт тот же ответ после restart |
| `StartupRecovery(C)` | provider finalized `< C`, checkpoint hash уже canonical | оба | ждать; не сканировать, не rewind/delete/rewrite checkpoint, не emit `FinishedHeight`, readiness не выше provider | повторять до catch-up |
| `StartupRecovery(C)` | provider finalized `== C` и hash совпадает | оба | перейти в `Running`, восстановить jobs обычным canonical scan | дальнейшая runtime-регрессия снова fatal |
| `StartupRecovery(C)` | provider finalized `== C`, hash отличается; checkpoint block отсутствует/noncanonical | оба | typed fatal/startup failure | не ослаблять canonical validation |
| `Running` | provider finalized ниже уже принятого watermark | оба | typed fatal: настоящая runtime-регрессия | evidence + orderly Node shutdown |
| любое состояние | typed fatal опубликован | оба | отправить existing Node exit; ExEx остаётся pending до teardown | Reth не получает ложный `ExEx finished` panic |

Ошибки runner классифицируются typed adapter'ом как `Retryable` или `Unrecoverable` до
перехода FSM. Строковый `Stage { detail }` не используется для принятия решения wait/fatal.

## 9. Canonical outcome, FullNode barrier и checkpoint

- ExEx может начинать вычисление только после finalized JobIntent authority.
- Tentative/reorgable result не разрешает `Verified` и не вызывает mismatch fatal.
- Exact comparison выполняется только с finalized canonical quorum result.
- Локальная проверка не вставляется обратно в EVM, transaction validation или consensus.
- До `Verified` FullNode не сообщает OCOMP/NOD readiness для соответствующего результата.
- Если FullNode дошла до deadline `D` без durable local result, второй optional readiness
  handle в follower `ExecutorActor` остаётся на `D-1`. Перед `new_payload` блока `D+1`
  actor ждёт exact OCOMP readiness так же, как уже ждёт существующую projection readiness.
  Validator получает `None`, поэтому его execution path не меняется.
- Exact result снимает progression gate; `Fatal*` отправляет typed shutdown верхнему
  lifecycle Node.
- `ExExEvent::FinishedHeight` используется только для ExEx notification/pruning progress и
  не считается execution barrier.
- Canonical result определяется finalized успешной q-forming `ResultVoteV1`: Node связывает
  receipt с `LysisActivated`, декодирует result из calldata, повторно проверяет job/snapshot/
  OCOMP bindings и сверяет digest с `OcompCompletedBindingV1`. Для NOD readiness локальные
  roots/counts дополнительно сверяются с `ActiveGenerationV1`.
- Finalized `NoQuorum` является terminal outcome без canonical Lysis result и снимает barrier
  из любого nonterminal local state.
- Локальный `discovery_generation` старого single-current Supervisor-журнала не является
  authority embedded-пути. Export adoption связывается по exact `JobId`, finalized cursor и
  hash полного `FinalizedJobSpecV1`; это позволяет нескольким jobs исполняться независимо,
  не вводя новый durable job ledger.

Checkpoint нормативен: ExEx хранит один durable contiguous watermark `{height, block_hash}`,
а не job ledger. Watermark fsync'ится только когда каждый observed request до высоты уже
материализован в exact job и все такие jobs получили локальное terminal состояние
(`Verified` или `ClosedNoQuorum`), до публикации `FinishedHeight`. На startup
Node проверяет hash против canonical chain, вызывает `set_notifications_with_head` от этого
watermark и повторно сканирует последующий canonical диапазон. Несовпадение checkpoint с
каноном — typed local startup failure, не молчаливый skip.

Проверенный checkpoint и Reth finalized marker сохраняются разными владельцами и могут
разойтись после crash. Поэтому startup имеет отдельную ограниченную фазу восстановления:
если provider сообщает высоту ниже checkpoint, ExEx ждёт догонки, сохраняя checkpoint и
canonical hash неизменными. Такая терпимость разрешена только до первого exact catch-up.
Она не разрешает rewind, повторную обработку уже checkpointed диапазона или runtime-
регрессию после перехода в `Running`.

Обычные Reth ExEx notifications только обслуживают backpressure. Если их historical replay
возвращает ошибку (в частности CE exact-parent mismatch после warm restart), embedded OCOMP
логирует её один раз и перестаёт читать этот stream. Это не отключает canonical scan:
`provider.finalized_block_num_hash()` и exact block/state reads остаются единственным входом
reducer, а успешно продвинутый durable watermark по-прежнему публикуется через
`ExExEvent::FinishedHeight`.

## 10. Live execution, restart и historical replay

Отдельного алгоритма replay нет:

```text
ordinary historical block replay
→ OCOMP ExEx observes historical JobIntent
→ same input adoption
→ same SupervisorJobRunner and Workers
→ at historical deadline without result: hold local block progression and wait
→ same local result
→ historical finalized quorum result
→ exact comparison and resume catch-up
```

Требования:

- crash до durable local result повторяет exact JobId идемпотентно;
- crash после local result, но до verification, переиспользует immutable result/CAS;
- unresolved job не может быть пропущена после restart;
- live и historical FullNode применяют одну policy: deadline без result удерживает локальный
  block progression до результата, но не создаёт fatal сам по себе;
- входы job удерживаются существующим retention/CAS до локального terminal outcome.

Параллельный replay engine не добавляется. Текущий протокол допускает несколько live jobs,
поэтому ExEx держит минимальную in-memory map `JobId → local state/runner`. Она восстанавливается
canonical scan от durable watermark и существующего retention registry/CAS. Новый durable
multi-job ledger, scheduler или policy запрещены scope. Текущая standalone-логика
`cancel_superseded_job` в embedded path не используется.

После restart `LocalReady`/`Verified` переоткрывает immutable manifest, plan, admissions и
result chunks по существующим deterministic CAS references. Один durable local result record
на `JobId` хранит canonical `LysisResultV1` и ссылки, достаточные для повторной проверки и NOD
serving; сами chunks не дублируются.

## 11. Ownership

- Node mode, ZeroMQ listener, Worker registry, FullNode progression gate and shutdown:
  `outbe-chain` / Node runtime.
- OCOMP orchestration and deterministic computation: embedded Supervisor ExEx using
  `outbe-ocomp` library.
- Worker protocol: existing `outbe-ocomp` Worker transport.
- CAS, input/output artifacts and local result: existing OCOMP storage modules.
- Validator vote/sign-once/submission: existing Validator policy inside Supervisor ExEx.
- FullNode comparison and fatal policy: FullNode branch of Supervisor ExEx.
- FullNode block progression: optional OCOMP readiness handle в follower `ExecutorActor`;
  engine stack создаёт и передаёт его только в режиме FullNode.
- ExEx checkpoint/backfill: embedded OCOMP reducer; один contiguous `{height, hash}` watermark.
- Canonical quorum and state application: existing Metadosis/EVM path, unchanged.
- Authenticated committee continuity: consensus DKG manager/application producer разделяют
  current-boundary и next-preannounce lookup; consensus follower выполняет bounded поиск
  matching prior-epoch-finalized `CommitteePreAnnounce`. Artifacts и DKG schedule не меняются.
- DKG completion → durable pending artifact → manager publication → activation ownership:
  `crates/blockchain/engine/src/stack.rs`; один immutable artifact проходит весь путь.
- Authenticated epoch → follower verifier VRF version: consensus `follow::CommitteeChain`;
  canonical historical fingerprint проверяет engine follower reconciliation.

## 12. Expected production paths

Ожидаемый узкий набор seam'ов:

- `bin/outbe-chain/Cargo.toml` — использовать `outbe-ocomp` library;
- `bin/outbe-chain/src/main.rs` — Node mode, Node-owned Worker endpoint, ExEx install,
  FullNode progression gate и typed shutdown wiring;
- `bin/outbe-ocomp/src/lib.rs` — экспортировать существующие reusable Supervisor pieces;
- `bin/outbe-ocomp/src/main.rs` — удалить standalone Supervisor runtime path, сохранив
  Worker и необходимые operational roles;
- `bin/outbe-ocomp/src/supervisor_job.rs` — переиспользовать compute runner без изменения
  Lysis semantics; runner принимает Node-owned dispatcher и возвращает typed error class;
- `bin/outbe-ocomp/src/worker_transport.rs` — переиспользовать ZeroMQ router; wire не менять;
- `crates/blockchain/node/src/ocomp/local_result.rs` — durable result/comparison/fatal evidence,
  без EVM authority;
- `crates/blockchain/consensus/src/executor/actor.rs` — optional FullNode OCOMP readiness перед
  `new_payload`; Validator path не меняется;
- `crates/blockchain/engine/src/stack.rs` — создать/передать readiness только follower FullNode;
- `crates/blockchain/consensus/src/follow/engine.rs` и follower tests — не привязывать
  authenticated pre-announce carrier к единственной вычисленной высоте последнего блока;
- `crates/blockchain/consensus/src/dkg_manager.rs`, `application/handler.rs` и focused tests —
  разделить exact current-boundary (`E`) и next-preannounce (`E+1`) lookup, не меняя
  verifier, artifact или DKG schedule;
- `crates/blockchain/engine/src/stack.rs` и `stack_tests.rs` — после DKG completion построить
  artifact один раз, durable сохранить, опубликовать в manager и передать тот же
  object в activation;
- `crates/blockchain/consensus/src/follow/mod.rs` и tests — создавать verifier с VRF version,
  равной authenticated epoch, чтобы follower parent-record fingerprint совпадал с state;
- существующие finalized notification/provider, input exporter и retention seam — canonical
  scan, exact input adoption и удержание данных до local terminal outcome;
- минимальный FullNode-only ExEx orchestration module в `outbe-chain` или reusable OCOMP
  library seam без Cargo cycle;
- runtime wrappers, E2E harness, ADR/flow/runbook.

Stop/re-plan trigger: production path вне этого ownership map, consensus-visible type change,
новый transport/protocol, новый replay ledger, новый scheduler либо третий semantic pass по
hot file.

## 13. Test-first acceptance

Validator regression:

1. Node-owned ZeroMQ принимает Workers.
2. Validator ExEx вычисляет тот же result.
3. Validator создаёт и отправляет тот же `ResultVoteV1`.
4. Quorum, deadline, accountability и jail не изменились.

FullNode:

1. FullNode не загружает vote keys и не отправляет vote.
2. Local-first и canonical-first exact результаты приводят к `Verified`.
3. До exact verification OCOMP/NOD readiness отсутствует.
4. Local result отсутствует на live или historical deadline — FullNode удерживает следующий
   block, остаётся в `WaitAtDeadline` и продолжает вычисление.
5. Late exact result снимает barrier, после чего FullNode продолжает sync/catch-up.
6. Mismatch — durable evidence и shutdown только FullNode.
7. Worker временно пропадает до или после deadline — `Wait`, lease/retry через существующий
   protocol; сам timeout/heartbeat loss не является fatal.
8. Restart повторяет unresolved exact job и не пропускает canonical outcome.
9. Historical replay использует тот же compute path.
10. NoQuorum не создаёт ложного canonical match.
11. Validator и другие FullNodes продолжают работу после одной FullNode fatal.
12. Два одновременно live `JobId` вычисляются независимо; новый не отменяет старый.
13. Durable watermark не проходит unresolved job; restart повторно доставляет её и сохраняет
    exact `{height, hash}` identity.
14. Finalized `NoQuorum` снимает deadline barrier из `Computing`, `WaitAtDeadline` и
    `LocalReady`.
15. Request до finalized job блокирует checkpoint; restart повторно обнаруживает exact event.
16. Ошибка historical ExEx notification не создаёт busy loop: stream закрывается после
    первой ошибки, provider scan восстанавливает durable local result и продолжает progression.

Topology/E2E:

1. Standalone Supervisor не запускается.
2. Validator и FullNode Nodes сами поднимают ZeroMQ.
3. Workers подключаются напрямую к соответствующей Node.
4. Tribute → Metadosis → JobIntent → Workers → validator votes → quorum → FullNode exact
   comparison → Lysis/NOD проходит production-shaped путём.
5. Отдельные E2E проверяют FullNode deadline wait/resume и mismatch stop.
6. Warm promotion сохраняет синхронизированный Reth datadir и OCOMP artifacts, но не
   изменяет committed NodeHost identity на месте: после остановки FullNode его локальные
   TEE/NodeHost artifacts архивируются, а Validator получает новую identity перед запуском.
   Профиль и `node_id` committed NodeHost manifest остаются неизменяемыми.
7. FullNode проходит хотя бы одну реальную DKG boundary до promotion; pre-announce,
   расположенный раньше последнего блока предыдущей эпохи, не останавливает replay.
8. Producer до activation публикует `CommitteePreAnnounce(E+1)` из next lookup, а на
   activation публикует `BoundaryOutcome(E)` из current lookup; перепутывание epoch
   покрыто focused test.
9. Crash между DKG completion и activation восстанавливает exact pending artifact;
   preannounce и activation используют одинаковые bytes.
10. Reconstructed epoch-1 verifier имеет `vrf_material_version=1`; построенный им
    `committee_set_hash_v2` равен historical snapshot hash после boundary.
11. FullNode→Validator restart с уже опубликованным local result не вооружает старый
    snapshot повторно; exact local result восстанавливается из durable store и Job A
    продолжает жить по исходному pinned snapshot.
12. Dynamic-membership E2E переходит следующую реальную суточную Cycle-
    границу штатным test-only logical clock перед ожиданием Job B;
    production Metadosis settlement не обходится.

Quality gates: formatting, Clippy, affected workspace tests, OCOMP registry/shape/capacity,
E2E compile, production-shaped LocalNet outside sandbox, документация и scope audit.

## 14. PR slices

1. **Shared embedded Supervisor seam**: сделать текущий runner/Worker router вызываемыми из
   `outbe-chain`; Node создаёт dispatcher, runner его принимает. Production routing пока не
   переключается.
2. **Common ExEx + policies**: canonical notification reducer, per-`JobId` in-memory runners,
   typed errors, Validator sign/submit policy и FullNode no-key policy. Переключить production
   только когда оба режима целиком собраны.
3. **FullNode replay seam**: durable watermark/backfill, canonical q-forming result join,
   `NoQuorum`, optional `ExecutorActor` readiness и restart/NOD recovery.
4. **Authenticated boundary continuity**: typed current/next producer lookup и bounded
   authenticated поиск matching pre-announce внутри предыдущей эпохи; отдельный
   узкий slice без изменений artifacts/DKG schedule/Validator semantics.
5. **Restart rehydration correction**: не пропускать pre-finalization request checkpoint'ом,
   закрывать ошибочный auxiliary notification stream без остановки provider scan, загрузить
   immutable local result до compute dispatch и восстановить existing per-job state; без новых
   форматов и scheduler semantics.
6. **Crash-checkpoint recovery correction**: test-first локальная state machine
   `StartupRecovery → Running` и orderly fatal handoff в `outbe-chain` ExEx. Отдельный
   функционально завершённый checkpoint внутри текущей testnet-preparation ветки; consensus,
   canonical state, wire/layout, Reth и harness не меняются.
7. **Runtime/E2E/docs**: удалить standalone Supervisor startup, обновить wrappers, production
   E2E, ADR/flow/runbook и выполнить финальный scope audit.

Каждый PR компилируется, проходит применимые тесты и не направляет production в частично
реализованный путь.

Hot files и допустимые semantic passes:

| Hot file | Pass 1 | Pass 2 (только integration correction) |
|---|---|---|
| `bin/outbe-ocomp/src/main.rs` | извлечь standalone assembly в library seam | удалить старый Supervisor routing |
| `bin/outbe-ocomp/src/supervisor_job.rs` | injected dispatcher + typed result | integration correction |
| `bin/outbe-chain/src/main.rs` | embedded runtime wiring | shutdown/startup correction |
| `crates/blockchain/consensus/src/executor/actor.rs` | optional readiness + tests | integration correction |
| `crates/blockchain/engine/src/stack.rs` | FullNode-only readiness wiring | integration correction |
| `crates/blockchain/node/src/ocomp/local_result.rs` | per-job durable result/references/evidence | restart correction |
| `bin/outbe-ocomp/src/embedded_runtime.rs` | reusable embedded domain | local-result rehydration adapter |
| `bin/outbe-chain/src/ocomp_exex.rs` | canonical scan + Validator/FullNode policies | restart checkpoint/notification/rehydration correction |
| `crates/blockchain/consensus/src/follow/engine.rs` | bounded authenticated carrier lookup | integration correction |
| `crates/blockchain/consensus/src/dkg_manager.rs` | typed current/next pending lookup | integration correction |
| `crates/blockchain/consensus/src/application/handler.rs` | next lookup только для pre-announce | integration correction |
| `crates/blockchain/engine/src/stack.rs` | publish already-durable artifact at DKG completion | integration correction |
| `crates/blockchain/consensus/src/follow/mod.rs` | authenticated epoch задаёт verifier VRF version | integration correction |

`E2E-EXEX-RESTART-001` исчерпал два первоначально разрешённых semantic pass для
`bin/outbe-chain/src/ocomp_exex.rs` и поэтому остановил реализацию. После трёх независимых
аудитов файл допускается к одному новому, заранее ограниченному pass только после явного
решения пользователя: добавить `StartupRecovery` и orderly fatal handoff вместе с RED
регрессиями. Любая необходимость менять второй production-файл или canonical поведение
снова останавливает работу и требует нового re-plan.

Release SGX/no-attest verification subsequently exposed one incorrect private classifier row:
the canonical and already-existing `AwaitingFinality(finalized)` phase was rejected before its
exact `open_height`. After three independent audits and explicit user authorization,
`bin/outbe-chain/src/ocomp_exex.rs` receives one additional correction pass limited to restoring
that row: discover and materialize the finalized job while leaving any durable local result dormant
until `VotingOpen`. The canonical record, Metadosis transitions, embedded FSM/runtime, ABI, storage,
wire types, timing, and harness semantics remain unchanged. The inline classifier matrix and the
release `@ocomp-late-local-result` SGX/no-attest scenario are the required evidence. No other
production path is approved by this correction.

The next exact release run reached quorum and exposed a separate composition defect: the durable
checkpoint pruned a terminal job before an already-owned compute/vote callback arrived, so the
strict reducer reported `UnknownJob` and the node published a generic fatal. A full FSM audit also
proved that canonical `Completed` is not locally terminal for a FullNode still awaiting exact local
verification, and that restart currently restores a durable result before applying canonical
terminal authority.

After three independent audits and explicit user authorization, one correction slice is approved:

- `bin/outbe-chain/src/ocomp_exex.rs`: classify private callbacks against both runtime/reducer
  generations before side effects; both absent after checkpoint pruning is a `ProtocolOwned` no-op,
  one-sided or mismatched presence is fatal; apply canonical disposition before restoring durable
  local results;
- `bin/outbe-ocomp/src/embedded.rs`: `LocalFailed` is state-based—current FullNode
  `Computing | WaitAtDeadline` is `FatalLocalFailure`, current Validator is `Abstain`, and already
  locally terminal states are `ProtocolOwned`;
- tests are limited to inline ExEx tests and
  `bin/outbe-ocomp/tests/embedded_state.rs`.

`FinalizedAwaitingOpen` leaves durable local results dormant until `VotingOpen`; `Completed` is
observed before FullNode restore/verification and never starts a Validator vote; closed terminals
cancel without restore. Checkpoint pruning remains immediate and bounded. The public reducer keeps
strict `UnknownJob`; arbitrary unknown events, duplicate conflicting compute callbacks, new durable
queues, and changes to protocol/state, `embedded_runtime.rs`, ABI, storage, wire, timing, SGX, or
harness semantics are non-goals. These two production files receive exactly this authorized
integration-correction pass. Any further production path or semantic correction stops the slice.

Any semantic pass beyond the explicitly frozen corrections above, a new canonical type, new
transport, new durable queue, or path outside the map requires another stop and architecture review.

## 15. Вопросы независимой проверки

Каждый reviewer должен ответить отдельно:

1. Позволяет ли решение Validator действительно выполнить Off-chain Computation, подписать
   и отправить vote без изменения протокольной семантики?
2. Решает ли единый ExEx-путь historical replay FullNode, включая restart, deadline и
   восстановление локальных NOD/output данных?
3. Достаточен ли план, не содержит ли неопределённостей и является ли он стратегически
   правильным без расширения scope и лишней сложности?
