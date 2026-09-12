# Aptos/XELIS: применимость к no-TEE Tribute → Nod → Gratis

Проверено 2026-09-11 по первичным источникам и текущему host PoC. Это исследование следующего варианта реализации; **Twisted ElGamal/Bulletproofs здесь не запускались и не заменили backend PoC**. Отдельный разбор Aptos — [APTOS_CONFIDENTIAL_ASSET_REVIEW](APTOS_CONFIDENTIAL_ASSET_REVIEW.md).

Последующее исполнение: добавлен самостоятельный [variant2](variant2/README.md), сохраняющий baseline и VSS aggregates. Его [результаты и парное сравнение](variant2/RESULTS.md) относятся к новому запуску. Утверждения «не реализовано/не измерено» ниже фиксируют состояние на момент исследования; границы фактически исполненного варианта перечислены отдельно.

## 1. Итог существующего PoC

Экспериментальная цепочка исполнилась без TEE, включая source P_L2, P_link, две ротации shares, открытие S/S_l, Lysis/Nod, приватные денежные transitions и связанную историю Fidelity. Проверенный основной профиль — 256 разных TributeOffer по 32 SU. Cold P_link занимает около 4 с и 388–395 MB peak RSS; warm batch — 2,68 proof/с и 443 MB. Это соответствует бюджету wallet 512 MB на host, но не является измерением телефона. 64-SU процесс остановлен при превышении бюджета; неограниченный SU этим prover не решён.

Проверки P_L2 + P_link дают около 332 Tribute/с на измеренном batch, без admission/consensus. Lysis kernel и запись 256 Nod descriptors заняли 8,73 мс **после** получения нужных агрегатов/лиг. Один Nod descriptor — 278 B; обычный сериализованный Tribute artifact — 19 165 B, включая JSON-дубли и отчётный файл. Эти числа нельзя называть production TPS/wire format.

Главные измеренные проблемы: текущий MPyC Intex для двух выплат — 119,96 с; expiry 255 прав — 46,76 с; whole-map JSON storage и перенос per-record shares не подходят для миллиарда записей. Это ограничения данного backend и harness, не нижняя граница стоимости конфиденциальной схемы. Полная методика, источники чисел и security boundaries — [RESULTS](RESULTS.md), [COVERAGE_AND_STORAGE](COVERAGE_AND_STORAGE.md).

Фактический стек PoC: UltraHonk P_L2; Groth16/BN254 P_link с native Baby-Jubjub Pedersen; Groth16 state proofs с четырьмя 64-bit commitments для uint256; Pedersen VSS 2-of-3; passive honest-majority MPyC. Поэтому результаты Aptos/XELIS нельзя приписывать нашим замерам.

## 2. Что подтверждают Aptos и XELIS

| Свойство | Aptos Confidential Asset | XELIS |
|---|---|---|
| Приватное денежное состояние | Twisted ElGamal, separate pending/available | Twisted ElGamal, versioned final/output balances |
| Работа сети | Проверить proofs, обновить ciphertext; owner rollover/normalization | Проверить proofs, прибавить зачисления/вычесть списания |
| Разрядность рассмотренной реализации | 64-bit transfer: 4×16-bit chunks; 128-bit available: 8×16 | Рассмотренные amount/decode APIs используют u64 и ограниченный interval DLog |
| Доказательства | Bulletproof range proofs и специализированные Σ relations | Bulletproof range proofs, ciphertext validity, commitment equality |
| Получение числа владельцем | Секретный ключ → точки chunks → bounded DLog | Секретный ключ → точка → bounded DLog с таблицей |
| Конкурирующие входящие/исходящие | Pending отделён от available; ограничение накопления перед rollover | Versioned output/final balances, правила порядка транзакций |

Источники: [Aptos: конструкция и chunking](https://alinush.github.io/confidential-assets), [Aptos: проверенные code pins и lifecycle](APTOS_CONFIDENTIAL_ASSET_REVIEW.md); XELIS [balance state](https://github.com/xelis-project/xelis-blockchain/blob/db59b5c246ab0e40386d6c241dcd77f0aea84b10/xelis_common/src/account/balance.rs), [decrypt API](https://github.com/xelis-project/xelis-blockchain/blob/db59b5c246ab0e40386d6c241dcd77f0aea84b10/xelis_common/src/crypto/elgamal/key.rs), [proof components](https://github.com/xelis-project/xelis-blockchain/tree/db59b5c246ab0e40386d6c241dcd77f0aea84b10/xelis_common/src/crypto/proofs).

**Основной вывод:** сеть действительно может вести приватный account balance без раскрытия суммы и без MPC для каждого денежного сложения. Наличие этих реализаций подтверждает пригодность такого подхода для слоя Gratis. Они не реализуют наши source binding, late Fidelity, дневные S/S_l или динамический threshold committee.

## 3. Кто что создаёт и считает

В принятой в рассмотренных реализациях записи, для secret `s != 0`:

```text
EK = s^(-1) H
C  = mG + rH                    # Pedersen commitment
D  = r EK                       # decryption handle
Enc_EK(m; r) = (C, D)
C - sD = mG                     # ещё точка, не integer m
```

Именно handle превращает обычный commitment в ciphertext, расшифровываемый владельцем. Его секретный ключ позволяет удалить blinding из точки, но восстановление числа дополнительно требует ограниченного поиска либо chunks. Формулы подтверждены [XELIS key](https://github.com/xelis-project/xelis-blockchain/blob/db59b5c246ab0e40386d6c241dcd77f0aea84b10/xelis_common/src/crypto/elgamal/key.rs) и [Pedersen/handle](https://github.com/xelis-project/xelis-blockchain/blob/db59b5c246ab0e40386d6c241dcd77f0aea84b10/xelis_common/src/crypto/elgamal/pedersen.rs).

Предлагаемый денежный маршрут, **не выполненный новым backend**:

| Момент | Кошелёк | Нода | Публичное хранение / приватное хранение |
|---|---|---|---|
| Регистрация Gratis | Создаёт ключ и доказательство нулевого баланса/владения | Проверяет и регистрирует начальное состояние | EK, ciphertext, version / secret key и recovery material |
| Claim Nod | Зная nominal и opening, считает exact result, создаёт credit ciphertext и proof связи с Nod/terms | Проверяет право, формулу, отсутствие повторного claim; добавляет credit | Nod reference, ciphertext, proof, nullifier/version / witness и локальный balance cache |
| Входящее зачисление | Отправитель создаёт ciphertext суммы для ключа получателя и proof | Прибавляет корректное зачисление; получатель может быть offline | Pending ciphertext / получатель позднее восстанавливает chunks |
| Нормализация | Получатель получает сумму chunks, вычисляет carry, доказывает сохранение integer balance | Проверяет proof, переводит pending в нормализованное состояние | Новый ciphertext, versions и proof / ключ, числа и witness |
| Приватное списание | Создаёт amount ciphertext, range/equality proofs, привязанные к версии баланса и операции | Проверяет достаточность и корректность debit; вычитает ciphertext | Proof, новая версия и ciphertext / числа и witness |
| Gratis → COEN | Доказывает допустимое списание объявленной публичной суммы | Применяет debit и создаёт публичный COEN output атомарно | Сумма COEN публична; остаток Gratis остаётся ciphertext |

Суммирование ciphertext работает под **одним EK**. Для sender/receiver или owner/committee нужны отдельные handles и доказательство общего plaintext; ciphertext разных owner keys нельзя просто сложить для дневного открытия. XELIS validity proof связывает commitment и handles получателей с одним amount/opening; [исходник proof](https://github.com/xelis-project/xelis-blockchain/blob/db59b5c246ab0e40386d6c241dcd77f0aea84b10/xelis_common/src/crypto/proofs/ciphertext_validity.rs).

Кошелёк не обязан помнить сумму всех исторических randomizers: equality proof может использовать decryption secret и новое opening. При этом резервирование ключа, корректная привязка recovery ciphertext и доступность истории/актуального состояния остаются задачами wallet protocol. [XELIS equality proof](https://github.com/xelis-project/xelis-blockchain/blob/db59b5c246ab0e40386d6c241dcd77f0aea84b10/xelis_common/src/crypto/proofs/commitment_eq.rs) прямо требует от caller включить public key, ciphertext и commitment в transcript; наш адаптер также обязан связывать chain, asset, account, operation и state version.

**Fidelity остаётся отдельным потребителем.** В текущем trace нужны закрытые cohorts, exact timestamp, LIFO и offline league classification. Зашифрованный общий Gratis balance не содержит всей этой информации. Поэтому можно заменить денежный proof/backend и сохранить отдельный history gate; из этого не следует исчезновение всего MPC.

## 4. uint256: что требуется сверх готовых библиотек

Scalar order Ristretto около 2^252, меньше 2^256. Одно кодирование `mG` не связывает произвольный uint256 как integer: разные целые, отличающиеся на порядок группы, дают одну точку. Простого range proof «m — uint256» недостаточно для исправления такого представления.

Chunking позволяет хранить широкое число. Но добавление ещё восьми chunks к Aptos не завершает перенос на uint256: взвешенная conservation relation только modulo scalar field тоже допускает aliases. Нужны доказанные локальные carries/borrows, диапазоны limbs и проверка отсутствия итогового overflow. Это относится и к балансу, и к сумме переводов, и к intermediate products; дополнительные биты произведения нельзя отбросить до проверки.

В текущем canonical R01 номинал в fixed6 ограничен 104 битами, а сумма до миллиарда таких входов — 134 битами. Это следствие source codec и формулы; **не ограничение произвольного пожизненного uint256 Gratis**. При новом source profile эти границы надо вывести заново. См. [trace R01](../PROTOCOL_TRACE_AND_REQUIREMENTS.md).

Умножение ciphertext на публичный коэффициент доступно алгебраически. Для наших целочисленных формул отдельно нужны bounds и нормализация разрядов. Операция `scalar.invert()` даёт деление в поле, а не `floor(numerator/denominator)`. В XELIS benchmark с названием division используется именно inversion; [исходник](https://github.com/xelis-project/xelis-blockchain/blob/db59b5c246ab0e40386d6c241dcd77f0aea84b10/xelis_common/benches/homomorphic_encryption.rs). Поэтому exact private Intex division не становится бесплатной операцией ElGamal.

## 5. Дневной агрегат: chunked ElGamal — отдельный кандидат

Ранее отвергнутое восстановление **одного** 134-bit `S·G` через generic DLog остаётся непрактичным. Этот аргумент нельзя переносить на все chunked схемы.

Собственный расчёт для canonical nominal `a < 2^104` и `N <= 10^9`:

```text
a_i = sum_j a_ij * 2^(16j),  0 <= a_ij < 2^16
7 chunks на вход, верхний chunk ограничен 8 битами
T_j = sum_i a_ij < 10^9 * (2^16 - 1) < 2^46
S = sum_j T_j * 2^(16j)
```

После гомоморфного сложения требуется восстановить несколько значений с максимум 46 битами вместо одного 134-bit числа. Это другой, потенциально пригодный time/memory tradeoff. Он **не измерен** здесь. XELIS документирует bounded DLog и крупную предварительную таблицу, однако его опубликованные latency/memory не доказывают наши показатели committee или мобильного uint256 prover. [XELIS recovery](https://docs.xelis.io/features/wallet/ecdlp).

### Почему нельзя сразу публично открыть все T_j

Разрешение пользователя — раскрывать **S и S_l** после закрытия, а не дополнительные суммы по разрядам. Простой пример в основании 10:

```text
19 + 1  = 20; сумма единиц = 10, сумма десятков = 1
10 + 10 = 20; сумма единиц =  0, сумма десятков = 2
```

S одинаков, но опубликованные ненормализованные разряды различаются. Это дополнительная статистика о наборе входов, не утверждение о восстановлении конкретного Tribute. Публичный carry после открытия эту утечку уже не устраняет.

Для соблюдения текущего disclosure contract нужен протокол, который сохраняет T_j закрытыми, выполняет normalization и выдаёт только S/S_l. Threshold decryption в публичные точки `T_j G` не удовлетворяет этому условию, если диапазон позволяет всем восстановить T_j. Фраза «сделаем carry в MPC» тоже не завершает схему: надо определить, как integer T_j приватно попадёт в MPC без предварительного публичного DLog. Отдельные authenticated shares, private recovery либо иное доказанное кодирование — самостоятельные конструкции с собственной стоимостью. Их готовой реализации для этого маршрута в Aptos/XELIS не найдено в рассмотренном scope.

**Решение по текущему PoC:** сохранить VSS как работающий baseline числового S/S_l без DLog; chunked threshold ElGamal оставить отдельным кандидатом на исследование. Не объявлять его ни готовой заменой, ни принципиально невозможным. Измеренное открытие S/S_l после snapshot на 256 входах — 0,309 с; проблемы хранения и ротации VSS при больших N этим не снимаются.

## 6. Динамический комитет и хранение

Общий committee EK с корректным refresh/handoff долей **того же** секрета потенциально позволяет не переносить все ciphertext при каждом изменении validators. Переносятся key shares, а публичные ciphertext остаются. Новый независимый DKG key каждый час такого свойства не даёт. Late leagues и expiry/Intex по-прежнему требуют доступных исходных rights/ciphertexts, а не только одного накопленного ciphertext дня.

Это архитектурная возможность, не функция готового Aptos/XELIS wallet SDK. В convention `EK=s^(-1)H` decryption shares линейны по s, но обычный DKG для `sH` сам по себе не создаёт связанный `s^(-1)H`: нужен корректный distributed key-generation/inversion protocol либо согласованная другая convention. Malicious share proofs, refresh/erasure, lineage, authorized aggregate manifests и VSS/P_link input binding остаются отдельными обязательствами. Гарантия закрытости, как и у текущего VSS, зависит от ограничения сговора держателей; chain deadline сам не мешает достаточному числу colluding holders расшифровать ciphertext раньше.

### Размеры: ciphertext не равен ciphertext SEAL

XELIS сериализует commitment и handle как две точки по 32 B: **64 B за один ciphertext с одним handle**. Это реальное свойство формата, но не размер полного Tribute. [Compressed format](https://github.com/xelis-project/xelis-blockchain/blob/db59b5c246ab0e40386d6c241dcd77f0aea84b10/xelis_common/src/crypto/elgamal/compressed.rs).

Собственные raw projections, без metadata, proofs, source Offer, indices и replicas:

| Предлагаемое представление | Байты |
|---|---:|
| 104-bit nominal, 7×16-bit ciphertext chunks, один EK | 448 B/Tribute; 448 GB на миллиард |
| То же, общий C и два handles: owner + committee | 672 B/Tribute; 672 GB на миллиард |
| Произвольный uint256, 16×16-bit chunks, один EK | 1 024 B на сумму/баланс |
| Два таких uint256 account balances: pending + available | 2 048 B/account |

Это простые варианты упаковки, не минимальные размеры и не реализованный wire format. Для полного uint256 nominal вместо source-bounded 104-bit также понадобилось бы 16 chunks; proof и обработка aggregate overflow меняются. На фоне нынешнего P_L2 8 900 B ciphertext может не быть крупнейшей частью Tribute, но сотни GB/миллиард требуют явного storage/DA решения.

## 7. Что можно взять из кода

**XELIS:** на проверенном commit `db59b5c246ab0e40386d6c241dcd77f0aea84b10` доступны Rust Twisted ElGamal, compressed types, validity/equality proofs и bounded-DLog integration. `xelis_common/Cargo.toml` использует XELIS forks: `bulletproofs v5.3.0`, `curve25519-dalek v5.0.2` с `ecdlp`, `merlin v4.1.0`. Это конкретные версии forks, не обещание совместимости с одноимёнными crates.io API. `xelis_common` связан с остальным workspace; минимальный extraction/adapter и его аудит ещё нужны. [Dependencies](https://github.com/xelis-project/xelis-blockchain/blob/db59b5c246ab0e40386d6c241dcd77f0aea84b10/xelis_common/Cargo.toml), [repository BSD-3-Clause license](https://github.com/xelis-project/xelis-blockchain/blob/db59b5c246ab0e40386d6c241dcd77f0aea84b10/LICENSE). Лицензии отдельных dependencies проверяются отдельно при выборе состава.

**Aptos:** полезны account lifecycle, chunk bounds, normalization, proofs, SDK и recovery/rotation interfaces. Точные packages/pins и условия LICENSE зафиксированы в [отдельной записке](APTOS_CONFIDENTIAL_ASSET_REVIEW.md). Текущий рассмотренный core/SDK нельзя автоматически обозначать Apache-2.0: в этих LICENSE стоит Innovation-Enabling Source Code License. Это фиксация текста источника, не правовое заключение.

**Совместимость с P_link:** наш commitment — Baby-Jubjub, готовые рассмотренные денежные компоненты — Ristretto255. Их точки нельзя подменить друг другом. Нужен доказанный bridge одного bounded integer между группами либо изменение P_link/backend. Цена bridge, foreign-curve arithmetic, proving key и cold wallet RSS входит в оценку следующего PoC; прежний PASS 512 MB не переносится автоматически.

Bulletproof range proof и Σ equality доказывают диапазоны и конкретные алгебраические relations. Они не заменяют canonical source hashing, связь с четырьмя P_L2 inputs, oracle-price формулу и checked division в P_link. Текущий source-linked proof остаётся отдельным слоем.

## 8. Выбор следующего варианта

| Участок trace | Вывод исследования |
|---|---|
| TributeOffer → P_link → C(nominal) | Сохранить проверенный source-linked путь; новую связь с Ristretto сначала спроектировать и измерить |
| Nominal → S/S_l после close | VSS baseline работает; отдельно исследовать chunked threshold aggregation с выводом только S/S_l и непрерывностью ключа |
| Lysis → Nod с commitment и публичными terms | Совместимо с deferred owner proof; все integer bounds сохраняются |
| Nod claim → приватный Gratis | Twisted ElGamal + специализированные proofs — обоснованный кандидат |
| Gratis credit/debit/receive/recovery | Самое прямое применение Aptos/XELIS; uint256 и concurrency требуют явной адаптации |
| Gratis → COEN | Проверенный debit с публичной суммой вывода и закрытым остатком |
| Offline Fidelity, Intex, поздние residuals | Эти проекты не закрывают наши нелинейные/history consumers; нужны отдельные протоколы и новые замеры |

Практически следующий ограниченный эксперимент должен проверять **денежный Gratis backend**: exact uint256 limbs/carries, два разных владельца, offline credit, нормализацию, debit/withdraw, восстановление с ключом и binding с Nod/P_link. Измерять cold RSS вместе с recovery table и bridge, proof bytes, verify time и bytes/account. Отдельно сравнивать стоимость чистого денежного transition и оставшегося Fidelity gate. До такого запуска нельзя приписывать этому варианту ускорение или прохождение 512 MB.

Итог проверки: Aptos/XELIS дают серьёзную основу приватного balance layer и требуют уточнить прежнее слишком широкое отбрасывание ElGamal. Они не дают готовой замены всей цепочки или доказательства масштабируемости нашего протокола. Новые benchmarks в рамках этой записки не выполнялись.
