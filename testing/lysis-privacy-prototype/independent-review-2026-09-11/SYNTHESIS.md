# Три независимые проверки: теория и полнота схемы без TEE

Дата: 2026-09-11. Проверялись неизменённые документы и исходники из [INPUT_MANIFEST.json](INPUT_MANIFEST.json), HEAD `177a72ddbea9f2e52eef094405481292ecd56046`, рабочее дерево с ранее существовавшими локальными изменениями. В этой работе production и проверяемые документы не менялись.

**Ответ на (a): основные конструкции и арифметика теоретически состоятельны при перечисленных предпосылках. Ответ на (b): нет, полнота не подтверждена; есть конкретные замечания.** Полная malicious/mobile композиция, безопасный distributed prover и прохождение ресурсных ограничений не доказаны.

Каждый из трёх рецензентов независимо прочитал весь исследуемый маршрут, проверил Q1–Q7/C01–C14 и исследовал первичные источники. Первые отчёты подготовлены без чтения чужих или прежних review conclusions. Ниже результаты сведены по доказательствам, а не голосованием. Если находка отсутствует в другом отчёте, это не означает её опровержение.

## 1. Исходные независимые заключения

| Рецензент | Теория | Полнота | Основные собственные находки |
|---|---|---|---|
| [A](REVIEW_A.md) | Условно подтверждены математические отношения и направления | Не подтверждена | Credis external writers, внутренний Lego blinder, inclusion timestamp Fidelity |
| [B](REVIEW_B.md) | Направление состоятельно; полная композиция не доказана | Не подтверждена | Contributor payout Intex, Credis external writers |
| [C](REVIEW_C.md) | Условно подтверждены блоки и арифметика | Не подтверждена | Внутренний Lego blinder, supply invariant, Fidelity query API; замечание об outputs уточнено после проверки |

[Дополнение C](REVIEW_C_ADDENDUM.md) подготовлено после сохранения первого отчёта и уточняет два его собственных пункта. Первые три отчёта не переписаны под общий вывод.

## 2. Подтверждённые пробелы

### IR-01 — High: публичная выплата Intex остаётся потребителем скрываемого nominal

**Источник находки:** B-01; при сведении повторно прочитан текущий `pay_contributor_batch`.

[IntexFactory runtime](../../../crates/core/intexfactory/src/runtime.rs), `pay_contributor_batch:460`, использует certified contributor leaf и считает:

```text
payout_i = floor(round.amount * leaf.nominal / eligible_nominal_total)
transfer_balance(factory, leaf.owner, payout_i)
```

Расчёт расположен на строках 495–499, публичный перевод — 515–516. Замена contributor `nominal` на C(a) лишает этот consumer входа. Раскрытие a в payout возвращает исходную утечку; публичная пропорциональная выплата тоже несёт сведения об a. Например, если все contributors eligible и `round.amount=S=eligible_nominal_total`, выплаты равны nominal. Это проверенный арифметический контрпример, не исполненная транзакция и не утверждение о текущем deployed pot.

Основной research честно оставляет дальнейший Intex вне аудита. Но этот конкретный consumer продолжает использовать результат Lysis; его нельзя исключить из вывода о конфиденциальности всего денежного потока.

**Что необходимо дописать:** конечный интерфейс contributor payout — кто считает и доказывает пропорции/округление, какие commitments и private data сохраняются, куда зачисляется результат, как обрабатывается offline owner и остаток. Либо требуется отдельное решение о поддержке/публичности этого пути. Аудит всей экономики Intex для закрытия данного интерфейса не требуется.

### IR-02 — High: модель аккаунта не покрывает изменения Gratis/Fidelity без владельца

**Источники находки:** A-01 и B-02 независимо; при сведении повторно прочитаны callers и writers.

[CredisFactory runtime](../../../crates/core/credisfactory/src/runtime.rs) вызывает `release_to_eoa` после settlement (`:245`) и `burn_pledged_with_fidelity` при `void_position` (`:266–294`). [Gratis runtime](../../../crates/core/gratis/src/runtime.rs), `release_to_eoa:423`, читает и обновляет тот же liquid balance и pledged state; collateral burn также меняет Fidelity. Эти переходы не предполагают, что владелец сейчас строит proof.

Контрпример: owner отправил Tribute и ушёл offline; его collateral position истекла до Fidelity snapshot. Пропуск forced Out меняет будущую лигу; сохранение прежнего вызова оставляет TEE; требование нового wallet proof останавливает автоматическое исполнение. При стороннем погашении кредита executor также не знает opening старого `H(balance limbs, salt)`, чтобы сформировать новый account commitment по предложенной модели.

**Что необходимо дописать:** полный перечень writers одного account — pledge, unpledge, consume ticket, release, forced burn; их authority и conservation liquid/pending/pledged; достаточное закрытое состояние для offline mutation; создание proof/root и доставку нового recoverable witness владельцу. Доступность shares для чтения league сама по себе не определяет протокол записи нового баланса/истории.

### IR-03 — Medium, критично для приватности adapter: в Lego нужен отдельный внутренний blinder

**Источники находки:** A-02 и C-01 независимо; при сведении повторно прочитан тот же pinned upstream source.

`create_random_proof_incl_cp_link` принимает `v` и `link_v` от caller; сама функция генерирует только Groth16 `r,s`. Внутренний публичный элемент равен `D=a*K+v*J`. При известном `v=0` получается `D=a*K`, по которому можно проверять догадки a, хотя внешний `link_d=a*G+link_v*H` ослеплён. [Dock prover.rs:32–47 и :358–379, commit 224f195b](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/prover.rs#L32).

**Что необходимо дописать:** prover adapter сам выбирает независимый свежий `v_internal` из CSPRNG для каждого proof; внешний `link_v=r_external` задаётся отдельно. Развести имена с библиотечными `r,s`, учесть все публикуемые commitments. Нужен полный пример вызова и проверки соответствующего контракта.

Это пропуск безопасного рецепта использования API. Он не опровергает LegoSNARK, не означает дефект upstream при корректном использовании и не является уже исполненным exploit Outbe. Full proof с намеренно плохим v в этой проверке не создавался.

### IR-04 — Medium: не определён производитель точного времени нового Fidelity state

**Источник находки:** A-03; при сведении повторно прочитаны acquisition/sale writers.

[GratisFactory mint/mine_coen](../../../crates/core/gratisfactory/src/runtime.rs), `:137–160`, берут `storage.timestamp()` при исполнении. [CohortState](../../../bin/outbe-tee-enclave/src/fidelity.rs), `cohort_in:156` и `cohort_out:174`, записывает это время в acquisition/sold history.

Если кошелёк заранее доказал новый private state со временем T, а включение произошло в T+3, нельзя одновременно сохранить прежнее exact-time правило и молча заменить timestamp внутри доказанного состояния. Clock binding как пункт будущего теста не определяет producer/consumer этого входа.

**Что необходимо дописать:** конкретный контракт. Например, wallet доказывает amount/order/update payload, а runtime канонически добавляет публичный inclusion timestamp к проверенным cohort commitments и формирует root. Для полностью скрытого времени нужен иной способ завершить authenticated state; logical-time вариант является отдельным изменением правил. Это решаемый интерфейсный пробел, не ошибка floor/12-comparison identity и не доказательство невозможности заранее строить proofs вообще.

### IR-05 — Уточнение supply: есть дополнительная эмиссия Promis → Gratis

**Источник:** C-03 и его последующее bounded уточнение. Родитель дополнительно прочитал `mine_gratis` и публичный dispatch.

Для строго ограниченного профиля `zero genesis + current-source Nod-only + каждый day budget расходуется однократно` проблема global overflow разрешается простым bound:

```text
WorldwideDay = u32
minted_from_day < 2^176
cumulative/live Gratis supply < 2^32 * 2^176 = 2^208 < 2^256
```

**В этом профиле отдельный MPC range check global supply только ради overflow не требуется.** Символический пример `T_old=U256_MAX` не доказывает достижимую ошибку допустимой истории. Источник day type и условия conservation/initial state подробно указаны в [дополнении C](REVIEW_C_ADDENDUM.md).

Однако существующая система не ограничена этим профилем: [PromisFactory `mine_gratis:69–82`](../../../crates/core/promisfactory/src/runtime.rs) сжигает Promis и вызывает тот же GratisFactory mint, включая Fidelity acquisition. [Публичный `mineGratis` dispatch:42–58](../../../crates/core/promisfactory/src/precompile.rs) делает этот путь доступным. Он требует собственного контракта перевода в fixed18, авторизации/приватности и вклада в общий mint bound. Экономика всех источников Promis здесь не аудировалась; достижимый overflow не заявляется.

**Что необходимо дописать:** профиль всех эмиссий/initial imports, global conservation и судьбу `totalSupply()`/событий. Можно ограничить конкретный прототип Nod-only, но это не утверждает совместимость со всей действующей системой. Маленький дневной S сам по себе не описывает все mint sources.

### IR-06 — Low: судьба owner-authorized Fidelity index queries не указана

**Источник:** C-04; при сведении повторно прочитаны runtime и dispatch.

[Fidelity `query_index_at:155–172`](../../../crates/core/fidelity/src/runtime.rs) и [precompile dispatch](../../../crates/core/fidelity/src/precompile.rs) возвращают owner-authorized RCFI через enclave. Публичный league slot и его 12-comparison вычисление не заменяют эти методы.

**Что необходимо дописать:** сохраняется ли запрос как локальный расчёт кошелька по recoverable history, как private-output сетевой запрос либо меняется/исключается API. Это небольшой, но конкретный пункт полноты отказа от TEE; отсутствие такого решения не опровергает основной aggregate method.

## 3. Что не принято как новая ошибка

- **C-02 про singleton S_l снят как blocker.** Пользователь разрешил S/S_l, а trace уже определяет privacy относительно разрешённых публичных результатов. Это решение не пересматривается. Пример с `G−F` относится к уже открытому выбору forfeit policy; отдельной атаки на VSS здесь нет. Рецензент зафиксировал исправление своей классификации в addendum.
- **Global U256 overflow не объявлен достижимым.** Для ограниченного Nod-only профиля найден sufficient lifetime bound; для всей системы нужен учёт реального второго mint source. Это требования conservation/совместимости, не доказанный production defect.
- **512 МБ и миллиард записей не объявлены невозможными.** Полных запусков нет; это непроверенные hard requirements. Отдельные 398/497 МБ и component encoding throughput не доказывают их выполнения.
- **Необъявленный malicious/mobile theorem не переименован в арифметическую ошибку.** Стандартные VSS/DPSS/MPC семейства имеют собственные модели. Требуется выбрать и доказать композицию конкретного протокола; текущие документы честно оставляют это открытым.

## 4. Что все три проверки поддерживают

| Область | Подтверждённый результат | Не следует из результата |
|---|---|---|
| Источник/числа | Current codec и формула дают a<2^104, дневной S<2^134 для 1e9 и <2^136 для полного u32 count | Произвольный uint256 input profile, стоимость/пожизненная история и другие mint paths не получают этот bound автоматически |
| P_link | Native Baby/Groth16 и Lego CP_link подходят как конструкции same-value binding с корректными encodings/setup/blinders | Soundness фактического ещё не созданного полного circuit, безопасность полного adapter и ≤512 МБ |
| S/S_l | Парные Pedersen shares, same-C binding, accepted-set completeness и scalar interpolation дают точный численный aggregate | Commitment alone, signing DKG или per-record receipts без common coverage недостаточны |
| Repair/rotation | Weighted sub-sharing и Lagrange combine сохраняют a/r | Malicious completion, mobile secrecy, erasure и сетевой SLA собственной адаптации не доказаны одной алгеброй |
| Lysis/Nod | После S_l kernel может быть публичным; g18=a6*f6*10^6, c18=a6*f6*p6; Nod сохраняет исходный C(a) | Соседние payouts, invalid/zero cost policy и все сторонние mutations автоматически не покрываются |
| Fidelity | Условное threshold identity сохраняет nested floors; поиск по 4096 slots — до 12 сравнений | Скрытые zero guards, широкие числа, input binding, state mutation, exact queries и private distributed proving не исчезают |
| Forfeit | Residual groups и linked private debit могут обслужить разрешённый final refund без online owner | Произвольный partial pass не получается из одного group total; disclosure/timing policy ещё нужна |
| Масштаб | Формулы bytes, 36.5N retained-record moves, 5555.56/11574.07 admissions/s и 3906250 задач по 256 records арифметически согласованы | Это не actual DB/DA/network bytes, worker latency или измеренная пропускная способность |

Ни одна проверка не дала основания вернуть SEAL в shortlist или сделать P-384 обязательным для текущего source profile.

## 5. Что требуется до вывода о полном решении

1. Довести trace до обязательных Intex payout и всех writers Gratis/Fidelity, включая Promis conversion; согласовать границы поддержки этих путей.
2. Уточнить полный prover adapter, время state transitions, recovery после внешнего обновления, supply/conservation и Fidelity read API.
3. Зафиксировать оставшиеся protocol choices: adversary/committee и erasure model, malicious private repair/MPC/public verification, source bounds/privacy, payment18, zero/cost policy, разрешённый forfeit, wire/VK/DA/replay.
4. Выполнить уже намеченные полные P_link A/B и lifecycle/scale проверки. Ни один из пунктов выше не заменяется component benchmark.

IR-03 можно исправить в рецепте adapter без смены экономической модели. IR-01/IR-02 требуют новых строк trace с конкретными input/output/authority и storage; просто добавить их названия в список недостаточно. IR-04/IR-06 требуют выбранного интерфейса. IR-05 требует явного профиля эмиссии и conservation argument.

## 6. Проверки и границы evidence

- Все три рецензента завершили первоначальные независимые отчёты; сбоев инфраструктуры вместо verdict не засчитано.
- Заморожены 8 research inputs и 24 исходника. Рецензенты проверяли существенные функции напрямую; дополнительные consumers зафиксированы в отчётах. Совпадение hashes не заявляется полным аудитом codebase.
- Повторялся исходный arithmetic script: 137 117 случаев. A/B отдельно выполнили по 200 небольших scalar resharings; A проверил 1000 normalization cases; C сделал toy redistribution. Эти результаты не складываются в фиктивное число независимых security tests.
- При сведении повторно прочитаны существенные новые consumers, точный pinned Lego prover и timestamp/query writers. Runtime exploit, новый полный proof и malicious distributed execution не запускались.
- Прежние measurement files проверены по scope: source-only peak398049280 B; отдельный wide-component peak497418240 B; worker file сам обозначен `kernel_and_proposed_encoding_only_not_worker_tps`.
- Graph MCP недоступен. Использован exact-source fallback; generation/coverage графа, deployment status и физическое secure erasure не подтверждались.

[BASELINE_VALIDATION.json](BASELINE_VALIDATION.json) фиксирует базовую сверку; [REVIEW_EVIDENCE.json](REVIEW_EVIDENCE.json) — итоговые hashes, состав reviewers, дополнительные источники и пределы проверки. Первичные papers/specs/pinned libraries перечислены рядом с соответствующими утверждениями в трёх исходных отчётах.
