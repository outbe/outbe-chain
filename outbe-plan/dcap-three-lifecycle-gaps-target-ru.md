# DCAP: prerequisite и три lifecycle gaps

Статус: целевое поведение для реализации и проверки.

Исходная точка: `main`, коммит `9caeba1`.

## Цель и границы

После одного обязательного prerequisite документ закрывает ровно три lifecycle
задачи:

0. сделать уже существующий `DcapRequired` join и разовую выдачу offer key
   реально исполнимыми;
1. добавить автоматический renewal и поддержанный CLI;
2. исключать validator с истёкшей attestation из следующего consensus set;
3. запрещать enclave upgrade, пока candidate не получил постоянный offer key.

P0 не является новой lifecycle-функцией. Это блокирующий дефект фундамента:
сейчас production command matrix безусловно запрещает обе offer-key команды,
которые вызывает реализованный join. Без P0 FullNode и post-genesis Validator не
могут получить первый binding, поэтому полная проверка трёх задач невозможна.

Production wiring для remote-session admission в эту работу **не входит**. Оно
остаётся отдельной deferred-задачей и не является способом закрыть P0 или
same-platform upgrade.

Также вне scope:

- light client или `FinalityFollower` внутри enclave;
- cross-machine upgrade и двухфазный candidate-slot в Registry;
- mid-epoch удаление validator из текущего committee;
- recovery offer key через governance, peers или оператора;
- FullNode role gate после expiry;
- fail-open допуск expired validator ради сохранения quorum;
- общий рефакторинг TEE lifecycle и production networking.

Testnet использует `DcapRequired`; devnet имеет другой chain ID/genesis и не
является fallback. Потерянный постоянный offer key не восстанавливается.

## P0. Исполнимый первоначальный join и разовая выдача offer key

### Текущий блокер

`registerEnclave` после успешной проверки evidence вызывает
`emit_offer_key_sealed_for_registry_v1`. Тот просит локальный enclave выполнить
`SealOfferKeyForRegistry`. После финализации транзакции CLI передаёт полученный
artifact target enclave через `IngestSealedOfferKeyForRegistry`.

Обе команды сейчас имеют класс `FinalizedAuthorizationRequired`, а этот класс
в production запрещён при любом профиле и состоянии. В результате реальный
`DcapRequired` register завершается `PrecompileError::Fatal`. Unit-тесты этого
не обнаруживают, потому что подставляют test-only sealer.

### Выбранная семантика

Используется Secret-Network-подобная разовая выдача постоянного genesis offer
key exact attested recipient. Она purpose-bound к успешной проверке конкретной
регистрации, но не требует отдельного finality verifier внутри enclave.

Source enclave выдаёт детерминированно зашифрованный artifact только как часть
успешного enclave-resident DCAP verification flow для `RegisterEnclave`:

1. enclave получает bounded canonical evidence, active policy, consensus
   timestamp, node signature и candidate enclave signature;
2. проверяет QVL, exact `RegisterEnclave` intent и обе подписи;
3. проверяет chain/genesis, профиль, exact recipient и offer-key epoch;
4. только для принятой регистрации шифрует resident offer-key material exact
   `recipient_x25519`;
5. возвращает verification verdict вместе с optional onboarding artifact;
6. Registry принимает artifact только из результата этой же проверки и
   публикует `OfferKeySealedForRegistryV1`.

Generic production-команда, которой host может выбрать произвольный recipient,
не разрешается. Source sealing становится частью deep verification operation,
а не отдельным raw `Seal` после неё.

Технически это один purpose-bound request, а не два независимо разрешённых
вызова. Commitment upload-протокола включает operation, canonical evidence,
active policy, consensus timestamp, node signature и enclave signature.
`Finish` возвращает signed local outcome с verdict и optional onboarding
artifact. Локальная attestation-подпись результата проверяется вызывающей нодой,
но в consensus-visible event попадает только детерминированный artifact.

Все consensus-ноды уже имеют один OST3 offer secret и поэтому для exact request
получают одинаковый artifact. Существующий deterministic static-static ECDH
сохраняется, но для onboarding используется отдельная context-bound derivation:

- `context_hash` коммитит chain/genesis, полный intent hash, node/enclave
  identities, recipient, offer public key и key/tribute-offer epoch;
- AEAD key и nonce выводятся из static-static shared secret и `context_hash` с
  отдельными onboarding domain tags;
- оба onboarding tag отличаются друг от друга, от `DKG_SHARE_INFO` и от
  существующего `REGISTRY_NONCE_INFO`; DKG и onboarding не делят HKDF domain;
- exact replay даёт те же байты;
- другой intent для того же recipient получает другой AEAD key/nonce.

Каноническая derivation фиксируется независимо для обеих сторон:

```text
shared     = X25519(offer_secret, recipient_public)
key        = HKDF-SHA256(context_hash, shared, ONBOARDING_KEY_INFO_V1)
nonce_okm  = HKDF-SHA256(context_hash, shared, ONBOARDING_NONCE_INFO_V1)
nonce      = nonce_okm[0..12]
```

`context_hash` уже включает recipient, поэтому exact source и target получают
одинаковую derivation, а разные contexts криптографически разделены.

Последний пункт обязателен: нельзя просто добавить меняющийся intent в plaintext
существующего deterministic seal, потому что это повторно использовало бы один
AEAD key/nonce для разных plaintext.

Artifact канонически связывает как минимум:

- chain ID и genesis hash;
- hash registration intent и node identity;
- recipient key и enclave identity;
- постоянный offer public key и key/tribute-offer epoch.

Target ingest разрешён только initialized, keyless enclave, чей manifest и
recipient совпадают с artifact. Он проверяет chain/genesis, intent binding и
ожидаемый on-chain offer public key, сохраняет ключ write-once, fsync-ит blob и
после reopen снова сравнивает public key. Существующий ключ не перезаписывается.

Ingest не доверяет nonce из wire artifact. Он канонически пересчитывает
`context_hash`, static-static shared secret, expected AEAD key и expected nonce
из собственного recipient secret и exact offer public key. Если nonce остаётся
в wire format для framing/диагностики, он обязан byte-for-byte совпасть с
expected nonce; AEAD open использует пересчитанный expected nonce. Несовпадение
отклоняется до decrypt и не отличает tag failure от других invalid-artifact
ошибок наружу.

Для этого target-команда получает отдельный capability class
`KeylessOnboardingArtifact`: host может передать только полный sealed artifact и
его exact public context, но не secret или выбранный source key. Криптографически
невалидный, чужой или контекстно несовпадающий artifact ничего не меняет. Raw
source `SealOfferKeyForRegistry` остаётся запрещён при любом профиле.

CLI считает join успешным только после финализации transaction receipt,
проверки exact binding в Registry, ingest и durable reopen. Само наличие ключа
не даёт consensus membership: canonical Registry/ValidatorSet admission
проверяется отдельно.

### Принятая reorg-семантика

Sealing выполняется при исполнении registration transaction, до её финализации.
Поэтому enclave с валидными DCAP evidence, разрешённым measurement и точными
node/enclave signatures может получить offer key, даже если блок позднее
исчезнет при reorg и binding не останется в canonical state.

Это осознанное свойство выбранной одноразовой модели:

- получатель всё равно является exact допустимым enclave, авторизованным своей
  NodeHost identity;
- reorg не даёт ему consensus membership или canonical binding;
- выданный ключ нельзя отозвать или доказуемо удалить;
- повторный canonical join использует обычные Registry nonce/id rules;
- если когда-либо потребуется выдача строго после finality, понадобится
  отдельный двухфазный протокол. Он не маскируется под P0.

### Места реализации

- `bin/outbe-tee-enclave/src/dcap_verifier.rs` и `transport.rs` — purpose-bound
  verification-and-seal и keyless exact ingest;
- `crates/system/tee/src/client.rs`, `dcap_protocol.rs` и `protocol.rs` — typed
  bounded result и artifact;
- `crates/system/teeregistry/src/v1.rs` и `v1_precompile.rs` — emission только
  из результата успешной проверки;
- `bin/outbe-cli/src/commands/tee.rs` — finality, exact-binding, ingest и reopen
  checks.

### Критерии проверки P0

- [x] Реальные production Validator и FullNode завершают `DcapRequired` join.
- [x] Невалидные quote, policy, node/enclave signatures или recipient не создают
  onboarding artifact.
- [x] Raw host-selected source sealing остаётся запрещённым.
- [x] Две production source-ноды с одним OST3 key возвращают byte-identical
  artifact для exact request.
- [x] Exact retry возвращает те же байты, а другой intent для того же recipient
  использует другую context-derived key/nonce domain; тест фиксирует отсутствие
  AEAD nonce/key reuse между разными context hashes.
- [x] Onboarding key/nonce tags отличаются друг от друга и от всех DKG registry
  seal domains; тест pin-ит exact protocol constants.
- [x] Ingest пересчитывает expected nonce из exact public context и отклоняет
  изменённый wire nonce до AEAD open; wire nonce никогда не является authority.
- [x] Artifact другого chain, intent, node, enclave, recipient, offer key или
  epoch отклоняется target enclave.
- [x] Ingest работает только для keyless enclave и не перезаписывает blob.
- [x] Crash после записи восстанавливается только через durable reopen exact
  blob; partial/corrupt state fail-closed.
- [x] Reorg-тест фиксирует принятую семантику: recipient может уже владеть
  ключом, но без canonical binding CLI не объявляет join успешным и нода не
  получает consensus admission.
- [x] Unit-тесты не подменяют production command matrix test-only sealer-ом в
  единственном доказательстве работоспособности.

## 1. Автоматический renewal и CLI

### Проблема

Registry поддерживает `RenewEnclave`, но production-нода не строит и не
отправляет renewal автоматически. Существующий replacement journal отвергает
`RenewEnclave`, а transaction/RPC код живёт внутри bin-only `outbe-cli`.

### Общий flow

Validator и FullNode используют один reusable renewal service. Его вызывают:

- background worker работающей ноды;
- `outbe-cli tee renew-now`;
- `outbe-cli tee status` для read-only состояния и diagnostics.

Service:

1. читает финализированный binding, active policy и ближайший DKG freeze;
2. сверяет NodeHost identity, manifest, enclave и resident offer public key;
3. строит exact-next `RenewEnclave` intent;
4. получает свежие quote и PCS collateral и обе подписи;
5. durable-сохраняет попытку до первой отправки;
6. подписывает и отправляет transaction через настроенный relay signer;
7. ждёт финализации и сверяет exact новый binding.

Renewal сохраняет node/enclave/binding identities, manifest, recipient и offer
key. `registration_version` и `renewal_nonce` увеличиваются ровно на один.

### Durable renewal journal

В node data dir существует один owner-only journal и lock для renewal. Запись
не содержит секретов, но содержит всё необходимое для точного retry:

- hash и canonical bytes intent/evidence;
- node/enclave signatures и calldata;
- relay address, chain ID, account nonce и подписанные raw transaction variants;
- requested lease, collateral validity ceiling и safety margin;
- исходный Registry version/nonce и transaction hashes;
- состояние `Prepared`, `Submitted`, `Finalized` или `Abandoned`.

Пока сохранённый submission ещё может быть принят, restart, worker и CLI
повторяют exact evidence/signatures. Они не создают конкурирующий Registry
intent.

Если финализированный consensus timestamp уже сделал старые collateral или
requested lease окончательно непроводимыми, а Registry всё ещё содержит
исходный binding, попытка атомарно помечается `Abandoned`. После этого можно
получить свежие quote/collateral и создать новый intent с тем же требуемым
`current + 1` Registry renewal nonce. Relay account nonce читается заново или
безопасно заменяется только после доказанной непроводимости старого calldata.

### Submission и операционная конфигурация

Существующие RPC и transaction signer из CLI выносятся в reusable слой, а не
пишутся заново. Node daemon получает явную конфигурацию funded relay signer.

- Validator может явно использовать отдельный relay key или свой EVM key.
- FullNode получает новый отдельный funded relay key; Reth P2P key не является
  EVM signer.
- Relay оплачивает transaction, но не является attestation authority.
- NodeHost identity key по-прежнему подписывает registration intent: Validator
  использует свою установленную node identity, FullNode — persistent Reth P2P
  identity. Relay key не может заменить ни одну из этих подписей.
- Недостаточный balance, account-nonce conflict, RPC/PCS/PCCS outage и
  transaction replacement видны в `tee status`, logs и metrics.

Host wall clock используется только для wakeup. Eligibility, expiry и
окончательная непроводимость определяются финализированными consensus
height/timestamp.

### Freeze-relative alerts

Операционный deadline — ближайший freeze, а не просто `valid_until`.

- Worker читает тот же DKG schedule, который определяет `freeze_height`.
- Если текущий lease может не покрыть ближайший freeze, renewal становится
  обязательной работой, как только Registry открывает final-third window.
- Critical alert поднимается, если renewal ещё не финализирован к
  `freeze_height - finality_safety_margin_blocks`.
- Фактическая eligibility всё равно вычисляется по реальному timestamp exact
  freeze header; прогноз используется только для раннего alert.

### Критерии проверки

- [x] Validator и FullNode автоматически renew-ятся через один service.
- [x] `renew-now` использует тот же service; `status` не мутирует state.
- [x] До final-third Registry отказывает без изменения state.
- [x] Restart в `Prepared`/`Submitted` повторяет exact submission.
- [x] Дубликат меняет Registry один раз.
- [x] Окончательно expired submission атомарно abandon-ится; свежая попытка
  использует exact next Registry nonce и свежие evidence.
- [x] Relay account nonce replacement не позволяет одновременно провести две
  разные renewal-попытки.
- [x] FullNode работает с отдельным funded relay signer.
- [x] Ошибки PCS/PCCS/RPC/balance/nonce видны оператору и не продлевают lease.
- [x] Warning/critical alerts рассчитаны относительно ближайшего freeze.
- [x] Identity, manifest и offer public key до и после renewal совпадают.

## 2. Expiry и следующий consensus set

### Точное правило

Expiry не меняет текущий committee посреди эпохи. Но при freeze следующего
DKG/reshare target каждый `ACTIVE` или подтверждённый `PENDING` кандидат должен:

1. пройти существующие правила `ValidatorSet`;
2. иметь ready, non-expired Validator binding в `TeeRegistry` на timestamp exact
   finalized freeze block.

Expired validator не попадает в новый target. Fail-open правила «если осталось
мало — включить expired» нет.

### Реальное место фильтра

`outbe-teeregistry` уже зависит от `outbe-validatorset`, поэтому обратный вызов
из `get_reshare_target_set` создал бы Cargo cycle. Композиция выполняется в
engine:

1. `stack.rs::refresh_validator_set_at_height` получает exact block hash,
   header и state на `freeze_height`;
2. `engine/src/validators.rs` читает базовый target из `ValidatorSet`;
3. там же каждый кандидат проверяется через read-only `TeeRegistry`; engine
   отдельно сохраняет filtered target и канонический ordered
   `tee_expired_target_exclusions` — адреса, удалённые именно TEE-expiry
   фильтром, а не отсутствием в результате DKG;
4. read-only provider получает chain/genesis, block number и timestamp exact
   freeze header. Нулевой timestamp запрещён для этого API;
5. frozen DKG state переносит exclusions до boundary; bounded unique list и её
   commitment входят в canonical `DkgBoundaryArtifact` и сравниваются voting
   validators вместе с filtered target.

Local clock и latest RPC head не участвуют.

### Что именно обеспечивает consensus

Честные voting validators строят одинаковый filtered target и одинаковый
pending boundary. Proposal без фильтра, с другим target или без ожидаемого
boundary отклоняется при сравнении с локальным pending artifact. То же относится
к отсутствующему, переставленному или изменённому
`tee_expired_target_exclusions`.

STF при историческом replay не пересчитывает DCAP eligibility на freeze height.
Он проверяет внутреннюю согласованность уже сертифицированного boundary и
применяет его. Поэтому гарантия формулируется как notarization текущим
committee, а не как самостоятельный state-transition invariant eligibility.

### State transition исключённого validator

Сейчас такого branch нет: `activate_reshared_set` очищает share у всех, но
оставляет пропущенный `ACTIVE` в статусе `ACTIVE` и держит
`pending_set_change = true`. Реализация добавляет новый узкий branch и расширяет
boundary/hook input каноническим `tee_expired_target_exclusions`.

Обычное отсутствие в `new_active_set` недостаточно для демоушена. Live DKG может
завершиться подмножеством frozen target, поэтому валидный validator может не
попасть в `output.players()` по причине участия в церемонии, не связанной с TEE.
Только membership в сертифицированном `tee_expired_target_exclusions` доказывает
нужную для этого slice причину.

При atomic boundary activation для каждого адреса из exclusions:

- текущий `ACTIVE` становится `PENDING`, теряет `has_bls_share`, а его
  `val_join_confirmed` очищается;
- текущий `PENDING` остаётся `PENDING`, но `val_join_confirmed` очищается;
- `EXITING`, `UNBONDING`, `JAILED` и остальные статусы идут прежними ветками и
  этим правилом не затрагиваются.

Это не заставляет STF заново проверять historical attestation, но делает
notarized expiry-result частью replayable state. `ACTIVE`, отсутствующий в
`new_active_set`, но отсутствующий и в TEE exclusions, сохраняет существующую
семантику: остаётся `ACTIVE` без share и `pending_set_change = true` запускает
обычный retry reshare. Этот slice не меняет DKG non-participation policy.

Возврат требует двух условий: финализированного renewal и явного
`confirmValidatorReady()`. После этого validator может войти только в следующий
обычный reshare, снова пройдя TEE filter.

### Strict halt и observability

Если после фильтрации target не способен завершить существующий DKG, новый
committee не активируется. После существующего activation grace validators
завершают работу с ошибкой. Для testnet это принятый strict fail-closed режим.

Он не скрывается за recovery или автоматическим продлением expired bindings.
Выживаемость обеспечивают P0, durable renewal, freeze-relative alerts и
достаточный lease overlap.

На freeze обязательны structured log и metrics:

- freeze height/timestamp и target hash;
- каждый исключённый validator, binding и `valid_until`;
- eligible/expired counts;
- прогноз достижения следующего threshold и critical alert при риске halt.

### Критерии проверки

- [x] Expiry не меняет текущий committee mid-epoch.
- [x] Expired `ACTIVE` и confirmed `PENDING` отсутствуют в следующем target.
- [x] Renewal, финализированный до freeze, сохраняет eligibility.
- [x] Renewal после freeze не меняет frozen target.
- [x] Проверка выполняется в engine по exact freeze header timestamp.
- [x] Provider с нулевым/отсутствующим block context fail-closed.
- [x] Proposal с unfiltered или иным target отклоняется честными voting
  validators.
- [x] Boundary коммитит exact canonical `tee_expired_target_exclusions`; duplicate,
  non-canonical, missing или изменённый список отклоняется.
- [x] После boundary TEE-expired `ACTIVE` становится `PENDING`, теряет share и
  `join_confirmed`; TEE-expired confirmed `PENDING` теряет `join_confirmed`.
- [x] Валидный `ACTIVE`, пропущенный DKG output, но отсутствующий в TEE exclusions,
  не демоутится и сохраняет существующий `pending_set_change` retry path.
- [x] Без нового `confirmValidatorReady()` он не возвращается в target даже
  после renewal.
- [x] Недостаточный filtered target приводит к существующему strict halt, а не
  допуску expired validator.
- [x] Logs/metrics называют exact исключённых validators и ближайший freeze.
- [x] Достаточно четырёх логических validators; 32-validator тест не требуется.

## 3. Offer-key-ready upgrade до transition

### Проблема и выбранная граница

Текущий NodeHost умеет вести active A и candidate B, но production orchestration
не вызывает этот flow, а transition может сделать B активным в Registry до
появления постоянного offer key в B.

Для testnet реализуется same-platform, same-signer continuity через
MRSIGNER-sealed `sealed_root.bin` **до** transition.

Существующая on-chain delivery является правильным строительным блоком для
cross-machine migration, но не решает порядок сама по себе: transition сразу
supersede-ит A, а B сможет ingest-ить event только позже. Crash в этом окне
оставляет on-chain B без ключа и без допустимого rollback к A.

Безопасный cross-machine flow требует отдельного двухфазного Registry protocol
`stage candidate → deliver/key-ready → activate`. Это отдельный будущий scope,
а не скрытая часть текущего upgrade.

MRSIGNER continuity — осознанная уже существующая trust boundary. Она позволяет
любому enclave того же signer и допустимого SVN на этой платформе распечатать
root; этот slice не превращает её в exact-MRENCLAVE sealing. DCAP policy и
`TransitionKeyReadyProofV1` не дают неподходящему measurement стать canonical B,
но не доказывают, что host никогда локально не запускал другой same-signer
enclave. Для testnet same-platform upgrade это принято; устранение этой границы
потребовало бы двухфазной enclave-to-enclave delivery и относится к отдельному
cross-machine/generalized design.

### Целевой same-platform flow

1. A остаётся active в finalized Registry и committed startup manifest. После
   включения transition в ещё не финализированный block execution head может
   уже показывать B, но это не является authority для local promotion.
2. После governance staging successor policy операторский workflow создаёт
   candidate manifest с новой enclave identity для B в отдельном fresh tee-dir.
   Процессом sidecar по-прежнему управляет внешний deployment manager; workflow
   имеет явные проверяемые checkpoints `B stopped` и `B restarted`, сохраняет
   прогресс и безопасно продолжается после повторного запуска команды.
3. Persistent NodeHost key/state остаётся общим в node data dir; он не
   копируется из A в B. В tee-dir B не копируются manifest,
   `sealed_identity.bin` или NodeHost authorization blobs A.
4. B останавливается. Upgrade flow копирует только `sealed_root.bin` A в fresh
   tee-dir B и fsync-ит файл и directory.
5. B запускается повторно, потому что sealed root загружается только на boot.
   Он unseal-ит постоянный key и сравнивает resident public key с Registry.
6. Только key-ready B генерирует transition quote и
   `TransitionKeyReadyProofV1`, связанный с exact intent hash и offer public key.
7. Proof входит внутрь canonical `AttestationEvidenceV1`; ABI
   `transitionEnclaveMeasurement(bytes,bytes,bytes)` и selector не меняются.
   Canonical типы живут в `crates/blockchain/primitives/src/tee_attestation_v1.rs`.
8. Registry проверяет quote, signatures, proof и совпадение offer public key до
   изменения binding.
9. После финализации transition NodeHost через существующий finalized-state
   adapter строит opaque authorization и атомарно промотирует уже key-ready B.

`TransitionKeyReadyProofV1` подписывается persistent attestation key candidate,
который уже связан quote/report-data с candidate intent. Его domain включает как
минимум chain/genesis, exact transition intent hash, candidate manifest hash,
transition nonce и resident offer public key. Поэтому proof от другого
candidate, перехода или chain не переиспользуется.

`predecessor_manifest_hash` не является consensus-полем proof: Registry не
хранит initialization manifest A, а enclave B не получает его как доверенный
вход. Связь candidate с committed A остаётся локальным fail-closed инвариантом
существующего NodeHost candidate journal. Registry проверяет только те поля
proof, которые он может независимо связать с canonical intent, текущим binding,
global offer public key и candidate attestation key; NodeHost дополнительно
сверяет candidate manifest hash перед отправкой и promotion.

Proof нельзя получить отдельной raw host-командой: production enclave создаёт
его только при resident exact offer key. Так как DCAP lane ещё не запущен в
testnet, изменение canonical evidence layout выполняется без legacy decoder или
fallback.

### Upgrade deadline и renewal

Transition разрешён только пока successor policy staged и
`block_number < activation_height`. После activation старая policy больше не
renew-ит A, а transition уже закрыт.

Upgrade worker поэтому:

- обнаруживает staged successor и её activation height;
- прекращает начинать обычный renewal A, который не покрывает безопасный
  transition window;
- поднимает warning/critical deadline alerts;
- требует key-ready B и finalized transition до activation safety margin;
- после пропущенного cutoff останавливает автоматические попытки и сообщает
  terminal operator error, а не выбирает legacy/fallback путь.

### Crash/restart

- Root скопирован, B ещё не перезапущен: A active, transition не отправлен.
- B key-ready, submission не финализирован: повторяются exact durable bytes; A
  остаётся finalized active и committed startup target.
- Transition есть только в unfinalized execution head: restart не промотирует B
  по receipt/latest state и сохраняет committed A; staged candidate/submission
  остаются для exact retry или canonical rollback.
- Transition отклонён: A active, B не промотируется.
- Transition финализирован, local promotion не завершена: restart завершает
  promotion exact B; ключ уже durable.
- B не unseal-ит root или public key не совпадает: Registry не меняется.
- После finalized B fallback к A запрещён.

### Критерии проверки

- [x] Production operator flow полностью создаёт, перезапускает, аттестует,
  отправляет и промотирует B; это не только библиотечный gate.
- [x] Transition без valid `TransitionKeyReadyProofV1` отклоняется.
- [x] Wrong intent/binding/manifest/chain/offer key/signature/replay отклоняются.
- [x] Повреждённый или отсутствующий `sealed_root.bin` не меняет Registry.
- [x] В tee-dir B копируется только sealed root; общий NodeHost state не
  копируется и сохраняет тот же authorization hash.
- [x] A остаётся canonical-finalized binding и committed startup target до
  finality, а B уже key-ready до on-chain transition.
- [x] Receipt или unfinalized latest state не могут создать promotion authority;
  её даёт только exact finalized Registry binding.
- [x] Все crash boundaries восстанавливаются без keyless promotion.
- [x] Worker обнаруживает staged policy и fail-closed обрабатывает пропущенный
  activation cutoff.
- [x] Validator и FullNode проходят один порядок.
- [x] Cross-machine/two-phase flow не заявляется реализованным.

## Deferred: production remote-session networking

Peer discovery, ticket delivery и production Noise transport остаются
отдельной deferred-задачей. Они не требуются для P0, renewal, strict expiry или
same-platform upgrade.

## Порядок реализации и итоговая проверка

Порядок обязателен:

1. P0 production join/key delivery;
2. durable renewal, relay и freeze-relative alerts;
3. strict expiry filter, state demotion и observability;
4. same-platform key-ready upgrade и activation deadline;
5. focused E2E и финальный scope audit.

Logic/state-machine тесты используют controlled finalized height/timestamp и не
ждут час wall-clock. Dedicated `DcapRequired` harness должен уметь поднять
четыре логических Validators и один FullNode. Один SGX host доказывает реальные
quote/QVL, P0 ingest, renewal evidence и same-platform unseal; synthetic time
tests не называются hardware evidence.

Аппаратный focused-runner запускается из корня репозитория после сборки текущего
enclave с единственным project-wide DCAP pin:

```bash
cargo build --release -p outbe-tee-enclave --features native-dcap
OUTBE_TEE_IO_TIMEOUT_SECS=180 cargo run \
  -p outbe-e2e-harness \
  --features dcap-lifecycle-sgx \
  --bin outbe-dcap-lifecycle-sgx -- \
  --enclave-binary target/release/outbe-tee-enclave
```

Runner использует testnet `chain_id = 54322345`, один явный SGX signing key и
отдельные tee-dir. Он не поднимает production networking и не имитирует
32-validator сеть. Успехом считается итоговый JSON, где подтверждены обе роли,
restart renewal, одинаковый permanent offer public key, same-MRSIGNER reopen B,
transition key-ready proof и принятие всех evidence enclave-resident QVL.

Финальный аудит проверяет:

- достижим ли каждый критерий P0 и трёх задач;
- совпадают ли authority, finality и crash order с документом;
- нет ли dev/legacy/recovery fallback;
- не попали ли remote networking, cross-machine migration или новая общая
  security-архитектура обратно в scope.

### Итоговая проверка реализации — 2026-08-03

- Canonical schema, P0, QVL и NodeHost: `outbe-primitives`, `outbe-tee
  --features native-dcap`, `outbe-tee-enclave --lib` — PASS.
- Renewal/operator/relay/journals: `outbe-operator`, `outbe-cli` — PASS.
- Expiry/exclusions/demotion/halt: `outbe-validatorset`, `outbe-consensus`,
  `outbe-engine` — PASS.
- Registry/finality/startup: `outbe-teeregistry --features tee-attestation-v1`,
  focused `outbe-node --test tee_remote_session`, `outbe-chain` — PASS.
- Harness: `outbe-e2e-harness --features dcap-lifecycle-sgx --lib` — PASS;
  аппаратный runner из команды выше — PASS.
- `cargo fmt --check`, `cargo machete`, `git diff --check` — PASS.

Аппаратный runner использовал четыре founder Validator, один отдельный
Validator onboarding-case и один FullNode onboarding-case. Дополнительный
onboarding target не является членом consensus committee; размер проверяемого
committee остаётся четыре, а 32-validator сеть не создаётся.

Один общий repository gate вне этого scope остаётся красным:
`outbe-node --test fee_history_system_gas` дважды не дождался non-empty Reth
payload за 30 секунд. Все 86 node unit tests, 13 stateless tests и четыре
целевых finalized-state tests прошли. Этот gas/fee-history timeout не касается
DCAP lifecycle и намеренно не исправлялся в рамках этой задачи.

Scope audit не нашёл production remote-session networking, cross-machine
candidate-slot, offer-key recovery, governance recovery, ARM, 32-validator
localnet, mid-epoch ejection, FullNode expiry gate или dev/legacy fallback.
`tee_remote_session.rs` изменён только как существующий node-local адаптер exact
finalized Registry state для opaque promotion authority.

## Условия, при которых решение отвергается

Это спецификация реализации, а не доказательство уже написанного кода. Slice не
принимается, если выполняется хотя бы одно из условий:

- P0 оставляет host-callable raw source seal, недетерминированный event или
  повторяет AEAD key/nonce между разными onboarding contexts, делит HKDF domain
  с DKG либо доверяет переданному host'ом wire nonce;
- renewal после restart строит новые evidence при ещё проводимой durable попытке,
  либо FullNode relay становится authority вместо его NodeHost identity;
- freeze filter использует wall clock, latest head или timestamp `0`, либо
  expired validator возвращается без нового `confirmValidatorReady()`, либо
  обычное отсутствие в DKG output ошибочно считается доказательством expiry;
- transition меняет finalized startup target до доказанной resident key
  readiness или допускает promotion по receipt/unfinalized state;
- единственное доказательство любого пункта использует mock/dev seam вместо
  достижимого production `DcapRequired` flow.
