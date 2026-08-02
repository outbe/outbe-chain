# План улучшения `crates/core/metadosis` по результатам Citadel-аудита

Статус Citadel hardening: **implementation applied in the working tree;
exact-revision Linux evidence pending**

Статус architecture deepening follow-up: **A1–A6 implementation and local
audit complete; exact-revision Linux evidence pending**

Основание: [`metadosis-citadel-audit.md`](metadosis-citadel-audit.md), текущий `main` на
2026-07-30.

## 1. Цель и правило закрытия

Цель плана — закрыть конкретные дефекты CITADEL-001..008 без переписывания
OCOMP или изменения normal-path экономики Metadosis. Единственное явно
разрешённое расширение — общепроектный каркас `VerificationLedger`; он не
расширяет production behavior ноды. Экономические исключения в scope
ограничены двумя новыми WWD terminal outcomes: полностью пропущенный OFFERING
и достижимый `cap+1`. Оба обязаны иметь точную однократную маршрутизацию value
и typed terminal receipt. `SupplyExceedsAuctionDomain` не является третьим
новым WWD outcome: это уже существующая Desis capacity rejection, которую план
сужает до одного typed committed result.

Finding считается закрытым только когда одновременно выполнены три условия:

1. зафиксировано точное условие, при котором дефект достижим;
2. изменение устраняет это условие структурно либо задаёт однозначный
   протокольный outcome;
3. closure test проходит через тот же production seam, через который команда
   выполняется в ноде.

Зелёный direct fixture test сам по себе не закрывает production claim.

## 2. Scope

### В scope

- `crates/core/metadosis/**`;
- только необходимые Rust-seams соседних модулей:
  - `crates/core/desis/src/{api,runtime}.rs`;
  - `crates/system/cycle/**` для production-dispatch и rollback tests;
  - `crates/blockchain/evm/**` для OCOMP production-path tests, узких adapter
    imports и выдачи purpose-bound Metadosis authority;
  - `crates/blockchain/primitives/src/storage/{mod,handle,hashmap,metadosis_mutation}.rs`
    только для приватного Metadosis mutation frame/guard, который связывает
    mutation с текущим execution/checkpoint scope; общий storage API не
    меняется;
  - `crates/blockchain/node/src/ocomp/fork.rs` и точная startup config seam,
    чтобы fresh-devnet node отклонял отсутствие/invalid OCOMP install в genesis
    manifest;
  - `crates/core/tribute/**` только для constant-size aggregate forfeiture
    transition, потому что существующий интерфейс не может атомарно завершить
    populated partition без certified Lysis result;
  - `crates/core/compressed-entities/src/api.rs` только для узкого read-seam
    `ExecutionScope::authenticated_partition_root(TributeWwd)`, необходимого
    этому forfeiture transition; generic CE API, storage, schema и retirement
    redesign запрещены;
  - `crates/core/tributefactory/**` только если текущий raw status read должен
    быть заменён typed query;
- только ADR и normative-документы, перечисленные в разделе 9.1; упоминание
  соседнего ADR не делает его production-модуль частью scope;
- общий versioned `VerificationLedger` schema/verifier в e2e-harness,
  полученный минимальным извлечением reusable evidence-механики из
  `ocomp_evidence`;
- общепроектный ledger index и два domain packs:
  - существующий OCOMP pack без изменения его requirements/closure semantics;
  - новый Metadosis pack только для CITADEL-001..008;
- только выбранный fresh-devnet genesis/OCOMP install artifact и его fixture;
- новые тесты, непосредственно закрывающие CITADEL-001..008.
- узкий platform adapter для kernel-authenticated local peer credentials на
  Linux/macOS и host-portable Docker launcher внутри `e2e-harness`; test,
  assemble и verify по-прежнему исполняются только в pinned Linux image.
- architecture deepening follow-up только для существующего Metadosis
  mutation/test seam и его текущих непосредственных test consumers:
  private fixture kernel, feature-gated semantic test facade, compile-fail
  gates, typed pre-launch genesis builder и доказательное удаление только
  избыточных nested checkpoints.

### Вне scope

- изменение OCOMP quorum, wire protocol, worker topology или owner receipts;
- переписывание OCOMP FSM, который уже является сильнейшей частью модуля;
- изменение длительности WWD-фаз или normal, non-missed-window economic
  allocation;
- общий redesign `StorageHandle`, compressed-entities, storage macros или schemas соседних модулей:
  разрешён только узкий Metadosis-specific lease, без reusable framework;
- новый универсальный `Command/Authority` framework;
- новый общий WWD projection/validation module: status/day-type decode уже
  разделяется через typed enums, а point read, ABI read и aggregate guard имеют
  намеренно разные membership, missing-record и cost semantics;
- изменение публичного Metadosis ABI ради возврата внутренних typed outcomes;
- compatibility exports для raw `schema`, `runtime`, `ocomp`,
  `MetadosisContract`, provider или `StorageHandle` под `test-utils`;
- новый persisted due-index до доказательства, что bounded in-memory sort не
  помещается в block budget;
- изменение terminal EmissionLimit callback contract;
- аудит, requirement mapping или evidence population любых модулей, кроме
  существующего OCOMP pack и нового Metadosis pack;
- изменение OCOMP test IDs, task ownership, deferrals, retired rows или
  closure semantics при извлечении общего `VerificationLedger`;
- общепроектный redesign CI, artifact storage или release orchestration;
- production-поддержка macOS node/OCOMP runtime за пределами узкого
  local-control peer-credential adapter, нужного для native harness build;
- governance recovery повреждённого consensus state;
- deployment, fork activation, backfill и миграция реально запущенной сети;
- поддержка replay/state, созданных до этого изменения: target — только новый
  fresh devnet с новым genesis.

## 3. Зафиксированные инварианты

Эти правила не являются открытыми вопросами:

1. process environment не может влиять на consensus execution;
2. Обычный `Err` production command означает отсутствие committed provider
   state/indexes и ordered events; для CE-capable command `CeWorkCheckpoint`
   восстановлен до entry state. Gas, metrics, tracing и fault counters в это
   semantic равенство не входят. Ошибка rollback infrastructure является
   fatal execution abort, а не обычным command `Err`;
3. Предусмотренный доменный исход представлен закрытым typed value до commit и
   фиксируется только через `Ok`. Внешний command/ABI может вернуть `()` или
   empty bytes, если исход однозначно записан как typed state, receipt или
   event. Обычный `Err` никогда не оставляет полезную partial terminalization;
4. пропущенное целиком окно OFFERING обрабатывается **fail-closed до
   settlement**: Desis и Lysis не вызываются, late auction не создаётся, exact
   limit маршрутизируется в carry-over не более одного раза;
5. Широкая emergency terminalization сохраняется как отдельный reducer event
   и commit-owned command. Текущий test-only `mark_wwd_failed` не является
   production command и не становится публичным raw setter;
6. storage-set iteration order не является protocol order;
7. schema/layout меняются только внутри Metadosis и только если это необходимо
   для durable receipts/invariants; поскольку target — fresh devnet,
   migration/backfill и mixed-history replay не требуются, но новый genesis и
   slot/layout assertions обязательны;
8. внешний `U256::ZERO` в terminal emission sink означает «unused amount = 0»
   и остаётся частью существующего EmissionLimit-контракта. Внутри Metadosis
   outcome должен быть typed;
9. ни `BlockRuntimeContext`, ни raw `StorageHandle` не являются authority.
   Stateful-команда исполняется только внутри приватного
   `with_metadosis_mutation` frame: guard создаёт и удерживает purpose-bound
   lease, callback его не получает, а mutation API проверяет frame-owned proof;
10. `cap+1` не является fatal invariant: достижимый overflow завершается
    детерминированным forfeiture одного явно выбранного WWD, с exact value
    routing и durable typed receipt;
11. compatibility target — только fresh devnet. Изменения consensus behavior
    являются genesis behavior; hard-fork activation и поддержка старого state
    не входят в план;
12. legacy/no-OCOMP execution отсутствует. Для выбранного fresh devnet OCOMP
    profile является genesis-active до первого Cycle command в block 1;
    отсутствие/invalid binding или runtime activation делают node config
    недопустимой до block execution;
13. READY priority — `(scheduled_process_time, worldwide_day)`, строго
    oldest-first. При нескольких eligible WWD один tick обрабатывает ровно один
    первый key; container/insertion order не участвует в выборе;
14. единственный committed Desis rejection —
    `SupplyExceedsAuctionDomain { supply, max: u128::MAX }`. Он не создаёт
    auction state и маршрутизирует full supply в Promis. Любая другая Desis
    error означает `Err` и полный semantic rollback;
15. обязательная closure/release lane — Linux, совпадающий с generated
    fresh-devnet OCOMP machine profile (`x86_64`, Ubuntu 24.04). Только
    artifact из этой lane может закрывать Citadel gates. Один Rust launcher из
    `e2e-harness` запускается на macOS/Linux, pin-ит exact Linux/amd64 image и
    связывает raw Docker inspect facts с runner receipt; native macOS run не
    считается closure evidence;
16. `VerificationLedger` является общепроектным versioned evidence-каркасом,
    но этот план добавляет normative requirements только для Metadosis.
    Существующий OCOMP pack должен пройти без semantic drift; другие модули не
    получают фиктивные, deferred или placeholder rows.
17. `commit_transition` владеет purpose-bound command frame, checkpoint и
    pre/post aggregate validation. `CommitPermit` создаётся единственным
    private transition-scoped owner helper внутри commit module, живёт одну
    outer transition и обязателен для всех production outer-WWD mutations,
    включая emergency path. Reducer, runtime и OCOMP не могут создать или
    сохранить permit; существующие OCOMP index/owner-receipt writers permit не
    получают и сохраняют свои protocol-specific replay identities.
18. Raw fixture writes разрешены только private fixture kernel внутри
    `outbe-metadosis` для precondition или intentional corruption. Через
    `test-utils` доступен только semantic facade из узких opaque scenarios,
    typed observations, production commands и явно названных corruption
    operations; arbitrary closure/key/slot/value access запрещён.
19. Nested checkpoint сохраняется только если ошибка поглощается локально и
    execution продолжается либо функция является самостоятельным production
    mutation entrypoint без эквивалентного внешнего rollback owner. Отдельное
    domain ownership, включая OCOMP q-forming, само по себе checkpoint не
    оправдывает.
20. Candidate 04 (`WWD record validation reuse`) закрыт без реализации:
    существующие typed point read, ABI read и aggregate guard остаются разными
    interfaces с разными guarantees и cost shapes.

## 4. Достижимость findings и disposition

| Finding | Severity | Когда реально проявляется | Решение в этом плане |
|---|---|---|---|
| CITADEL-001 | **Critical** | non-`test-utils` debug validator, установлен `OUTBE_E2E_OCOMP_OWNER_FAILPOINT`, исполняется q-forming vote | удалить runtime env branch и сравнить production execution между env/debug/release |
| CITADEL-003 | **High** | нет успешного WWD tick в течение полного OFFERING window, затем Cycle catch-up использует текущий block timestamp | named fail-closed transition до READY/settlement |
| CITADEL-004 | **High** | OCOMP profile отсутствует, READY-день populated, synchronous Lysis возвращает persistent corruption/error | удалить no-OCOMP branch; genesis manifest обязан материализовать active OCOMP profile до Cycle block 1 |
| CITADEL-002 | **High** | новая in-process crate получает `StorageHandle` и использует публичный raw schema/mutator вместо canonical adapter | закрыть visibility и оставить только узкие production adapters/read projections |
| CITADEL-005 | **Medium** | одновременно существует минимум два READY-дня после halt/catch-up или OCOMP backlog | bounded oldest-first selection с доказанным active cap |
| CITADEL-006 | **Medium** | Desis превращает technical/fatal error в `Ok(false)`, а caller принимает его как business fallback; public direct legacy formation replay повторяет запись/event | typed business receipt, fatal propagation и закрытие direct replay seam |
| CITADEL-007 | **Medium** | U256 allocation получает extreme valid total; corrupt tag попадает в storage; ABI caller передаёт неизвестный status byte | точечная checked arithmetic и typed decode; без numeric-framework redesign |
| CITADEL-008 | **Low** | reviewer/release gate использует устаревший ADR или fixture test как production evidence | синхронизировать ADR и расширить существующую OCOMP evidence machinery Metadosis IDs |

## 5. Порядок реализации

```text
P0  CITADEL-001: убрать consensus env failpoint
 |
P1  CITADEL-002: authority + validated aggregate + atomic commit seam
 |
P2  CITADEL-003/004: exhaustive outer WWD reducer и terminal policies
 |
P3  CITADEL-005: explicit READY order + derived cap + cap+1 forfeiture
 |
P4  CITADEL-006/007: typed effects, receipts, replay intent, checked values/tags
 |
P5  G9: production fault matrix + independent persisted model
 |
P6  CITADEL-008/G10: fresh-genesis contract, ADR и exact-revision evidence
```

Все grilling decisions уже зафиксированы в разделе 11. P1 создаёт единственный
mutation seam до реализации новых outcomes, чтобы последующие шаги нельзя было
обойти через public adapter. P2/P3 используют уже выбранные value-routing
решения и не открывают их повторно.

Architecture deepening follow-up выполняется после уже применённого Citadel
hardening в следующем порядке; это порядок изменения interface/test surface,
а не новые protocol phases:

```text
A1  additive private fixture kernel + semantic test facade
 |
A2  CommitPermit для всех outer-WWD mutations + commit-owned emergency command
 |
A3  atomic workspace cutover всех raw test consumers и закрытие raw exports
 |
A4  compile-fail с default/test-utils + production-route behavioral gates
 |
A5  недостающие full fault sweeps (q-forming/day-limit/response-window)
 |
A6  удаление доказанно redundant nested checkpoints по одному
```

`A1` не закрывает raw surface сам по себе. `A2` должен существовать до финального
`A3`, чтобы external tests не были вынуждены сохранять raw emergency/mutation
path. `A6` запрещён до `A3` и соответствующего `A5`. Compatibility layer между
raw и semantic test interfaces не создаётся.

### 5.1. Обязательная целевая форма

```text
Cycle / EVM finality / fork install / verified vote
                         |
                         v
           purpose-bound MetadosisMutationLease
                         |
                         v
              Metadosis command interface
                         |
              module-owned/proven checkpoint
                         |
       ValidatedWwdAggregate::load_and_validate
                         |
 exhaustive reduce(state, command) -> TransitionPlan
                         |
       typed local effects -> mandatory receipts
                         |
      commit_transition(record + indexes + events
                        + replay receipt)
```

Обязательные элементы этой формы:

1. Внешние commands ограничены фактическими production causes:
   `CreateDay`, `AdvanceDue`, `ProcessReady`, `RecordCertifiedFinality`,
   `InstallForkProfile`, `RunOcompLifecycle`, `SubmitVerifiedResultVote`.
   `EmergencyFail`, `MissedOffering` и `CapacityForfeiture` остаются private
   reducer events.
2. `ValidatedWwdAggregate::load_and_validate` до effects проверяет record,
   typed status/day type, `active XOR closed`, OCOMP membership и локальные
   cross-field invariants.
3. Outer WWD reducer исчерпывающе определяет `Create`, `Advance`,
   `ProcessReady`, `EnterOcompPending`, `Complete`, `EmergencyFail`,
   `MissedOffering` и `CapacityForfeiture`. Недопустимая пара `state/event`
   отклоняется до effects; named business outcomes не проходят через
   `EmergencyFail`.
4. Один private `commit_transition` владеет purpose-bound command frame,
   checkpoint и pre/post aggregate validation. Единственный private
   transition-scoped owner helper внутри commit module создаёт non-forgeable
   `CommitPermit`; permit живёт одну outer transition и требуется каждым
   production raw mutator записи WWD/status/rate/day-limit/active/closed и их
   canonical events. Отдельно mint-ящие permit helpers и передача permit в
   reducer/runtime/OCOMP запрещены. Существующие OCOMP index/owner-receipt
   writers остаются у protocol-specific OCOMP owners без permit и без второго
   replay layer.
5. Каждый эффект возвращает non-ignorable typed receipt. В частности,
   scheduling возвращает `ScheduledWwdReceipt`, а Promis/Tribute/Desis/Lysis/
   terminal effects имеют exhaustive outcomes вместо `bool` и sentinel value.
   Production adapter после exhaustive match может вернуть `()`/empty ABI;
   менять публичный ABI только ради typed internal outcome запрещено.
6. Replay identity является command-specific:
   - новые outer-WWD/Cycle receipts используют
     `(trigger_id, scheduled_slot, WWD, command_kind, canonical_params_hash)`;
   - `RecordCertifiedFinality`, genesis/fork install, OCOMP lifecycle и
     verified vote сохраняют существующие certificate/install/`JobId`/
     attempt/vote identities;
   - второй Metadosis replay layer поверх OCOMP identities не создаётся.
   Тот же intent возвращает записанный result без effects; тот же key с другим
   intent отклоняется. Для outer-WWD receipt задаётся retention либо terminal
   tombstone не короче поддерживаемого retry/reorg horizon; после допустимого
   pruning stale command отклоняется authority/cursor guard до effects.
7. Lease покрывает storage, event log и CE work одним доказанным atomicity
   domain только там, где такой domain реально предоставляет executor. До
   реализации каждый Tribute terminal effect классифицируется как
   `journaled-local`, `transactionally coupled` либо изменяющий CE work. Lease
   не создаёт atomicity сам: он unforgeably связывает команду с существующим
   `ExecutionScope`/checkpoint. Если effect меняет CE work, Cycle-issued
   command обязан получить тот же доказанный CE checkpoint/restore contract,
   что OCOMP activation; если retirement лишь пишет journaled consensus
   request, это фиксируется явно и новый CE checkpoint не добавляется. Nested
   checkpoint не сохраняется из-за количества writes или отдельного domain
   owner: требуется локально поглощённая ошибка с продолжением либо
   самостоятельный production entrypoint без внешнего rollback owner.
8. Effect adapters внутри одного transition синхронны, journal-aware и не
   имеют callback/reentrancy route обратно в Metadosis. Lease отклоняет nested
   Metadosis mutation до чтения aggregate. Если caller inventory обнаружит
   наблюдение промежуточного state между effects, последовательность заменяется
   structurally staged commit; один только rollback после `Err` G2/G4 не
   закрывает.

Это не универсальный command/capability framework: все перечисленные типы
Metadosis-specific, а изменения primitives/EVM ограничены их выдачей и
проверкой.

### 5.2. Provenance узкого mutation lease

Точная реализация типов может отличаться, но closure обязан заполнить эту
матрицу фактическими production symbols. Пустая или «trusted caller» ячейка
означает незакрытый G1.

| Command class | Кто выдаёт authority | Проверенная provenance | Scope/срок жизни | Linearization point |
|---|---|---|---|---|
| Cycle WWD lifecycle | Cycle system-transaction dispatcher | registry trigger id, scheduled slot, cursor и WWD intent | один текущий `ExecutionScope`; single-use до command return | outer receipt + WWD/index/event commit, согласованный с Cycle cursor |
| Certified finality | EVM/finality adapter | verified certificate binding к chain/block/request | текущий block checkpoint; existing OCOMP replay identity | finality record/index commit |
| Genesis/fork profile | genesis loader и существующий OCOMP install adapter | canonical manifest bytes, install hash, chain id, genesis hash | genesis activation либо существующий install scope; не внешний `StorageHandle` | persisted active-profile commit |
| OCOMP lifecycle / verified vote | EVM OCOMP adapter | `JobId`/attempt/vote identity и verified result binding | текущий OCOMP checkpoint; existing retry/expiry rules | существующий typed owner receipt и q-forming atomic apply |

Эта таблица не создаёт новую authority hierarchy и не меняет OCOMP identities:
она только доказывает, где именно уже проверенная provenance превращается в
неподделываемый Metadosis-specific lease.

## 6. Critical

### C-1. Удалить process environment из q-forming activation

**Проблема и условие.**
`apply_certified_result` выполняет owner mutations, после чего
`inject_test_receipt_fault` в debug/no-`test-utils` build читает
`OUTBE_E2E_OCOMP_OWNER_FAILPOINT`
(`crates/core/metadosis/src/ocomp/activation.rs:378-446`). Одинаковый block
может commit на одном debug validator и reject на другом.

**Изменение.**

1. Удалить `debug_assertions` branch и любое чтение process environment.
2. Оставить только:
   - явный test fixture injector при `test`/`test-utils`;
   - строгий no-op во всех node artifacts без `test-utils`.
3. Существующие owner-failure tests перевести на явный
   `ActivationReceiptFault`; не переносить env failpoint в другой слой.

**Минимальные файлы.**

- `crates/core/metadosis/src/ocomp/activation.rs`;
- `crates/core/metadosis/src/ocomp/test_support.rs`;
- `crates/blockchain/evm/tests/ocomp_request_lifecycle.rs`;
- `crates/testing/e2e-harness/src/metadosis_p0.rs`.

**Closure tests.**

- строка env не присутствует в node artifact без `test-utils`;
- один и тот же maximum-shape q-forming block под пустым и произвольным env даёт
  одинаковые EVM receipt, state root, CE root и import verdict;
- debug и release execution совпадают;
- все явные owner receipt faults по-прежнему полностью откатывают vote slot,
  quorum и четыре owner effects.

**Закрыто, когда.**
Между owner apply и receipt verification нет ни одного runtime/process input.

**Не делать.**
Не менять OCOMP activation algorithm, quorum, schemas и admission debug-ноды:
для данного finding это не требуется.

## 7. High

### H-1. Fail-closed для полностью пропущенного OFFERING

**Проблема и условие.**
`update_wwd_status` напрямую пишет status, выведенный из timestamp
(`state.rs:109-140`), тогда как Tribute unseal/seal выполняются только на точных
edges (`runtime.rs:381-423`). Дефект достижим, если не было успешного tick в
течение всего OFFERING window, а первый post-gap block уже находится после
`offering_end`.

**Изменение.**

1. Разделить pure decision и effects:
   `plan_wwd_advance(current, block_time) -> WwdTransitionPlan`.
2. Зафиксировать полную таблицу
   `(current_state, canonical_time_region) -> ordered edges | named outcome |
   rejection` для каждого persisted state:
   - обычный one-edge advance возвращает точный edge и обязательные effects;
   - если OFFERING ранее был успешно открыт, gap через `offering_end` сначала
     выполняет ровно один `CloseOffering`/seal effect, затем каждый следующий
     edge и его обязательные effects в каноническом порядке;
   - `WAITING` после process time сначала становится READY и участвует в
     обычном oldest-first admission/processing; settlement не прячется внутри
     произвольного timestamp update;
   - backward time и любая неописанная пара отклоняются до effects.
3. Только для `current < OFFERING && block_time >= offering_end`, то есть когда
   OFFERING никогда не был открыт, вернуть named
   `MissedOffering` plan:
   - не вызывать Desis или Lysis;
   - не переводить день в READY;
   - exact сформированный limit один раз передать в Promis carry-over;
   - потребовать sealed и пустой Tribute day. Это следует из достижимой
     history: без успешного OFFERING edge день ни разу не был unsealed и не мог
     принять Tribute;
   - запросить retirement пустой partition; populated partition в этой ветке
     является `FatalInvariant` и полностью откатывается;
   - завершить день как `FAILED` через отдельный private
     `MissedOffering` reducer event, не через широкий `EmergencyFail`;
   - вернуть typed receipt с reason и value-routing result.
4. Применять весь plan только через `MetadosisMutationLease`, связанный с
   executor-owned checkpoint для storage, events и CE work. Любой `Err`
   оставляет полный pre-command state.
5. Удалить direct timestamp-to-arbitrary-status write как публичную команду.
6. Для не пропущенных edges сохранить текущую business semantics и длительности.

**Почему не replay всех edges.**
После полного пропуска OFFERING пользователи не могли своевременно отправлять
offers. Поздний unseal/auction создаёт экономическое событие задним числом и
возвращает phantom-clearing risk. Это противоречит уже выбранному fail-closed
правилу.

**Минимальные файлы.**

- `crates/core/metadosis/src/{state,runtime}.rs`;
- `crates/core/metadosis/src/tests/{state,lifecycle}.rs`;
- `crates/system/cycle/src/tests.rs`;
- ADR-C-MET-001 — только после принятого behavior.

**Closure tests.**

- T-1/T/T+1 каждой обычной границы;
- bootstrap с равными `forming_end`/`lookback_end`;
- post-gap до OFFERING, внутри OFFERING и после `offering_end`;
- полная state/time-region table покрывает `OFFERING -> WAITING/READY`,
  `WAITING -> post-process`, backward time и все multi-edge комбинации;
- missed case не вызывает Desis/Lysis и не достигает READY;
- carry-over увеличивается ровно на exact limit;
- empty Tribute partition получает retirement outcome; искусственно
  populated partition в этой ветке даёт fatal rollback без burn/retirement;
- повторный dispatch/replay не повторяет carry-over и terminal event;
- fault до/после Promis, Tribute, status и event восстанавливает полный
  pre-state;
- direct test и real Cycle dispatcher наблюдают одинаковый outcome.

**Закрыто, когда.**
Ни одна accepted history не содержит READY без обязательного OFFERING effect
или отдельного `MissedOffering` typed outcome, атомарно committed через `Ok` и
подтверждённого durable receipt/state/event; ни один named business outcome не
проходит через fatal/emergency command.

**Не делать.**
Не менять Cycle cursor, WWD durations, allocation и OCOMP request FSM.

### H-2. Удалить legacy/no-OCOMP execution

**Проблема и условие.**
Сейчас отсутствие profile является допустимой runtime-конфигурацией:
`load_ocomp_fork_install` возвращает `Ok(None)`, а `start_metadosis` при
`ocomp_profile == None` вызывает synchronous `process_metadosis`. Для
populated positive-gratis дня эта ветка вызывает Lysis; при ошибке пишет
`FAILED`/event и возвращает `Err`, который Cycle затем откатывает
(`runtime.rs:571-615`).

Для выбранного fresh-devnet contract эта вторая execution semantics не нужна:
OCOMP install является частью genesis authority, а profile уже active до
первого Cycle command в block 1.

**Изменение.**

1. Fresh-devnet startup требует `ocompForkInstallV1` в genesis config:
   - canonical bytes декодируются и полностью валидируются;
   - install hash, chain id и genesis hash обязаны совпасть;
   - выбранный profile материализуется как genesis-active до начала block 1;
     startup predicate не переводит timestamp первого OFFERING в высоту;
   - отсутствие/invalid/late install останавливает node до block execution.
2. `start_metadosis` больше не exhaustive-match `Option<profile>`:
   - отсутствие persisted active profile на OFFERING/READY path =
     `FatalInvariant` с полным rollback;
   - populated positive-gratis READY всегда создаёт OCOMP pre-admission и
     request;
   - synchronous Lysis никогда не вызывается из Metadosis.
3. Не удалять законные local terminal outcomes. Текущий `process_metadosis`
   разделить/переименовать в private `process_local_terminal_outcome`, который
   принимает только закрытый reducer outcome:
   - zero day limit;
   - известный enum-вариант `WwdDayType::Unknown` с закреплённым discriminant;
   - empty Tribute day;
   - zero gratis allocation.
   Этот helper не принимает `ParentBodySource`, не импортирует Lysis и
   структурно не может обработать populated positive-gratis day.
4. Удалить legacy synchronous Lysis branch, его error/event path, direct
   fixtures и no-profile business-success tests.
5. Genesis/profile verification выполняется до WWD effects; reducer повторно
   проверяет active profile перед OFFERING/READY как defense against corrupted
   persisted state.
6. ADR-C-MET-001 и ADR-C-LYS-001 фиксируют: Metadosis больше не является
   synchronous Lysis caller; Lysis доступна только через verified OCOMP result
   flow.

**Минимальные файлы.**

- `crates/core/metadosis/src/runtime.rs`;
- `crates/core/metadosis/src/{ocomp,tests}/**` только для profile guard и
  production-interface tests;
- `crates/blockchain/node/src/ocomp/fork.rs`;
- точный fresh-devnet genesis/OCOMP install artifact;
- `crates/system/cycle/src/tests.rs`;
- ADR-C-LYS-001 и ADR-C-MET-001.

**Closure tests.**

- fresh-devnet startup отклоняет missing, malformed, wrong-hash, wrong-chain,
  wrong-genesis и любой runtime/non-genesis activation;
- valid genesis install persisted-active до первого Cycle command block 1;
- corrupt/absent persisted profile на OFFERING и READY даёт fatal full rollback
  без `FAILED` state/event;
- populated positive-gratis READY создаёт OCOMP request и никогда не вызывает
  synchronous Lysis;
- zero-limit/`WwdDayType::Unknown`/empty/zero-gratis local outcomes сохраняют
  текущую exact routing semantics, но проходят через закрытые reducer variants;
- compile/caller inventory не содержит Metadosis -> `outbe_lysis::runtime::lysis`;
- direct и Cycle/EVM tests не поддерживают отдельную no-profile semantics.

**Закрыто, когда.**
Для fresh devnet существует ровно один populated positive-gratis execution
path: verified OCOMP. Отсутствие profile невозможно как валидная
конфигурация и не переключает бизнес-логику во время исполнения.

**Не делать.**
Не менять Lysis algorithm, OCOMP quorum/wire protocol и не проектировать
совместимость с существующей no-OCOMP chain history.

### H-3. Закрыть публичный raw storage/mutator surface

**Проблема и условие.**
Это не calldata exploit и текущий bad production caller не найден. Но любая
новая workspace crate с `StorageHandle` сейчас может скомпилировать raw status,
record/index drift, caller-supplied finality или fork initialization.

**Изменение.**

1. Выполнить inventory фактических external callers.
2. Сделать `schema`, `state` и invariant-bearing OCOMP internals private либо
   `pub(crate)`.
3. Оставить generated storage structs публичными только внутри private module,
   если этого требует macro.
4. Добавить Metadosis-specific `MetadosisMutationLease`:
   - private constructor доступен только точным EVM/executor provider routes;
   - lease связан с chain/execution scope и активным checkpoint;
   - lease нельзя `Clone`, сериализовать, получить из `StorageHandle` или
     сконструировать в другой crate;
   - каждая capability purpose-bound: Cycle lifecycle, certified finality,
     fork profile и verified vote не взаимозаменяемы.
5. Наружу экспортировать только:
   - typed read-only WWD/OCOMP projections;
   - authority-bound Cycle/finality/fork/result-vote entrypoints;
   - immutable config/proof-layout values, которые нужны node/OCM proof code;
   - ABI dispatch.
6. `BlockRuntimeContext::new`, `StorageHandle` и caller-supplied
   hash/root/height сами по себе не дают Metadosis authority. Finality command
   принимает verified certificate binding от authority owner.
7. `mark_wwd_failed` и все forfeiture paths оставить внутренними reducer
   outcomes.
8. Разделить test support на два слоя:
   - private raw fixture kernel внутри `outbe-metadosis` выполняет только
     precondition setup и intentional corruption;
   - feature-gated public `test_support` экспортирует несколько узких opaque
     scenario types, typed observations, production-command execution и
     закрытый набор явно названных `corrupt_*` operations.
   Public scenarios не содержат `StorageHandle`, provider, execution scope,
   `MetadosisContract`, schema entries или `CommitPermit`; generic raw
   key/slot/value и arbitrary callback отсутствуют.
9. External tests перевести атомарно на semantic facade и те же
   authority-bound production entrypoints. Если тест доказывает command `X`,
   action under test обязан вызвать production command/SystemTx/precompile/EVM
   dispatch `X`; raw seed допустим только как declared predecessor state.
10. Production-relevant config/types/limits/layout values экспортировать
    узкими canonical root paths вне `test_support`; не открывать ради них
    целые внутренние модули.
11. E2E допускает typed pre-launch genesis/chain-spec builder только как
    замену существующего raw setup. После запуска любые mutations/evidence идут
    через node execution, SystemTx, RPC/public ABI, receipts и artifacts;
    новый e2e evidence contract или новые scenarios не добавляются.
12. Не вводить универсальный authority framework: primitives/EVM change
   ограничивается выпуском этого конкретного lease.

**Минимальные call sites для адаптации.**

- Cycle lifecycle/handler/triggers;
- EVM begin-block and result-vote dispatch;
- node OCOMP proof-layout import;
- TributeFactory typed «offering day» query, если raw read подтверждён;
- Metadosis/EVM/Cycle/e2e tests.

**Closure tests.**

- compile-fail external crate не может:
  - импортировать raw schema;
  - создать `MetadosisContract`;
  - писать storage fields;
  - вызвать state/index helpers;
  - вызвать raw finality/profile/fork initializer;
- тот же compile-fail contract реально запускается с `--features test-utils` и
  подтверждает, что `schema`, `runtime`, raw `ocomp`, raw mutators и fixture
  storage fields остаются недоступны;
- compile-fail external crate не может получить или подделать
  `MetadosisMutationLease`;
- purpose mismatch (`Cycle` lease для finality/fork/vote) не компилируется либо
  детерминированно отклоняется до state read/effect;
- все реальные production mutations проходят через перечисленный adapter list;
- request/finality/open/vote/activation/replay tests сохраняют behavior;
- все существующие external raw `MetadosisContract` consumers переведены на
  semantic scenarios/typed projections; успешные transitions не моделируются
  raw writes;
- e2e pre-launch setup использует typed genesis builder, а post-launch direct
  storage mutation отсутствует;
- record/status/active/closed/OCOMP index equivalence проверяется после каждой
  canonical command;
- fresh-devnet storage layout содержит только явно перечисленные новые
  Metadosis receipt/invariant fields и закреплён exact slot/layout assertions;
  случайный сдвиг соседних schemas запрещён.

**Закрыто, когда.**
Invariant-breaking raw mutation и forgeable provenance больше нельзя
скомпилировать из другой crate при default и `test-utils`; public semantic
facade не раскрывает raw capabilities, а action under test и production
используют один authority/checkpoint seam.

**Не делать.**
Не менять storage macros, Solidity ABI, OCOMP FSM и schemas соседних crates.

## 8. Medium

### M-1. Явный READY order и доказанная граница работы

**Проблема и условие.**
При двух и более READY-днях `start_metadosis` выбирает первый элемент
swap-remove set (`runtime.rs:133-153`). Это достижимо после halt/catch-up либо
OCOMP backlog. Все ноды детерминированы относительно одинакового storage, но
экономический порядок случаен.

**Изменение.**

1. Зафиксировать protocol key:
   `(scheduled_process_time, worldwide_day)`, oldest-first.
2. Создать `ValidatedWwdAggregate::load_and_validate`, который строит один
   внутренний typed active snapshot, проверяет membership/index invariants,
   `len <= MAX_ACTIVE_WWDS` и сортирует по protocol key.
3. Использовать один snapshot implementation и для phase advancement, и для
   READY selection.
4. Сохранить один settlement за tick.
5. Вывести две независимые границы:
   - `MAX_PIPELINE_WWDS` из production creation cadence, длительности WWD
     phases и максимального catch-up rate;
   - `MAX_RETAINED_WWDS` из поддерживаемого OCOMP retained work.
   Итоговый `MAX_ACTIVE_WWDS = MAX_PIPELINE_WWDS + MAX_RETAINED_WWDS`; числа
   не угадывать. `MAX_ACTIVE_WWDS` ограничивает bounded scan и проверяется на
   каждом active insertion, включая `CreateDay`; `len > MAX_ACTIVE_WWDS` при
   load означает `FatalInvariant`. Если bound нельзя вывести из production
   cadence/config, это scope conflict, а не разрешение выбрать константу.
6. При реально достижимом `cap+1` выполнять named
   `CapacityForfeiture`, а не `Err`/halt:
   - этот cap — отдельный `MAX_RETAINED_WWDS` admission limit, не
     `MAX_ACTIVE_WWDS`;
   - capacity guard проверяется на `WAITING -> READY` до записи нового status;
   - если `retained_count == MAX_RETAINED_WWDS`, victim — сам новый admission
     candidate, то есть новейший WWD относительно уже retained очереди;
   - `retained_count > MAX_RETAINED_WWDS` означает corrupt state и
     `FatalInvariant`, а не новый business outcome;
   - существующие READY/OFFCHAIN_PENDING jobs не вытесняются и не меняются;
   - OCOMP intent/request/index для victim не создаются;
   - full unconsumed `metadosis_limit_amount` маршрутизируется один раз в
     Promis carry-over;
   - Tribute обрабатывается одним constant-size aggregate terminal effect,
     после которого count/nominal равны нулю и partition получает retirement
     request;
   - WWD становится `FAILED`, удаляется из active, добавляется в closed;
   - `CapacityForfeitureReceipt` связывает victim, cap evidence, exact limit,
     Promis receipt, Tribute forfeiture receipt и terminal outcome;
   - fault в любой точке полностью откатывает все перечисленные эффекты.
7. Добавить узкий `Tribute::forfeit_sealed_partition` по существующей
   constant-size модели `retire_certified_partition`:
   - аутентифицировать sealed collection root, source generation и exact
     aggregate count/nominal;
   - bulk-обновлением обнулить DayTotals/total supply, без обхода отдельных
     Tribute records;
   - запросить CE partition retirement и продвинуть generation;
   - сохранить канонический `TributePartitionRetired(uint32)` event для
     существующей CE/projection deletion; cap/value evidence записать в typed
     receipt и Metadosis terminal event, не вводя второй generic projection
     protocol;
   - вернуть `TributeForfeitureReceipt { wwd, sealed_root, forfeited_count,
     forfeited_nominal, retired_generation, retirement_outcome }`.
   Physical body retention/release остаётся node-owned, как в существующем
   certified retirement, и не входит в consensus loop.
8. Exact value semantics:
   - только `victim.metadosis_limit_amount` кредитуется в Promis;
   - Tribute nominal/issuance values не конвертируются в Promis, Nod или Intex;
     Tribute generation логически forfeited и retired;
   - Desis, Lysis и OCOMP request для victim не вызываются;
   - повторный intent возвращает тот же receipt без новых credits/events.
9. Ordered effect sequence фиксирован:
   `validate aggregate/cap -> Tribute forfeiture -> Promis carry-over ->
   commit WWD status/index/event/replay receipt`. Все четыре шага находятся в
   одном lease/checkpoint domain; ошибка на любом шаге откатывает предыдущие.
10. Если один tick видит несколько due admission candidates, reducer берёт один
   по `(scheduled_process_time, worldwide_day)`; остальные остаются в
   `WAITING`. Progress claim действует только при продолжающихся ticks и
   bounded OCOMP expiry/retry policy: отсутствие внешней finality не считается
   scheduler starvation. Для этих условий выводится конечная верхняя граница
   ожидания из cap и tick cadence. Это сохраняет один potentially heavy
   terminal effect за tick.
11. Измерить cap-case на Linux. Persisted due-index рассматривать только если
   доказанный cap-case не помещается в budget.

**Closure tests.**

- insert/remove/requeue histories с двумя и более READY всегда выбирают
  oldest-first;
- property model на `BTreeSet<(scheduled_time, wwd)>`;
- cap-1/cap/cap+1;
- cap+1 выбирает именно новый READY admission candidate; более старые retained
  WWD и их OCOMP indexes byte-for-byte не меняются;
- victim не получает OCOMP intent/request и фиксирует один
  `CapacityForfeitureReceipt`; replay не повторяет Promis/Tribute/terminal
  effects;
- Tribute forfeiture работает на aggregate с `0`, `1` и maximum-shape count,
  не вызывает per-record read/delete/event loop и сохраняет exact
  count/nominal в receipt;
- CE/projection по-прежнему получает ровно один canonical
  `TributePartitionRetired(uint32)` event; дополнительный generic retirement
  event отсутствует;
- два и более due candidates обрабатываются по одному на tick; при
  продолжающихся ticks и bounded OCOMP expiry/retry каждый non-forfeited
  candidate укладывается в выведенную max-wait bound;
- fault before/between/after Tribute, Promis, status, indexes, event и replay
  receipt восстанавливает semantic pre-state;
- callback inventory и nested-call test доказывают, что промежуточный aggregate
  нельзя наблюдать или повторно мутировать через Metadosis;
- continuous arrivals при указанных progress assumptions не нарушают
  max-wait bound; отсутствие finality классифицируется отдельно;
- reorg/replay дают одинаковые order, events и state root;
- worst-case работа имеет не менее согласованного headroom в block budget.

**Закрыто, когда.**
Order является protocol rule, active population имеет доказанный предел,
cap+1 имеет полностью определённый bounded value outcome, а cap-case измерен.
Простая константа без derivation finding не закрывает.

**Не делать.**
Не добавлять due-index, batch settlement и новый fork до отрицательного
результата bounded-sort measurement.

### M-2. Typed Desis outcome и single-owner day-limit formation

**Проблема и условие.**
Реальный дефект — не внешний `U256::ZERO`, а то, что Desis превращает любую
ошибку, включая fatal/corruption, в `Ok(false)`, после чего Metadosis может
commit business fallback. Сейчас единственный явный pre-call rejection —
`supply_promis > u128::MAX`; общий `best_effort` также проглатывает
`InvalidWorldwideDay`, `InvalidStageTransition`, timestamp overflow и любые
storage/index/event errors. Публичный direct legacy formation seam позволяет
обойти Cycle-owned exactly-once marker и повторно перезаписать amount/event.

**Изменение.**

1. Заменить `Result<bool>` на:
   `Result<AuctionBriefReceipt::{Accepted, RejectedToCarryOver{reason}}>`;
2. единственный допустимый `RejectedToCarryOver` —
   `SupplyExceedsAuctionDomain { supply, max: u128::MAX }`; он достижим на
   валидном `U256`, не создаёт Desis state/index и возвращает full rejected
   supply в receipt;
3. `InvalidWorldwideDay`, `InvalidStageTransition`, timestamp/anchor overflow,
   storage/index/event errors, `Fatal` и `BodyReadCorruption` распространять
   как `Err`;
4. удалить общий `best_effort` catch. Техническая ошибка не испускает
   committed `AuctionDispatchFailed` и не превращается в business receipt;
5. для committed capacity rejection вернуть/испустить typed
   `AuctionBriefRejectedToCarryOver` с `worldwide_day`, exact `supply`,
   `max_accepted` и stable reason code; произвольная debug-строка не входит в
   consensus outcome;
6. Metadosis обязан exhaustive-match receipt и при rejection кредитовать
   Promis ровно на `receipt.supply`;
7. внутри emission sink использовать typed `DayLimitFormationReceipt::Formed`;
8. после H-3 убрать direct legacy formation из поддерживаемого public mutation
   surface;
9. `daily_settled[prev_day]` остаётся Cycle schedule marker, а Metadosis
   durable replay receipt является owner semantic result. Оба обновляются в
   одном lease/checkpoint domain и проверяются на согласованность;
10. повторный dispatch того же Cycle slot возвращает тот же recorded semantic
   result и не повторяет writes/events;
11. внешний terminal-sink adapter после успешного typed receipt возвращает
   `U256::ZERO` как documented unused amount.

Поскольку изменения применяются только к fresh devnet, новый Metadosis replay
receipt создаётся genesis-only. Backfill существующих formed days и
pre/post-upgrade mixed history не проектируются.

**Closure tests.**

- `u128::MAX` принимается, `u128::MAX + 1` возвращает
  `SupplyExceedsAuctionDomain` и exact full carry-over;
- capacity rejection не создаёт auction config/stage/schedule index и
  испускает один typed committed rejection event;
- technical/fatal errors полностью откатывают Cycle command;
- invalid day/stage, anchor overflow и fault каждого Desis write/index/event не
  превращаются в rejection receipt и не оставляют `AuctionDispatchFailed`;
- GREEN accepted/rejected и RED accepted сохраняют текущую economic routing;
- repeated Cycle slot не входит в formation и не повторяет writes/events;
- compile-fail test доказывает отсутствие external direct formation seam;
- direct и Cycle outcomes совпадают.

**Закрыто, когда.**
Ни одно ignored `bool` не выбирает business outcome, technical error нельзя
случайно commit как carry-over fallback, а semantic exactly-once имеет одного
явного owner — Metadosis replay receipt.

**Не делать.**
Не менять EmissionLimit callback contract, Desis auction FSM и strict OCOMP
budget receipt.

### M-3. Checked allocation arithmetic и typed stored tags

**Проблема и условие.**

- unknown ABI status достижим любым caller уже сейчас;
- Tribute nominal total может быть больше `U256::MAX / 32`, поэтому обычное
  `total * 32 / 100` не имеет доказанного domain bound;
- corrupt raw status/day type структурно возможны до H-3 и при damaged state.
- unchecked `u64` window arithmetic является обязательной правкой только если
  caller inventory подтвердит, что fresh-devnet accepted input может превысить
  upstream timestamp/config bound; далёкий календарный `u64::MAX` сам по себе
  не создаёт отдельный work item.

**Изменение.**

1. Добавить closed `WwdStatus`/`WwdDayType` с фиксированными discriminants и
   `TryFrom<u8>`.
2. Сохранить physical storage fields как `u8`; layout не менять.
3. Byte, не декодируемый closed enum, даёт `FatalInvariant` из storage и
   `Revert` из ABI. Известный discriminant `WwdDayType::Unknown` остаётся
   валидным business variant и не смешивается с corrupt byte.
4. Raw setters закрыть; writes принимают typed enum.
5. Для `floor(total * 32 / 100)` использовать проверенный full-precision mul-div
   либо mathematically equivalent quotient/remainder decomposition; не
   отклонять корректный `U256` только из-за overflow промежуточного `* 32`.
6. Все allocation additions/subtractions сделать checked; убрать saturating
   fallback, маскирующий invalid receipt.
7. Для window arithmetic документировать фактический upstream accepted bound.
   Если он закрывает overflow, добавить boundary assertion/test без отдельного
   numeric redesign; если не закрывает — checked operation и exact invalid
   input rejection становятся частью этого пункта.

**Closure tests.**

- все bytes `0..=255`: known round-trip, unknown storage fatal;
- unknown ABI status revert;
- `0`, `1`, `MAX-1`, `MAX` совпадают с независимой wide-integer моделью;
- timestamp/config boundary test подтверждает upstream bound либо exact
  checked rejection реально допустимого overflow input;
- conservation:
  - `allocation + remainder == day_limit`;
  - `remaining_gratis <= allocation`;
  - `used + remaining_gratis == allocation`;
- debug/release parity.

**Закрыто, когда.**
На Metadosis consensus path U256 allocation закрыта для всего accepted domain,
raw tag comparisons отсутствуют, а time arithmetic либо доказанно ограничена
upstream contract, либо checked на реально допустимой границе.

**Не делать.**
Не вводить общий numeric newtype framework и не менять Tribute bounds.

### M-4. Доказать sole command rollback ownership и удалить только redundant checkpoints

**Проблема и условие.**
`commit_transition` уже владеет purpose-bound storage/ordered-event journal, а
CE-capable commands добавляют command-level `CeWorkCheckpoint`. Внутри
day-limit formation, `CapacityForfeiture`, `MissedOffering`, local terminal,
result/q-forming vote и response-window close остаются nested checkpoints.
Проверенный control flow возвращает их реальные ошибки наружу; отдельное
domain ownership или количество writes не доказывают необходимость savepoint.

**Изменение.**

1. Выполнять этот пункт только после atomic raw-test cutover H-3: test-only
   direct calls больше не должны нуждаться в standalone atomicity внутренних
   функций.
2. Для каждого checkpoint доказать одно из двух условий сохранения:
   - ошибка поглощается локально, subflow откатывается, outer execution
     продолжает работу и коммитится;
   - функция является самостоятельным production mutation entrypoint без
     эквивалентного внешнего rollback owner.
3. Если ни одно условие не выполнено, checkpoint является кандидатом на
   удаление. OCOMP q-forming atomicity сохраняется как единая transaction, но
   не получает автоматического исключения: production vote command уже
   покрывает slot, quorum, owner effects, terminal state, events и CE.
4. До удаления провести successful-run mutation count `N`, затем inject fault
   для каждого `i in 0..N`; на каждом `Err` сравнить provider state, ordered
   events и CE work с entry snapshot, а exact retry — с clean run, включая
   typed receipts и owner effects.
5. Сначала закрыть недостающие full sweeps для q-forming, day-limit formation
   и response-window close. Существующий test только mutation index `0` не
   является достаточным deletion evidence.
6. Удалять ровно один nested checkpoint за change и повторять всю его fault
   matrix. Если эквивалентный snapshot code появляется снова либо continuation
   semantics меняется, checkpoint проходит deletion test и сохраняется.
7. Capability/order frames, включая Lysis activation frame, не считать
   rollback checkpoints и не удалять в рамках этого пункта.

**Implementation evidence (A6, three-review gate approved).**

- Единственный production owner rollback для provider state и ordered EVM
  events — `commit_transition` через purpose-bound
  `with_metadosis_mutation`; pre/post `ValidatedWwdAggregate` остаётся внутри
  того же command boundary.
- Единственный дополнительный rollback owner — private
  `commands::with_ce_checkpoint`, и только для команд, реально меняющих
  `ExecutionScope` CE work, потому что CE work не входит в provider journal.
- Удалены production nested savepoints day-limit formation,
  `CapacityForfeiture`, `MissedOffering`, всех local terminal branches,
  terminal request/budget, fork install, pre-admission initialize/seal,
  OCOMP enqueue/defer/request/finality/voting-open/expiry/conflict/completed,
  q-forming apply, response close и retained-record eviction. Удаление
  выполнялось по одному с full `0..N` mutation sweep до и после.
- Оставшиеся `with_checkpoint` внутри Metadosis находятся только в
  `#[cfg(test)]` fixture builders (`ocomp/schema.rs` и
  `ocomp/test_support.rs`); они не являются production entrypoints и не
  участвуют в доказательстве runtime atomicity.
- Lysis activation authority и прочие capability/order frames не являются
  rollback ownership и потому сохранены без изменения semantics.
- Полное отображение deletion families на production-route fault/retry tests
  закреплено требованием `M-4` в
  `outbe-plan/metadosis-evidence-ledger.yaml`; Linux process evidence остаётся
  pending и этим локальным closure не заявляется.

**Closure tests.**

- caller inventory/compile-fail подтверждает отсутствие production и
  test-utils bypass внешнего command seam;
- каждый удалённый checkpoint имеет полный per-mutation fault sweep;
- ordinary `Err` не оставляет provider state, ordered events или CE work;
- domain outcomes (`MissedOffering`, `CapacityForfeiture`, `NoQuorum` retry,
  `Conflict`, `Completed`) коммитятся только как `Ok` и подтверждаются typed
  internal outcome плюс durable typed state/receipt/event;
- public ABI остаётся byte-compatible: unit/empty output не заменяется новым
  return type;
- exact retry после каждого injected failure совпадает с clean execution.

**Закрыто, когда.**
Все оставшиеся nested checkpoints имеют доказанную local-continuation либо
standalone-entrypoint semantics; каждый удалённый checkpoint исчез без нового
rollback module или дублирующего snapshot code.

**Не делать.**
Не вводить общий checkpoint framework, не менять OCOMP quorum/wire/worker
semantics, public ABI, state layout или e2e evidence contract.

## 9. Low

### L-1. Синхронизировать ADR, ABI docs и evidence

**Изменение.**

1. После M-3 отразить в ABI docs уже реализованный typed revert для unknown
   `getWorldwideDaysByStatus` byte; отдельного второго code owner здесь нет.
2. Desis ABI/event contract различает committed
   `AuctionBriefRejectedToCarryOver` и technical `Err`; generic
   `AuctionDispatchFailed` больше не является committed error sink.
3. Не на этапе утверждения этого плана, а в соответствующем implementation
   change и при его evidence closure:
   - применить матрицу ADR и normative-документов из раздела 9.1;
   - normative decision text обновлять вместе с изменением контракта, а
     implementation/evidence status — только после прохождения указанного
     closure gate на той же revision;
   - не менять ADR status автоматически: `Proposed`/`Accepted` меняются только
     по принятому в репозитории governance-процессу.
4. Создать общепроектный versioned `VerificationLedger` как минимальное
   извлечение уже работающей evidence-механики:
   - generic core живёт в
     `crates/testing/e2e-harness/src/verification_ledger/{mod,schema,ledger,verify}.rs`;
   - `outbe-plan/verification-ledger.yaml` является только index и pin-ит
     version/path каждого domain pack;
   - существующий `outbe-plan/off-chain-poc-evidence-ledger.yaml` остаётся по
     прежнему пути как OCOMP pack, а
     `outbe-plan/metadosis-evidence-ledger.yaml` становится Metadosis pack;
   - `ocomp_evidence/**` сохраняет OCOMP-specific collectors, lane assembly и
     policy adapter, импортируя generic core;
   - generic schema/parser/verifier не кодирует `OCM-*`, PoC-only modes,
     фиксированные OCOMP retired rows или OCOMP requirement taxonomy;
   - domain pack владеет namespace, requirement taxonomy, task ownership,
     required lanes/profiles, substitutions, allowed deferrals, test catalog и
     closure policy;
   - общий verifier владеет duplicate-key rejection, source/toolchain
     identity, member digests, exact-revision checks, assertion statuses,
     discovery и fail-closed reference validation; generic policy engine лишь
     вычисляет правила, объявленные domain pack;
   - общепроектный index перечисляет versioned domain packs и отвергает
     неизвестный namespace, duplicate IDs, cross-pack references и mixed
     revision/profile evidence.
5. Подключить существующий
   `outbe-plan/off-chain-poc-evidence-ledger.yaml` как OCOMP domain pack без
   изменения его test IDs, task graph, deferrals, retired rows, CLI output или
   closure semantics. Старые OCOMP verifier fixtures обязаны проходить через
   generic core без изменения результата; существующий OCOMP policy adapter
   остаётся нормативным владельцем OCOMP-specific lane semantics.
6. Добавить отдельный Metadosis domain pack с requirement IDs для
   C-1/H-1..H-3/M-1..M-3 и полями: test target/name, production-interface
   class, substitutions, Linux lane, required/optional и exact
   revision/profile. Не добавлять placeholder rows для других модулей.
7. Зафиксировать fresh-devnet contract:
   - genesis/config содержат обязательный canonical OCOMP install, pin-ят его
     hash и genesis-active profile до block 1, а также создают пустые
     replay/receipt indexes;
   - новые Metadosis slots/layout pin-ятся assertions;
   - unknown ABI status документирован как revert с block 1;
   - migration, backfill, post-genesis fork selection и mixed old/new replay
     намеренно отсутствуют.

**Closure tests.**

- required test отсутствует, renamed, ignored или skipped -> closure verifier
  fail;
- fixture/direct/Cycle/EVM/process evidence не смешиваются;
- mixed revision/profile evidence reject;
- unknown/duplicate namespace, cross-pack requirement reference и domain-pack
  schema mismatch reject;
- неизменённый OCOMP bundle даёт тот же closure verdict до и после извлечения
  generic core;
- Metadosis closure не требует requirements или artifacts других модулей;
- release evidence содержит exact commit, artifact profile и фактически
  выполненную Linux lane;
- fresh devnet boot from genesis проходит Create/Advance/READY/OCOMP/terminal
  path, а manifest/layout mismatch fail-closed до запуска;
- ADR statements совпадают с текущим implementation/evidence status.

**Закрыто, когда.**
Reviewer может машинно отличить implemented claim от production-proven claim
на одной exact revision, а общий verifier одинаково проверяет OCOMP и
Metadosis packs без смешивания их normative semantics.

**Не делать.**
Не заполнять ledger для остальных модулей, не менять OCOMP closure contract и
не превращать узкую harness/local-control portability в production macOS
node/OCOMP support.

### 9.1. Какие ADR и normative-документы изменять

Эта матрица является частью scope, а не списком возможных улучшений. Строка
`обязательно изменить` означает, что выбранный Metadosis behavior невозможно
честно описать без синхронизации данного владельца контракта. Строка
`только сверить` не разрешает редизайн этого компонента: файл меняется только
если реализация действительно нарушила уже существующий общий контракт.

#### Обязательно изменить

| ADR | Почему он является владельцем решения | Точное ограниченное изменение |
|---|---|---|
| `ADR-C-MET-001` | Владеет WWD identity, FSM, окнами, active/closed indexes и processing limit | Зафиксировать exhaustive outer FSM, missed-OFFERING transition, READY order и batch=1, `cap+1` victim/forfeiture, typed terminal receipts, replay intent, purpose-bound mutation entry и отсутствие synchronous Lysis path |
| `ADR-C-LYS-001` | Владеет Metadosis→Lysis request/result contract | Удалить описание synchronous Metadosis caller; оставить только verified OCOMP result/apply path; явно отделить capacity forfeiture от Lysis и запретить создание Nod/Intex для forfeited partition |
| `ADR-S-OCM-004` | Владеет OCOMP activation/job FSM и protocol binding | Заменить stale branch/status claims точной revision; зафиксировать обязательный genesis-active profile до первого Cycle command block 1 и границу между outer WWD FSM и OCOMP job FSM, не меняя quorum/wire/worker semantics |
| `ADR-C-DES-001` | Владеет Desis auction command и его business outcome | Заменить generic best-effort `bool`/committed failure на typed receipt: только `SupplyExceedsAuctionDomain` является committed rejection; остальные ошибки дают полный rollback |
| `ADR-C-TRB-001` | Владеет authenticated Tribute partition lifecycle | Добавить только два выбранных terminal contracts: empty retirement для missed OFFERING и constant-size aggregate forfeiture для `cap+1`; не менять normal Lysis economics |
| `ADR-C-PRM-003` | Владеет unallocated-limit carry-over | Перечислить точные sources и once-only routing: missed OFFERING full limit, capacity forfeiture full limit и Desis oversize full rejected supply; связать их с durable receipt/replay key |
| `ADR-S-CYC-001` | Владеет deterministic trigger/cursor dispatch, но не WWD economics | Зафиксировать один canonical Metadosis command на tick, deterministic trigger intent и связь Cycle cursor с durable Metadosis receipt; не переносить WWD FSM в Cycle |
| `ADR-B-GEN-001` | Владеет genesis identity, schema activation и startup validation | Зафиксировать fresh-devnet-only contract: обязательный canonical OCOMP install/binding/hash и genesis-active profile до block 1; инициализировать только фактически добавленные appended receipt/replay/invariant slots; due-index остаётся запрещён без отрицательного bounded-sort measurement и отдельного scope-решения |
| `ADR-B-TST-001` | Уже владеет общепроектной production-verification/evidence architecture | Уточнить один versioned generic `VerificationLedger`, OCOMP и Metadosis domain packs, exact-revision Linux artifact и fail-closed cross-pack validation; не добавлять requirements других модулей |
| `ADR-B-OCD-011` | Нормативно разрешает retirement только после successful Lysis и запрещает retirement `FAILED`, что прямо противоречит выбранным terminal outcomes | Добавить ровно две typed retirement authorities: empty `MissedOffering` и sealed exact-aggregate `CapacityForfeiture`; для forfeiture нет Nod/Intex, сохраняется canonical `TributePartitionRetired(uint32)` projection event, а cap/value evidence живёт в typed receipt и Metadosis terminal event; generic CE deletion mechanics не менять |

#### Только сверить; планового изменения архитектуры нет

| ADR | Что именно сверить | Когда допускается правка |
|---|---|---|
| `ADR-B-EVM-005` | purpose-bound authority, module-owned checkpoint и typed lifecycle context соответствуют существующему stateful-module contract | Только если узкий Metadosis lease требует уточнения уже принятого общего правила; новый universal authority framework запрещён |
| `ADR-B-EVM-004` | новые appended slots, discriminants, selectors и events покрыты generated layout/ABI assertions | Только status/evidence или точечное перечисление артефакта; storage/ABI framework не менять |
| `ADR-B-RLS-001` | обязательный Linux artifact и provenance соответствуют уже определённой reproducible-build lane | Только если меняется общий supported artifact/profile contract; отдельный Metadosis release redesign запрещён |

#### Не-ADR документы, которые обязаны остаться согласованными

- `README.md`: Stateful Runtime Module Contract и Emission Model — только
  фактический Metadosis mutation/OCOMP/value-routing flow;
- `docs/flows/002-off-chain-poc-protocol-flow.md`: заменить stale
  `feat/ocomp-poc` implementation/evidence status точной revision и добавить
  fresh-genesis profile guard; сам flow уже описывает OCOMP path;
- `docs/flows/009-multichain-auction-day.md`: кроме missed OFFERING, READY order,
  Desis rejection и `cap+1`, заменить synchronous порядок на
  `Metadosis split + Desis brief -> OCOMP intent -> certified
  contributor/retirement apply`; creator payout ждёт certified contributor
  state;
- `docs/flows/index.md` и `docs/flows/e2e-inventory.md`: убрать stale
  synchronous/no-OCOMP status;
- `docs/adr/coverage.md`: отразить фактический набор ADR выше без объявления
  соседних модулей частью Metadosis implementation scope;
- `docs/adr/index.md`: согласовать status с заголовками ADR, устранив известные
  расхождения для `ADR-S-OCM-004` и `ADR-B-RLS-001`, но не повышать status без
  governance-решения;
- `contracts/precompiles/src/{IMetadosis,IDesis,ITribute}.sol` и соответствующие
  `contracts/precompiles/abi-export/*.json` для изменённых typed receipts,
  status revert и terminal events; `IPromisLimit` меняется только если
  действительно меняется его публичное событие;
- `outbe-plan/verification-ledger.yaml`,
  `outbe-plan/off-chain-poc-evidence-ledger.yaml` и
  `outbe-plan/metadosis-evidence-ledger.yaml`: index и два разрешённых domain
  packs без placeholder rows для других модулей.

#### Порядок синхронизации

1. До или в том же atomic change, где меняется behavior, обновляется normative
   decision text соответствующего ADR.
2. Формулировки `implemented`, `production-proven` и ссылки на evidence
   добавляются только после прохождения closure tests на exact revision.
3. ADR status не выводится из зелёных тестов автоматически; status в
   `docs/adr/index.md` обязан совпадать с самим ADR.
4. Если ADR из группы `только сверить` не нарушен, он остаётся без diff. Сам
   факт его чтения или упоминания не создаёт work item.
5. Любая найденная потребность изменить иной ADR сначала считается scope
   conflict и требует отдельного решения, а не молчаливого добавления в этот
   план.

## 10. Общий verification gate

Минимальные обязательные vertical slices:

1. **Authority/interface:** compile-fail external crate в default и
   `test-utils`, purpose mismatch, semantic test facade без raw capabilities и
   каждый реальный public selector/adapter.
2. **Reducer/state:** exhaustive state/event table, pure reducer/arithmetic и
   persisted aggregate invariants.
3. **Atomicity:** real Cycle/EVM command с существующими `fail_mutation_at` /
   `fail_after` hooks на каждой distinct storage/index/event/CE/effect границе;
   ordinary `Err` сравнивается с entry provider state, ordered events и CE
   work, exact retry — с clean run. Nested checkpoint удаляется только после
   такого sweep и только по одному.
4. **Independent models:** две связанные suites:
   - outer persisted WWD model посылает accepted/rejected commands через Cycle
     interface, shrink-ит failure и сохраняет seed;
   - существующая independent OCOMP attempt model, без переписывания reducer,
     проходит production request/finality/open/vote/expiry adapters и сверяет
     observable persisted indexes.
   Composition invariant связывает READY/OCOMP membership с terminal WWD
   outcome. Distribution labels покрывают terminal, illegal, duplicate,
   rollback, cap boundary, forfeiture и multi-record cases.
5. **Order/cap:** production slice отдельно доказывает active-scan cap на
   `CreateDay`, retained-admission cap на `WAITING -> READY`,
   `cap-1/cap/cap+1`, exact forfeiture routing и conditional max-wait bound.
6. **OCOMP activation:** EVM proposer/import/historical replay parity для
   q-forming activation и public `submitLysisResult` owner-fault matrix.
7. **Evidence:** общий `VerificationLedger` verifier с отдельными OCOMP и
   Metadosis domain packs на exact Linux artifact и fresh-devnet
   genesis/profile.

Обязательная lane запускается только из pinned image digest для `x86_64`
Ubuntu 24.04, на котором сгенерирован выбранный OCOMP devnet machine profile.
Альтернативный image допустим только с byte-identical
toolchain/profile/artifact proof, записанным в ledger; слово «идентичный» без
digest и proof не принимается. Evidence pin-ит exact revision, toolchain,
build profile, features, genesis и OCOMP profile;
consensus-critical cases проходят в debug и release. До появления такого
Linux artifact ни один пункт нельзя объявлять закрытым только по статической
проверке. Host launcher поддерживает macOS/Linux, но запускает один exact
Linux/amd64 image; current local Unix Docker socket, host networking,
same-absolute-path mounts, numeric host UID/GID и direct-without-sudo inner
Docker calls являются проверяемой частью launcher contract. Выбранный
macOS/Linux container runtime должен поддерживать host networking; для Docker
Desktop эта возможность должна быть явно включена. Receipt nonce,
image/container inspect и фактический inner argv входят в content-addressed
evidence. Native macOS результаты остаются только локальным optional signal и
не закрывают обязательный gate.

### 10.1. Gate-level closure

| Gate | Что должно стать структурно истинно | Обязательное доказательство |
|---|---|---|
| G1 | все production mutation входят через Metadosis command + purpose-bound lease; private raw fixture kernel не выходит через semantic test facade | compile-fail в default/test-utils + полный inventory production и test callers |
| G2 | persisted WWD загружается только как validated typed aggregate; `active XOR closed` и OCOMP membership проверены | corrupt-state table + invariant property |
| G3 | один exhaustive reducer определяет все WWD events, catch-up и forfeiture | state/event matrix + T-1/T/T+1/backward/multi-edge |
| G4 | lease связывает command с доказанным executor rollback domain; CE-changing и journal-only retirement различены; nested mutation невозможна; один `commit_transition`; nested checkpoint остаётся только для local continuation или standalone production entrypoint | effect/caller classification + callback inventory + full per-mutation fault/retry sweep до и после удаления каждого checkpoint |
| G5 | каждый effect имеет consumed typed receipt | exhaustive receipt matching + compile-time `must_use` |
| G6 | порядок, arithmetic, active-scan cap, retained-admission cap и `cap+1` outcome детерминированы и bounded | два cap properties + conditional max-wait + Linux worst-case measurement |
| G7 | record/status/active/closed/OCOMP/replay facts имеют одного owner | arbitrary-history invariant model |
| G8 | canonical intent делает retry/replay однозначными; terminal replay effect-free | same-intent/same-result и same-key/different-intent tests |
| G9 | production interface, outer WWD и OCOMP production-adapter models, fault matrix и proposer/import parity закрыты | composition invariant + required exact-revision evidence IDs |
| G10 | новый behavior зафиксирован как fresh-devnet genesis contract с OCOMP active до Cycle block 1 | genesis/config/ABI/ADR assertions; отсутствие runtime activation и migration path |

## 11. Решения grilling

Открытых вопросов не осталось.

### Зафиксированные решения

1. Stateful mutation требует узкий unforgeable Metadosis-specific lease.
2. Достижимый `cap+1` завершается deterministic forfeiture с exact value
   routing, а не fatal halt.
3. Изменения применяются только к fresh devnet: migration/backfill и
   hard-fork activation не требуются.
4. Missed OFFERING не может содержать законно выпущенный Tribute: partition не
   был unsealed. Exact outcome — full limit в Promis, empty partition
   retirement, WWD `FAILED`; противоречащий populated state даёт fatal rollback.
5. `unknown status: empty -> revert` разрешён как fresh-genesis ABI behavior.
6. При `cap+1` victim — новый WWD на admission `WAITING -> READY`, до OCOMP
   request. Его полный limit один раз уходит в Promis; sealed Tribute
   generation forfeited constant-size aggregate transition и retired; WWD
   становится `FAILED`. Существующая retained очередь не вытесняется.
7. Legacy/no-OCOMP mode удаляется. Fresh-devnet genesis обязан содержать
   валидный genesis-active OCOMP install до первого Cycle command block 1;
   populated positive-gratis WWD имеет только verified OCOMP execution path.
8. READY выбирается строго по
   `(scheduled_process_time, worldwide_day)`, oldest-first; один WWD за tick.
9. Единственный committed Desis rejection —
   `SupplyExceedsAuctionDomain`; full rejected supply уходит в Promis.
   Остальные Desis errors дают `Err` и полный rollback.
10. Linux на generated fresh-devnet OCOMP machine profile (`x86_64`,
    Ubuntu 24.04) и pinned image digest — обязательная closure/release lane.
    `e2e-harness` имеет один macOS/Linux host launcher; kernel peer identity
    читается через `SO_PEERCRED` на Linux и `getpeereid`/`LOCAL_PEERPID` на
    macOS без bypass; собственный effective UID читается через portable Unix
    `geteuid`, а не Linux-only `/proc`. Launcher принимает только local Unix Docker context,
    монтирует socket и host paths по тем же абсолютным путям, использует host
    network и запрещает inner `sudo`; macOS runtime должен предоставлять
    эквивалентный host networking (для Docker Desktop он включается явно). Сам
    evidence flow остаётся Linux-only и native macOS execution не является
    closure evidence.
11. Создаётся общепроектный versioned `VerificationLedger`. Расширение
    ограничено generic evidence schema/verifier и OCOMP/Metadosis domain
    packs; requirements остальных модулей не входят в этот план.

### Architecture deepening decisions (2026-07-31)

#### Candidate 01 — accepted: compiler-governed outer mutation

- `CommitPermit` обязателен для всех production outer-WWD mutations, включая
  commit-owned emergency command; raw test helper не является исключением.
- Permit создаётся только owner-функцией commit module, живёт одну transition и
  недоступен reducer/runtime/OCOMP/test facade.
- Raw mutators требуют permit; нарушение ownership должно отвергаться Rust
  compiler/privacy, а не `include_str!`/source-text search.
- Source-text ownership tests удаляются только после compile-fail и behavioral
  replacement gates.

#### Candidate 02 — accepted: private fixture kernel + semantic test facade

- Один feature-gated root `test_support` экспортирует несколько узких opaque
  scenarios, а не один универсальный mutable fixture.
- Private raw kernel владеет setup/corruption. Public facade не раскрывает
  provider/storage/schema/permit и предлагает только semantic preconditions,
  production actions, typed observations и closed named corruptions.
- Production types/config/limits/layout имеют canonical non-test exports;
  внутренние модули ради них не открываются.
- Cutover всех workspace consumers атомарный, без deprecated compatibility
  exports. Compile-fail обязан реально исполняться с `test-utils`.
- E2E pre-launch setup проходит typed genesis builder; post-launch mutation и
  evidence — только production node/SystemTx/RPC/public ABI.

#### Candidate 03 — accepted: characterize, then delete nested rollback

- Command seam состоит из purpose-bound provider/ordered-event journal и
  command-level CE checkpoint для CE-capable commands.
- Nested savepoint сохраняется только для locally consumed error с observable
  continuation либо standalone production mutation entrypoint без equivalent
  outer rollback owner.
- Q-forming остаётся одной atomic transaction, но не является автоматическим
  исключением: его nested checkpoint — deletion candidate после Candidate 02
  и полного fault sweep.
- Ordinary `Err` не оставляет committed provider state, ordered events или CE
  work. Expected domain disposition коммитится как `Ok` через typed internal
  outcome и durable typed state/receipt/event; public unit/empty ABI остаётся.
- Rollback-infrastructure failure является fatal execution abort. Checkpoints
  удаляются по одному, без нового framework.

#### Candidate 04 — closed without implementation

- `WwdStatus::try_from` и `WwdDayType::try_from` уже концентрируют общую tag
  validation.
- Typed point read, ABI read и aggregate guard сохраняют разные membership,
  missing-record, invariant и cost contracts.
- Новый общий projection/validation module не создаётся. К extraction можно
  вернуться только при доказанном behavioral drift либо появлении новой
  действительно одинаковой validation complexity.

### Порядок architecture follow-up

1. Добавить private fixture kernel и semantic facade без закрытия старого
   interface.
2. Закрепить `CommitPermit` и commit-owned emergency command структурно.
3. Атомарно мигрировать consumers и удалить raw exports; source-text tests
   сохранять до compiler/privacy replacements следующего шага.
4. Заменить source-text policing compiler/privacy и production-route gates,
   затем закрыть недостающие fault sweeps.
5. Удалить доказанно redundant checkpoints по одному.

Блокирующих design-вопросов нет. Exact перечень named corruption operations
выводится только из существующих тестов при migration и не создаёт новые
behavior scenarios.

## 12. Definition of done

- [ ] **G1:** другая crate не может получить raw mutation surface или
      сконструировать/подделать purpose-bound lease при default и
      `test-utils`; public semantic test facade не содержит raw capabilities;
      все production actions under test входят через Metadosis command seam с
      заполненной provenance-матрицей.
- [ ] **G2:** все persisted tags и cross-field/index invariants проходят через
      `ValidatedWwdAggregate`; corrupt state fail-closed.
- [ ] **G3:** один exhaustive reducer покрывает каждый state/event, catch-up,
      backward time, terminal absorption, missed OFFERING и capacity
      forfeiture; populated positive-gratis READY имеет только OCOMP path.
- [ ] **G4:** каждый terminal effect классифицирован по реальному rollback
      domain; lease связан с доказанным executor checkpoint, nested mutation
      невозможна; `commit_transition` владеет command frame/checkpoint/aggregate
      validation, а transition-scoped commit owner — outer record/event commit;
      ordinary `Err` восстанавливает provider state, ordered events и CE work,
      а каждый удалённый nested checkpoint прошёл полный fault/retry sweep.
- [ ] **G5:** outer reducer получает exhaustive typed receipts только от
      непосредственно вызываемых Oracle/Tribute/Desis/Promis/limit/schedule/
      terminal effects; Lysis/Nod/contributor применяются исключительно через
      существующие verified OCOMP owner receipts без schema redesign.
- [ ] **G6:** process environment не влияет на consensus; accepted allocation
      domain checked; READY order, active-scan cap, retained-admission cap,
      conditional max-wait и `cap+1` outcome детерминированы и bounded.
- [ ] **G7:** record/status/active/closed/OCOMP/replay equivalences имеют одного
      owner и сохраняются после каждой accepted/rejected history.
- [ ] **G8:** command-specific outer intent обеспечивает same-intent replay без
      повторных effects, collision rejection, tombstone/retention и explicit
      terminal retry; существующие OCOMP replay identities не заменены вторым
      Metadosis layer.
- [ ] **G9:** связанные outer-WWD и OCOMP production-adapter models, полный
      fault matrix, composition invariant, proposer/import/replay parity и case
      distribution проходят через production interface.
- [ ] **G10:** schema/ABI/ADR/genesis/profile описывают один fresh-devnet
      contract с genesis-active OCOMP profile до Cycle block 1;
      runtime activation, migration/backfill/post-genesis fork path отсутствуют
      намеренно и явно.
- [ ] CITADEL-001..008 имеют stated closure test и requirement ID на одной
      exact revision.
- [ ] Общий `VerificationLedger` проверяет отдельные OCOMP и Metadosis domain
      packs; OCOMP verdict не изменился, cross-pack/mixed-revision evidence
      fail-closed, requirements остальных модулей отсутствуют.
- [ ] Все десять ADR из раздела 9.1 синхронизированы с выбранным behavior и
      фактическим implementation/evidence status; три consistency-only ADR
      либо подтверждены без diff, либо изменены строго по указанному условию.
- [ ] `README.md`, PFS-002, PFS-009, flow index/inventory, ADR coverage/index и
      ABI/evidence docs не содержат прежний synchronous/no-OCOMP,
      best-effort-Desis или неопределённый forfeiture contract.
- [ ] Все closure tests проходят в обязательной `x86_64` Ubuntu 24.04 lane,
      совпадающей с fresh-devnet OCOMP machine profile, без ignored, skipped,
      retry-hidden или missing tests.
- [ ] Ни одно изменение не выходит за scope раздела 2.
- [ ] Candidate 04 не породил новый projection/validation module или ABI
      change; его no-change disposition сохранён в implementation review.
