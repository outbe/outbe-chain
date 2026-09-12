# P_link: L2 draft → скрытый nominal → P-384 commitment

**Статус: не проходит требование кошелька ≤512 MB RAM.** Полный запуск достиг 19.445 GB; прямую эмуляцию P-384 в BN254 нельзя считать пригодным вариантом для кошелька. Сохранено как проверенный функциональный baseline.

Исследовательский компонент от 2026-09-11. P_link доказывает знание канонического TributeDraft с заданным L2 hash, точный расчёт nominal и открытие **того же** P-384 commitment, который используется в [wide-vss](../wide-vss/README.md). TEE для этих вычислений не нужен. Production admission пока не подключён.

Здесь используется настоящий рандомизированный **Groth16 proof над BN254**, а не только `is_satisfied()` или native-проверка. P-384 выражен внутри R1CS через арифметику другого поля. [Исследование gadget](../../P_LINK_GADGET_RESEARCH.md), [целевой trace T00](../../TARGET_TRACE_NO_TEE.md#t00-связь-существующего-l2-proof-с-закрытым-nominal).

## Что именно доказано

```text
private: draft_id, base_u64, remainder_u6, nominal_u256, blinding_r384
public:  owner, draft_hash, binding_hash, L2_root,
         L1_sender, chain, day, currencies, exclusion_flag,
         three Oracle values, source markers, C_nominal

u6 = base * 10^6 + remainder; 0 <= remainder < 10^6
E  = max(reference_vwap, reference_scurve)
N  = u6 * 10^6 * reference_vwap
D  = issuance_vwap * E
N  = nominal * D + division_remainder
0 <= division_remainder < D; D > 0; 0 < nominal < 2^256
C_nominal = nominal * G + r * H; 0 <= r < q_P384

canonical_TributeDraft_hash == SAME draft_hash as P_L2
binding(L1_sender, draft_id, chain) == SAME binding_hash as P_L2
```

Точная формула nominal соответствует текущему enclave `compute.rs`. Произведения и остаток ограничены целочисленными limbs: равенство остатков по модулю BN254 не подменяет равенство uint256. Для источника сохранён текущий формат `u64 base + remainder < 10^6`; это не произвольный uint256 на входе. Issuance — приватный промежуточный результат; отдельный C_issuance в этом профиле не нужен.

Канонический hash воспроизводит pinned `outbe-protocol v0.14.0` / `984d57ed0d2f014a1a74d0b3b4b0769801957791`: id seed, owner, day, currency, base, remainder, SortedSet length и отсортированные SU IDs. Независимый integration test использует настоящий `Entity` derive из этой зависимости. Poseidon2 проверяется также по известным Noir vectors.

Профиль допускает **до четырёх source markers**; активные markers строго возрастают, неактивные slots равны нулю и не входят в draft hash. Число markers публично. Это ограничение тестового circuit, не утверждение о production лимите. Увеличение профиля меняет circuit/VK.

## Кто считает и хранит

| Участник | Работа | Данные |
|---|---|---|
| L2 / источник | Передаёт пользователю canonical draft и уже готовый P_L2 | Знает исходную сумму; канал доставки в кошелёк ещё требуется |
| Кошелёк | Считает nominal, выбирает r, создаёт C_nominal и P_link; создаёт VSS для nominal,r | Хранит draft до окончательного admission, nominal/r до расходования права Nod; секреты на сеть не публикует |
| Валидатор | Проверяет P_L2 и P_link, совпадение hash/owner/binding/root, chain state и Oracle, replay protection | Получает proofs, public inputs, commitment; исходная сумма, nominal и r не требуются |
| VSS получатель | Проверяет свою share-pair и D[0] == проверенный C_nominal | Хранит собственные секретные доли по [контракту хранения](../../STORAGE_AND_AGGREGATE_PROTOCOL.md) |
| Lysis | Позже использует проверенный агрегат и записывает C_nominal с коэффициентами в Nod | P_link не требует раскрытия индивидуального nominal для этого шага |

P_link не проверяет подпись root/Merkle membership вместо P_L2. Он связывает исходный draft с денежным commitment. Без отдельного успешного P_L2 любой самостоятельно придуманный draft тоже может иметь корректный P_link. Public Oracle values проверяются относительно chain state, а не принимаются со слов prover.

Public input vector содержит 40 элементов BN254 Fr: 39 полей/limbs и завершающий domain-separated context hash. Координаты C занимают 12 limbs по 64 bits; SEC1 C декодируется канонически verifier-side. Context связывает также root, reference currency и exclusion flag, но подлинность этих metadata устанавливает внешний admission. Сам P_link не доказывает eligibility исключения.

## Измерения и воспроизведение

Результаты полного запуска: [artifacts/results.json](artifacts/results.json). Проверяемые артефакты: `proof.bin`, `vk.bin`, `public_inputs.bin`; читаемая копия — `public_inputs.json`. Private witness в эти файлы не включается. Benchmark fixture, включая его blinding, намеренно опубликован в исходнике: это тестовые данные, не реальные секреты.

Измерено на Apple M4 Max, 14 CPU cores, 36 GiB RAM, macOS arm64, rustc 1.96.0; release, `RAYON_NUM_THREADS=8`, CPU не изолирован. Proving/setup — по одному образцу; verification — медиана пяти после прогрева. [Полный log](artifacts/benchmark.log), [память и CPU](artifacts/host_resources.json), [provenance](provenance.json).

| Измерение | Результат |
|---|---:|
| R1CS constraints | 5,600,598 |
| Создать P_link пользователю | 130.623 с |
| Проверить P_link с подготовленным VK | 0.962375 мс |
| Подготовить VK для verifier | 1.077875 мс |
| Proof, compressed | 128 B |
| C_nominal, compressed SEC1 | 49 B |
| Public field payload / сериализация с length | 1280 / 1288 B |
| Verifying key, общий для circuit | 1,544 B |
| Proving key, общий для circuit | 1,160,681,200 B (1.161 GB) |
| Одноразовый исследовательский setup | 137.142 с |
| Peak RSS всего запуска, включая setup и negative R1CS checks | 19.445 GB (18.110 GiB) |

Public input vector — вход verifier, не утверждённый формат хранения Tribute: chain-derived поля можно восстанавливать из проверенного состояния. Сам proof вместе с C_nominal занимает 177 B; это **не полный размер Tribute**, поскольку P_L2, VSS, metadata и transport сюда не входят. Proving key не дублируется на каждый Tribute. Предел до четырёх source markers относится ко всем этим цифрам.

Арифметическое обратное verification latency — около 1039 P_link checks/с последовательно на одном исполнителе. Это не измеренная пропускная способность admission: в ней ещё P_L2, VSS receipts, state access и consensus. Создание разных proofs независимо и может распределяться между пользователями/машинами; scaling и несколько prover-процессов одновременно здесь не измерены. В этом reference варианте создание proof дорого; из его времени нельзя выводить предел Pedersen или всех ZK backend.

Из корня репозитория с закешированными зависимостями:

```sh
cargo test --offline --locked --release --manifest-path testing/lysis-privacy-prototype/measurements/p-link/Cargo.toml -- --test-threads=1
RAYON_NUM_THREADS=8 cargo run --offline --locked --release --manifest-path testing/lysis-privacy-prototype/measurements/p-link/Cargo.toml --bin outbe-p-link-measurements -- bench /tmp/outbe-p-link
cargo run --offline --locked --release --manifest-path testing/lysis-privacy-prototype/measurements/p-link/Cargo.toml --bin outbe-p-link-measurements -- verify /tmp/outbe-p-link
cargo run --offline --locked --release --manifest-path testing/lysis-privacy-prototype/measurements/p-link/Cargo.toml --bin check_vss_link -- /tmp/outbe-p-link
cargo fmt --check --manifest-path testing/lysis-privacy-prototype/measurements/p-link/Cargo.toml
```

`bench` выполняет синтез и satisfaction check, одноразовый setup, создание одного proof, проверку (warmup + пять образцов), сериализацию/десериализацию, отрицательные проверки public inputs/proof и два неправильных witness. Новый запуск создаёт новые ключи и proof и перезаписывает указанный каталог. Proving key измеряется, но не сохраняется: это общие данные prover для circuit, не данные каждого Tribute. `verify` читает только три публичных бинарных артефакта и требует canonical encoding без trailing bytes.

Setup использует OsRng, однако выполняется одним процессом. **Эти ключи нельзя использовать в production:** нужен отдельный разрешённый VK и проверенная процедура setup/ceremony. Размер Groth16 proof не включает P_L2, метаданные Tribute, VSS coefficients, signatures или DA. Эти компоненты нельзя выдавать за end-to-end Tribute/s или общий размер Tribute.

`check_vss_link` сначала проверяет сохранённый настоящий P_link и полный ожидаемый public context, затем вызывает исходный wide-vss компонент: проверяет D[0] == C_nominal, все 16 shares, ротацию 16/6 и открытие исходной тестовой пары после неё. Корректный VSS для другого nominal отвергается сравнением C. Результат — `vss_link.json`; это локальная композиция компонентов на публичном fixture, без сетевого admission.

## Проверки и граница результата

В полном runner проверяются подмены draft hash, derived owner, binding, root, sender, chain, day, currencies, exclusion flag, Oracle, source marker, commitment и proof point. R1CS отвергает неверный nominal даже с соответствующим ему новым C; подмена исходного draft с пересчитанной экономикой также отвергается. Gadget tests сверяют результат с RustCrypto, включая infinity, inverse, doubling и полные разрядности.

Найден и исправлен underconstraint старшего limb расширенного произведения. Regression меняет одновременно limb и его bit decomposition: атака принималась старой версией и отвергается исправленной. Используются только ключи, созданные после исправления. Это adversarial проверка конкретного дефекта, не криптографический аудит всего circuit.

Не выполнены: production wiring P_L2+P_link, сетевые VSS receipts/accepted-set/handoff, доставка witness от L2 кошельку, private PayNote/Fidelity, proof точного claim 6→18 и миграция скрытого Gratis. Старые Ristretto/Bulletproofs результаты относятся к другой схеме и не являются результатами этого P_link.

## Заимствованный код

Poseidon2 constants скопированы из outbe-poseidon `a6066ce` с адаптацией только типа поля ark 0.5; [MIT notice](LICENSE-outbe-poseidon). Complete addition адаптирован из ark-r1cs-std 0.5.0 (RCB Algorithm 1); [MIT notice](LICENSE-ark-r1cs-std). Версии зависимостей закреплены в [Cargo.lock](Cargo.lock).
