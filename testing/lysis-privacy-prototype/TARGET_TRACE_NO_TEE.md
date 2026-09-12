# Целевой trace без TEE: Tribute → Nod → скрытый Gratis → COEN

> Новая исходная схема и подтверждённые ограничения: [PROTOCOL_TRACE_AND_REQUIREMENTS.md](PROTOCOL_TRACE_AND_REQUIREMENTS.md). Выбор криптографии отложен; приведённые здесь конструкции/замеры — предшествующие варианты. Для текущего source codec новый trace выводит более тесные денежные диапазоны и учитывает Fidelity/forfeit после Lysis.

Проверено 2026-09-11 в рабочем дереве `177a72ddbea9f2e52eef094405481292ecd56046`. Это проект перехода и результаты локального прототипа; production-код не изменён. Этот документ заменяет прежний вариант, где Lysis заранее вычислял индивидуальные `gratis_load/cost`.

**Цель:** пользователь доказывает правильность своего Tribute, сеть получает итоговый nominal дня без публикации индивидуальных nominal, Lysis фиксирует коэффициенты в Nod, пользователь позже доказывает начисление в скрытый Gratis. Публичная индивидуальная сумма появляется при Gratis → COEN.

**Рабочий вариант дальнейшего исследования — Pedersen commitments + ZK-доказательства пользователя + проверяемое секретное распределение (VSS) для агрегатов. SEAL исключён из дальнейшего рассмотрения в этой схеме по решению пользователя.** Его прежние измерения сохранены в разделе 8 как архив. Для T03 теперь проверен отдельный компонент P-384 VSS: один полный scalar позволяет открыть одну денежную сумму S без limb totals. Добавлен [P_link](measurements/p-link/README.md): canonical draft hash → точный nominal → тот же P-384 commitment. Production admission и проверка двух proofs вместе ещё требуют интеграции.

**Кто, что и до какого момента хранит; кто принимает доли, переносит их и открывает S:** [контракт хранения и агрегирования](STORAGE_AND_AGGREGATE_PROTOCOL.md). [Исследование P-384/VSS](WIDE_FIELD_VSS_RESEARCH.md) и [новые компонентные замеры](measurements/wide-vss/README.md) дополняют trace. Все прежние Ristretto размеры ниже явно относятся к предыдущему варианту.

## 0. Уточнённая точность: входы 10⁶, результаты 10¹⁸

**Пользователь подтвердил строгий масштаб входов 10⁶:** nominal a₆, коэффициент f₆ и entry price p₆. В рассматриваемом варианте Gratis load, cost, приватный баланс и денежные лимиты сохраняют точность 10¹⁸. Это изменение целевого формата, не выполненная миграция production.

~~~text
M = 10^6
g18 = a6 * f6 * M
c18 = a6 * f6 * p6
balance_new18 = balance_old18 + g18

G18 = Σ_l (S_l6 * f_l6 * M) = Σ_i g_i18
unused18 = B18 - G18
~~~

**Округления при получении g/c больше нет.** Произведение nominal и коэффициента имеет максимум 12 десятичных знаков, после цены — максимум 18. Например a₆=f₆=p₆=1 даёт g=10⁻¹² и cost=10⁻¹⁸: оба результата точны.

**Исчезает прежняя проблема суммы individual floors:** при известных S_l6 и окончательных f_l6 Lysis точно вычисляет G18 и сразу возвращает B18−G18. Для этого не нужны VSS пользовательских g и ожидание claims. Непогашенные права остаются обеспечены выделенным G18; возможный возврат за истечение прав — отдельная политика.

Прежние измерения claim/withdrawal ниже относятся к варианту с промежуточным floor и балансом 10⁶. **Это не замеры нового exact-варианта.** Commitment/VSS компоненты остаются полезными ориентирами; новый proof пока не измерен.

Диапазон nominal uint256 и связь нового P-384 commitment с входом реализованы в отдельном P_link; его production integration и поздние лиги T05 остаются открытыми. При сохранении старого limb proof нужны также переносы/связь представлений. Деления при получении исходного nominal и публичных коэффициентов сохраняются там, где заданы формулами. Промежуточные произведения должны быть достаточно широкими; конечные значения проверяются на переполнение.

## 1. Что уже выполнено и что не следует считать готовым

Запущен Rust-прототип с настоящими Pedersen commitments, Bulletproofs R1CS proofs, проверкой VSS-долей, передачей агрегата новому комитету и последующим начислением/списанием скрытого баланса. Проверены отрицательные сценарии. Отдельно запущен Microsoft SEAL 4.4.0 BFV.

Это **арифметическая цепочка**, не интеграционный прогон блокчейна. В измерениях нет доказательства подлинности банковского offer, приватной оплаты PayNote, Fidelity, сетевого согласования комитетов, OCOMP, диска, DA и консенсуса. R1CS backend использует экспериментальный `yoloproofs`; наши арифметические gadgets не проходили криптографический аудит.

Исходники, команды воспроизведения, необработанные JSON: [measurements/README.md](measurements/README.md).

Дополнительно выполнен [P-384 VSS component](measurements/wide-vss/README.md): создание/проверка долей, накопление, reshare и открытие одной S. Он не включает source/range/admission ZK, сетевые receipts и новый exact claim. Теперь добавлен [P_link](measurements/p-link/README.md), доказывающий source hash/range/arithmetic для того же C(a). Проверка существующего P_L2, сетевой admission и последующий exact claim ещё не собраны в один end-to-end протокол.

## 2. Обозначения и граница видимости

- `M = 10^6`; issuance u_i, nominal a_i, коэффициенты f_l, цены p_i, агрегаты S и S_l имеют масштаб 10⁶. Gratis g_i, cost c_i, баланс b и денежные бюджеты/лимиты B,R,E,K,A,D,Q в целевом варианте имеют масштаб 10¹⁸. Суффиксы 6/18 ниже обозначают raw integers.
- `S = Σa_i`; `S_l = Σ(a_i в лиге l)`; `n_l` — число Tribute лиги.
- `B` — бюджет Lysis; `f_l` — окончательный коэффициент лиги; `p_i` — entry price.
- `C(x)` — публичный commitment; `r_x` — его секретное открытие вместе с x.
- `[x]_j` — секретная доля у участника j. Она не является ни x, ни ключом ко всем Tribute.
- В прежнем измеренном Ristretto-варианте `C(uint256)` — четыре commitments к 64-битным частям, **128 байт**, а не одна 32-байтная точка. Значение uint256 не помещается целиком в его скалярное поле. В новом компоненте P-384 весь uint256 занимает один scalar, commitment — **49 байт**; для него добавлен отдельный P_link, требующий production integration с P_L2 и VSS admission.

Публичные owner/ID/время/валюта/лига здесь сохраняются. Скрываем суммы; анонимность владельцев этим проектом не заявляется. Источник, которому пользователь изначально сообщил сумму, не «забывает» её из-за commitment. Владелец хранит собственные суммы и randomness, чтобы позже строить proofs; validators получают только доли.

## 2.1. Исполнители: кто именно считает

**Пользователь** ниже — его локальный кошелёк/prover. **Валидатор** проверяет публичные транзакции и переходы состояния. **Участник VSS-комитета** — валидатор с собственной секретной долей; необязательно каждый валидатор сети входит в комитет. **Lysis worker** исполняет расчёт/формирует результаты. **Nod** — объект права, он сам ничего не вычисляет.

Это распределение обязанностей **целевого протокола**. Оно не означает, что все перечисленные proofs уже реализованы.

| Шаг | Что считает/готовит пользователь | Что считает нода и какая именно | Что проверяет/сохраняет сеть |
|---|---|---|---|
| T00: offer → Tribute | Вычисляет `a_i` из своего `u_i` и публичных цен; создаёт commitments и proof источника/арифметики | Валидатор исполняет verifier, не вычисляя plaintext nominal | Подлинность входа, формулу, диапазоны, owner/day, уникальность; сохраняет commitments |
| T01: VSS и приём | Создаёт полиномы shares для `a_i,r_a`, commitments их коэффициентов; отправляет каждому участнику его доли | Каждый член комитета проверяет свои доли и прибавляет их к своему закрытому накопителю. Любая нода может сложить публичные commitments | Согласованный accepted set, proof, связь VSS с тем же C(a), достаточную доступность долей |
| T02: ротация | Не участвует | Старые участники создают fresh sharing своего взвешенного вклада для всего retention manifest; новые проверяют и суммируют доли | Те же commitments и checkpoint, включая индивидуальные записи поздних лиг |
| T03: открытие дня | Не участвует | t участников публикуют суммарные пары; любая нода проверяет их и интерполирует S,R. До открытия одна доля S не раскрывает | Каждый вклад, итоговый commitment, финальный набор и целочисленные границы |
| T04: бюджет | Не участвует | Ноды, исполняющие Metadosis, вычисляют D, Q, B, A из публичного S и состояния | Проверяют детерминированную арифметику и сохраняют параметры дня |
| T05: суммы по лигам | Для самого сложения nominal не нужен; путь получения Fidelity-лиги без TEE ещё требует реализации | Комитет складывает сохранённые shares по выбранным лигам; публичные commitments группируются так же. В открытом варианте комитет открывает S_l | Snapshot/полноту группировки, ΣS_l=S и Σn_l=N; зависимость от позднего snapshot сохраняется |
| T06: коэффициенты | Не участвует | Lysis worker считает f_l, цены/параметры и R из проверенных публичных агрегатов | Проверяет/certifies результат по протоколу выполнения |
| T07: создание Nod | Не участвует; может быть offline | Worker берёт готовый C(a_i) из Tribute и добавляет коэффициенты/метаданные. Создаёт записи и roots | Проверяет покрытие принятых Tribute, ссылки на C(a_i), параметры и manifest |
| T08: возврат лимита | Не участвует; claims ещё не нужны | Lysis считает точный G18 из S_l6 и f_l6; ноды возвращают B18−G18 | Точный состав прав Nod, коэффициенты, G18≤B18 и обеспечение выделенных прав |
| T09: Nod → Gratis | Вычисляет точные g18, c18 и b_new18; создаёт commitments и proof расчёта/приватной оплаты/баланса | Валидатор проверяет proof, платёж, eligibility и nonce; VSS для g ради возврата лимита не требуется | Атомарно расходует Nod/платёж и заменяет C(balance); индивидуальные суммы не открываются |
| T10: Gratis → COEN | Выбирает открытый x, считает скрытый b_new=b_old−x; создаёт новый commitment и proof списания | Валидатор проверяет proof и выпускает публичное x18 native COEN: масштаб уже 10¹⁸ | Атомарно обновляет commitment/nonce и COEN balance |

## 2.2. Кто создаёт каждый commitment

В этом варианте кошелёк выбирает свежий blinding для новых индивидуальных денежных commitments. Ноды могут **выводить агрегатные commitments сложением** уже проверенных точек: знать суммы или их открытия для этого не требуется. Это разные действия.

После уточнения точности g18 и c18 линейны по a6 при публичных f6/p6. В представлении с достаточными диапазонами соответствующие commitments произведений можно вывести умножением C(a6) на публичный множитель. В выбранном Nod по-прежнему достаточно хранить C(a6) и коэффициенты; при claim кошелёк создаёт commitments с новыми blindings и доказывает равенства. Для четырёх 64-битных limbs умножение точек само по себе не выполняет канонизацию целого и переносы: эти проверки нельзя удалить вместе с floor.

| Commitment | Создатель / момент | Из каких данных получается | Кто знает его скрытое значение и opening |
|---|---|---|---|
| `C(u_i)` — issuance | Пользователь, T00 | Собственный issuance и свежий blinding | Пользователь |
| `C(a_i)` — nominal | Пользователь, T00 | Вычисленный nominal и свежий blinding | Пользователь; участники комитета позже получают только shares |
| `D_i[k]` — коэффициенты VSS полиномов | Пользователь, T01 | Секретные коэффициенты полиномов значения и blinding | Пользователь знает свой исходный sharing; каждый получатель знает лишь свои evaluations |
| `A_day[k]=Σ_i D_i[k]` — накопленные VSS commitments | Любая нода по принятому набору, T01 | Сложение публичных coefficient commitments | Открытия распределены по shares комитета; ноде не нужен plaintext для сложения точек |
| Aggregate commitments по лиге / eligible | Любая нода, когда определён точный набор группы | Сложение соответствующих проверенных commitments | Комитет имеет shares соответствующего агрегата; индивидуальные открытия остаются у пользователей |
| Новые коэффициенты sharing при ротации | Старые участники комитета, T02 | Каждый создаёт sharing своего взвешенного секретного вклада; новые участники проверяют его | Ни одному участнику для этого не требуется знать весь агрегат |
| Commitment nominal внутри Nod | **Новый не создаётся**, T07 | Worker сохраняет тот же `C(a_i)` или проверяемую ссылку на него | Opening по-прежнему у пользователя |
| `C(g_i)` и `C(c_i)` | Пользователь, T09 | Вычисленные по Nod load/cost и свежие blindings | Пользователь |
| `C(b_new)` при зачислении | Пользователь, T09 | Свой старый баланс + g_i, свежий blinding | Пользователь; нода проверяет proof перехода от текущего C(b_old) |
| Commitments выходов/сдачи приватного платежа и нового Fidelity state | Пользовательский prover в целевом T09/T10; конкретные форматы ещё не реализованы | Собственный платежный/Fidelity witness и доказанный переход от предыдущих commitments | Владелец соответствующего приватного состояния; verifier получает commitments и proof |
| `C(b_new)` при выводе COEN | Пользователь, T10 | Свой старый баланс − публичный x, свежий blinding | Пользователь |
| Начальный `C(0)` Gratis | Пользователь при регистрации приватного account | Нулевой баланс и свежий blinding | Пользователь; сеть требует proof открытия именно нуля |

В последней строке выбран явный bootstrap для новой приватной записи. Нельзя разрешить пользователю поставить произвольный первый commitment без доказательства нулевого баланса. Миграция уже существующих Gratis-балансов — другой переход, он этим bootstrap не покрыт.

Постоянный коэффициент VSS должен быть **тем же commitment**, который защищает admission proof: `D_i[0] = C(a_i)`. В старом Ristretto-варианте это проверялось для каждого limb; в P-384 равенство одно; соответствующий source/range/arithmetic proof теперь реализован компонентом P_link. Только при выполненной связи пользователь не может подменить доказанный nominal другим VSS-значением. Для точного раннего расчёта G18 отдельный sharing g_i теперь не нужен.

При ротации меняются shares и commitments непостоянных коэффициентов общего sharing, но постоянный aggregate commitment остаётся тем же. Здесь нет нового commitment к произвольно выбранной «сумме дня».

**Для старого варианта из четырёх частей uint256** все суммы commitments выполняются покомпонентно. Их результат — commitments к суммам частей, а не автоматически нормализованный commitment к S. Новый P-384 компонент T03 устраняет это разбиение у VSS; для сохранения старого пользовательского proof потребуется доказать связь двух представлений.

Worker также создаёт Merkle roots / manifest hashes / привязку коэффициентов. Это commitments к публичным данным/структуре результата; их нельзя путать с Pedersen commitments к скрытым индивидуальным суммам.

Генераторы Pedersen G,H и правила верификации — общие параметры версии протокола. Пользователь выбирает свои blindings, а не произвольные G,H для отдельного Tribute. В частности, никто не должен знать дискретный логарифм одного генератора по другому.

### Что хранит кошелёк, а что хранит нода

- **Кошелёк:** суммы и openings незавершённых прав, текущее значение Gratis и его opening, данные для авторизации/proofs. Суммы не отправляются в verifier как публичные inputs.
- **Обычная нода:** commitments, публичные параметры, proofs/certificates, roots, owner/nonce/spent state.
- **Член комитета дополнительно:** собственные секретные shares накопителей; при сохранении поздних лиг — также данные для поздней группировки из T05.
- **Lysis worker:** публичные входы, проверенные агрегаты, таблица коэффициентов и формируемые записи Nod. Индивидуальные nominal/load/cost ему для T06–T07 не передаются.

Полная матрица «данные → создатель → держатель → условие удаления» и правила WAL/backup/DA находятся в [контракте хранения, §§1–2](STORAGE_AND_AGGREGATE_PROTOCOL.md#1-кто-хранит-что). При ротации переносится весь ещё нужный набор, а не только S дня. Opening Nominal хранится кошельком до завершения права Nod; актуальный witness Gratis нужен для следующего перехода. Root не заменяет доступность этих данных.

Кошельку нужны **и значение, и blinding**, а не только randomness: из `C(a)` и одного r нельзя практически восстановить произвольный uint256 a. Поэтому возможность позднего claim зависит от сохранения пользовательского witness. Свойства создания, открытия и сложения commitments: [Pedersen](https://www.zkdocs.com/docs/zkdocs/commitments/pedersen/); принцип распределённых shares: [Shamir](https://www.zkdocs.com/docs/zkdocs/protocol-primitives/shamir/).

### Кому принадлежат уже измеренные затраты предыдущего варианта

| Измеренный компонент | Кто выполняет работу |
|---|---|
| 141.928 мс issue proof + 1.407 мс VSS deal для 16/11 | Кошелёк пользователя при создании Tribute |
| 11.101 мс issue verification | Каждый проверяющий валидатор; измерена только арифметическая часть |
| 0.200 мс проверка своей VSS-доли, 16/11 | Каждый получатель доли в комитете |
| 52.684 мс handoff 16/11 | Суммарная последовательная работа всех старых/новых участников в локальной модели; не latency одной ноды |
| 0.126 мс открытие четырёх limb-агрегатов | Локальное восстановление/проверка из уже доступных aggregate shares; не стоимость strict-only-S протокола |
| 0.087 мс coefficient kernel, 8 лиг | Lysis worker; публичный расчёт можно независимо проверить |
| 0.108 мс шаблоны/хеши 256 Nod | Worker; без OCOMP, сети, хранения и остальных проверок |
| 302.087 мс claim proof | Кошелёк при Nod → Gratis |
| 21.907 мс claim verification | Каждый проверяющий валидатор |
| 141.683 мс withdrawal proof / 11.096 мс verification | Соответственно кошелёк / проверяющий валидатор |

Эти таблицы не добавляют новых замеров и не включают пока нереализованные source/PayNote/Fidelity proofs.


## 3. Трассировка по переходам

### T00. Offer → доказанный закрытый nominal; пользователь, во время offering

**Публичный вход:** day, owner, валюты, аутентифицированные oracle values, версия формулы, идентификатор/обязательство авторизованного offer.

**Секретный вход пользователя:** issuance `u_i`, исходное подтверждение offer и openings.

```text
vI = issuance_vwap
vR = reference_vwap
e  = max(reference_vwap, reference_scurve)
a_i = floor(u_i * M * vR / (vI * e))

public output: C(u_i), C(a_i), e, metadata, proof_offer
private output: u_i, a_i, r_u, r_a
```

Proof должен связывать **подлинный источник**, владельца, отсутствие повторного использования и эту формулу. Агрегат здесь не нужен. Скрывать только nominal недостаточно, если публично оставить issuance, из которого nominal считается по известным ценам.

**TEE заменяется** доказательством связи проверенного L2 draft с арифметикой и денежным commitment. Существующий FullProof можно сохранить как отдельную проверку авторизации/включения; новый proof должен привязать к нему скрытые поля и nominal. Commitments и секретные доли сами по себе эту связь не доказывают. Точный контракт двух proofs — ниже.

**Измерено:** арифметический proof полного uint256 с публичными множителем/делителем, включая range и ненулевые суммы. Авторизация источника в этот замер не входит. Формула в текущем коде: [compute_nominal](../../bin/outbe-tee-enclave/src/compute.rs#L118).

#### T00: связь существующего L2 proof с закрытым nominal

Уточнено по текущему коду 2026-09-11. Путь registered L2 с включённым ZK уже вызывает настоящий `verify_circuit::<FullProof>`: [validate_zk_result](../../crates/core/tributefactory/src/runtime.rs#L282). Перед этим проверяется подпись зарегистрированного L2 над root и совпадение root с public input. Unregistered/zk-disabled ветки проходят без этой проверки; целевой приватный admission не должен получать такой обход без другого эквивалентного доказательства источника. [Gate](../../crates/core/tributefactory/src/runtime.rs#L104).

В закреплённой зависимости `outbe-circuits` v0.14.0, commit `984d57ed0d2f014a1a74d0b3b4b0769801957791`, FullProof проверяет ownership/signature relation и Merkle inclusion для `nft_hash`. Четыре public inputs — `derived_owner, nft_hash, binding_hash, merkle_root`. Исходные base/atto и формула nominal не являются входами этого circuit. [Pinned dependency](../../Cargo.lock#L9925), [исходный circuit](https://github.com/outbe/outbe-circuits/blob/984d57ed0d2f014a1a74d0b3b4b0769801957791/crates/outbe-zk-canonical/noir/outbe-full-circuit/src/main.nr).

Сейчас недостающую связь создаёт enclave: расшифровывает payload, строит canonical TributeDraft hash из id/derived_owner/day/currency/base/atto/su_ids и binding hash из L1 sender/draft id/chain id; host сравнивает их с public inputs proof. Затем enclave вычисляет nominal. [derive_expected_hashes](../../bin/outbe-tee-enclave/src/zk_claim.rs#L44), [process_one](../../bin/outbe-tee-enclave/src/process.rs#L61).

Предлагаемый переход сохраняет два отдельных verifier вызова; рекурсивное доказательство проверки первого proof для этого не требуется:

~~~text
P_L2: existing FullProof over (derived_owner, H_draft, H_binding, root_L2)

P_link public:
  SAME derived_owner, H_draft, H_binding, root_L2
  L1 sender, chainId, day, currencies, pricing inputs/version
  C_nominal, source markers; optional C_issuance if a downstream consumer needs it

P_link private:
  draft id, base, atto; other canonical fields are bound public inputs
  r_nominal; optional r_issuance if C_issuance is published

P_link constraints:
  canonical_entity_hash(draft) == H_draft
  draft.derived_owner == derived_owner
  draft.day == day; draft.currency == tribute_currency
  binding(L1_sender, draft.id, chainId) == H_binding
  0 <= atto < 10^6; base fits the canonical u64 source format
  u6 = base*10^6 + atto
  a6 = exact current nominal formula(u6, verified Oracle inputs)
  if publishing C_issuance: C_issuance == u6*G + r_issuance*H
  C_nominal  == a6*G + r_nominal*H
  source markers correspond to the SAME draft.su_ids
  integer ranges, nonzero rules, quotient/remainder and overflow checks

L1 validator:
  verify registered L2 root authority and P_L2
  verify P_link; compare all shared public values exactly
  check pricing against chain state, day/owner/replay/source markers
  require VSS constant coefficient == SAME C_nominal
  require VSS availability before final admission
~~~

`C_issuance` нужен, если он остаётся частью целевого Tribute; можно не публиковать отдельную точку для u, если она далее нигде не нужна, сохранив u как private intermediate нового proof. Нельзя заменить проверку canonical entity hash обычным произвольным хешем другой структуры. В текущем коде `derived_owner` — L2 owner commitment, а `offer.owner` — L1 caller; новый proof должен сохранить обе роли, а не молча приравнять их.

| Значение | Что известно сети сейчас | Как проверяется без TEE |
|---|---|---|
| Root L2 | Public input + root signature зарегистрированной сети | Существующий gate и FullProof |
| derived_owner | Public input существующего FullProof | Та же величина включена в canonical draft нового proof |
| nft_hash | Public input; соответствие расшифрованному draft сейчас устанавливает TEE | P_link доказывает знание canonical draft с тем же hash |
| binding_hash | Public input; сейчас TEE связывает sender/draft id/chain id | P_link доказывает эту связь; sender/chain сверяются с вызовом |
| День, валюты, цены | Публичные поля и Oracle state | Валидатор сверяет public inputs P_link с chain state |
| issuance/nominal | Сейчас enclave возвращает plaintext результаты | Только private witness/intermediate и проверенные commitments |
| SU replay protection | Сейчас SU identifiers выходят из enclave и отмечаются used | P_link связывает выдаваемые markers с тем же draft; сеть проверяет повторы |

**Кто получает и хранит witness:** для пользовательского proving L2 должен предоставить кошельку исходный canonical draft (например, в шифротексте под ключом кошелька) и существующий P_L2 с public inputs. Один шифротекст под ключом enclave или один P_L2 не позволяют кошельку получить эти данные. Для отдельного P_link пользователю не нужны приватные signature/Merkle-path witnesses уже готового P_L2: он не пересоздаёт первый proof. Draft хранится до final admission/завершения повторных подач; a6,r_nominal сохраняются до завершения права Nod. Наличие такого канала в L2-клиенте здесь не проверено.

**Реализация P_link:** [standalone Groth16 component](measurements/p-link/README.md) выражает P-384 через emulated field внутри BN254 R1CS, связывает canonical hash с точной формулой nominal и C(a). Профиль — до четырёх SU markers; C_issuance не публикуется, u остаётся private intermediate. Формат hash сверен независимым Entity derive закреплённого outbe-protocol. [Результаты и ограничения](measurements/p-link/artifacts/results.json). L2 FullProof остаётся отдельным verifier; этот executable не выполняет его и не заменяет root authority / Oracle / replay / availability checks. Setup исследовательский, production ceremony/VK registry и интеграция не выполнены.

**Ресурсный блокер:** пользователь установил предел создания P_link в кошельке ≤512 MB RAM. Измеренный прямой P-384/BN254 вариант его не проходит и сохраняется как функциональный baseline; выбор этого backend не принят.

**Замер P_link (M4 Max, до четырёх SU markers):** создание 130.623 с (один образец), проверка 0.962375 мс (медиана пяти после прогрева), proof 128 B, C_nominal 49 B. Общий proving key 1.161 GB; peak RSS всего запуска 19.445 GB, включая setup и отрицательные R1CS проверки. [Подробности, публичные артефакты и тест P_link → VSS → reshare](measurements/p-link/README.md). Это цифры нового P_link, а не прежнего Ristretto issue proof или полного admission.

Существующая каноническая сумма источника ограничена форматом `u64 base + remainder <10^6`, даже если последующие типы — uint256. Поддержка произвольного uint256 уже на входе потребует версии source schema/circuit; P-384 VSS сам по себе этот формат не расширяет. [parse_canonical_amount](../../bin/outbe-tee-enclave/src/compute.rs#L160).

### T01. Admission Tribute → закрытое накопление; пользователь + комитет

Пользователь создаёт VSS для того же `a_i,r_a`. Постоянные коэффициенты VSS commitments должны совпасть с `C(a_i)`, проверенным в T00. Каждый получатель проверяет свою долю относительно одного общего набора coefficient commitments.

```text
public:
  verify proof_offer and commitment binding
  verify admission / availability certificate for the accepted input
  count += 1
  aggregate_commitments += C(a_i)
  accepted_set_root = append(exact Tribute record)

private at each participant j:
  aggregate_shares[j] += verified_share(a_i, r_a)[j]
```

**Всем видны:** commitments, proof, метаданные, count/root, VSS coefficient commitments и admission certificate. **Не видны:** issuance, nominal, индивидуальные openings, plaintext shares другим участникам.

Нельзя принять Tribute только по корректному proof, если достаточный состав комитета не получил согласованные доли: иначе пользователь может сделать сумму нераскрываемой. Откаты/повторы должны одинаково учитываться в каноническом наборе и накопителях.

Конкретный сетевой проект: фиксированный обязательный Q=11 получателей при n=16,t=6,f≤5; все Q подтверждают полноту одного и того же batch после durable сохранения своих shares. Произвольные разные наборы из 11 receipts на запись не гарантируют общий полный накопитель. При отказе Q приём останавливается до handoff; это ещё не реализованный сетевой протокол. [Правила P1–P3](STORAGE_AND_AGGREGATE_PROTOCOL.md#p1-общий-набор-получателей-а-не-произвольные-receipts).

**Стоимость/состояние:** один заранее определённый агрегат — 96 байт закрытых долей на участника в новом P-384 компоненте; в старом four-limb варианте — 256 байт. Индивидуальные доли после согласованного накопления не нужны **для этого агрегата**. Для позднего распределения по лигам они сохраняются вместе с доступными публичными coefficient vectors — см. T05.

Точка замены текущего кода: [bump_day_bucket](../../crates/core/tribute/src/state.rs#L317), сейчас `S += nominal_amount`; также [issue/event](../../crates/core/tribute/src/runtime.rs#L319) не должны публиковать суммы.

### T02. Ротация комитета внутри 50 часов; старый + новый комитет

```text
input:
  canonical day/root/count/checkpoint
  old verified aggregate shares
  retained individual records and public coefficient vectors if later grouping needs them
  new committee and its threshold

operation:
  verifiable resharing of the accumulated secret
  new committee confirms the same aggregate commitment and checkpoint
  admission switches to the new committee exactly at the agreed boundary

output:
  fresh shares of THE SAME accumulated secret
  no public nominal or intermediate day sum
```

**DKG новых ключей сам по себе накопленную сумму не переносит.** Нужен handoff/resharing с согласованием набора входов. Для одного агрегата стоимость передачи зависит от размеров комитетов, а не от числа уже принятых Tribute.

В старом Ristretto-варианте измерена математическая передача `16 участников / порог 11 → новый 16/11`: **52.684 мс**, все dealer/recipient проверки последовательно на одной машине; **45,056 байт** закрытых долей + **15,488 байт** coefficient commitments.

Новый P-384 компонент, 16/6 → 16/6: **80.778 мс**, **9,216 байт** private payload и **1,764 байт** публичных coefficient commitments, один образец для одного агрегата. Сетевая задержка, consensus и передача множества индивидуальных записей не измерены. Новый комитет подтверждает весь retention manifest до удаления старого состояния: [P4](STORAGE_AND_AGGREGATE_PROTOCOL.md#p4-что-переносится-при-ротации).

Конфиденциальность зависит от отсутствия порогового сговора. Для защиты от накопления компрометаций за много ротаций нужны свежие shares, удаление старых секретов и защищённые каналы; математический reshare-тест не доказывает исполнение этих условий.

### T03. Закрытие offering → открытие итогового S; комитет

После окончательной фиксации набора и до первого потребителя суммы дня:

```text
input: final accepted-set binding + closed aggregate + commitments
output: verified S, count, final-set binding, aggregate certificate/proof
remain private: every a_i and u_i
```

**Именно здесь открывается итог дня.** Не каждый блок, не каждый префикс и не каждый Tribute. Пользователям повторно приходить не нужно. Lifecycle по текущим default-константам: 50 часов offering и ещё 12 часов waiting; открытие привязывается к завершённому состоянию, а не к таймеру на клиенте. [Lifecycle](../../crates/core/metadosis/src/lifecycle.rs#L206), [constants](../../crates/core/metadosis/src/constants.rs#L13).

**Теперь проверен конкретный компонент P-384 VSS.** Его scalar field вмещает весь uint256 и даже максимальную промежуточную сумму 10⁹ входов (286 бит). Пользователь создаёт один C(a), а не четыре limb commitments. После freeze:

1. t членов комитета публикуют свои суммарные пары (U_j,W_j), привязанные к одному epoch и final input root.
2. Любая нода проверяет каждую пару по публичному aggregate coefficient vector.
3. Эта же нода интерполирует S и случайный aggregate blinding R, затем проверяет `S*G+R*H=CΣ` и диапазоны.
4. Проверенный S передаётся в T04. Пользователь в открытии не участвует.

Открывается **один денежный итог S**, плюс R и проверяемые aggregate contributions; денежные limb totals отсутствуют. MPC-финализатор переносов в этом компоненте не нужен. Opening с проверкой всех t вкладов измерен: **4.390 мс для 16/6**, 7.543 мс для 16/11. [Код, результаты и границы](measurements/wide-vss/README.md); [точные сообщения и проверки P5](STORAGE_AND_AGGREGATE_PROTOCOL.md#p5-кто-открывает-s-и-какое-доказательство-выдаёт).

**Связь с admission:** новый [P_link](measurements/p-link/README.md) защищает именно этот P-384 C(a), включая исходный canonical draft hash, nominal formula и диапазон. D[0] VSS должен сравниваться с C(a) из проверенного proof. Старые Ristretto proofs для этой связи не используются. Production P_L2+P_link admission и сетевой final-set binding ещё требуют реализации.

Старый uint256-прототип открывал четыре суммы 64-битных частей и их blindings; замер 0.126–0.189 мс ниже относится к нему. Эти четыре totals содержат больше информации, чем S, поэтому тот вариант остаётся отдельным baseline.

Если протокол требует также `S ≤ uint256::MAX`, это отдельное обязательное ограничение; нельзя молча взять `S mod 2^256`. P-384 позволяет обнаружить широкий итог, но сохранение текущего отказа на переполнение уже при admission потребует закрытой проверки накопленного диапазона или заранее установленного более строгого лимита входов. В прототипе сетевого admission с этим правилом нет.

### T04. S₆ → бюджет B₁₈; Metadosis

Ноды получают проверенный S₆ из T03. Целевые денежные бюджеты и накопитель лимита выражены в 10¹⁸; старое входное поле 10⁶ умножается на 10¹² ровно один раз. Переход единиц требует версии состояния/API.

~~~text
S18 = S6 * 10^12
D0_18 = floor(S18 * 32 / 100)
Green: D18=D0_18;          Q18=E18
Red:   D18=floor(D0_18/8); Q18=floor(E18/8)
B18 = min(D18,Q18)

C0_18 = E18-B18
Green: A18=min(S18-B18,K18+C0_18)
Red:   A18=0
receipt.day_limit18=E18+A18
Lysis budget=B18
~~~

Это открытая арифметика. При B18=0 действует терминальная ветка. A18 не увеличивает бюджет начисления Gratis.

Код до миграции единиц: [calculate_metadosis](../../crates/core/metadosis/src/settlement.rs#L32), [READY](../../crates/core/metadosis/src/settlement.rs#L111), [budget](../../crates/core/metadosis/src/ocomp_budget.rs#L41), [request](../../crates/core/metadosis/src/ocomp/request.rs#L148). Aggregate receipt связан с точным sealed набором.

### T05. Определение лиг → S_l; точка, где одного дневного агрегата мало

Текущий код фиксирует `owner → league` при READY, после offering: [build_fidelity_league_snapshot](../../crates/core/metadosis/src/ocomp/snapshot.rs#L21). Затем Lysis получает `(league, n_l, S_l)`: [fidelity_map/reduce](../../crates/core/lysis/src/program_v1/phases.rs#L156).

**Чтобы оставить текущую формулу коэффициентов открытой, нужны проверенные S_l**, а не только S. Это дополнительный публичный выход предлагаемого маршрута. Если разрешён только общий S, вычисление коэффициентов по закрытым S_l переносится в отдельный MPC/ZK; его стоимость здесь не измерена.

| Вариант | Что сохранять во время offering | После snapshot | Цена/изменение |
|---|---|---|---|
| Сохранить позднее определение лиги | Проверяемые закрытые доли по владельцам/Tribute и публичные coefficient vectors | Сгруппировать доли и commitments; открыть S_l | P-384: 96 B private на запись на участника, при 1 млрд — 96 GB; дополнительно при t=6 публичные vectors занимают 294 GB в одной DA-копии. Ротация O(N). Старый Ristretto: 256 B private на запись |
| Зафиксировать лигу при admission | По одному накопителю на лигу | Уже готовые агрегаты S_l | O(число лиг) состояния; **меняет момент Fidelity snapshot** |
| Сохранить поздние лиги и открывать только S | Закрытые данные, достаточные для группировки, плюс распределённое вычисление | Выдать доказанные коэффициенты из закрытых S_l | Не устранение потребности в данных; отдельная неизмеренная MPC/ZK-система |

**Выбор здесь не сделан за пользователя.** Для масштабного O(число лиг) решения требуется явное изменение snapshot-семантики либо другой протокол, сохраняющий возможность поздней группировки.

При сохранении поздних групп удалять все индивидуальные shares после T01 нельзя. Сумма S уже не содержит распределение по будущим лигам.

Для Fidelity тоже нужен путь без TEE: доказательство корректного обновления скрытого состояния и принадлежности к публичной лиге. Сохранить вызов enclave в Fidelity и назвать весь маршрут «без TEE» нельзя.

### T06. Агрегаты → коэффициенты f₆ и точный G₁₈; Lysis

Предполагаются разрешённые публичные S_l6 либо эквивалентное доказанное закрытое вычисление. Worker использует агрегаты и публичные параметры.

~~~text
S18 = S6 * 10^12
y_l6 = floor(S_l6 * M / S6)
last_share += M - sum(y_l6)
f6 = floor(B18 * M / S18)
fmax6 = 2*f6
f_l6 = calc_fraction_distribution_fp(y_l6,n_l,N,f6,fmax6)

G0_18 = Σ_l (S_l6 * f_l6 * M)
if G0_18 > B18:
    f_l6 = floor(f_l6 * B18 / G0_18)

G18 = Σ_l (S_l6 * final_f_l6 * M)
assert G18 <= B18
R18 = G18
~~~

**G18 — точная сумма всех будущих g18**, не верхняя оценка. Публичные деления внутри алгоритма коэффициентов остаются. После фиксации f_l6 индивидуальные g/c считаются без округления.

Текущие [compute_fraction_map_from_groups](../../crates/core/lysis/src/program_v1/execute.rs#L392) и [coefficient kernel](../../crates/core/lysis/src/algorithm.rs#L229) — точки изменения. Нынешнюю нормализацию по суммам floor в масштабе 10⁶ нельзя перенести без изменения: она может пропустить расход, превышающий бюджет в точной модели. Проверяются G0_18 и B18.

### T07. Проверенные входы + f_l → Nod; worker по 256 записей

```text
input public per Tribute:
  id, owner, day, league binding, C(a_i), currencies,
  effective tribute price, exclude flag
input public shared:
  coefficient table/root, entry prices, policy/version, reservation R

per record:
  attach f_l and p_i
  h_i = floor(max(tribute_price_i, p_i) * 108 / 100)
  make Nod{id/owner/day, C(a_i), f_l, p_i, h_i, eligibility/expiry bindings}

NOT computed at this stage:
  individual g_i18 = a_i6 * f_l6 * M
  individual c_i18 = a_i6 * f_l6 * p_i6
```

**Всем видны:** Nod, исходный commitment nominal, коэффициенты, цены и правила погашения. **Скрыто:** nominal и ещё не раскрытые индивидуальные результаты.

Владелец не обязан быть онлайн. Его proof из T00 уже проверен; worker опирается на канонический принятый набор. Повторять каждый арифметический proof в Lysis не обязательно, если протокол аутентифицирует результат admission. Проверки root, полного покрытия входов, eligibility, сортировки, DA и сертификации остаются.

**Изменение текущего кода:** [amount_map](../../crates/core/lysis/src/program_v1/phases.rs#L358) и [output_finalize](../../crates/core/lysis/src/program_v1/phases.rs#L637) сейчас рассчитывают и переносят числовые g/c. Их заменяет deferred claim descriptor. При положительных a6, f6 и p6 точные g18/c18 положительны: исчезает обнуление маленьких сумм из-за индивидуального floor. Нулевой f6 остаётся возможным публичным результатом квантования коэффициента; его политика должна быть определена. Диапазоны/overflow проверяются отдельно.

Ветка contributors с `exclude=false` требует аналогично заменить индивидуальный nominal на commitment. Её сумму `H_eligible` можно накапливать с admission, поскольку exclude известен заранее; это ещё один агрегат, если существующий finalizer продолжает его требовать.

### T08. Точный возврат неиспользованного лимита; конец Lysis

~~~text
G18 = Σ_l (S_l6 * f_l6 * M) = Σ_i g_i18
unused18 = B18 - G18
return unused18
keep G18 assigned to the created Nod rights
~~~

Этот шаг выполняется после проверки полного набора Nod **до пользовательских claims T09**. Ноды уже знают точный G18 по агрегатам. Ждать владельцев и открывать индивидуальные g не требуется; отдельный VSS по g для этого не нужен.

G18 означает объём выделенных прав Nod, а не уже погашенный объём. Возврат за позднее истечение непогашенных прав, если он нужен, — отдельная политика.

Остаток unused18 может иметь больше шести знаков: PromisLimit должен сохранять точность 10¹⁸ либо точно учитывать дробный остаток отдельно. Приведение результата к старому 10⁶ теряет точность.

Если finalizer требует также общий cost, а p_i различается между записями, одних S_l6 для cost недостаточно. Нужны суммы по группам с одинаковыми (лига, entry price), либо закрытая проверка weighted total. Это отдельный потребитель агрегатов; он не мешает точному расчёту G18.

Numeric conservation/stream checks требуют адаптации к commitments и единицам: [finalizer](../../crates/core/lysis/src/program_v1/finalizer.rs#L241), [activation](../../crates/core/metadosis/src/ocomp/activation.rs#L358).

### T09. Nod + приватная оплата cost → приватный Gratis; пользователь, позже

**Публичный вход:** исходный Nod и его неизменяемые коэффициенты, owner, состояние квалификации/срок, старый commitment Gratis, nonce, платежный nullifier/commitment и валюта.

**Секретный witness пользователя:** `a_i,r_a`, старый баланс и opening, закрытые платежные данные.

```text
g_i18 = a_i6 * f_l6 * M
c_i18 = a_i6 * f_l6 * p_i6
require g_i18 > 0 and c_i18 > 0
prove private payment covers EXACTLY c_i18 under the chosen settlement units
b_new18 = b_old18 + g_i18
prove all integer ranges and no overflow

atomic public state transition:
  verify proof + authorization/eligibility + current nonce
  consume payment nullifier
  mark Nod spent
  replace C(b_old) with fresh C(b_new)
  increment nonce
```

Proof теперь проверяет точные целочисленные равенства g18=a6·f6·M и c18=a6·f6·p6. Remainder witness для этих двух шагов не нужен. Проверки range, переносов между limbs, авторизации, оплаты и обновления баланса остаются.

**Снаружи нет g_i, c_i, b_old или b_new.** Commitments g/c связывают арифметику с платежом и балансом; для общего выделенного G18 VSS этих значений больше не нужен. Публичный event или изменение публичного totalSupply на g_i снова раскрыло бы начисление; такие изменения тоже должны быть закрытыми.

**Оплату нельзя пропустить:** текущий [discharge_cost](../../crates/core/nodfactory/src/runtime.rs#L256) проверяет `claim.spend_amount == cost`, spender и settlement asset. Если оставить spend_amount открытым, путь не сохраняет требуемую приватность cost. Нужен приватный PayNote spend/change с proof равенства c18 и согласованными единицами. Если платёжный инструмент допускает лишь шаг 10⁻⁶, произвольный cost с 18 знаками им без остатка не оплатить: нужно отдельное правило дробного учёта/расчёта либо округления на платёжной границе. Эта граница ещё не реализована. Прототип проверяет формулу cost, **но не реализует этот платежный proof**.

Текущий [mint Gratis](../../crates/core/gratisfactory/src/runtime.rs#L129) обновляет ещё и Fidelity cohort. В целевом варианте это закрытое обновление с доказательством, без TEE; в измерение арифметического claim оно не входит.

### T10. Gratis₁₈ → публичный COEN₁₈; пользователь

~~~text
public input: account, x18, nonce, C(balance_old18)
private witness: balance_old18 and its opening, fresh blinding
prove:
  balance_old18 = balance_new18 + x18
  valid integer ranges, authorization, nonce and Fidelity transition
atomic effect:
  C(balance_old18) -> C(balance_new18), nonce++
  native COEN balance += x18
~~~

Кошелёк считает скрытый остаток и создаёт commitment/proof. Валидатор проверяет переход и выпускает публичный COEN. **Оба целевых баланса имеют 18 знаков; дополнительного множителя 10¹² нет.**

Текущий [mine_coen](../../crates/core/gratisfactory/src/runtime.rs#L145) переводит Gratis₆ → COEN₁₈ через 10¹². Это меняется вместе с форматом Gratis. Сохранить прежний множитель после перехода входного amount к 10¹⁸ нельзя. Burn, mint и Fidelity остаются атомарными.

Публичны x18, получатель и факт операции; остаток Gratis и исходные nominal/g/c не публикуются.

## 4. Краткая карта видимости

| Объект/момент | Видно всем в целевом маршруте | Скрыто |
|---|---|---|
| Tribute до закрытия дня | ID/owner/day/валюты, commitments, proofs, цены, count/root | issuance, nominal, openings |
| VSS во время offering | Coefficient commitments, checkpoint, committee/admission certificates | Индивидуальные и суммарные shares; у участника только его доля |
| После открытия дня | S, aggregate blinding R и проверяемые суммарные вклады относительно финального набора | Индивидуальные nominal/openings; P-384 компонент T03 открывает S без limb totals, admission proof ещё нужен |
| Лиги/коэффициенты | l, n_l, f_l, B; в варианте открытого coefficient kernel также S_l и R | nominal каждого Tribute |
| Nod | C(nominal), коэффициенты, цены, owner, правила погашения | nominal, индивидуальные Gratis load и cost |
| Nod → Gratis | proof, spent marker/nullifier, новые commitments, nonce | g, c, платежный amount, баланс до/после |
| Gratis account | commitment баланса, account/nonce; commitments Fidelity | Баланс и внутренние cohort-данные |
| Конец Lysis | Точный G18 из S_l6 и f_l6, возврат B18−G18 | Индивидуальные начисления |
| Gratis → COEN | Выводимый x18, он же raw native COEN amount | Остаток Gratis |

## 5. Измеренные время и размер предыдущего варианта

**Замеры сохранены без пересчёта. Claim/withdrawal проверяли прежние floor-формулы и масштаб баланса 10⁶; это не измерения нового exact-перехода 6→18.** Новые арифметические proofs требуют отдельного запуска.

Машина: **Apple M4 Max, 14 CPU (10P+4E), 36 GB RAM**, arm64, release. Последовательные локальные замеры; CPU не изолирован от других процессов. Числа не являются TPS сети. JSON: [Rust](measurements/rust_results.json), [worker components](measurements/worker_results.json), [SEAL](measurements/seal_results.json).

### Пользовательские proofs

| Проверяемая арифметика | Создание proof | Проверка | Proof | Все commitments statement |
|---|---:|---:|---:|---:|
| issuance → nominal, uint256 | 141.928 мс | 11.101 мс | 1,121 B | 256 B |
| nominal → g/c → скрытый Gratis | 302.087 мс | 21.907 мс | 1,185 B | 640 B |
| скрытый Gratis → публичный вывод | 141.683 мс | 11.096 мс | 1,121 B | 256 B |

Время — медиана четырёх измерений после одного прогрева. Включены commitments и арифметика; генерация общих Bulletproof generators вынесена из времени. Для issue замерены публичные numerator/denominator по `10^12`; более широкие коэффициенты меняют размер схемы и время.

Не все commitments заново передаются: в claim C(nominal) и старый C(balance) уже находятся в состоянии. Новые C(g), C(c), C(balance) занимают 384 B; арифметический claim payload — **1,569 B** до PayNote/Fidelity/metadata. Для вывода proof + новый C(balance) + публичный uint256 amount — **1,281 B** до остальных полей.

### VSS, один номинал из четырёх частей

| Параметр | n=16, t=11 | n=128, t=86 |
|---|---:|---:|
| Создать все доли/commitments | 1.407 мс | 19.519 мс |
| Проверить долю у одного получателя | 0.200 мс | 0.478 мс |
| Отправить закрыто всем получателям | 4,096 B | 32,768 B |
| Доля одного получателя | 256 B | 256 B |
| Доп. публичные coefficient commitments сверх C(a) | 1,280 B | 10,880 B |
| Закрытый накопитель одного агрегата у получателя | 256 B | 256 B |
| Публичные coefficients накопителя | 1,408 B | 11,008 B |
| Раскрыть/проверить четыре limb-агрегата | 0.126 мс | 0.189 мс |
| Raw shares для открытия | 2,816 B | 22,016 B |

Это базовый Pedersen VSS, без amortized/batched proofs и без сетевых сертификатов. Рост публичного описания с t — реальная стоимость именно этой реализации. Аутентифицированное шифрование транспорта, подписи и encoding overhead добавляются.

### Lysis и Nod

В benchmark напрямую включён production `lysis/src/algorithm.rs`, с минимальной заменой типов ошибок/констант вне алгоритма. Для одинаковых долей и populations:

| Компонент | Локальное время |
|---|---:|
| coefficient kernel, 8 лиг | 0.087 мс |
| coefficient kernel, 32 лиги | 0.404 мс |
| Сформировать/скопировать 256 шаблонов Nod по 343 B и посчитать по одному Keccak | 0.108 мс |

Последняя строка даёт примерно **2.37 млн сериализаций/хешей Nod в секунду**. Это **не** скорость создания Nod в блокчейне: нет проверок inputs, Merkle tree, storage, DA, OCOMP и consensus. Байты commitment в этом замере уже считаются полученными; вычисление/проверка кривой не измеряется.

**Полное время worker на 256 записей пока не измерено**, поскольку целевого worker с deferred claims в production ещё нет. Замер текущего eager worker не был бы замером предлагаемого протокола. Формула бюджета его времени:

```text
T256 = authenticated input fetch/check
     + coefficient-table lookup
     + 256 public descriptor constructions
     + ordering/Merkle/manifest work
     + storage/DA
     + OCOMP verification/certification
```

Если протокол всё-таки заново проверяет 256 issue proofs, только эта арифметическая часть добавляет **2.842 секунды на одном потоке**. При доверии к проверенному admission она не должна автоматически добавляться второй раз.

## 6. Сколько данных при 1 млн и 1 млрд — предыдущий вариант

Ниже сохранены размеры прежнего Ristretto/limb варианта. Новые P-384 размеры и отдельный учёт public/private retention приведены в [контракте хранения, §4](STORAGE_AND_AGGREGATE_PROTOCOL.md#4-объём-хранения--выбранные-поля-не-вся-транзакция); эти два набора нельзя смешивать.

Ниже задан **конкретный пример encoding**, не нынешний ABI и не финальный размер транзакции:

- Tribute body: version 1 + ID 32 + owner 20 + day 4 + currencies 40 + price 32 + C(issuance) 128 + C(nominal) 128 + exclude 1 + source binding 32 = **418 B**.
- С арифметическим proof: **1,539 B**.
- С базовым VSS public coefficients: **2,819 B** при 16/11, **12,419 B** при 128/86.
- Nod body: version 1 + source ID 32 + owner 20 + day 4 + league 2 + C(nominal) 128 + fraction 32 + entry price 32 + floor price 32 + currency 20 + expiry 8 + params root 32 = **343 B**.
- Значение Gratis account: C(balance) 128 + nonce 8 = **136 B** без ключа owner, Fidelity, индекса и накладных расходов БД.

Внешний source proof, приватный PayNote, Fidelity, availability certificate, transaction framing и consensus proofs **в эти размеры не входят**. Полный размер production Tribute ещё не установлен.

| Поток/данные | 1 млн Tribute | 1 млрд Tribute |
|---|---:|---:|
| Публичные Tribute+арифм. proof+VSS, 16/11 | 2.819 GB | 2.819 TB |
| Закрытая рассылка долей, суммарно 16 адресатам | 4.096 GB | 4.096 TB |
| Публичные Tribute+арифм. proof+VSS, 128/86 | 12.419 GB | 12.419 TB |
| Закрытая рассылка долей, суммарно 128 адресатам | 32.768 GB | 32.768 TB |
| Тела Nod по 343 B | 343 MB | 343 GB |
| Сохранённые индивидуальные shares при поздней группировке, на участника | 256 MB | 256 GB |

Единицы десятичные. Публичные объёмы посчитаны для одной копии истории; репликация умножает физический объём. Можно отдельно проектировать pruning/DA, но данные для приёма и проверки всё равно передаются.

**Для одного S накопленное секретное состояние остаётся 256 B на участника**, даже при 1 млрд входов. Это не означает, что вся цепочка занимает 256 B: commitments, proofs, история и возможные shares для поздних лиг считаются отдельно.

Простая передача каждой индивидуальной записи при каждой ротации чрезвычайно дорога: при равномерном поступлении за 50 часов и 49 часовых handoff получается 24.5·N передач записей. Для схемы, где t старых участников каждый resharing-ит запись всем n новым, только закрытая рассылка даёт ~1.10 PB при 16/11 и ~69.0 PB при 128/86 для N=1 млрд. Это оценка **базового неупакованного протокола**, не нижняя граница любой возможной схемы. Компактный handoff одного агрегата от N не зависит.

Формулы и расчёт: [scale_model.py](measurements/scale_model.py), [scale_results.json](measurements/scale_results.json).

## 7. Можно ли параллелить Tribute

**Да.** Пользователи независимо строят свои proofs и VSS deals; проверки разных Tribute можно распределять по CPU; частичные суммарные shares и commitments складываются ассоциативно. Пачки можно свести в один канонический root/aggregate.

По этому прототипу один поток проверяет примерно **90 арифметических issue proofs/с**, или **86–88 proofs + одна VSS-share check/с**. Это скорость одного криптографического компонента, не лимит всех возможных ZK-систем.

1 млрд за 50 часов — **5,556 Tribute/с**. Для текущего исследовательского backend это ~**63–64 эквивалента такого CPU-ядра на каждую реплику, проверяющую все входы**, только на арифметический proof и её share check. Здесь ещё нет source proof, проверок доступности, consensus и overhead. Параллельный запуск/масштабирование на 64 ядрах не измерены; это деление стоимости на требуемую скорость.

Nod claims разных владельцев также параллелятся. Изменения одного и того же Gratis account сериализуются по nonce; нужны retry/batching правила, поскольку два proofs против одного старого commitment нельзя оба применить.

## 8. Архив: SEAL исключён из рабочего варианта

Ниже сохранены уже полученные результаты сравнения. Дальнейшая трассировка и реализация рассматривают Pedersen + VSS + ZK; новых работ по SEAL не планируется.

Официальный SEAL предоставляет точные модульные BFV/BGV вычисления и приближённый CKKS; для денежных целых здесь проверен **BFV**. [Microsoft SEAL](https://github.com/microsoft/SEAL).

Использованы 16 слотов по 16 бит на один uint256, 50-битный plaintext modulus, остальные слоты нулевые. **Один пользователь — один ciphertext** в этом замере. Совместное упаковывание 8192 независимых пользователей бесплатно не предполагается.

| BFV профиль | Ciphertext без сжатия | Public-key encrypt | Add | Decrypt+decode | Stress 10^9 повторов |
|---|---:|---:|---:|---:|---|
| degree 4096 | 131,185 B | 0.567 мс | 0.00656 мс | 0.127 мс | **Ошибка: noise budget исчерпан** |
| degree 8192 | 524,401 B | 1.495 мс | 0.02595 мс | 0.437 мс | Точный результат, 86 бит noise budget осталось |

Stress реализован двоичным сложением одного ciphertext миллиард раз. Это проверка конкретного арифметического/noise сценария, **не обработка миллиарда независимых сетевых сообщений** и не доказательство параметров для всех adversarial inputs.

В профиле, прошедшем stress, миллиард входных ciphertexts — **524.401 TB** до proofs и репликации. Один rolling aggregate действительно занимает один ciphertext, но это не убирает входной трафик и историю, если ciphertexts хранятся там.

В этом SEAL benchmark один процесс владеет полным secret key. **Threshold decryption, валидность каждого входа, equality proof с Tribute commitment и ротация ключевых долей не реализованы и не измерены.** Поэтому скорость SEAL decrypt нельзя выдавать за время открытия итогового агрегата валидаторами.

SEAL мог бы заменить арифметический транспорт агрегирования, но не заменяет proofs владельца и приватное состояние Gratis. Для суммы и открытых коэффициентов его возможности умножения шифротекстов в этом маршруте не используются.

## 9. Решения, которые trace действительно выделил

| Вопрос | Вывод |
|---|---|
| Можно ли накопить общую сумму, не открывая каждый nominal? | Да: VSS-суммирование закрытых долей с commitments и проверкой входов. Отдельное хранение миллиарда ciphertexts для одного агрегата не требуется |
| Достаточно ли только Pedersen? | Для commitments и линейных операций — да; для получения публичного числового агрегата без владельцев — нужны закрытые доли/другой транспорт и протокол открытия |
| Должен ли Lysis вычислять каждый g/c? | В целевом deferred-claim контракте — нет; вычисляет пользователь позже и доказывает |
| Получится ли скрытый Gratis? | Арифметика доказуема; надо также заменить публичные amount-events, платежный spend, totalSupply updates и Fidelity |
| Возможен ли точный ранний возврат лимита? | Да: с входами 10⁶ и результатами 10¹⁸ G18 точно получается из S_l6 и f_l6. Снятие промежуточных округлений и масштаб учёта меняют прежнюю экономику/ABI |
| Что пока мешает назвать тест полным? | P-384 only-S opening и связывающий P_link реализованы компонентно; production admission должен объединить P_L2, P_link и VSS availability. Также не реализованы сетевые receipts/handoff, поздние лиги без TEE, source authorization, private PayNote/Fidelity и интеграция worker/chain |
| Какой набор продолжать проверять? | Pedersen + пользовательские ZK proofs точных равенств + VSS nominal-агрегатов; заново измерить exact claim и интеграцию |

Математические свойства Pedersen и требования к generator/randomness описаны в [ZKDocs](https://www.zkdocs.com/docs/zkdocs/commitments/pedersen/); Shamir shares — в [описании Shamir](https://www.zkdocs.com/docs/zkdocs/protocol-primitives/shamir/). Числа производительности выше получены из локальных исходников, а не из этих источников.

## 10. Проверяемость этого отчёта

Граф: project `Users-sakor-outbe-io-outbe-chain`, generation `2026-09-07T14:04:40Z`, Tier 2. Проверена coverage всех использованных source paths. `tribute/state.rs`, `nodfactory/runtime.rs`, `gratisfactory/runtime.rs` имеют changed metadata; значимые участки перечитаны напрямую. Новые benchmark-файлы не отслеживались индексом и проверены по исходникам. Чистая coverage означает отсутствие зарегистрированных дыр, не полный аудит.

Прототип подтвердил: uint256 MAX в identity-формуле принимается; нулевой nominal, неверный nonce, коэффициент, результат floor, лишнее зачисление и плохая VSS-доля отвергаются. Сценарий с двумя владельцами переносит агрегат в новый комитет, открывает итог, проверяет claim и вывод. Секретные fixture values в JSON помечены debug-only: это не формат публичной транзакции.

Полная security review, сетевой fault-injection, мобильный противник, нагрузочный тест и production integration остаются отдельными работами. Неизмеренные строки выше намеренно не заполнены оценками «на глаз».
