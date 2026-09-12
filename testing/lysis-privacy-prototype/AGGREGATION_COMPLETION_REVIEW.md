# Проверка завершённости исследования Q2/Q3/Q6

Историческая узкая проверка агрегирования, выполненная до трёх независимых full-route reviews. Её алгебраические выводы сохраняются; указанные ниже строки/хеши относятся к прежнему snapshot. Текущие замечания и исправления, включая новый retention consumer Intex, — [REVIEW_REMEDIATION.md](REVIEW_REMEDIATION.md); текущая полнота/gates — [RESEARCH_COMPLETION_REVIEW.md](RESEARCH_COMPLETION_REVIEW.md).

Дата: 2026-09-11. Проверены [нормативное описание R00–R14/Q2/Q3/Q6](PROTOCOL_TRACE_AND_REQUIREMENTS.md), [приложение агрегации](DEEP_RESEARCH_AGGREGATION.md) и §§5, 6, 10 [основного отчёта](DEEP_RESEARCH_IMPLEMENTATION.md). Это ограниченный review исследовательского результата; production implementation и benchmarks в задачу не входили.

**Статус: существенный пропуск найден и исправлен в ходе проверки; исправление перечитано и проверено. В проверенном scope исследование можно считать завершённым как анализ применимости и план дальнейших проверок. Готовность денежного протокола к внедрению, malicious/mobile security composition и требуемая производительность не доказаны и отчётом не заявлены.**

## 1. Найденный пробел: per-record receipts не дают общих держателей агрегата — исправлен

В исходном тексте переход от `Q−f−d≥t` receipts на каждую запись к локальному `s_j=Σ_iF_i(j)` пропускал требование полного набора долей у одних и тех же исполнителей. Это ошибка в предусловии transcript, а не только вопрос скорости.

Собственный контрпример: `n=4,t=2,f=1,Q=3`; receipts четырёх записей имеют множества `{1,2,3}`, `{1,2,4}`, `{1,3,4}`, `{2,3,4}`. Если node 4 withholding, каждая запись имеет минимум две честные доли, но ни один честный держатель не имеет все четыре записи. Без приватного repair ни один из них не может вычислить показанный полный aggregate share. Пример не выбирает параметры Outbe.

В исправленном тексте появились:

- общий `Ready(finalRoot,epoch,transcriptRoot,coverageRoot)` перед открытием и запрет суммирования отсутствующих долей: [основной отчёт, строки 176–201](DEEP_RESEARCH_IMPLEMENTATION.md), [приложение, строки 59–65](DEEP_RESEARCH_AGGREGATION.md);
- отдельные counting conditions конфиденциальности, сохранности и достижимости quorum: `f<t`, `Q−f−d≥t`, `Q≤n−f−d_ingress`; их совместная консервативная граница `n≥t+2f+d+d_ingress`: [приложение, строки 47–55](DEEP_RESEARCH_AGGREGATION.md);
- аналогичный complete-coverage gate для S_l, residual creation и debit updates; до него dependent stage ждёт repair и не удаляет admitted rights: [приложение, строка 101](DEEP_RESEARCH_AGGREGATION.md).

Эти неравенства — счётные условия выбранного baseline, не самостоятельное доказательство network/consensus liveness. Если f участников не подписывают, нельзя выбирать Q или K, собрать которые можно только с их помощью.

Независимый primary anchor: hbACSS отделяет completeness — получение корректных shares всеми честными исполнителями — от наличия восстанавливающего подмножества; §VI прямо рассматривает необходимость recoverable shares для последующих линейных комбинаций. Это поддерживает найденное различие, но не делает hbACSS готовой Outbe реализацией. [Yurek et al., hbACSS, §§II.B, VI](https://eprint.iacr.org/2021/159.pdf).

## 2. Явный fallback redistribution — алгебра подтверждена

Перечитан новый [§2.4 приложения, строки 79–103](DEEP_RESEARCH_AGGREGATION.md). Для t_old старых verified helpers H_i, их различных индексов и Lagrange coefficients λ_h:

```text
E_i(h) = Σ_k h^k A_i,k
U_h(0) = λ_h y_i,h
V_h(0) = λ_h z_i,h
B_h,0 = U_h(0)G + V_h(0)H = λ_h E_i(h)
A'_i,0 = Σ_h B_h,0 = C_i
```

Из `Σ_hλ_h y_i,h=a_i` и аналогичного равенства для r_i следует сохранение **того же secret и opening**, а не только их публичного group image. Получатель проверяет helper deals и складывает их privately; ни одному координатору не требуется scalar reconstruction. Полиномы helpers имеют новый degree t_new−1 и свежую случайность. H_i может различаться между записями, но итоговые shares должны относиться к одному new roster/profile/manifest.

Стандартный sub-sharing/Lagrange-combine argument изложен в [Groth, Non-interactive distributed key generation and key resharing, §2.5](https://eprint.iacr.org/2021/339.pdf). Двойные Pedersen polynomials и проверка weighted constant commitment — явная адаптация отчёта, а не утверждение о готовой реализации Groth с денежными inputs. Точные условия old threshold существенны и в официальном [Kyber resharing API: OldThreshold](https://github.com/dedis/kyber/blob/master/share/dkg/pedersen/dkg.go), где отдельно указан риск downgrade при неправильном числе старых deals.

Исправление достаточно конкретно для research transcript: incomplete attempt не final; старый state сохраняется; retry меняет authenticated attempt/H_i и random polynomials; новые shares принимаются после проверки actual data и полного coverage. Стоимость `O(t_old·n_new)` pair transfers/record явно вынесена сверх исходного `64n` upload: [приложение, строки 99–103](DEEP_RESEARCH_AGGREGATION.md), [основной отчёт, строка 219](DEEP_RESEARCH_IMPLEMENTATION.md).

**Граница подтверждения:** корректность этой алгебры не даёт автоматической защиты от mobile corruption, утечки channel keys, накопления исторических shares или бесконечного withholding. Выбор конкретного malicious handoff/recovery protocol, условий erasure и execution model остаётся следующим этапом; отчёт это прямо сохраняет. Достаточное число актуальных честных helpers необходимо, а timeout сам по себе не восстанавливает данные.

## 3. Q6: residual claim/forfeit — существенной новой ошибки не найдено

Проверены три необходимых условия:

1. Начальная свёртка относится к полному issued set и группам одинаковых coefficients/lifecycle contexts. Удалять individual shares можно только после покрытия остальных consumers: [основной отчёт, строки 266–282](DEEP_RESEARCH_IMPLEMENTATION.md).
2. Claim debit должен иметь тот же amount, что расходуемый Nod; fresh blinding допустим лишь с equality proof. Debit availability, spent marker, private accounting и residual version меняются атомарно. Теперь common-coverage contract явно распространяется и на debit updates: [основной отчёт, строки 278–280](DEEP_RESEARCH_IMPLEMENTATION.md), [приложение, строка 101](DEEP_RESEARCH_AGGREGATION.md).
3. Forfeit раскрывает разрешённый weighted остаток, а не все промежуточные residual amounts. Сохранение ограниченных проходов и переход к одному batch opening разделены как разные политики: [основной отчёт, строки 284–290](DEEP_RESEARCH_IMPLEMENTATION.md).

Для последнего пункта точечно прочитан production [forfeit_members](../../crates/core/nod/src/called.rs): функция получает один bucket_key, читает его day (строка 249), выбирает членов в цикле с budget (252–259), удаляет права и накапливает credit (280–295). Поэтому предупреждение о timing/granularity обосновано текущим кодом. Если остаётся прежний arbitrary bounded pass внутри residual group, одного group total недостаточно; нужны сохранённые данные для этого pass. Отчёт это условие оставляет, а изменение поведения не выдаёт за принятое.

Bound `F≤G≤B≤floor(S6·10^12·32/100)<2^176<q` корректен **для одного соответствующего дневного бюджета** при R01/R05/R10 и неотрицательности остатка: [приложение, строка 207](DEEP_RESEARCH_AGGREGATION.md), [основной отчёт, строка 251](DEEP_RESEARCH_IMPLEMENTATION.md). Он не переносится автоматически на объединение неограниченного числа дней, arbitrary U256 budget, cost или пожизненный Gratis. Ошибки прежнего предположения `f_l≤10^6` в итоговом аргументе нет.

## 4. Проверка границ privacy и масштаба

Нет оснований объявлять threshold Paillier готовым mobile-safe решением: отчёт оставляет same-key proactive refresh обязательным отдельным условием. Это согласуется с **static** adversary в [Tiresias, §1.2](https://eprint.iacr.org/2023/998.pdf). Подстановка signing DKG вместо monetary state handoff не предлагается.

Прочитанный hbACSS fault recovery после доказательства faulty dealer предусматривает раскрытие decryption keys для recovery (§IV). Поэтому его наличие не разрешает автоматически раскрывать денежные shares в любой жалобе: secrecy scope, кто является dealer при resharing/packing, и весь historical transcript надо проверить отдельно. Исправленный отчёт корректно не заимствует этот путь без проверки. [hbACSS, §IV, Share Recovery](https://eprint.iacr.org/2021/159.pdf).

Размеры 64 B/share pair и 768 B/Paillier ciphertext в §§10.1–10.3 — размеры указанных форматов, не полных транзакций и не benchmarks. Новая стоимость repair теперь не прячется в строке upload. В обоих документах согласована одна модель **36.5N record-epochs**: [приложение, строка 240](DEEP_RESEARCH_AGGREGATION.md), [основной отчёт, строка 464](DEEP_RESEARCH_IMPLEMENTATION.md). Она даёт 2.336 TB raw share state при 64 B на пару и N=10⁹; это условная арифметика выбранных границ checkpoints, а не measured traffic. Пропускная способность, стоимость malicious faults и overlapping days не заявлены доказанными.

## Итоговый статус и остающаяся работа

| Scope | Статус исследования | Что не следует считать выполненным |
|---|---|---|
| Q2 | Исправлена цепочка receipts → private redistribution → common Ready → exact S | Production admission/repair, security/liveness proof композиции, измерение TPS |
| Q3 | Late public mapping и retention/common coverage согласованы | Packed linkage/regrouping implementation и масштаб handoff |
| Q6 | Residual lifecycle и его связь с claim/forfeit изложены с необходимыми gates | Выбор granularity/timing публикации F, реализация/проверка transition proofs |

Новых изменений production для завершения **этого исследования** не требуется. Остаточные пункты должны оставаться явно открытыми в реализации; они не являются поводом заявлять, что весь private Tribute→Lysis→Nod уже построен.

## Границы проверки

Источники кода и ограничения trace зафиксированы в [TRACE_EVIDENCE.json](TRACE_EVIDENCE.json). SHA-256 всех 24 перечисленных исходников повторно совпали с manifest. Это подтверждает неизменность source snapshot, а не полноту аудита. Непосредственно прочитан участок `forfeit_members` в [called.rs](../../crates/core/nod/src/called.rs), строки 241–295; остальные code facts опираются на [нормативный trace](PROTOCOL_TRACE_AND_REQUIREMENTS.md). Проверка не охватывает весь репозиторий или доказательство безопасности готовой реализации.

Проверенные текстовые snapshots (SHA-256; последующие согласованные правки могут сдвинуть строки):

```text
DEEP_RESEARCH_AGGREGATION.md
61b3be7878aa4613611ce0b88c4edfc69456b4299447a73278645b8a47f52d7b
DEEP_RESEARCH_IMPLEMENTATION.md
697b82478663b3c283daaad3e234bdf92233d4507b74070c9787dbc840b70bdc
PROTOCOL_TRACE_AND_REQUIREMENTS.md
850e2bc5487497e846da156817c0fc88acf88571f481fb33148d2252f5374c58
TRACE_EVIDENCE.json
d29cc0b5e5f87fb2666fc5cf87ec05dbd2c6f4d4b3c383d998e03d4ed95ce55b
```
