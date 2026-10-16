# Governments: запуск и проверка

Статус: рабочий runbook реализации, не release acceptance. На source `8987e23ae`
проверен локальный сценарий с native sidecars, настоящими BLS partials и
криптографической reshare, но fixture delivery/finality. Production E2E,
положительный сетевой E2E fresh catch-up managed Outbe узла и финальный комплект CI/evidence ещё не выполнены.
Функциональный путь fresh catch-up подключён; текущая проверка ограничена unit tests и сборкой.
Полный сценарий `@governments-patch-vote` написан; его сетевой запуск оставлен пользователю.

## Отдельный запуск полного E2E

В `testing/e2e-harness/features/radicle.feature` сценарий с тегом
`@governments-patch-vote` проходит production genesis/initial DKG → автоматическое
создание governments → обычный Ed25519 author patch → реальные Vote transactions
четырёх текущих validators → finalized approval точной revision → автоматические
BLS Review Accept и Merge → получение принятой истории новым validator после DKG
activation → обычный native Fetch в пустой независимый Radicle home.
До Vote проверяются open patch и неизменный main; после — exact revision/head,
единственный BLS owner, native signatures, собственные finalized receipts каждого
validator и одинаковые signed refs. Новый validator не голосует повторно.
Проверяющий код не устанавливает refs и не создаёт отсутствующий managed owner pin.

Команды ниже **подготовлены для отдельного запуска, здесь не выполнялись**.
Выполнять из этого worktree на Linux-хосте с уже настроенным SGX/Gramine и Docker,
как остальные сценарии `@radicle @sgx-no-attest`. Используется существующий
artifact build lane: сначала собрать согласованный набор binaries и manifest,
затем запустить только этот сценарий. Manifest хранится вне source tree.

```sh
cargo +1.96.0 run --locked -p outbe-e2e-harness --bin outbe-e2e-build -- \
  --lane radicle-sgx --output /tmp/outbe-governments-artifacts.json

./target/release/outbe-e2e \
  --artifact-manifest /tmp/outbe-governments-artifacts.json \
  --input testing/e2e-harness/features/radicle.feature \
  --tags '@governments-patch-vote' \
  --validators 4 --tee sgx-no-attest --sudo --all --no-cleanup
```

Сценарий использует DKG cadence 96/15 с grace 20 и voting window 24 blocks:
81 block до prepare обеспечивает требуемое publication окно ≥64. Существующий
joiner helper использует те же prepare/grace. Он запускает пятый узел через
FullNode → shareless validator → настоящий DKG/activation, без копирования shares
или готовых publication packages.

Штатный `scenario-*.json` сохраняет `radicle.public_path.governments_patch`:
RID/revision/base/head, proposal/vote transaction hashes, finalized approval,
проверенные receipt/manifest bytes и наблюдения четырёх validators, нового validator
и ordinary reader. `--no-cleanup` оставляет native stores и process logs.
До выполнения сценария это **не runtime PASS и не закрытие P8**; отрицательные
сценарии и обязательный CI полного плана этим happy-path не заменяются.

## Genesis

В обычный seed для `scripts/seed_genesis.py` добавить поле `governments` внутри
существующего `radicle_registry`. Пример для пустого initial tree:

```json
{
  "radicle_registry": {
    "max_repositories": 8,
    "governments": {
      "description": "Network governance",
      "initial_tree_oid": "0x4b825dc642cb6eb9a060e54bf8d69288fbee4904",
      "initial_content_manifest_sha256": "0x4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945"
    }
  }
}
```

Это Git OID пустого tree и SHA256 точных bytes `[]`. Seeder связывает OGD1 с
chain ID и genesis timestamp и записывает descriptor одновременно в genesis config
и RadicleRegistry storage. Описание ограничено 256 UTF-8 bytes. Capacity включает
зарезервированное место для governments. Без поля `governments` автоматический
сетевой repo выключен; существующие permissionless user repositories продолжают работать.

Для governments effective окно `epochLengthBlocks - min(dkgPrepareWindowBlocks,
epochLengthBlocks)` должно быть не меньше 64 blocks. Примеры из проверок genesis:
300/30 и 65/1. Это проверка поддерживаемой конфигурации, а не гарантия доставки
при произвольной сетевой задержке.

RID и owner key вручную в genesis не задаются. Их связывает automatic bootstrap
с существующим DKG group public key и OGD1. Отдельный `rad init` для governments
не нужен. Полный owner secret key не собирается; share остаётся в node provider.

## Процессы и наблюдение

Используются штатные validator node и `scripts/run-radicle.sh`. Script принимает
`<rad-home> <listen> <status-listen> <max-validators> <advertise> [advertise ...]`.
Existing Radicle transport identity должна быть подготовлена обычным node setup;
она используется для Ed25519 NodeId/Noise, а не как владелец governments.
Sidecar и node запускаются от одного UID, home/node directories имеют mode 0700,
private local socket — 0600. Private node socket определяется от native control
socket с суффиксом `.outbe`; вручную выдавать через него peer grants не нужно.

После готовности DKG и собственного finalized state collector автоматически:

1. Готовит одинаковые genesis/native objects и собирает threshold contributions.
2. Передаёт bounded publication attestations через существующий node mailbox.
3. Дожидается принятого chain receipt, проверяет exact native objects/signatures, устанавливает
   accepted snapshot и вызывает штатный seed/announce.

Для наблюдения используются existing RPC `outbe_radicleStatus`,
`outbe_radiclePeers`, `outbe_radicleRepositories` и sidecar log
`governments collector: ...`. Готовность обычной mesh сама по себе не доказывает
принятие конкретного patch. В evidence нужны exact repo/patch/revision, finalized
approval, accepted Review/Merge receipts и actual native refs.

## Patch и смена material

Ed25519 author предлагает обычный patch/revision. Vote фиксирует точные
repo/patch/revision/expected target; later author revision не заменяет это решение.
После finalized Approved сеть автоматически публикует native BLS Review Accept,
затем native Merge в рамках того же решения. Второй Vote для успешного Merge не нужен.

`ApprovedUnmerged` с `Conflict` сохраняет принятый Review и прежний main.
`TargetMoved` также сохраняет Review; для нового expected target нужен новый Vote.
Обычный Review Accept сам по себе не является изменением Git main.

При смене текущего material незавершённая подпись старого поколения не завершается
как новое действие. `BindMaterial` связывает новый public Output, не меняя owner key
и native refs. Новые действия используют current material; уже accepted exact
history остаётся читаемой без current share/quorum. `WaitingForWindow` означает,
что collector не начал действие в недостаточном текущем окне; consensus не ждёт его.

## Обычный пользователь

BLS-совместимый Radicle выполняет обычный clone/fetch governments. Ed25519 author
patch/revision и BLS network identity/Review/Merge/signed refs проверяются автоматически.
Пользователю не нужны участие в Vote, share, DKG Output, witness archive или chain receipts.
Подписанный context не заменяет проверку фактических native bytes, repository key и parents.
В CLI Heartwood 1.10.3 штатная команда обновления — `rad sync --fetch`; новый transport
или отдельная пользовательская команда проверки не вводятся.

## Перезапуск и доступность archive

Сохраняются native Git storage, managed-publication requirements и private collector/
node publication stores. При restart worker перечитывает собственные finalized
receipts, проверяет сохранённые packages и устанавливает latest accepted snapshot
через existing CAS. Удалять journal/owner requirements или вручную двигать refs
для обхода ошибки нельзя: это уничтожает данные проверки вместо восстановления.

Свежий managed Outbe узел читает descriptor, receipts и patch approvals из собственного
finalized состояния. Существующий native fetch получает immutable Git objects владельца;
до обычной проверки identity локальная policy восстанавливает manifests из конкретных
sigrefs/COB/identity signature containers. Только совпадение manifest hash с собственным
receipt и полная native BLS проверка создают grants. Prefetch сам не применяет refs/cache.
Collector сохраняет manifests, а SDK восстанавливает отсутствующий публичный descriptor
из собственного chain reader. Если peer опережает собственную finality, fetch ждёт её
продвижения; recovery plan принимает только продолжение того же immutable receipt prefix.
Без доступных objects узел остаётся в `AwaitingAcceptedPackage` и повторяет обычный fetch.
Обычный Radicle пользователь без managed-конфигурации получает repo штатным fetch.
Предложение D4 о новом archive transport не реализуется. Старые partial/Output и local
attestation slots не являются условием чтения уже принятой истории.

## Сборка и граница совместимости

Root workspace закрепляет Rust 1.96.0; standalone Heartwood workspace закрепляет
Rust 1.98.0. Успешная root сборка не заменяет standalone проверки на его toolchain.
Для governments нужны Outbe BLS fork и validating publication policy; upstream Ed25519
client не получает прав BLS owner через transport identity. Обычный Ed25519 workflow
проверяется отдельно как regression, без изменения его authority.

Локальные команды, действительно выполненные на `8987e23ae` (macOS, root Rust 1.96.0):

```sh
cargo +1.96.0 test --offline --locked -p outbe-radicle-runtime --test governments automatic_bootstrap_vote_review_and_merge_with_one_offline_member_and_restarts -- --nocapture
cargo +1.96.0 clippy --offline --locked -p outbe-radicle -p outbe-radicle-runtime --all-targets -- -D warnings
```

Тест: 1 PASS, 72.19 s; Clippy PASS. Использовались existing Cargo cache,
`CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_INCREMENTAL=0`,
`CARGO_BUILD_JOBS=4`, `CARGO_TARGET_DIR=/private/tmp/outbe-radicle-p1-target`.
Они не являются production E2E/required CI или основанием закрыть P8.
