# Trace вычислений и раскрытия агрегатов: Tribute → Lysis → Nod

> Новая исходная схема и подтверждённые ограничения: [PROTOCOL_TRACE_AND_REQUIREMENTS.md](PROTOCOL_TRACE_AND_REQUIREMENTS.md). Выбор криптографии отложен; приведённые здесь конструкции/замеры — предшествующие варианты. Для текущего source codec новый trace выводит более тесные денежные диапазоны и учитывает Fidelity/forfeit после Lysis.

> Для текущей цели — без TEE, Nod с коэффициентами, поздний пользовательский proof, скрытый Gratis и открытый вывод COEN — см. [TARGET_TRACE_NO_TEE.md](TARGET_TRACE_NO_TEE.md). Ниже сохранена трассировка текущего eager-расчёта и прежнего варианта его приватизации; его T11–T16 не являются выбранной целевой схемой.

Проверка исходников: 2026-09-11, HEAD `177a72ddbea9f2e52eef094405481292ecd56046`. Рабочее дерево содержит сторонние изменения; этот документ не меняет production-код.

**Постановка:** индивидуальные суммы Tribute остаются закрытыми. Требуется вычислить общую сумму и передать её открытым числом потребителю. Ниже отдельно показаны текущие операции и необходимые замены. Ни один предлагаемый `OpenAggregate` ещё не реализован этим кодом.

Отдельно: [публичные и скрытые поля Tribute, Nod и Gratis](PRIVACY_VISIBILITY.md).

Обозначения: `a_i` — nominal конкретного Tribute; `M=10^6`; `S=Σa_i`; `E` — выделенная дню эмиссия Metadosis; `B` — бюджет Lysis. `C(x)` — commitment; `[x]_j` — секретная доля у участника j. Commitment и доля — разные данные. `[x]` обозначает распределённое закрытое значение, а не готовое числовое поле Rust.

В колонке «замена» рассматривается маршрут с открытыми итоговыми агрегатами и коэффициентами. Для сумм лиг и итогов Lysis это явно указанные выходы такого варианта; исходное требование пользователя разрешает раскрытие общей суммы дня. Если дополнительные агрегаты должны оставаться закрытыми, их потребители также переходят в закрытое вычисление.

## Trace: приём и получение суммы дня

~~~text
T00. process_one → compute_nominal                         [каждый offer]
  INPUT:
    amount_i; issuance_vwap_i; reference_vwap_i; reference_scurve_i
  CALC:
    price_i = max(reference_vwap_i, reference_scurve_i)
    a_i = floor(amount_i * M * reference_vwap_i
                / (issuance_vwap_i * price_i))
  OUTPUT СЕЙЧАС:
    TributeOfferResult.issuance_amount_minor = amount_i
    TributeOfferResult.nominal_amount_minor  = a_i
    TributeOfferResult.effective_reference_price_minor = price_i
  NEXT:
    T01
  ЗАМЕНА:
    amount_i и a_i остаются внутри приватного вычисления.
    Наружу: C(amount_i), C(a_i), подтверждение авторизованного offer
    и правильности формулы; price_i и метаданные могут быть открыты.
    Для последующих вычислений передаются также проверяемые закрытые
    данные a_i. Одного C(a_i) для получения суммы дня недостаточно.
  АГРЕГАТ НЕ НУЖЕН.

T01. issue_inner → bump_day_bucket                         [каждый принятый Tribute]
  INPUT СЕЙЧАС:
    TributeData{owner, day, a_i, issuance_amount, currencies, price_i, exclude}
    DayTotals{N, S}
  CHECK:
    valid Tribute; день принимает; ID не повторяется; диапазоны/overflow
  CALC СЕЙЧАС:
    N' = N + 1
    S' = S + a_i
    mint(body); emit TributeIssued(...nominalAmountMinor=a_i...)
  OUTPUT:
    новое тело Tribute; новые DayTotals
  NEXT:
    T02, затем T04
  ЗАМЕНА:
    открыто:  N' = N+1; C_S' = C_S + C(a_i); root принятого набора
    закрыто: [S']_j = [S]_j + [a_i]_j
    тело/event содержат commitments вместо индивидуальных amounts.
    Proof/admission связывает доли, C(a_i), авторизацию и этот же Tribute.
    Отклонённый/откаченный Tribute не входит в сумму; удаления учитываются
    согласованным вычитанием из того же набора.
  АГРЕГАТ НАКАПЛИВАЕТСЯ ЗАКРЫТО. ЧИСЛО S НАРУЖУ НЕ ВЫДАЁТСЯ.

T02. CloseOffering → seal_day                              [приём завершён]
  INPUT:
    окончательный принятый набор дня; его count/root и закрытый [S]
  EFFECT СЕЙЧАС:
    новые Tribute дня больше не принимаются
  NEXT:
    T03 можно выполнить после фиксации окончательного набора;
    T04 наступает при READY.
  ЗАМЕНА:
    зафиксировать, к какому day/count/root относится агрегат.
    Для раскрытия берётся окончательный канонический набор.
    seal_day и последующий CE/pre-admission seal — разные границы кода.
  ВРЕМЯ:
    offering по default-константам — 50 часов; waiting до обработки —
    ещё 12 часов. Момент берётся из lifecycle, не из локального таймера.

T03. OpenAggregate(S)                                      [НОВЫЙ шаг]
  INPUT:
    закрытые суммарные доли [S]_j и [R]_j;
    C_S = сумма commitments принятого набора;
    day, count, binding к окончательному набору
  CALC:
    участники раскрывают/восстанавливают только агрегаты S и R
    проверка C_S = Commit(S,R), плюс диапазон целого S и binding набора
  OUTPUT ОТКРЫТО:
    AggregateReceipt{day, count, collection_binding, S, verification}
  OUTPUT ЗАКРЫТО:
    индивидуальные a_i; их openings/доли не публикуются
  NEXT:
    T04; позднее тот же S связывается с exact sealed root в T06.
  КОГДА:
    после окончательной фиксации входов; ДО первого calculate_metadosis.
    Не надо ждать amount worker. При отсутствии verified S T04 ждёт.
  ЧЕМ ВЫПОЛНЯЕТСЯ:
    отдельный протокол агрегирования; точная схема приведена ниже.
    Это не «дешифрование суммы Pedersen» и не уже существующий вызов Rust.

T04. process_ocomp_ready_candidate → calculate_metadosis    [READY]
  INPUT ОТКРЫТО:
    S из T03; E; Green/Red; N
  BRANCH:
    E=0 / N=0 → соответствующий локальный terminal outcome, worker не нужен
    Unknown day type → ошибка/failed outcome
  CALC:
    D0 = floor(S*32/100)
    Green: D=D0;          Q=E
    Red:   D=floor(D0/8); Q=floor(E/8)
    B = min(D,Q)
  OUTPUT ОТКРЫТО:
    gratis_demand=D; gratis_supply=Q; gratis_allocation=B
  NEXT:
    B=0 → локальная terminal-ветка без Lysis
    B>0 → T05, T06, затем T08
  ЗАМЕНА:
    арифметике этой функции скрытие больше не требуется:
    она получает verified S, а не индивидуальные a_i.
    Текущие чтения DayTotals заменяются чтением проверенного результата T03.
~~~

Код: [T00 compute](../../bin/outbe-tee-enclave/src/compute.rs#L118), [возврат из TEE](../../bin/outbe-tee-enclave/src/process.rs#L61), [T01 issue/event](../../crates/core/tribute/src/runtime.rs#L319), [T01 total](../../crates/core/tribute/src/state.rs#L317), [T02 close](../../crates/core/metadosis/src/lifecycle.rs#L206), [durations](../../crates/core/metadosis/src/constants.rs#L13), [T04 первый потребитель S](../../crates/core/metadosis/src/settlement.rs#L111), [T04 формулы](../../crates/core/metadosis/src/settlement.rs#L32).

**Точка замены для основной задачи:** `bump_day_bucket: S += a_i` становится закрытым накоплением; перед `process_ocomp_ready_candidate` добавляется получение и проверка открытого итогового `S`. Следующий код получает обычный uint256-агрегат. Это требует нового admission/aggregation интерфейса, а не замены типа поля на commitment без других изменений.

## Trace: от суммы дня до окончательных коэффициентов

~~~text
T05. build_fidelity_league_snapshot                        [READY, до OCOMP request]
  INPUT:
    owners принятого дня + timestamp + Fidelity
  OUTPUT ОТКРЫТО:
    owner → league l_i; snapshot_root
  NEXT:
    T06 связывает snapshot_root с job; T07 группирует amounts по l_i.
  a_i ДЛЯ ЭТОЙ ФУНКЦИИ НЕ НУЖЕН:
    текущая функция читает тела для перечисления owners;
    выбор owners/лиг можно выполнять по открытым метаданным.
  ВАЖНО ДЛЯ СОХРАНЕНИЯ ДАННЫХ:
    окончательная лига фиксируется здесь, не в T01.
    Данные для группировки a_i по владельцам должны дожить до T07.

T06. build_and_commit_request + RequestBudgetSplit         [после provisional CE seal]
  INPUT:
    verified S,N; exact collection root; snapshot_root;
    E,B,day type; PromisLimit K; oracle/entry prices
  CHECK:
    pre-admission eligible; exact sealed projection;
    совпадение candidate/sealed envelope; targets/receipt
  CALC:
    C0 = E-B
    available = K+C0
    Green: A=min(S-B,available)
    Red:   A=0
    receipt.day_limit = E+A
    сейчас в PromisLimit добавляется C0; A пока не списывается
  OUTPUT ОТКРЫТО:
    JobIntent.authenticated_day_nominal = S
    JobIntent.authenticated_day_count = N
    exact input root, snapshot root, B, D, Q, A, цены, время
  NEXT:
    T07–T14 получают одни зафиксированные параметры.
  ЗАМЕНА:
    проверка exact nominal должна связывать receipt T03 с exact root.
    Формулы бюджета остаются открытыми. B не заменяется на E+A.

T07. fidelity_map → fidelity_reduce                        [первый проход worker]
  INPUT СЕЙЧАС:
    a_i; owner→l_i; ordinal/Tribute ID; два согласованных league observations
  CALC:
    S_check = Σ a_i
    S_l = Σ(a_i для league=l)
    n_l = число Tribute league=l
    N_check = Σ n_l
  OUTPUT СЕЙЧАС:
    FidelityAggregate{S_check,N_check, ordered(l,n_l,S_l)}
  NEXT:
    T08
  ЗАМЕНА:
    по открытым l_i суммируются закрытые [a_i] в [S_l];
    commitments группируются тем же способом;
    закрытые частичные суммы map/reduce не публикуются.
    OpenAggregate(S_l) → проверенные открытые итоги лиг.
    Проверка: все входы распределены ровно один раз; ΣS_l=S; Σn_l=N.
  КОГДА РАСКРЫВАЕТСЯ S_l:
    после snapshot и проверки полного состава групп;
    ДО finalize_fi_fraction_table / T08.
  ПОЧЕМУ S ИЗ T03 НЕ ЗАМЕНЯЕТ S_l:
    один итог дня не содержит распределение nominal между лигами.

T08. compute_fraction_map_from_groups                      [один общий расчёт]
  INPUT ОТКРЫТО:
    S, B, N; упорядоченные (l,S_l,n_l) из T07
  CALC:
    y_l = floor(S_l*M/S)
    y_last += M-Σy_l
    f = floor(B*M/S)
    fmax = 2*f
  OUTPUT:
    shares y_l, populations n_l, f, fmax
  NEXT:
    T09
  СКРЫТАЯ ИНДИВИДУАЛЬНАЯ СУММА НЕ ТРЕБУЕТСЯ.

T09. calc_fraction_distribution_fp                        [общий расчёт]
  INPUT ОТКРЫТО:
    y_l,n_l,N,f,fmax
  CALC:
    одна лига → f_l=f
    несколько лиг:
      n_l,N → policy_tau_fp → tau
      y_l,tau → compute_moments_fp → m,Y,EY,VarY
      m,Y,EY,VarY,f,fmax → предварительные f_l
      W=Σfloor(f_l*y_l/M)
      если W>f: f_l=floor(f_l*f/W)
  OUTPUT:
    предварительная таблица коэффициентов
  NEXT:
    T10
  ВСЕ ВХОДЫ УЖЕ ОТКРЫТЫ. Скрывать корни/деления этого шага не требуется
  для маршрута с раскрытыми S_l. Точная арифметика — в приложении ниже.

T10. compute_fraction_map_from_groups: real normalization  [перед amount pass]
  INPUT ОТКРЫТО:
    S_l, B, коэффициенты T09
  CALC:
    R_projected = Σ floor(S_l*f_l/M)
    если R_projected>B:
      каждый f_l = floor(f_l*B/R_projected)
  OUTPUT ОТКРЫТО:
    окончательная таблица league → f_l
  NEXT:
    T11
  R_projected — расчётная верхняя проекция; это НЕ фактически потраченный G.
  Отдельное списание «резерва лиги» здесь не выполняется.
~~~

Код: [T05](../../crates/core/metadosis/src/ocomp/snapshot.rs#L21), [T06 request](../../crates/core/metadosis/src/ocomp/request.rs#L148), [T06 budget](../../crates/core/metadosis/src/ocomp_budget.rs#L41), [T07](../../crates/core/lysis/src/program_v1/phases.rs#L156), [T08/T10](../../crates/core/lysis/src/program_v1/execute.rs#L392), [T09](../../crates/core/lysis/src/algorithm.rs#L229).

## Trace: закрытые индивидуальные начисления и раскрытие расхода

~~~text
T11. amount_map → calculate_gratis_load                    [каждый Tribute, после T10]
  INPUT:
    ЗАКРЫТЫЙ a_i; ОТКРЫТЫЙ f_(l_i); идентификаторы и league binding
  CALC СЕЙЧАС:
    g_i = floor(a_i*f_(l_i)/M)
    require g_i>0
  OUTPUT ДЛЯ ПРИВАТНОГО ВАРИАНТА:
    ЗАКРЫТЫЙ g_i + C(g_i) + проверка отношения к C(a_i)
  NEXT:
    T12 и T13
  ЗАМЕНА:
    умножение на открытый f — линейное;
    floor и g_i>0 требуют вычисления/доказательства над скрытым witness.
    Простая операция над C(a_i) окончательный g_i не вычисляет.
  КОГДА НУЖЕН WITNESS:
    сейчас, когда f_l уже известны.
    Либо владелец доступен после T10, либо приватный исполнитель сохранил
    необходимые закрытые данные. Commitments без такого исполнителя
    не позволяют автоматически завершить этот шаг.

T12. amount_map → calculate_cost + calc_floor_price        [каждый Tribute]
  INPUT:
    ЗАКРЫТЫЙ g_i; ОТКРЫТЫЕ entry price p_i и tribute price_i
  CALC:
    c_i = floor(p_i*g_i/M); require c_i>0; require p_i>0
    h_i = floor(max(price_i,p_i)*108/100)
  OUTPUT ДЛЯ ПРИВАТНОГО ВАРИАНТА:
    ЗАКРЫТЫЙ c_i, C(c_i), доказательство расчёта;
    ОТКРЫТЫЙ h_i и будущий bucket key
  NEXT:
    T13–T15
  ЗАМЕНА:
    cost floor — закрытое вычисление/доказательство;
    floor price h_i — обычный открытый расчёт, amount ему не нужен.

T13. gratis_summary / prefix + итоговые totals            [после amount pass]
  INPUT:
    закрытые g_i,c_i; открытые B и точный input/output manifest
  CALC:
    [G] = Σ[g_i]
    [T_cost] = Σ[c_i]
    проверка всех individual g_i>0,c_i>0 и правильности T11/T12
  OUTPUT ДЛЯ ПРИВАТНОГО ВАРИАНТА:
    OpenAggregate(G), OpenAggregate(T_cost)
    доказательство связи итогов с commitments всех выходов
  NEXT:
    budget check G<=B; T14 формирует выходы; T15 возвращает B-G
  КОГДА РАСКРЫВАЕТСЯ:
    после проверки полного набора начислений;
    до публичного закрытия бюджета / finalizer.
  ЧТО МЕНЯЕТСЯ В КОДЕ:
    сейчас prefix-функции читают индивидуальные g_i и числовые частичные
    остатки. Их нельзя оставить как есть с C(g_i) вместо числа.
    При доказанных g_i>=0 и точном G условие G<=B уже означает,
    что каждый префикс помещается в B. Это позволяет заменить проверки
    раскрытых prefix-чисел доказательством общего расхода и положительности.
    Точные valid-result суммы сохраняются; интерфейс проверки меняется.

T14. output_finalize → shuffle_owners / shuffle_buckets    [формирование выходов]
  INPUT:
    commitments индивидуальных g_i,c_i,a_i;
    owner, Tribute ID, league, currencies, p_i,h_i, exclude flag, time
  OUTPUT ДЛЯ ПРИВАТНОГО ВАРИАНТА:
    Nod с C(g_i),C(c_i) и привязкой к исходному C(a_i)/проверке;
    contributor с C(a_i), если exclude=false;
    bucket/order/count/root из открытых метаданных
  ДОПОЛНИТЕЛЬНЫЙ АГРЕГАТ ТЕКУЩЕГО КОДА:
    H_eligible = Σ(a_i при exclude=false)
    может накапливаться закрыто уже с T01: exclude известен при приёме.
    Раскрывается с итогами перед T15, проверяется связь с contributor root.
  ЗАМЕНА:
    текущие NodAction/Contributor содержат индивидуальные plain amounts;
    их encoding, root commitments и проверки заменяются согласованно.
    Bucket shuffle сортирует ключи/ID; amount-агрегат для него не нужен.

T15. finalizer                                            [до принятия результата]
  INPUT ОТКРЫТО:
    S,N,B,G,T_cost,H_eligible; roots/counts; proofs результата
  CHECK:
    exact input/output coverage; связь totals с committed records;
    G<=B; H_eligible<=S
  CALC:
    U = B-G
  OUTPUT ОТКРЫТО:
    conservation totals, roots, unused_lysis=U, carry_over_credit=U
  NEXT:
    T16
  ЗАМЕНА:
    сейчас stream_result_chunks повторно суммирует plain g_i,c_i,a_i.
    В приватном варианте это проверка proofs агрегатов/выходов,
    а не повторное чтение скрытых полей как обычных uint256.
    Вычитание B-G остаётся открытым.

T16. activation                                           [сертифицированный результат]
  INPUT ОТКРЫТО:
    результат T15; frozen receipt{A}; bindings/targets
  EFFECT:
    установить Nod/contributor generation;
    вернуть U в PromisLimit;
    списать ранее зафиксированный A и выдать Desis brief;
    завершить retirement Tribute
  ЗАМЕНА:
    числовой возврат лимита выполняется по открытому U.
    Он НЕ требует открытия какого-либо отдельного a_i,g_i,c_i.
  ОТДЕЛЬНАЯ ВЕТКА:
    если полного A к моменту списания нет → ошибка/rollback activation;
    это не дефицит D>Q из T04 и не пересчёт коэффициентов Lysis.
~~~

Код: [T11/T12](../../crates/core/lysis/src/program_v1/phases.rs#L358), [floor price](../../crates/core/lysis/src/constants.rs#L3), [prefix](../../crates/core/lysis/src/program_v1/phases.rs#L493), [prefix leaf](../../crates/core/lysis/src/program_v1/phases.rs#L607), [T14 выходы](../../crates/core/lysis/src/program_v1/phases.rs#L637), [contributors/buckets](../../crates/core/lysis/src/program_v1/phases.rs#L759), [T15 пересчёт amounts](../../crates/core/lysis/src/program_v1/finalizer.rs#L449), [T15 возврат](../../crates/core/lysis/src/program_v1/finalizer.rs#L241), [T15 eligible bound](../../crates/core/lysis/src/program_v1/finalizer.rs#L948), [T16](../../crates/core/metadosis/src/ocomp/activation.rs#L358).

## Когда и какой агрегат открывается

| Агрегат | Когда его можно получить | Последний момент перед потребителем в выбранном маршруте | Кому передаётся | Что сохраняется закрытым |
|---|---|---|---|---|
| `S=Σa_i` | После фиксации окончательного набора дня | **T03, до первого READY calculate_metadosis** | Metadosis, sealed projection/JobIntent, Lysis coefficient calculation | Все индивидуальные `a_i` |
| `S_l=Σ(a_i для l)` | После T05 snapshot и группировки закрытых входов | **T07, до finalize_fi_fraction_table** | Расчёт долей лиг и обеих нормализаций | Индивидуальные `a_i` внутри групп |
| `G=Σg_i` | После известных коэффициентов T10 и закрытых floor T11 | **T13, до публичного budget finalization** | Проверка потребления, finalizer, PromisLimit | Все индивидуальные `g_i` |
| `T_cost=Σc_i` | После T12 | **С итогами T13/T15** | Nod conservation/activation | Индивидуальные `c_i` |
| `H_eligible=Σ(!exclude_i ? a_i : 0)` | Подмножество известно при admission; окончательно после фиксации набора | **Не позднее T15** | Contributor conservation/activation | Индивидуальные contributor nominal |

Это точки раскрытия **предлагаемого** приватного маршрута. Сейчас суммы и индивидуальные поля в этих функциях открытые. Для `S` задано требование раскрытия; `S_l,G,T_cost,H_eligible` перечислены отдельно, чтобы решение о каждом выходе было явным.

**Числовая проверка дефицита:** одна лига; приватные nominal 400 и 600; открываем `S=1000`; Green `E=100` → `D=320,Q=100,B=100` → `f_l=100000` → закрытые `g_1=40,g_2=60`. При `p_i=2` (wire `2*M`) закрытые costs 80 и 120. Открытые итоги: `G=100,T_cost=200,U=0`. Внешнему расчёту бюджета нужны 1000, 100, 200 и 0, а индивидуальные 400/600, 40/60, 80/120 не являются публикуемыми выходами этого примера.

## Что конкретно стоит внутри OpenAggregate

Это алгоритмический контракт для T03, а не заявление о готовом протоколе ротации.

Предположения этого варианта: проверяемые вклады, защищённая доставка долей и число скомпрометированных участников ниже порога. Каждое сложение выполняется в одной согласованной sharing-эпохе; доли разных составов нельзя просто сложить без переноса в общее представление.

Для Pedersen в одной допустимой координате:
`C_i=a_i*P+r_i*Q`, `C_S=ΣC_i`. Сложение даёт commitment суммы, но не само число. Для проверки открытия нужны сумма значений и суммарная случайность. Это [свойства Pedersen commitments](https://www.zkdocs.com/docs/zkdocs/commitments/pedersen/). Механизм распределённого владения значением — отдельный слой, например [verifiable secret sharing](https://www.cs.cornell.edu/courses/cs754/2001fa/129.PDF).

~~~text
Во время T01, для каждого принятого Tribute:
  публично:
    C_i; proof происхождения/range;
    связывание secret shares с тем же C_i; ID/day и правило включения
  участник j приватно получает:
    [a_i]_j, [r_i]_j
  участник j обновляет:
    [S]_j += [a_i]_j
    [R]_j += [r_i]_j
  сеть обновляет:
    C_S += C_i
    count/root ровно того же принятого набора

Во время T03:
  проверяется binding к одному окончательному набору;
  порог участников публикует проверяемые доли АГРЕГАТОВ [S]_j,[R]_j;
  из них восстанавливаются S,R;
  проверяется C_S == S*P + R*Q;
  наружу выдаётся S с проверкой и binding.
  Доли каждого a_i и каждого r_i не открываются.
~~~

Для линейной secret sharing сумма долей одного участника является долей суммы: если `f_i(0)=a_i`, то `F(x)=Σf_i(x)` и `F(0)=S`. Поэтому в T03 восстанавливается `F(0)`, без восстановления каждого `f_i(0)`. Это применение алгебры [Shamir secret sharing](https://www.zkdocs.com/docs/zkdocs/protocol-primitives/shamir/) к данному trace, а не отдельная реализация из репозитория.

Для **одного S** накопитель имеет постоянное число секретных координат на участника; не требуется хранить миллиард ciphertexts именно ради расшифровки дневной суммы. Но приём, проверка каждого вклада и commitments/manifests остаются работой на N входов. И это не разрешение выбросить все индивидуальные закрытые данные: T07 ещё должен сгруппировать их по поздним лигам, T11 — выполнить индивидуальные floor. Где и в каком виде эти данные доживают до T11 — отдельное требование этого же trace.

При смене валидаторов передаются/обновляются закрытые суммарные доли с проверкой неизменности `C_S` и набора. Формула накопления сама не обеспечивает безопасный handoff, защиту от накопления старых shares, доступность или связь данных разных эпох. Эти части должны быть реализованы до заявления о работающем 50-часовом цикле. DKG сам по себе этот handoff и передачу вкладов не выполняет.

**uint256 без ошибки modulo:** формулы одной координаты действуют в поле. Нельзя молча поместить полный uint256 в scalar меньшего порядка и считать результат обычной целой суммой. Например, для N≤10^9 можно разбить `a_i` на четыре 64-битных limb: сумма каждой limb меньше `2^94`. Поле подходящего размера позволяет накопить четыре суммы без wrap, затем восстановить целое S с переносами и проверить `S≤2^256−1`. Потребуются commitments/range proofs и связь limb с исходным amount; это конкретизация числового представления, а не дополнительное раскрытие индивидуальных сумм. Проверка переполнения на admission также должна сохранить правило текущего `bump_day_bucket`.

Альтернативная реализация интерфейса T03 через TEE держала бы accumulator внутри enclave и выдавала итог с attestation/binding. Она меняет доверительную границу: enclave знает индивидуальные inputs. В текущем `process_one` такого закрытого accumulator нет — сейчас enclave возвращает индивидуальные amounts наружу.

## Где достаточно линейной операции, а где нужен proof скрытого расчёта

| Узел | Только операции над commitments достаточны? | Недостающая операция |
|---|---|---|
| T01: сложить commitments | Да, для получения `C_S` | Для числового S всё равно нужен T03 |
| T03: получить S | **Нет** | Открытие проверяемого агрегата из закрытых данных |
| T04, T06: demand/min/auction | Скрытая операция не требуется при открытом S | Обычная uint256-арифметика |
| T07: сгруппировать и сложить commitments | Да, для `C_(S_l)` | Открытие числовых сумм лиг перед T08 |
| T08–T10: shares/roots/normalization | Скрытая операция не требуется при открытых агрегатах | Текущий fixed-point алгоритм |
| T11: `a_i*f_l` | Да, commitment произведения на публичный scalar | Целочисленный floor и положительность g_i |
| T12: `g_i*p_i` | Да, commitment произведения на публичный scalar | Целочисленный floor и положительность c_i |
| T13: сложить C(g_i), C(c_i) | Да, для commitments итогов | Проверенные числовые G/T_cost для финализации |
| T14: сформировать приватный Nod | Commitments можно записать | Проверки вычисления и согласованный новый encoding |
| T15: `U=B-G` | При открытых B,G — обычная операция | Связь G с private output records должна быть доказана |

Точные statements для двух floor, где `a_i,g_i,c_i,u_i,v_i` — закрытые witness:

~~~text
a_i * f_l = M*g_i + u_i;    0 <= u_i < M;    g_i > 0
g_i * p_i = M*c_i + v_i;    0 <= v_i < M;    c_i > 0
~~~

Оба statement связываются с входными/выходными commitments, owner/day/Tribute ID и окончательной таблицей T10. Полноразрядная проверка исключает арифметику с незамеченным wrap. Proof даёт проверку этих равенств и диапазонов; сам verifier не получает witness. Существование range proofs для скрытых величин описано, например, в [Bulletproofs](https://crypto.stanford.edu/bulletproofs/); конкретный proof backend для этого trace не выбран.

Доказательство, поданное при T01, может подтвердить исходный amount и nominal. Для фиксированных итоговых `g_i,c_i` коэффициенты ещё неизвестны: T11/T12 требуют позднего вычисления владельцем или приватным исполнителем. Простой перенос всех вычислений на погашение Nod не удовлетворяет сегодняшним T13/T15, которым точный расход нужен при закрытии дня.

## Проверка trace

Пути найдены через codebase-memory, Tier 2, project `Users-sakor-outbe-io-outbe-chain`, generation `2026-09-07T14:04:40Z`. Существенные зависимости проверены по исходникам; coverage проверена для всех приведённых source paths. `tribute/state.rs` с изменившимися metadata прочитан напрямую; отмеченная строка `process.rs:516` тоже. Generic Rust call edges графа могут ошибочно связывать одноимённые методы; такие связи не использованы как доказательство.

Граница: offer nominal → приём → нормальный положительный OCOMP request → Lysis outputs/activation; показаны локальные нулевые ветки и две разные нехватки бюджета. Это не аудит Oracle/Fidelity internals, погашения Nod или всего recovery lifecycle. Новая приватная схема не реализована и не измерена этим документом. Тесты из предыдущего разбора не выдаются за проверку предложенных замен.

## Приложение: точная арифметика T08–T10

Источник входной обвязки: [compute_fraction_map_from_groups](../../crates/core/lysis/src/program_v1/execute.rs#L392). Лиги сортируются по ID; ниже `k` — число непустых групп.

```text
(S_l, S)              → y_l = floor(S_l*M/S)
Σ y_l                 → недостающий до M остаток прибавляется последней лиге
(B, S)                → f = floor(B*M/S), fmax = 2*f
(n_l, N)              → policy_tau_fp → tau[0..k]
(y_l, tau)            → compute_moments_fp → m, cumulative Y, EY, VarY
(m, Y, EY, VarY,f,fmax)→ calc_fraction_distribution_fp → предварительные f_l
(f_l, y_l, f)         → первая нормализация → f_l
(f_l, S_l, B)         → вторая нормализация → окончательные f_l
```

Для одной группы функция сразу возвращает `f`. Для нескольких групп в [algorithm.rs](../../crates/core/lysis/src/algorithm.rs#L95) выполняется следующая арифметика:

1. Внутренние веса `tau_i` строятся из `(i−0.5)^(1/5)` и `min(n_i^(1/10), n_(i−1)^(1/10))`, каждый корень — целочисленный fixed-point. В обозначении кода populations индексируются с нуля, `i=1..k−1`. Нулевые populations/root divisor используют fallback от `k` и `N`. Крайние веса — 20% и 80% суммы внутренних весов, с отдельным округлением.
2. `m_j = floor(tau_j*M / Σtau)`, либо нули при нулевой сумме весов. `Y` — накопленные `y_l`, с `Y_0=0`, `Y_k=M`. Из-за округления сумма `m_j` может быть меньше `M`.
3. `EY = Σ floor(m_j*Y_j/M)`; `EY2 = Σ floor(m_j*Y_j²/M²)`; `VarY = max(0, EY2−floor(EY²/M))`.
4. `beta_num = floor(f*M/fmax)−EY` (доля равна нулю при `fmax=0`). Для каждого `j` вычисляется знаковый `beta_term = trunc(beta_num*(Y_j−EY)/VarY)`, либо 0 при нулевой дисперсии.
5. Для лиги `i=1..k`: `T_i = Σ(j>=i) trunc(m_j*(M+beta_term_j)/M)`; предварительный `f_i=max(0,trunc(fmax*T_i/M))`.
6. Первая нормализация: `W=Σ floor(f_l*y_l/M)`. Если `W>f`, каждый `f_l` заменяется на `floor(f_l*f/W)`.
7. Вторая нормализация: `R=Σ floor(S_l*f_l/M)`. Если `R>B`, каждый `f_l` заменяется на `floor(f_l*B/R)`.

`R` — защитная проекция по группам. В коде на этом шаге **нет отдельного выделения или списания «резерва лиги»**. Реальный расход определяется ниже как сумма индивидуальных `g_i`.

Из этого следует: сравнения `D <= Q` недостаточно для сохранения текущей политики. Даже когда лимита хватает, соотношения `S_l/S` нужны для распределения между лигами. При дефиците также нужна числовая величина `B/S`, а не один бит «не хватило».
