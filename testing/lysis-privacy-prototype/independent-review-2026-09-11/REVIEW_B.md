# Независимая проверка B: теория и полнота приватного денежного маршрута

Дата: 2026-09-11. HEAD: `177a72ddbea9f2e52eef094405481292ecd56046`. Входы — `INPUT_MANIFEST.json`, три основных документа, `TRACE_EVIDENCE.json`, library pins и arithmetic script. Все 24 source hashes и 8 input hashes перепроверены и совпали. Другие независимые отчёты и прежние completion/review conclusions до сдачи этого отчёта не читались. Production не менялся; подагенты не запускались.

## 1. Два отдельных вердикта

**(a) Теоретическая корректность: направление состоятельно, полная композиция не доказана.** Не найдено противоречия в двух вариантах P_link, точном scalar VSS-агрегировании текущего source profile, алгебре weighted redistribution, exact 6→18 формулах, deferred Nod и условной оптимизации Fidelity через 12 сравнений. Это подтверждение отдельных математических отношений при указанных предпосылках. Из него не следует malicious/mobile security законченного хранения, публично проверяемый приватный Fidelity prover или прохождение 512 MB.

**(b) Полнота: ответ «всё необходимое учтено, замечаний нет» неверен.** Найдены два конкретных потребителя/писателя того же состояния, не закрытых предложенными интерфейсами: **B-01 — публичная contributor-выплата Intex; B-02 — внешние изменения Gratis/Fidelity из Credis**. Оба замечания относятся к замыканию заявленного маршрута, а не к поиску дефектов во всём Intex/Credis. Документ честно исключает дальнейший аудит этих модулей, но исключение из исследования не отключает их достижимые операции.

Кроме этих находок остаются уже честно обозначенные открытые решения: параметры противника и committee, конкретный private repair/mobile handoff, Fidelity representation/prover, cost/denomination, granularity F, wire/DA/verifier и реальные измерения. Отсутствие benchmark здесь не классифицируется как логическая ошибка.

## 2. Проверка фактического маршрута

Graph MCP отсутствует среди доступных инструментов. Использован Tier Verify fallback: точное чтение и адресный `rg`, без заявления graph coverage или исчерпывающего аудита репозитория.

| Участок | Проверенная связь в текущем коде | Следствие для целевого протокола |
|---|---|---|
| R00–R01 | `l2registry/src/api.rs:40–62` проверяет BLS root зарегистрированного enabled источника; `tributefactory/src/runtime.rs:101–128,204–245,282–307` связывает public inputs с enclave result и вызывает FullProof verifier. `zk_claim.rs:27–79` включает private draft id, amount, canonical SU set и sender/chain binding. | Самостоятельные P_L2/P_link совместимы только при одинаковых аутентифицированных общих значениях. Unregistered/disabled обход нельзя перенести в private admission без эквивалентной source authority. Доставка/приватность реального L2 payload не проверена. |
| R01–R02 | `compute.rs:118–177`: u64 base, remainder<10⁶, checked U512 nominal; `tribute/src/runtime.rs:319–353,393–407` и `state.rs:317–360`: addition/burn и публичные суммы. | Source range позволяет меньший scalar; accepted-set count, source proof и rollback обязательны. Удалять только поле тела недостаточно: события и pre-admission/conservation проекции тоже меняются. |
| R04–R08 | `metadosis/settlement.rs:119–180`, `ocomp_budget.rs:41–85,95–159`: total→budget; snapshot вызывается при READY. `ocomp/snapshot.rs:21–55` и `fidelity/runtime.rs:100–123` реально читают позднее состояние владельцев. `lysis/program_v1/execute.rs:392–459` и `algorithm.rs:33–85,229–307` выполняют публичный integer kernel. | Разрешённых S/S_l достаточно для Lysis kernel. Один S недостаточен для поздних групп; source shares нельзя удалить на R04. Переход на точные units18 меняет денежную нормализацию. |
| R09–R10 | `phases.rs:426–477,684–707` создаёт Nod и contributor; `finalizer.rs:221–310` проверяет roots/conservation и B−G; `activation.rs:323–373` устанавливает оба выходных потока. | Commitment descriptor достаточен для deferred claim, но не закрывает contributor consumer B-01. Zero load/cost и cost overflow остаются правилами до выпуска права, а не неожиданным отказом при будущем claim. |
| R11–R13 | `nod/runtime.rs:29–74` закрепляет qualification/call terms. `nodfactory/runtime.rs:170–238,255–313` проверяет owner/deadline, читает bucket entry price, публично расходует PayNote и вызывает Gratis mint. `gratisfactory/runtime.rs:129–173` обновляет Gratis/Fidelity и конвертирует units. | Нужны скрытый платёж и exact denomination18, атомарный account/payment/Fidelity/residual transition. Повторно умножать Gratis18 на 10¹² при COEN нельзя. |
| R14 | `nod/called.rs:241–297` читает load каждого непогашенного Nod и возвращает сумму в PromisLimit. | Residual shares должны пережить claims, ротации и deadline. Public per-pass refund не автоматически разрешён; его гранулярность меняет утечку. |
| Дополнительные необходимые границы | `intexfactory/runtime.rs:460–516`; `credisfactory/runtime.rs:242–249,266–294`; `gratis/runtime.rs:423–457,464–513`. | Эти consumers/writers непосредственно затрагивают скрываемые nominal и те же Gratis/Fidelity accounts; см. находки ниже. |

## 3. Находки

### B-01 — High: contributor output нельзя завершить заменой nominal на commitment

**Документ:** [IMPLEMENTATION §6.1, строки 255–264](../DEEP_RESEARCH_IMPLEMENTATION.md) предлагает заменить contributor nominal ссылкой на C(a), сохранив conservation, и оставляет дальнейшие Intex/Credis consumers за границей исследования. [TRACE R09–R10, строки 278–294](../PROTOCOL_TRACE_AND_REQUIREMENTS.md#r09-worker-создаёт-nod-descriptors) фиксирует этот поток, но не его дальнейший расчёт.

**Первичные code references:** [Lysis contributor creation](../../../crates/core/lysis/src/program_v1/phases.rs#L701), [установка certified contributor root](../../../crates/core/metadosis/src/ocomp/activation.rs#L333), [certified payout](../../../crates/core/intexfactory/src/runtime.rs#L460), [декодирование открытого nominal](../../../crates/core/intexfactory/src/runtime.rs#L610).

`pay_contributor_batch` принимает листья, декодирует `leaf.nominal`, проверяет их против certified root и считает `floor(round.amount * leaf.nominal / generation.eligible_nominal_total)` (`runtime.rs:481–499`). Затем делает `transfer_balance` каждому публичному owner (`515–516`). Это действующий certified consumer, не только legacy путь. При замене leaf на C(a) прежний consumer перестаёт вычисляться. Если раскрыть a в payout calldata, C02 нарушается напрямую. Если добавить proof, но оставить публичную индивидуальную выплату, появляется дополнительная функция от a, не входящая в разрешённые S/S_l или Gratis→COEN.

**Контрпример:** два eligible nominal 7 и 13, известный дневной S=20, публичный pot=20 raw native units. Пропорциональные переводы равны 7 и 13, т.е. публично раскрывают исходные a. Выбор pot=H — допустимый пример арифметики текущего consumer, не утверждение, что такая сумма уже встретилась в сети. Даже вне этого частного случая выплаты раскрывают пропорции/интервалы. Проверка примера исполнена малым Python расчётом; транзакция в runtime не исполнялась.

**Минимальное исправление:** добавить конкретный интерфейс contributor payout в Q5/Q7 и retention ledger: какие связанные commitments/данные переживают Lysis, кто при offline owner вычисляет пропорциональные выплаты и округление, куда зачисляются скрытые права/средства, как закрывается remainder. Либо отдельно принять допустимость этого публичного денежного результата/отключение данной ветки для private Tribute. Нельзя без отдельного решения считать public native payout совместимым с C02. Это не требует сейчас проектировать весь Intex.

**Что уже было честно открыто:** документ прямо сообщает отсутствие аудита остальных Intex/Credis consumers (`IMPLEMENTATION:41,262,264`). Новая часть замечания — конкретный обязательный consumer, его формула и наблюдаемая утечка, доказывающие, что одного `C(a)` на выходе недостаточно.

### B-02 — High: wallet-only Gratis transition не покрывает внешние записи и offline Fidelity mutations

**Документ:** `IMPLEMENTATION:292–307` строит account transition с opening старого баланса у кошелька; `311–319` говорит о доказательстве владельцем каждого изменения Fidelity и передаче shares нового состояния. `TRACE R06:203–217` перечисляет acquisition на claim и sale на withdrawal, но не существующий forced sale из collateral lifecycle.

**Первичные code references:** [pledge/probe](../../../crates/core/gratisfactory/src/runtime.rs#L71), [Credis collateral release](../../../crates/core/credisfactory/src/runtime.rs#L242), [release меняет тот же balance blob](../../../crates/core/gratis/src/runtime.rs#L423), [Credis void](../../../crates/core/credisfactory/src/runtime.rs#L266), [daily вызов без владельца](../../../crates/core/credisfactory/src/called.rs#L149), [burn pledged с Fidelity](../../../crates/core/gratis/src/runtime.rs#L464).

В `release_to_eoa` (`gratis/runtime.rs:435–439`) читаются и переписываются **те же** liquid balance и pledged state; вызов не принимает wallet witness. В `void_position` (`credisfactory/runtime.rs:273–290`) enclave раскрывает EOA, затем сжигает pledged Gratis и применяет Fidelity `Out` к истории этого EOA. Daily scan вызывает void без владельца (`called.rs:169–172`). Это влияет на последующий R06 snapshot, даже если пользователь после admission offline.

**Конкретный незакрытый сценарий:** владелец с действующей collateral position имеет private Tribute и уходит offline. До READY позиция voids. Сохранение старого вызова сохраняет TEE и несовместимый ciphertext формат; пропуск вызова меняет Fidelity league; требование wallet proof останавливает автоматический lifecycle. MPC только для чтения league не заменяет доказанную запись нового cohort state. При release в hash-committed account исполняющий модуль дополнительно не имеет старого opening и не может сам построить правильный новый C(balance) по описанному wallet-only API.

**Минимальное исправление:** явно описать границу всех writers одного private account: pledge/unpledge/release/forced burn, liquid+pledged conservation, authority этих операций и units18. Для offline изменений определить, кто имеет достаточные authenticated shares/witnesses, кто строит transition proof, как обновляет версии и доставляет владельцу recoverable новый witness. Для relevant Fidelity включить forced Out до snapshot в trace и проверку состояния. Альтернатива — явно принятая изоляция private account от этих операций с определённым обращением уже активных позиций. Просто объявить весь Credis «вне аудита» эту зависимость не устраняет.

**Что уже было честно открыто:** общая Fidelity/MPC/input-binding/storage задача и отсутствие общего аудита Credis указаны. Новое замечание — внешние writers, которым нужны разные полномочия и offline mutation API, а не только ещё один benchmark той же формулы.

## 4. Матрица Q1–Q7

`Покрыто` ниже означает проверенное математическое отношение или конкретное требование интерфейса; не production readiness.

| ID | Статус | Результат проверки |
|---|---|---|
| Q1 | Условно | Native Baby/Groth16 и LegoCP_link имеют подходящий statement/interface. Нужны полный canonical circuit, same-C proof, точные input counts/VK bundle, setup, source privacy и полный cold peak. |
| Q2 | Покрыта алгебра; протокол условен | Same-C Pedersen VSS и scalar opening работают. Receipts≠common coverage; private redistribution и Ready обязательны. Полного malicious/mobile implementation/theorem нет. |
| Q3 | Условно | Поздние публичные selectors применимы к retained individual state; одного S недостаточно. Цена packing/regrouping/retention не установлена; B-01 продлевает требование к sufficient state. |
| Q4 | Неполно | Cohorts/LIFO и exact comparison identity корректно разобраны; MPC state binding и публичная проверка остаются открыты. В lifecycle отсутствует forced Out из B-02. |
| Q5 | Неполно | Exact g/c, limb balance, hidden payment и atomic transition в принципе реализуемы. Denomination/cost policies открыты; B-01/B-02 требуют дополнительных интерфейсов. |
| Q6 | Условно | Same-value fresh debit и residual groups алгебраически достаточны для Nod forfeit. Нужны completeness, versions, durable debit и согласованный F output. Это не освобождает от других consumers. |
| Q7 | Неполно | Нужные VK/wire/DA/checkpoint требования перечислены, но полного формата и replay/certification реализации нет; B-01/B-02 расширяют обязательные переходы. |

## 5. Матрица C01–C14

| ID | Статус | Основание |
|---|---|---|
| C01 | Неполно | Нет полного no-TEE transition set для shared Gratis/Fidelity — B-02. |
| C02 | Неполно | Основной claim может быть скрытым, но contributor payout требует отдельного privacy решения — B-01. Source payload и все supply/event форматы также gates. |
| C03 | Условно покрыто | S получается интерполяцией scalar shares, с accepted-root/count и no-wrap; не через DLog. |
| C04 | Покрыто на уровне интерфейса | Nod копирует исходный C(a), f/p/bucket/formula authenticated; claim доказывает тот же a. |
| C05 | Условно | Offline выпуск Nod и forfeit допускаются retained shares; Fidelity read/prove и внешние mutations ещё не замкнуты. |
| C06 | Покрыто для current source | Storage U256 сохраняется; доказанные a<2¹⁰⁴ однозначно кодируются. Arbitrary-U256 admission требует другой профиль. |
| C07 | Не подтверждено измерениями | Billion profile сохранён; проверены формулы объёма/нагрузки, не обработан миллиард записей. |
| C08 | Условно | 50h+12h — source/default и scenario. Hourly handoff учтён в модели; concrete mobile security/throughput не подтверждены. |
| C09 | Покрыта арифметика; миграция условна | g18=a6·f6·10⁶, c18=a6·f6·p6. Denomination, zero/overflow и shared writers требуют выбранных правил. |
| C10 | Не подтверждено | Нет полного cold A/B P_link ≤512000000 B; 398.049 MB — только исторический компонент. |
| C11 | Покрыто на уровне отношения | b_old=b_new+x18; x/recipient публичны, остаток закрыт, при приватных остальных delta. |
| C12 | Покрыто | SEAL исключён из предложенного shortlist. |
| C13 | Покрыто для current source | R01+checked u32 count дают S<2¹³⁶<2²⁵⁶; альтернативные пути должны доказывать тот же bound или закрыто проверять overflow. |
| C14 | Покрыто как разрешение | S/S_l открываются после freeze/snapshot. Singleton/differencing утечки самих разрешённых outputs не исправляются криптографией и не объявляются нарушением принятой output-relative privacy. |

## 6. Где существенных математических замечаний не найдено

### P_link и поля

У Baby-Jubjub действительно разные coordinate/scalar fields; pinned arkworks задаёт cofactor 8, `a=1`, `d=168696/168700`, и указанный 251-bit scalar modulus. Для a<2¹⁰⁴ переход однозначен. Native group gadget не избавляет от limbs/carry для integer products. Требования к subgroup/encoding/H обязательны. Первичные источники: [pinned curve](https://github.com/arkworks-rs/algebra/blob/4d708c733d9340dedf3b3a8b41718d3370d8cf46/curves/ed_on_bn254/src/curves/mod.rs), [pinned Fr](https://github.com/arkworks-rs/algebra/blob/4d708c733d9340dedf3b3a8b41718d3370d8cf46/curves/ed_on_bn254/src/fields/fr.rs).

В pinned Dock source `link_d` действительно строится из committed witnesses и `link_v`; при первом committed witness a и bases [G,H] это требуемый C(a). ProofWithLink содержит пять G1 и один G2, т.е. заявленные 224 B при выбранном compressed BN254 encoding; повторно добавлять C(a) нельзя. `verify_proof_incl_cp_link` проверяет link и Groth proof; отдельные `pvk/vk` и проверка только верхней границы input length в `prepare_inputs` оправдывают описанный adapter. Не выполнены proving, malformed-wire tests или extended-CRS ceremony. Первичные источники: [generator:167–197](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/generator.rs#L167), [prover:200–233](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/prover.rs#L200), [proof types](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/data_structures.rs#L8), [verifier](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/verifier.rs).

### VSS, repair и ротации

Same-C constant, degree t−1, парные shares и их проверка соответствуют Pedersen VSS. Интерполяция sum shares возвращает число и blind; bound<q связывает поле с точным integer. Проверена первичная схема и Lemma 4.2 на страницах 132–133: [Pedersen 1991](https://cgi.di.uoa.gr/~aggelos/crypto/page8/assets/Pedersen-VSS.PDF).

Неравенства `Q−f−d≥t` и `Q≤n−f−d_ingress` решают разные counting задачи. Контрпример раздельных receipts в AGGREGATION §2.4 корректен. Проверка `B_h,0=λ_h E_i(h)` и summing fresh subpolynomials сохраняют a/r; неполный helper set нельзя молча принять. 200 независимых небольших randomized scalar resharings сохранили оба constant terms и восстановление. Это алгебра, **не доказательство конфиденциальности исполнения**, malicious completion или mobile security.

Прочитанные первичные источники подтверждают, что именно переходные corruption windows, refresh и recovery требуют отдельной модели. CHURP временно меняет структуру shares, чтобы выдержать old+new leakage; простого появления новой committee недостаточно. [Работа соавтора CHURP, главы 3, 5–6](https://www2.eecs.berkeley.edu/Pubs/TechRpts/2019/EECS-2019-62.pdf). Packed DPSS даёт amortized bound при конкретных packing/degree/committee условиях и synchronous assumptions; §4.2 ограничивает изменение размера и совместимость old/new threshold. Эти результаты не являются theorem для предложенного Outbe fallback. [Baron et al., §§4–5](https://web.cs.ucla.edu/~rafail/PUBLIC/188.pdf).

Поздние лиги требуют различимых inputs; residual groups требуют одинаковых coefficients/lifecycle и exact membership. Fresh debit blinding допустим при proof равенства a и согласованном обновлении residual commitment. Его изменение атомарно с consumption/payment/balance/Fidelity. До finality/остальных consumers удаление запрещено. DKG подписи, сохранённый ciphertext и receipt не заменяют эти свойства.

### Fidelity и bounded integers

По `fidelity-math/lib.rs:48–88` и enclave `fidelity.rs:156–222` подтверждены LIFO, active/sold contributions, saturating differences и nested floors. При public weights threshold identity выводится обратным применением `floor(X/Y)≥k ⇔ X≥kY`; 12 сравнений достаточно для 4096 slots. D=0, z=0, w=0 и unqualified ветки необходимо обработать отдельно. Ширина nominal ничего не доказывает про lifetime A/D: текущий checked U256 success-domain и proposed wider domain нельзя смешивать.

MP-SPDZ действительно имеет разные malicious/semi-honest protocol families; active-secure computation не превращается автоматически в publicly verifiable proof. [Официальная protocol matrix/security notice](https://mp-spdz.readthedocs.io/en/latest/readme.html). Корректный конечный SNARK сам по себе не доказывает privacy distributed proving: первичная публикация прямо описывает invalid-witness и compiler composition pitfalls. В этой проверке прочитан abstract; полный PDF не загрузился, поэтому положительные теоремы конкретного coSNARK не объявляются проверенными. [Garg et al., CRYPTO 2025](https://eprint.iacr.org/2025/1026).

### Storage, recovery и масштаб

Проверена расчётная арифметика: a — 104 бита; S для 10⁹ — 134; для full u32 count — 136; S18 — 174/176. Bound F≤0.32·S18 ещё теснее (172/175 бит на точных максимумах), поэтому приведённые 174/176 upper bounds безопасны. Cost и lifetime account state ими не ограничиваются.

32N bytes commitments, 64N bytes share pairs/holder, 320/384 GB A/B core payload при t=6, 190-byte illustrative Nod, ~960-byte binary Merkle path и 3,906,250 primary shards по 256 — расчётные размеры выбранных форматов. Это не DB size, wire size всего Tribute или измеренная TPS. Hourly модель 36.5N live-record moves, 5555.56 isolated admissions/s и 11574.07 steady admissions/s согласованы. WAL, двойной handoff checkpoint, encryption/receipts, overlapping days, cohorts и active rights остаются дополнительными затратами. Ciphertext/key-only handoff у threshold Paillier условен корректным same-key proactive протоколом; независимые hourly keys нельзя агрегировать без нового механизма.

Wallet seed/root не восстанавливает a/r/balance salt/cohort witness. Нужны recoverable encrypted backups и policy получения актуального witness после каждого state transition; B-02 добавляет transitions, которые не создаёт сам wallet. DA roots также не заменяют доступные тела/proofs. Часовая ротация не проверена как deployment configuration.

## 7. Что реально исполнено и что осталось предположением

**Исполнено:** повторная проверка HEAD и всех manifest hashes; source reads и адресные cross-module searches; `python3 testing/lysis-privacy-prototype/research_arithmetic_checks.py` — 137117 successful identity cases; собственные 200 randomized weighted resharings; exact source/S18/F bounds; маленький числовой contributor counterexample. Пиковые prover нагрузки, builds и production/runtime транзакции не запускались.

**Source-only:** денежные маршруты и два найденных consumers; pinned Baby/Dock API и proof element layout. Pinned источники прочитаны по HTTPS после cache miss web-инструмента. Pedersen PDF проверен визуально; DPSS и CHURP author text прочитаны в релевантных sections. Для части ePrint PDF получены 403/robots; использовались доступные первичные авторские копии, а где их не было — граница проверки указана явно. Исторические 398/497/19445 MB цифры приняты только как сохранённые результаты компонентов, без нового воспроизведения.

**Не подтверждено:** soundness/zero knowledge фактического полного circuit, source deployment/payload privacy, ceremony и security target, malicious/mobile composition/repeated-repair leakage, physically enforced erasure, input-to-MPC binding, coSNARK security, availability при выбранных n/t/f, полный wire/verifier/replay/bootstrap, 512 MB A/B и throughput до 10⁹. Ни один из этих пунктов не получает статус PASS от успешной scalar arithmetic проверки.

**Общий итог:** математические строительные блоки дают жизнеспособное направление. Утверждать завершённую теорию всей композиции или полную учтённость денежного lifecycle пока нельзя; прежде всего нужно закрыть B-01/B-02 и уже перечисленные протокольные gates.
