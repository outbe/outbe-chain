# Host PoC: результат и масштаб

Дата: 2026-09-11. Host: **Apple M4 Max, 14 cores, 36 GiB RAM**, macOS arm64. Rust release; Python 3.14.6/MPyC 0.11.2 без gmpy2/numpy. Кошелёк ограничен **512 000 000 B**, server setup измеряется отдельно. Методика и команды — [README](README.md); фактические роли/покрытие — [COVERAGE_AND_STORAGE](COVERAGE_AND_STORAGE.md).

Новый native P_link для **32 SU укладывается в лимит памяти на host**. Настоящие P_L2, P_link, VSS handoff, private state proofs и трёхпроцессный MPC исполняются без TEE. Это подтверждает работоспособность экспериментальной цепочки в passive модели. Текущая реализация **не готова к миллиарду Tribute**: остаются стоимость нелинейного MPC, объём per-right state и production security/storage adapters.

## Основные измерения

Основной набор — 256 **разных** canonical offers, по 32 разных SU каждый; source IDs между offers не повторяются. Каждому создан собственный P_L2 и P_link. Есть две смены состава держателей долей. Все дальнейшие денежные сценарии выполняет один основной owner, дополнительная начальная Fidelity история и Intex используют отдельные явно указанные fixtures.

| Работа | Измерение на host | Что включено |
|---|---:|---|
| Cold P_link, 32 SU | 4,06 с; 388,17 MB peak RSS | PK load, witness/circuit, proof, self-verify, serialize; fresh process |
| 256 разных P_link, warm | 95,41 с; **2,68 proof/с** | После однократного PK load 3,56 с; generation+self-verify+files |
| Тот же batch целиком | 99,04 с; 442,91 MB peak | PK load и все 256 proofs; лимит 512 MB соблюдён |
| Source: 256 настоящих P_L2 | 54,03 с | Fresh fixture/ACVM/UltraHonk + self-verifications; source role, не обязательная работа wallet при уже готовом Offer |
| Node: P_L2 verify, 256 разных | 295,88 мс | Batch verifier после SRS initialization |
| Node: P_link verify, 256 разных | 476,17 мс | Чтение proofs/public data и crypto check после VK load |
| Обе проверки | 772,04 мс; **≈332 Tribute/с** | Последовательная сумма двух batch measurements; без VSS/admission/consensus |
| Worker: 256 Nod descriptors | **3,50 мс**, 71 168 B | Binary serialization, file fsync и SQLite transaction |
| Public Lysis kernel + worker | **8,73 мс** | Две итоговые лиги в fixture; включает запуск Rust math helper |

Cold P_link контрольного полного запуска с 4 records: 3,93 с и 389,66 MB. Это повтор на том же host, не статистическая оценка latency. MB/GB/TB в этом отчёте десятичные; 388,17 MB ≈370,19 MiB.

После proof verification сериализованный harness выполнил VSS admission 256 records и первую ротацию за **21,35 с: ≈12 records/с**. Внутри: wallet VSS deal 0,91 с суммарно; 768 holder verify/persist вызовов — 14,48 с. Здесь три holders исполняются последовательно отдельными subprocesses с whole-map JSON storage. Это измерение текущего controller, не максимальная пропускная способность распределённых валидаторов. Получить aggregate shares S/S_l у двух holders и проверить opening заняло **0,309 с** после snapshot. Две ротации вместе: 0,52 с dealer computation, 1,73 с recipient verify/persist, плюс orchestration/receipts.

Из 3,50 мс на 256 получается около 73 тыс. **локальных descriptor writes/с** при арифметической экстраполяции. Вместе с kernel — около 29 тыс./с. Это не измерение sustained Nod TPS сети: итог требует prior aggregate/Fidelity, DA, проверки результатов и consensus. Один нетривиальный offline Fidelity snapshot в этом PoC занимает порядка 11–12 с; 255 остальных owners имеют initial-empty league1. Время 256 сложных Fidelity histories не измерено.

## Cold wallet и рост SU

| SU | Constraints | PK на диске | Cold wall | Peak RSS | 512 MB |
|---:|---:|---:|---:|---:|---|
| 1 | 18 850 | 4,00 MB | 0,99 с | 100,24 MB | PASS |
| 16 | 58 601 | 10,67 MB | 2,30 с | 236,14 MB | PASS |
| 32 | 100 913 | 18,74 MB | 3,85 с | 394,67 MB | PASS |
| 64 | 185 801 | 34,92 MB | 6,64 с до остановки | **604,73 MB при остановке** | **FAIL; killed** |

64-SU proof **не завершился**: RSS sampler остановил процесс при превышении лимита. 604,73 MB — зарегистрированный пик прерванного запуска, а не требование полного 64-SU prover. 128/256 SU после этого не запускались. Таким образом, 32 — подходящий измеренный профиль; из этого не следует максимальный размер списка SU протокола. До точного предела между 32 и 64 этот sweep не сужался.

Cold state proofs текущего полного запуска:

| Wallet operation | Wall с PK load | Peak RSS | PK |
|---|---:|---:|---:|
| Claim | 3,10 с | 236,47 MB | 10,69 MB |
| Move/receive/release | 2,40–2,43 с | до 194,97 MB | 8,51 MB |
| Withdraw | 1,33–1,35 с | до 108,74 MB | 4,42 MB |
| Promis mint | 1,76 с | 142,69 MB | 6,02 MB |
| Pledge | 1,75 с | до 139,61 MB | 5,92 MB |

Это только работа wallet. До окончательного money commit runtime также ждёт следующий MPC gate.

## Параллельное создание

Отдельное сравнение: одни и те же 32 разных wallet inputs, новые proofs в каждом запуске, **2 Rayon threads на процесс**.

| Prover processes | Proofs | Wall с cold PK loads | Throughput с loads | Peak RSS каждого |
|---:|---:|---:|---:|---:|
| 1 | 32 | 33,09 с | 0,97/с | 424,46 MB |
| 2 | 16+16 | 19,72 с | 1,62/с | 424,17 / 424,08 MB |

Ускорение wall — **1,68×**; собственно proof phase — с 27,55 с на 32 до ≈13,99 с на 16 в каждом процессе. Создание независимых Tribute действительно параллелится. Сумма peaks двух процессов — 848,25 MB (верхняя оценка из отдельных peaks, не одновременный RSS trace); каждый процесс проходит 512 MB, но двум нужен отдельный общий RAM budget. Это полезно для разных пользователей/host workers. Два процесса по 2 threads не сравниваются как лучший вариант с основным 14-core default batch на 256 proofs: там другой CPU budget.

## Стоимость приватных committee операций

Три настоящих локальных процесса, degree1 sharing. Time включает process startup/connect; RAM — максимальный peak одного actor. Traffic — сумма отправленных application frames всех actors, без handshake/TCP/TLS overhead.

| Сценарий | Wall | Max actor RSS | Framed traffic |
|---|---:|---:|---:|
| Fidelity league, 1 сложная история | 11,61 с | 42,35 MB | 15,78 MB |
| Claim: связать money и cohorts | 4,90 с | 45,78 MB | 7,68 MB |
| Private move / receive | 6,74 с каждый | до 47,51 MB | ≈11,66 MB каждый |
| Promis→Gratis history | 7,76 с | 47,24 MB | 13,36 MB |
| Forced A burn + private LIFO | 10,59 с | 48,19 MB | 18,11 MB |
| Intex: **2 выплаты**, hidden denominator, exact floors | **119,96 с** | 129,32 MB | **153,72 MB** |
| Expiry: оставшиеся 255 прав | **46,76 с** | 138,40 MB | **94,61 MB** |

Intex baseline даёт около 0,017 payout/с на такой трёхпроцессный batch; это недостаточно для масштаба. Expiry здесь импортирует и конвертирует shares каждого права отдельно, хотя его weighted sum линейна. Сложение в source field до дорогой конвертации, с доказанной границей отсутствия wraparound либо limb/carry обработкой, — конкретный путь ускорения; **он не реализован и не измерен**. Числа выше не являются нижней границей стоимости криптографической схемы.

## Байты

| Объект | Размер |
|---|---:|
| P_L2, фактический combined production формат | **8 900 B** |
| P_link Groth16 | **128 B** |
| Nominal commitment | **32 B** |
| P_link public inputs, capacity32 | 58×32 = 1 856 B |
| P_link proving / verifying key | 18 737 488 / 2 120 B; общий cache профиля |
| Uint256 private balance commitment | **128 B**, четыре 64-bit limbs |
| State transition Groth16 proof | **128 B**, дополнительно statement/context/signature и recovery |
| Nod/NoEntitlement experimental descriptor | **278 B** |

Один обычный фактически сериализованный `tribute-1/` занимает **19 165 B**: 8 900 B P_L2, 128 B P_link, две копии Offer JSON (3 069+3 070 B), VSS JSON 2 698 B, три receipts по 339 B, source measurement JSON 283 B. Если в этом же формате хранить только одну копию Offer и убрать measurement JSON, получится **15 813 B**. Это детерминированное удаление дубликатов из измеренного payload, не новая протестированная wire schema.

Первый Tribute дополнительно переносит три genesis cohort notes — он намеренно больше обычного. Размер полного каталога `public/` включает SQL, повторные manifests, отчётные JSON, operation certificates, recovery packets и retained handoff transcripts; его нельзя делить на N и объявлять размером production Tribute.

Фактический primary run: Tribute directories min **19 154 B**, mean **19 232,02 B**, max **34 445 B**; `public/` целиком **9 463 592 B** на момент finish (до записи самого result.json). Финальные текущие holder directories: **229 500 / 229 535 / 229 505 B** каждый, с сохранёнными промежуточными MPC outputs. Wallet0 с дополнительными genesis/recovery fixtures — **12 405 B**; все private role directories перечислены в result.json. Это логический объём содержимого файлов, не filesystem allocated blocks.

Для оценки отдельно от monetary history: epoch1 store одного holder после всех 256 admissions содержит 268 records (256 nominal + 12 genesis limbs) и занимает **130 817 B** JSON. Около 488 B/record в этом sample — значительно больше raw minimum 64 B/share-pair. Перенос этого sample на миллиард дал бы порядка 0,49 TB на holder только такой JSON map; actual large-N layout не измерялся.

## Что означает миллиард Tribute

Это **проекции размеров/необходимой скорости**, не выполненный billion run:

- 1 млрд за 50 часов — **5 556 Tribute/с** в среднем; за 24 часа — 11 574/с.
- Только существующие P_L2: **8,9 TB** на миллиард. P_link добавляет 128 GB. Публичные source metadata, SU, VSS/receipts и replicas идут сверх этого.
- Текущий обычный artifact 19 165 B → **19,165 TB**; описанное удаление двух report/duplicate файлов → **15,813 TB**.
- Nod descriptors: **278 GB** на миллиард; хранение proofs/rights/metadata и replicas добавляется отдельно.
- Минимум nominal private shares: **64 GB на holder** текущего epoch. Три holders — 192 GB, до IDs/indexes/polynomials/cohorts/retained rights. В текущем JSON store фактический объём выше.
- При ротации 2→3 минимум **384 GB secret evaluation payload** на миллиард retained nominal records за одну смену, без commitments/AEAD/metadata.

Ускорить P_link можно независимыми prover-процессами и распределением работы по пользователям. Node verification также можно распараллелить, сохранив сериализованную проверку uniqueness/state finality. Число нужных CPU/валидаторов из одного host batch достоверно не выводится: не измерены sustained queues, WAN, failover и production block limits.

## Ограничения, от которых зависит жизнеспособность

1. **MPC backend.** Исполнен passive MPyC n=3,t=1. Malicious inputs, validated VSS→authenticated-MPC bridge, production committee protocol и публичная проверяемость MPC результата ещё не реализованы. Подписи не заменяют эти свойства.
2. **Storage/rotation.** Whole-map JSON загружается в RAM и переписывается при каждом input. Для большого N это blocker реализации. Нужны indexed/streamed/sharded storage и измеренный handoff; per-right state всё равно нужен для поздних consumers.
3. **Нелинейная арифметика.** Exact private Intex division и checked Fidelity работают, но baseline дорог. Замена backend/арифметического протокола требует новых измерений и сохранения uint256/rounding/context contracts.
4. **Неограниченный SU.** Capacity32 — основной эксперимент, не protocol cap. Холодный prover больших monolithic circuits упирается в RAM; chunk/recursive aggregation design ещё не реализован.
5. **R14.** F сохранён как private returned-limit note и проверен против unclaimed rights. Повторное использование его в следующем публичном PromisLimit не выполнено; требуется отдельное решение disclosure/accounting policy.
6. **Интеграция.** Source registry/funding/call/auction/Metadosis adapters, collateral disclosure profile, true cross-owner payment, production consensus/OCOMP и полный набор upstream mint sources не проверены как production path.

Память 32-SU prover подходит для следующего native mobile прототипа с бюджетом 512 MB. Фактические RAM/time/thermal behavior на устройстве 2/4 GB не измерялись, как и browser/WASM. Хранение PK на диске и одновременное удержание всех PK в RAM — разные вещи: этот PoC загружает только параметры текущей операции.

## Evidence

Полные запуски **lifecycle4-final и lifecycle256-final завершились успешно**; оба имеют `experimental_lifecycle_executed=true` и `full_production_protocol_pass=false`. Логические phases/deadlines и две смены состава исполнены в локальном сценарии: реальное ожидание 50 часов и hourly WAN churn не проводились.

Публичные отобранные JSON: [256-record result](results/host-2026-09-11/lifecycle256-final/result.json), [timings](results/host-2026-09-11/lifecycle256-final/timings.json), [SU growth](results/host-2026-09-11/growth.json), [parallel](results/host-2026-09-11/parallel.json), [security checks](results/host-2026-09-11/security-checks.json), [host](results/host-2026-09-11/host.json). Private fixtures не включены. Каждый полный run фиксирует hashes исходников/включённых production kernels и отклоняет завершение, если они изменились в ходе запуска. Параметры фиксируются отдельными hashes. Более ранние `runs/lifecycle8-*`/`lifecycle4-d` — exploratory snapshots до последних исправлений, не evidence текущей реализации.

Исполнены 4 Rust library tests для uint256/alias/overflow, VSS и AEAD/signatures. Native P_link R1CS check проверил u64 source boundary и отверг подмену source/nominal/blinder; в SU sweep verifier отверг 27/42/58 public-input mutations для 1/16/32 profiles. Реальный P_L2 verifier принял control и отверг мутации каждого из четырёх public inputs и proof body. State verifier отвергает каждую мутацию public input. Сквозные negative cases включают preclose opening, source-set/operation mismatch, отсутствующий или другой денежный certificate, изменённые MPC metadata/request/domain, replay и stale timestamp/version с rollback. Отдельный приватный oracle сравнивает S/S_l, Fidelity league, LIFO, exact Intex payouts/conservation и expiry с исходными fixtures.

[Независимая перепроверка RR-A-01/02/03](STATE_REVIEW_FINAL.md) закрыла найденные ошибки consumer/certificate в её ограниченном scope. Она не является аудитом всей криптосистемы или production security conclusion.
