# P0: контракт BLS repository и current-set publication

Статус: candidate для трёх независимых P0 reviews. Реализация P1–P8 ещё отсутствует.
База исследования: `42d08693a06a7fdc088cf452b582c2d18596ed9a`.
Source anchors S01–S28/C01–C05 разрешаются в `evidence/outbe-chain-pjz.1/source-audit.json`:
там exact paths, строки и SHA-256. `investigation.md` сохраняет первоначальные вопросы;
решения ниже заменяют его открытые варианты, но не объявляют production реализацию.

## 1. Выбранный контракт и доверие

Владелец governments — нативный **BLS key**, а не новый Outbe-specific вид repository.
Ed25519 остаётся для пользователей и Noise/NodeId. Heartwood не читает Vote, epoch
ValidatorSet или EVM state. Его криптографический verifier проверяет key/message/signature.

Разрешение нового canonical write в управляемом репозитории требует **двух** проверок:

1. Все native BLS identity/COB/sigrefs signatures корректны под repository owner key.
2. Outbe publication policy предъявила подтверждение, что exact package зафиксирован
   в chain state полномочным текущим набором после разрешённого решения.

Для второго условия вводится общий Heartwood interface `PublicationPolicy`: он получает
repository/expected previous refs/package manifest и разрешает или запрещает promotion.
Обычные репозитории используют существующие native правила. Для governments policy
передаёт `crates/system/radicle`, используя только аутентифицированные сведения своего
validating Outbe node. Внутри Heartwood нет ветки `if Vote Approved` или OutbeNetwork DID.

Policy gate охватывает local writes и fetch/canonical refs/identity/cache promotion.
Нельзя проверить только честный collector: получатель с пустым storage должен отклонять
неавторизованный package. Immutable objects разрешено хранить в quarantine; они не
влияют на canonical identity, patch evaluator или refs до проверки полного manifest.
Репозиторий помечается managed в доверенной локальной конфигурации из genesis descriptor;
peer, отсутствующая запись или неизвестная policy не могут включить permissive fallback.

Граница доверия: full validating node и его защищённый local UDS. Неаутентифицированный
RPC ответ или self-signed public key не создают publication authority. Самостоятельный
обычный Heartwood клиент может проверить BLS, но без Outbe policy не доказывает current-set
authorization. Второй независимый проверяющий в E2E — другой validating node + sidecar.

## 2. Алгоритмы, ключи и signing bytes

Используется pinned Commonware `d476a2361ce6840d2b9d0aa6fb30a924429046d4`, MinSig:
public key G2 compressed 96 bytes, signature G1 compressed 48 bytes, DST
`BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_POP_`. Message framing — ровно Commonware
`union_unique(namespace, message)`, не raw message для blst. Native namespace фиксирован:
`radicle.repository.bls.v1`; callers не выбирают namespace.

Existing `crypto::PublicKey`, `SigningKey` и NodeId сохраняют Ed25519 transport semantics.
Новый `RepositoryKey = Ed25519(existing key) | Bls12381G2([u8;96])` используется для DID,
COB ActorId, storage RemoteId/namespace, refs и repository authorization. Транспортный
peer не обязан владеть repository key, объекты которого доставляет.

DID/key format: existing Ed25519 `did:key` encoding неизменен; BLS использует multicodec
`0xeb` (canonical unsigned varint `eb01`) и compressed G2, base58btc multibase.
Unknown codec/length, noncanonical encoding, zero/infinity и неверный subgroup запрещены.
Ни public BLS key, ни BLS signature не кодируются как SSH Ed25519.

Нативные subjects (S05–S10):

| kind | native payload | metadata commitment |
| --- | --- | --- |
| 1 / identity | OID canonical identity document blob, 20 bytes | 32 zero bytes |
| 2 / COB | operation tree OID, 20 bytes | SHA256 canonical commit metadata без signature header |
| 3 / sigrefs | exact `Refs::canonical()` bytes | SHA256 previous sigrefs commit OID (zero для root) |

BLS message V1 имеет fixed framing:

```text
RDB1[4] || kind:u8 || repo_id[20] || policy_context[32] || metadata_hash[32]
        || native_length:u32be || SHA256(native_payload)[32]
```

Предел native payload — 1 MiB; длина проверяется до выделения памяти. Kind 1/2 требуют
ровно 20 байт. Для управляемых jobs `policy_context` — SHA256 от canonical Outbe job
context, описанного ниже; generic crypto не интерпретирует его. Для обычного static
BLS repo context нулевой. Managed policy отвергает нулевой/неожиданный context.

Envelope сохраняет все поля message и signature[48]. Для COB это отдельный versioned
`radicle-bls-sig` Git header; existing `gpgsig` SSH parser не переопределяется. Identity
action signature принимает tagged BLS envelope или legacy Ed25519 representation.
Sigrefs `/signature` хранит legacy 64 bytes либо envelope с magic `RDB1`; прочие
форматы не допускаются. Проверяющий восстанавливает message из actual object, не
доверяет дублированным payload/meta fields envelope. Все envelopes одного package
проверяются до canonical promotion.

Repository wire messages/namespaces получают versioned format для RepositoryKey.
Existing NodeId frames остаются 32-byte Ed25519. Unsupported peers получают явный
unsupported-format отказ для BLS, без misparse или downgrade; Ed25519 exchange сохраняется.

## 3. Откуда берутся shares и почему это не готовый arbitrary signer

Выбран **существующий authenticated DKG material**, отдельный policy-limited signer
interface и фиксированный новый namespace, а не второй несогласованный DKG.
Native owner key равен group public key initial network DKG. Это даёт один native DID
и стабильный RID при обычном resharing состава. Secret не реконструируется и не
передаётся sidecar. Repository signer возвращает только indexed partial signatures.

Обоснование допустимости: Commonware threshold signing принимает произвольные bytes
и namespace (C01); VrfMaterialProvider уже делает эту операцию под отдельным seed
namespace (S23). Публичные ключи и shares математически пригодны; API/authorization
ещё отсутствуют и реализуются в P3. Новый actor не принимает namespace или свободное
message от sidecar: он восстанавливает RDB1 из проверенного typed signing job.
Его prefix отделён от **всех** consensus/VRF namespaces через `union_unique`;
подпись Radicle не проверяется как seed signature и наоборот. Аргумент опирается на
обычную chosen-message security BLS с disjoint hash-domain inputs; один успешный тест
не называется самостоятельным криптографическим доказательством.

P3 добавляет узкий signer к shared versioned material provider; export `Share` в
public interface запрещён. Подписание выполняется вне block execution. Доступ к
provider имеет ограниченный critical section; ожидание approval/partials/publication
не держит consensus mutex. Архивный material не подписывает новые jobs.

В V1 repository привязан к immutable initial group key сети. Изменение **membership,
polynomial и generation** поддерживается; reset/fresh network group key с тем же
chain identity текущим reshare протоколом не является нормальным переходом. Если
входящий материал меняет group key, repository signing fail-closed/RuntimeDegraded,
а не переписывает RID или делегатов. Новый chain genesis создаёт новый repository.
Существующие group-key consistency проверки сохраняются. Это не запрещает static
BLS repository иметь собственный key; такой repo не использует Outbe managed policy.

## 4. Current set и authenticated publication checkpoint

Текущий набор определяется **при применении attestation в canonical execution state**:
ValidatorSet current epoch + `get_active_consensus_set()`, с явным mapping EVM address ↔
MinPk identity key ↔ DKG participant index. Порядок DKG players берётся из аутентифицированного
output, не из сортировки произвольного RPC списка. Snapshot reader проверяет соответствие
commitments и n/threshold/material generation в одном exact finalized block (S19–S24).
При несовпадении jobs приостанавливаются. Epoch, block height и material version — разные поля.

Signing threshold — `q = n - floor((n-1)/3)` pinned N3f1. Collector проверяет identity
каждого sender, индекс, membership, current polynomial partial public key, namespace,
полный message и duplicate index **до** сохранения и recovery. Partial frame содержит
message SHA256, chain/genesis, epoch/material hash/version, participant index и partial;
current MinPk identity подписывает fixed encoding frame в namespace
`outbe.radicle.partial.v1`. Это отдельный namespace от native BLS/VRF/publication. Запрос старой generation
не допускается. Смена current snapshot аннулирует незавершённую попытку.

После восстановления **всех настоящих threshold native signatures** каждый участник
независимо проверяет complete package, approval и q valid current partials; затем
подписывает `PublicationBody` своим current MinPk identity key в отдельном namespace
`outbe.radicle.publication.v1`. Это дополнительное доказательство актуальных участников,
а не замена threshold BLS owner signature агрегатом individual signatures.

```text
PublicationBody V1 (canonical fixed bytes, big endian):
  chain_id:u64, genesis_hash[32], repo_id[20], descriptor_hash[32],
  owner_bls_public[96], players_commitment[32],
  epoch:u64, material_version:u64, material_hash[32],
  finalized_anchor_height:u64, finalized_anchor_hash[32],
  sequence:u64, previous_receipt_hash[32],
  action:u8 (bootstrap=0, bind_material=1, review=2, merge=3), proposal_id:u256,
  patch_id[20], revision_id[20], expected_target_head[20],
  package_manifest_hash[32]
```

Bootstrap использует нулевые proposal/patch/revision и точный genesis descriptor.
BindMaterial также не является patch Vote: q current identity endorsements фиксируют
точный current DKG output hash/ordered players commitment, сохраняя owner key. Initial
bootstrap одновременно фиксирует initial material. После membership transition новые
jobs ждут BindMaterial checkpoint; honest attester сопоставляет body с canonical DKG
output, а не с присланным peer ключом. Это BFT authorization нового material текущим
набором, не доверие first registrant/RPC. Если chain уже содержит соответствующий
DKG output commitment, он дополнительно обязан совпасть. Review/Merge требуют terminal
Approved именно указанного repo/patch/revision.
Manifest перечисляет exact Git object type/OID/SHA256(bytes), desired ref updates
с old/new OIDs, native subjects/envelopes и canonical metadata. Manifest также содержит public DKG output или его content-addressed proof, сопоставленный
с authenticated material hash, и q distinct current partial frames для каждого native
subject. Receiver может независимо проверить и этот contribution proof, не доверяя
только итоговой owner signature. Publication identity signatures не входят в manifest.
Manifest entries сортированы по type/path/OID, partial frames по subject ordinal/index;
duplicates запрещены. Максимальный размер manifest — 1 MiB, число native subjects
не больше трёх (bootstrap Doc/COB/refs); Review и Merge содержат COB/refs. Сам certificate не входит в hash
package; circular self-signing отсутствует. Typed `action` и context связывают actual
native Review/Merge с chain permission; signature над произвольным code commit не подходит.

Body принимается в chain **по одной bounded identity attestation за раз**. Verified
distinct current MinPk signatures учитываются по `(repo, epoch, sequence, body_hash)`;
один participant не аттестует два body одной sequence. Ровно q действительных участников
завершают checkpoint. Проверяются current epoch/material commitment, `previous_receipt_hash`
и sequence CAS, Approved subject/step, descriptor/RID и непогашенная execution stage.
Окончательный receipt атомарно фиксирует manifest hash и новое chain-managed состояние.
Неуспешная/stale попытка не потребляет approval и не меняет refs.

Carrier — новый opt-in record tag `0x09` в `OutbeBlockArtifacts` (OART V0x0C для
включённой feature), **наряду с** existing consensus/DKG record, максимум одна attestation
за block. Tag 0x04 навсегда retired и не используется. Сохраняется общий 64 KiB budget,
reservations CE root/late-finalize и реальные codec/size-budget checks. Если attestation
не помещается, она остаётся в mailbox; DKG/mandatory artifacts не выкидываются. Для
стабильного committee конечный DKG заканчивается, после чего эти slots доступны.
Fair bounded очередь обрабатывает уже Approved jobs; bootstrap имеет приоритет.
V0x0B/legacy режим сохраняется при выключенной feature.
Это не требует operator EVM owner key или fee-wallet для automatic bootstrap. Любой
proposer берёт уже подписанные frames из bounded nonblocking mailbox; он не собирает
threshold, не ждёт sidecar и не выполняет Git I/O. Absence поля всегда допустимо.
DKG и Radicle не заменяют один и тот же enum slot: это два независимых optional record.
Begin-zone Radicle application идёт после committee boundary activation и Vote hooks,
с использованием effective execution-state set этого шага. Attester выбирает anchor
только из проверенной local finality; genesis anchor разрешён только для bootstrap.
Для остальных actions anchor state должен содержать permission/material. Node verifier
проверяет ancestry этого anchor и доступный authenticated finalized context до допуска
artifact; EVM storage handler не объявляет самостоятельно блок финализированным.
Verifier выполняет только bounded deterministic crypto/storage checks. Invalid artifact
означает invalid payload, не node-stop fatal; mismatch epoch после boundary не даёт state
effect. Activation flag/genesis descriptor включает новый format; legacy chains остаются
на прежнем формате. Это намеренное wiring P3/P5, а не существующая capability.

Receipt hash = SHA256(`OPB1` || exact fixed PublicationBody bytes). Attestation подписывает
те же bytes, включая magic. Group key compressed bytes и players commitment — часть
body, поэтому нельзя подменить material при неизменном manifest.

Boundedness: n не больше существующего consensus bound 256. Одна attestation содержит
fixed body, index и одну MinPk signature; проверка constant crypto + bounded state read.
Нет q pairing loop внутри одного block/precompile call. Pending state — фиксированные
slots по participant index (максимум 256), каждый содержит epoch/sequence/body hash.
Generation tags делают старые slots неактивными без неограниченной очистки. Для tally
допустим scan этих ≤256 slots, но не их crypto re-verification. Conflicting candidate
не создаёт новые storage keys: один index занимает один slot для sequence. Finalized
receipt хранится отдельно и неизменяем; history растёт только с approved executions и ограниченными chain epoch material bindings.

Новый canonical write публикуется **после** finality этого checkpoint. Receiver Outbe
adapter читает receipt из exact finalized state своего node и передаёт Heartwood ticket
для exact manifest/expected previous refs. Gate повторно проверяет ticket при CAS/promotion.
Удалённый peer не может создать ticket; missing/mismatched proof/cache entry fail closed.
Ticket/cache содержит finalized block hash и монотонный receipt sequence; conflicting
block hash на принятой finalized высоте локально запрещает managed writes.
Archive и Promote различаются: старый accepted receipt разрешает immutable historical
objects, но не rollback canonical refs. Promote принимает только latest committed
complete signed refs snapshot из exact trusted view, с sequence не меньше локального
принятого. Fresh/lagging receiver может установить latest snapshot после проверки его
полного reachable object closure и receipts, не проигрывая все промежуточные ref deltas.
Local CAS сравнивает actual local refs root, захваченный перед install; global previous
receipt hash остаётся precondition chain execution, а не требованием, что локальная
реплика уже была на этом predecessor. Same-sequence different snapshot запрещён.
До получения latest snapshot реплика не объявляется current/ready для signing.

### Security argument и границы гарантии

- Stale group signature сама по себе остаётся valid, что подтверждает executable check.
- Для нового checkpoint нужны q **current identity** keys. Старые identity keys или
  self-asserted current list не проходят state-derived mapping. При ≤f current Byzantine
  каждый accepted quorum содержит honest participants, проверивших exact approved package
  и current partials. Подделка публикации требует нарушения BLS/ECDLP/hash assumptions
  либо компрометации достаточного current quorum; постоянный owner scalar сам по себе
  такого права не даёт.
- Certificate, собранный до membership transition, но включённый после, stale и не
  создаёт receipt. Пересборка использует новую generation и то же неизменяемое approval.
- Checkpoint, принятый **до** transition, является уже разрешённой историей: его точный
  package можно доставить позднее. Это не новая операция retired participants. Любое
  изменение package/sequence/prev hash требует нового current quorum.
- Историческая проверка читает immutable chain receipt/commitments и native signatures;
  она не принимает произвольные backdated certificates от old quorum. Cached crypto-valid
  objects без receipt не повышаются до authoritative history.
- Требуется eventual stable committee и доставка q attestations для liveness. При
  постоянной churn/partition repo может ждать; consensus после bootstrap продолжает
  работу. Чудесной гарантии progress без q online участников нет.

## 5. Genesis / initial identity / RID

Genesis descriptor V1 фиксирует name=`governments`, branch=`main`, description,
canonical initial Git tree/content commitment, schema/version, native BLS policy и
initial group-key source=`initial authenticated network DKG`. Descriptor hash включает
chain identity. В genesis **нет выдуманного заранее готового group public key/RID**.

После initial DKG validating node получает authenticated group key/output. Из descriptor
и key строится canonical native identity document с одним BLS delegate, delegate
threshold=1; криптографический signing threshold при этом q. Doc blob OID даёт RID (S05).
Один deterministic bootstrap package подписывается shares, затем q current validators
аттестуют bootstrap body. Finalized checkpoint атомарно привязывает descriptor → RID,
owner key и initial refs manifest и добавляет repo в существующий registry.

Ordinary permissionless `register_repository()` не может присвоить descriptor. Если
кто-то заранее зарегистрировал тот же RID, authenticated bootstrap сохраняет индекс
и явно помечает его managed; registrant не получает owner или право менять binding.
При отсутствии места bootstrap остаётся pending с явной причиной, capacity policy
задаётся genesis так, чтобы зарезервировать одно место managed repo. Изменять legacy
slots 0–5 нельзя; descriptor/binding/checkpoint schema append-only и opt-in.

Initial metadata фиксировано descriptor/genesis: author/committer `Outbe Network`,
email `network@outbe.invalid`, UTC timestamp genesis timestamp; root parents пусты.
Отдельные COB commit timestamps не определяют RID, но нужны одинаковые packages.
После finality bootstrap checkpoint sidecars materialize/publish объекты; повтор/join
читает те же bytes и RID. Нет ручного rad init, реконструкции owner secret или первого
непроверенного RPC регистратора. До binding repo не входит в bootstrap availability
barrier, иначе получилось бы ожидание chain blocks до запуска consensus.

## 6. Vote и execution lifecycle

Patch target — расширение RadicleRegistry, не Governance kind/text и не второй registry.
Canonical payload V1: `version`, `repoId`, `patchId`, `revisionId`, `action=accept`,
`targetBranch=main`, `expectedTargetHead`. OIDs/target head — проверяемые ссылки и
precondition принятия patch, не отдельный предмет голосования за будущий code commit.
Malformed/unknown/duplicate fields и oversized payload отклоняются.

Target-specific Vote hook определяет electorate=ACTIVE ∩ effective consensus set при
cast и tally. Для остальных targets default ACTIVE behavior неизменен. Count и tally
используют один и тот же список; сохраняется existing правило `yes*3 >= count*2`.
Истёкший/ошибочный/нефинализированный proposal не даёт permission. Chain admission не
читает Git: существование/авторские signatures/revision/target проверяет signer до partial.

Approved handler пишет immutable approval и execution stage, без Git/network I/O.
`policy_context` = SHA256(`OPJ1` || chain_id:u64be || genesis_hash[32] || repo[20]
|| proposal_id:u256be || patch[20] || revision[20] || expected_target[20]
|| action:u8 || subject_ordinal:u8 || epoch:u64be || material_version:u64be
|| material_hash[32] || previous_receipt_hash[32]). Bootstrap использует proposal/patch/
revision=zero; BindMaterial native signing не требует. Native metadata не входит в этот
job-base hash: оно отдельно защищено RDB1, поэтому document signature внутри COB tree
не создаёт self-reference. Native subjects готовятся последовательно: Doc signature,
затем содержащий её COB tree, затем signed refs. Уже собранные signatures входят только
в конечный package manifest, который аттестуется последним. Collector проверяет
actual revision и target, не `latest()`. Авторское обновление/редактирование target не
расширяет approval. No-voter может подписать корректное исполнение Approved.

Сначала публикуются Review Accept + его signed refs через отдельный finalized checkpoint.
Затем merge: если current target head отличается от expectedTargetHead — состояние
`ApprovedUnmerged(TargetMoved)`, нужен новый Vote с новой precondition. Если совпадает,
FF применяется когда возможен, иначе clean three-way merge из approved base/head и
expected target, с fixed parent order и deterministic metadata от finalized approval.
Конфликт не разрешается автоматически. Никаких новых code edits/rebase/squash.
Merge commit, native Merge action и signed refs составляют следующий package/checkpoint.

При двух approvals к одной ветке порядок `(approval finalized height, proposalId)`.
После первого merge второе с прежней precondition явно остаётся unmerged; это не
ожидание несуществующего approval будущего merge commit. Review и Merge execution
statuses наблюдаемы отдельно; native patch не получает выдуманный State::Approved.

Journal-first FSM каждого collector:

```text
ObservedFinalizedApproval -> PreparedNativeObjects -> VerifiedPartials
 -> NativeSignaturesRecovered -> CompletePackagePersisted -> PublicationAttesting
 -> CheckpointFinalized -> ObjectsInstalled -> RefsCAS -> Announced -> Done
```

Job key включает subject/step/material/expected predecessor. Before checkpoint при
смене context job отменяется/пересоздаётся; после checkpoint воспроизводится exact
authorized package. После падения новая операция не выдумывается: CAS проверяет old
refs; already-new refs — идемпотентный успех; несовпадающее состояние quarantined.
Git object install до refs, complete sigrefs validation до promotion, durable marker
до announcement. Одновременно один writer на repo; collecting не блокирует реактор.

## 7. Карта реализации и проверки

P1: сохранить весь Outbe delta при stable update; root/standalone builds, sidecar limits
и replication. Историческая baseline failure не является waiver обязательного лимита.

P2: `radicle-crypto` RepositoryKey/envelope; `radicle` DID/doc/cob/store/storage namespaces;
`radicle-cob` change/signatures + generic two-phase preparation; git metadata header;
`radicle-{protocol,fetch,node,remote-helper}` key-aware repository frames и fail-closed
publication hook. Network NodeId/Noise остаются Ed25519. Два static BLS peers и legacy
Ed25519 workflow обязаны пройти реальный fetch/reopen/tamper tests.

P3: `consensus/hybrid/vrf_material.rs` narrow fixed-domain signer, engine finalized DKG
mapping; `primitives/reshare_artifact` и consensus/engine header-artifact bundle carrier;
`radicleregistry` typed publication attestation/checkpoint runtime/schema; validating
node ↔ local sidecar authenticated ticket interface. Проверить real namespace isolation,
old shares/current identity rejection, membership transition, constant per-block work
на n=256, no blocking sidecar dependency и consensus/VRF regressions.

P4: genesis descriptor in scripts/codec/network inputs + reserved registry capacity;
post-DKG native bootstrap, descriptor binding и manager allowlist. Проверить одинаковый
RID/objects на двух узлах, early ordinary registration, replay/restart/join, bad genesis/key.

P5: registry canonical patch payload/approval/readers, target-specific Vote hook и actual
EVM registry, `contracts/precompiles/src/IRadicleRegistry.sol` + exported ABI, exact-state
approval reader. Проверить complete EVM lifecycle, electorate mismatch, gas/rollback,
other Vote targets unchanged и невозможность receipt для не Approved patch.

P6: `crates/system/radicle` collector/journal/package/checkpoint publishing, P3 bounded
carrier bridge в `crates/blockchain/radicle`/launch wiring. Проверить two collectors,
partial sender/index/domain validation, bad/missing receiver ticket, empty fresh receiver,
raw old-owner forgery без checkpoint, race before/after checkpoint и crash boundaries.

P7: deterministic merge/execution journal + native Merge/refs, conflict/TargetMoved,
concurrent approvals, partial publication recovery, immutable historical replay.

P8: реальная four-validator сеть, initial DKG и reshare, два independent Radicle stores,
genesis→patch→Vote→finality→threshold→checkpoint→Review→Merge→fetch; old owner/current
membership attacks, liveness при sidecar outage, required checks/CI и operator runbook.

P0 evidence — source matrix, exact primitive/model checks и этот выбранный контракт.
Они доказывают проектное решение и осуществимость примитивов, а не наличие checkpoint,
generic policy hook, signer, bootstrap, VoteTarget или E2E в production source.
Не закрывать P0 до трёх независимых full-AC reviews этого документа и evidence на одном SHA.
