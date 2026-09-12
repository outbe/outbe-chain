# Реализация приватного Tribute → Lysis → Nod → Gratis без TEE

Проверка полноты и границы готовности: [RESEARCH_COMPLETION_REVIEW.md](RESEARCH_COMPLETION_REVIEW.md). Этот отчёт выбирает направления реализации; прохождение сквозного цикла и ресурсных ограничений ещё не подтверждено.

Исправления после трёх независимых reviews: [замечания и их disposition](REVIEW_REMEDIATION.md), [обязательные downstream consumers R15–R18](DOWNSTREAM_PRIVATE_STATE_TRACE.md). Уточнённые интерфейсы-кандидаты не считаются автоматически принятыми правилами протокола.

## 1. Вывод и рекомендуемое направление

Для описанного маршрута подходит **сочетание пользовательских ZK proofs, Pedersen commitments, проверяемого распределённого хранения секретов и ограниченного MPC**. Одна библиотека весь протокол не реализует. Публикация итогов `S` и `S_l` позволяет оставить экономический kernel Lysis обычным целочисленным кодом. Закрытые вычисления нужны для получения этих итогов, Fidelity и последующего учёта непогашенных прав.

Для P_link есть два конкретных кандидата, которые устраняют причину прежнего большого расхода памяти:

1. **Обычный Groth16 над BN254 + native Baby-Jubjub commitment внутри circuit.** Координатная арифметика commitment выполняется в том же поле, что и circuit. Реализация: `ark-groth16`, `ark-r1cs-std`, `ark-ed-on-bn254`; альтернативный frontend — Circom с native prover Rapidsnark. Это первый кандидат для контрольного измерения из-за близости к имеющемуся source circuit.[^1][^2][^3]
2. **LegoGroth16 с CP_link + Pedersen непосредственно в BN254 G1.** Связь с commitment обеспечивается commit-and-prove конструкцией, без вычисления elliptic-curve commitment внутри R1CS. В `docknetwork/crypto` есть нужные API, включая внешние основания commitment. Это второй кандидат для сравнения: криптографическая конструкция сложнее стандартного Groth16, зато устраняется сам group gadget.[^4][^5]

**Для обоих кандидатов прохождение 512 МБ пока не доказано.** Предыдущий source-only эксперимент с пиком 398.049 МБ — полезный ориентир, но ни один новый полный P_link не измерен. Подменять этот пробел оценкой числа constraints нельзя.

Для агрегации наиболее прямой путь — Pedersen VSS с проверяемым переносом состояния при ротации. Его оптимизированный конкурент — packed distributed proactive secret sharing. Threshold Paillier остаётся сравнительным вариантом: перенос небольшого ключевого состояния потенциально дешевле переноса всех индивидуальных shares, но ciphertext значительно больше, а надёжная динамическая схема также требует отдельной реализации.[^6][^7][^8]

Три результата исследования меняют архитектурный выбор:

- Действующий source codec даёт **104-битный nominal и 134-битный итог миллиарда Tribute**. Для этих значений широкое поле P-384/BW6 не требуется. Тип и внешний предел остаются uint256.
- **Индивидуальные закрытые данные нужны до поздней группировки.** После определения лиг и Nod buckets их можно преобразовать в закрытые групповые остатки; корректный claim будет уменьшать такой остаток. Это позволяет не хранить первоначальные shares каждого Tribute до истечения каждого Nod.
- **Fidelity остаётся самостоятельной задачей.** Для полностью скрытой истории нужен MPC с корректным state transition. Если временные коэффициенты доступны публично, точное определение лиги можно свести к сравнению двух закрытых взвешенных сумм; скрытые деления можно исключить. Этот вариант условен относительно формата приватной истории.

Рекомендация — сначала проверить два P_link кандидата и сквозной VSS state lifecycle, а наиболее тяжёлый отдельный эксперимент направить на Fidelity. Внедрение всего маршрута сейчас преждевременно: не закрыты стоимость ротации при миллиарде записей, приватный платёж с точностью 18 знаков и правила публикации позднего forfeit.

## 2. Исходные условия и границы вывода

Нормативное задание — [PROTOCOL_TRACE_AND_REQUIREMENTS.md](PROTOCOL_TRACE_AND_REQUIREMENTS.md), R00–R18 и C01–C14. Исследование выполнено по состоянию источников на 11 сентября 2026 года. Ниже «кандидат» означает применимость конструкции при перечисленных условиях, а не готовность библиотеки к запуску сети.

| Условие | Следствие для выбора |
|---|---|
| TEE исключён; SEAL исключён | Нельзя использовать enclave для восстановления суммы или скрытого Fidelity snapshot |
| `S` и `S_l` можно раскрыть после закрытия | Секретный nonlinear Lysis kernel не нужен |
| Owner может быть offline после admission | Сеть должна иметь shares/другое достаточное состояние для будущих групп и Fidelity |
| 50 часов admission, смена валидаторов | Нужны handoff, repair, сохранение принятого набора и защита от последовательных компрометаций |
| Индивидуальный nominal и дневной итог — uint256 | Любое кодирование должно представлять целое, без modular wrap |
| `a6`, `f6`, `p6`; целевые load/cost/Gratis18 | `g18=a6*f6*10^6`, `c18=a6*f6*p6`; индивидуальное округление этих произведений отсутствует |
| P_link в кошельке ≤512 000 000 B RAM | Измерять весь cold lifecycle, включая загрузку ключей и сериализацию |
| До `10^9` Tribute | Оценивать O(N) данные, fanout, ротации, DA, overlapping days и остатки Nod |

Сведения о коде основаны на указанной трассировке и её [реестре исходников](TRACE_EVIDENCE.json); все 24 хеша исходников повторно совпали. Дополнительно прочитаны canonical hash и Fidelity math. После независимой проверки добавлен [trace обязательных consumers](DOWNSTREAM_PRIVATE_STATE_TRACE.md): Intex contributor payout, writers Gratis/Fidelity, Promis→Gratis и Fidelity queries. Это проверка этих интерфейсов, не всей экономики Intex/Credis/Promis и не доставки приватного draft из L2. Точные snapshots просмотренных библиотек сохранены в [DEEP_RESEARCH_LIBRARY_PINS.json](DEEP_RESEARCH_LIBRARY_PINS.json). Они фиксируют прочитанные версии, а не автоматически рекомендуют branch head для production.

## 3. Полный маршрут: исполнитель, данные и метод

В этой таблице описан **предлагаемый вариант на VSS**. `C(a)` создаётся один раз кошельком; `P_link` может быть реализован любым из двух кандидатов §4. Для Fidelity указаны альтернативы §7, а не подразумеваемый скрытый TEE.

| Trace | Кто действует | Что делает и создаёт | Публичный результат | Закрытое состояние и срок |
|---|---|---|---|---|
| R00, offer | L2 и кошелёк | L2 передаёт canonical draft и P_L2; сеть проверяет root authority | P_L2, его public inputs, разрешённый контекст | Полный draft у кошелька; исходные суммы не попадают в публичный L2 payload |
| R01, nominal | Кошелёк | Считает целочисленный nominal; выбирает `r`; создаёт `C(a)` и P_link | `C(a)`, P_link, доказанные связи с P_L2/Oracle/owner/day | `a,r,draft` у кошелька |
| R02, приём | Кошелёк, получатели shares, consensus | Кошелёк создаёт VSS полиномы; получатели проверяют shares; consensus атомарно принимает proof и availability receipt | Polynomial commitments/проверяемый эквивалент, accepted root/count/epoch | По share pair у держателя; pending отделён от accepted |
| R03, накопление и ротация | Держатели shares | Обновляют дневной accumulator; переносят достаточно данных для последующей группировки | Проверяемые checkpoints и handoff context | Индивидуальные shares сохраняются до последних потребителей, требующих разбиения |
| R04, закрытие | Consensus и держатели shares | Фиксируют final set; восстанавливают только сумму и суммарный opening | `S`, opening/certificate, привязка к finalRoot | Индивидуальные shares пока сохраняются |
| R05, Metadosis | Обычный runtime | Выводит бюджеты из `S` и публичных лимитов | `B`, request budget, public receipt | Новых секретов нет |
| R06, Fidelity | MPC исполнители либо принятый иной механизм | Вычисляют лиги по актуальному приватному state и точному timestamp | `owner→league`, snapshot root, доказательство/оговорённый сертификат | Приватные cohorts, shares и commitments их состояния |
| R07, суммы лиг | Держатели shares | Локально суммируют shares по публичным membership keys, открывают результаты | `S_l`, `n_l`, проверка полного разбиения accepted set | Индивидуальные данные ещё нужны для формирования остаточных групп |
| R08, коэффициенты | Lysis | Обычный deterministic integer kernel и exact monetary normalization | `f_l`, `G`, подтверждённые roots | Секретные суммы отдельных Tribute не читаются |
| R09, Nod | Lysis worker | Собирает descriptor с прежним `C(a)`, immutable terms, bucket и формулой | Nod descriptors/root; output certificate | Кошелёк сохраняет opening; агрегатор строит residual state |
| R10, unused budget | Runtime | Проверяет `G=ΣS_l*f_l*10^6`, возвращает `B−G` | Возврат и roots всех прав | Shares отдельных Tribute можно удалить после подтверждённого преобразования и окончания иных consumers |
| R11, call | Nod runtime | Применяет публичные price/time/count правила | Qualification, call terms, deadline | Закрытые остатки активных групп |
| R12, claim | Кошелёк; ноды проверяют | Создаёт proof расчёта `g,c`, оплаты, нового Gratis/Fidelity и закрытого debit остатка | Новые commitments, nullifiers/nonce, принятый переход | `a,r`, balance/payment/Fidelity witnesses у кошелька; новые shares у исполнителей |
| R13, COEN | Кошелёк; runtime | Доказывает уменьшение Gratis на публичный `x18` | `x18`, получатель, новый balance commitment | Остаток Gratis и его opening скрыты |
| R14, forfeit | Держатели residual shares; runtime | Фиксируют непогашенный набор; восстанавливают разрешённый остаточный aggregate | Точный возврат `F18`, закрытие прав | Residual state удаляется после завершения всех его consumers и finality |
| R15, Intex payout | Wallet при claim либо закрытый исполнитель | Доказывает floor пропорциональной выплаты по certified eligibility и скрытому total | Commitment payout right, round/spent markers; индивидуальный payout скрыт | Opening nominal и denominator/round state до завершения всех прав |
| R16, collateral writers | Owner, authorized factory/runtime и MPC | Pledge/release/forced burn меняют compartments и Fidelity по единому versioned contract | Commitments, authority, state roots; скрытые delta | Recoverable owner payloads и достаточные shares для offline Out |
| R17, Promis→Gratis | Authorized conversion caller, prover/MPC; validators | Связывает burn Promis6 с mint Gratis18 и новым Fidelity In | Proof, source nullifier, новые commitments | Источник/границы supply; private conversion witnesses |
| R18, Fidelity read | Владелец локально либо выбранный private-output сервис | Вычисляет точный RCFI по authenticated history/time/global context | RCFI только владельцу | Восстанавливаемая история, соответствующие roots |

Ссылка на descriptor/root позволяет пользователю позднее скачать публичные условия. Она **не позволяет восстановить `a,r` или приватный balance witness**. Для этого кошельку нужен собственный защищённый backup. Архивный root также не заменяет доступность публичных тел и доказательств.

## 4. Q1: P_link и предел 512 МБ

### 4.1. Единый statement для обоих кандидатов

| Вход | Кто задаёт | Что обязан проверить verifier |
|---|---|---|
| `derived_owner,nft_hash,binding_hash,root` | Из проверенного P_L2 | Точное совпадение между proofs, правильная версия P_L2 и разрешённый root authority |
| owner/sender, chain, day, currencies, source markers | Transaction и chain state | Авторизация, offering window, canonical encoding, uniqueness/SU reuse |
| `vI,vR,sc` | Oracle за соответствующий day | Значения берутся из chain state; клиент не выбирает выгодные цены |
| `C(a)` | Кошелёк | Привязка к bounded nominal и той же VSS записи |
| VK/circuit/formula/commitment version | Protocol registry | Разрешённый набор параметров; несовместимые версии отвергаются |
| Private `draft_id,base,remainder,a,r` | Кошелёк | Canonical draft hash, identity/binding, деление и opening commitment |

Экономическое отношение:

```text
u6 = base*10^6 + remainder
0 <= base < 2^64; 0 <= remainder < 10^6
e = max(vR, sc); vI>0; vR>0
u6*10^6*vR = a6*(vI*e) + division_remainder
0 <= division_remainder < vI*e; a6>0
```

Проверки — над точными целыми, с limbs/carry/range constraints для широких промежуточных произведений. Равенство только в поле SNARK недостаточно. Native curve устраняет эмуляцию **кривой**, но не отменяет проверку целочисленной арифметики.

P_L2 проверяется отдельно; рекурсивное включение его verifier в P_link не требуется для sound composition, если оба statements проверены и их общие значения совпадают. Proof linking не исправит источник, который уже публично опубликовал скрываемую сумму.

**Различать TributeId и private draft_id.** Текущий canonical hash включает draft id; прежний source circuit держит его private. Публичный deterministic hash не является самостоятельным hiding commitment для суммы с небольшим пространством вероятных значений. Нельзя без проверки публиковать draft seed и все остальные preimage fields, рассчитывая, что Poseidon сохранит приватность. Нужно подтвердить приватность/энтропию seed и L2 payload; свежий Pedersen `r` не исправляет утечку через другой публичный hash. Это отдельная граница R00/R01, а не проблема суммы дня.

### 4.2. Почему достаточно небольшого поля

Из текущего codec и `e>=vR`, `vI>=1` следует:

```text
u6 < 2^64*10^6
a6 <= u6*10^6 < 2^104
N<=10^9  => S6<2^134
N<2^32  => S6<2^136
```

У `ark-ed-on-bn254` subgroup order
`q=2736030358979909402780800718157159386076813972158567259200215660948447373041` — 251 бит. У BN254 G1 scalar field — 254 бита. Обе границы заведомо выше дневного nominal. Поэтому суммирование по модулю q совпадает с целым итогом для **доказанного current-source profile**.[^2]

Это не ограничение uint256 по желанию библиотеки. Если появится admission произвольного 256-битного nominal, нужны другой профиль и проверка текущего суммарного переполнения. Возможен вариант с двумя 128-битными limbs: каждую компоненту складывать отдельно, выполнять carry и приватную проверку total при admission. Само по себе появление полного uint256 не обязывает выбирать P-384. Но простая гарантия отсутствия переполнения, используемая сейчас, тогда исчезает.

### 4.3. Кандидат A: native commitment внутри обычного Groth16

`ark-ed-on-bn254::Fq` совпадает с полем circuit `ark_bn254::Fr`. Кошелёк доказывает `C=aG+rH`, используя constrained bits скаляров и native-coordinate group operations. VSS работает над **scalar field Baby-Jubjub**, а не над её coordinate field. Перенос `a` между ними однозначен по 104-битному range proof.[^2]

Нужно зафиксировать точную модель кривой, subgroup, сериализацию, основания и derivation H. В arkworks используется нормализованная Edwards-модель `a=1`, `d=168696/168700`, cofactor 8; нельзя смешать её байты и generators с другой Baby-Jubjub библиотекой без конверсии. Проверять canonical point encoding, prime subgroup и запрещённые вырожденные параметры. H нельзя получать как `known_scalar*G` с известным участнику scalar.

Обычный compressed Groth16 proof BN254 занимает `32+64+32=128 B`; отдельный compressed commitment — 32 B. Это размер криптографических элементов, **не всего Tribute**. Proof API содержит два G1 и один G2.[^1]

### 4.4. Кандидат B: LegoGroth16 + CP_link

В `docknetwork/crypto/legogroth16` найден конкретный путь:

```text
commit_witness_count = 1
first committed circuit witness = a6
link_bases = [G,H]                  # общие для денежных commitments
r_external <- CSPRNG.uniform(Fr)   # opening денежного commitment
C_a = a6*G + r_external*H
v_internal <- CSPRNG.uniform(Fr)   # независимо, заново на каждый proof
proof = create_random_proof_incl_cp_link(
    circuit, v_internal, r_external, registered_pk, csprng)
require proof.link_d == C_a
verify_proof_incl_cp_link(...)      # обычные public inputs отдельно
```

`generate_random_parameters_incl_cp_link` принимает внешние commitment generators. `create_random_proof_incl_cp_link` получает **два отдельных blinder от caller**: `v` и `link_v`; сама выбирает только Groth16 randomness `r,s`. Дополнительное доказательство связывает внешний `link_d` с внутренним commitment к witness. Верификатору не передаются `a,r_external,v_internal`. Функции `verify_commitments`/`verify_link_commitment`, принимающие opening, используются prover-side и не заменяют публичный verifier.[^5]

**Обязательный prover adapter, исправление IR-03:** публичный `proof.groth16_proof.d = a*K + v_internal*J`, где K/J происходят из CRS. Известный `v_internal=0` позволяет проверять догадки a по `a*K`, даже при правильно скрытом внешнем C(a). Адаптер сам получает независимый свежий `v_internal` из CSPRNG для каждого proof; запрещены фиксированные/reused defaults, вывод в логи и использование внешнего opening как внутреннего blinder. Независимость — обязанность prover, публичный verifier не может проверить качество его случайности. Это пропуск прежнего рецепта интеграции, не дефект конструкции LegoSNARK. [Pinned prover, выбор randomness и построение D](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/prover.rs#L32).

Кошелёк сохраняет `a,r_external` для денежных consumers; `v_internal` нужен только текущему proving и затем удаляется вместе с временным witness. Библиотечные `r,s` не совпадают по назначению ни с одним из них. Будущая проверка адаптера должна включать два полных proofs одной суммы со свежими internal blinders, валидность обоих и проверку отсутствия детерминированного D; отдельный намеренно плохой `v=0` fixture показывает возможность проверки кандидатов. Такая проверка не доказывает качество CSPRNG и ещё не выполнена; полный run подчиняется пределу 512 МБ.

`ProofWithLink` содержит пять G1 и один G2: **224 B на BN254**, включая `C(a)=link_d`; второй раз 32 B за него добавлять не нужно. Внутренний `proof.d` зависит от CRS; для совместимости circuit versions денежным интерфейсом следует сделать общий внешний `link_d`.[^5]

Плюс — source circuit не нуждается ни в P-384, ни в Baby-Jubjub group gadget, ни в SHA bridge. Минусы — дополнительный pairing-based proof layer, расширенный setup, специальный verifier и зависимость текущей реализации от arkworks 0.4, тогда как локальный прототип использует 0.5. Проектная версия `legogroth16=0.18.0` и готовые API не доказывают завершённый аудит этой композиции. Требуются отдельные отрицательные проверки witness order, подмены link bases, неправильного opening и перехода circuit versions.[^5]

Прямой вызов single-party `generate_random_parameters` не является production ceremony. Для LegoGroth16 необходимо отдельно построить/проверить генерацию расширенного CRS; наличие обычного Groth16 Powers of Tau не закрывает это автоматически. Для обоих кандидатов нужен зафиксированный security target: BN254 pairing не следует называть 128-битной конструкцией; актуальный CFRG Internet-Draft относит его к примерно 100-битному уровню. Если требуется иной уровень, пересматривается весь source/proof stack, например BLS12-381+Jubjub; перенос Poseidon из исходного поля тогда имеет цену.[^9]

**Обязательный verifier adapter, уточнённый при повторной проверке:** библиотечный `verify_proof_incl_cp_link` принимает подготовленный Groth16 VK и полный VK с link parameters отдельными аргументами. Адаптер получает один зарегистрированный bundle и сам готовит `pvk` из его `groth16_vk`. Клиент не передаёт второй независимо выбранный ключ. `prepare_inputs` проверяет только верхнюю границу длины относительно `gamma_abc_g1`, поэтому адаптер обязан требовать точный layout и `len(public_inputs)+1+commit_witness_count == len(gamma_abc_g1)`, включая implicit one. Для данного кандидата `commit_witness_count=1`, порядок committed witness фиксирован manifest. Также обязательны полное потребление wire bytes, canonical scalar/point decoding, subgroup checks и одинаковый `C(a)` в proof, VSS и accepted record. Это требования нашей интеграции, не заявление об исполненном exploit upstream.[^27]

### 4.5. Что реально известно о памяти

| Локальный результат | Peak RAM | Что измерено | Вывод |
|---|---:|---|---|
| Прежний P-384-in-BN254 | 19.445 GB | Полный запуск с setup/proving/проверками | Отклонён при бюджете 512 МБ |
| Source-only BN254→salted digest | 398.049 MB | Cold proving process; PK load 7.408 s, proving 1.325 s, весь process 8.827 s | Проверенный компонент, не полный P_link |
| Wide-native→digest | 497.418 MB | Proof создан, cold PK load около 41.9 s; композиция не завершена | Малый запас, не принятая схема |
| Новый native Baby-Jubjub P_link | Не измерено | Кандидат по структуре арифметики | Нельзя объявить прохождение RAM limit |
| Новый LegoGroth16 P_link | Не измерено | Проверена применимость API/statement | Нельзя объявить прохождение RAM limit |

Исходные локальные измерения перечислены в [§10 трассировки](PROTOCOL_TRACE_AND_REQUIREMENTS.md#10-что-уже-известно-из-прежних-экспериментов); [source prove](measurements/p-link-source/artifacts/prove.json), [source RSS](measurements/p-link-source/artifacts/prove_resources.json), [source verification](measurements/p-link-source/artifacts/verify.json). Ни один показатель здесь не является результатом нового криптографического запуска.

Измерять нужно фактическую максимальную длину source markers и canonical draft, без неутверждённого ограничения «4 SU». Для browser учитывать WASM linear memory, JS buffers и workers; для mobile — process footprint на устройстве. `snarkjs` имеет настройки `singleThread` и начальной памяти witness calculator, но они не устанавливают верхнюю границу памяти всего prover.[^10]

## 5. Q2–Q3: проверяемое получение S и S_l

### 5.1. Прямой VSS вариант

Пусть `n` — число держателей, `t` — число shares для восстановления, `f` — допустимое число скомпрометированных держателей. Для приватности необходимо `f<t`. Выбор `n,t,f` — параметр протокола; пример 16/6 ниже используется только для арифметики размеров.

Для каждого принятого Tribute кошелёк выбирает два полинома степени `t−1` над scalar field commitment group:

```text
F_i(0)=a_i; R_i(0)=r_i
A_i,k = F_i,k*G + R_i,k*H, k=0..t-1
A_i,0 = C(a_i)

recipient j получает приватно (F_i(j), R_i(j))
проверяет F_i(j)*G + R_i(j)*H = Σ_k j^k*A_i,k
```

P_link подтверждает допустимый **constant term** a и его связь с источником. VSS подтверждает, что shares соответствуют этому же commitment и единой polynomial structure. Shares сами равномерны в поле и не должны ограничиваться 104 битами. Нельзя заменить Pedersen VSS на публикацию Feldman `aG`: детерминированная точка позволяет проверять догадки о небольшом a.[^6]

Получатель подписывает receipt только после проверки и durable записи, с контекстом `(chain,day,TributeId,epoch,commitment/transcript hash)`. При пороге receipts `Q`, до f лживых получателей и до d дополнительных честных отказов нужно `Q−f−d>=t`. Это даёт достаточные доли **каждой записи в пределах модели отказов**, но не означает, что одинаковые t держателей получили все записи. Перед локальным суммированием необходим проверяемый coverage checkpoint либо приватный repair/redistribution, описанный ниже. Admission окончателен только после совместного принятия proof, receipts и accepted-set update; сохранение и repair являются обязательством держателей после ухода владельца.

Чтобы Q вообще можно было собрать при f отказывающихся подписывать узлах, требуется отдельно `Q<=n−f−d_ingress`, где d_ingress — дополнительные недоступные во время приёма. Аналогичное условие проверяется для полного coverage checkpoint. Эти counting inequalities не заменяют network/consensus liveness proof.

Pending inputs, отклонённые proofs и незавершённые deposits не входят в aggregate. Burn во время offering вычитает запись из accepted state. Reorg требует replay/rollback той же операции. Открытие привязывается к final set, а не к локальному счётчику получателя.

### 5.2. Открывается целое число, не discrete logarithm

Перед открытием фиксируется `Ready(finalRoot, epoch, transcriptRoot, coverageRoot)`: один набор хотя бы `t+f+d` держателей подтвердил наличие проверенных актуальных долей **всех** accepted records. Полиномы разных записей могут отличаться, но используют общие координаты получателей и совместимый threshold/epoch profile. Это достаточный baseline для переживания f withholding и d дополнительных отказов; более экономичная процедура допустима только с собственным доказанным условием доступности. Если наборы receipts различаются, сначала выполняется приватный repair. Нельзя подставить нули за пропуски или раскрыть отдельные Tribute ради восстановления. До Ready зависимый этап ждёт; финально принятые права сохраняются.

После закрытия и Ready каждый полный держатель вычисляет:

```text
s_j = Σ_accepted F_i(j)
r_j = Σ_accepted R_i(j)
```

Из t проверенных пар восстанавливаются `S=Σa_i` и `r_sum=Σr_i mod q`. Любая нода проверяет:

```text
Σ_accepted C(a_i) == S*G + r_sum*H
```

Полный VSS transcript дополнительно позволяет проверить opening shares. Правильность публичного числа связана с доказанными inputs, точным accepted set и отсутствием wrap. **Discrete-log поиск не выполняется:** интерполяция работает со скалярными shares. Одни Pedersen commitments или threshold ElGamal в показателе дали бы `S*G`, из которого 134-битный S практически не извлечь.[^6]

Для лиги l держатели суммируют shares только записей из подтверждённого snapshot, сохраняя тот же проверяемый coverage contract. Валидаторы проверяют membership, суммы counts, `ΣS_l=S` и openings против соответствующих sums of commitments. Достоверный итог дня сам по себе не подтверждает достоверность распределения по лигам.

### 5.3. Почему одного накопителя недостаточно

Дневной accumulator хранит одну линейную комбинацию всех сумм. Позднее разбиение по ещё неизвестному `owner→league` требует других линейных комбинаций. Их нельзя в общем случае вывести из одного предыдущего итога. Поэтому до фиксации групп нужны индивидуальные shares либо другое представление, сохраняющее индивидуальную различимость: packed sharing, ciphertexts, либо участие владельцев. Последнее противоречит offline requirement.

Агрегация по owner до закрытия не даёт выигрыша в существующей схеме: на owner/day разрешён один Tribute. Агрегация по лиге заранее меняет snapshot semantics, если лига может измениться за offering/waiting. Packed sharing может уменьшить replication/communication, но не означает хранение только одного scalar на весь день.

После R07 нужно обслужить ещё formation of residual groups, `eligible_nominal_total`, cost/contributor checks и любые неустранённые consumers. Только после проверяемого преобразования всего нужного состояния можно удалить индивидуальные shares.

### 5.4. Ротация — перенос данных, не новый DKG

При обычном resharing старые участники распределяют взвешенные shares выбранного reconstructing set новым участникам. Constant terms новых полиномов должны соответствовать старым authenticated shares; сумма новых полиномов сохраняет прежние `a,r`. Нужны проверки корректности каждого вклада, согласованный qualified set и durable ACK нового состава.[^7]

Смена BLS/DKG ключа подписи не выполняет это преобразование. Более того, клиентская VSS раздача сама по себе не требует единого decryption key: её dealer — кошелёк. Существующий DKG может пригодиться для сертификатов и координации, но не заменяет monetary shares.

Для mobile adversary необходимо ограничение на компрометации в каждой эпохе, корректная refresh/resharing схема и удаление старых shares, каналов восстановления и резервных копий. Сохранённые шифротексты старых shares под впоследствии раскрываемыми ключами могут отменить пользу refresh. Публичный complaint protocol также не должен раскрывать достаточное количество индивидуальных shares. Это обязанности конкретного протокола, а не свойства любого Shamir API.

**Исправление пробела между admission и aggregate:** даже без смены состава неполная матрица receipts может потребовать redistribution. Конструктивный медленный fallback — заново распределить каждую неполную запись через t проверенных старых helpers, сохранив её C(a). Ни один helper не восстанавливает a; каждый создаёт VSS своего взвешенного share. Полная алгебра, обязательные проверки и retry context приведены в [§2.4 приложения](DEEP_RESEARCH_AGGREGATION.md#24-выравнивание-доступности-перед-агрегированием). Стоимость O(t_old*n_new) private share pairs на обрабатываемую запись надо включать в repair/rotation measurements; прежняя строка `64*n` описывает только начальную клиентскую раздачу. Эта алгебра не заменяет malicious/mobile security proof выбранного handoff protocol.

Если старый состав уже исчез и нигде не осталось достаточного закрытого состояния, новый DKG не восстановит принятые суммы. Поэтому «состав сменился» и «старый состав может немедленно удалить всё» — разные события. Истечение handoff SLA должно иметь explicit hold/repair/fail policy.

### 5.5. Packed DPSS и threshold Paillier

| Подход | Что даёт | Что остаётся реализовать | Статус |
|---|---|---|---|
| Обычный Pedersen VSS | Прямая связь `C(a)`↔shares; сумма и public weighted sums; понятный baseline | Durable storage, complaints/repair, proactive handoff, grouping, atomic debit | Основной baseline; масштабирование не подтверждено |
| Packed distributed proactive secret sharing | Несколько секретов в sharing structure; amortized handoff дешевле наивного per-secret resharing | Связать каждый bounded input с packed state, random-access grouping, partial batch, динамические thresholds | Перспективная оптимизация после baseline |
| Threshold Paillier / Damgård–Jurik | Точные аддитивные суммы; recovery числового plaintext без DLog; потенциальный перенос только key shares | Input proof↔ciphertext↔commitment, malicious DKG/decryption, proactive same-key refresh, accepted-query binding | Реальный конкурент по rotation cost, хуже по ciphertext size |
| Prio/VDAF/DAP | Разделение client validation и агрегации; существующие protocol/implementation patterns | Иная схема aggregator roles, epochs, late regrouping, связь с нашим P_link | Использовать идеи и отдельные компоненты, не переносить как готовый протокол |

Packed DPSS — не только теоретическое пожелание: работа Baron et al. рассматривает dynamic proactive sharing с амортизированной коммуникацией; её конкретные условия изменения committee size и thresholds необходимо соблюдать. Это не доказательство O(1) хранения всего дня.[^7]

Tiresias реализует scalable threshold Paillier, но его static-corruption модель не доказывает требуемую mobile security при hourly rotations. При 3072-битном Paillier modulus один ciphertext modulo `n²` занимает 768 B до proofs/metadata. Само число «миллиард ciphertexts» малоинформативно без размера: здесь это **768 GB raw**, тогда как один 32-байтовый commitment — 32 GB.[^8]

Подробные источники, протоколы и ограничения Q2/Q3/Q6 — [DEEP_RESEARCH_AGGREGATION.md](DEEP_RESEARCH_AGGREGATION.md). Политика разрешённых открытий должна быть частью state machine: произвольные промежуточные subset queries не разрешаются только потому, что участники технически умеют их посчитать.

## 6. Q5–Q6: Nod, точные расчёты и остатки прав

### 6.1. Выпуск Nod не требует вычислять его приватную сумму

Worker получает public metadata, `C(a)`, league snapshot и окончательные terms. Он создаёт descriptor с тем же commitment. `g` и `c` вычисляет и доказывает владелец на claim. Детерминированность и полнота выпуска подтверждаются существующим output certification механизмом, адаптированным к новому descriptor; отдельный ZK proof суммы на каждый Nod при выпуске не нужен.

Exact модель обеспечивает:

```text
G18 = Σ_l S_l6*f_l6*10^6
unused18 = B18-G18
```

Здесь возврат не зависит от прихода пользователей. По current-source profile `S18<2^174` для миллиарда входов, а R05 задаёт `B18<=floor(0.32*S18)`. После проверки R10 имеем `0<=F18<=G18<=B18`. Поэтому также и weighted reclaim помещается в native поле кандидатов. Для полного u32 count запас остаётся: `S18<2^176`. Этот bound нельзя переносить на иной budget rule или произвольный future uint256 source.

Для cost такая гарантия автоматически не следует: `p6` — отдельный множитель. Перед выпуском нужны гарантии корректного uint256 cost и доступности denomination. Возможны доказанный общий верхний bound по публичной цене/коэффициенту, закрытая проверка получателями или явно иная policy. «Потом пользователь не сможет доказать claim» не является корректной заменой этой проверки.

Текущие соседние поля finalizer также требуют явной замены:

| Поле/проверка | Кандидат замены | Граница |
|---|---|---|
| `eligible_nominal_total` | Сумма commitments по публичному exclude selector и закрытая conservation check | Раскрытие этого числа отдельно не разрешено автоматически |
| `nod_cost_total` | Weighted commitment/проверяемая закрытая сумма с отдельным integer bound | Публичных S_l недостаточно при разных ценах; mixed-currency semantics сохраняются явно |
| Numeric per-shard budget prefixes | Для проверки только `prefix<=B` достаточно доказанного полного набора, неотрицательности всех g и `G<=B` | Это изменение verifier/schema; оно не заменяет остальные проверки полноты/порядка chunks |
| Contributor output с nominal | Ссылка на исходный C(a), скрытый eligible total и проверяемое право на пропорциональную выплату | Обязателен downstream Intex payout из R15; текущий публичный перевод несовместим с C02/C11. Одной замены поля leaf недостаточно |

Таким образом, public monetary prefixes не обязательны для самой бюджетной границы. Но удаление fields из действующего finalizer требует проследить все их consumers и сохранить соответствующие проверки, а не просто убрать сравнения.

### 6.2. Свёртка индивидуальных shares в residual state

После определения групп формируется, например:

```text
group = (bucket_id, day, fraction/formula version, immutable terms)
R_group6 = Σ_{active rights in group} a_i6
C_Rgroup = Σ C(a_i6)
```

В одну группу должны попасть права с одинаковыми необходимыми коэффициентами и lifecycle rules. Можно хранить nominal residuals и позднее применять публичный коэффициент; тогда не требуется новый opening `g` от offline кошелька. Initial residual state создают держатели shares, а commitments выводятся из принятых Tribute/Nod.

На claim владелец знает `a_i,r_i`, но после ротаций не знает актуальные group-share polynomials. Ему не нужно их знать: он создаёт **новую проверяемую VSS раздачу debit** с тем же a, привязанную proof к расходуемому Nod. Допустим fresh blinding `r_debit`, если proof подтверждает равенство значения в исходном C(a) и debit commitment. Держатели вычитают новые debit shares из residual shares. Public residual commitment обновляется той же операцией.

Атомарно должны произойти четыре действия: приём корректного доступного debit, расход Nod, платёж/Gratis/Fidelity transition и изменение residual root/version. Replay или повторный debit недопустим. До подтверждения нового закрытого состояния claim не final. Приватный размер g не публикуется через event, balance delta или aggregate delta.

После этого исходные per-Tribute shares можно удалить, только если отдельно обеспечен R15 Intex payout (включая закрытый denominator и все будущие rounds), остальные consumers и rollback/finality policy. До forfeit остаются **O(number of active residual groups)** secret accumulators, а публичные активные Nod descriptors и spent accounting по-прежнему имеют индивидуальную гранулярность. Число residual groups может быть большим; оно определяется реальными buckets/terms, а не только числом лиг.

### 6.3. Forfeit

После deadline фиксируется набор remaining rights. Держатели формируют weighted sum residual shares и раскрывают только разрешённый `F18`, с проверкой против weighted sum commitments. Открывать каждый `R_group6` для вычисления F не требуется.

Bound из §6.1 относится к правам одного дневного бюджета. Если один refund объединяет несколько дней, для его суммы нужен отдельный integer bound либо отдельные допустимые дневные outputs; нельзя автоматически перенести на него 174/176-битную границу одного дня.

В текущем коде возврат идёт ограниченными проходами. Можно сохранить эту семантику, сохранив достаточно данных для каждого прохода; либо зафиксировать закрытый остаточный batch и открыть весь его итог один раз. Второй вариант лучше сочетается с компактными residual groups, но меняет timing возврата и должен быть принят отдельно. Разрешение на S и S_l само по себе не утверждает публикацию всех промежуточных reclaim totals.

### 6.4. Приватный Gratis как account state

Для скрытого баланса не обязательно использовать тот же аддитивный commitment, что и для дневной суммы. Практичный кандидат:

```text
balance limbs: b0,b1,b2,b3 ∈ [0,2^64)
C_balance = H(domain, owner, asset, version, b0,b1,b2,b3, secret_salt)
```

Кошелёк доказывает opening прежнего state, арифметику в четырёх limbs, диапазоны и commitment нового state со свежей солью. Это сохраняет **полный uint256 balance**, не сокращая его до scalar field. Вариант с Pedersen commitments отдельных limbs тоже возможен, но требует больше точек и правил conservation. Выбор account model соответствует публичным владельцам в требованиях; анонимный shielded UTXO ledger не является обязательным расширением задачи.

Для начального account нужен proof `b=0`. Для claim — `b_new=b_old+g` без overflow. Для COEN — `b_old=b_new+x`, где x публичен. `version/nonce` связывает transition с единственным текущим состоянием. **Этот рецепт достаточен только для обновления известного владельцу state.** Сторонний settlement и forced collateral burn не могут открыть старый salted hash. Для полного маршрута нужны отдельные compartments/pending credits и authenticated offline transitions, описанные в [R16 дополнительного trace](DOWNSTREAM_PRIVATE_STATE_TRACE.md#r16). Монолитный wallet-only hash account не считается решением IR-02. Модель commitments, spend authorization, nullifiers и проверок balance conservation имеет зрелый пример в Zcash, но его готовый note format не является заменой Outbe account/Fidelity и uint256 arithmetic.[^11]

**Скрыть нужно и платёж Nod.** Публичный `spend_amount=c` раскрывает g/a при известных terms. Для asset с 6 decimals можно, например, депозитить `d6` публичных единиц в закрытый платёжный subledger, получая `d6*10^12` внутренних units18; claim списывает exact c18, дробный остаток остаётся внутри. Вывод наружу выполняется кратно `10^12`, остаток сохраняется. Это конкретная реализуемая accounting модель, но новый протокольный слой, а не существующее свойство PayNote. Альтернатива — явно принятое округление на границе платежа.

C_balance, payment change и Fidelity state обновляются одним проверяемым transition. Открытый total supply, per-claim receipt или event с точной delta может раскрыть скрытое зачисление; эти consumers надо изменить вместе с основным account. Для wallet recovery нужен encrypted witness backup, например canonical payload с AEAD/HPKE, а не только seed доступа к account. HPKE — стандартный транспортный компонент, он не обеспечивает сам по себе VSS validity или data availability.[^12]

### 6.5. Supply: точный профиль эмиссии и единицы

Нельзя выводить пожизненный supply только из дневного S. Для ограниченного профиля `zero initial state + current-source Nod-only + однократный бюджет на каждый WorldwideDay(u32) + отсутствие imports/re-mint` существует достаточный bound:

```text
minted_from_one_day <= G_day < 2^176
cumulative_mint < 2^32 * 2^176 = 2^208
0 <= live_supply <= cumulative_mint < q_Baby < 2^256
```

Тип [WorldwideDay](../../crates/blockchain/primitives/src/time.rs#L138) — u32; фактическое множество календарных дат меньше, что только усиливает грубую границу. В этом профиле отдельный MPC supply range check только ради uint256 overflow не нужен. Для underflow и conservation по-прежнему нужны отсутствие двойной эмиссии/расхода, authorized burns и корректное начальное состояние. Это исправляет чрезмерно общий overflow-контрпример первоначального REVIEW_C; он не доказал достижимую ошибку.

Действующая система содержит [PromisFactory.mine_gratis](../../crates/core/promisfactory/src/runtime.rs#L69). Её private conversion contract и границы upstream mint описаны в [R17](DOWNSTREAM_PRIVATE_STATE_TRACE.md#r17). Для него `mint18=burn6*10^12`, один раз, с проверкой `burn6<=floor(U256_MAX/10^12)`; legacy TEE/MAC и публичный amount необходимо заменить. Нельзя приписать всей системе Nod-only bound.

Общий invariant целевого ledger: `T = initial + Σ(mint_Nod + mint_Promis + other_authorized_mint) − Σ(burn_COEN + burn_collateral + other_authorized_burn)`, и T равен сумме liquid L, pending pledge tickets T_ticket, active pledged A и отдельно incoming credits I; pledged_total включает только T_ticket+A. Один ticket/credit не учитывается дважды. Transfers и merge pending не являются mint; forfeit невыпущенного Nod не является burn уже существующего Gratis. Все дельты связаны с теми же commitments/proofs и source nullifiers; arbitrary modular supply update недостаточен.

Текущие `totalSupply()`, mint/burn events и публичные collateral fields не могут сохранять точные скрытые дельты. Предлагается хранить commitment/proof accounting вместо текущего публичного точного T; агрегаты для публикации кроме S/S_l требуют отдельной policy. Это изменение интерфейса и полноты source accounting, не утверждённое удаление legacy API. Если допустимые mint/import правила не дают глобальный no-wrap bound, нужен wide integer supply proof с проверкой диапазона; выбранный native field не решает эту задачу автоматически.

Отдельная граница units: Credis void возвращает collateral burn в PromisLimit raw6. Для Gratis18 надо либо сохранить exact grid `10^12` и reverse conversion, либо изменить reserve precision/остатки по принятой политике; передать raw18 в существующий reserve6 недопустимо. Варианты и public stake dependency — [R16](DOWNSTREAM_PRIVATE_STATE_TRACE.md#r16).

## 7. Q4: Fidelity без TEE

### 7.1. Сохранение полностью приватной истории

Текущий Fidelity использует active и sold cohorts, LIFO расход, времена операций и qualified_start. Commitment к текущему балансу не содержит сведений, необходимых для этой формулы. Для owner-initiated операции владелец доказывает amount/order и передаёт связанные проверяемые shares; runtime добавляет фактическое время исполнения по контракту §7.4. Изменения без владельца требуют authenticated MPC transition и recoverable witness, см. [R16](DOWNSTREAM_PRIVATE_STATE_TRACE.md#r16). На READY committee вычисляет snapshot по итоговому frozen state/time, включая все такие изменения.

Для первичной оценки подходит **MP-SPDZ**: он реализует malicious и semi-honest варианты arithmetic/binary sharing, comparisons и смешанные вычисления. Для hostile validators нужно выбирать конкретный active-secure protocol — например malicious Shamir или MASCOT — и отдельно задавать threshold/abort/liveness model. Не использовать semi-honest benchmark как оценку принятого malicious режима.[^13]

Кошелёк → commitment → VSS state и VSS → MPC authenticated input требуют доказанного связывания. Наличие Shamir в двух библиотеках не означает одинаковые sharing domains, MAC keys, labels и каналы. Dynamic durable storage вокруг MP-SPDZ также придётся разработать. Его README прямо ограничивает уровень production security review; это framework для feasibility/benchmark, а не готовый validator subsystem.[^13]

Приватные времена/индексы делают доступ к cohorts частью MPC. Нельзя разветвлять обычный код по secret boolean или публиковать изменённые LIFO positions, если это раскрывает скрываемую структуру. Нужны padded state, oblivious access или иной проверенный формат. Без bound на lifetime history нельзя заявить конечную фиксированную стоимость одного snapshot.

### 7.2. Условная оптимизация: публичные времена, скрытые размеры

Если timestamps и допустимая структура cohort slots публичны, весь `T(age)` можно вычислять обычным кодом. Это **не автоматическое разрешение раскрыть приватные timestamps/LIFO split structure**: формат требует отдельной оценки. Публикация того, какая cohort полностью исчерпана, может раскрыть дополнительную информацию; возможны padded slots с нулевыми скрытыми размерами, но их стоимость надо учесть.

При допустимом public-time формате:

```text
A = Σ_active size * public_T(now-acquired)
D = A + Σ_sold size * public_max(T(now-acquired)-T(now-sold),0)
```

A и D — линейные комбинации скрытых размеров. Holders вычисляют shares локально; публичные commitments выводятся теми же весами, с контролем полноты cohort set. Ни A, ни D публиковать не требуется.

**Новый алгебраический вывод по текущей формуле:** пусть `K=10^18`, `z=T(now-qualified_start)`, `w=maximum_rcfi` — публичны и положительны; `D>0`.

```text
e = floor(A*K/D)
r = floor(z*e/K)
slot = min(floor(4096*r/w),4095)

для любого m=1..4095:
h_m = ceil(m*w/4096)
k_m = ceil(h_m*K/z)
slot >= m  <=>  A*K >= k_m*D
```

Это следует из `floor(X/Y)>=k ⇔ X>=kY` для неотрицательных целых и положительного Y. Все ceil thresholds вычисляются публично. Двоичный поиск определяет slot максимум за 12 secure comparisons; промежуточные решения поиска определяются итоговым публичным slot. При `D=0`, `z=0`, `w=0` и `qualified_start=0` сохраняются соответствующие нулевые ветки. Не требуется раскрывать e или r.

Так можно исключить **secret division** из выбора лиги, сохранив вложенные floor. Эквивалентность проверена на 137 117 случаях: исчерпывающая малая область и 20 000 детерминированных больших входов. [Скрипт](research_arithmetic_checks.py), [результат](DEEP_RESEARCH_ARITHMETIC.json). Это проверка целочисленного тождества, не доказательство безопасности MPC и не benchmark.

Дополнительный вариант для publicly verifiable proof: после выбора публичного slot доказывать две соседние threshold inequalities вместо повторного 12-шагового поиска внутри final proof. Committee по-прежнему должен безопасно узнать правильный slot. Эта оптимизация не устраняет необходимость доступа к authenticated A/D witnesses.

### 7.3. Ширина чисел и доказательство результата

Nominal 104 бит не означает, что Fidelity тоже помещается в 104 или 251 бит. `T` достигает примерно `526.58*10^18` — 69 бит. Даже active contribution для uint256 balance может потребовать около 325 бит; denominator включает sold history, чья lifetime сумма дополнительно ограничивается правилами state growth. `A*K` требует ещё порядка 60 бит. Нужны limbs/carries либо более широкая MPC arithmetic domain с доказанными bounds. Нельзя включить probabilistic fixed-point truncation и считать её эквивалентом консенсусного floor.[^14]

Алгебраическая оптимизация выше определена над неотрицательными математическими целыми. Она совпадает с кодом на области, где все checked U256 операции исходника успешны. Сохранение текущих overflow failures либо расширение их диапазона при Gratis18 — отдельное правило миграции.

Есть два разных способа подтвердить MPC snapshot:

| Выход | Кто проверяет корректность | Дополнительное доверие |
|---|---|---|
| Threshold certificate после active-secure MPC | Участники MPC проверяют вычисление; остальные проверяют certificate | Корректность league зависит от честного порога/правил подписи, не только от ZK soundness |
| Collaborative SNARK по distributed witness | Любой validator проверяет ZK proof к state root, time и результату | Нужны приватный distributed prover и корректное input binding; подпись не заменяет proof |

Если требуется независимая криптографическая проверка league обычными validators, нужен второй вариант либо эквивалентный publicly verifiable MPC. CoSNARK — именно нужное семейство, потому что witness распределён. `TaceoLabs/co-snarks` имеет coCircom/coNoir и совместимые Groth16/PLONK/UltraHonk proofs, но текущий README помечает реализацию experimental/un-audited.[^15][^16]

Нельзя считать, что финальный корректный SNARK автоматически защищает witness во время злонамеренного distributed proving. Работа CRYPTO 2025 показывает privacy pitfalls на invalid witnesses и при наивной композиции malicious MPC compilers; положительные результаты имеют конкретные условия. Поэтому для coSNARK path нужен выбор подтверждённого протокола, а не только запуск существующего demo.[^17]

### 7.4. Кто фиксирует время: проверенный переход и timestamped log

**КОД:** [mint/mine_coen](../../crates/core/gratisfactory/src/runtime.rs#L129) читают `storage.timestamp()` при исполнении; [cohort_in/out](../../bin/outbe-tee-enclave/src/fidelity.rs#L156) используют его для acquisition/sale. Wallet proof нового полного root со временем T не подходит включению в T+3 без нового proof либо изменения clock semantics.

**ПРЕДЛОЖЕННЫЙ контракт IR-04:** private Fidelity state представляется аутентифицированным журналом проверенных переходов с публичным временем исполнения и закрытым содержимым. Это кандидат формата, не утверждённый backend и не готовая реализация. Он сохраняет фактическое время исходного кода; не требует предсказывать timestamp блока.

| Шаг | Исполнитель и вход | Расчёт / проверка | Выход и хранение |
|---|---|---|---|
| T1. Подготовка | Кошелёк: latest finalized account/Fidelity roots, nonce, история/openings, сумма и authority операции | Доказывает bounded amount, balance conservation, допустимость In/Out и LIFO amount/order относительно текущей истории. Создаёт `C_delta` к payload и связанные доступные shares | Proof, C_delta, expected old roots, owner/action/nonce. В payload нет заранее придуманного `sold_at/acquired_at` новой операции |
| T2a. Проверка на точном времени | Закрытый исполнитель: T1 payload/shares, актуальные roots и кандидат execution context с timestamp t | Выполняет In/Out/Probe и все обязательные time-dependent проверки, включая successful checked evaluation Fidelity. Создаёт proof либо отдельно принятый certificate к `(old roots,C_delta,t,global context,result)` | Временное свидетельство допустимости. Пользователь не возвращается. Изменение roots/time/context делает его непригодным; денежная операция ещё не final |
| T2b. Исполнение | Runtime: T1 и T2a evidence, current roots, фактический block timestamp `t_exec` | Проверяет `t==t_exec`, версии, authority, availability и результат всех exact-time guards. Только затем добавляет leaf `H(domain,owner,seq,action,C_delta,t_exec,context)` | Новый log root/version и balance/escrow transition атомарны. Root создаёт runtime, C_delta — prover. Без подходящего T2a переход не фиксируется |
| T3. Семантика истории | Wallet либо MPC: authenticated log, скрытые payload и публичные t_exec | Последовательно применяет точный In/Out: In>0 создаёт cohort в t_exec; Out переносит исходное acquired_at проданных slices и ставит sold_at=t_exec; нулевая операция не создаёт cohort/qualified_start | Лог однозначно задаёт cohorts. Closed snapshot commitment при необходимости строит wallet/MPC с proof fold этого журнала; это уже известные времена |
| T4. READY | MPC и validators: frozen log root/version, cutoff block/order, evaluation timestamp, global Fidelity context | Включает все принятые до cutoff операции, в том числе Credis void. Доказывает fold и league либо выдаёт отдельно принятый threshold certificate | Только разрешённый league/snapshot result; input root и evaluation time входят в statement |
| T5. Возврат владельца | Кошелёк: canonical log и recoverable payloads | Сверяет восстановленные openings с C_delta, применяет t_exec, проверяет snapshot roots | Актуальный локальный witness. Хранить только seed или старый balance opening недостаточно |

`C_delta` обязан скрывать размеры и amount-dependent LIFO selection/splits. В публичном log не появляются исчерпанные cohort indices, число проданных slices или secret zero guards. Их проверяют внутри wallet proof/MPC; полное replay/fold проверяется по аутентифицированному журналу. Добавление публичного t_exec к одному непрозрачному payload **не означает** разрешение раскрыть внутреннюю структуру cohorts. Оптимизация §7.2 требует отдельного допустимого padded/public-time формата; она не следует автоматически из журнала.

При конфликте version кошелёк обновляет witness и строит новый proof; блок не подставляет другую историю в старый proof. For forced Out producer — выбранный закрытый исполнитель с shares/authority из R16; owner не вызывается. Данные перехода должны быть доставляемы владельцу: например, проверяемое шифрование payload под зарегистрированный recovery key либо приватное восстановление committee→owner с проверкой commitment. Простого обещания «зашифруем backup» недостаточно: требуется связь encrypted payload/shares с C_delta и durable receipt до finality.

T2a обязателен, если сохраняются текущие failure semantics: [apply_cohort_section](../../bin/outbe-tee-enclave/src/fidelity.rs#L236) вызывает `state.evaluate(timestamp)` **до записи** нового blob, а [RcfiAccumulator](../../crates/core/fidelity-math/src/lib.rs#L76) может отвергнуть checked intermediate, например A×10^18. Нельзя сначала зачислить деньги в T2b и обнаружить эту ошибку только на READY. Evidence привязывается к конкретному candidate block/execution context; при другом timestamp нужен повтор T2a либо отсутствие final transition. Правила получения/удержания такого контекста, abort/retry и стоимость на critical path ещё требуют реализации и liveness analysis. Одна запись t_exec в журнал этой проблемы не решает. Альтернатива — доказать все guards для допустимого интервала времени либо явно изменить overflow semantics; ни одна альтернатива здесь не считается автоматически выбранной.

Сохраняются guards и overflow semantics исходного Fidelity. Текущий defensive clamp Out не заменяет proof денежного списания: в целевой корректной истории размер burn уже обеспечен balance/cohort conservation; начальные/imported несогласованные состояния требуют отдельной migration policy. `qualified_start` устанавливается на первой положительной acquisition, а не на первой записи журнала. Производство global first-qualified metadata также должно следовать этому правилу.

Полный hidden-time вариант может завершать state через MPC после включения; logical/proposed time изменил бы правила и здесь не принят. Для предложенного log ещё требуются конкретные payload encoding, ZK/VSS/encryption binding, bounded replay/checkpoint format и измерение стоимости. Исправлен producer/consumer времени; feasibility этих компонентов не объявлена доказанной.

### 7.5. Точное чтение Fidelity владельцем

Публичная league не заменяет текущие owner-authorized `query_index_at/now`. Предложенный основной интерфейс — точный локальный расчёт кошелька по проверенной восстановленной истории, выбранному query timestamp и аутентифицированному global context. Его trace, пределы исторического чтения и вариант private-output сетевого API — [R18](DOWNSTREAM_PRIVATE_STATE_TRACE.md#r18). Результат RCFI не публикуется автоматически. Без recovery после внешних writers локальное чтение не считается реализуемым.

## 8. Библиотеки: что использовать и для какой части

| Компонент | Язык / лицензия просмотренной поставки | Применение | Ограничение и приоритет |
|---|---|---|---|
| `ark-groth16`, `ark-r1cs-std`, `ark-ed-on-bn254` | Rust; MIT или Apache-2.0 | P_link A, claim/withdraw circuits, group/field primitives | Первый baseline; локальная база 0.5, актуальные snapshots отдельно зафиксированы. Upstream README предупреждает о research/prototype status; собственного protocol audit нет[^1][^2] |
| `docknetwork/crypto/legogroth16` 0.18.0 | Rust; MIT/Apache-2.0; arkworks 0.4 | P_link B и последующие proofs с общим внешним commitment | Второй baseline; расширенный CRS и специальный verifier; нет подтверждённого аудита нашей композиции[^5] |
| `gnark` 0.16.3 | Go; Apache-2.0 | Независимая реализация того же circuit, Groth16/PLONK; полезна для сравнения RAM/скорости | Есть опубликованные audits, но важен их scope. Нельзя переносить custom Groth16 commitment extension в денежный интерфейс без проверки hiding и binding[^18][^19] |
| Circom + `rapidsnark` | Circuit DSL; prover C++/ASM, LGPL-3.0 | Native mobile/desktop P_link A после перевода circuit | Поддерживаются Android/iOS/macOS ARM paths; RAM 512 МБ на данном statement не подтверждена. Witness generator и setup toolchain учитывать отдельно[^3] |
| `snarkjs` | JS/WASM; GPL-3 | Browser prover, ceremony tooling, reference verification | Наличие WASM и single-thread режима не гарантирует требуемую память. Не принимать JSON proof size за compressed wire size[^10] |
| Zcash `halo2` | Rust; MIT/Apache-2.0 | Альтернатива при требовании IPA/no circuit-specific trusted setup | Иной field/circuit stack; source Poseidon/commitment linkage требуют переноса. Не смешивать с KZG forks под тем же именем[^20] |
| Noir + Barretenberg | Noir compiler MIT/Apache-2.0; backend требует отдельной фиксации версии/лицензии | Удобный frontend и возможная согласованность с source tooling | Альтернатива, не доказанный low-memory вариант; profiling считает gates, но не заменяет cold process RSS[^21] |
| `MP-SPDZ` | Python compiler/C++ runtime; BSD-3-Clause и указанные third-party условия | Exact Fidelity MPC, comparisons, проверка альтернативных security models | Feasibility framework; malicious protocol, input binding, persistence/rotation и externally verifiable output требуют отдельной реализации[^13][^14] |
| Dock `secret_sharing_and_dkg` | Rust; Apache-2.0 | Pedersen VSS/DVSS primitives для baseline; совместимая исходная экосистема с LegoGroth16 | Есть также PVSS/DKG, но каждый вариант имеет свой statement; durable proactive monetary service не предоставлен[^26] |
| `divviup/libprio-rs`, `divviup/janus` | Rust; MPL-2.0 | VDAF validation/aggregation и DAP operational reference | Реальные библиотеки, но versions/drafts и late grouping/rotation существенно отличаются от Outbe; подробнее в приложении[^25] |
| `TaceoLabs/co-snarks` | Rust; преимущественно MIT/Apache-2.0, `co-circom` и compiler GPL-3.0 | Distributed witness generation/proving для Fidelity | Experimental/un-audited; соответствие malicious/mobile requirements не установлено[^16][^17] |
| `CHURP`, packed DPSS references | CHURP reference — Go; DPSS — paper/construction | Проектирование monetary state handoff | Не готовая долговременная private database. Условия протокола и лицензии reference code проверять для конкретного adoption[^7] |
| Tiresias | Rust reference; license/revision gap обозначен в приложении | Сравнение threshold Paillier против VSS | Static adversary и крупные ciphertext; GitHub API на дату проверки возвращает 404, доступность воспроизводимой поставки не подтверждена[^8] |
| Dalek `bulletproofs` | Rust; MIT | Range-proof компонент, отдельные confidential-ledger варианты | Range proof не подтверждает source/economics; R1CS API помечен экспериментальным. Не основной P_link backend[^22] |

**Материальное обновление по gnark:** на дату проверки latest release — 0.16.3 от 24 августа 2026 года. Advisory `GHSA-3mvx-pp85-pm65` касается soundness нескольких std gadgets и указывает исправление с 0.16.2. Более ранний `GHSA-9xcg-3q8v-7fq6` касался нарушения zero knowledge в commitment extension до 0.10 включительно, исправление — 0.11. Это не основание отвергать актуальный gnark, но основание не брать старые benchmark versions и не считать всякий API с названием commitment скрывающим.[^18][^19]

Рекомендация по integration boundary: protocol structs, canonical encodings, integer reference math и accepted state machine оставить независимыми от backend. Prover/verifier adapters должны выдавать одинаковый statement digest и явный suite/version. Совпадение названия Groth16 не гарантирует совпадение сериализации, point checks и расширений между arkworks, gnark, snarkjs и LegoGroth16.

## 9. Что не решает весь маршрут

| Семейство | Решает | Не решает без дополнений | Решение |
|---|---|---|---|
| Только Pedersen | Hiding/binding и публичные линейные операции над commitments | Получение числового S без openings; offline Fidelity | Недостаточно |
| Только range proof / Bulletproofs | Допустимый диапазон скрытого числа | Источник, exact nominal formula, доступность агрегата и поздние группы | Компонент, не полный протокол |
| Threshold ElGamal в показателе, одна точка на полную сумму | Аддитивную агрегацию ciphertext | Практическое извлечение 134-битной числовой суммы через generic DLog | Не подходит для single-point recovery; chunked вариант рассматривается отдельно ниже |
| Prio3 / DAP / Flamingo / Lighthouse | Некоторые модели private input validation и secure aggregation | Готовую связь P_L2→nominal, late leagues и динамический monetary ledger | Архитектурные источники; adaptation существенна[^25] |
| Polynomial/vector commitments | Компактную аутентификацию вектора и openings | Само наличие закрытых значений у offline исполнителя | Возможная оптимизация DA/proofs, не замена shares |
| SnarkPack / proof aggregation | Сжатие множества proofs и часть verification work | Агрегацию скрытых **денег**, DA, public-input processing, uniqueness | Поздняя оптимизация Q7[^23] |
| zkVM / STARK / recursive execution | Универсальное доказательство исполнения | Подтверждённый cold wallet run ≤512 МБ и distributed secret storage | Не основной кандидат по имеющимся данным; не объявляется принципиально невозможным |
| Time-lock encryption | Открытие после времени при определённой модели | Aggregate-only opening: individual ciphertext может раскрыть индивидуальную сумму | Не отвечает этому интерфейсу |
| Новый DKG | Новый распределённый ключ | Восстановление потерянного старого monetary state | Недостаточно |

При оценке zkVM найден официальный RISC Zero datasheet, но он относится к **апрелю 2023 года**. Его RAM/proof-size цифры исключены из сравнения актуальных библиотек и из оценки P_link: обновлённая версия, другое число cycles и recursion path меняют результат. Обоснованного актуального утверждения «любой zkVM обязательно превышает 512 МБ» здесь нет.[^24]

Уточнение после проверки Aptos/XELIS 2026-09-11: короткие chunks меняют оценку DLog и являются отдельным кандидатом. Для нашего canonical профиля сумма одного 16-bit разряда на миллиард входов меньше 2^46. Однако открытие ненормализованных сумм разрядов раскрывает больше S/S_l; private normalization/recovery и полный uint256 conservation protocol ещё не разработаны. Twisted ElGamal с range/Σ proofs также применим к приватному Gratis независимо от решения дневного агрегирования. Источники, размеры, ограничения и фактический стек текущего PoC — [новая проверка](poc/TWISTED_ELGAMAL_APPLICABILITY.md).

SEAL не входит в shortlist по зафиксированному требованию. Возвращать полный homomorphic nonlinear Lysis только ради уже разрешённых публичных S/S_l нет оснований.

## 10. Масштаб: размеры, время и параллельность

### 10.1. Полный Tribute и распределённые данные

Ниже арифметика **конкретного простого VSS формата** с 32 B на scalar/point. Это не универсальная нижняя граница всех протоколов. `meta` включает все реально передаваемые public context/input bytes, а не только заголовок; значения, восстановимые из chain state, можно не дублировать в wire payload. `P_L2` и receipts считаются отдельно.

```text
Tribute_A = meta + bytes(P_L2) + 128 + 32*t + receipt_evidence
Tribute_B = meta + bytes(P_L2) + 224 + 32*(t-1) + receipt_evidence
private upload per Tribute = 64*n + transport/authentication overhead
private nominal state per holder = 64*N + indexes/WAL/checkpoints
```

В A `32*t` включает `C(a)=A_0` и остальные polynomial commitments. В B C(a) уже входит в 224 B ProofWithLink; остаются t−1 дополнительных coefficient commitments.

| Составляющая, N=`10^9` | Raw объём |
|---|---:|
| Только C(a), 32 B | 32 GB |
| Groth16 proof, 128 B | 128 GB |
| Дополнительные VSS coefficient commitments при t=6 | 160 GB |
| A: proof + все VSS commitments при t=6 | 320 GB, без P_L2/meta/receipts |
| B: ProofWithLink + дополнительные VSS commitments при t=6 | 384 GB, без P_L2/meta/receipts |
| Share pairs у одного держателя | 64 GB |
| Share pairs у 16 держателей, сумма по сети | 1.024 TB |
| Paillier ciphertext при 3072-bit modulus | 768 GB, без доказательств и metadata |

32 B scalar container не означает, что share можно хранить как 13-байтовый nominal: shares распределены по полю. Packed sharing может изменить соотношение секретов и shares; это другая строка модели после конкретизации параметров, не бесплатное сжатие таблицы.

На узле отдельно измеряются actual DB size, key/index overhead, WAL, двойной checkpoint при handoff, replication, incomplete batches и overlapping days. Public transcript может быть вынесен из hot consensus state в DA, но тогда должна существовать подтверждённая availability и возможность проверки при replay/bootstrap. Публичные proofs можно прунить лишь по утверждённой self-contained checkpoint policy.

### 10.2. Размер Nod

У нового Nod нет обязательного отдельного proof суммы при выпуске: descriptor содержит принятый C(a) и сертифицированные public terms. Пример **ещё не утверждённого** фиксированного descriptor:

| Поле | B |
|---|---:|
| Source Tribute id | 32 |
| C(a) | 32 |
| Owner address | 20 |
| Day | 8 |
| Fraction integer | 32 |
| Bucket id | 32 |
| Terms/certified context root | 32 |
| Formula version | 2 |
| **Всего без framing/indexes/отдельного id** | **190** |

Это 190 GB для миллиарда активных descriptors. Terms root должен однозначно ссылаться на доступные immutable условия; иначе компактность получена потерей нужных данных. При бинарном Merkle tree на миллиард leaves одиночный путь — около `30*32=960 B`; он передаётся при чтении/погашении и не обязан храниться отдельной копией рядом с каждым leaf. Реальный wire format, compact encoding и multiproofs надо измерить.

### 10.3. Часовая ротация может стоить дороже однократного приёма

Для условного дня с равномерным поступлением N записей за 50 часов и 12 часами дальнейшего хранения, при hourly full-record handoff суммарное число перемещаемых live records примерно:

```text
N*(1+2+...+49)/50 + 12*N = 36.5*N
```

При 64 B одной пары это **2.336 TB перемещаемого raw share state на последовательность держателя** для одного дня, до криптографического расширения resharing, сети между всеми участниками и служебных данных. Конкретное расположение переходов на границах часов меняет число на одну-две полные партии. Это иллюстрация модели, не measured network traffic и не lower bound для packed DPSS.

Поэтому выбор только по размеру admission proof ошибочен. Нужно сравнить:

- перенос individual VSS state;
- packed proactive redistribution;
- key-share refresh с сохранением encrypted data;
- state transformation в residual groups после Lysis.

Отделение storage committee от текущего consensus validator set или удержание старых валидаторов на время handoff может снизить churn, но меняет обязанность участия и trust/liveness policy. Это не уже принятое архитектурное условие.

### 10.4. Скорость Tribute и Lysis

Для одного изолированного окна `10^9/(50*3600)=5555.56 admissions/s`. Если каждый календарный день создаёт собственный миллиард, steady-state средняя нагрузка — `10^9/86400=11574.07 admissions/s`, поскольку offering windows перекрываются.

Системная пропускная способность ограничивается минимумом нескольких стадий:

```text
min(wallet proving capacity,
    P_L2 + P_link verification,
    per-recipient share checking and durable writes,
    consensus/DA bandwidth,
    rotation/repair capacity)
```

Пользовательские proofs **можно готовить параллельно**: независимым владельцам не требуется ждать друг друга. Это не означает линейное ускорение admission — остаются SU/owner uniqueness, consensus, storage и handoff. У одного приватного account два claims к одной старой версии balance конфликтуют; они требуют сериализации либо одного доказанного batch transition. Несколько prover threads также повышают peak RAM одного кошелька.

Shard Lysis — **256 Tribute records**, не 256 блоков. Для миллиарда это 3 906 250 tasks. Новый worker не рассчитывает g/c и не строит индивидуальный SNARK; он читает proofs/сертифицированные inputs, применяет public mapping и пишет descriptors. Поэтому нужны отдельные measurements:

```text
T_job = dependency critical path:
        input/DA readiness -> Fidelity snapshot -> grouping/openings ->
        public coefficient kernel -> descriptor shards -> final certification

descriptor throughput ≈ min(worker compute, input fetch, output writes, certification)
```

Для W одинаковых workers с measured `t256` сек на shard идеализированная compute оценка — `W*256/t256 Nod/s`. Полный job включает barriers и I/O, поэтому это не обещанная TPS. Даже 1 ms на одну проверку означает около 11.6 CPU-seconds каждой секунды при 11 574 proofs/s, прежде чем добавить P_L2 и всё остальное; это арифметический пример, не оценка нового P_link.

SnarkPack или иной batch/recursive verifier может уменьшить стоимость Q7 после измерения baseline. Он не устраняет O(N) чтение public metadata, связывание public inputs с accepted set и хранение доступных свидетелям данных.[^23]

## 11. Сопоставление с исследовательским заданием

| Пакет | Результат | Что ещё препятствует утверждению полного решения |
|---|---|---|
| Q1 source→P_link | Два конкретных native/commit-and-prove кандидата; явный statement и поля; широкая кривая не нужна current profile | Полный cold run ≤512 МБ, SU bounds, source seed/privacy, setup/security target |
| Q2 дневной S | Проверяемое открытие scalar VSS aggregate, same-C binding, no-wrap; явный переход per-record receipts → common coverage/repair | Malicious admission/repair implementation, security/liveness выбранного handoff и throughput |
| Q3 late S_l | Перегруппировка shares после snapshot; нельзя преждевременно оставить только S | Цена retention/resharing миллиарда inputs; packed input link |
| Q4 Fidelity | Exact MPC, timestamped transition log; linear A/D и 12 comparisons при допустимом формате; R16/R18 | Forced Out, recovery, hidden history/lifetime bounds, state handoff, публичная проверяемость и судьба query API |
| Q5 Nod/Gratis | Deferred exact claim, uint256 limbs; R15 payout, R16 compartments, R17 mint-source contract | Payment18, cost bounds; Intex private payout policy, offline external mutations, Promis conversion/auth и общий supply |
| Q6 forfeit | Проверяемый переход в residual groups и private debit на claim | Согласовать granularity/timing раскрытия F; доказать residual completeness/debit availability |
| Q7 verifier/DA | Versioned adapters; точные input counts и единый VK bundle; DA/coverage checkpoints; независимость proof bytes и state bytes | Полный wire schema, verification resource bounds, replay/bootstrap и certification |

**Статус всей конструкции: требует перечисленных реализаций и нескольких явных протокольных решений.** Она математически покрывает возможность суммировать скрытые Tribute и раскрывать S/S_l, но пока не подтверждена как система для миллиарда записей и 512-МБ кошелька. Ни SEAL, ни P-384-in-BN254 не требуются как обязательная часть следующего этапа.

## 12. Последовательность реализации и измерений

### Этап A — зафиксировать интерфейсы до криптографического кода

Определить suite security target; диапазоны source/cost/Fidelity и всех mint sources; wallet runtime; допуски committee corruption/dropout; payment18 policy; разрешённую публикацию forfeit и дополнительных totals; public/private time metadata. Выбрать контракты R15–R18: приватное право Intex и его asset/round lifecycle, compartment/offline mutation и recovery, Promis boundary и supply API, точное чтение Fidelity. Зафиксировать typed encodings, field-vs-integer conversions, VK registry, все state roots и atomic transitions. Оставшиеся решения не менять ради выгодного benchmark.

### Этап B — два маленьких, но полных P_link кандидата

Один и тот же canonical source/economics statement проверить на native Baby-Jubjub Groth16 и LegoGroth16 CP_link. Для обоих: реальные source fixtures, real P_L2 composition, valid proof, mutations public fields, разные commitments, invalid ranges/carries, near-modulus values, maximum supported source list. Для Lego — adapter-owned independent internal v и external opening, неизменный зарегистрированный VK bundle и точный layout public inputs. В выводах разделять security assumptions, setup, cold load, proving, serialization и peak.

**Gate:** свежий процесс на целевом runtime укладывается в 512 000 000 B, полный proof проверен и связан с тем же commitment для VSS. Отдельные source/commitment proofs без проверенной композиции gate не проходят. Ограничитель RAM прекращает experiment, а не позволяет ему разрастись до десятков GB.

### Этап C — сквозной денежный state lifecycle

Реализовать accepted/pending, malicious share checks, burn/reorg, final S, минимум две ротации, поздний league map, S_l, создание residual groups, offline owner, последующий claim с fresh debit sharing и forfeit. Реальные разные inputs, нулевые/ошибочные/duplicate записи, crash после ACK и в середине handoff. Проверять все roots и conservation, а не только сумму повторённого fixture.

**Gate:** корректность и availability при заданном `n,t,f,dropouts`, отсутствие лишних открытий, установленный момент удаления каждого класса данных. После этого сравнивать simple VSS с packed DPSS и threshold Paillier по end-to-end storage+rotation стоимости.

### Этап D — Fidelity и приватный платёж

Воспроизвести текущие transition/league значения на representative histories. Измерить полный скрытый вариант и, если формат разрешён, public-time comparison variant. Для результата выбрать публичный proof либо явно принятый MPC certificate model. Проверить диапазоны, LIFO, hidden state sizes, clock binding, owner offline и переход между эпохами. Отдельно проверить exact payment/change18 и отсутствие раскрытия c/g через PayNote/events/supply. Обязательные сценарии: owner offline → Credis void → READY; third-party settlement → recoverable account; inclusion T+3; Intex round и hidden floor/remainder; Promis conversion → Fidelity In; точный owner RCFI query после внешнего изменения.

**Gate:** один законченный `Nod→payment→Gratis→Fidelity→COEN`, вместе с изменением residual state, без TEE и без зависимости от online owner для будущего snapshot.

### Этап E — масштабирование и итоговая таблица SLA

Результат каждого нагрузочного запуска должен заполнять одинаковые колонки:

| Измерение | Что включать |
|---|---|
| Client | Cold/warm latency p50/p95, peak RAM, witness/PK sizes, runtime/device, parallelism |
| Admission | P_L2/P_link verification, share verification, ACK wait, durable write, accepted/s, rejects |
| Data | Полный encoded Tribute; public DA; private fanout; DB/WAL; replays и replicas |
| Rotation | Live record count, old/new n/t/f, bytes, CPU, wall time, overlap peak, incomplete batches |
| Lysis | Fidelity, grouping, aggregate opening, public kernel, 256-task latency, I/O, full job wall time |
| Nod | Encoded descriptor, issuance/s, membership data, active rights and residual groups |
| Claim/COEN | Full proof+payment+Fidelity+residual transition, conflicts, fees, private state recovery |
| Long term | Несколько перекрывающихся дней, surviving Nod groups, cohort growth, pruning/repair |

Переход от 10⁴ к 10⁶/10⁷ записям сначала проверяет модель роста. Экстраполяция к 10⁹ должна быть подписана как модель с measured coefficients; фактический billion-record test называется так только при миллиарде уникальных inputs. Значения из papers и upstream README не заполняют колонку измерений Outbe.

## Источники

Нумерация соответствует ссылкам по тексту. Для реализаций дата просмотра — 11 сентября 2026 года; точные Git snapshots основных библиотек — в [реестре](DEEP_RESEARCH_LIBRARY_PINS.json). Публикации подтверждают свойства конструкций в своих моделях; адаптации к R00–R18 и расчёты объёмов являются выводами этого отчёта.

[^1]: Jens Groth. [On the Size of Pairing-based Non-interactive Arguments](https://eprint.iacr.org/2016/260), EUROCRYPT 2016. Arkworks: [Groth16 README](https://github.com/arkworks-rs/groth16), [Proof/ProvingKey structures](https://github.com/arkworks-rs/groth16/blob/8f0904a7d7a2c8945bf770bdd3c2081e0be1941a/src/data_structures.rs), [algebra README/security notice](https://github.com/arkworks-rs/algebra).
[^2]: Arkworks: [Baby-Jubjub field/model](https://github.com/arkworks-rs/algebra/blob/4d708c733d9340dedf3b3a8b41718d3370d8cf46/curves/ed_on_bn254/src/lib.rs), [curve/subgroup/generator](https://github.com/arkworks-rs/algebra/blob/4d708c733d9340dedf3b3a8b41718d3370d8cf46/curves/ed_on_bn254/src/curves/mod.rs), [scalar modulus](https://github.com/arkworks-rs/algebra/blob/4d708c733d9340dedf3b3a8b41718d3370d8cf46/curves/ed_on_bn254/src/fields/fr.rs), [r1cs-std](https://github.com/arkworks-rs/r1cs-std). Старый `arkworks-rs/curves` перенесён в algebra; ссылаться на него как на активно развиваемый отдельный repository неверно.
[^3]: Iden3. [Rapidsnark README: native/mobile build, wrappers, LGPL-3.0](https://github.com/iden3/rapidsnark/blob/81eddf1a536d26497b237c0b8a04fe90baf7e439/README.md).
[^4]: Campanelli et al. [LegoSNARK: Modular Design and Composition of Succinct Zero-Knowledge Proofs](https://eprint.iacr.org/2019/142), 2019. Commit-and-prove framework и LegoGro16; опубликованные speedups не используются как прогноз нашего circuit.
[^5]: Dock. [LegoGroth16 README](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/README.md), [generator](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/generator.rs), [prover](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/prover.rs), [proof structures](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/data_structures.rs), [Cargo version/dependencies](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/Cargo.toml).
[^6]: Torben P. Pedersen. [Non-Interactive and Information-Theoretic Secure Verifiable Secret Sharing](https://cgi.di.uoa.gr/~aggelos/crypto/page8/assets/Pedersen-VSS.PDF), 1991. Transcript, privacy и обязательства адаптации также разобраны в [приложении Q2/Q3/Q6](DEEP_RESEARCH_AGGREGATION.md).
[^7]: Maram et al. [CHURP: Dynamic-Committee Proactive Secret Sharing](https://eprint.iacr.org/2019/017), CCS 2019; Baron et al. [Communication-Optimal Proactive Secret Sharing for Dynamic Groups](https://eprint.iacr.org/2015/304), 2015. Разные конструкции и конкретные adversary/committee assumptions.
[^8]: Friedman et al. [Tiresias: Large Scale, Maliciously Secure Threshold Paillier](https://eprint.iacr.org/2023/998), 2023, в частности §1.2 и DKG sections; [reference implementation](https://github.com/dwallet-labs/tiresias). Damgård, Jurik, Nielsen: [обобщение Paillier](https://people.csail.mit.edu/rivest/voting/papers/DamgardJurikNielsen-AGeneralizationOfPailliersPublicKeySystemWithApplicationsToElectronicVoting.pdf).
[^9]: Sakemi et al. [Pairing-Friendly Curves, draft-irtf-cfrg-pairing-friendly-curves-14](https://www.ietf.org/ietf-ftp/internet-drafts/draft-irtf-cfrg-pairing-friendly-curves-14.html), сентябрь 2026. Internet-Draft, не финальный стандарт; security classification BN254 и альтернативных pairing curves.
[^10]: Iden3. [snarkjs README](https://github.com/iden3/snarkjs/blob/9a8f1c0083d18b9b5e18f526cfd729e7259423be/README.md): Groth16/PLONK/FFLONK, ceremonies, WASM/threading, witness memory option, GPL-3.
[^11]: Hopwood et al. [Zcash Protocol Specification](https://zips.z.cash/protocol/protocol.pdf), версия v2026.7.0-187-ge753a6, 3 сентября 2026; §§3.2–3.9, 4: commitments, nullifiers, shielded state и conservation. Пример модели, не готовая Outbe реализация.
[^12]: Barnes, Bhargavan, Lipp, Wood. [RFC 9180: Hybrid Public Key Encryption](https://datatracker.ietf.org/doc/html/rfc9180), февраль 2022, особенно §9.7: replay/forward-secrecy non-goals и application embedding.
[^13]: Marcel Keller / MP-SPDZ. [Getting Started: protocol matrix и security notice](https://mp-spdz.readthedocs.io/en/latest/readme.html), [implementation](https://github.com/data61/MP-SPDZ), [лицензия с third-party условиями](https://github.com/data61/MP-SPDZ/blob/892ac0e2a2a9edabbe0249febc0b316ca649b479/License.txt).
[^14]: MP-SPDZ. [High-Level Interface](https://mp-spdz.readthedocs.io/en/latest/Compiler.html), [types implementation](https://mp-spdz.readthedocs.io/en/latest/_modules/Compiler/types.html): integer/comparison/division domains. Ширина и algebraic threshold optimization в отчёте выведены из локального [Fidelity math](../../crates/core/fidelity-math/src/lib.rs).
[^15]: Alex Ozdemir, Dan Boneh. [Experimenting with Collaborative zk-SNARKs: Zero-Knowledge Proofs for Distributed Secrets](https://www.usenix.org/conference/usenixsecurity22/presentation/ozdemir), USENIX Security 2022.
[^16]: TACEO. [co-snarks README](https://github.com/TaceoLabs/co-snarks/blob/23217ff78fc52f806420fd6d5c27563bea9c74bd/README.md): composition, backends, лицензии и experimental/un-audited disclaimer.
[^17]: Garg, Goel, Jain, Roberts, Sekar. [Malicious Security in Collaborative zk-SNARKs: More than Meets the Eye](https://eprint.iacr.org/2025/1026), CRYPTO 2025.
[^18]: Consensys. [gnark README и аудитные scopes](https://github.com/Consensys-Incorporated/gnark/blob/fd5c2443d59970eb1c3e4202fb8f10a23ef60632/README.md), [release v0.16.3](https://github.com/Consensys-Incorporated/gnark/releases/tag/v0.16.3), 24 августа 2026.
[^19]: Consensys. [GHSA-3mvx-pp85-pm65](https://github.com/Consensys-Incorporated/gnark/security/advisories/GHSA-3mvx-pp85-pm65), 24 августа 2026; [GHSA-9xcg-3q8v-7fq6](https://github.com/Consensys-Incorporated/gnark/security/advisories/GHSA-9xcg-3q8v-7fq6), 6 сентября 2024. Это разные defects и разные исправленные версии.
[^20]: Zcash. [Halo2 proving system](https://zcash.github.io/halo2/design/proving-system.html), [curve/encoding background](https://zcash.github.io/halo2/background/curves.html), [implementation/license](https://github.com/zcash/halo2).
[^21]: Noir. [Repository/лицензия](https://github.com/noir-lang/noir), [Profiler documentation](https://noir-lang.org/docs/tooling/profiler). Frontend/backend versions нужно фиксировать совместно.
[^22]: Dalek. [Bulletproofs README и экспериментальный R1CS API](https://github.com/dalek-cryptography/bulletproofs/blob/be67b6d5f5ad1c1f54d5511b52e6d645a1313d07/README.md), [MIT license](https://github.com/dalek-cryptography/bulletproofs/blob/be67b6d5f5ad1c1f54d5511b52e6d645a1313d07/LICENSE.txt).
[^23]: Gailly, Maller, Nitulescu. [SnarkPack: Practical SNARK Aggregation](https://eprint.iacr.org/2021/529), 2021. Aggregation proofs и их verification, не получение денежного S.
[^24]: RISC Zero. [Performance Datasheet](https://dev.risczero.com/datasheet.pdf), **апрель 2023**, commit cd1a37e. Источник просмотрен, численные результаты исключены из оценки актуального P_link.
[^25]: [VDAF](https://datatracker.ietf.org/doc/draft-irtf-cfrg-vdaf/), [DAP](https://datatracker.ietf.org/doc/draft-ietf-ppm-dap/); Ma et al. [Flamingo](https://arxiv.org/html/2308.09883v1); Garg et al. [Lighthouse](https://www.usenix.org/system/files/usenixsecurity26-garg-sanjam.pdf). Детальные версии, implementation references и границы применения — в [приложении](DEEP_RESEARCH_AGGREGATION.md).
[^26]: Dock. [Secret sharing and DKG README](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/secret_sharing_and_dkg/README.md), [license](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/LICENSE).
[^27]: Dock, тот же pinned commit. [verifier.rs](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/verifier.rs): `prepare_inputs`, `verify_proof_incl_cp_link`; [generator.rs](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/generator.rs): `n=num_instance_variables+commit_witness_count`; [data_structures.rs](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/data_structures.rs): VK bundle и `num_public_inputs`. Правила adapter выведены из этих API; cryptographic negative tests нового adapter ещё не запускались.
