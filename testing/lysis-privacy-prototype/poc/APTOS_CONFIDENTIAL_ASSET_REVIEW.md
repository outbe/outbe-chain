# Aptos Confidential Asset: применимость к no-TEE PoC

Проверено 2026-09-11. Это ограниченное исследование официальных AIP, документации и публичного исходного кода; код PoC не менялся, доказательства и benchmarks не запускались. Graph tools недоступны: project/generation/coverage не заявляются. Локальная сверка — direct source/`rg`, сетевые действия — только чтение публичных источников. XELIS в этот документ не входит.

**Вывод:** Twisted ElGamal + Bulletproofs + Sigma — полезный кандидат для шифрованного денежного состояния и входящих зачислений без участия получателя. Готовый Aptos CA не заменяет наш полный протокол: его диапазоны, восстановление чисел и ротация относятся к другому профилю. Для наших uint256, раскрытия только S/S_l, сменяемого комитета и существующего Baby P_link нужны отдельные доказанные переходы. Ни пригодность полного кошелька для 512 000 000 B, ни работа с 32 SU этими источниками не подтверждены.

## 1. Точные версии и статус

| Источник | Проверенный pin | Что установлено |
|---|---|---|
| [aptos-core][core-pin] | `4ef6adb579007267aac36b884098743726350d4f`, commit 2026-09-10 | Move CA, Sigma relations, native verifier и DK backup существуют в исходниках |
| [aptos-ts-sdk][sdk-pin] | `da6319287572e8f62f38c73be0e2346ca7447e21`, commit 2026-08-25 | пакет `@aptos-labs/confidential-asset` версии **2.3.0**, SDK encryption/proving/rotation/recovery |
| [confidential-asset-bindings][bindings-pin] | `51696df1674829eb6d0b9013e5bf0fa1128e5fe6`, commit 2026-06-29 | пакет **1.1.2**, Rust core + WASM/native bindings |
| [ristretto255-dlog][dlog-pin] | `94c068ad9b9c5e4f89a8bcf79c823867d63564d9`, commit 2026-03-12 | 16/32-bit DL algorithms; тот же commit в bindings Cargo.lock |
| [AIP-143][aip] | AIPs `35e3f5c1f374859b82096a647c952ca6e8062623` | актуальный путь `aips/aip-143-confidential-apt.md`, статус **Accepted**, не Draft |

Это больше, чем предложение на бумаге. Однако чтение `main`, Accepted AIP и [документации][docs] не проверяет текущие mainnet bytecode, flags, allowlist, auditor keys или emergency pause. Здесь нет заявления о проверенном deployment. Старый URL `aip-143.md` и ранние варианты статьи/PR нельзя использовать вместо этих pins. В текущем [Fiat–Shamir source, строки 80–123][fs] уже включены ответы Sigma в transcript для challenge пакетной проверки; автор отдельно указывает [исправление PR #19711][fs-fix].

## 2. Шифрование: точная convention

Пусть G — канонический генератор Ristretto255, H — отдельный фиксированный генератор, s ≠ 0 — **DK**. В Aptos:

\[
EK=s^{-1}H,\quad P=mG+rH,\quad R=rEK,\quad P-sR=mG.
\]

В Move компоненты называются `P,R`, в SDK `C,D`. Секретный ключ здесь не имеет convention `EK=sH`. Формулы подтверждены [SDK encrypt/decrypt, строки 65–142][elgamal] и [publicKey(), строки 287–297][key]. H фиксируется в том же SDK, строки 28–35; для совместимости надо брать точные generators и encoding. Ristretto имеет порядок

\[
q=2^{252}+27742317777372353535851937790883648493,
\]

а не координатный модуль `2^255−19`. Точка кодируется 32 байтами, один `(P,R)` — **64 B** без обрамления. [RFC 9496, §§4, 4.3, 4.4][ristretto].

Для одного получателя при фиксированном EK сложение пар шифрует сумму. Для нескольких получателей/аудиторов можно оставить общий Pedersen P и дать каждому собственный `R=rEK`; Sigma доказывает равенство скрытого plaintext. Знание DK снимает случайную маску, но возвращает **точку mG**. Преобразование её в число — отдельная bounded-DL операция. Это не обычное ElGamal шифрование произвольных байтов с прямым восстановлением сообщения.

## 3. Chunks, pending и переносы

[confidential_balance.move, строки 1–8, 35–42, 62–74][balance] задаёт основание `B=2^16`, `a=Σ a_i B^i`, отдельное шифрование каждого chunk; pending имеет **4 chunks**, available — **8**. Свежие transfer chunks и нормализованные balance chunks доказываются в `[0,2^16)`. Поэтому свежая сумма перевода имеет 64 бита, нормализованный available — 128. Их ciphertext payload без аудитора равен соответственно **256 B** и **512 B**. Дополнительный auditor добавляет R-компоненты.

Pending отделён от available: внешнее зачисление меняет pending, поэтому не инвалидирует уже создаваемое доказательство расхода available. Это полезно для нашего concurrent/offline receive. [Официальное описание двух балансов][docs].

Важно отличать разрядность **нормализованного представления** от сырых колонок после сложения. При максимум `2^16` входящих операциях колонка pending ≤ `2^16(2^16−1)`; после прибавления нормализованной available-колонки результат ≤ `2^32−1`. Поэтому DL ограничен 32 битами. Суммарное значение четырёх ненормализованных колонок может занимать до 80 бит по одной лишь этой алгебраической оценке: название «4×16» не означает четыре независимых 16-bit счётчика после накопления. Дополнительные экономические ограничения системы действуют отдельно.

Rollover линейно прибавляет pending к available и обнуляет pending; **он не вычисляет carries**. Следующий rollover требует normalization. При normalization владелец восстанавливает число, разбивает его заново и шифрует со свежими randomness; доказательство связывает старое и новое представление. SDK [ConfidentialNormalization.create, строки 74–102][normalization] показывает re-encryption. Auditor R после rollover временно остаются stale; их актуальность обозначена hint. [Lifecycle source][lifecycle].

## 4. Операции и граница public/private

| Операция | Текущий контракт |
|---|---|
| Register | signer + nonidentity EK + Sigma знания DK; создаёт **публично известный ноль**, не скрытый начальный баланс |
| Deposit/topup | положительный **public u64** перевод из FA в pool; ciphertext credit с r=0; amount остаётся публичным |
| Confidential transfer | signer отправителя; проверка proofs; новый available отправителя, encrypted credit получателю; получатель не подписывает |
| Withdraw | **public u64**, range+Sigma; выход из pool в public FA |
| Normalize | withdrawal с amount=0; разрешён для ненормализованного available |
| Rollover | owner signer, normalized=true, pending count>0; новый available, pending=0, normalized=false |
| Key rotation | owner signer; incoming paused, pending public-zero/count=0; Sigma, новый EK и R |

Источник таблицы: [confidential_asset.move, строки 418–559, 616–797][lifecycle]. Проверка cap `65536` применяется и к deposit, и к transfer; после его достижения новые credits отклоняются. Это ограничение **на один pending**, не на всю сеть.

Нулевой public deposit явно запрещён. Для confidential transfer range `[0,2^64)` включает ноль, а main guard не добавляет доказательство положительности: корректный нулевой перевод другому account также расходует один слот pending. Это часть admission/liveness модели, которую при переносе следует сохранить либо явно изменить.

Публичны адреса, asset, timing и тип операции; внутренний transfer скрывает сумму. Deposit/withdraw и общий размер pool не скрыты: [get_total_confidential_supply, строки 1080–1084][supply] возвращает публичный FA balance. Прямое использование deposit для нашего скрытого issuance/nominal нарушило бы требование уже на входе. Нужен собственный доказанный private credit из source, а не публичный topup. Настройки effective/voluntary auditors меняют круг имеющих доступ к plaintext; для требования скрыть individual от validators нельзя молча назначить committee таким аудитором с полным DK. [AIP, auditing policy][aip].

## 5. Что именно доказывается

**Range proofs** относятся к P-commitments отдельных новых chunks. CA использует две отдельные aggregated Bulletproofs для transfer: четыре amount chunks и восемь new-balance chunks. Это доказывает диапазон; корректность R и сохранение денег доказывает Sigma. [Range-proof adapter, строки 29–73][ranges]. Текущий native wrapper принимает bit widths `{8,16,32,64}` и batch sizes `{1,2,4,8,16}`, generators capacity 16; это конкретное API-ограничение, а не общий предел Bulletproofs. [Native source, строки 75–92][native].

**Transfer Sigma** имеет witness `s, new_a_i, new_r_i, v_j, r_j`; проверяет `H=sEK_sender`, корректность новых encryptions, одинаковые transfer plaintext/randomness для sender/recipient/auditors и агрегированное равенство:

\[
\sum B^i P_i^{old}=s\sum B^i R_i^{old}+(a^{new}+v)G.
\]

Это равенство группы, то есть по модулю q. Новые диапазоны и допустимый старый state превращают его в integer conservation в исходном Aptos-профиле. Старые openings не нужны — sender знает DK. [Точная relation и witness][transfer].

**Withdraw Sigma** аналогична, но v публичен; normalization использует v=0. [Точная withdraw relation][withdraw]. Session/transcript связывают contract, chain, protocol, account/asset context, statement и proof commitments; действующий код отдельно связывает batch challenge с responses. [Fiat–Shamir][fs]. Это важно сохранять при адаптации: нельзя склеить самостоятельно придуманный Schnorr transcript и назвать его текущим Aptos proof.

Эти relations не проверяют TributeDraft codec/hash, source-authorized Merkle root, FullProof P_L2, точный nominal из 32 SU, division/floor, Fidelity cohort transitions или вычисление Lysis. Общая возможность Bulletproofs для иных relations не означает, что эти relations уже реализованы данным CA API. [Исходный Bulletproofs проект][bp-paper] описывает range proofs без trusted setup; это не свидетельство наличия нашего source circuit в Aptos.

## 6. Восстановление кошелька и реальная ротация

DK + актуальные ciphertext chunks позволяют восстановить суммы без сохранённых r: SDK сначала пробует 16-bit lookup, затем 32-bit DL; вне этого диапазона decrypt завершается ошибкой. [decryptAmount(), строки 110–125][elgamal]. Default Rust solver — TBSGS-k32 с таблицей и 16-bit lookup на её основе. [discrete_log.rs, строки 28–78][dl-core]. Это снимает часть проблемы потерянных openings у получателя, но не восстанавливает потерянный DK и не создаёт witness для нелинейной истории Fidelity.

В текущем SDK **уже есть**, а не только предложен в статье, [DK backup][dk-backup]: HKDF-SHA512 от Ed25519 backup seed, XChaCha20-Poly1305, nonce24; ciphertext32-byte DK занимает72B. В [account.move, строки 593–624][account-backup] backup-key update и opaque DK ciphertext сохраняются атомарно. Но chain **не проверяет**, что ciphertext содержит DK, соответствующий зарегистрированному EK; это прямо описано в source. Наша обязательная восстановимость общего state требует дополнительной связи с commitment/EK и правил хранения/проверки, а не одного факта наличия зашифрованного blob. `fromPepperBase()` также реализован в [SDK, строки 218–252][key], но это отдельная keyless trust/recovery схема, не механизм validator committee.

Для wallet rotation `δ=s_old/s_new`, `EK_new=δ EK_old`, `R_new=δ R_old`; P сохраняется. Sigma доказывает знание old DK и корректность δ и δ^-1. [Key rotation relation, строки 1–28][rotation]; [SDK authorizeKeyRotation, строки 182–223][rotation-sdk]. Эта операция не требует знания старых r или DL суммы, но требует old/new DK и owner authorization. Сохранённая публичная история остаётся расшифровываемой старым DK; rotation не стирает архив ciphertexts и не даёт ретроактивную секретность.

## 7. Адаптация к uint256: конкретный запрет на наивное расширение

Наш PoC использует полный uint256 для денежных балансов; см. [README](README.md) и [master requirements](../PROTOCOL_TRACE_AND_REQUIREMENTS.md). У Aptos исходный initializer специально требует 128-bit available и ограничивает scalar range252; [строки 325–341][init].

**Контрпример для предложенного расширения, не баг текущего Aptos:** заменить eight chunks на sixteen, оставить единственное weighted conservation equality и доказывать каждый chunk16. Взять `old_balance=1`, `v=1`, `new_balance=q`. Число q помещается в uint256 и разлагается на допустимые 16-bit chunks. Но `1G=(q+1)G`, поэтому та же Sigma relation удовлетворяется. Злоумышленник знает DK, новые amounts/randomness и может быть честным prover ложной *integer* операции. Range proofs новых chunks не исключают эту подмену.

Нужно доказать целочисленные carries/borrows между limbs с локальными диапазонами, удерживающими каждое равенство ниже q, либо выбрать другую полностью определённую арифметическую конструкцию. Ограничить весь balance до `<q` — изменение принятой экономики, не бесплатная оптимизация. Наши четыре 64-bit limb commitments уже предназначены именно для широкого состояния; замена должна сохранять эту гарантию.

## 8. Миллиард входов и раскрытие только S/S_l

Для текущего source-профиля `a6<2^104`, `N≤10^9`, значит `S<2^134`. Это существенно ниже q, но после обычного threshold decryption одного aggregate ciphertext получается `SG`; generic bounded-DL на134-bit диапазоне требует порядка `2^67` групповых шагов. Поэтому single-cipher решение не становится практичным только из-за отсутствия scalar wrap.

**Chunked кандидат этим не исключён.** Семь 16-bit chunks представляют a104, с отдельным ограничением верхнего chunk до8бит. После N сложений каждый нижний raw chunk `<10^9·2^16<2^46`; generic DL масштаб имеет порядок `2^23` шагов на chunk. Это аналитическая оценка, не время/RAM benchmark. Текущий Aptos decoder поддерживает16/32, не46; padding batch7→8 и верхний range8 также требуют явно заданной relation/API adaptation.

**Публично расшифровать raw column sums нельзя**, если разрешён только итог S. Например в основании B: входы `[B−1,1]` и `[0,B]` имеют одинаковый S=B, но raw суммы колонок `(B,0)` и `(0,1)`. Это различимая дополнительная информация. Публичное выполнение carries после раскрытия уже не устраняет утечку. Нужна **приватная** normalization/recovery с доказательством результата, раскрывающая только S/S_l и разрешённый transcript; либо реконструкция нужного integer aggregate из уже связанных secret shares. Это дополнительный MPC/proof протокол.

Cap65536 и owner normalization ограничивают offline receive. Для одного daily accumulator миллиард входов потребовал бы порядка15259 окон такого размера, если переносить этот cap буквально; распределение по accounts/groups меняет схему, но не устраняет необходимость полного coverage и согласованного snapshot. Aptos не определяет наше правило «никаких intermediate totals до close». Нельзя автоматически применять wallet normalization, раскрывающую число владельцу, к committee-owned collective state.

## 9. Сменяемый комитет — отдельная криптографическая задача

Возможный **алгебраический** интерфейс: committee хранит Shamir shares s_i DK=s и выдаёт корректно доказанные `D_i=s_iR`; достаточный набор даёт `sR`. Но это ещё не protocol: нужны DKG/source of key, корректные public verification shares, VSS, membership/epoch binding, threshold authorization, робастность, coverage, availability и erasure/adaptive-corruption assumptions.

Даже convention важна: обычный DKG `s↦sH` сам по себе не выдаёт требуемый Aptos `EK=s^-1H`. Нужна согласованная distributed inversion/key-generation процедура либо другая доказанно согласованная convention. Это вывод из §2, а не реализованный здесь threshold backend.

Wallet rotation §§4/6 переносит один баланс на другой полный ключ; validator resharing сохраняет общий секрет/EK, меняя держателей долей. Ни одна из этих операций автоматически не даёт proactive/mobile security theorem. Если ciphertexts хранятся публично, последующая реконструкция полного старого DK раскрывает индивидуальные маленькие chunks из истории. Требования по erasure, старым epochs, repeated repairs и recovery остаются. Изменение committee/EK требует также не потерять owner-offline old ciphertexts и возможность их проверить.

## 10. Связь с P_link и полезный узкий эксперимент

Самая полезная переносимая идея — **входящее encrypted credit без recipient witness**, отдельный pending и восстановление суммы по DK+ciphertexts. Это может упростить storage/recovery шифрованного Gratis. Внешний mint/forced debit всё равно обязан доказать source authorization, сумму, integer conservation/underflow, account version и связь с private Fidelity history. Homomorphic subtraction не доказывает достаточность баланса.

Наш существующий P_link фиксирует source/body и Baby commitments, готовый Aptos — Ristretto P. Нужна проверяемая equality одного bounded integer между этими представлениями, либо иной согласованный commitment/prover design. Равенство labels/байтов или наличие двух валидных proofs этой связи не даёт. [L2 source boundary](L2_SOURCE_FEASIBILITY.md) также остаётся: CA не связывает FullProof с canonical TributeDraft и не заменяет authority root.

Поэтому следующий кандидат должен иметь отдельные gates: exact uint256 limb relation; source→ciphertext link; private aggregate opening only-after-close; threshold key lifecycle; offline recovery всех необходимых witnesses; атомарность shared balance/Fidelity. До их решения корректно оценивать CA как компонент, а не как полный no-TEE заменитель.

## 11. Libraries, лицензии и performance evidence

Bindings [Cargo.lock][bindings-lock] фиксирует:

| Dependency | Версия/pin | Обнаруженная license metadata |
|---|---|---|
| `bulletproofs` | 5.0.0, checksum `012e2e5f88332083bd4235d445ae78081c00b2558443821a9ca5adfe1070073d` | [crates.io: MIT](https://crates.io/crates/bulletproofs/5.0.0) |
| `curve25519-dalek` | 4.1.3, checksum `97fb8b7c4503de7d6ae7b42ab72a5a59857b4c937ec27a3d4539dba95b5ab2be` | [crates.io: BSD-3-Clause](https://crates.io/crates/curve25519-dalek/4.1.3) |
| `merlin` | 3.0.0, checksum `58c38e2799fc0978b65dfff8023ec7843e2330bb462f19198840b34b6582397d` | [crates.io: MIT](https://crates.io/crates/merlin/3.0.0) |
| `ristretto255-dlog` | 0.1.0, commit из §1 | в прочитанных Cargo.toml/README и полном repository tree отдельная license declaration/file не найдена; разрешение не выводится из публичности GitHub |
| bindings package | 1.1.2 | [package.json][bindings-package] объявляет `Apache-2.0`; это не проверка лицензий всех транзитивных компонентов |

Bindings генерирует BP5, тогда как pinned core workspace объявляет BP4; repository имеет `cross_version_compat.rs`, а README заявляет совместимость proof format/DST. Это **source/test-presence evidence**, не выполненный здесь compatibility test. [Bindings README][bindings-readme], [Rust core Cargo.toml][bindings-cargo]. Sigma остаётся в TS SDK/Move, bindings сам по себе не является всем CA prover.

**Лицензионный факт, без правового заключения:** текущие [aptos-core/LICENSE][core-license] и [SDK confidential-asset/LICENSE][sdk-license] называются **“Innovation-Enabling Source Code License”**. Их текст ограничивает использование вне **“personal, non-production and non-commercial environments”**, отдельно описывает переход конкретной версии к Apache2.0 через4года и Additional Use Grant. SDK package2.3.0 указывает `SEE LICENSE IN LICENSE`; core workspace — `LicenseRef-Aptos`. Поэтому нельзя объявить весь текущий Aptos CA переносимым Apache2.0 кодом только по старым SPDX comments. Возможность использовать конкретную прежнюю версию/компонент здесь не установлена.

| Primary evidence | Что можно утверждать | Чего она не доказывает |
|---|---|---|
| [Автор, Appendix benchmarks][author-bench] | single-thread M4 Max microbenchmarks; v1.1 no-auditor transfer: 3026 gas, 4.13 KiB TX payload | mobile proving time, wallet RSS, наши 32 SU, billion-ingest |
| [Bindings README, algorithm table][bindings-readme] | заявлены default TBSGS table≈512KiB и WASM≈774KiB | это размеры таблицы/артефакта, не full process peak RAM |
| [Bindings advanced.md, строки 50–65][bindings-advanced] | mobile16-bit<10ms,32-bit200–500ms обозначены как approximate estimates; cold table load отдельно | нет указанного device/полного reproducible cold proof RSS run |
| [Rust range_proof.rs, строки 11–19][bp-core] | cached generators64bits×16, локальный range prover | память всей JS/WASM+Sigma+P_link+P_L2 композиции |

В рассмотренных первичных материалах **не найден выполненный полный wallet benchmark с лимитом512 000 000 B**, включающий наш source P_link, P_L2,32SU, cold parameters и recovery. Это ограничение свидетельств, не логическая ошибка Aptos и не доказательство превышения RAM. Сам документ не содержит новых замеров.

## 12. Граница проверки

Выполнено: публичные GitHub metadata/tree/raw reads по pins; прочитаны точные Move relations/lifecycle, SDK encrypt/decrypt/normalization/rotation/backup, native/BP/DL interfaces, Cargo.lock/license metadata; проверены RFC/AIP/docs и авторские performance claims. Исходники для просмотра сохранены только во временном `/tmp/aptos-ca-review-b`; изменён только этот Markdown.

Не выполнено: запуск Aptos/Move/Rust/WASM, проверка опубликованного package binary против source, независимый security audit библиотек, mainnet state verification, proof compatibility execution, телефонные замеры, threshold implementation или адаптация PoC. Математические контрпримеры §§7–8 — выводы из указанных relations, не выполненные exploits против Aptos.

[core-pin]: https://github.com/aptos-labs/aptos-core/tree/4ef6adb579007267aac36b884098743726350d4f
[sdk-pin]: https://github.com/aptos-labs/aptos-ts-sdk/tree/da6319287572e8f62f38c73be0e2346ca7447e21/confidential-asset
[bindings-pin]: https://github.com/aptos-labs/confidential-asset-bindings/tree/51696df1674829eb6d0b9013e5bf0fa1128e5fe6
[dlog-pin]: https://github.com/aptos-labs/ristretto255-dlog/tree/94c068ad9b9c5e4f89a8bcf79c823867d63564d9
[aip]: https://github.com/aptos-foundation/AIPs/blob/35e3f5c1f374859b82096a647c952ca6e8062623/aips/aip-143-confidential-apt.md
[docs]: https://aptos.dev/build/smart-contracts/confidential-asset
[ristretto]: https://www.rfc-editor.org/rfc/rfc9496.html#section-4
[bp-paper]: https://crypto.stanford.edu/bulletproofs/
[elgamal]: https://github.com/aptos-labs/aptos-ts-sdk/blob/da6319287572e8f62f38c73be0e2346ca7447e21/confidential-asset/src/crypto/twistedElGamal.ts#L65
[key]: https://github.com/aptos-labs/aptos-ts-sdk/blob/da6319287572e8f62f38c73be0e2346ca7447e21/confidential-asset/src/crypto/twistedEd25519.ts#L287
[balance]: https://github.com/aptos-labs/aptos-core/blob/4ef6adb579007267aac36b884098743726350d4f/aptos-move/framework/aptos-framework/sources/confidential_asset/confidential_balance.move#L1
[lifecycle]: https://github.com/aptos-labs/aptos-core/blob/4ef6adb579007267aac36b884098743726350d4f/aptos-move/framework/aptos-framework/sources/confidential_asset/confidential_asset.move#L418
[supply]: https://github.com/aptos-labs/aptos-core/blob/4ef6adb579007267aac36b884098743726350d4f/aptos-move/framework/aptos-framework/sources/confidential_asset/confidential_asset.move#L1080
[init]: https://github.com/aptos-labs/aptos-core/blob/4ef6adb579007267aac36b884098743726350d4f/aptos-move/framework/aptos-framework/sources/confidential_asset/confidential_asset.move#L325
[normalization]: https://github.com/aptos-labs/aptos-ts-sdk/blob/da6319287572e8f62f38c73be0e2346ca7447e21/confidential-asset/src/crypto/confidentialNormalization.ts#L74
[ranges]: https://github.com/aptos-labs/aptos-core/blob/4ef6adb579007267aac36b884098743726350d4f/aptos-move/framework/aptos-framework/sources/confidential_asset/confidential_range_proofs.move#L29
[native]: https://github.com/aptos-labs/aptos-core/blob/4ef6adb579007267aac36b884098743726350d4f/aptos-move/framework/natives/src/cryptography/bulletproofs.rs#L75
[transfer]: https://github.com/aptos-labs/aptos-core/blob/4ef6adb579007267aac36b884098743726350d4f/aptos-move/framework/aptos-framework/sources/confidential_asset/sigma_protocols/proofs/sigma_protocol_transfer.move#L20
[withdraw]: https://github.com/aptos-labs/aptos-core/blob/4ef6adb579007267aac36b884098743726350d4f/aptos-move/framework/aptos-framework/sources/confidential_asset/sigma_protocols/proofs/sigma_protocol_withdraw.move#L18
[rotation]: https://github.com/aptos-labs/aptos-core/blob/4ef6adb579007267aac36b884098743726350d4f/aptos-move/framework/aptos-framework/sources/confidential_asset/sigma_protocols/proofs/sigma_protocol_key_rotation.move#L1
[rotation-sdk]: https://github.com/aptos-labs/aptos-ts-sdk/blob/da6319287572e8f62f38c73be0e2346ca7447e21/confidential-asset/src/crypto/confidentialKeyRotation.ts#L182
[fs]: https://github.com/aptos-labs/aptos-core/blob/4ef6adb579007267aac36b884098743726350d4f/aptos-move/framework/aptos-framework/sources/confidential_asset/sigma_protocols/sigma_protocol_fiat_shamir.move#L80
[fs-fix]: https://github.com/aptos-labs/aptos-core/pull/19711
[dk-backup]: https://github.com/aptos-labs/aptos-ts-sdk/blob/da6319287572e8f62f38c73be0e2346ca7447e21/confidential-asset/src/crypto/dkEncryption.ts#L12
[account-backup]: https://github.com/aptos-labs/aptos-core/blob/4ef6adb579007267aac36b884098743726350d4f/aptos-move/framework/aptos-framework/sources/account/account.move#L593
[dl-core]: https://github.com/aptos-labs/confidential-asset-bindings/blob/51696df1674829eb6d0b9013e5bf0fa1128e5fe6/rust/core/src/discrete_log.rs#L28
[bp-core]: https://github.com/aptos-labs/confidential-asset-bindings/blob/51696df1674829eb6d0b9013e5bf0fa1128e5fe6/rust/core/src/range_proof.rs#L11
[bindings-lock]: https://github.com/aptos-labs/confidential-asset-bindings/blob/51696df1674829eb6d0b9013e5bf0fa1128e5fe6/rust/Cargo.lock
[bindings-cargo]: https://github.com/aptos-labs/confidential-asset-bindings/blob/51696df1674829eb6d0b9013e5bf0fa1128e5fe6/rust/core/Cargo.toml
[bindings-package]: https://github.com/aptos-labs/confidential-asset-bindings/blob/51696df1674829eb6d0b9013e5bf0fa1128e5fe6/package.json#L74
[bindings-readme]: https://github.com/aptos-labs/confidential-asset-bindings/blob/51696df1674829eb6d0b9013e5bf0fa1128e5fe6/README.md
[bindings-advanced]: https://github.com/aptos-labs/confidential-asset-bindings/blob/51696df1674829eb6d0b9013e5bf0fa1128e5fe6/docs/advanced.md#L50
[core-license]: https://github.com/aptos-labs/aptos-core/blob/4ef6adb579007267aac36b884098743726350d4f/LICENSE
[sdk-license]: https://github.com/aptos-labs/aptos-ts-sdk/blob/da6319287572e8f62f38c73be0e2346ca7447e21/confidential-asset/LICENSE
[author-bench]: https://alinush.github.io/confidential-assets#appendix-benchmarks
