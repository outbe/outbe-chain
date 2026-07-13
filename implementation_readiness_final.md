# Финальный Implementation Readiness Gate — CES v6.1

**Дата:** 2026-07-13  
**Scope:** текущие concept/decision/benchmark документы, roadmap и T01–T36.

## 1. Финальный verdict

<!-- markdownlint-disable MD013 -->

| Вопрос                                                                   | Verdict                                             |
| ------------------------------------------------------------------------ | --------------------------------------------------- |
| Можно ли начать реализацию сейчас?                                       | **GO**                                              |
| Можно ли одновременно передать весь T01–T36 DAG автономным исполнителям? | **CONDITIONAL_GO**                                  |
| Готова ли Stage 1 testnet activation?                                    | **NO_GO сейчас; штатные activation gates остаются** |
| Нужен ли ещё один broad audit?                                           | **NO**                                              |

<!-- markdownlint-enable MD013 -->

Roadmap достаточно зрелый, чтобы перейти от аудита к реализации. Большая часть
29 findings из `.reaudit/ledger.md` уже закрыта текущими правками. Оставшиеся
проблемы не требуют ещё одного бесконечного review loop: это четыре bounded
pre-start remediation packages и несколько штатных activation gates.

## 2. Независимые reviews и adjudication

- Codex reviewer A: full roadmap `NO_GO`, safe first wave `GO`.
- Codex reviewer B: full roadmap `NO_GO`, safe first wave `GO`.
- Claude reviewer C: full roadmap `GO`, пустой `START_BLOCKER` set.

Claude корректно обнаружил, что предыдущий canonical ledger устарел как список
текущих blockers: последние task edits закрыли aggregate read bounds, T24 B1/B2/
B3 cycle, external RC attestation, snapshot collection descriptors, interval
endpoint, typed body-read outcomes и большинство ownership gaps.

Codex reviewers корректно обнаружили residual seams, которые task headers и
acceptance ещё не закрывают. Parent synthesis проверил спорные места по текущим
файлам и классифицировал их ниже.

## 3. Safe first wave — начать сейчас

### Wave S0

Эти задачи имеют фиксированные inputs и не зависят от residual contracts:

1. **T01** — CES1 Poseidon primitives и tag registry;
2. **T11** — typed `BlockLifecycle::EndBlockResult` refactor;
3. **T29** — Stage 1 Variant-A owner-decision/profile gate;
4. **T32** — read-only Reth/Commonware persistence spike.

### Wave S1

- **T03** — после T01;
- **T35** — после T29;
- **T34 Part A draft** — после T29; approval остаётся после T30;
- T36 inventory drafting может идти параллельно, но approval должен ждать T35
  после исправления dependency, описанного ниже.

Таким образом, команда может переходить к реализации без ожидания очередного
полного аудита.

## 4. Pre-start remediation packages

### R1 — Mechanical DAG и acceptance ownership patch

Это один planning patch, а не новый architecture design.

1. **Activation predicate ownership.** T14 зависит от T13/T16, но T14 владеет
   `ces_active`, который должны потреблять T13/T16/T23/T29
   (`tasks/14-genesis-activation.md:41-45`,
   `tasks/16-persistence-coordinator.md:27-33`). Вынести predicate в upstream
   artifact/T14-A0 и добавить реальные edges.
2. **T35 → T36.** T36 ссылается на T35-owned aggregate rows, но оба gate сейчас
   зависят только от T29. Approval T36 должен зависеть от T35; drafting
   non-aggregate inventory может остаться parallel.
3. **T30 → T15 encoding sub-part.** T15 реализует T30-owned staged-batch
   grammar, но header содержит только T03. Добавить edge либо явно разделить
   early MDBX environment и post-T30 encoding/commit part.
4. **Read-surface inventory timing.** Repository-wide selector/consumer
   inventory сейчас принадлежит downstream T27, хотя T36 раньше фиксирует
   product surface. Генерацию inventory перенести в T36; T27 оставляет
   post-cutover re-check.
5. **Upstream versus downstream acceptance.** T04/T14, T12/T16 и T20/T33 должны
   разделить producer-side fixture/API acceptance и downstream real integration
   evidence. Upstream task не должен завершаться только после consumer code.

**Blocks:** approval/implementation затронутых T13/T15/T16/T20/T27/T36 paths.  
**Не blocks:** Wave S0.

### R2 — Active-empty partition lifecycle carrier

Текущая recorded semantics:

- never-populated active partition имеет domain-state-only retirement;
- core call и `PartitionRetiredV1` event отсутствуют;
- T20 effective coverage должен отражать post-H activation/retirement;
- T33 использует effective coverage для readiness.

Не определён finalized input, через который T20 атомарно узнаёт domain-only
active-empty lifecycle.

Нужно выбрать один вариант до T20/T23/T33 implementation:

1. versioned finalized domain lifecycle event/delta; или
2. exact-checkpoint bounded reconciliation через `ActiveTributePartitionsView`.

Зафиксировать activation, domain-only retirement, core retirement, replay,
idempotence, crash atomicity и effective-coverage update.

### R3 — Multi-block unfinalized ancestry reconstruction

T12 говорит, что потерянный unfinalized batch пересоздаётся re-execution
кандидата against finalized parent. Этого недостаточно, когда direct parent и
несколько его ancestors также unfinalized. T17 сознательно занимается только
finalized heights.

До T12/T17 implementation определить:

- owner ancestry resolver/replay;
- источник ordered candidate blocks;
- replay от последнего persisted finalized marker;
- проверку каждого hash/root/artifact link;
- candidate-window bounds;
- outcomes для missing middle block/fork switch;
- restart/cache-loss/multi-ancestor tests.

Local defer/abstain остаётся корректным fallback, но не является recovery
algorithm.

### R4 — Full snapshot activation barrier

T22 теперь проверяет exact local hash at H, но acceptance не требует полного T16
durable-Reth barrier:

- durable block;
- receipts;
- EVM state;
- `last_sealed_root(H)`;
- artifact/scheme binding.

Перед T22 implementation зафиксировать reuse общего DB-only barrier API и
negative tests для wrong slot root/artifact и missing receipts/state.

Fresh-node contradiction в основном закрыт честным scope narrowing: CE snapshot
не содержит Reth state, Reth H получается раньше через ordinary sync или paired
restore. Нужно только синхронизировать остаточную concept wording.

## 5. Authority sync до затронутых consumers

Это не новый product design и не блокирует S0, но должно выполняться до T07/T10/
T24 и activation review.

1. Decision map Q15 привести к уже принятому pre-reserve body-size rule и
   включить `retire_partition`.
2. Concept/decision map/benchmark requirements добавить полный T24 output set:
   aggregate Mongo read bounds и `K_domain` selection.
3. Сохранить единый execution-only `<2 s` gate; persistence-through-ACK остаётся
   report metric.
4. Decision map явно синхронизировать с Variant-A Mongo execution exception,
   readiness и Gem deferral.
5. §19/Q13 заменить production/mainnet wording на Stage 1 testnet evidence,
   сохранив отдельный OPEN production gate.
6. T27 убрать ложное утверждение о существующих Nod CLI commands; отсутствие
   surface является допустимым решением T36, а не поводом создать новую feature.

## 6. Activation gates — не блокируют начало реализации

1. T24 Q11 numerical closure и все B2 consumer re-baselines;
2. T25 cross-architecture, fuzz, crash, restore, soak и external RC attestation;
3. T34 hardware/benchmark protocol/runbooks/SLO;
4. finalized-parent Variant-A liveness scenario: body-dependent work на
   certified-but-not-finalized parent даёт defer/abstain без invalid verdict и
   сеть возобновляет progress после finalization;
5. import-ID retention-lease → Mongo baseline/`FinishedHeight` durable handoff
   fault matrix;
6. testnet/mainnet documentation sync.

Это ожидаемые deliverables реализации и release phase, а не причины продолжать
архитектурный аудит сейчас.

## 7. Что считать закрытым

Текущие task edits уже закрывают или честно сужают прежние крупные gaps:

- aggregate Lysis/point-read bounds и T24 owner;
- T35 self-sufficiency и single aggregate authority;
- T24 B1/B2/B3 split;
- external immutable RC attestation;
- T33/T27 post-cutover ownership;
- snapshot collection descriptors;
- Reth-first snapshot prerequisite;
- recent-version exclusive endpoint;
- tree-only full-node degraded posture;
- execution-only Q11 timing gate;
- typed point-read outcome union;
- `K_domain` candidate objective/tie-break;
- frozen byte-accounting structure;
- repository-wide cutover inventory scope;
- corrected milestone sequencing.

Старый `.reaudit/ledger.md` больше нельзя использовать как прямой current
blocker count без сверки с текущими task packets.

## 8. Финальное решение

```text
Begin implementation: GO
Safe immediate wave: T01, T11, T29, T32
T03: start after T01
Full autonomous parallel T01–T36 launch: wait for R1–R4 packet edits
Stage 1 activation: wait for implementation + activation gates
Further broad roadmap audit: STOP
```

Следующий шаг — внести R1–R4 как один короткий planning patch и параллельно
запустить Wave S0. После этого проверки должны быть task-local: tests,
acceptance evidence и targeted dependency checks, а не новый полный audit loop.
