# D4: доставка уже принятого пакета свежему validating node

Статус: предложение для независимых проверок и явного ответа пользователя.
Зависимая реализация транспорта ещё не начата. Это не acceptance review P6/P8.

## Требуемый результат

Свежий или отставший узел получает уже принятый `governments` и его точную историю
без нового Vote, без нового quorum/signing и без operator owner key. Узел проверяет
пакет относительно собственной finalized chain, устанавливает latest accepted snapshot,
после чего штатный Radicle продолжает replication. История остаётся принятой после reshare.
Нельзя выдавать новые права старому material или разрешать canonical owner refs до proof.

Основание: `design.md` §6 (fresh/lagging receiver, historical exact replay),
раздел witness/manifest (строки 225–246), P6 empty fresh receiver, P7 historical delivery,
P8 independent fetch/current-set transition. Witness по утверждённому контракту
находится вне Git objects/refs; manifest содержит newly created objects, а pinned
content closure доставляется отдельно и проверяется по OIDs.

## Проверенный разрыв

1. `collector.rs::install` и `collector/review.rs::replay_accepted` читают пакет только
   из локального `ReviewPackageStore`. Нет пакета — `AwaitingAcceptedPackage`.
2. `publication/store.rs` хранит body, manifest, witness, bootstrap descriptor/content
   и native objects на диске узла. Store не имеет peer export/import.
3. Native fetch передаёт Git packs/refs. Он не читает эти файлы Outbe store.
   Его own-entry policy требует полного accepted proof. Поэтому нельзя считать native
   fetch доказательством доставки witness или временно разрешить owner entries ради fetch.
4. Прогон `/private/tmp/radicle-fresh-receiver.log` завершился PASS (1 test, 72.19 s):
   существующие три stores проходят bootstrap, Vote/Review/Merge, реальную криптографическую
   reshare с fixture delivery/finality, Conflict/TargetMoved и historical reopen/fetch.
   Четвёртый пустой store видит receipt, остаётся AwaitingAcceptedPackage;
   native fetch возвращает `missing identity document`, owner refs и HEAD отсутствуют.
   Отдельный direct native authorization в том же тесте подтверждает persistent owner pin.
   Это доказательство отсутствия обхода, НЕ доказательство положительного fresh catch-up.

Native fetch/storage файлы здесь только прочитаны. `storage/git.rs` не меняется.
Обычная репликация после полной проверки принятого пакета уже работает в прогоне.

## Предлагаемое решение

Передавать **публичные bytes уже accepted пакета** через существующий authenticated
Outbe peer channel 8 и existing private node↔sidecar socket. Новый service/port/HTTP/RPC
и изменения Heartwood не нужны. Подпись и решение о patch этим транспортом не выполняются.

Порядок работы:

1. Collector получает отсутствующий sequence/receipt hash из собственной finalized chain.
   Через local socket запрашивает этот exact archive у доступных chain peers. Ответивший
   peer не становится authority: он поставляет bytes.
2. На источнике чтение Git/store выполняет sidecar worker, вне consensus и network actor.
   Источник обслуживает только public governments и receipt, который сам видит accepted.
   Предлагаемые типы данных: body/manifest/witness/bootstrap inputs и immutable Git objects
   по OID, необходимые для native package и pinned code/COB closure. Никаких filesystem
   путей от peer, arbitrary refs, shares или приватных ключей в протоколе.
3. Передача request/response с receipt hash, типом данных, OID для Git object и offset.
   Получатель связывает каждый ответ со своим outstanding request и authenticated peer.
   Git kind/OID/bytes проверяются обычным hash_object; metadata проходят существующие
   canonical decoders. Вспомогательный список dependencies источника, если нужен,
   считается только подсказкой, а не trusted manifest или разрешением на owner entry.
4. Получатель получает полный pinned closure, включая native COB/resource parents и
   approved revision base/head/expected target. Использует существующие native evaluator,
   pinned revision selection и `review::object_closure`, а не latest graph/author refs.
   Объекты до полного proof остаются staging data; canonical refs/read grants не меняются.
5. Выполняются существующие Bootstrap/Review/Merge/BindMaterial accepted verifiers по всей
   цепочке receipts: собственные immutable approvals, manifest hash, historical DKG Output,
   q indexed partials, native signatures/bytes и exact refs. Missing/bad proof отвергается.
   Peer не задаёт current ValidatorSet и не обновляет signer material.
6. Только после proof пакет сохраняется в существующие collector и protected node stores
   (они сейчас раздельные). Import не создаёт current signing/attestation token. Existing
   latest snapshot/CAS installer устанавливает состояние; native seed/announce/fetch
   продолжает штатную работу. Accepted packages не подписываются повторно.

Read-only archive exchange должен работать **без current DKG Output, local share и quorum**.
Нельзя использовать `gossip_material()`/`verify_sender()` от OSIG для archive traffic:
они проверяют новые signing contributions и требуют current material. Новый тип сообщения
на том же channel использует уже authenticated peer connection и собственную receipt check.
Peer admission остаётся существующим: transport уже включает validators и admitted
non-consensus peers (`engine/stack/epoch/transport.rs`). Доступность хотя бы одного peer,
сохранившего accepted package, остаётся необходимым условием доставки. Retired signers
не получают разрешения подписывать, даже если отдают historical public bytes.

## Ограничения и поверхность изменения

Использовать существующие границы: channel quota 32 messages/s, endpoint frame size
`MAX_ENDPOINT_FRAME_BYTES` (4387 bytes включая header), pending bound
`MAX_PENDING_REQUESTS` (256), request timeout 5 s; metadata limits manifest/witness 1 MiB.
Payload chunk вычисляется из existing frame limit и header. Нельзя буферизовать весь
repository/archive в actor: chunks идут через bounded queues и private disk staging.
Общий fetch budget берётся из существующего native data-fetch limit, а не нового
настраиваемого ограничения. Переполнение/timeout оставляет старое accepted состояние.
Archive не должен занимать OSIG slots или блокировать delivery подписей/attestations.

Предполагаемые файлы:

- `crates/blockchain/radicle/src/integration/network.rs`, `network/{receive,archive}.rs`:
  bounded request/chunk relay на existing channel; без Git/disk I/O в actor.
- `integration/local.rs`, `local/archive.rs`: private relay и проверенный archive import;
  existing reader/store/identity ownership checks сохраняются.
- `integration/publication/{store,archive}.rs`: public accepted export/import и reuse
  существующих verifiers; approved pinned closure, immutable persistence.
- `crates/system/radicle/src/collector.rs`, `collector/archive.rs`: обработка archive
  requests и missing accepted sequence в existing worker; latest installer переиспользуется.
- existing SDK/runtime tests и P8 E2E. Никаких изменений `vendor/heartwood`, Vote,
  registry carrier, proposer, DKG, peer admission или нового job discovery subsystem.

## Минимальное доказательство реализации после согласования

1. Empty receiver с own finalized receipts получает bootstrap→Review→BindMaterial→Merge
   от другого peer, independently verifies package и exact latest refs, затем native fetch.
2. Current material на принимающем и отдающем archive пути недоступен: exact accepted
   history всё равно восстанавливается. Нет нового Vote, partial или attestation.
3. Missing/tampered manifest/witness/object/Output/approval, wrong receipt/peer/request/offset,
   old-key object без accepted receipt — отказ без canonical refs/read grants.
4. Interrupted/duplicate transfer не подменяет immutable archive или accepted snapshot;
   bounded queue/flood проверяется отдельно от normal signing traffic.
5. Реальный P8 E2E использует production transfer и real finality/DKG, без копирования
   store directories или fixture grants как замены этому пути.

## Что reviewer должен решить независимо

Подтвердить или отвергнуть: доказан ли разрыв; требуется ли именно этот результат
утверждённым планом; закрывает ли предложенный путь весь разрыв; есть ли меньший existing
механизм с теми же гарантиями; не возвращает ли предложение запрещённую D2 storage/fetch
переделку; достаточно ли конкретны peer eligibility, complete closure, separate stores,
resource bounds и fail-closed checks. Не считать этот документ выполненной реализацией.

Reviewers не читают чужие отчёты до собственного verdict. Production/spec изменения
по D4 допускаются только после всех трёх review и явного ответа пользователя.
