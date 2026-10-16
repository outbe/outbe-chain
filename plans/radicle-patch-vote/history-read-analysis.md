# Чтение exact accepted history: manifest и q-witness

Исследованный source: `6f86ccf68030a71f56242e53174d25bb7ed56ae3`, worktree `feat/radicle-patch-vote`. Автор этой части: `/root/d3_review_3`. Read-only source research; tests и сетевые процессы не запускались. Изменён только этот исследовательский документ. Старые D4 review conclusions не использовались как доказательство необходимости механизма.

## Вывод

**Для уже принятой exact истории нужно доказать, что полученные объекты — именно те bytes/refs, которые сеть уже приняла. Повторно доказывать новый Vote или получать новый quorum не требуется.**

В текущем коде эти две задачи механически соединены: accepted verifier требует и manifest, и bundle с historical Output/q-partials. Но это разные гарантии:

- **Manifest** связывает фактические native объекты, envelopes и owner refs с `package_manifest_hash` в собственном finalized receipt. Убирать эту связь нельзя; можно исследовать вычисление эквивалентного canonical manifest из Git/chain вместо доставки отдельного файла.
- **Historical q-witness** повторно доказывает происхождение native подписи от q индексированных долей конкретной принятой DKG-generation, с identity authentication каждой partial. Это независимая проверка transcript/provenance. При доверенном exact finalized receipt, честном publication quorum и сохранённой полной привязке native bytes она не добавляет нового разрешения читать owner history.

Контрпример «старые shares создают НОВУЮ разрешённую запись» при удалении только historical q-checks и сохранении exact trusted receipt/object binding не найден: новая запись меняет manifest/commitment и не проходит старый receipt. Есть более сильная audit-модель с компрометацией q publication identity keys, описанная ниже; её нельзя выдавать за обычную потребность свежего узла повторять старое голосование.

Это **conditional security sufficiency**, а не доказанная drop-in замена существующего verifier или готовая совместимость native fresh fetch. Production/spec/task изменений не предлагается выполнять без отдельного согласования.

## Факты текущей реализации

### 1. Accepted receipt — не просто Approved

`crates/blockchain/primitives/src/radicle_publication.rs:86–107,109–137`: PublicationBody содержит network/genesis/RID/owner, material/players commitments, action, точные proposal/patch/revision/expected target, predecessor/sequence и `package_manifest_hash`. Самих manifest bytes, Output и partial frames в body нет.

`crates/system/radicleregistry/src/publication.rs:105–130,151–175`: chain handler проверяет indexed MinPk publication attestations над exact body и фиксирует receipt при q=`n-(n-1)/3`. `:178–220` проверяет current epoch/effective committee/material. `:294–317` связывает Review/Merge с immutable exact patch approval и допустимым execution stage. `:353–387` записывает body, accepted height и hash.

**Факт:** chain handler не получает native Git objects, manifest preimage или q repository partials. Он не выполняет их cryptographic/native validation внутри execution; он проверяет текущие network identity attestations над commitment. Это выбранная граница offchain проверок/consensus.

`crates/blockchain/radicle/src/manager/snapshot.rs:411–476`: FinalizedPublicationReceipt строится из собственного canonical finalized state, с проверкой network/genesis/sequence/hash/accepted height. Peer не может заменить его своим «accepted=true».

Поэтому «доверенный exact accepted receipt» здесь существенно сильнее `Vote Approved` и owner BLS signature: он фиксирует конкретный опубликованный package commitment, а не только разрешает впоследствии подготовить какую-нибудь запись.

### 2. Manifest даёт exact-byte inclusion

`crates/blockchain/radicle/src/integration/publication.rs:194–215` определяет native object kind/OID/SHA256, ref old/new и native envelope. `:320–368` восстанавливает owner refs, сравнивает canonical manifest hash с receipt и проверяет owner/repository/kind/job context envelopes.

`integration/publication/review.rs:213–297` заново готовит exact Review из trusted approval/predecessor, проверяет/прикрепляет native signatures и вычисляет canonical manifest. `:319–373` проверяет exact accepted action/payload/predecessor/context и полное равенство вычисленного manifest принятому. Похожий accepted bootstrap: `integration/bootstrap.rs:554–623`; Merge: `integration/publication/merge.rs:245–300`.

Принципиально важно сохранить **actual native reconstruction + native signature verification + commitment equality**, а не доверять peer-declared envelope/refs. `vendor/heartwood/crates/radicle-crypto/src/repository.rs:452–471` проверяет envelope против отдельно восстановленного subject и owner, включая подпись над actual RDB1 bytes.

`integration/review.rs:153–190,389–444,490–527`: pinned context включает accepted owner refs/sigrefs, exact approved revision, её base/head/expected target, native dependency graph и Git closure. Файлы/refs «latest от peer» не заменяют эти roots.

### 3. Что именно добавляет historical witness

`integration/signing/accepted.rs:27–54` требует canonical public Output и проверяет его n/q/group key и ODM1 hash against accepted body. `:85–107` сначала убеждается, что envelope **точно совпадает** с envelope в уже committed manifest; затем вызывает witness verifier.

`integration/signing/witness.rs:34–50` сопоставляет Output, subject count/order и frames. `integration/signing.rs:879–915` требует q..n distinct sorted partial indices и recover полученной native подписи в точное committed envelope. `:918–959` проверяет:

- message hash actual native subject;
- historical chain/genesis/epoch/version/material;
- player identity ↔ DKG index;
- identity signature над partial frame;
- partial по принятой public polynomial.

`signing.rs:962–993` восстанавливает подпись, а не private key. `:996–1013` определяет ODM1 commitment, включающий полный canonical Output.

**Факт:** witness bytes не включены в package manifest/body commitment. Это отдельный interchangeable bundle. Разные корректные q-subsets допускаются (`signing/witness.rs` module contract), а native signature/envelope остаётся тем же. Bundle нельзя считать дополнительным voted object, единственный сохранённый original signer subset не является consensus identity принятой записи.

`integration/publication/binding.rs:133–161`: BindMaterial вообще не имеет native subjects/partial frames; accepted witness проверяется с пустым `actual`, то есть в этом случае он переносит public Output, а не «голоса за чтение history».

### 4. Новая запись и повторное чтение разделены лишь типами/путями, но проверки сейчас общие

Current signer/recovery выполняют `authorize_current_review` прежде partial recovery (`signing.rs:490–533`). Подготовка новой публикации `publication/review.rs:83–114` проверяет current q witness до создания VerifiedReviewPublication. Current publication attestation принимает только sealed durable SelectedReviewPublication и повторно читает current authority/material/permission (`network.rs:622–682`).

Accepted path использует отдельный `AcceptedReviewPublication` без current signing capability (`publication/review.rs:300–317`). `publication/policy.rs:143–196,199–239` создаёт exact entry grants из accepted proof; canonical snapshot install отдельно требует latest accepted sequence/sigrefs (`:62–95`).

**Факт:** historical q-check сейчас нужен программе, потому что API accepted proof его требует. Это не само по себе доказательство доменной необходимости той же проверки при каждом чтении. Удаление только этих checks из history не означает разрешение удалять их из signing/prepublication или ослаблять current-material guard.

## Что доступно из chain/Git, а что не выводится

| Данные | Что уже доступно | Что требует дополнительных bytes/работы |
| --- | --- | --- |
| Receipt body, sequence, immutable approval, network owner/context | Собственное finalized chain state (`snapshot.rs:411–476`, registry source выше) | Не доверять сетевому supplied receipt вместо local read. |
| Native payloads, commits/trees/blobs, COB metadata, BLS envelopes, signed refs | Это native Git data. COB loader читает `radicle-bls-sig` из commit (`radicle-cob/src/backend/git/change.rs:274–296`); sigrefs reader читает `/signature` из Git tree (`radicle/src/storage/refs/sigrefs/read.rs:602–620`). | Реально получить pinned objects; peer tips не доказывают, какой owner snapshot accepted. |
| Canonical publication manifest | В chain есть только hash. Review/Bootstrap/Merge preparation уже умеет вычислять manifest из trusted context + pinned native inputs. | **Из одного hash preimage не вывести.** Reconstruction из полного Git/chain представляется возможной для конкретных actions, но это пока вывод, не готовый universal decoder: current accepted helpers сами берут envelopes из supplied manifest и требуют previous snapshot. Нужны поиск exact entries/signatures, historical selection и цикл admission/fetch. |
| Public historical DKG Output | Не выводится из96-byte group key/ODM1 hash. В коде есть детерминированная reconstruction из ceremony Info + finalized dealer logs (`consensus/src/dkg_manager/ceremony.rs:212–229`, `dkg_manager/finalized.rs:57–102,117–126`). | Не доказано, что каждый bootstrap/manual mode и любое pruned node сохраняет все эти inputs. Текущий bundle удобно доставляет Output, но blanket claim «Output отсутствует в chain и иначе получить невозможно» неверен. Доступность/исторический replay требуют отдельной проверки. |
| q repository partial frames и individual identity signatures | В final native BLS envelope их нет; publication receipt их не хранит. | Из одной aggregate threshold signature нельзя восстановить оригинальный signer subset/authenticated frames. Их нужно отдельно хранить/доставлять, **если** выбранная audit-гарантия требует их повторно проверять. Это не доказывает, что они нужны для read authorization. |
| Genesis initial-content manifest artifact | Descriptor содержит его SHA256. `bootstrap.rs:184–198` требует exact artifact bytes отдельно и проверяет committed initial tree closure. | Общий arbitrary artifact preimage не выводится из hash/Git tree без правил canonical reconstruction. Для текущего empty content literal `[]` известен; обобщать на произвольные genesis artifacts нельзя. Это отличается от publication manifest и q witness. |

## Вывод при явных допущениях: q-only historical removal

Рассматривается только чтение уже finalized accepted package. Сохраняются:

1. Собственный trusted canonical finalized receipt R и его exact immutable chain context; не Approved bool.
2. Canonical manifest M (доставленный или независимо восстановленный), где SHA256(M)=R.package_manifest_hash; его schema/scope/context проверки.
3. Native verification actual objects/envelopes, exact object-byte/OID/SHA commitments, pinned content closure, approved revision, predecessor/history и exact owner refs.
4. Accepted-entry grants только для этого proof; latest sequence/CAS для canonical promotion; current signing/new-publication checks не меняются.
5. Обычные cryptographic content-binding assumptions и честный publication quorum: честный attester проверяет complete native/q package до подписи exact body.

**Argument:** после принятия R peer может либо предоставить тот же M и те же проверенные bytes (это точная разрешённая история), либо другой M/objects. Другой M не проходит receipt hash; другие actual bytes/refs не проходят native reconstruction/object commitments. Изменение/удаление некоммитящегося witness не меняет ни R, ни M, ни множество grantable exact entries. В этом сценарии повторная q-проверка лишь запрещает читать принятые bytes при недоступном/повреждённом transcript. Она не создаёт дополнительной защиты от публикации новой owner записи.

**Old shares scenario:** после reshare старые shares могут дать group-valid signature для произвольного нового subject, потому что group key сохраняется. Но новый subject/signature/native commit/refs создают другой manifest commitment. Без нового accepted receipt он не получает grant. Подпись старым ключом над **точно уже принятыми bytes** воспроизводит историю и не выдаёт новых прав. Новый узел не должен требовать новый Vote за это воспроизведение.

Это source-backed conditional argument. Не выполнялся эксперимент с изменённым verifier; не утверждается drop-in equivalence всех ошибок/типов/хранилищ. У текущего API отсутствие witness по-прежнему означает reject.

## Stronger audit case, находящийся за пределами honest-publication-quorum

Контрпример к более сильной гарантии provenance существует при другом допущении:

- противник располагает q **текущими publication MinPk identity keys** (или q attesters обходят required native/q checks), но не q текущими DKG shares;
- у него достаточно старых shares для прежнего group secret/signing после same-key reshare;
- есть настоящий immutable exact Approved patch, и сохраняются все прочие chain/body/stage constraints;
- он создаёт native-valid exact package старым group material, подписывает его body q текущими MinPk identities и добивается accepted receipt через существующий handler.

Registry проверяет именно эти MinPk body attestations (`publication.rs:118–130,151–175`), а не partial transcript. Поэтому такой body проходит deterministic publication validation при соблюдении прочих preconditions. Historical witness, привязанный к принятой новой polynomial, дополнительно отвергнет old-generation partials; native BLS + exact receipt alone этого происхождения подписи не различает. Отказ mixed/old polynomial показан существующими source assertions `consensus/src/dkg_actor/actor/tests/reshare/repository_signing.rs:231–262` (тест здесь не запускался).

**Граница вывода:** это нарушение assumed honest publication quorum, а не атака одним retired validator на честно принятую историю. Оно демонстрирует independent transcript audit, а не необходимость новому узлу голосовать, получать новую подпись или пересматривать принятое решение. Если такая дополнительная проверка обязательна как отдельный контракт, её нельзя молча удалить; нужно явно согласовать audit-vs-chain-authority trust boundary. Само слово «BLS» не доказывает generation provenance, а один старый-key object без exact receipt остаётся запрещённым и в упрощённой модели.

## Открытые вопросы перед любым изменением

- Доменный контракт пользователь уже задал: решение принимает сеть через Vote, exact accepted history не отменяется, новый узел не участвует повторно в прошлом решении. Independent transcript audit при нарушении honest-q assumptions не следует автоматически добавлять к этому требованию. Текущий design.md формально требует такого audit на историческом пути; его изменение нужно согласовать как исправление выбранного механизма, а не снова спрашивать пользователя о заданном поведении.
- Как practically получить ещё не допущенные native objects для самостоятельного manifest reconstruction? Текущие helpers не universal history decoder, а managed admission/fresh fetch до полного proof образуют реальный dependency cycle. Доказать достаточность данных не значит доказать готовность текущего native fetch.
- Для каждой action отдельно нужны точный алгоритм reconstruction/mapping accepted owner entry↔receipt и preservation latest canonical snapshot; не заменять manifest hash просто Approved/BLS.
- Доступность full historical DKG inputs и произвольного genesis content artifact не установлена для всех режимов. Их нельзя объявлять неизбежной новой transport-функцией без проверки меньшего existing пути.
- Изменения implementation/spec/tasks не выполнялись. Любое предложение менять уже утверждённые historical witness требования требует обычного decision gate, но этим исследованием такое изменение не согласовано.

Не запускались tests, production transfer и изменённый historical verifier. Доказана структура source и условный security argument; drop-in native fresh-fetch compatibility и универсальный decoder остаются непроверенными.


## Дополнительная проверка координатора: ordinary native fetch и причина блокировки

Координатор `/root` независимо прочитал chain admission, manifest binding,
`AcceptedPublicationVerifier`, witness recovery, owner policy и native fetch на том же SHA.

- `vendor/heartwood/crates/radicle-fetch/src/state.rs:370–455` описывает и реализует
  штатный путь: identity → signed refs → data objects → validation → ref application.
  Он переносит native Git history. Родительский метод `run_stage` сначала получает
  pack, затем вызывает `prepare_updates` (`:220–280`); наличие объектов в ODB ещё не
  означает разрешённую authoritative history.
- `vendor/heartwood/crates/radicle/src/identity/doc.rs:1057–1064` вызывает
  `authorize_entry` при document load, до завершения native signature validation.
  `storage/publication.rs:190–235` требует локального policy provider/grant для managed
  owner. Таким образом, дополнительное условие задаёт Outbe adapter через generic hook;
  стандартный Git/COB transport не требует transcript голосования или historical partials.
- `crates/system/radicle/src/collector.rs:562–604` при receipt сначала открывает/создаёт
  локальный repo, затем требует local selected package и historical witness до grants.
  `collector/review.rs:342–399` повторяет это для следующих accepted receipts.
  `publication/store.rs:421–540` требует все сохранённые локальные packages для rebuild.
  Именно local package API сейчас делает историю зависимой от наличия transcript files.
- Frozen fresh-receiver test зафиксировал **`missing identity document`**, а не
  cryptographic `bad/missing witness` rejection. Он доказывает, что данный empty-store
  путь не дошёл до успешного catch-up. Его нельзя использовать как самостоятельное
  доказательство, что native transport принципиально нуждается в новом archive protocol.
  Direct authorization assertion отдельно подтверждает persisted managed owner pin.
- Removing only historical witness checks не удаляет остальные local prerequisites:
  manifest preimage, exact mapping receipt↔COB/sigrefs, genesis inputs и admission/fetch
  ordering. Поэтому «witness избыточен для read authorization» **не равно** «удаление
  одного вызова сейчас автоматически починит fresh fetch».

### Publication manifest и genesis content artifact — разные зависимости

`bootstrap.rs:184–198` использует initial-content artifact только для сравнения его
SHA256 с descriptor, а initial tree выбирает по независимому committed tree OID.
`bootstrap.rs:335–393` строит publication manifest из native owner roots/sigrefs,
objects/envelopes/refs. Сам исходный content artifact отдельно сохраняется store
(`store.rs:362–367`); это не доказательство того, что он всегда входит в native Git
closure. Для literal `[]` bytes известны, для arbitrary artifact hash не даёт preimage.
Потребность повторно иметь его bytes в accepted bootstrap reconstruction — ещё одно
условие текущего verifier. Его необходимость для обычного чтения exact accepted tree
нельзя автоматически выводить из требований к **созданию** genesis repository.

### Практический ответ на вопрос пользователя

1. Проверка actual native bytes/signature и их связи с **уже принятым exact receipt**
   необходима: peer может прислать другие данные, а group-valid подпись сама по себе
   не устанавливает право нового действия старым share material.
2. Обязательное повторное получение q partial frames для каждого исторического read
   не является необходимым разрешением на чтение при заявленном chain-authority контракте.
   Текущий код смешал read authorization с independent signing-transcript audit.
3. Manifest — выбранное представление связи с accepted bytes. При нынешнем body,
   содержащем только его hash, эту связь нельзя просто выкинуть. Но отдельный файл и
   отдельная транспортная подсистема не становятся доменным требованием из-за того,
   что текущий store/API требуют файл. Нужен минимальный способ сохранить exact binding;
   universal reconstruction/transport replacement этим исследованием не реализован.
4. Следовательно, D4 нельзя обосновывать одной фразой «новый узел не участвовал и ему
   нужны старые partials». Предыдущие D4 PASS подтверждали недостающий механизм при
   неизменном design.md, а не доказывали необходимость самого historical audit contract.

Production code, tests, plan, design.md и Beads не менялись. Новые проверки не запускались.
Результат — source-backed исследование и условный security argument, а не готовая правка.
