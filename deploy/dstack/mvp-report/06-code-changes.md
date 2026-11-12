# Что, где и как изменено

Ветка `mvp-tdx-dstack`, два коммита поверх `main` `607751fc9`:

- `0e7fe0b62` — режимы аттестации защищённых VM и развёртывание на dstack;
- `c2efeeb97` — смешанная сеть, повторный вход, защита ключа от оператора.

Всего: 131 файл, +11098 −202 строк.

## Внешняя поверхность протокола

Все изменения ниже утверждались проектными записками (Р1–Р32, `notes/`).

| Что | Значение | Где |
|---|---|---|
| Новые режимы аттестации | `SevSnpRequired = 0x03`, `TdxRequired = 0x04`, `ConfidentialVmRequired = 0x05` | `crates/blockchain/primitives/src/tee_attestation_v1/network.rs` |
| Новые виды доказательства | `AttestationEvidenceV1::SevSnp` (отчёт 1184 байта + ARK/ASK/VCEK), `::Tdx` (раскладка DCAP) | `…/tee_attestation_v1/evidence.rs` |
| Политика | поле `amd_root_der_hash` (только в режиме 0x05); проекции измерений SEV-SNP и TDX в поля правила | `…/tee_attestation_v1/policy.rs`, `tee_genesis_v1.rs` |
| Данные блока 1 | варианты участника для SEV-SNP и TDX, байт платформы на участника | `…/tee_bootstrap_v2.rs`, `codec.rs`, `validation.rs`, `assembly.rs` |
| Газ | формула как у DCAP | `…/tee_attestation_v1/gas.rs` |
| Правило реестра | повторная регистрация при действующей записи в режимах VM (Р26) | `crates/system/teeregistry/src/v1/mutation*`, `registration.rs` |
| Допустимые сети | режимы VM — только devnet и testnet | `network.rs` |

ABI реестра и формат обмена нода ↔ сервис не менялись: доказательства VM идут
по существующему пути DCAP, новых кодов отказа нет (причины отображены на
существующие).

Режим SGX (`DcapRequired`) не затронут, кроме одного:
закрепление корня Intel (Р28) действует и в режиме SGX.

## Проверка доказательств — `crates/system/tee`

| Файл | Что |
|---|---|
| `src/sev_snp_v1.rs` (новый, 867 строк) | проверка отчёта SEV-SNP: длина → корень AMD по закреплённому хэшу → версия (2, 3, 5) → цепочка ARK→ASK→VCEK → подпись → правила (VMPL 0, без debug, без migration agent, ключ VCEK, равенство версий TCB). Часы, файлы и сеть не читает. Константа `AMD_ROOT_DER_SHA256` (Milan, Genoa, Turin) |
| `src/sev_snp_host.rs` (новый) | сбор сертификатов AMD на стороне ноды: таблица от хоста → каталог `OUTBE_SEV_SNP_CERT_DIR` → встроенные в `sev` |
| `src/tdx_host.rs` (новый) | сбор обеспечения Intel на стороне ноды: цепочка PCK из квоты, остальное из `collateral.json` |
| `src/tdx_report.rs` (новый) | разбор полей квоты TDX |
| `src/dcap_v1.rs` | `verify_tdx_evidence` поверх конвейера DCAP; закрепление корня Intel `INTEL_SGX_ROOT_CA_DER_SHA256` |
| `native/qvl_wrapper.c`, `src/native_qvl.rs` | вход `tdx_qv_verify_quote` библиотеки Intel QVL 1.26 |
| `src/node_host/durable_submission.rs` | поддержка новых видов доказательства |
| `Cargo.toml` | зависимость `sev = "=8.0.0"` без OpenSSL, признак `sev-snp` |
| `tests/fixtures/` | настоящий отчёт Milan с сертификатами, отчёт симулятора, настоящая квота TDX v4 с обеспечением |

## Сервис — `bin/outbe-tee-enclave`

| Файл | Что |
|---|---|
| `src/confidential_vm.rs` (новый) | работа сервиса в защищённой VM: `configure(платформа, описатель, манифест)`, собственная идентичность из отчёта, ключ запечатывания случаен для процесса (состояние не переживает перезапуск), правила выдачи |
| `src/measured_manifest.rs` (новый) | разбор манифеста, сверка с `HOST_DATA` / `MRCONFIGID`, раздел `outbe_release`, правило «образ в списке и манифест тот же» |
| `src/tsm.rs` (новый) | чтение отчёта через интерфейс ядра `configfs-tsm`, один код для обеих платформ |
| `src/platform_attestation.rs` (новый) | источник доказательства: Gramine SGX либо TSM |
| `src/sev_snp_probe.rs` (новый) | `--probe-attestation`: печать фактов платформы |
| `src/dcap_verifier.rs` | выбор проверяющего по виду доказательства; `onboarding_recipient_admitted` — выдача ключа по правилам манифеста, а не по политике из запроса ноды |
| `src/transport/dispatch/onboarding.rs` | экспорт ключа для обновления образа в VM отклоняется |
| `src/run.rs` | флаги `--platform auto|sev-snp|tdx`, `--network-descriptor`, `--app-manifest` |
| `src/gramine.rs`, `sgx_sealing.rs`, `seal.rs`, `initialization*`, `transport/mod.rs` | точки подключения режима VM; `KeyPolicy::ConfidentialVmMemory` |
| `src/errors/platform.rs` | ошибки платформы |
| `src/lib.rs` | объявления модулей; атрибут, разрешающий неиспользуемый код `measured_manifest` без `sev-snp` |
| `Cargo.toml` | признак `sev-snp` |

Удалено в ходе работы: источник ключа от гостевого агента dstack
(`dstack_guest_agent.rs`) и производный ключ процессора SEV-SNP — после
решения хранить ключ только в памяти.

## Нода и движок

| Файл | Что |
|---|---|
| `crates/blockchain/engine/src/tee_bootstrap/submission.rs` | сборка доказательства SEV-SNP и TDX при старте сети; определение платформы по доказательству сервиса |
| `…/tee_bootstrap/validation.rs` | проверка набора основателей: `admits_evidence` вместо равенства вида и режима |
| `…/engine/src/args.rs` | аргументы для каталогов сертификатов и обеспечения |
| `crates/blockchain/evm/src/tee_attestation_activation.rs` | активация новых режимов |
| `crates/blockchain/operator/src/tee/*` | статус и продление: явный отказ для режимов VM |

## CLI и genesis

| Файл | Что |
|---|---|
| `bin/outbe-cli/src/commands/tee/join/*` | сборка доказательства VM; `ensure_joinable_binding` с учётом режима (Р26); исправление `await_finalized_join_target` |
| `bin/outbe-chain/src/tee_genesis.rs` | `tee genesis --mode sev-snp-required | tdx-required | confidential-vm-required` и их параметры |
| `bin/outbe-genesis-cli/src/attestation.rs`, `network.rs` | те же режимы в `network.yaml` |

## Развёртывание — `deploy/dstack/` (всё новое)

| Файл | Что |
|---|---|
| `build-binaries.sh`, `build-binaries-in-container.sh` | воспроизводимая сборка бинарников |
| `build-image.sh` | сборка образа: dstack + патчи + `rootfs/` |
| `patches/0001…`, `0002…` | диск данных без шифрования; `sev` 8.0.0 в dstack |
| `rootfs/usr/local/lib/outbe/provision.sh` | первичная раскладка бандла |
| `rootfs/usr/local/lib/outbe/run-role.sh` | запуск сервиса, Radicle, ноды; повторный вход |
| `rootfs/usr/lib/systemd/system/outbe-*.service` | четыре юнита |
| `make-bundles.sh` | генерация сети: бандлы, genesis с политикой, манифест с `outbe_release`; расчёт `HOST_DATA` и `MRCONFIGID` |
| `host-setup.sh` | подготовка хоста, сеть VM, `expose`, firewall |
| `app-compose.json` | манифест для сети без аттестации |
| `README.md`, `HARDWARE.md` | описание и чек-лист железа |

## Прочее

- `testing/tribute-offer-fixture/` — dev-инструмент для подготовки оффера.
  Написан до решения не трогать оффер; дальше не использовался.
- Строки вида `+1 −0` в тестах и фикстурах других крейтов — новое поле или
  вариант перечисления.

## Тесты

| Набор | Число | Что покрывает нового |
|---|---|---|
| `outbe-primitives` | 418 и наборы | кодирование доказательств и политики, проекции |
| `outbe-tee --all-features` | 213 и наборы | отчёт Milan v2 и v5, квота TDX v4, отказы: чужой корень, подмена полей, подписи, сертификатов |
| `outbe-teeregistry --all-features` | 64 | повторный вход (`v1_tests/rejoin.rs`) |
| `outbe-engine` | 254 | набор основателей |
| `outbe-cli` | 314 | правило входа |
| `outbe-tee-enclave --features mock,sev-snp` | 250 и наборы | манифест, привязка к железу, правило выдачи |

`clippy -D warnings` и `fmt --check` чисты для всех перечисленных, включая
сервис без признаков.

На чистом клоне тесты `outbe-tee` с `sev-snp` не собираются: не хватает двух
файлов с идентификатором чипа (ТД-14).
