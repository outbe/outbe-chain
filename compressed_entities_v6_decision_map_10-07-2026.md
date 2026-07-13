# Compressed Entity Storage v6 — карта решений

> **Superseded in parts (2026-07-13, postfix PF-H01):** этот документ — исторический снимок на 10-07.
> Более поздние recorded owner-решения Stage 1 (концепт §3.2/§14.3: Variant A Mongo execution exception,
> readiness state machine READY/DEGRADED_KEY/NOT_READY, Gem deferral, контракт Gate D0/T29) имеют
> приоритет при любом расхождении с текстом ниже.

Статус: Q1–Q10 и Q12–Q23 закрыты; Q11 имеет решённую структуру и provisional параметры, а numerical closure
требует обязательного benchmark.

Цель карты — последовательно закрыть решения, необходимые для прочной, ясной и расширяемой системы хранения
текущих NFT-like records вне live EVM state.

Off-chain computation не входит в эту карту. После ответа на каждый ticket соответствующие места обновляются в `compressed_entities_concept_v6_proposed_10-07-2026.md`.

Правило работы: tickets разбираются по порядку.

Вариант «рекомендуемое решение» — исходная позиция для обсуждения, а не уже принятое owner decision.

## #1: Какое точное обещание даёт система?

Blocked by: —

Type: Research

Research asset: `compressed_entities_v6_q1_guarantees_research_10-07-2026.md`

### Question

Какие свойства система обязана гарантировать?

Нужно отдельно подтвердить или отклонить:

1. Целостность текущего body и его присутствия относительно finalized root.
2. Что каждая committed mutation прошла зарегистрированные domain rules.
3. Доступность body при включении блока.
4. Долговременную доступность текущего body после pruning history.
5. Долговременную доступность исторических body.
6. Полноту secondary-index запросов (`by_owner`, `by_wwd`).
7. Доступность media bytes.

Рекомендуемое решение: consensus гарантирует пункты 1–3. Full-data operators хранят current bodies и
обслуживают verifiable point reads, но полная локальная custody не является наблюдаемым consensus invariant.

История, полнота списков и media availability не гарантируются.

### Answer

Решение владельца 2026-07-10 с уточнением 2026-07-12 по `Q10`:

Consensus гарантирует:

1. `R_sealed(H)` коммитит присутствие и body commitment каждой current entity.
2. Root получен из fork-active deterministic mutations, принятых domain rules и generic lifecycle.
3. При исполнении блока domain runtime детерминированно создаёт exact canonical body без MongoDB или внешней БД storage layer.

Operational validator profile требует хранить current bodies и обслуживать point body/proof reads.
Это не является globally provable completeness invariant:

1. Нода не может знать, что локально сохранены все bodies, без полного `O(N)` обхода SMT и body store.
2. Такой полный scan не является startup/readiness requirement и не выполняется на каждом checkpoint.
3. Отсутствующий или повреждённый body может оставаться необнаруженным до конкретного read либо будущей
   body-dependent operation.
4. При чтении нода проверяет найденный body против current leaf. Missing/mismatched body возвращает
   `unavailable` и запускает локальное восстановление из retained events, snapshot chunks или peers.
5. MongoDB может физически хранить bodies и indexes, но ни Mongo high-water, ни snapshot manifest не доказывают
   глобальную полноту текущих body bytes.
6. Secondary indexes остаются rebuildable projection и не имеют authenticated completeness.

Client rule:

1. Клиент проверяет finality/header и извлекает `R_sealed(H)` независимо от body RPC.
2. Body принимается только после проверки body hash и SMT proof против этого root.
3. Invalid или unavailable response вызывает failover; отдельный RPC slashing не требуется.

Network retrieval assumption: хотя бы один provider, имеющий запрошенный current body, достижим для клиента.

Каждый validator deployment обязан иметь current-body materialization и point body/proof service capability.
Эта capability может быть отделена от signing process; signing host не обязан быть публичным Internet endpoint.

Не гарантируются:

- body bytes для deleted/superseded historical versions;
- существование, достижимость или вечная работа archive provider;
- completeness, ordering и отсутствие пропусков secondary-index lists;
- media availability;
- proof generation для произвольных historical roots;
- off-chain computation.

Archive profile, если он запущен, хранит blocks/receipts с genesis.

Snapshot является node-local recovery/bootstrap carrier. Его root marker связывается с finalized checkpoint,
но snapshot не доказывает глобальную body completeness. Каждый реально возвращаемый или используемый body
проверяется против current leaf; пропуски являются локальной availability failure.

Формат и recovery mechanics принадлежат `Q10`.

## #2: Кто имеет исключительное право создавать mutation?

Blocked by: #1

Type: Discuss

### Question

Где проходит seam между domain runtime и `compressed_entities` module?

Нужно решить:

1. Может ли mutation инициировать только fork-registered domain module?
2. Может ли пользователь напрямую вызвать storage engine или адрес `0xEE0B`?
3. Кто вычисляет `id_bytes`, `tree_key`, active versions и `leaf_value`?
4. Кто эмитит canonical event и какой EVM address обязан стоять в log envelope?
5. Что именно domain module должен проверить до вызова storage module?

Рекомендуемое решение: fork-designated domain function проверяет invocation context и business rules.

Затем она вызывает typed core operation: mint/update с полным canonical body либо delete только с ID.

Core сам проверяет active domain и generic lifecycle, вычисляет commitment fields, записывает mutation и эмитит canonical event. У `0xEE0B` нет публичного mutation ABI.

### Answer

Решение владельца 2026-07-10, подтверждённое committee review:

```text
EVM transaction или fork-scheduled system handler
  → fork-designated mutating domain entrypoint
  → internal CompressedEntityStore interface
  → journaled mutation
```

1. Authority задаётся fixed fork-active consensus call graph. Отдельные capability tokens не используются.
2. Разрешены только явно назначенные mutating entrypoints, а не любая функция domain runtime.
3. Entrypoint может быть EVM precompile handler, system/lifecycle handler или явно предусмотренный cross-domain path.
4. Domain code проверяет authenticated invocation context, authorization, business rules и все domain-specific production conditions.
5. Для mint domain code внутренне генерирует `raw_id`; для update/delete выбирает существующий ID. Для mint/update он формирует полный canonical body.
6. `domain_id` закреплён за fork-active entrypoint. Он не является свободным user calldata argument.
7. Core отвергает unknown/inactive domain и проверяет generic lifecycle.
8. Core сам выводит `id_bytes`, `tree_key`, active versions, `leaf_value`, journaled block-batch update и canonical event.
9. Caller не передаёт готовые commitment fields, root, shard, pending state или canonical event payload/address.
10. Core не имеет mutating EVM ABI. Direct call к `0xEE0B` не может выполнить `mint/update/delete`.
11. Canonical generic event создаёт только core и эмитит от `0xEE0B`. Domain events допустимы, но не являются recovery source.
12. Domain writes, core block-batch mutation и canonical event находятся в одном journaled execution scope и откатываются вместе.
13. Mutating EVM domain entrypoints принимают только обычный `CALL` к зарегистрированному адресу. Dispatcher
    отклоняет static и foreign-context schemes, включая `STATICCALL`, `DELEGATECALL` и `CALLCODE`; внутренний
    core не получает EVM call scheme.

System mutations проходят через тот же domain runtime и core внутри receipt-visible system transaction. Raw hooks не могут напрямую вызывать compressed mutation.

## #3: Где создаётся canonical body и что получает storage core?

Blocked by: #2

Type: Discuss

### Question

Где заканчивается domain-specific processing и начинается generic compressed storage?

Нужно зафиксировать единый контракт для обычных, encrypted и любых будущих domains без специальных веток в storage core.

### Answer

Решение владельца 2026-07-10:

```text
mint/update domain entrypoint
  → проверяет все domain-specific правила
  → создаёт semantic record
  → canonical-encode по fork-active domain schema
  → вызывает CompressedEntityStore
```

1. Для mint/update canonical body всегда создаёт domain module до вызова storage core. Delete не имеет body.
2. Для Tribute `TributeFactory` обрабатывает encrypted calldata, а `Tribute` передаёт итоговый canonical `TributeData` в core.
3. Способ получения semantic record — обычный код, TEE, oracle или иной domain mechanism — не входит в storage semantics.
4. Для mint/update core получает `{fork_bound_domain_id, raw_id, canonical_body_bytes}`; для delete — `{fork_bound_domain_id, raw_id}`.
   Версии codec/hash выбираются по fork/domain registry, а не caller input.
5. Core не расшифровывает input, не проверяет domain evidence и не имеет TEE- или producer-specific path.
6. Core хеширует exact canonical bytes, применяет generic lifecycle, journal и SMT mutation и публикует canonical event по правилу `Q4`.
   Для mint/update event содержит полный body; delete event содержит только canonical entity identity.
7. ZKP для связи calldata и результата не используется. Корректность обеспечивает обычное consensus execution domain call graph.

Recovery paths не меняют module boundary:

- full execution повторяет domain path из transaction inputs;
- MongoDB projection применяет finalized canonical events;
- snapshot bootstrap принимает current body set на `H` и затем применяет finalized events после `H`.

Mint/update всегда передают полный canonical body; delete передаёт только ID. Exact event/DA carrier решается в `Q4/Q5`.

## #4: Как canonical mutation публикуется для recovery и MongoDB rebuild?

Blocked by: #1, #3

Type: Discuss

### Question

Должен ли каждый successful `mint/update` публиковать полный canonical body в receipt event, даже когда те же bytes уже доступны из calldata?

Зафиксированные требования:

1. Full execution может повторно получить body через domain call graph.
2. MongoDB должна восстанавливаться из finalized events без domain-specific re-execution.
3. Для Tribute calldata зашифрован и не является публичным body source.
4. Core хеширует те же canonical bytes, которые передаёт canonical publication path.
5. Любая system mutation должна иметь receipt-visible carrier; raw hook event, отсутствующий в receipt, не является recovery source.

Варианты:

1. Uniform full-body event: каждый successful `mint/update` публикует полный canonical body.
   Calldata остаётся domain input и может содержать те же bytes, derivation inputs или ciphertext. Рекомендуется.
2. Per-domain carrier: public domains восстанавливаются из calldata, encrypted/derived domains — из event. Экономит receipt bytes, но требует domain-aware projector и двух recovery paths.
3. Calldata-only body: event содержит metadata/hash. Не подходит Tribute и нарушает требование generic event-only Mongo rebuild.

Delete payload определяется в `Q5`; старый body для него не требуется.

### Answer

Решение владельца 2026-07-10, подтверждённое тремя независимыми reviews:

```text
reverted core mint/update/delete
  → no mutation
  → no block-batch change
  → no canonical event

successful core mint/update/delete
  → one ordered mutation
  → exactly one receipt-visible canonical event
  → full body for mint/update; canonical identity for delete
```

1. Calldata остаётся execution/replay input. Canonical event является единым input для Reth ExEx и MongoDB rebuild.
2. `Exactly one` означает один event на каждую successful core operation, а не один на ID, transaction или block.
3. Event эмитит только core от `0xEE0B`. Domain events не являются recovery source.
4. Для mint/update core вычисляет `leaf_value` из тех же exact canonical bytes, которые помещает в event.
5. Mutation batch update и event находятся в одном journaled execution scope и откатываются вместе.
6. Raw hook не может вызывать compressed mutation: его logs не входят в receipts. System mutation выполняется через receipt-visible system transaction.
7. ExEx применяет только непрерывную последовательность finalized blocks. Обычный Reth `ChainCommitted` не считается finality signal.
8. Finalized gate сверяет `{height, block_hash}` и догоняет диапазон `high_water+1..finalized_tip` из canonical blocks/receipts.
9. Event order: `block_number → transaction_index → log_index_in_receipt`.
10. Idempotency identity: `{block_hash, transaction_index, log_index_in_receipt}`. Equal replay — no-op; conflicting payload — corruption error.
11. Mongo high-water `{height, block_hash}` продвигается после durable whole-block apply. `FinishedHeight` отправляется только после этого.
12. Unknown event version, malformed canonical event или finalized hash conflict обрабатываются fail-closed.
13. ExEx/Mongo failure является локальной проблемой node и лечится replay. При missing/stale body point read
    возвращает `unavailable`; это не изменяет consensus root.
14. Successful core mutation без receipt-visible canonical event является non-conforming integration/protocol bug.
15. Delete также эмитит ровно один canonical event. По решению `Q5` он содержит canonical domain/ID и не содержит body.

Event size/gas limits принадлежат `Q11`. Snapshot tail и receipt retention принадлежат `Q10`. Детальный Mongo crash/commit protocol принадлежит `Q9`.

## #5: Каковы точные semantics mint, update и delete?

Blocked by: #2, #4

Type: Discuss

### Question

Как core представляет существование записи и что именно делают три generic операции?

### Answer

Решение владельца 2026-07-10, подтверждённое тремя независимыми reviews:

```text
CurrentState(key) = Absent | Present(non_zero_leaf)

Absent  --mint(id, full_body)----> Present(leaf(full_body))
Present --update(id, full_body)--> Present(leaf(full_body))
Present --delete(id)-------------> Absent
```

1. Core имеет три typed операции: `mint`, `update`, `delete`. Patch operation отсутствует.
2. Mint/update получают полный canonical body. Delete получает только fork-bound domain и raw ID.
3. Core сам выводит `id_bytes` и `tree_key`. Caller не передаёт key, leaf, status, root или event fields.
4. SMT хранит только `tree_key → non-zero leaf_value`. Generic status и tombstone отсутствуют.
5. `ZERO` является единственным empty/delete sentinel. Если derived present value равен `ZERO`, mint/update fail-closed.
6. Delete требует current presence и записывает transient `Deleted`; end-block seal превращает его в `SMT.update(tree_key, ZERO)`.
7. Pending overlay имеет три состояния: `Untouched | Set(non_zero_leaf) | Deleted`. Удаление pending entry не означает delete.
8. Mutation batch update и ровно один canonical receipt event коммитятся или откатываются вместе.
9. Mint/update events содержат полный body. Delete event содержит canonical domain/ID и не содержит old body, old leaf или placeholder status.
10. После finalized delete ExEx удаляет current Mongo body и его current index memberships.
11. Current proof после delete является non-membership proof. `Absent` означает только отсутствие key в выбранном current root.
12. `mint→update→delete` завершается absence; `delete→update` и повторный delete отклоняются.
13. По решению `Q6` domain generator никогда повторно не создаёт удалённый `raw_id`; historical used-ID registry core не хранит.
14. Snapshot после delete не содержит key, tombstone или старый body. Historical delete остаётся только в canonical ledger history.

CKB SMT v0.6.1 нативно реализует delete как update key нулевым value с удалением опустевших ветвей.

## #6: Как устроены ID namespace и domain registry?

Blocked by: #2

Type: Discuss

### Question

Кто создаёт `raw_id`, как исключаются cross-domain collisions и должен ли core хранить историю использованных ID после hard delete?

### Answer

Решение владельца 2026-07-10:

1. `raw_id` для mint генерирует только fork-registered domain module. Пользователь не передаёт свободно выбранный mint ID непосредственно в core.
2. Каждый domain сам выбирает алгоритм ID generation и обязан обеспечить его consensus determinism и lifetime uniqueness.
3. Один и тот же domain generator никогда не должен повторно создать `raw_id`, включая ID уже удалённой entity.
4. Алгоритм может использовать domain counter, nonce, UUID-like derivation, unique source identity или другую схему. Конкретный выбор не является generic storage primitive.
5. Любой entropy/source должен быть consensus-visible либо уже проверенным deterministic domain input. Локальный RNG и future block hash запрещены.
6. Если generator использует mutable state, его изменение journaled и откатывается вместе с failed mint.
7. Core не проверяет historical reuse и не хранит used-ID set, IMT или tombstone. При mint он проверяет только current absence.
8. Core всегда канонизирует ID через tagged encoding `{domain_id, encoding_kind, len, raw_id}` и выводит domain-separated `tree_key`.
9. Конкретный `id_encoding_kind_u8` фиксируется на domain version в fork-governed registry; caller и RPC не
   выбирают mode, byte value или version. Exact u8 assignment является обязательной частью domain activation
   specification и immutable для этой version.
10. Collision resistance `id_bytes/tree_key` опирается на ту же frozen hash assumption, что и остальные commitments.
11. Domain activation evidence обязано содержать collision/repeat/revert vectors для ID generator.

Следствие: hard delete остаётся настоящим `SMT.update(key, ZERO)` и не создаёт постоянного uniqueness state.

## #7: Каков commitment scheme и полный verification package?

Blocked by: #5, #6

Type: Discuss

### Question

Нужно решить:

1. Как shard roots объединяются в top commitment?
2. Входит ли `commitment_scheme_version` и `K` в domain-separated top commitment?
3. Что содержит header artifact кроме root; нужен ли `mutation_count`?
4. Что обязан содержать RPC proof package, чтобы исключить stale-root/cross-chain replay?
5. Обещает ли v1 proofs только для latest finalized state или также для historical heights?
6. Какая hash suite и byte-to-field encoding являются consensus rules?

Рекомендуемое решение: binary Merkle commitment над фиксированными shard roots и explicit root-scheme version.

Header artifact имеет вид `{scheme_version, root}`. Point proof связывается с `{chain_id, height, block_hash, root, domain, id, schema/hash versions, body}`.

V1 гарантирует proof generation только для latest finalized state.

### Answer

Решение владельца 2026-07-10:

```text
commitment_scheme_v1:
  hash suite = существующий Circom-parameter-compatible Poseidon-BN254 из outbe-poseidon
  collection K_domain = 2^k shards, fixed per domain version
  R_collection = fixed binary Poseidon top над collection shard roots
  Root Catalog SMT = collection_key → R_collection
  R_sealed = Poseidon(TAG_SEALED_ROOT, commitment_scheme_version, catalog_root)
```

1. Внутри Compressed Entity commitment scheme Keccak не используется. `id_bytes`, `tree_key`, body commitment, leaf, CKB SMT nodes, top-shard nodes и `R_sealed` используют одну Poseidon-BN254 hash suite.
2. V1 закрепляет текущий `outbe-poseidon::Poseidon` с BN254 `Fr` и Circom-parameter-compatible parameter set.
   Из-за non-zero initial domain tag circuit reproduction использует `PoseidonEx(initialState = tag)` либо
   эквивалентный gadget, а не stock zero-state `Poseidon(nInputs)` template. Poseidon2 создаёт другие roots и
   может появиться только как новая `commitment_scheme_version`, а не как прозрачная замена реализации.
3. Все symbolic domain tags соответствуют различным фиксированным ненулевым `Fr` constants. До activation их
   concrete canonical values перечисляются в normative parameter table §4.2 concept; golden vectors проверяют,
   но не определяют эту таблицу. Caller не передаёт tag, field, hash version или параметры Poseidon.
4. Произвольные bytes кодируются инъективно: слева направо по 31-byte chunks, последний chunk
   right-zero-padded, каждый chunk интерпретируется как unsigned big-endian integer `< 2^248 < p`.
   Original byte length и chunk count входят в tagged Poseidon chain. Прямая mod-reduction произвольных
   32-byte chunks запрещена.
5. Все field/hash outputs имеют единственную wire/storage форму: canonical 32-byte big-endian BN254 field
   element. Non-canonical external encodings отклоняются. `domain_id` имеет тип `u16`; `byte_len` и `body_len`
   являются unsigned `u64` byte counts, canonically embedded into `Fr` without reduction.
6. По `Q23`, `tree_key` включает derived `collection_key`; `shard_index = low_k_bits(tree_key)` для
   `K_domain = 2^k`, зафиксированного domain version.
7. Vendored CKB SMT сохраняет key/path, update/delete, compact-zero и proof mechanics,
   но его Outbe-owned node codec использует typed domain-separated Poseidon merges.
   `ZERO + ZERO → ZERO` остаётся structural empty rule. Normal, base и merge-with-zero nodes
   имеют разные tags и включают все CKB semantic fields.
   `zero_count` сохраняет upstream semantics: `u8` с wrapping addition modulo 256; full 256-level compact path
   кодируется значением `0`.
8. Structural empty subtree и empty shard root законно представлены `ZERO`. Только Poseidon output над
   non-empty content — present leaf, base/internal node, non-empty shard root, top node или sealed root —
   случайно равный `ZERO`, вызывает deterministic fail-closed error; `ZERO` не может обозначать present content.
9. Каждая collection имеет собственные shard SMT и fixed-depth top в ascending shard order. `R_collection`
   связывает scheme, collection key, `K_domain` и top root. Dynamic Root Catalog SMT связывает
   `collection_key → R_collection`.
10. `R_sealed` связывает `commitment_scheme_version` и Root Catalog root.
    Header artifact содержит только `{commitment_scheme_version, R_sealed}`.
    `mutation_count` не является consensus field.
11. Point-proof package содержит `{chain_id, block_height, block_hash, commitment_scheme_version,
    R_sealed, domain_id, partition_key_or_none, raw_id, schema_version, hash_version, body_bytes for membership,
    proof_encoding_version, smt_proof, collection_shard_proof, root_catalog_proof}`. Shard index всегда выводится из `tree_key` и не
    принимается как отдельное wire field.
12. Клиент независимо выбирает finalized header, сверяет `{chain_id, height, block_hash}`, связывает package с
    ожидаемыми `{domain_id, raw_id}`, берёт `id_encoding_kind_u8` из fork-active registry на этой высоте и заново
    выводит partition/collection/ID/key/shard/leaf. Package/RPC не выбирает derived locator. Затем клиент
    проверяет shard proof, collection top, Root Catalog proof и `R_sealed` из header artifact.
13. V1 обязан генерировать proofs только для latest persisted finalized state.
    Уже сохранённый historical proof остаётся проверяемым против своего historical finalized root,
    но in-place tree не обязан сгенерировать его позднее.
    Один proof package собирается внутри одной MDBX read transaction/snapshot: marker, top/shard metadata и
    SMT nodes не могут читаться из разных snapshots.
14. Exact Poseidon tags, byte-chain vectors, empty collection/catalog roots, every CKB merge form, configured
    per-domain shard tops, Root Catalog, `R_sealed`, membership и non-membership proofs входят в mandatory vectors.
15. В текущем `OutbeBlockArtifacts` wire namespace commitment использует следующий свободный tag `0x08`;
    `0x07` уже занят committee pre-announcement. Codec addition требует artifact-envelope version bump.
    Этот tag является integration identifier и не входит в Poseidon commitment semantics.

## #8: Как journaled block batch завершается атомарно?

Blocked by: #2, #5, #7

Type: Discuss

### Question

Нужно определить:

1. Где живёт block-level mutation batch и как он получает EVM rollback semantics?
2. Нужен ли ordered op-log при последовательном transaction execution?
3. Как повторные mutations одного key сворачиваются в final state?
4. Каков обязательный end-block post-condition?
5. Что происходит при tree/root/cleanup error?
6. Как seal work ограничивается по времени относительно consensus deadlines?

Рекомендуемое решение (уточнено Q23): journaled EVM block batch содержит unique composite entity locators,
pending entity states и pending collection retirements. Отдельный ordered op-log не нужен.

End-block сначала строит staged tree batch, затем атомарно записывает root, очищает каждый pending slot
и все touched entity/collection vectors, после чего проверяет post-condition.

Любая ошибка отклоняет block execution и отбрасывает staged tree batch.

### Answer

Решение владельца 2026-07-10:

```text
begin block:
  require touched entity/collection vectors are empty
  open journaled EVM block-batch view

each successful core mutation:
  derive collection_key and shard
  if pending[collection_key,key] == Untouched: append locator once
  pending[collection_key,key] = Set(non_zero_leaf) | Deleted
  emit exactly one canonical receipt event

end block:
  prepare one staged SMT batch from final pending values
  compute R_sealed; proposer exports it, validator verifies supplied artifact
  write last_sealed_root
  clear every pending entity/collection entry and touched vector
  flush once
```

1. Batch живёт в journaled EVM storage по адресу core. Поэтому nested-call, failed-transaction
   и atomic system-handler rollback автоматически откатывают batch update вместе с canonical event.
2. Transactions выполняются последовательно. Отдельный ordered op-log не хранится:
   receipt events уже сохраняют ordered mutation history, а SMT требует только final value
   каждого touched key.
3. `touched_entities` содержит composite locator ровно один раз — при первом переходе из `Untouched`.
   Повторные mutations только заменяют `pending[collection_key,key]`. Например, `mint → update → delete`
   оставляет один touched key и final `Deleted`.
4. `StorageVec::clear()` обязан занулить все element slots и затем length; текущий storage primitive уже имеет эту semantics.
5. End-block сортирует final updates в canonical order, группирует их по shard и создаёт staged tree batch без persistent side effects.
6. После всех transactions и остальных fork-active end-block lifecycle modules один общий
   `run_end_block_seal()` выполняется последним consensus end-block шагом перед state-root calculation.
   Proposer передаёт computed root в header artifact builder; validator требует точного совпадения с supplied
   artifact. Затем buffered post-block hook одной пачкой записывает `last_sealed_root`, зануляет pending slots,
   очищает entity/collection pending state и уведомляет state-root task через
   `OnStateHook(StateChangeSource::PostBlock(StateChangePostBlockSource::Other("compressed_entities_seal")))`.
7. Обязательный post-condition: оба touched vectors пусты; все обработанные pending entries `Untouched`;
   `last_sealed_root == computed R_sealed`.
8. По решению `Q22`, `BlockLifecycle::end_block()` имеет associated typed result. Ordinary modules возвращают
   `()`, а `CompressedEntitiesLifecycle` возвращает `SealOutput { R_sealed, staged_tree_batch }` прямо executor-у.
   Executor держит output как локальное typed значение; после успешных finish и block sealing присваивает
   `block_hash` и публикует batch в speculative cache.
   Если tree preparation, root comparison, cleanup, state flush, executor finish или sealing возвращает ошибку,
   block не производится/отклоняется, а provisional batch отбрасывается. Частично sealed block не существует.
9. `mutation_count` не является header/root artifact по `Q7`; для commitment correctness он не нужен.
   Provisional per-tx/per-block attempt counters из `Q11` являются executor-local, non-persistent и
   non-journaled resource guards; proposer и validator пересчитывают их на одинаковом sequential call path.
   Они не входят в EVM state или header artifact. Exact attempt boundary решает `Q15`.
10. По `Q23`, fixed per-domain collection shard tops и Root Catalog являются bounded work; основной variable
    cost — worst-case single-collection/single-shard batch, Poseidon byte hashing и journal cleanup.
11. Численные `max_unique_keys_per_block`, body-byte limits и deferred-seal gas charge определяются
    только в `Q11` после обязательного worst-case benchmark. Target: gas-saturated full-block execution
    на минимальном validator hardware укладывается менее чем в 2 секунды при default timing contract.
    До benchmark первая реализация использует provisional `50_000` gas per core mutation attempt и общий
    provisional cap `600` attempts per block; эти значения не являются activation evidence.
12. По решению `Q21` pending map использует один `U256` slot: `0 = Untouched`, canonical words
    `1 <= word < p` кодируют `Set(leaf_value)`, `U256::MAX = Deleted`; остальные words от `p` до
    `U256::MAX - 1` invalid.
    Delete sentinel не может совпасть с canonical `Fr` leaf и зануляется вместе с остальным transient state.

## #9: Каков единственный допустимый commit/recovery protocol?

Blocked by: #8

Type: Discuss

### Question

Нужно выбрать точный persistent ordering:

1. SMT batch коммитится после durable canonical block или только после finalized block?
2. Может ли persistent in-place tree когда-либо оказаться впереди durable chain head?
3. Какой marker пишется атомарно с tree nodes?
4. Как исполняется child поверх staged, но ещё не persisted parent?
5. Какие состояния restart допустимы и как каждое восстанавливается?
6. Как ограничивается память staged branches?
7. Когда current-body/Mongo high-water и ExEx `FinishedHeight` могут продвинуться относительно durable finalized block?

Рекомендуемое решение: persistent SMT содержит только finalized state. Финализированный блок и соответствующий
EVM state checkpoint всегда становятся durable раньше tree commit.

Marker `{commitment_scheme_version, height, block_hash, parent_block_hash, parent_root, new_root}`
атомарен с nodes.

Допустимо только `tree_height <= min(durable_evm_height, finalized_chain_height)`; lag лечится replay,
ahead/different hash считается invariant violation; speculative execution использует bounded in-memory overlays.

Mongo whole-block apply завершается до high-water marker, а `FinishedHeight` отправляется только после durable marker.

### Answer

Решение владельца 2026-07-12:

1. Persistent in-place SMT содержит только finalized state. Нефинализированные candidates создают immutable
   staged batches, keyed by `block_hash`; child читает через staged ancestor chain. Losing branches удаляются
   без persistent rollback.
2. Commonware Marshal сначала durable-записывает block + finalization archives и только затем dispatch-ит
   finalized block executor-у. Обычный Reth canonical notification, успешный `new_payload` или FCU сами по себе
   не являются durable EVM barrier.
3. После успешных execution и finalized FCU coordinator принудительно сохраняет Reth canonical block,
   receipts и EVM state через height `H`, ждёт completion и сверяет `{height, block_hash}` и on-chain
   `last_sealed_root`. Только после этого разрешён SMT commit для `H`.
4. Одна transaction в отдельном CE-owned MDBX environment под node datadir записывает changed nodes,
   shard roots/top metadata и
   `last_applied = {commitment_scheme_version, height, block_hash, parent_block_hash, parent_root, new_root}`.
   Marker и tree data атомарны.
5. Marshal ACK отправляется только после успешного idempotent SMT commit. При активном compressed storage
   finalized delivery ACK-gated, а `MAX_PENDING_ACKS = 1` является обязательным startup invariant: следующий
   finalized block не доставляется до ACK предыдущего. Поэтому durable Marshal history всегда не позади SMT,
   durable Reth EVM checkpoint никогда не позади SMT, а marker lag ограничен одним in-flight block.
6. Обязательный invariant:

   ```text
   persistent_tree_height <= min(durable_evm_height, consensus_finalized_height)
   ```

7. Crash до Reth durable barrier приводит к повторной executor delivery. Crash после Reth persistence, но до
   SMT commit, оставляет безопасный tree lag: batches восстанавливаются из finalized canonical core events
   и применяются последовательно с проверкой каждого committed root. Crash после SMT commit, но до Marshal ACK,
   приводит к idempotent redelivery/no-op по совпадающему marker.
8. Tree marker впереди durable EVM/finalized height, same-height hash conflict, broken parent continuity или
   root mismatch являются corruption/invariant violation: node fail-closed и восстанавливается из snapshot,
   anchored к independently verified finalized root, а не чинит in-place tree эвристически.
9. Staged batches являются reconstructible cache. Их branch/count/byte bounds задаются protocol-shaped limits;
   численные значения принимает `Q11` после benchmark. Eviction не меняет correctness: нужная ветка может быть
   повторно построена из finalized tree и последовательности candidate blocks.
10. Current-body materialization и secondary Mongo projection восстанавливаются независимо от SMT commit.
    Mongo block changes и `{height, block_hash}` high-water атомарны; `FinishedHeight` отправляется только после
    этого durable commit. Cursor доказывает только обработку event range, а не наличие каждого historical/current
    body row. Он не разрешает SMT advancement и не блокирует Marshal ACK.
    При Mongo outage `FinishedHeight` не продвигается, Reth pruning удерживается, а ExEx WAL растёт. Это
    ожидаемый durability backpressure и local disk/availability risk, который требует monitoring/capacity,
    но не меняет consensus state.
11. Proof-ready checkpoint определяется только finalized persistent tree:

    ```text
    proof_ready_height = persistent_tree_height
    ```

    Point body response дополнительно проверяет конкретный body против leaf на этом checkpoint. Missing/stale
    body даёт `unavailable`. Если body-store high-water впереди proof-ready tree, mismatch сначала считается
    возможным newer-body cursor skew: node ждёт tree catch-up либо получает body version выбранного root. Peer /
    event recovery current bytes запускается, только если mismatch сохраняется после выравнивания cursors.
    Глобального `required_body_height`/`ValidatorReady` gate нет.

## #10: Что snapshot/bootstrap гарантируют, а что остаётся local availability risk?

Blocked by: #4, #7, #9

Type: Discuss

### Question

Нужно решить:

1. Должен ли manifest доказывать отсутствие пропущенных tree chunks и current bodies?
2. Нужна ли глобальная one-body-per-present-leaf проверка или достаточно per-key verification?
3. Какие snapshot producers/peers обязан поддерживать validator bootstrap?
4. Что именно гарантируется после pruning исторических receipts?
5. Нужны ли ring/MMR в основном документе до появления pruning design?

Рекомендуемое решение: snapshot является resumable recovery carrier, anchored к finalized
`{height, block_hash, commitment_scheme_version, R_sealed}`. Он не является proof of local body custody.

Tree/body chunks имеют checksums и deterministic addressing; конкретные bodies проверяются против leaves при
загрузке или использовании. Глобальный one-body-per-leaf scan не требуется.

Archive history — отдельный profile; pruning/MMR — будущее решение.

### Answer

Решение владельца 2026-07-12:

1. Нода ничего не доказывает сети о содержимом своего диска. Snapshot bootstrap и body custody являются
   node-local availability/liveness concerns, а не consensus proof.
2. Snapshot выбирает finalized checkpoint `H` и содержит header metadata
   `{snapshot_format_version, profile, body_coverage, chain_id, genesis_hash, H, block_hash,
   commitment_scheme_version, R_sealed(H)}`.
   Marker сверяется с independently selected finalized header.
3. Snapshot transport следует решению `Q17`: логическая схема, canonical ordering/ranges, checkpoint identity
   и import semantics являются protocol-normative; физические chunk boundaries, compression и MDBX layout
   могут быть manifest-local. Peer-provided manifest/checksum не доказывает semantic completeness и не
   становится новым trust root.
4. Полный `SMT × body-store` scan и one-body-per-present-leaf pass не являются bootstrap/readiness requirement:
   при миллиардах записей нода не может постоянно подтверждать глобальную локальную completeness.
5. Tree chunks/nodes и body rows могут проверяться streaming при импорте либо лениво при доступе. Missing/corrupt
   tree data приводит к локальной execution/proof failure; missing/mismatched body приводит к `unavailable`.
   Они не позволяют подделанному body пройти client verification против `R_sealed(H)`.
6. Point read всегда заново сверяет конкретные canonical body bytes и proof с current finalized root.
   Пользователь не доверяет RPC node: invalid/unavailable response вызывает failover.
7. Missing data восстанавливаются из retained canonical events, других snapshot chunks или peers. Если ни один
   источник недоступен, соответствующая body-dependent функция этой node остаётся недоступной.
8. Snapshot producers и mirrors не являются trusted roles. Chunks можно получать от validators, archive peers
   или object storage; availability предполагает хотя бы один reachable source нужных bytes. Bulk snapshot
   endpoint не обязан быть публичным на каждом signing host.
9. Full replay остаётся fallback, пока требуемые blocks/receipts retained. После pruning гарантируется только
   проверяемость имеющихся current bodies против current root; deleted/superseded historical bodies и потерянные
   current bytes без внешней копии не восстанавливаются.
10. Snapshot в checkpoint `H` рекламируется как bootstrap-capable только пока доступный canonical event/receipt
    tail покрывает весь диапазон `H+1..head`. До pruning этого tail operator сохраняет его, создаёт/получает
    более новый usable snapshot либо перестаёт рекламировать старый snapshot как independently bootstrap-capable.
    Это operational availability invariant, а не обязанность каждого validator хранить историю с genesis.
11. Ring/MMR не решает body availability или snapshot completeness. Он нужен только будущей политике historical
    roots/proofs после history pruning и не входит в storage v1.
12. Off-chain computation остаётся вне scope. Будущий compute protocol отдельно определит поведение node при
    missing body-dependent inputs; Q10 не вводит преждевременный compute/readiness consensus rule.

## #11: Какие abuse cases и resource limits являются protocol invariants?

Blocked by: #2, #4, #8, #9

Type: Discuss

Required benchmark specification: `compressed_entities_v6_performance_benchmark_requirements_10-07-2026.md`

### Question

Нужно закрыть:

1. Direct calls и fake events.
2. Schema/hash-version downgrade.
3. Stale proof replay.
4. ID collision/squatting.
5. Shard grinding — все mutations в одном shard.
6. Excessive total calldata/event bytes.
7. Excessive unique keys/tree nodes/staged branches.
8. Malformed proof/snapshot parser inputs.
9. Node-local disk/map-size/resource exhaustion.

Рекомендуемое решение: explicit threat table; fixed fork-active mutation call graph без public core ABI; fixed canonical event source; versions selected by height, not caller.

Также обязательны aggregate byte/key limits, worst-case single-shard benchmark, bounded staging, structured errors и startup capacity checks.

Зафиксированный prerequisite из `Q8`: до выбора численных limits обязателен performance benchmark,
который одновременно измеряет Poseidon byte hashing, worst-case single-shard `update_all`, journal cleanup,
`OnStateHook(StateChangeSource::PostBlock(StateChangePostBlockSource::Other(...)))` notification, MDBX
reads/writes и concurrent proof serving на
минимальном validator hardware.

Benchmark обязан вывести, а не предположить:

1. `max_unique_keys_per_block`, `max_ce_mutation_attempts_per_tx` и
   `max_ce_mutation_attempts_per_block`;
2. aggregate body/event byte limits;
3. deterministic gas charge, заранее оплачивающий deferred seal work;
4. `max_staged_tree_bytes`;
5. safety margin, при которой gas-saturated full-block execution остаётся меньше 2 секунд.

### Answer

Provisional решение владельца 2026-07-12; финальное закрытие Q11 заблокировано benchmark:

```text
CE_MUTATION_GAS_PROVISIONAL = 50_000
MAX_CE_MUTATION_ATTEMPTS_PER_TX_PROVISIONAL = 600
MAX_CE_MUTATION_ATTEMPTS_PER_BLOCK_PROVISIONAL = 600
```

1. `50_000` является дополнительным fixed charge за каждую попытку внутреннего core `mint/update/delete`,
   а не за внешний precompile call или transaction. Batch из `N` core mutations платит `N × 50_000`.
2. Charge списывается до body hashing, journal writes и canonical event emission. Обычный dispatch/storage gas
   продолжает учитываться отдельно. EVM revert откатывает state/event, но не уже выполненную работу и gas.
   По `Q15`, после domain authorization вход в generic core атомарно reserve-ит один tx/block attempt slot и
   fixed gas. Failed gas/quota reserve не является attempt; после успешного reserve generic rejection или
   revert не отменяют attempt.
   Reserve classification order нормативен: gas sufficiency → per-transaction cap → remaining per-block
   capacity. Per-tx overflow всегда даёт `TransactionLimitExceeded`, даже если тот же entry исчерпывает block
   capacity; `BlockCapacityExhausted` применяется только к transaction, которая остаётся внутри собственного cap.
3. Block attempt counter ограничивает все CE paths вместе: user transactions, nested calls и receipt-visible
   system transactions. Это обязательно, поскольку system-call path имеет внутренний gas limit `10_000_000_000`
   и не ограничивается обычным user block gas `30_000_000`.
4. Attempts внутри включённой, но впоследствии reverted transaction учитываются в block resource budget:
   CPU/hash work уже произошло. При speculative execution transaction, которая не вошла в payload, proposer
   восстанавливает counter к pre-transaction checkpoint вместе с execution state.
5. Когда оставшегося block budget недостаточно для atomic transaction, payload builder откатывает её,
   возвращает typed `BlockCapacityExhausted`, не помечает transaction invalid и оставляет её в txpool для
   следующего блока. Оставшееся место может использоваться non-CE transactions.
6. Transaction, которая требует `>600` CE attempts даже в пустом block, не может быть бесконечно deferred.
   Статически распознаваемый oversized batch отклоняется admission; динамически обнаруженный overflow получает
   `TransactionLimitExceeded`/revert согласно entrypoint semantics и не считается кандидатом для переноса.
7. Proposer не включает transaction, вернувшую `BlockCapacityExhausted`. Validator, встретивший такой overflow
   в уже предложенном block, отклоняет block; capacity exhaustion не превращается в обычный successful/failed
   receipt, которым byzantine proposer мог бы поглотить nonce пользователя.
8. System mutation, не помещающаяся в cap, является block-build/integration error. Bulk system work обязано
   разбиваться по блокам с deterministic progress cursor.
9. До benchmark разрешены только текущие bounded/fixed canonical schemas Tribute/Nod/Gem. Variable/unbounded
   body schema не активируется с provisional flat gas.
10. `600 = 30_000_000 / 50_000` является временной согласованностью с user gas lane, а не доказательством
    производительности. С учётом additional ordinary gas normal user transaction достигает OOG до 600-й
    попытки; explicit attempt cap практически связывает 10B system lane. Только numerical closure Q11 остаётся
    open.
11. Обязательный benchmark должен подтвердить либо заменить `50_000/600`, после чего отдельно определяются
    final gas formula, body/event/key limits, worst-case single-shard capacity и local staging requirements.
12. Benchmark выбирает explicit candidate limits, строит saturated worst-case workload ровно на этих limits,
    измеряет complete path и принимает либо уменьшает candidates до выполнения `<2s` target с safety margin.

## #12: Как система эволюционирует без изменения core semantics?

Blocked by: #6, #7, #10

Type: Discuss

### Question

Нужно определить:

1. Какие изменения добавляют новый domain/schema без перестройки tree?
2. Какие изменения требуют нового commitment scheme и migration?
3. Как активируется initial root при продолжающемся legacy traffic?
4. Нужны ли freeze height, dual-write и activation height?
5. Как bounded-образом выводится из использования legacy EVM state?
6. Какие future concerns остаются только extension seams?

Рекомендуемое решение: новые domains/schemas используют существующий root scheme. Изменение K/hash/tree layout повышает `commitment_scheme_version`.

Для greenfield launch commitment scheme v1 активируется с genesis. Legacy migration, freeze и dual-write не нужны.

Off-chain computation и pruning accumulator остаются отдельными будущими документами.

### Answer

Решение владельца 2026-07-12:

1. Mainnet является greenfield network: Compressed Entity Storage v1 и `commitment_scheme_version = 1`
   активны с genesis.
2. Существующий testnet перед запуском новой реализации очищается. Его legacy Tribute/Nod/Gem state не
   мигрирует в новую сеть.
3. Genesis `R_sealed` является детерминированным empty-state root. Если genesis specification когда-либо
   включает entities, их canonical bodies и resulting root являются частью genesis artifacts и проверяются
   всеми нодами при genesis initialization.
4. Для v1 отсутствуют `H0/H1`, freeze window, legacy replay, dual-write, migration activation height,
   migration manifest и retirement старых EVM slots. Genesis domain versions имеют `activation_height = 0`;
   последующие domain versions сохраняют обычную fork-governed activation height.
5. Новый domain, body schema, derived index, media commitment внутри schema или proof transport может быть
   добавлен fork-активацией без изменения commitment scheme, пока неизменны общие key/value/tree rules.
6. Schema transition явно задаёт одну из политик: old record остаётся читаемым в своей версии, мигрирует при
   обычном update либо обновляется bounded protocol mutations. Callers не выбирают obsolete version.
7. Изменение `K`, Poseidon parameters, byte-to-field codec, key derivation, leaf/value formula, empty-tree
   semantics, SMT topology или top-root formula требует нового `commitment_scheme_version`.
8. Переход между commitment schemes является отдельным hard-fork design с собственными migration mechanics,
   acceptance evidence и activation plan. Эти mechanics не проектируются заранее внутри storage v1.
9. Off-chain computation и historical-root accumulators остаются отдельными будущими протоколами и не меняют
   v1 storage semantics.

## #13: Какие доказательства достаточны для принятия системы?

Blocked by: #7, #8, #9, #10, #11, #12

Type: Prototype

### Question

Какой acceptance package доказывает не только воспроизводимость, но и корректность/соразмерность решения?

Рекомендуемый минимум:

1. Независимая reference model: ordered mutations → final map → root.
2. Golden vectors для key/leaf/value/shard/top root/proof/event.
3. Differential tests vendored SMT против reference model.
4. Adversarial call/revert/OOG/delegate/callcode/static/reentrancy tests.
5. Crash-state matrix с fault injection между каждым persistent step.
6. Snapshot omission/corruption/resume tests.
7. Stale/cross-chain/wrong-version proof tests.
8. Worst-case single-shard and random-shard benchmarks на минимальном поддерживаемом validator hardware.
9. Long-run ingest/update/delete benchmark с MDBX growth и proof-serving concurrency.
10. Genesis initialization rehearsal на production-shaped empty state и, при наличии, genesis entities.

### Answer

Решение владельца 2026-07-12: принят многоуровневый acceptance package; все его группы являются
обязательными release gates перед mainnet.

1. Mathematical correctness: независимая reference model, golden vectors и differential tests SMT.
2. State-machine robustness: property tests и fuzzing для ordered mutations, codecs, proofs и malformed input;
   commitment vectors включают `zero_count` wrap 255→0 на полном 256-level compact path.
3. Consensus equivalence: одинаковые roots/events/errors у proposer и validator, replay и cross-architecture runs.
4. EVM integration security: nested revert, OOG, static/delegate/callcode/reentrancy, fake event, wrong emitter,
   unauthorized core call и version downgrade.
5. Persistence safety: fault injection на каждой durable boundary из `Q9` и полная restart matrix.
6. Recovery safety: snapshot omission/corruption/resume, malicious manifest, missing body/node и canonical replay.
7. Genesis reproducibility: одинаковый empty root и, если применимо, одинаковые genesis entities на всех нодах.
8. Resource safety: обязательный worst-case benchmark из `Q11`, включая single-shard load, MDBX, journal cleanup
   и concurrent proof serving.
9. Operational evidence: длительный testnet soak с sustained load, node restarts, catch-up и snapshot recovery.

Acceptance package не требует полной formal verification. Он проверяет observable protocol semantics и
critical failure boundaries соразмерными средствами.

Финальные performance thresholds определяет `Q11`. Длительность и точный workload testnet soak фиксируются
в release plan; это обязательный operational gate, но не consensus constant.

## #14: Каким механизмом Reth гарантированно persist-ит finalized tip до SMT commit?

Blocked by: #9

Type: Discuss

### Question

Принятый `Q9` ordering требует durable Reth block/receipts/EVM state через `H` до SMT commit и Marshal ACK.
В pinned Reth v2.2.0 normal persistence работает с lag threshold и не предоставляет публичного
`force_persist_to_head_and_wait()`; при паузе сети tip может не стать durable самостоятельно.

Варианты:

1. Настроить `persistence_threshold = 0`, `memory_block_buffer_target = 0`, ждать штатный
   `PersistedBlockSubscriptions`, затем проверять exact checkpoint через DB-only provider. Рекомендуется для v1:
   не требует Reth fork и сохраняет Q9 ordering; barrier latency входит в Q11 benchmark.
2. Добавить в Outbe fork Reth явный `persist_to_head_and_wait(H)` API. Даёт более прямой контракт, но создаёт
   дополнительную fork-maintenance поверхность.
3. Разрешить SMT commit до durable Reth checkpoint. Не рекомендуется: нарушает принятый Q9 invariant и создаёт
   состояние tree-ahead-of-EVM после crash.

Нужно выбрать механизм, timeout/fail-closed semantics и обязательную проверку completion.

### Answer

Решение владельца 2026-07-12: выбран вариант 1 — synchronous durable barrier на каждый finalized block без
добавления нового Reth API.

1. При активном compressed storage `persistence_threshold = 0` и `memory_block_buffer_target = 0` являются
   обязательной startup-конфигурацией, а не operator tuning. Несовместимая конфигурация отклоняется fail-fast.
2. После finalized FCU для блока `H` coordinator ожидает штатный Reth `PersistedBlockSubscriptions`. Notification
   означает, что persistence task завершил DB commit; если persisted tip уже `>= H`, ожидание завершается сразу.
3. После notification coordinator открывает DB-only read provider, который не видит canonical in-memory overlay,
   и проверяет: `persisted_tip >= H`, `block_hash(H) == expected_hash`, block/receipts/EVM state доступны и
   `last_sealed_root(H) == expected_R_sealed`.
4. Только после этих проверок разрешены atomic SMT commit для `H` и затем Marshal ACK.
5. Persistence error, disconnected notification или failed DB verification являются local fail-closed error:
   SMT не продвигается, ACK не отправляется, validator/readiness снимается. Consensus wall-clock timeout не
   меняет protocol result; operational watchdog может остановить node, но не пропустить barrier.

Два допустимых crash состояния после начала persistence protocol:

```text
Reth = H, SMT = H-1, crash до SMT commit
  → Reth уже содержит canonical block/receipts/EVM state H;
  → rebuild SMT batch H из canonical core events receipts H;
  → verify computed root == last_sealed_root/header artifact H;
  → commit SMT H; ACK.

Reth = H, SMT = H, crash после SMT commit, но до Marshal ACK
  → redelivery H сверяет complete last_applied marker;
  → equal marker является idempotent no-op;
  → ACK.
```

Состояние `Reth = H-1, SMT = H` этим ordering запрещено. Если оно обнаружено, это implementation/corruption
violation: node не предлагает/валидирует блоки и не обслуживает proofs до verified resync. Падение одной node
не меняет finalized network state; barrier нужен для bounded local recovery, а не для повторной финализации.

## #15: Где точно начинается и как учитывается CE mutation attempt?

Blocked by: #11

Type: Discuss

### Question

Attempt counter является block-validity resource guard, поэтому proposer и validator должны одинаково считать
OOG, generic lifecycle rejection, nested revert, full transaction revert и builder exclusion.

Варианты:

1. После domain authorization, при входе в generic core, атомарно reserve один attempt и provisional gas.
   Если reserve gas/quota не удался, attempt не начался. После успешного reserve attempt учитывается независимо
   от последующего generic rejection, nested revert или full tx revert; builder exclusion восстанавливает
   pre-transaction block-counter checkpoint. Рекомендуется.
2. Считать attempt сразу при входе до gas reserve, включая failed-OOG charge. Проще формально, но расходует
   block quota на операции, для которых deferred work даже не начинался.
3. Считать только successful mutations. Не рекомендуется: не ограничивает CPU, потраченный на rejected/reverted
   core paths, и расходится с принятым Q11 resource-budget смыслом.

Нужно также закрепить per-tx overflow, block-capacity overflow и validator fail point.

### Answer

Решение владельца 2026-07-12: выбран вариант 1.

```text
domain authorization / business validation
  → не является CE attempt

entry в generic core mint/update/delete
  → atomically check/reserve tx slot, block slot и fixed gas

reserve failed
  → attempt не начался

reserve succeeded
  → attempt учитывается независимо от последующего generic reject/revert
```

1. Per-tx и per-block counters являются executor-local, non-persistent и non-journaled. Они сбрасываются в
   начале transaction/block и одинаково пересчитываются proposer и validator.
2. OOG на самом fixed-gas reserve не считается attempt: deferred CE work не начинался, обычный EVM OOG остаётся.
   Reserve classification order: gas sufficiency → per-tx cap → remaining per-block capacity. Per-tx overflow
   всегда классифицируется `TransactionLimitExceeded`; `BlockCapacityExhausted` рассматривается только после
   того, как transaction остаётся внутри собственного cap.
3. После успешного reserve считаются mint-on-present, update/delete-on-absent, unknown/inactive-domain guard,
   и любой другой generic core rejection.
4. Attempt внутри nested revert или полной reverted transaction остаётся в block counter, если transaction
   включена в block. EVM state/events откатываются, resource work — нет.
5. На 601-м per-tx entry reserve не выполняется и возвращается `TransactionLimitExceeded`; предыдущие attempts
   этой включённой reverted transaction остаются в block counter.
6. Если transaction помещается в empty block, но не в оставшийся block budget, proposer получает
   `BlockCapacityExhausted`, полностью исключает speculative transaction и восстанавливает block counter к
   pre-transaction checkpoint. Transaction остаётся eligible для будущего блока.
7. Validator не создаёт receipt для block-capacity overflow предложенного payload: первый entry, который не
   может reserve block slot, делает весь block invalid.
8. Receipt-visible system paths используют те же counters. Их overflow является block-build/integration error;
   deterministic bulk work разбивается по блокам.

## #16: SMT использует основной Reth MDBX environment или отдельный CE-owned environment?

Blocked by: #9

Type: Discuss

### Question

Оба варианта сохраняют atomic nodes+marker transaction, но различаются integration и concurrency:

1. Таблицы в основном Reth MDBX environment: один DB lifecycle и writer serialization, но требуется расширить
   статическую Reth table schema и учитывать contention с engine persistence.
2. Отдельный CE-owned MDBX environment: private schema, независимый writer и более глубокий module boundary;
   Reth→SMT crash ordering остаётся cross-environment и уже проверяется Q9 restart matrix. Рекомендуется.

Нужно выбрать environment до реализации persistent provider и до Q11 benchmark, поскольку layout и writer
contention влияют на acceptance evidence.

### Answer

Решение владельца 2026-07-12: выбран вариант 2 — отдельный CE-owned MDBX environment внутри node datadir.

1. Canonical location: `<datadir>/compressed_entities/smt/`. Это отдельный MDBX environment, а не tables
   основного Reth database environment.
2. CE module полностью владеет table schema, migrations, map-size/capacity checks, writer lifecycle и
   read-transaction discipline. Reth static table schema не расширяется.
3. Один CE writer атомарно коммитит changed nodes, shard/top metadata и complete `last_applied` marker.
   Concurrent proof readers используют snapshot isolation и одну read transaction на proof package.
4. Reth и CE MDBX не образуют distributed transaction. Безопасность обеспечивает порядок `Q14`:

   ```text
   durable Reth checkpoint H
     → atomic CE MDBX commit H
     → Marshal ACK H
   ```

5. Environment identity metadata связывает базу как минимум с `{chain_id, genesis_hash,
   commitment_scheme_version}`. Wrong-chain/wrong-scheme datadir reuse отклоняется fail-fast.
6. Directory участвует в node datadir lifecycle, backup/restore и disk-capacity monitoring, но перенос одних
   CE files без соответствующего Reth checkpoint не считается согласованным full-node backup.
7. MDBX layout, map-size, write amplification и writer/proof-read concurrency входят в обязательный Q11
   benchmark и Q13 crash/soak evidence.

## #17: Что в snapshot protocol-normative, а что manifest-local?

Blocked by: #10

Type: Discuss

### Question

Multi-source resume и перенос состояния требуют разделить semantic snapshot и его физическую упаковку:

1. Protocol-normative chunk function: каждый producer для одного checkpoint строит одинаковые boundaries и IDs.
   Максимальная cross-producer interoperability, но snapshot transport становится сложным versioned protocol.
2. Manifest-local content-addressed chunks: один выбранный manifest фиксирует boundaries, sizes, checksums и
   content IDs; mirrors/object stores обслуживают chunks именно этого manifest. Другой producer может создать
   другой manifest для того же checkpoint.
3. Protocol-normative semantic snapshot с canonical logical ranges, но manifest-local chunk packaging.
   Независимые producers обязаны представлять одно логическое состояние и давать одинаковый результат импорта,
   но MDBX pages, compression и физические chunk boundaries могут различаться. Cross-producer продолжение
   работает по логическим ranges; byte-chunk resume — между mirrors одного manifest. Рекомендуется.

Во всех вариантах receiver независимо проверяет finalized root и semantic tree/body data; manifest не является
trust root и не доказывает body completeness.

### Answer

Решение владельца 2026-07-12: выбран вариант 3 — versioned protocol-normative semantic snapshot с canonical
logical ranges и implementation-local physical materialization.

### 17.1 Нормативный semantic invariant

Для одного finalized checkpoint `H`, snapshot profile и `body_coverage` корректный импорт обязан дать:

```text
import(snapshot(H, profile, body_coverage)) =>
  тот же canonical set (tree_key, leaf_value)
  те же collection shard roots, collection roots и Root Catalog
  тот же R_sealed(H)
  те же canonical current bodies в объявленной body_coverage
```

Этот инвариант не требует одинаковых MDBX pages, insertion order, map size, allocator history, compression,
container bytes или локальных secondary indexes.

Snapshot header нормативно содержит как минимум:

```text
snapshot_format_version
profile
body_coverage
chain_id
genesis_hash
height
block_hash
commitment_scheme_version
R_sealed(H)
```

Receiver выбирает finalized `{height, block_hash, R_sealed}` независимо от snapshot source. Snapshot, manifest,
producer signature или checksum не являются trust root и не заменяют finality verification.

### 17.2 Нормативная логическая схема

1. `snapshot_format_version` фиксирует record types, field widths, byte order, canonical encoding, uniqueness,
   ordering и import semantics. Unknown version/profile/scheme отклоняются fail-closed; silent downgrade запрещён.
2. Tree truth переносится как canonical logical records, независимо от MDBX page format. Минимальная
   нормативная запись leaf содержит `{collection_key, shard_index, tree_key, leaf_value}`. Records упорядочены
   по payload kind, collection, shard и key; shard обязан совпадать с derived index; duplicates, conflicts,
   non-canonical encoding и out-of-order input
   отклоняются.
3. Persistent internal SMT nodes являются derived materialization. Их можно включить как versioned acceleration
   section, но importer обязан иметь возможность отбросить их и пересобрать состояние из normative logical
   records. Acceleration section не может заменить проверку итоговых shard roots и `R_sealed(H)`.
4. Canonical current-body record содержит domain identity, canonical ID/body bytes и ожидаемый `tree_key` /
   `leaf_value`. Каждый импортированный или возвращаемый body проверяется против соответствующего leaf.
5. Canonical logical ranges адресуются как минимум через
   `{profile, payload_kind, collection_key, shard_index, start_key, end_key}`.
   Producer возвращает записи в canonical order и явный continuation boundary. Это позволяет receiver повторить
   подозрительный или отсутствующий range у другого producer, даже если физические manifests различаются.

### 17.3 Profiles и body completeness

1. `tree` profile содержит полный current leaf set, достаточный для восстановления всех shard roots и
   `R_sealed(H)`; импорт активируется только после проверки reconstructed root против independently selected
   finalized header.
2. `tree-with-bodies` переносит body records вместе с tree и нормативно объявляет `body_coverage` как набор
   canonical logical ranges. Внутри каждого объявленного range importer требует ровно один canonical body для
   каждого present leaf. Другая coverage означает другой snapshot identity. Сам manifest по принятому `Q10` не
   является consensus proof глобальной custody; bodies вне coverage могут отсутствовать и давать `unavailable`.
3. Реализация может определить versioned `full-current-body` operational profile. В нём exporter и importer
   выполняют одноразовый streaming merge leaf↔body и требуют ровно один canonical body на каждый present leaf.
   Это `O(N)` snapshot export/import check, а не постоянный startup/readiness scan и не consensus predicate.
4. Partial/lazy body bundles имеют отдельные profile/coverage/identity и не обещают готовую full-body service node.

### 17.4 Manifest, chunks и multi-source

1. Manifest фиксирует конкретный transport artifact: checkpoint/profile/body_coverage, ordered chunk descriptors, logical
   range каждого chunk, encoded/uncompressed sizes, checksums/content IDs и total coverage metadata.
2. Chunk boundaries, compression, container и file names могут быть manifest-local. Content identity считается
   по canonical decoded logical payload; transport compression не меняет semantic identity.
3. Byte-level resume и произвольное смешивание chunks гарантируются между sources, которые обслуживают один
   manifest. Независимый producer может иметь другой manifest; переключение между такими producers выполняется
   по canonical logical ranges и continuation keys, а не по номеру физического chunk.
4. Receiver импортирует во staging и не активирует snapshot при duplicate/overlap/gap, malformed range,
   checksum mismatch, resource-limit violation, checkpoint mismatch или reconstructed-root mismatch.
5. Parser и transport имеют bounds на manifest entries/bytes, encoded и decoded chunk size, record length,
   temporary disk, concurrency и decompression ratio. Chunk identifiers никогда не интерпретируются как paths.

### 17.5 Перенос datadir

Raw MDBX/datadir copy является отдельным node-local backup/relocation path, а не network snapshot format.

1. Physical clone создаётся только из остановленной node либо через database-native consistent checkpoint.
2. Full-node bundle связывает на одном `H` durable Reth checkpoint и CE tree marker; body materialization либо
   копируется с собственным high-water, либо восстанавливается из canonical events/snapshot после него.
3. Перенос только `<datadir>/compressed_entities/smt/` без соответствующего Reth checkpoint не является
   согласованным full-node backup согласно `Q16`.
4. Validator private keys, signer state, node identity, locks и ephemeral caches не входят в portable snapshot.
   Запуск исходного и клонированного signer с одним ключом должен предотвращаться operational double-sign guard.

Snapshot остаётся non-consensus recovery carrier: malformed или incomplete artifact ломает только локальный
bootstrap и не может изменить finalized network state.

Обязательные acceptance tests:

1. Независимые exporters одного checkpoint/profile/body_coverage → одинаковые logical records/ranges и одинаковый root после
   импорта, даже при разных manifests/compression/MDBX page sizes.
2. Multi-source range failover, manifest-local byte resume и повторная загрузка corrupted range.
3. Omission, duplicate, overlap, reorder, conflicting body, malformed codec, downgrade и decompression-bomb
   rejection.
4. Staging import не активируется до checkpoint/root verification; crash/restart до и после activation atomic.
5. Consistent full-datadir relocation и отклонение mismatched Reth/CE checkpoints.

## #18: Какие concrete Fr values закрепляются за commitment-scheme-v1 `TAG_*`?

Blocked by: #7

Type: Discuss

### Question

Структура commitment scheme закрыта в `Q7`, но activation-complete таблица concrete non-zero BN254 `Fr`
constants отсутствует. Нужно нормативно закрепить значения для:

```text
TAG_BYTES_INIT
TAG_BYTES_ABSORB
TAG_BYTES_FINAL
TAG_ID
TAG_KEY
TAG_BODY
TAG_LEAF
TAG_SMT_BASE
TAG_SMT_NORMAL
TAG_SMT_ZERO
TAG_TOP_NODE
TAG_SEALED_ROOT
```

Варианты:

1. Явные последовательные small non-zero field integers с отдельными reserved ranges для v1 и будущих
   extensions. Рекомендуется: минимальная реализационная/circuit complexity, простая проверка таблицы и отсутствие
   дополнительного hash-to-field primitive. Безопасность требует distinct fixed tags, а не случайность.
2. Выводить каждый tag как hash-to-field от normative ASCII label. Даёт self-describing derivation, но требует
   отдельно заморозить hash function, label bytes, endian/reduction/rejection rules и golden vectors.
3. Зафиксировать случайно выбранные canonical Fr values. Криптографического преимущества для domain separation
   не даёт, ухудшает auditability и воспроизводимость выбора.

Независимо от варианта таблица должна:

1. содержать canonical 32-byte big-endian encoding и human-readable integer каждого tag;
2. запрещать `ZERO`, duplicates и caller-supplied values;
3. определить namespace/reservation policy для новых object/node tags без silent reuse;
4. быть единственным нормативным источником, который зеркалят Rust constants, circuits и golden vectors;
5. зафиксировать empty shard/top/`R_sealed(0)` vectors после принятия значений.

### Answer

Решение владельца 2026-07-12: выбран структурированный sequential namespace без runtime hash-to-field.

```text
CES1_TAG_BASE = 0x4345533100000000 = 4847372043852709888
TAG(tag_id)   = Fr(CES1_TAG_BASE + tag_id)
```

`0x43455331` кодирует ASCII `CES1`. Принятое распределение IDs:

```text
1  TAG_BYTES_INIT       7  TAG_LEAF
2  TAG_BYTES_ABSORB     8  TAG_SMT_BASE
3  TAG_BYTES_FINAL      9  TAG_SMT_NORMAL
4  TAG_ID              10  TAG_SMT_ZERO
5  TAG_KEY             11  TAG_TOP_NODE
6  TAG_BODY            12  TAG_SEALED_ROOT
13 TAG_COLLECTION_KEY  14  TAG_COLLECTION_ROOT
```

Полные decimal values и canonical 32-byte big-endian encodings нормативно перечислены в §4.2
`compressed_entities_concept_v6_proposed_10-07-2026.md`.

1. ID `0` не назначается; stock zero-`Fr` Poseidon находится вне CES1 namespace и запрещён для CES1 hashes.
2. По Q23 IDs `13/14` назначены `TAG_COLLECTION_KEY/TAG_COLLECTION_ROOT`; IDs `15..=65535` зарезервированы для
   явно fork-activated CES1 extensions; reservation сама по себе не
   разрешает использование нового tag без normative registry update.
3. Assigned IDs/values immutable и не переиспользуются. Изменение существующего tag требует нового
   `commitment_scheme_version`; future scheme определяет собственный namespace.
4. Все CES1 tags distinct, non-zero и `< 2^64 < p`; field reduction не используется.
5. Таблица §4.2 является единственным normative source. Rust/circuit constants и golden vectors только
   зеркалят её и обязаны проверять exact equality.

## #19: Где именно genesis коммитит `R_sealed(0)`?

Blocked by: #18

Type: Discuss

### Question

`Q12` требует deterministic genesis root, а обычные блоки имеют два carriers: EVM
`last_sealed_root` и header artifact tag `0x08`. Нужно определить genesis exception или подтвердить оба carriers,
иначе genesis hash и block-1 parent-root verification допускают разные реализации.

Варианты:

1. Seed canonical `last_sealed_root = R_sealed(0)` в `0xEE0B` genesis alloc; block 0 не содержит tag-`0x08`
   artifact, а normative empty root также закреплён/проверяется chainspec genesis configuration. Header artifact
   начинается с block 1. Рекомендуется: genesis state root уже коммитит slot, light clients и block-1 startup
   имеют chainspec anchor, а существующий genesis header не требует нового artifact envelope special case.
2. Seed EVM slot и включить tag-`0x08` artifact в genesis header. Даёт одинаковые carriers для всех heights,
   но требует определить genesis artifact envelope/extra-data encoding и меняет genesis hash.
3. Не seed-ить carriers, а только выводить empty root в памяти и special-case block 1. Не рекомендуется: EVM slot
   в block 1 читает `ZERO`, расходится с lag-1 semantics и создаёт дополнительную consensus ветку.

Нужно также закрепить:

1. exact genesis storage slot/layout и EIP-161 marker для `0xEE0B`;
2. проверку genesis initialization: derived root, seeded slot и chainspec constant обязаны совпасть;
3. область правила §9.3: либо оба carriers обязательны только для `B >= 1`, либо tag `0x08` обязателен и в genesis;
4. block-1 parent-root source и fail-closed behavior при mismatch;
5. одинаковую процедуру для empty genesis и будущего genesis с preloaded entities.

### Answer

Решение владельца 2026-07-12: выбран вариант 1 — genesis коммитит `R_sealed(0)` через EVM state, но не имеет
tag-`0x08` header artifact.

1. Нормативный `0xEE0B` storage layout v1:

   ```text
   slot 0  storage_schema_version = 1
   slot 1  last_sealed_root = R_sealed
   slot 2  pending entity map base
   slot 3  touched_entities StorageVec base
   slot 4  pending retired-collection map base
   slot 5  touched_collections StorageVec base
   ```

2. Genesis alloc содержит account `0xEE0B` с EIP-161 marker bytecode `0xef`, `slot 0 = 1` и
   `slot 1 = R_sealed(0)`. Pending map и touched vector пусты. Адрес также входит в runtime marker set.
3. `R_sealed(0)` не является независимо настраиваемым root: node детерминированно выводит его из Q18 tags,
   commitment scheme v1 и canonical genesis entities, затем требует exact equality с seeded slot и genesis
   chainspec/state. Mismatch отклоняет genesis/startup fail-closed.
4. Genesis `extra_data` остаётся пустым. Правило двух carriers из §9.3 действует только для executed blocks
   `B >= 1`; tag `0x08` и соответствующий artifact-envelope version начинаются с block 1.
5. CE-owned MDBX инициализируется genesis checkpoint:

   ```text
   last_applied = {
     commitment_scheme_version: 1,
     height: 0,
     block_hash: genesis_hash,
     parent_block_hash: ZERO,
     parent_root: ZERO,
     new_root: R_sealed(0)
   }
   ```

   `ZERO` parent fields разрешены только для height 0. Missing local CE genesis database может быть
   детерминированно пересоздан из genesis specification и снова проверен против slot/genesis hash.
6. Block 1 начинает execution с EVM slot и CE marker, равными `R_sealed(0)`. End-block вычисляет
   `R_sealed(1)`, записывает его в slot 1 и включает tag-`0x08` artifact. Validator требует совпадения computed
   root, slot и artifact; parent marker обязан иметь `{height: 0, block_hash: genesis_hash, root: R_sealed(0)}`.
7. Light client для height 0 использует trusted chainspec/genesis hash и normative derived `R_sealed(0)`;
   для `H >= 1` использует обычный tag-`0x08` header carrier.
8. Если future genesis содержит entities, они canonical-sort-ятся, строят genesis SMT/current bodies и тот же
   initialization pipeline; synthetic events/migration не используются.
9. Genesis acceptance vectors обязаны покрывать empty и preloaded cases, slot/layout/marker, отсутствие tag
   `0x08` в block 0, block-1 parent check и deterministic CE MDBX rebuild.

## #20: Как `absent` proof связывается именно с запрошенным ID?

Blocked by: #6, #7

Type: Discuss

### Question

Нужно исключить ситуацию, в которой RPC выбирает другой ID encoding, выводит другой `tree_key` и возвращает
валидный non-membership proof не для того `{domain_id, raw_id}`, который запросил клиент.

Также нужно сохранить различие:

```text
absent       выбранный current root доказывает отсутствие запрошенного key
unavailable  key присутствует, но конкретная node не располагает проверяемым body
```

### Answer

Решение владельца 2026-07-12: non-membership proof сохраняется, а identity derivation не доверяет RPC.

1. По Q23 для `outbe_getBody(domain_id, partition_key?, raw_id, height?)` authoritative identity — точные
   `{domain_id, partition_key_or_none, raw_id}` из
   запроса клиента. Response обязан повторять их без изменения.
2. Verifier получает конкретный `id_encoding_kind_u8` из fork-active domain registry на выбранной proof height.
   Exact byte assignment является частью domain activation specification и immutable для этой domain version.
3. Verifier сам выводит `id_bytes`, `tree_key` и shard index. RPC/package не выбирает encoding kind, canonical
   ID, tree key или shard.
4. Point-proof package содержит `raw_id`, а не альтернативу `raw_id_encoding + raw_id or canonical id_bytes`.
   Любое дополнительно транспортируемое derived identity поле является только redundancy и отклоняется при
   несовпадении с derivation verifier-а.
5. Для offline proof expected `{domain_id, raw_id}` передаётся verifier-у отдельно. Без independently supplied
   expected identity package доказывает утверждение только о том ID, который записан в самом package, и не может
   использоваться как доказательство про другой requested ID.
6. `absent` возвращается только после успешной проверки non-membership для независимо выведенного `tree_key`
   против independently selected finalized root. Missing/stale body или proof для другой identity является
   `unavailable`/invalid, но не `absent`.
7. Решение не меняет mutation, SMT или root computation. Оно закрывает trust boundary внешнего proof/RPC слоя.
8. Acceptance suite включает wrong-ID substitution, RPC-selected encoding, mismatched redundant `id_bytes` и
   proof-for-another-key cases.

## #21: Как физически кодируются `Untouched`, `Set` и `Deleted` в pending map?

Blocked by: #5, #8

Type: Discuss

### Question

Pending map использует один EVM storage word на touched key. Нужно однозначно отличить transient delete от
настоящего `leaf_value`, не вводя второй discriminant slot и не создавая permanent tombstone.

### Answer

Решение владельца 2026-07-12: выбран single-slot out-of-field sentinel.

```text
0                         = Untouched
1 <= word < p             = Set(BE32(word))
U256::MAX                 = Deleted
p <= word < U256::MAX     = invalid
```

`p` — modulus BN254 scalar field.

1. Valid present `leaf_value` является canonical non-zero `Fr`, поэтому всегда удовлетворяет
   `1 <= leaf_value < p`.
2. `U256::MAX` находится вне field и не может совпасть с настоящим leaf. Отдельный discriminant slot не нужен.
3. Overlay read интерпретирует `0` как parent-tree fallback, `1 <= word < p` как pending `Set`, а `U256::MAX` как
   current absence после pending delete. Любое иное word отклоняется fail-closed.
4. End-block seal превращает `Set(v)` в `SMT.update(tree_key, v)`, а `Deleted` — в
   `SMT.update(tree_key, ZERO)`.
5. После построения staged batch pending slot зануляется до final state-root calculation. Sentinel не попадает
   в SMT, не остаётся в finalized EVM state и не резервирует 32 bytes навсегда.
6. Nested/full-transaction revert использует обычный EVM journal и откатывает sentinel вместе с mutation event.
7. Golden vectors и adversarial tests покрывают все четыре classes, границы `p-1`, `p`,
   `U256::MAX - 1`, `U256::MAX`, same-key set/delete sequences и cleanup post-condition.

## #22: Как `BlockLifecycle::end_block()` возвращает CE staged batch executor-у?

Blocked by: #8, #9

Type: Discuss

### Question

CE end-block seal одновременно изменяет journaled EVM state и создаёт provisional `staged_tree_batch`, который
executor должен сохранить только после успешного finish/sealing. Текущий unit-return contract
`end_block(ctx) -> Result<()>` не может передать этот typed output.

### Answer

Решение владельца 2026-07-12: общий `BlockLifecycle` получает associated typed end-block result.

```text
trait BlockLifecycle {
  type EndBlockResult

  begin_block(ctx: &BlockRuntimeContext) -> Result<()>
  end_block(ctx: &BlockRuntimeContext) -> Result<EndBlockResult>
}

ordinary lifecycle module:
  EndBlockResult = ()

CompressedEntitiesLifecycle:
  EndBlockResult = SealOutput { R_sealed, staged_tree_batch }
```

1. Все lifecycle modules продолжают использовать единый `BlockRuntimeContext` и explicit hard-fork-governed
   executor ordering. Отдельный CE block-boundary API не создаётся.
2. Associated type позволяет common primitives trait не зависеть от CE-specific `SealOutput`. Executor вызывает
   concrete `CompressedEntitiesLifecycle` и знает его concrete result type.
3. CE seal выполняется последним lifecycle step после остальных end-block modules и до state-root calculation.
4. Executor получает `SealOutput` обычным return value и держит его в локальной переменной. Global registry,
   type erasure, side channel и mutable output holder внутри `BlockRuntimeContext` отсутствуют.
5. После успешных executor finish и block sealing executor присваивает `block_hash` и публикует staged batch в
   speculative cache по этому hash.
6. Ошибка tree preparation, root comparison, cleanup, state flush, executor finish или sealing уничтожает
   локальный `SealOutput`; batch не публикуется и частично sealed block не существует.
7. Proposer и validator вызывают один lifecycle contract и имеют golden tests на одинаковые result/root,
   successful handoff, drop-before-publication и publish-after-block-hash ordering.

## #23: Как collections, optional partitions и per-collection shards образуют один `R_sealed`?

Blocked by: #6, #7, #8, #10

Type: Discuss

### Question

Нужно заменить ошибочно понятый global-256-shard layout на модель, где domain имеет собственную collection,
Tribute разделяет её по WWD, каждая collection независимо shard-ится для parallelism/locality, а все collection
roots агрегируются в один network root. Также нужно определить атомарное удаление partition и reuse policy.

### Answer

Решение владельца 2026-07-12:

1. Domain version фиксирует `partition_policy`, canonical partition-key derivation,
   `collection_shard_count = 2^k` и retirement policy.
2. `partition_key: Option<CanonicalBytes>`: `None` означает singleton collection domain-а; `Some(key)` —
   отдельную partition collection. Presence и byte length входят в `collection_key`, поэтому `None` не
   совпадает с `Some(empty)`.
3. Tribute v1 использует `partition_key = tribute_id[0..4]`, canonical big-endian `wwd_id: u32`; core и verifier
   выводят/проверяют его. Nod/Gem v1 используют `None` и имеют по одной singleton collection.
4. `collection_key = PBytes(TAG_COLLECTION_KEY; domain_id || presence || len || partition_key)`, а entity
   `tree_key` связывает `collection_key` и canonical ID. Caller/RPC не выбирает derived collection/shard.
5. Каждая collection имеет `K_domain = 2^k` независимых shard SMT; `shard_index = low_k_bits(tree_key)`.
   Все collections одной domain version используют одинаковый K. Конкретные K выбирает Q11 benchmark;
   изменение K существующего state требует migration/new commitment scheme.
6. Fixed binary Poseidon top над shard roots даёт
   `R_collection = P(TAG_COLLECTION_ROOT; scheme, collection_key, K_domain, top_shard_root)`.
7. Dynamic Root Catalog SMT хранит `collection_key → R_collection`.
   `R_sealed = P(TAG_SEALED_ROOT; commitment_scheme_version, catalog_root)`; только он записывается в EVM/header.
8. Entity proof: shard SMT proof → collection top proof → `R_collection` → Root Catalog proof → `R_sealed`.
   Если collection retired/absent, Root Catalog non-membership уже доказывает отсутствие любого её entity.
9. End-block группирует entity mutations по collection/shard, независимо готовит touched shard batches,
   детерминированно пересчитывает collection roots, затем одним catalog batch получает `R_sealed`.
10. `retire_partition(domain_id, partition_key)` — domain-authorized generic operation: удаляет collection leaf
    из Root Catalog одной mutation, делает все entity partition absent, эмитит один canonical
    `CompressedEntityPartitionRetiredV1` event и позволяет ExEx range-delete current rows.
11. Retired partition key имеет permanent non-reuse. Core не хранит tombstone; registered domain lifecycle
    обязан обеспечить lifetime uniqueness. Для Tribute закрытый WWD никогда не открывается повторно.
12. Physical collection shard namespaces удаляются только после finality. Исторические canonical events остаются
    согласно retention policy; их отсутствие не меняет current-root correctness.
13. Один CE MDBX environment хранит namespaced `{collection_key, shard_index, node_key}` trees, collection roots,
    Root Catalog и atomic `last_applied` marker. ExEx не строит и не изменяет SMT.
14. Q23 supersedes Q7 только в placement/aggregation shards: Poseidon/CKB codecs и header carrier сохраняются;
    global fixed 256 shards и eight-sibling top proof больше не являются v1 invariant.
