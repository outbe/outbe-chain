# P0: контракт BLS repository и current-set publication

Статус: candidate для трёх независимых P0 reviews. Реализация P1–P8 ещё отсутствует.
База исследования: `42d08693a06a7fdc088cf452b582c2d18596ed9a`.
Source anchors S01–S40/C01–C06 разрешаются в `evidence/outbe-chain-pjz.1/source-audit.json`:
там exact paths, строки и SHA-256. `investigation.md` сохраняет первоначальные вопросы;
решения ниже заменяют его открытые варианты, но не объявляют production реализацию.

## 1. Выбранный контракт и доверие

Владелец governments — нативный **BLS key**, а не новый Outbe-specific вид repository.
Ed25519 остаётся для пользователей и Noise/NodeId. Heartwood не читает Vote, epoch
ValidatorSet или EVM state. Его криптографический verifier проверяет key/message/signature.

Разрешение нового canonical write **от managed owner** требует **двух** проверок:

1. Все native BLS identity/COB/sigrefs signatures корректны под repository owner key.
2. Outbe publication policy предъявила подтверждение, что exact package зафиксирован
   в chain state полномочным текущим набором после разрешённого решения.

Для второго условия вводится общий Heartwood interface `PublicationPolicy`: он получает
repository/expected previous refs/package manifest и разрешает или запрещает promotion.
Обычные репозитории используют существующие native правила. Для governments policy
передаёт `crates/system/radicle`, используя только аутентифицированные сведения своего
validating Outbe node. Внутри Heartwood нет ветки `if Vote Approved` или OutbeNetwork DID.

Policy gate охватывает **действия managed owner**, включая их использование через
чужой namespace, при local writes, fetch, identity/refs promotion и чтении/rebuild cache.
Обычный Ed25519 автор может создать, изменить и реплицировать patch/revision до Vote:
его собственные COB entries и sigrefs проходят existing native signature/authorization
правила без publication checkpoint. Они доступны evaluator и exact revision lookup.
Это не разрешает автору подписываться за owner, менять delegate identity/owner refs
или включать неразрешённые owner entries как родителей своего COB. Каждая reachable
owner-signed entry требует exact accepted receipt независимо от доставившего namespace.
Смешанный graph проверяется по entry, не по внешнему transport/remote key; чужая ссылка
на forged owner Review/Merge не легализует его. Неразрешённые owner entries и зависимые
updates quarantined; чистые author updates не блокируются. Кэш хранит receipt provenance
owner entries; direct lookup, reopen и rebuild применяют тот же predicate до evaluation.
Нельзя проверить только честный collector: получатель с пустым storage обязан применять
эти правила. Immutable quarantined objects не влияют на canonical identity/patch/refs.
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
| 2 / COB | operation tree OID, 20 bytes | SHA256 exact unsigned Git commit buffer (§2.1) |
| 3 / sigrefs | exact `Refs::canonical()` bytes | SHA256(previous sigrefs commit OID[20]); zero OID[20] для root |

BLS message V1 имеет fixed framing:

```text
RDB1[4] || kind:u8 || repo_id[20] || policy_context[32] || metadata_hash[32]
        || native_length:u32be || SHA256(native_payload)[32]
```

Предел native payload — 1 MiB; длина проверяется до выделения памяти. Kind 1/2 требуют
ровно 20 байт. Для управляемых jobs `policy_context` — SHA256 от canonical Outbe job
context, описанного ниже; generic crypto не интерпретирует его. Для обычного static
BLS repo context нулевой. Managed policy отвергает нулевой/неожиданный context.

Envelope V1 = exact RDB1 message[125] || compressed owner key[96] || signature[48],
ровно 269 bytes. Signature покрывает message[125], key обязан совпадать с actor/delegate.
COB header — одна строка `radicle-bls-sig <538 lowercase hex characters>\n`, без folding.
Legacy `gpgsig` запрещён вместе с этим header; SSH parser не переопределяется. Identity
action signature — JSON object ровно с полями `algorithm: "bls12381-g2-v1"`,
`envelope: "<538 lowercase hex>"`; legacy Ed25519 representation неизменен. Unknown,
duplicate или лишние поля отвергаются. Sigrefs `/signature` хранит legacy 64 raw bytes
либо 269-byte envelope с magic `RDB1`. Truncation, trailing bytes, upper/mixed hex,
unknown kind, неверная длина/payload hash/meta/context отвергаются. Проверяющий
восстанавливает message из actual object, не доверяет дублированным полям envelope.
Все owner envelopes и receipts проверяются до использования entries/promotion.

### 2.1. Exact metadata, namespace и wire V1/V2

Unsigned COB buffer — exact Git commit content, без Git object `commit <length>\0`
prefix и без единственного physical `radicle-bls-sig` header. Его grammar:
`tree OID\n`, затем ordered `parent OID\n` (без duplicates),
`author NAME <EMAIL> SECONDS OFFSET\n`, `committer NAME <EMAIL> SECONDS OFFSET\n`,
пустая строка, затем UTF-8 message, оканчивающееся LF. OID — 40 lowercase hex;
NAME/EMAIL не содержат CR/LF/NUL/`<`/`>`; SECONDS — i64 decimal без leading zeros или
`-0`; OFFSET — `+HHMM` или `-HHMM`, HH≤23/MM≤59 (нулевой только `+0000`). Угловые
скобки вокруг EMAIL — literal Git delimiters; uppercase слова обозначают placeholders.
V1 BLS COB не допускает другие headers, CR, NUL, folding или несколько signature headers.
Message включает exact existing Resource/Related trailers, без их удаления/нормализации.
Подписанный header вставляется после committer перед пустой строкой; verifier удаляет
только эту одну строку, проверяет grammar и хеширует остальные bytes буквально.
Tree OID из buffer обязан совпадать с native subject. Managed metadata/parent snapshot
единственны по §6; static BLS author может выбрать допустимые author/time/parents/message,
но подпись защищает все эти bytes. Результат не зависит от Git config или process env.

Namespace textual key = `z` + base58btc(canonical multicodec varint || compressed key),
без `did:key:`; Ed25519 text и существующие namespace paths не меняются. DID добавляет
`did:key:` к этому key. Binary RepositoryKey V2 = `tag:u8 || key`, tag0=Ed25519[32],
tag1=BLS G2[96]. Внутри tagged поля нет длины/padding; unknown tag, short/trailing input
в standalone key, noncanonical/subgroup/identity key отвергаются. Binary NodeId остаётся
untagged Ed25519[32] в обеих версиях.

Legacy frame `rad\x01` и legacy RefsAt remote[32]||at[20] неизменны. Upgrade объявляет
feature bit `1<<63` в existing u64 Node Features (сейчас занят только SEED bit0).
Первичные control/gossip остаются V1. После получения authenticated peer capabilities
и отправки своего announcement peer может послать V1 control Upgrade: control type3,
payload byte `02`. Receiver, проверив оба capability, отвечает V1 control UpgradeAck:
type4, payload `02`. Ни stream id, ни длина внутрь этих двух control payload не добавляются;
existing outer Frame framing неизменен. Incoming V2 разрешён после отправки Ack, outgoing
V2 — после получения Ack. Upgrade/Ack idempotent в пределах Noise connection; disconnect
сбрасывает negotiation. Unsupported version/control/необъявленная capability отклоняются.
Тем самым ACK гонка не заставляет отправлять V2 peer, который ещё принимает только V1.
После этого допускаются `rad\x02` frames. V2 отличается только кодированием repository
key fields (RefsAt.remote и repository namespace lists): tag+key вместо Ed32; остальные
поля/varints/message tags/limits неизменны. Transport authentication/announcements signer
по-прежнему NodeId. Ненеготированный V2 отклоняется. BLS request к V1-only peer возвращает
explicit UnsupportedRepositoryKey, не кодируется в legacy remote и не понижается до Ed.
Ed-only replication может оставаться V1 между любыми peers. Git protocol namespace
strings используют canonical textual key выше и existing refname validation.

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

`players_commitment` = SHA256(`OPL1` || n:u16be || ordered entries), entry =
index:u32be || EVM address[20] || current MinPk public key[48]. Entries идут в exact DKG
players order, indices contiguous 0..n-1. `material_hash` = SHA256(`ODM1` || epoch:u64be
|| version:u64be || players_commitment[32] || output_length:u32be || public Output.encode()),
где Output — pinned Commonware `feldman_desmedt::Output<MinSig, identity key>`; его
summary/public/dealers/players/revealed encoding определяется SDK, secret Share исключён.
Alternate proofs canonical Output не меняют. Honest nodes получают один Output из
canonical dealer logs; несовпадение — explicit material fault, не выбор другого candidate.

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
с old/new OIDs, native subjects/envelopes и canonical metadata. **Manifest не включает
interchangeable contribution witnesses**: public DKG proof и q indexed partial frames
передаются в отдельном witness bundle. Bundle обязан сопоставляться с body material hash
и exact native subjects; каждая partial проверяется до recovery/attestation. Receiver
также проверяет q current frames для каждого subject независимо от owner signature.
Разные valid q-subsets разрешены и дают один manifest/body; witness substitution допустима
только при повторной полной проверке. Witness bytes не являются Git objects/refs и не
влияют на owner state. Identity attestations также не входят в manifest.
Manifest содержит ровно newly created native objects и deterministic desired owner refs
snapshot (§6), не произвольные extra reachable author objects; content closure доставляется
отдельно и проверяется по OIDs. Objects сортированы по kind/OID, refs по имени,
subjects по ordinal; duplicates запрещены.
Witness frames отсортированы по subject ordinal/index, duplicates запрещены, отдельный
bundle ограничен 1 MiB. Максимальный размер manifest — 1 MiB, число native subjects
не больше трёх (bootstrap Doc/COB/refs); Review и Merge содержат COB/refs. Сам certificate не входит в hash
package; circular self-signing отсутствует. Typed `action` и context связывают actual
native Review/Merge с chain permission; signature над произвольным code commit не подходит.

Manifest V1 — UTF-8 canonical JSON через existing Heartwood `canonical::formatter::CanonicalFormatter` (как Doc::encode),
без whitespace/extra fields: `version:1`, `objects:[{kind,oid,sha256}]`,
`refs:[{name,old,new}]`, `subjects:[{ordinal,envelope}]`,
`ownerSnapshot:[{name,oid}]`. kind — `blob`/`tree`/`commit`; OID — lowercase hex40,
SHA256 — hex64, envelope — hex538; zero OID только отсутствие old/new ref, не object.
Objects включают ровно созданные blobs/trees/commits native action, sigrefs и clean merge;
уже pinned dependencies не дублируются. Snapshot содержит complete desired owner refs,
исключая сам sigrefs ref. `refs` дополнительно включает update sigrefs branch; отсутствие
изменения не создаёт ref delta. BindMaterial: первые три arrays пусты, snapshot предыдущий.
Key sorting recursive canonical serializer; array ordering выше; duplicate JSON keys,
invalid refname, repeated OID/name/ordinal и size limit нарушают canonical format.

Body принимается в chain **по одной bounded identity attestation за раз**. Verified
distinct current MinPk signatures учитываются по `(repo, epoch, material_version,
material_hash, sequence, body_hash)`; один participant не аттестует два body одной
sequence **в одной current material generation**. Ровно q действительных участников
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
с использованием effective execution-state set этого шага. Anchor **единственный для job**:
bootstrap — genesis; BindMaterial — activation block текущего epoch; Review/Merge — блок
с максимальной height среди terminal approval, current material binding и previous receipt.
Hash/height берутся из своей проверенной canonical finalized chain; если нужный блок ещё
не finalized, attester ждёт его, а не выбирает более новый локальный tip. В BindMaterial
anchor уже полномочен current identity set, а новый output отдельно заверяется этим set.
Для Review/Merge anchor state содержит permission/material. Node verifier
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
slots по participant index (максимум 256), каждый содержит epoch/material generation/
sequence/body hash. Смена material аннулирует pending slots даже внутри одного epoch;
новая generation может использовать незавершённую sequence заново.
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
- Для одного execution job honest collectors получают одинаковые native bytes (§6),
  уникальную recovered BLS signature, один manifest без interchangeable witnesses и один
  job-derived anchor. Их body совпадает при любых valid q-subsets и разных local tips.
  Honest attester не подписывает body, отличающийся от independently reconstructed job.
  Неизменяемый slot поэтому не блокирует honest convergence: ≤f Byzantine slots могут
  занять другие hashes, остальные ≥q honest slots сходятся на одном. Требуются доставка
  всех закреплённых author objects и q shares/attestations; неподтверждённый candidate
  не меняет metadata/parents job. Contention approvals сериализуются по §6.
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

Preparation snapshot **не равен локальному latest COB graph**. Для Review COB tips —
exact approved revision entry плюс previous accepted owner COB tip этого patch из
previous receipt, если он существует. Для Merge tips — finalized Review entry данного
approval плюс previous accepted owner tip. Resource — accepted canonical identity tip
из previous receipt. Related — только required code commit refs exact action (пусто для
Review; merge result для Merge). Commit parents: ordered COB tips (lexicographic raw OID,
dedup), затем sorted/dedup Related, затем Resource, с сохранением первого вхождения при
повторении. Resource/Related trailers отражают те же sorted lists. Все ancestor/object
dependencies pinned этими OIDs; новые author tips после approved revision не добавляются.
Signer проверяет нужный native state именно на этом graph slice. Missing objects ждут
fetch; malformed pinned graph завершает execution явной ошибкой, не меняет snapshot.

Managed action JSON и manifest используют existing canonical serializers, фиксированные
значения и field order, без local clock/random IDs. Author/committer всегда `Outbe Network`
`<network@outbe.invalid>` UTC; timestamp Review/Merge — timestamp terminal approval block,
bootstrap — genesis timestamp. Message `Bootstrap repository\n`, `Accept patch\n` либо
`Merge patch\n` + required native trailers; нельзя добавлять collector/local metadata.
Owner signed refs snapshot = предыдущий accepted complete snapshot + ровно action updates
(bootstrap identity/default head/COB; Review patch COB; Merge head/patch COB), sigrefs ref
не подписывает сам себя. Previous sigrefs commit берётся из previous receipt. Новые
sigrefs commit metadata фиксированы теми же actor/time и `Update signed refs\n`.
BindMaterial — metadata-only chain checkpoint: native subjects/objects пусты, owner refs
snapshot unchanged; manifest фиксирует это единственное no-op, не заново подписывает refs.
Таким образом optional extra objects, доставка witness subset, локальный Git config,
новейший author tip и более новый finalized tip не меняют honest candidate bytes.

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
`primitives/src/system_tx/{kind,input,layout,phase,binding,gas}.rs` и tests/ — actual
codec, activation и begin order; EVM re-export не владеет отдельным system_tx/ codec.
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
different valid witness subsets/local tips/extra author tips → identical native package,
pre-Vote author admission и owner-entry laundering negatives во всех entry/cache paths,
partial sender/index/domain validation, bad/missing receiver ticket, empty fresh receiver,
raw old-owner forgery без checkpoint, race before/after checkpoint и crash boundaries.

P7: deterministic merge/execution journal + native Merge/refs, conflict/TargetMoved,
concurrent approvals, partial publication recovery, immutable historical replay.

P8: реальная four-validator сеть, initial DKG и reshare, два independent Radicle stores,
genesis→patch→Vote→finality→threshold→checkpoint→Review→Merge→fetch; old owner/current
membership attacks, liveness при sidecar outage, required checks/CI и operator runbook.

P0 evidence — source matrix, exact primitive/model checks и этот выбранный контракт.
`p0-check/src/native.rs` и vectors/ дают finite byte/crypto vectors; fixture parsers не
production parsers. `design-check.log` содержит actual native payloads, OPJ1/RDB1 bytes,
Commonware framing, metadata hashes, signatures/envelopes и repository-key wire bytes.
Они доказывают проектное решение и осуществимость примитивов, а не наличие checkpoint,
generic policy hook, signer, bootstrap, VoteTarget или E2E в production source.
Не закрывать P0 до трёх независимых full-AC reviews этого документа и evidence на одном SHA.
