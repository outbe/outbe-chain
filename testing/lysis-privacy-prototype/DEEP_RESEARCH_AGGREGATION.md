# Q2/Q3/Q6: точные закрытые агрегаты, поздние группы и смена валидаторов

Материал дополняет [основной отчёт](DEEP_RESEARCH_IMPLEMENTATION.md). Источники проверены по состоянию на **2026-09-11**; сравниваются конструкции и реализации, а не результаты нового benchmark.

Нормативная локальная основа — [PROTOCOL_TRACE_AND_REQUIREMENTS.md](PROTOCOL_TRACE_AND_REQUIREMENTS.md), особенно R01–R04, R07–R10, R14 и C13/C14. Границы проверки кода заданы её source evidence; выводы не являются исчерпывающим аудитом production.

После независимой проверки уточнён retention contract: [R15 Intex payout](DOWNSTREAM_PRIVATE_STATE_TRACE.md#r15) сохраняет потребность в закрытом eligible total и индивидуальных правах после Lysis. [R16/R17](DOWNSTREAM_PRIVATE_STATE_TRACE.md#r16) отдельно поддерживают mutable Gratis/Fidelity state. Числа ниже для nominal VSS не включают их дополнительное состояние. Алгебра S/S_l не изменилась; [реестр исправлений](REVIEW_REMEDIATION.md) отделяет её от открытых interface/security gates.

## Вывод для интеграции

1. **Для текущего R01 числовой дневной агрегат не требует 286-битного поля.** Доказанные source constraints дают `a6 < 2^104`, `S6 < 2^136` при текущем u32 count; для 10⁹ записей — 134 бита. Pedersen VSS в одном подходящем scalar field позволяет получить точное число `S6` и затем `S_l6`, без discrete log и без возвращения владельца.
2. **Главная цена VSS — хранение и перенос закрытых данных до позднего snapshot.** Один дневной итог не содержит распределения по ещё неизвестным лигам. Packed DPSS может существенно улучшить перенос; оценку простой реализации `O(n²N)` нельзя выдавать за нижнюю границу всех протоколов.
3. **Threshold Paillier/Damgård–Jurik даёт другой обмен затратами:** существенно больше ciphertext на запись, зато при сохранении одного public key ciphertext могут оставаться неизменными, а между комитетами передаётся только key state. Для этого необходим отдельный проверенный proactive key-handoff. Статически безопасный threshold decrypt и обычный новый DKG этого не доказывают.
4. **После окончательных групп и создания Nod можно преобразовать закрытое состояние в остатки прав по подходящим публичным группам.** Claim обновляет остаток с доказанным закрытым дебетом. Это позволяет не переносить индивидуальные monetary shares до каждого forfeit, если все остальные потребители уже покрыты. Public descriptor/nullifier state при этом остаётся.
5. **Current R01 вместе с R05 даёт native-field bound и для R14:** `F18≤G18≤B18≤floor(S6·10^12·32/100)<2^176<q` при текущем u32 count; при N=10⁹ достаточно bound 174 бита. Одной декларации U256 для этого было бы недостаточно. При альтернативном полном U256 budget/source понадобится wide-integer/CRT путь; cost и пожизненные балансы этим выводом не покрыты.

Ни один рассмотренный repository не подтверждает готовую реализацию всего маршрута со связкой R01, late league, mobile validators, durable admission и forfeit.

## 1. Числа, которые действительно надо агрегировать

| Профиль | Вход и предел суммы | Следствие для одного scalar field |
|---|---|---|
| Current canonical R01 | `a_max6=(2^64·10^6−1)·10^6`; при N=10⁹ S занимает 134 бита; при N<2³² S<2¹³⁶ | Дневной и каждый league nominal помещаются в q251/q252/q254 без редукции значимого результата |
| Альтернативный arbitrary U256 source | Каждый a≤2²⁵⁶−1; raw sum 10⁹ записей до 286 бит; accepted total всё равно обязан соответствовать C13 | Нужны доказанные limbs/CRT и закрытая проверка overflow либо иной integer protocol; простая редукция по q неверна |
| Current allocated/forfeit load18 | `F≤G≤B≤floor(S6·10^12·32/100)` по R05/R10 и неотрицательности прав | Вместе с R01: F<2¹⁷⁶ для полного u32 count, значит native q достаточно; arbitrary U256 budget — отдельный профиль |
| Cost18 | `c_i18=a_i6·f_i6·p_i6` | Отдельные границы price, intermediate products, currencies и finalizer schema; не решаются границей S |

Границы первой строки действуют лишь при обязательном доказательстве R01, уникальности admitted records и публичном проверенном count. Это сохраняет U256 policy, а не вводит произвольный новый economic cap.

Для commitments возможны разные группы. У BabyJubjub prime subgroup имеет 251-битный порядок и cofactor 8; Jubjub — 252-битный scalar modulus и cofactor 8; BN254 G1 использует 254-битное Fr. Выбор должен учитывать интерфейс P_link: arithmetic gadget на native curve и внешняя commitment-link proof — разные способы связки. Здесь далее используется абстрактная группа простого порядка q, а не обязательный выбор BabyJubjub. [EIP-2494](https://eips.ethereum.org/EIPS/eip-2494), [Jubjub: параметры](https://github.com/zkcrypto/jubjub), [BN254 Fr](https://github.com/arkworks-rs/curves/blob/master/bn254/src/fields/fr.rs).

## 2. Pedersen VSS: конкретный transcript

Базовый строительный блок — два полинома и скрывающие commitments коэффициентов; shares можно складывать. Это **verifiable secret sharing**, а не арифметика только над одним публичным commitment. Ниже — собственная адаптация к master, а не утверждение, что статья уже содержит Tribute admission. [Pedersen, 1991, §4–5](https://cgi.di.uoa.gr/~aggelos/crypto/page8/assets/Pedersen-VSS.PDF).

Обозначения: n — число получателей; t — минимальное число долей для восстановления; f — максимум скомпрометированных участников; d — дополнительные недоступные честные получатели; Q — число durable receipts для admission. `G,H` — проверенные независимые генераторы, отношение discrete log которых неизвестно участникам.

### 2.1 Upload и admission

1. **Owner** знает `a_i,r_i`, создаёт `C_i=a_iG+r_iH` и P_link, который связывает этот a с canonical source/R01/context. Свободное доказательство диапазона без исходного offer не заменяет P_link.
2. Owner выбирает полиномы P_i(X), R_i(X) степени t−1: `P_i(0)=a_i`, `R_i(0)=r_i`. Публикует коэффициентные commitments `A_i,k=P_i,k·G+R_i,k·H`; **A_i,0 обязан быть тем самым C_i**.
3. Для каждого уникального ненулевого индекса j owner посылает по защищённому каналу пару `y_i,j=P_i(j)`, `z_i,j=R_i(j)`. Узел проверяет canonical scalars, точки нужной подгруппы и равенство:

   `y_i,j·G+z_i,j·H = Σ(k=0..t−1) j^k·A_i,k`.

4. **Receiver j** выдаёт подписанный receipt лишь после успешной проверки и устойчивой записи share pair. Receipt связывает day, source/Tribute ID, epoch, index, C_i, digest всего coefficient vector и версию формата. Подпись над полученным ciphertext без проверки его содержимого недостаточна.
5. **Admission/consensus** проверяет P_link, source reuse, owner/day uniqueness, offering state, bounds/count и Q receipts на один transcript. Только затем атомарно добавляет record в accepted root и связанные accounting updates. Pending upload не входит в S. Owner может уйти offline после этого события.

Для такого receipt rule достаточны разные, не взаимозаменяемые условия:

- конфиденциальность: `f < t`;
- достаточно долей после withholding и дополнительных потерь: `Q−f−d ≥ t`, то есть `Q ≥ t+f+d`;
- инфраструктура должна обеспечивать `n−f−d ≥ t` и соответствующий durable handoff.

Отдельно проверяется возможность собрать Q receipts: при f отказывающихся подписывать узлах и d_ingress других недоступных во время admission нужно `Q≤n−f−d_ingress`. Это условие прогресса, а не конфиденциальности. Совместно с предыдущей консервативной оценкой оно требует `n≥t+2f+d+d_ingress`; конкретные модели могут иметь другие sufficient conditions, но нельзя показывать только reconstruction inequality и обещать liveness.

Это собственная worst-case оценка: f и d считаются непересекающимися потерями. Она не заменяет согласование root, recovery protocol или модель мобильного противника. Q не превращает слабый t в более сильный privacy threshold. Коалиция из t хранителей может восстановить отдельное a, независимо от того, разрешает ли ledger такую операцию.

### 2.2 Закрытая подготовка и проверяемое открытие

Локальное суммирование разрешено после checkpoint, подтверждающего полный coverage одного accepted manifest у общего набора держателей (§2.4). Только полный держатель может получить `(Y_j,Z_j)=Σ_i(y_i,j,z_i,j)`. До закрытия публиковать эти aggregate shares не требуется. Burn в offering и rollback меняют ровно те же records и root; повторная доставка не создаёт второе слагаемое.

После закрытия узлы публикуют shares разрешённого агрегата, подписанные с root/query ID. Их можно проверить по сумме коэффициентных commitments; из t корректных пар интерполируются **числа** `(S,R)`. Публичная проверка:

`Σ_i C_i = S·G+R·H`, `0≤S≤verified_bound<q`, `count=N`.

Для late snapshot каждый узел повторяет ту же линейную операцию по публичному `owner→league`: получаются S_l и aggregate blind R_l. Verifier проверяет snapshot binding, разделение accepted set, commitments групп и `Σ_lS_l=S`, `Σ_ln_l=N`. Проверка только последнего равенства недостаточна: можно перераспределить суммы между лигами, сохранив total.

Public commitment equality даёт денежную привязку при binding assumption; signatures/roots обеспечивают принадлежность конкретному transcript. Нужна доступность coefficient commitments или эквивалентного проверяемого polynomial commitment state для проверки aggregate shares. Merkle root сам по себе эти данные не заменяет.

### 2.3 Complaint/repair после ухода владельца

Вариант «при жалобе опубликовать raw shares записи, затем повторить» требует отдельного leakage analysis: повторные repair и эпохи могут накопить достаточно материала для индивидуального открытия. Нельзя просто отключить complaints и сохранить прежнее утверждение о robust availability.

Для заданного маршрута нужны private authenticated recovery между держателями либо доказательства корректности зашифрованных shares/PVSS; faults подтверждаются без публикации произвольного набора индивидуальных plaintext shares. Эти механизмы должны сохранять общий C_i, canonical polynomial state и atomic admission/handoff. Конкретный malicious secure PVSS/repair слой с такой связкой **в этом исследовании не предъявлен как готовая библиотека**. После admission его нельзя заменять ожиданием, что owner снова придёт и исправит пакет.

### 2.4 Выравнивание доступности перед агрегированием

При повторной проверке найден пропущенный шаг: Q receipts для каждой записи не гарантируют общих держателей всех записей. Например, `n=4,t=2,f=1,Q=3`; записи A/B/C/D имеют receipts `{1,2,3}`, `{1,2,4}`, `{1,3,4}`, `{2,3,4}`. При withholding узла 4 каждая запись всё ещё имеет две честные доли, но ни один честный держатель не имеет все четыре записи. Формула локальной суммы из §2.2 без repair здесь неисполнима.

Конструктивный fallback — verifiable redistribution каждой неполной записи на общий roster. Он применим и при смене состава. Все операции ниже над одним scalar field q; old indices ненулевые и попарно различны. Для записи i выбирается согласованный набор `H_i` из t_old доступных verified helpers; `λ_h` — Lagrange coefficients в нуле для этого набора. Индексы, H_i и epoch включаются в transcript.

```text
E_i(h) = Σ_k h^k * A_i,k                 # публичный commitment старой пары h

helper h создаёт свежие degree-(t_new−1) полиномы U_h,V_h:
U_h(0) = λ_h * y_i,h
V_h(0) = λ_h * z_i,h
B_h,k = U_h,k*G + V_h,k*H

обязательная публичная проверка: B_h,0 == λ_h * E_i(h)
new recipient j получает приватно U_h(j),V_h(j), проверяет их по B_h,k

y'_i,j = Σ_h U_h(j); z'_i,j = Σ_h V_h(j)
A'_i,k = Σ_h B_h,k
A'_i,0 == Σ_h λ_h E_i(h) == C_i
```

Интерполяция обеспечивает `Σ_hλ_h y_i,h=a_i` и аналогичное равенство для r_i. Новые secret полиномы имеют те же constant terms; отдельный helper знает лишь свою старую пару. Распределять её нужно приватно: публикация scalar constants или их восстановление одним координатором не требуется. Sub-sharing старых долей с Lagrange combine — стандартная основа resharing в [Groth, 2021/339, §2.5](https://eprint.iacr.org/2021/339.pdf); приведённая связь двойных Pedersen commitments — адаптация для нашего денежного интерфейса.

Если helper не завершил свою раздачу, попытка не становится final. Старое состояние сохраняется; выбирается новый согласованный H_i и свежие полиномы, либо выполняется проверенный recovery выбранного протокола. Нельзя смешать contributions разных H_i/коэффициентов или использовать threshold shares разных epochs как один polynomial. Каждый retry связан с record, attempt ID, old/new manifests и authenticated channels. Гарантия live completion зависит от наличия достаточных доступных честных helpers и конкретного malicious protocol; алгебра не даёт её сама.

Перед открытием принимается `Ready(finalRoot, epoch, transcriptRoot, coverageRoot)`. В консервативном baseline хотя бы `K≥t_new+f_new+d` держателей подписывают **полное** покрытие manifest после проверки и durable записи. Сбор K signatures также должен быть возможен при отказах; проверяется отдельная liveness inequality, как в §2.1. Такие же gates требуются для S_l, создания residual state и его debit updates. Неполный manifest не становится Ready; S/Lysis/forfeit ждут repair, а принятые права не удаляются и не раскрываются. Формальное различие recoverable shares, completeness и дальнейшей работы с линейными комбинациями также рассматривает [hbACSS, 2021/159, §§II.B, VI](https://eprint.iacr.org/2021/159.pdf); его конкретный fault transcript не переносится сюда без проверки privacy.

Fallback стоит `O(t_old*n_new)` private pair transfers на запись и новые coefficient commitments. Это проверяемый по алгебре медленный путь, не обещание масштабирования. Повторные отказы дополнительно увеличивают стоимость. Для mobile security нужны условия secure erasure и malicious handoff из §4; простой повтор этой формулы не доказывает безопасность при последовательных компрометациях.

## 3. Late league: что надо сохранить и что можно сжать

Это вывод из требований R06/R07, не криптографическая impossibility theorem.

Если owner→league определяется позднее, хранение только `S=Σa_i` теряет информацию для нового разбиения. Для семейства произвольных будущих selector vectors единственный короткий итог не сохраняет все точные линейные ответы: например, `[1,5]` и `[2,4]` имеют один S, но разные суммы при разделении владельцев. Общее sufficient representation должно сохранять различимость нужных распределений. Это **не разрешение публиковать любые selectors**, тем более индивидуальные queries.

Практические варианты состояния до snapshot:

- по VSS — индивидуальные share pairs, либо packed shares с возможностью проверяемого выделения поздних групп;
- по threshold encryption — индивидуальные ciphertext и metadata в data-availability storage; key shares отдельно;
- по Prio — удержанные валидированные per-record output shares и membership data вместо преждевременного сворачивания в один total.

Здесь O(N) относится к сохраняемой информации в общей системе для неограниченного позднего разбиения, **не обязательно к объёму, который каждый час передаётся каждому валидатору**. Ciphertext storage и key handoff особенно наглядно разделяют эти два расхода.

Owner-based compression в текущем master ограничена: уже разрешён только один Tribute на owner/day. Схлопывать разные дни можно лишь при доказанной одинаковой семантике future snapshot, прав и deadlines; в общем случае она различается. После фиксации лиг агрегирование до S_l корректно для coefficient kernel, но для R10 weighted cost и R14 этого состояния может оказаться недостаточно.

256 записей в execution batch — размер рабочего задания, не автоматически privacy cohort. Открывать block/worker/epoch subtotal master не требует. Допустимы зафиксированные S, S_l и разрешённый F. Поэтому проверяющие decryption/opening shares должны аутентифицировать именно разрешённый запрос и сохранять его статус при handoff. Неутверждённые subset queries не должны добавляться как побочный интерфейс реализации.

## 4. Proactive resharing и мобильный противник

Устойчивость к f скомпрометированным узлам за всё время и устойчивость к меняющейся коалиции f за эпоху — разные модели. Proactive sharing обновляет shares, оставляя секрет прежним. **CHURP** специально рассматривает меняющиеся комитеты; авторы заявляют mobile-adversary security и optimistic `O(n)` on-chain / `O(n²)` off-chain communication. Это стоимость протокола shared secret, не доказанная стоимость всего миллиарда денежных records. [Авторская страница CHURP](https://www.fanzhang.me/publications/19-churp/), [статья, 2019](https://eprint.iacr.org/2019/017.pdf).

Нужный Outbe handoff transcript, как требование к будущей реализации:

1. Старый комитет замораживает authenticated state/root и admission boundary; новый получает тот же canonical manifest.
2. Протокол преобразует старые shares в новые, сохраняя денежные secrets/commitments и создавая свежую случайность. Нельзя реконструировать все amounts или полный decrypt key у координатора и назвать это resharing.
3. Новые участники проверяют shares, state completeness и consistency. Финальный handoff certificate удостоверяет достаточно **сохранивших проверенное состояние** получателей.
4. Только после durable completion производится cutover; retry/crash не разрешает два независимых accepted histories. Старый комитет остаётся доступным до допустимого completion/abort, а не исчезает по таймеру посреди передачи.
5. Старые честные участники уничтожают obsolete shares, использованную randomness и доступные пути их восстановления. Сохранённые WAL, snapshots, backups и старые channel keys способны нарушить эту предпосылку. Commit certificate доказывает принятие состояния, но не доказывает физическое стирание чужого диска.

Это адаптация требований к proactive model, а не полный security proof. Ограничение мобильного противника должно учитывать **переходное окно old/new**, а не только отдельно два списка активных валидаторов. Опубликованный или украденный индивидуальный secret уже не становится неизвестным после refresh. Ключи транспорта также должны поддерживать нужную forward secrecy; зашифрованные исторические shares под навсегда сохраняемым recipient key не исчезают от обновления live shares. Модель secure erasure и adaptive corruption прямо различается в литературе. [Canetti et al., Adaptively Secure MPC, §1–2](https://www.iacr.org/archive/crypto2019/116940188/116940188.pdf).

### 4.1 Packed/robust DPSS — существенная альтернатива простой пересылке

Baron–El Defrawy–Lampkins–Ostrovsky представляют dynamic proactive sharing с **O(1) amortized communication per secret** при пакетной обработке, включая perfect/statistical security варианты. В §4 есть ограничения на переход: размер группы меняется не более чем вдвое, а old/new degree и thresholds удовлетворяют условиям безопасной реконструкции; §5 описывает redistribution и error correction. Это основание исследовать packed state, а не утверждать неизбежность `O(n²N)`. [DPSS, 2015, §4–5](https://eprint.iacr.org/2015/304.pdf).

Собственная иллюстрация packing: полином с ℓ secret positions и f случайными степенями свободы может иметь степень `ℓ+f−1`; n evaluations хранят ℓ секретов, а не один. Но это ramp sharing: reconstruction threshold и запас на faults меняются. Получатель с одним evaluation не может просто извлечь нужную внутреннюю позицию и суммировать произвольную late league без дополнительного linear transformation/repacking protocol. Открытие всего polynomial раскрывает внутренние записи.

Для подключения packed DPSS остаются конкретные работы: ingest независимых клиентов без совместной сессии; проверяемый переход из индивидуального C_i/VSS в packed commitments; out-of-order admission/удаление burn; private regrouping по публичным late selectors; сохранение нужного packing rate после смены n; durable recovery. O(1) asymptotic не сообщает число байтов коэффициента, размер batch, preprocessing и цену такого linkage. Готовый measured Outbe путь отсюда не следует.

### 4.2 DKG подписей не является переносом monetary state

Public-key generation/signature DKG решает другую задачу, чем хранение `a_i,r_i` и поддержание их актуальных shares. Даже если он использует Shamir/Feldman, это не перенос миллиардов денежных записей. Аналогично BLS scalar shares нельзя непосредственно использовать как threshold Paillier key shares: у Paillier иной modulus generation и sharing над целыми. Для выбора интеграции требуется явно назвать, **что переносится**: signing key, monetary shares, encrypted data key или private Fidelity/Gratis state.

## 5. Threshold Paillier / Damgård–Jurik

При Paillier ciphertext `E(a;ρ)=(1+N_RSA)^a·ρ^N_RSA mod N_RSA²`; произведение ciphertext даёт шифрование суммы. Threshold decryption возвращает числовой plaintext, а корректность decryption shares можно доказывать. Damgård–Jurik обобщает пространство сообщений до `Z_(N_RSA^s)` и ciphertext до `Z_(N_RSA^(s+1))`. [Damgård–Jurik–Nielsen, §3, §5](https://people.csail.mit.edu/rivest/voting/papers/DamgardJurikNielsen-AGeneralizationOfPailliersPublicKeySystemWithApplicationsToElectronicVoting.pdf).

**Tiresias** улучшает malicious threshold Paillier и проверку пакетных decryption shares; §1.2 формулирует **static corruption**, а §3/Appendix F обсуждают distributed key generation. Поэтому paper не подтверждает необходимый нам произвольный mobile handoff. Его числа decryptions/s не являются Tribute admission TPS: там другая работа и размер batch. [Tiresias, 2023, §1.2, §3, §5, Appendix F](https://eprint.iacr.org/2023/998.pdf).

Собственный transcript для нашего случая:

1. Комитет получает public encryption key без dealer, знающего полный секрет; фиксируются proof/verification parameters и epoch lineage.
2. Owner отправляет один ciphertext a_i, C_i и доказательство знания **одного a_i** одновременно в R01, `C_i=a_iG+r_iH` и ciphertext. Это cross-group/cross-ring statement; простой Paillier range proof без связи с C_i не подходит. Цена этой proof здесь не измерена.
3. Admission проверяет linked input и durable ciphertext availability. Owner offline. Ciphertext включается в accepted root.
4. После closure любой исполнитель вычисляет произведение ciphertext дня; после snapshot — произведения по утверждённым лигам. Комитет выдаёт проверяемые partial decryptions только этих aggregate ciphertext. Verifier проверяет canonical manifest, ciphertext product и decryption proofs.
5. Для R14 возможны ciphertext остатки: умножение добавляет amount, умножение на inverse ciphertext вычитает его, возведение в публичную степень вычисляет weighted load. Остатки/claims должны быть связаны с теми же records, а не просто быть любыми ciphertext корректной формы.

Широкий plaintext modulus устраняет scalar-field wrap для 134-битного S и 256-битного F при доказанных неотрицательных диапазонах и суммах ниже modulus. Он **не устраняет** C13, отрицательные malicious inputs, неверный источник, повторный debit или несанкционированное subset decryption.

Главное преимущество при rotation условно: **тот же public key + корректный proactive key-share refresh** позволяет оставить O(N) ciphertext на месте; handoff key state не зависит от N. Верификационные ключи долей и параметры threshold protocol тоже должны обновляться согласованно. Нельзя подставить обычный prime-field Shamir refresh в integer-sharing Paillier без доказательства совместимости.

Если каждый час создавать независимый PK_e, нельзя просто перемножить ciphertext разных эпох в один decryptable aggregate. Открытие отдельных epoch×league totals раскрывает больше утверждённого S_l. Альтернативы — доказанный key switching/re-encryption без owner либо MPC, выдающий только финальную сумму; ни одна здесь не заявлена бесплатной. Старые хранители ключа должны оставаться доступными до такого перехода, если он ещё не завершён.

Damgård–Jurik packing помогает, когда можно заранее безопасно назначить slots публично разрешённым итогам и ограничить carry. Он не превращает независимые one-shot uploads с неизвестной late league в один маленький общий report: каждому входу всё ещё нужна самостоятельная admission/linkage, а исходные данные нужны для последующего разбиения.

### Почему одна точка threshold ElGamal не заменяет это числовое открытие

При exponential/lifted ElGamal расшифровка encrypted sum возвращает `S·G` (или `g^S`), а не integer S. ElectionGuard использует ограниченный размер tally для последующего discrete-log extraction. [ElectionGuard specification paper, §2.3.2, §2.7.3](https://eprint.iacr.org/2024/955.pdf).

Собственная оценка: generic interval discrete log для неизвестного 134-битного S имеет порядок 2⁶⁷ групповых операций, поэтому этот путь нельзя считать практической числовой реконструкцией в нашем профиле. Разбиение на короткие разряды требует дополнительных ciphertext, carry/range/link proofs и анализа раскрытий. Pedersen VSS выше избегает DLog, поскольку threshold восстанавливает scalar shares; Paillier избегает его своим plaintext decoding.

Дополнение 2026-09-11: проверка Aptos/XELIS подтверждает практическую значимость chunked ElGamal как отдельного кандидата, а не опровержение оценки single-point DLog. При 16-bit chunks и N≤10⁹ отдельный aggregate chunk меньше 2^46, но публичное открытие таких ненормализованных chunks даёт дополнительные статистики сверх S/S_l. Нужен протокол приватного recovery/normalization до раскрытия. Подробный пример, роль threshold key handoff и raw storage projections — [TWISTED_ELGAMAL_APPLICABILITY §5–6](poc/TWISTED_ELGAMAL_APPLICABILITY.md). Новый backend в PoC не измерялся.

## 6. Prio / DAP / FL secure aggregation: границы применения

Актуальные на дату проверки drafts: **VDAF-22** и **DAP-19**. Prio3 выводит output shares фиксированным отображением input shares; отдельного aggregation parameter у Prio3 нет. DAP задаёт заранее согласованную task configuration и non-overlapping batches, а privacy требует хотя бы одного честного Aggregator. Проверка диапазона не доказывает истинность исходного платежа. [VDAF-22, §7](https://datatracker.ietf.org/doc/draft-irtf-cfrg-vdaf/), [DAP-19, §4.2, §8](https://datatracker.ietf.org/doc/draft-ietf-ppm-dap/).

Вывод для master: one-shot upload и валидированные output shares полезны. Сохранить per-record output share до snapshot и затем суммировать по публичной league алгебраически возможно, если исходное представление позволяет точный нужный диапазон. Однако обычная DAP task не предоставляет из коробки публично проверяемую связь с C(a), hour-by-hour замену физических shareholders и повторные residual-state updates до forfeit. Распределение двух логических Aggregators по n валидаторам требует отдельного MPC/threshold handoff и нового threat model. Это не автоматически безопасность «одного честного валидатора».

Flamingo использует one-shot normal clients и отдельную работу decryptors; late membership внутри report влияет на раскрытие self/pairwise masks. Lighthouse снижает server–committee работу с помощью batched threshold KEM и отдельного proof, но не доказывает economics input. [Flamingo, §4–5, Appendix B–D](https://arxiv.org/html/2308.09883v1), [Lighthouse, §3–5](https://www.usenix.org/system/files/usenixsecurity26-garg-sanjam.pdf).

Конкретный предел для нашего маршрута: произвольные независимые arrivals 50 часов, неизвестная league, множество последующих групп и changing decryptors требуют иной работы, чем один согласованный FL round. Нельзя повторно использовать один и тот же masked report с конфликтующими online/offline manifests ради разных league sums. Глобальный key refresh также не переносит автоматически individual-recipient encrypted shares/seeds. Держать один logical round 50 часов допустимо только после спецификации такой state continuity; рассмотренные исходники этого end-to-end пути не подтверждают. Подробный прежний source audit — [COMMITTEE_PROTOCOL_APPLICABILITY.md](COMMITTEE_PROTOCOL_APPLICABILITY.md); его старые double-floor выводы не заменяют новые exact formulas master.

## 7. R14: закрытый остаток вместо вечного индивидуального monetary state

Это **конструктивная адаптация**, не готовая опубликованная схема.

Когда известны league/final f и создан immutable Nod descriptor, следует определить минимальную публичную группу b, внутри которой все права имеют одинаковый коэффициент возврата и одинаковые условия выбора forfeit set. Безопасный начальный вариант ключа: `(day, bucket, f, asset, formula version, expiry/call context)`. Сам price нужен для cost consumers, но не обязательно для load forfeit; объединять buckets разрешено только если будущие права действительно обрабатываются одинаково.

1. Сеть проверяемо преобразует исходные monetary shares/ciphertexts в closed residual `R_b6=Σ_{i∈b, unspent} a_i6` и `C_b=ΣC_i`. Для VSS сохраняются shares R_b и blind; для Paillier — aggregate ciphertext. Проверяется полное соответствие issued Nod manifest; исключённый contributor всё ещё имеет Nod по master.
2. При claim owner приходит с a_i,r_i и связанной claim proof. Для VSS он может отправить свежие polynomial shares **того же a_i,r_i**, со constant commitment C_i. Каждый узел вычитает проверенный debit из residual shares. Если отдельная proof связывает другое debit commitment, должен быть доказан и согласован точный переход residual commitment.
3. Списание residual, приватный claim/payment, public spent/nullifier marker и accounting transition атомарны. Доказать лишь `0≤debit≤bound` недостаточно: debit должен совпадать с amount именно погашаемого Nod. При Paillier сохранившийся linked input ciphertext позволяет вычитать его напрямую, если он принадлежит текущему compatible key lineage.
4. Для истёкшего набора групп открывается **только разрешённый** `F18=Σ_b R_b6·f_b6·M`; F возвращается в лимит. Не требуется повторное появление непогасивших владельцев.

В bound §7.1 речь идёт о группах одного дневного бюджета. Объединение нескольких дней в один refund требует отдельной границы суммы или отдельных разрешённых дневных outputs.
5. Individual monetary shares можно удалить после доказанного преобразования, если R10, Fidelity и R15 Intex payout уже обеспечены другим достаточным состоянием. В частности, H/round rights и online/private-denominator либо offline payout mechanism не исчезают при создании Nod residuals. Individual public descriptors/membership/nullifiers и возможность owner скачать свои public witness data живут по отдельной policy.

Закрытый остаток скрывает intermediate monetary debits относительно разрешённых публичных outputs. Public status и разрешённый final F остаются наблюдаемыми; granularity/timing раскрытия F требуют явного правила.

Размер secret state после преобразования — O(K_active), где K_active есть число реально различимых будущих групп, а не обещанная константа. При уникальных условиях buckets K_active может быть O(N). При общих deadlines/coefficient contexts экономия значительна. Claim-owner должен сохранить opening; aggregate storage не заменяет ему локальный секрет.

### 7.1 Native-field forfeit: что доказывает current budget и что изменится при расширении

Master задаёт `f6=floor(B18·M/S18)` и `fmax6=2f6`. Поэтому здесь **не принято** допущение `f_l6≤10^6`. Нужный bound следует из другого места: R05 задаёт `B18=min(D,Q)`, причём `D≤floor(S18·32/100)`, `S18=S6·10^12`. R10 проверяет `G18≤B18`; неотрицательные непогашенные права дают `0≤F18≤G18`. Следовательно, current R01+R05+R10 обеспечивают F<2¹⁷⁶<q для полного u32 count, независимо от размера отдельного E18. Для N=10⁹ безопасен bound 174 бита. Это замыкает точное scalar opening F в VSS **при сохранении этих invariants**, без MPC carry только ради forfeit. Неверный debit или состав прав всё ещё обязан исключаться связанными proofs.

Если заменить R05 произвольным U256 budget или расширить source profile, остаётся лишь `F≤U256`; одного q251/252/254 тогда недостаточно. Открыть `F mod q` и проверить `F≤U256` неверно: несколько допустимых integers могут иметь один residue. Для такого отдельного расширения возможны:

- поддерживать достаточные CRT residues F с доказанной связкой каждого input/transition между представлениями; произведение модулей превышает максимальный F;
- выполнять authenticated wide-integer arithmetic/carry normalization над закрытыми остатками и публиковать canonical integer F с proof;
- использовать threshold ciphertext с plaintext modulus, заведомо большим B.

Для расширенного профиля, в котором nominal всё ещё ограничен 136 битами, полезна собственная оценка MPC варианта: публичный weight `w_b=f_b·M` можно разложить на 64-битные limbs. Для каждого limb сумма `Σ_b R_b·w_b,k` имеет bound менее 2²⁰⁰ при `ΣR_b<2¹³⁶`; такие внутренние суммы помещаются в перечисленные native fields. Но публиковать их отдельно нельзя автоматически: они могут раскрыть дополнительные линейные сведения сверх F. Нужна закрытая carry normalization в один разрешённый результат. Цена такого MPC зависит от K_active и числа limbs; она не измерена. Bounds костов/других assets остаются отдельной задачей.

## 8. Байты и работа: расчётные границы

Обозначения: N — records; n — recipients; t — reconstruction threshold; E — число refresh/handoff checkpoints; N_e — retained records при e; K — число остаточных групп. GB/TB здесь десятичные. Ниже **raw crypto payload**, без source/P_link, signatures, metadata, encryption envelopes, database indexes, replicas и recovery logs.

У Jubjub scalar encoding — 32 B; compressed group encoding также 32 B. BabyJubjub implementation использует compressed point `[32]byte`. Для таблицы VSS выбран canonical scalar container 32 B; это формат оценки, а не 13-байтная упаковка исходного a. [Jubjub Fr](https://github.com/zkcrypto/jubjub/blob/main/src/fr.rs), [Jubjub group](https://github.com/zkcrypto/jubjub/blob/main/src/lib.rs), [BabyJubjub compression](https://github.com/iden3/go-iden3-crypto/blob/master/babyjub/babyjub.go).

| Вариант/операция | На record / формула | При N=10⁹ |
|---|---|---|
| Один public C_i | 32 B для приведённых compact encodings | 32 GB |
| Pedersen VSS, пара shares одному node | 2 scalars = 64 B | 64 GB/node |
| VSS fanout владельцев всем n | 64nN B | 64n GB всего |
| Прямой coefficient vector сверх C_i | 32(t−1) B | 32(t−1) GB public material |
| Простое открытие одного aggregate | t pairs = 64t B, плюс proof/context/auth | Не растёт с N; вычисление commitments/membership и чтение входов растут |
| Наивный handoff: t_old старых nodes re-deal каждому n_new | порядка 64·t_old·n_new·N_e B приватного share payload | Это конкретная простая конструкция, не lower bound DPSS |
| Packed amount+blind, ℓ records/полином | в идеальной основной части 64·ceil(N/ℓ) B/node | Без linkage/commitments/packing/recovery overhead |
| Paillier, illustrative modulus 3072 bits | ciphertext mod N_RSA²: 768 B | 768 GB ciphertext |
| Damgård–Jurik | ceil((s+1)·k/8) B ciphertext при k-bit modulus | Расширенный plaintext не отменяет per-input proof |
| Residual VSS после R10 | примерно 64K B/node плюс verification state | Выигрыш только когда K≪N |

3072 здесь — пример размера, не утверждённые security parameters. Ни 64 B VSS-share, ни 768 B ciphertext не являются размером полного Tribute с source proof. Для link proof, PVSS, AEAD, inclusion и availability certificates должна быть отдельная сериализация до окончательного per-report бюджета.

При равномерном поступлении N за E offering checkpoints собственная модель даёт:

`Σ_e N_e = N·(1+2+...+49)/50 + 12N = 36.5N`.

Это модель 49 почасовых переходов до окончания offering и 12 переходов при полном наборе до обработки, согласованная с основным отчётом; включение дополнительных граничных checkpoints меняет результат на полные партии N. Поэтому данные «64 GB/node» и «один handoff» нельзя подставлять вместо всего lifetime traffic. Наличие 1200-block rotation в текущем deployment здесь не утверждается; hourly rotation — исследуемый сценарий master.

Требуемый средний ingest: `10^9/(50·3600)≈5555.6 records/s`. Это арифметическая нагрузка, **не измеренная пропускная способность**. Клиентские proofs/шифрование независимых records можно параллелить; проверить достижение нагрузки нужно на полном linked admission, durability, retained-state refresh и downstream claims. Public Lysis с S_l не требует decrypt каждого ciphertext, но ingest/late grouping всё равно читает O(N) входных записей.

## 9. Проверенный shortlist строительных блоков

| Проект | Язык / лицензия, что проверено | Применимость и граница |
|---|---|---|
| [zkcrypto/jubjub](https://github.com/zkcrypto/jubjub) | Rust; MIT или Apache-2.0; README прямо сообщает отсутствие review/audit | Group/scalar arithmetic, не VSS/PVSS/handoff service |
| [iden3/go-iden3-crypto](https://github.com/iden3/go-iden3-crypto) | Go; MIT или Apache-2.0 | BabyJubjub primitives; внешний audit этой конкретной поставки здесь не установлен |
| [CHURPTeam/CHURP](https://github.com/CHURPTeam/CHURP) | Go; README называет academic prototype, не предназначенный для deployment; лицензию самого проекта здесь подтвердить не удалось | Исследовательский handoff reference; используемые PBC/GMP имеют собственные licences; не готовый monetary state engine |
| [dwallet-labs/tiresias](https://github.com/dwallet-labs/tiresias) | Rust; README сообщает internal review, отсутствие third-party audit; точный license text недоступен в проверенных fetches | Threshold Paillier/decryption reference; static proof не подтверждает mobile refresh. До adoption требуется восстановить доступ к точной лицензии и зафиксировать revision |
| [divviup/libprio-rs](https://github.com/divviup/libprio-rs) | Rust; MPL-2.0; README связывает 0.18/main с VDAF-18/DAP-17 и предупреждает о движущемся main | Реальный Prio3/VDAF building block; не следует молча приравнивать эту реализацию к актуальным VDAF-22/DAP-19; mobile handoff и C(a) linkage не установлены |
| [divviup/janus](https://github.com/divviup/janus) | Rust; MPL-2.0; README: DAP, только trivial aggregation parameters, main между draft versions | Operational DAP reference; не готовый произвольный late-query/threshold-validator service; audit status этой поставки здесь не установлен |
| [Dock secret_sharing_and_dkg](https://github.com/docknetwork/crypto/tree/224f195bb8babc2d0de5256135120e0aca9fbd19/secret_sharing_and_dkg) | Rust; Apache-2.0: [LICENSE](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/LICENSE); [README](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/secret_sharing_and_dkg/README.md): Pedersen/Feldman VSS, Gennaro/FROST DKG, PVSS | Библиотечные primitives; наличие durable proactive monetary handoff с требуемым input linkage не установлено |
| [TNO protocols.distributed_keygen](https://github.com/TNO-MPC/protocols.distributed_keygen) | Python; Apache-2.0; README: semi-honest DKG и отсутствие audit | Не удовлетворяет malicious validators как готовая реализация; годится для понимания интерфейса, не security substitution |

Oasis ADR-0023 применяет CHURP к keymanager и прямо описывает enclave access policy. Это полезный пример operational framing, **не доказательство готового TEE-free пути** для Outbe. [Oasis ADR-0023](https://github.com/oasisprotocol/adrs/blob/main/0023-keymanager-secret-sharing.md).

Source revisions: Dock — `224f195bb8babc2d0de5256135120e0aca9fbd19`; CHURP — `3b0a03a04761e473d7b2b799759217a20e7563e0`; libprio-rs — `b7f0cbe7145c01b618cd7b2f6a9636490b62f276`; Janus — `743ab5851b1ab16ba472a0c03a9bd423e9bf509d`. README последних двух проектов повторно прочитаны по этим SHA. Прочие branch links не являются immutable pins. Для Tiresias GitHub API вернул 404: paper и доступный web snapshot не гарантируют возможность получить актуальную воспроизводимую поставку. Полный реестр основных snapshots — [DEEP_RESEARCH_LIBRARY_PINS.json](DEEP_RESEARCH_LIBRARY_PINS.json).

## 10. Что передать в итоговый протокол, а что остаётся открытым

| Вопрос | Обоснованный результат | Нерешённая часть до утверждения реализации |
|---|---|---|
| Q2 точный S | Current R01 bound позволяет одно native scalar; VSS либо threshold ciphertext численно агрегируются offline | P_link→share/ciphertext binding, robust private repair, durable receipt/cutover; окончательный размер полного report |
| Q3 late S_l | Удержанные records и authenticated snapshot достаточны; aggregate shares/decryption можно проверить по canonical manifests | Цена сохранения/packed regrouping/mobile handoff; actual K и mapping consumers |
| Q6 late forfeit | Closed residual groups с доказанным claim debit позволяют owner оставаться offline до expiry; current R01/R05 bounds дают F<q | Exact state transition proof, granular expiry contexts, исключение двойного spend/forfeit; wide F path только для расширенного budget/source profile |
| Dynamic validators | DPSS/CHURP доказывают, что направление возможно; key-only handoff меняет масштаб | Конкретный malicious/mobile протокол выбранного представления, erasure/transport/recovery model, параметры n,t,f,Q,d |
| Public verifiability | Сам C_i и group algebra полезны для связывания monetary state | Public output proof не является доказательством DA или erasure; quorum signature не заменяет отсутствующий share/ciphertext consistency proof |

Отдельного победителя здесь нет. Для малого record payload и current 104-bit source сильна линия **общий commitment + VSS/packed DPSS**; для неизменяемого per-record storage и потенциально небольшого ключевого handoff — **linked threshold Paillier**, с большим ciphertext и нерешённым proactive refresh. Точный forfeit и долговечность закрытого Fidelity/Gratis state должны входить в сравнение вместе с admission.

## Основные первичные источники

- Pedersen, *Non-Interactive and Information-Theoretic Secure Verifiable Secret Sharing*, 1991: [PDF](https://cgi.di.uoa.gr/~aggelos/crypto/page8/assets/Pedersen-VSS.PDF).
- Maram et al., *CHURP: Dynamic-Committee Proactive Secret Sharing*, CCS 2019: [paper](https://eprint.iacr.org/2019/017.pdf), [авторская страница](https://www.fanzhang.me/publications/19-churp/), [reference code](https://github.com/CHURPTeam/CHURP).
- Baron et al., *Communication-Optimal Proactive Secret Sharing for Dynamic Groups*, 2015: [paper](https://eprint.iacr.org/2015/304.pdf).
- Damgård, Jurik, Nielsen, *A Generalization of Paillier’s Public-Key System with Applications to Electronic Voting*: [paper](https://people.csail.mit.edu/rivest/voting/papers/DamgardJurikNielsen-AGeneralizationOfPailliersPublicKeySystemWithApplicationsToElectronicVoting.pdf).
- Friedman et al., *Tiresias: Large Scale, Maliciously Secure Threshold Paillier*, 2023: [paper](https://eprint.iacr.org/2023/998.pdf), [code](https://github.com/dwallet-labs/tiresias).
- IRTF *VDAF-22*: [draft](https://datatracker.ietf.org/doc/draft-irtf-cfrg-vdaf/); IETF *DAP-19*: [draft](https://datatracker.ietf.org/doc/draft-ietf-ppm-dap/). Drafts, не утверждение о финальном RFC.
- *Flamingo*: [paper](https://arxiv.org/html/2308.09883v1). Garg et al., *Lighthouse*: [USENIX Security 2026 paper](https://www.usenix.org/system/files/usenixsecurity26-garg-sanjam.pdf).
- ElectionGuard cryptographic specification paper, 2024: [paper](https://eprint.iacr.org/2024/955.pdf).
