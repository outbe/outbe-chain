# P0: проверенные исходники и открытый контракт приёма

Исторический журнал первого прохода. Выбранный candidate контракт теперь в `design.md`;
открытые варианты ниже сохранены как история исследования, а не текущая спецификация.
Статус P0: **candidate подготовлен, task не закрыта**. Это не approved design и не evidence
реализации P1–P8. Рабочая база: `42d08693a06a7fdc088cf452b582c2d18596ed9a`.
Задача Beads: `outbe-chain-pjz.1`, `in_progress`.

## Выполненная проверка

В `evidence/outbe-chain-pjz.1/source-audit.json` сохранены 33 проверенных source anchors,
их реальные номера строк и SHA-256 файлов. Проверены **закреплённые** Commonware sources:
`d476a2361ce6840d2b9d0aa6fb30a924429046d4` из root Cargo.lock, а не другая checkout
или одноимённый registry package. Скрипт `p0-check/audit_sources.py` разрешает путь
через Cargo cache и проверяет Git revision без загрузки пакетов для ненужных target platforms.

Поиск общей связи ключей дал 141 candidate source file. Это перечень мест для
классификации и caller audit, а не требование механически менять все эти файлы.
Сетевые Ed25519 типы должны сохраняться.

### Подписываемые сообщения и фактический RID

| ID source anchor | Проверенный факт | Следствие для реализации |
| --- | --- | --- |
| S05 | `Identity::id()` возвращает `self.root().blob.into()` | RID — OID начального canonical identity document blob; не OID COB/Git commit |
| S06 | `Doc::sign()` подписывает `Doc::encode()` blob OID | Identity signature проверяется отдельно от подписи COB entry |
| S07–S08 | COB backend подписывает tree OID; verifier проверяет `Entry.revision` | Это не code commit, PatchId или пользовательский Patch RevisionId |
| S09–S10 | Sigrefs подписывает `Refs::canonical()` bytes | Нельзя вместо native refs bytes подставить произвольный Git OID |
| S14–S15 | Review доступен обычному actor; Merge имеет authorization/branch/ancestry/threshold checks | Review Accept сам по себе не устанавливает `State::Merged` |
| S16 | Commit writer берёт `repo.signature()` и локальное время; есть env override даты | Общая tree signature не гарантирует одинаковые COB commit IDs на коллекторах |

Нужна явная production prepare/attach/publish форма, где для сетевых jobs фиксированы
author, committer, timestamps, sorted parents/related, resource, message/trailers и tree.
Нельзя использовать process-global `GIT_COMMITTER_DATE` как механизм координации.
Existing Ed25519 user workflow должен сохранять свои форматы и обычное время.

### Связь репозитория и транспортной identity

| Anchors | Наблюдение |
| --- | --- |
| S01–S04 | Signer связан с конкретными Ed25519 types; DID оборачивает тот же PublicKey; SSH envelope поддерживает Ed25519 |
| S11 | Storage `RemoteId = PublicKey` |
| S12–S13 | PublicKey wire decoder читает ровно 32 байта; Noise использует `crypto::SigningKey` |
| S28 | Git metadata signature parser различает PGP/SSH; BLS нельзя просто объявить SSH Ed25519 signature |

Repository key/actor/namespace должен поддерживать Ed25519 и BLS отдельно от Ed25519
transport NodeId. Изменение только trait Signer оставит broken storage/fetch/wire paths.
Нужен BLS-specific versioned envelope; старые Ed25519 bytes сохраняются. Новый writer
не должен создавать данные, которые старый reader ошибочно примет за Ed25519.

### Vote и effective consensus membership

| Anchors | Проверенный факт |
| --- | --- |
| S17–S18 | castVote и tally сейчас используют lifecycle ACTIVE |
| S19 | `get_active_consensus_set()` использует current-consensus predicate; включает Exiting и JailRetained до boundary exclusion |
| S20 | `current_epoch_u64()` и `active_consensus_set_hash()` доступны в state queries |
| S21 | Radicle manager читает точное `state_by_block_hash(finalized.hash)` |
| S27 | Patch VoteTarget отсутствует в production registry |

Нельзя менять все Vote targets на effective consensus set: у этого есть изменение
семантики unrelated proposals. Для patch потребуется target-specific electorate seam
на cast и tally. Рабочий кандидат electorate — ACTIVE ∩ effective consensus set.
Подписи исполнения используют current effective DKG players; голосовавший No не
запрещён как добросовестный исполнитель Approved. Два quorum не обязаны совпадать:
existing Vote использует своё правило, DKG N3f1 — `n - floor((n-1)/3)`.
Финальный выбор и точный hook ещё должны быть записаны в design и проверены по AC P0.

### DKG и криптографическая осуществимость

S22–S24: individual MinPk identity signatures и MinSig threshold VRF — отдельные пути.
Existing `VrfMaterialProvider::sign_seed()` не является готовым repository signer.
Canonical DKG manager принимает chain-finalized dealer logs, в отличие от P2P gossip.

В exact Commonware revision C01–C05 проверены:

- Threshold `sign_message`, `verify_message`, `recover` доступны для arbitrary bytes
  и namespace. Сам `recover` не заменяет проверку каждого partial перед учётом.
- MinSig: public G2 = 96 bytes, signature G1 = 48 bytes;
  DST `BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_POP_`.
- Message API добавляет Commonware `union_unique(namespace, message)`. Verifier должен
  воспроизводить этот framing, а не просто вызывать blst над raw message.
- DKG `Info::new` различает initial DKG (`previous=None`) и reshare. Reshare сохраняет
  group key, меняя распределение shares. Namespace/round должны быть отдельными для
  repository authority; VRF ceremony/state нельзя незаметно переиспользовать.

Исполнен standalone `p0-check` на exact Git revision Commonware. Fixture действительно
выполняет pairing/sign/recovery, не подменяет BLS mock. Проверены 3-of-4, все четыре
подмножества по три участника, t-1, дубликаты, wrong index, другой public key,
wrong domain и altered subject. Public vectors сохранены в `threshold-check.log`.
`deal_anonymous` здесь **только fixture primitive check**, не реализация distributed DKG
или proof готового production signer.

## Критический открытый контракт: старые shares и получатель

Эксперимент подтверждает: математически правильная signature продолжает проверяться
старым public key. Добавить epoch в sidecar state/packet или отфильтровать отправителей
только в честном collector недостаточно для запрета нового подписанного объекта.
Получатель обязан отличать validity подписи от authorization нового mutable transition.

Требования одновременно:

1. Heartwood поддерживает BLS как общий алгоритм, не разбирает Vote/Outbe chain proposals.
2. У repository стабильный RID и network-owned authority, без единоличного owner key.
3. Новое действие допускают только полномочные current participants.
4. Исторические уже принятые объекты продолжают проверяться.
5. Смена набора не требует присутствия старого operator owner и не останавливает consensus.

Нужно доказать один конкретный приёмный контракт, а не выбрать название типа.
Ни один следующий кандидат пока не объявлен готовым решением:

### A. Fresh key каждой generation и authenticated authority binding

Fresh independent DKG даёт другой public key и криптографически отвергает старую
signature под новым key (primitive check это подтверждает). Но native DID/delegates
потребуют аутентифицированного key transition. Нельзя принять новую self-signed identity
или переименовать отвергнутую пользователем OutbeNetwork authority в другой тип.
Нужно решить native delegation/rotation, freshness независимого receiver и доступность,
если old committee уже ушёл. RID сохраняет initial document; new doc не меняет RID.

### B. Stable native BLS owner и проверяемый publication checkpoint

Native owner остаётся обычным BLS key; repository shares отдельно reshared. Вне
generic crypto Outbe policy разрешает публикацию только по финализированному checkpoint
точного native object/refs package. Такое разрешение должно доказывать вклад current
participants и связывать package hash, repo, patch/revision, epoch/material, expected
previous head и sequence. Старый group signature без такого checkpoint не даёт нового
canonical write. Повтор уже зафиксированного пакета является историческим replay.

Возможная конструкция для проверки: current validator identity подписи удостоверяют
его current-polynomial partials и exact package; chain module проверяет current
membership, partials и quorum и сохраняет immutable receipt. Это **не** замена native
threshold BLS signatures агрегатом individual signatures: native signatures по-прежнему
восстанавливаются из shares. Дополнительная current-member certificate защищает право
публикации; raw BLS по постоянному key сам по себе не делает revocation.

Открытые обязательные проверки этого кандидата:

- Где receiver применяет fail-closed policy перед canonical refs/identity promotion,
  включая fetch/cache/restart, сохраняя generic Heartwood без знания Vote?
- Как bound каждого сообщения/криптографической проверки укладывается в production gas
  для `config_max_validators`, а не только для fixture из четырёх узлов? При необходимости
  bounded per-contribution calls вместо одного неограниченного certificate call.
- Как finalized receipt препятствует race «собрали -> committee сменился -> опубликовали»,
  включая fresh independent verifier и позднюю публикацию уже authorized history?
- Как chain module аутентифицирует repository DKG polynomial/output, не доверяя
  RPC/sidecar/first registrant, и как формируется deterministic initial RID после DKG?
- Как новые authority/bootstrap/receipt calls добавляются к RadicleRegistry и ABI,
  не меняя legacy slots 0–5 и не делая Git I/O внутри execution?

Нельзя закрыть P0, пока эти вопросы остаются открыты. Сравнение A/B должно закончиться
конкретным source-backed контрактом, полной картой P1–P8 и negative traces.

## Genesis и исполнения patch

S25–S26: registry сегодня permissionless, genesis seeder фиксирует только capacity.
Genesis должен сначала фиксировать descriptor/content/policy commitment. Если initial
BLS key неизвестен до DKG, initial identity blob и RID становятся известны после DKG;
затем нужен authenticated binding к descriptor. Ordinary `register_repository()` первого
вызвавшего не является таким binding. Факт RID от doc blob снимает зависимость от
commit timestamp, но не от содержащегося в doc BLS key.

Для execution необходимо зафиксировать в design:

- immutable repo/patch/revision и ожидаемую target branch/precondition;
- native Review Accept отдельно от branch update/Merge action/sigrefs;
- deterministic merge metadata и поведение target movement/conflict;
- durable ordering unsigned preparation -> verified partials -> recovered signatures
  -> authorized publication -> refs/announcement, recovery на каждой границе;
- exact subject/parent/context binding, чтобы tree signature не переносилась в другой repo/job.

## Источники вне checkout

- Commonware primary source: Git tag v2026.9.0, revision из root Cargo.lock;
  фактически использованный exact checkout указан в source-audit.json.
- https://datatracker.ietf.org/doc/html/draft-irtf-cfrg-bls-signature-07
  (Internet-Draft; наш фактический ciphersuite определяется pinned Commonware source).
- https://raw.githubusercontent.com/multiformats/multicodec/master/table.csv:
  `bls12_381-g2-pub` = `0xeb`, Ed25519 = `0xed`; varint prefixes `eb01` и `ed01`.

## Следующий проверяемый результат

Завершить receiver/current-authority контракт и genesis binding, записать `design.md`
без unresolved blockers, обновить карту P1–P8, подготовить evidence на reviewed commit.
Только затем открыть три P0 review children и получить три независимых full-AC PASS.
