# Полный Ristretto backend

Исполняемый PoC цепочки без TEE: P_L2 → P_link → nominal commitment → VSS → S/S_l → Lysis/Nod → приватный Gratis → публичный COEN. Здесь **все commitments приватных сумм, VSS и шифрованный Gratis используют Ristretto255**. Денежных Groth16 proofs и Baby-Jubjub/Ristretto bridges нет.

Это исправленный вариант сравнения. [Исходный baseline](../../README.md) и [ранний гибрид](../README.md) сохранены отдельно. Гибрид оставлял Baby-Jubjub в исходных суммах и истории; его проверку нельзя считать измерением полного Ristretto backend.

Groth16/BN254 остаётся способом доказать **P_link**, включая арифметику Ristretto в чужом поле. Входной P_L2 остаётся настоящим UltraHonk FullProof с прежними canonical hashes. «Полный Ristretto» относится к приватным commitments и состоянию; это не замена внешнего L2 proof system.

## Кто считает и хранит

| Участник | Вычисления | Приватное хранение | Публичные данные |
|---|---|---|---|
| Источник L2 | Настоящий P_L2 и canonical source fields | Исходный witness L2 | P_L2, owner/hash/root/source IDs |
| Кошелёк Tribute | Nominal по исходной формуле; Ristretto C; полный P_link; VSS deal | Source witness, nominal, blinder, соль P_link, identity key | C, 13 proofs P_link, цепочка salted digests, VSS polynomials и AEAD packets |
| Валидатор при admission | P_L2/P_link, signatures, VSS consistency, source uniqueness | Собственная проверенная VSS доля | Accepted Tribute и receipts |
| Держатели VSS | Проверяемая передача долей при двух сменах состава; сложение долей S/S_l | Текущие доли nominal и cohort limbs | После закрытия — только разрешённые S/S_l и aggregate openings |
| Lysis worker | Прежние checked integer kernels с открытыми S/S_l; коэффициенты, резерв, Nod descriptors | Скрытые суммы не требуются для публичного kernel | Nominal commitment, итоговые коэффициенты и права в Nod |
| Владелец Nod/Gratis | Claim, debit, credit, transfer, withdrawal proofs; новые ciphertexts и Ristretto note commitments | Ключ расшифрования; доступные владельцу суммы и необходимые witnesses | Ciphertexts, note commitments, Bulletproofs и Σ proofs; COEN amount при выводе |
| Получатель | Восстановление суммы по собственному ключу и dual handle; принятие перевода | Собственный ключ и recovered value | Proof принятия, новые ciphertexts/commitments |
| MPC parties | Те же Fidelity, LIFO, Intex, expiry с точным integer/floor | Проверенные VSS доли истории в поле q_R | Разрешённые результаты и commitments, signatures/receipts |

Все actors запускаются отдельными локальными процессами. Controller читает публичные файлы и передаёт пути к private files. Отдельный **test-only oracle** читает fixtures нескольких кошельков и возвращает только результаты сравнений; он не является частью протокола.

## P_link

Прямой большой foreign-field circuit не прошёл RAM probe. Используется 13 обязательных Groth16 proofs: одна исходная формула и 12 шагов Ristretto opening. Кошелёк доказывает source104 и канонический blinder `< q_R`; промежуточные Edwards points скрыты, наружу выходят только Poseidon digests с общей приватной случайной солью. Каждый шаг имеет свой фиксированный индекс и test VK; соседние digests обязаны совпасть. Последний шаг проверяет RFC 9496 quotient equality с канонической Ristretto-декомпрессией публичного C.

Public proof packet — **1 664 B**, 13 × 128 B. Дополнительно нужны public statement, opening binding и 13 digests. Параметры proving хранятся на диске и загружаются по частям. Соль и промежуточные points не публикуются. Конфиденциальность salted digests требует соответствующего hash-hiding допущения; это не perfectly hiding Pedersen commitments.

Verifier читает полный statement/proof packet один раз, проверяет все 13 частей над этим snapshot и выдаёт receipt точных данных. Admission использует именно проверенный Offer. Неполный набор частей не является P_link.

Source bound104 следует из текущего codec: `u = base_u64 × 10^6 + fraction6`, `nominal = floor(u × 10^6 × ref / (issuance × max(ref, scurve)))`; при положительном integer issuance результат `< 2^104`. Для миллиарда таких входов сумма `< 2^134 < q_R`. Это позволяет VSS aggregate открыться без scalar wrap. Произвольный внешний uint256 nominal вне этого source codec потребовал бы limb-wise VSS и отдельного source proof profile.

## Приватный Gratis и точные uint256

Одна сумма хранится как 16 шифрованных 16-bit chunks. `EK = s⁻¹H`, `C_i = m_iG + r_iH`, `D_i = r_iEK`. Владелец получает `C_i − sD_i = m_iG` и восстанавливает каждый chunk точным lookup на 65 536 значений. Исторические encryption randomizers не нужны.

Raw ciphertext — **1 024 B**, текущий bincode с key и lengths — **1 072 B**. Note — четыре 64-bit Ristretto Pedersen commitments, **128 B**. Четыре небольших Schnorr proofs связывают эти commitments с нормализованными chunks в той же группе. Реестр хранит ранее доказанные note/cipher связи, поэтому повторная полная связь не требуется.

Bulletproofs доказывают ranges chunks и переносов, Σ protocols — корректность ciphertexts и равенство скрытых линейных выражений. `money.rs` проверяет debit/credit/withdraw с переносами. `wide.rs` проверяет публичные коэффициенты в 48 base-65536 columns: полный uint256 × 512-bit product, без отбрасывания старших разрядов. Signed32 carries имеют нулевые endpoints; каждый локальный остаток меньше порядка группы. Claim связывает исходный nominal commitment с дополнительно зашифрованным nominal и source104 range. Отрицательный остаток, overflow и scalar-alias не допускаются.

Claim проверяет `newGratis = oldGratis + nominal × fraction × 10^6`, а escrow — `nominal × fraction × price`. PROMIS conversion использует точное `×10^12`. Публичный COEN вывод проверяет сохранение суммы и положительность. Нулевой cross-owner перевод также запрещён.

Перевод другому владельцу использует dual handles одной суммы и отдельное принятие получателем. **Автономное немедленное нормализованное зачисление офлайн-получателю не реализовано**. Форма production pending/available registry, авторизация обновления ключа и история восстановления требуют отдельных adapters.

## Запуск и границы результата

Из корня репозитория:

```sh
cargo build --offline --locked --release --manifest-path testing/lysis-privacy-prototype/poc/variant2/ristretto/Cargo.toml
cargo test --offline --locked --release --manifest-path testing/lysis-privacy-prototype/poc/variant2/ristretto/Cargo.toml -- --test-threads=1
python3 testing/lysis-privacy-prototype/poc/variant2/ristretto/run_variant.py --count 256 --su 32 --out testing/lysis-privacy-prototype/poc/variant2/ristretto/runs/my-fresh-run
```

Нужны собранные baseline `poc-math`, L2 source helper, MPyC environment и CRS, описанные в baseline README. Setup — single-party **тестовая** ceremony; prover и node доверяют configured VK paths. Compile/setup не включаются в wallet proving time. Cold RSS ограничен 512 000 000 B; показатели относятся к host, не к телефону с 2/4 GB и не к WASM. 32 SU — профиль измерения, не максимум протокола.

Fidelity/Intex/expiry остаются passive honest-majority MPyC экспериментом с локальным транспортом. Активно защищённый MPC, production DKG/VSS transport, безопасное стирание при ротации, consensus/DA, authenticated source/funding adapters, key rotation и production setup этим запуском не подтверждаются. Публичные owner IDs, source IDs, коэффициенты, operation graph, ciphertext/proof sizes и разрешённые S/S_l остаются видны. Это конфиденциальность сумм, не анонимность всей активности.

Математика и source-review: [RISTRETTO_SOURCE_RESEARCH.md](../RISTRETTO_SOURCE_RESEARCH.md). Измеренные результаты и состояние полного запуска фиксируются отдельно в RESULTS.md; описанная конструкция сама по себе не заменяет выполненные проверки.
