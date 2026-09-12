# Архив измерений гибрида Baby-Jubjub + Ristretto

**Уточнение 2026-09-12:** этот запуск сохранил Baby-Jubjub source/history и добавил Ristretto Gratis через bridges. Он не выполнил цель перенести весь приватный backend на Ristretto. Измерения ниже сохранены как результаты гибрида; исправленный полный вариант находится в [ristretto/](ristretto/README.md). Регрессии 661/638/168 мс относятся именно к этому гибриду.

2026-09-11. Apple M4 Max, 14 cores, 36 GiB RAM, macOS26.7/arm64. Wallet budget **512 000 000 B**. Все MB/KB ниже десятичные. [Host metadata](results/host-2026-09-11/host.json), [команды и схема](README.md).

**Второй вариант реализован и исполнил цепочку на 256 разных TributeOffer по 32 SU.** Он снижает RAM и время ряда денежных prover, позволяет восстанавливать сумму по ключу, но в текущей композиции значительно увеличивает стоимость проверки нодой и размер операции. Дорогой элемент — совместимость нового Ristretto состояния с прежним Baby-Jubjub source/Fidelity через bitwise bridge. Поэтому результат не обосновывает замену baseline целиком этим backend.

## 1. Что действительно прошло

- Настоящие P_L2 и P_link для256 разных offers; по32SU, две ротации VSS, открытие S/S_l после close, Lysis и256 Nod.
- Claim Nod с точной source/economics связью → зашифрованный Gratis; private split/receive, withdraw→публичный COEN, Promis conversion, pledge/release/forced burn.
- Перевод между **двумя отдельными владельцами** с отдельными wallet processes, одинаковой доказанно положительной суммой и атомарным обновлением двух balances/двух Fidelity roots. Получатель участвует в принятии и доказывает отсутствие overflow. Повтор и stale timestamp отклонены с rollback.
- Восстановление сумм из ciphertext и собственного DK; old encryption randomizers не сохраняются и не читаются. Baby/Fidelity witnesses восстанавливаются отдельным прежним механизмом.
- Те же трёхпроцессные passive MPyC Fidelity/Intex/expiry consumers. Для двух дополнительных владельческих transitions выбрана экспериментальная политика sender Out / recipient In.
- **6 Rust tests PASS**: carry/borrow на границах uint256, overflow/underflow и scalar alias, два ключа и amount handle binding, подмена integer между группами, context/proof mutations, zero withdraw, zero cross-credit и подмена Bundle при сохранённом statement.
- Парное сравнение **11 денежных statements на тех же private witnesses** с baseline Groth16.

Финальный run: `variant2/runs/lifecycle256-final`, `experimental_lifecycle_executed=true`, `full_production_protocol_pass=false`. [Результат](results/host-2026-09-11/result.json), [cross-owner](results/host-2026-09-11/cross-owner.json), [private oracle: public verdict](results/host-2026-09-11/oracle.json), [verification](results/host-2026-09-11/verification.json).

Предварительный `lifecycle4-b` тоже завершился, но предшествует трём исправлениям verifier/integration. Он не является финальным evidence. `lifecycle4-a` прерван из-за sandbox-запрета `ps` и не считается RAM measurement.

## 2. Кошелёк: одинаковые входы, холодный запуск

Включены initialization, PK load где требуется, lookup table, proof и self-verification. Компиляция/setup исключены. Новый столбец **включает retained Groth16** для claim/mint/pledge; это последовательные процессы, поэтому RAM берётся как максимум, а время складывается.

| Операция | Baseline, с | Variant2, с | Baseline RAM, MB | Variant2 RAM, MB |
|---|---:|---:|---:|---:|
| Claim Nod | 3,296 | **4,819** | 234,18 | 234,18 |
| Private split/payment | 2,474 | **1,614** | 194,54 | 11,42 |
| Receive | 2,461 | **0,543** | 197,90 | 11,19 |
| Withdraw Gratis→COEN | 1,371 | **0,657** | 106,64 | 10,35 |
| Promis→Gratis | 3,010 | **3,760** | 140,77 | 140,77 |
| Pledge1 | 1,856 | **2,605** | 138,33 | 138,33 |
| Release | 2,496 | **0,546** | 196,59 | 10,86 |
| Cross-owner sender proof | 2,489 | **0,754** | 193,43 | 10,90 |
| Cross-owner recipient proof | 2,467 | **1,183** | 192,12 | 11,44 |
| Intex payout cashout | 1,358 | **0,774** | 109,26 | 10,62 |

В cross-owner строках сравниваются одинаковые локальные money statements; две строки не являются полной latency перевода. Dual-handle/positivity work, recovery, signatures, оба committee gates и commit учитываются отдельно в [timings](results/host-2026-09-11/timings.json). Baseline не выдаётся за ранее реализованный cross-owner protocol.

Сама новая часть claim занимает **1,523 с и 13,89 MB**; остальные3,296с — сохранённый Groth16. Максимум native ciphertext prover среди измеренных операций —13,89MB. Полное восстановление трёх сумм отдельным холодным процессом — **0,214 с / 7,73 MB**, внутренний decode —0,172с. Для обоих участников после cross-owner операции отдельные recovery processes заняли0,217–0,219с / около7,8MB.

Это один host sample на операцию, не статистическое утверждение о типичной latency. Неизменённые участки тоже меняются во времени от запуска к запуску; различия их цифр нельзя приписывать новому backend. Полные paired data — [comparison.json](results/host-2026-09-11/paired-comparison/comparison.json).

## 3. Нода: существенная регрессия проверки

| Операция | Baseline Groth16 verify, мс | Variant2 bundle verify, мс |
|---|---:|---:|
| Claim | 3,28 | **661,12** |
| Payment | 2,44 | **637,68** |
| Receive | 2,17 | **159,82** |
| Withdraw | 2,02 | **168,16** |
| Cross-owner sender | 2,37 | **286,07** |
| Cross-owner recipient | 1,90 | **532,88** |

Baseline измеряет crypto verify после подготовки VK. Variant2 проверяет Bulletproofs, Σ relations, все новые cross-curve bridges и retained economics proof, включая загрузку небольшого VK там, где он нужен. Чтение bundle/registry с диска исключено из обоих соответствующих внутренних измерений. Это не sustained network TPS и не результат batch optimization. [Точные node measurements](results/host-2026-09-11/paired-comparison/node-verification.json).

В приведённых строках verifier стал примерно **в70–280раз дороже**. Это цена текущего составного доказательства и его реализации, а не универсальное свойство Twisted ElGamal/Bulletproofs. Для масштабируемой сети такую регрессию нельзя игнорировать ради уменьшения RAM кошелька.

## 4. Байты и хранение

| Объект | Размер |
|---|---:|
| Ristretto uint256 ciphertext:16×(C,D) | **1 024 B raw** |
| Тот же ciphertext в bincode с key и lengths | 1 072 B |
| Сохранённые Baby commitments для того же uint256 | ещё128B |
| Один joint-bit bridge на uint256 | **87 336 B** |
| Baseline Groth16 proof | 128B |
| Новые money proofs без cross-curve bridge | несколькоKB; конкретные размеры в storage.json |
| Общий cache PK для32SU/claim/mint/pledge | **41 370 464 B на диске**, не одновременно в RAM |

Фактические новые bundles содержат statement, ciphertexts и все proofs:

| Операция | Baseline statement+proof, B¹ | Variant2 bundle, B | Новых bridges |
|---|---:|---:|---:|
| Первый claim | 1 819 | **456 816** | 5 |
| Payment | 1 511 | **277 419** | 3 |
| Receive | 1 511 | **102 747** | 1 |
| Withdraw | 941 | **96 735** | 1 |
| Cross-owner sender | 1 511 | **190 083** | 2 |
| Cross-owner recipient | 1 511 | **364 755** | 4 |

¹ Для сопоставления рассчитан эквивалентный bincode tuple `(Public, proof bytes)`. Это не объявляется deployed baseline wire format. Новый столбец — реально записанные `twisted.bin`. Подписи, committee receipts, VSS packets и source artifacts идут дополнительно. В первой claim проверяются и начальные funding bindings; это не измерение каждого последующего claim.

Основная причина размера — мост: в первой claim **436 680 B из456 816B** занимают пять bridges. Это около96%. Оптимизированное доказательство между группами, смена представления history/VSS или общего commitment backend здесь не реализованы и не измерены.

На finish registry сохраняет24cipher bindings и занимает93 860B JSON. Текущие holder directories — около251,5KB каждый. Wallet1 с source/recovery fixtures —34 168B, wallet2 —9 525B; дополнительные private operation directories считаются отдельно. Полный public run до paired comparison —16 162 987B, включая повторные registry snapshots, proofs, receipts, manifests и SQLite. Делить это на256 и называть production Tribute size нельзя. [Развёрнутый storage accounting](results/host-2026-09-11/storage.json).

**Роли:** wallet хранит свой DK, необходимые source/Baby/history witnesses и восстановленные суммы; нода — ciphertext/commitments/proofs/versions/registry; holders — прежние VSS shares для aggregates и history. Новый DK не даёт восстановить Baby blinders или Fidelity history. Подробные входы/выходы и границы recovery — [README](README.md#кто-считает-и-хранит).

## 5. Tribute, Lysis и прежние дорогие consumers

В этом варианте **агрегаты остались на VSS**. Поэтому Ristretto ciphertext не добавляется к каждому Tribute: он появляется в денежном слое после claim. Формат source Tribute/Nod сохранён.

| Участок нового полного прогона | Измерение |
|---|---:|
| Cold P_link32 | 4,361с;392,46MB |
| 256 P_link, warm proof phase | 116,894с;≈2,19proof/с |
| Тот же cold batch целиком | 120,662с;453,71MB peak — **PASS512MB** |
| Node P_L2 + P_link,256 | 384,03+560,42мс;≈271proof pairs/с, без admission |
| Serial VSS admission + first rotation | 22,35с |
| 256Nod, serialization/fsync/SQLite | 1,98мс;71 168B |
| Lysis kernel + worker | 7,10мс **после prior aggregates/Fidelity** |
| Один Nod descriptor | 278B |
| Tribute artifacts,256 | среднее19 231,32B; min19 151B; genesis first record больше |

P_link/source/VSS код не менялся: здесь выполнен повтор исходного профиля, а не оптимизация этих участков. Неограниченный SU не решён; предыдущий monolithic64-SU prover превышал512MB, новый balance backend этот source circuit не заменяет. Телефон/WASM не измерены.

MPC stages отдельно от денежных prover:

| Consumer | Wall, с | Application traffic |
|---|---:|---:|
| Late Fidelity,1сложная история | 11,81 | 15,79MB |
| Claim history gate | 9,81 | 7,70MB |
| Payment / receive history gates | 9,37 /14,67 | 11,64 /11,65MB |
| Withdraw history gate | 10,77 | 11,39MB |
| Cross-owner sender Out / recipient In | 11,41 /5,50 | 19,58 /8,78MB |
| Intex:2выплаты | **121,12** | **153,73MB** |
| Expiry:255прав | **47,20** | **94,66MB** |

Committee остаётся тремя локальными процессами, passive honest-majority. В timing входят process startup/connect; traffic — framed application bytes. Для Intex/expiry max actor RSS≈128,78/140,94MB. Два владельческих history gates при cross-owner исполняются последовательно в harness. Увеличение последующей истории основного owner после перевода влияет на дальнейшие pledge consumers; это не контрольный baseline без дополнительного события.

Intex/expiry практически сохранили прежний масштаб затрат. Другие wall timings заметно варьируются даже при сходном traffic, поэтому один sample не используется как доказательство изменения MPC алгоритма. Новый backend **не убирает** closed history/MPC и не решает перенос индивидуального state при больших N.

## 6. Вывод о жизнеспособности

**Подтверждено:** точный uint256 с локальными carry proofs совместим с encrypted balance и key-only amount recovery; источник/Nod можно связать с этим состоянием без TEE и без расширения P_link foreign-curve circuit. Основной профиль32SU проходит512MB на host. Два кошелька не передают друг другу DK и старые балансы.

**Текущая композиция не подходит как готовая замена baseline для большого throughput:** node verification и operation bytes резко выросли, а Intex/expiry/VSS storage/rotation остались. Например, миллион операций размера измеренного первого claim — около456,8GB только таких bundles; это арифметическая проекция данного формата, не выполненная нагрузка и не нижняя граница всех схем. Масштаб зависит от количества claims и активных балансов, а не только от числа Tribute.

Следующий предмет оптимизации — стоимость совместимости Baby/Ristretto и отдельные committee arithmetic/storage paths. Новый proof между группами либо переход history к другой commitment group требует нового soundness анализа и повторного end-to-end измерения. Просто удалить bridges означало бы потерять доказанную связь ciphertext с source/Fidelity.

## 7. Проверка и воспроизводимые evidence

Сохранены61публичный evidence-файл и [MANIFEST с SHA256](results/host-2026-09-11/MANIFEST.json), включая metrics, paired comparisons и небольшой [withdraw sample](results/host-2026-09-11/withdraw-sample/statement.public.json). Приватные witnesses/keys/shares в этот набор не копировались.

Все42файла runtime source snapshot совпадают с завершённым run;33исходных baseline files также сохранили прежние hashes. [Source snapshot](results/host-2026-09-11/source-snapshot.json), [verification record](results/host-2026-09-11/verification.json). Baseline результаты не перезаписывались.

Независимая ограниченная source проверка нашла три P1: zero withdraw, zero cross-owner Fidelity update и подмена повторно прочитанных Bundle без смены statement. Все исправлены, повторно прочитаны reviewer и покрыты regression tests. [Review ledger](CRYPTO_DESIGN_REVIEW.md#source-recheck). Это не внешний аудит полной криптосистемы; reviewer не выдаёт свои source findings за самостоятельно выполненные benchmarks.

Production gates сохраняются: malicious/adaptive committee protocol, VSS→MPC authentication, проверенные setup/generators, production authority/funding/consensus/OCOMP, произвольный SU и physical-mobile profile. Немедленный offline recipient credit и owner-key rotation не исполнялись. Cross-owner — экспериментальная экономическая политика; production Gratis transfer остаётся выключенным.
