# Независимая проверка C: теория и полнота приватного денежного маршрута

Дата: 2026-09-11. Замороженный HEAD: `177a72ddbea9f2e52eef094405481292ecd56046`.

## Два отдельных verdict

**(a) Теоретическая корректность: условно подтверждаю основные строительные блоки и приведённую арифметику; законченный безопасный протокол пока не предъявлен.** Native Baby-Jubjub внутри BN254 Groth16 и LegoGroth16 CP_link являются обоснованными кандидатами для связи bounded nominal с commitment. Проверяемые VSS-суммы дают числовые S/S_l; показанное взвешенное redistribution сохраняет секрет; closed residual с правильно связанным debit может обслуживать forfeit. Выводы о current-source no-wrap, exact fixed6→fixed18 и условном устранении Fidelity divisions корректны. Это не доказательство malicious/mobile security собственной композиции, durable availability или памяти кошелька. Дополнительная существенная оговорка к конкретному Lego API — C-01.

**(b) Полнота: нет, оснований для «всё необходимое учтено и замечаний нет» недостаточно.** Исследование само правильно оставляет открытыми несколько обязательных protocol/security gates. Дополнительно нужно явно зафиксировать внутреннее ослепление Lego proof, композиционное раскрытие из разрешённых итогов, глобальный supply invariant и судьбу существующих Fidelity index queries. Последние два замечания относятся к полноте interfaces/consumers, а не к опровержению VSS или SNARK.

Ниже severity оценивает риск переноса неполного описания в реализацию. Это не отчёт об исполненных атаках на deployed Outbe.

## Независимость и границы доказательств

- Начал с `DEEP_RESEARCH_AGGREGATION.md`: отказы, receipts, common coverage, repair, rotation, erasure, retention и forfeit. Затем прочитал весь trace, основной research report, pins, evidence manifest и arithmetic script.
- Не читал отчёты других рецензентов, `RESEARCH_COMPLETION_REVIEW.md`, прежние completion/review conclusions и не общался с другими рецензентами. Подагентов не создавал.
- Сам повторил SHA-256 сверку: **24/24 source files и 8/8 research inputs совпали** с `INPUT_MANIFEST.json`.
- Codebase graph tools в моём наборе также отсутствуют. Использован **Verify с direct-source fallback**. Generation/coverage графа не заявляются. Совпадение hashes подтверждает snapshot, но не полноту трассировки.
- Материальные code claims проверены непосредственно в `compute.rs`, `zk_claim.rs`, `tributefactory/runtime.rs`, `tribute/runtime.rs`, `fidelity/runtime.rs`, `fidelity-math/lib.rs`, `metadosis/ocomp/snapshot.rs`, `lysis/program_v1/{phases,execute,finalizer}.rs`, `metadosis/ocomp/activation.rs`, `nod/called.rs`, `nodfactory/runtime.rs`, `gratisfactory/runtime.rs`. Дополнительно прочитаны необходимые участки `gratis/{runtime,schema,api}.rs` и `fidelity/{precompile,api}.rs`.
- Это проверка R00–R14 и названных непосредственных consumers. Не полный аудит Intex/Credis, L2 producer, Oracle, consensus, CE/DA, всех entrypoints и существующей миграции с TEE.
- Первичные интернет-источники исследованы независимо. Web fetch pinned GitHub первоначально давал `Cache miss`; точные immutable файлы Dock и arkworks затем успешно прочитаны прямым read-only HTTPS fetch. Официальный crate source на docs.rs использован как дополнительная проверка, не как замена Git pin.
- Не запускал heavy prover, setup, builds, distributed MPC, реальную rotation, browser/mobile RAM tests или billion-record test. Production/remote state не менял.

## Findings

### C-01 — Medium: transcript Lego CP_link не фиксирует второй обязательный blinder `v`

**Место:** `testing/lysis-privacy-prototype/DEEP_RESEARCH_IMPLEMENTATION.md:124`–141, в частности `link_v=r` и описание `create_random_proof_incl_cp_link`.

**Точное upstream evidence:** pinned Dock `224f195bb8babc2d0de5256135120e0aca9fbd19`, `legogroth16/src/prover.rs:32`–47 и `:358`–379. API принимает и `v`, и `link_v`; сам генерирует только Groth16 randomness `r,s`. Для одного committed witness публичный внутренний элемент имеет вид:

```text
proof.groth16_proof.d = a * gamma_abc_g1[num_instance_variables]
                      + v * eta_gamma_inv_g1
proof.link_d = a*G + link_v*H
```

**Контрпример к небезопасному adapter:** caller передаёт `v=0`, считая, что функция с названием `create_random_proof...` сама рандомизирует весь proof. Внешний `link_d` остаётся ослеплённым, CP_link остаётся связанным с тем же a, но `proof.d=a*K` позволяет публично проверять догадки о небольшом/предсказуемом a. Случайные Groth16 `r,s` не скрывают отдельно опубликованный `d`. Проверяющий не может восстановить приватность уже опубликованного transcript.

**Минимальное исправление:** добавить к нормативному transcript независимый свежий `v←Fr` из CSPRNG для каждого внутреннего commitment/proof; отдельно назвать внешний `link_v` и Groth16 `r,s`, чтобы не смешивать одинаковые имена. Prover adapter должен сам владеть этим sampling. Negative demonstration с намеренно известным `v` должна показывать утечку, а обычные tests — корректную работу с обоими blinders. Это не требует утверждать новый backend или менять P_link statement.

**Статус раскрытия:** новое конкретное замечание к инструкции использования API. Общие setup/negative-test gates в отчёте уже есть, но второй blinder явно не указан. Не утверждаю, что upstream библиотека сломана, что предложенный метод требует `v=0`, или что в Outbe уже существует такой adapter.

**Первичный источник:** [pinned Dock prover.rs](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/prover.rs#L32), [LegoSNARK paper](https://eprint.iacr.org/2019/142).

### C-02 — Medium: C02 необходимо определить относительно совместно наблюдаемых разрешённых outputs

**Место:** `PROTOCOL_TRACE_AND_REQUIREMENTS.md:14`–26, `:285`–290, `:337`–345; `DEEP_RESEARCH_IMPLEMENTATION.md:284`–290; `DEEP_RESEARCH_AGGREGATION.md:193`–203.

Нельзя обещать безусловное сокрытие individual nominal/load только потому, что ни один отдельный proof не раскрывает его. Разрешённые outputs вместе могут однозначно определить сумму.

**Контрпример 1:** лига содержит одного owner. Разрешённое `S_l=a_i` уже раскрывает его nominal. Это следует из принятого C14; это не ошибка реализации и не основание молча отменить разрешение пользователя.

**Контрпример 2, даже без singleton forfeit:** в одном дне/контексте с общим ненулевым f известен `G=Σg_i`. Только один Nod погашен, остальные истекли и включены в разрешённый финальный `F`. Публичные spent/forfeit IDs устанавливают, кто погасил право. Тогда `g_claimed=G−F`, а `a_claimed=g_claimed/(f*10^6)`. Например, nominal `[4,6,10]`, одна claim первого owner, остальные вместе forfeited: `S=20`, `F=16*f*10^6`, следовательно individual claim load и nominal известны. Укрупнение самого forfeit batch здесь не помогает.

**Минимальное исправление:** явно определить leakage function: owners/IDs/terms, `S,S_l`, allowed F, public withdrawals, leagues и статусы; приватность означает отсутствие дополнительных сведений сверх этих outputs и собственной информации corrupt clients. Добавить small-cohort, all-but-one и joint-output примеры. До утверждения F policy показать её последствия для C02. Если требуется более сильное сокрытие, это уже отдельное изменение разрешённых outputs/семантики; выбор такой политики не делается рецензентом.

**Статус раскрытия:** forfeit granularity/timing и запрет произвольных subset queries уже явно открыты; замечание уточняет конкретную недостающую композиционную границу. Формулировка приложения «относительно разрешённых outputs» верна, но её последствия ещё не доведены до C02. Это не «взлом VSS»: контрпримеры используют только публичную арифметику.

**Первичные основания:** исходные формулы trace; [Pedersen VSS](https://cgi.di.uoa.gr/~aggelos/crypto/page8/assets/Pedersen-VSS.PDF) скрывает shares/commitments, но не информацию, логически следующую из открытых результатов; определение statement-relative ZK см. [LegoSNARK](https://eprint.iacr.org/2019/142).

### C-03 — Medium: глобальный Gratis supply — отдельный скрытый state invariant, которого нет в account-only transition

**Место:** `DEEP_RESEARCH_IMPLEMENTATION.md:294`–307; `PROTOCOL_TRACE_AND_REQUIREMENTS.md:306`–320 и `:386`.

**Точный consumer:** `crates/core/gratis/src/runtime.rs:137`–147 (`mint_impl`) проверяет `total_supply.checked_add(event_amount)`, записывает supply и публикует его; `:194`–204 (`burn_impl`) делает `checked_sub`. `crates/core/gratis/src/schema.rs:25`–35 хранит отдельный U256 supply, а `gratis/src/api.rs:32` и precompile выставляют интерфейс чтения.

Описанная account proof доказывает `b_new=b_old+g` и предел U256 для этого owner. Она не доказывает глобальный `T_new=T_old+g`, `T_old=sum live balances` и сохранение прежнего глобального checked-overflow поведения. После удаления публичного T пользователь не знает его opening. Просто сложить Pedersen commitments также недостаточно для полного U256 без no-wrap/bound аргумента.

**Символический counterexample к достаточности локального statement:** два U256 account balances могут каждый оставаться корректным, тогда как их сумма превышает U256; при `T_old=U256_MAX`, `b_old=0`, `g=1` локальная proof проходит, прежняя supply операция отвергает. Это counterexample к interface-level implication, **не доказательство достижимости такого состояния при current-source monetary history**. Для текущего профиля может существовать более тесная lifetime bound; её ещё требуется вывести из всех mint sources, уникальности day budgets и начального состояния.

**Минимальное исправление:** добавить отдельную строку consumer/transition для circulating supply: либо доказанная lifetime conservation/bound позволяет удалить избыточный numeric guard и заменить API по согласованной политике, либо поддерживать скрытый supply с exact update/range proof и достаточным distributed state. Связать его с тем же g/x и атомарным account/Fidelity/residual transition. Не переносить дневной 134/176-bit bound на lifetime supply без доказательства.

**Статус раскрытия:** необходимость убрать утечку supply уже раскрыта на `IMPLEMENTATION:307`; конкретное сохранение глобального integer invariant и исполнитель обновления пока не описаны. Это уточнение открытого consumer, а не новая утверждённая production уязвимость.

**Первичные основания:** указанные локальные source consumers; различие bounded integer arithmetic и field relation подтверждается моделью arithmetic circuits в [ERC-2494](https://eips.ethereum.org/EIPS/eip-2494#abstract). Из внешнего источника не выводится политика supply Outbe.

### C-04 — Low: существующие owner-authorized Fidelity index queries не имеют явной судьбы после удаления TEE

**Место:** `DEEP_RESEARCH_IMPLEMENTATION.md:309`–368 описывает transition и league snapshot; `PROTOCOL_TRACE_AND_REQUIREMENTS.md:194`–217 задаёт R06.

**Точный consumer:** `crates/core/fidelity/src/runtime.rs:155`–182, `query_index_at/query_index_now`, передаёт в enclave owner authorization, timestamp и приватный cohort state. `fidelity/src/precompile.rs:34`–40 выставляет эти calls. Они возвращают Fidelity query result, а не только league slot.

Если считать удаление TEE из relevant Fidelity завершённым только после замены In/Out и league snapshot, эти методы останутся без исполнителя. Оптимизация «12 comparisons и только публичный slot» также не заменяет exact index-query result.

**Минимальное исправление:** явно отнести запросы к сохраняемому интерфейсу (например, wallet-local evaluation по recoverable собственному witness; либо private-output MPC с прежней авторизацией), к изменяемому API или к исключённому scope. Это небольшое решение о границе; оно не требует перетаскивать весь Credis/Intex в текущий прототип.

**Статус раскрытия:** общей оговорки про неаудированные Intex/Credis consumers недостаточно для этого прямого Fidelity API. В R06 конкретно этот consumer не перечислен. Если пользователь сознательно исключает read API из C01, finding становится только пунктом фиксации scope.

**Первичные основания:** локальный `query_index_at`; возможность/границы MPC outputs обсуждаются в [MP-SPDZ protocol documentation](https://mp-spdz.readthedocs.io/en/latest/readme.html). Здесь не заявляется доказанный готовый MPC query implementation.

## Что подтверждено по полной цепочке

### R00–R02: источник и admission

- `tributefactory/runtime.rs:105`–127 действительно различает verified root и незарегистрированные/disabled ветки. `validate_zk_result:282`–307 сравнивает canonical hashes и вызывает настоящий `verify_circuit::<FullProof>`. Требование закрыть альтернативные admission paths без равноценной source authorization правильно.
- `zk_claim.rs:27`–41 и `:53`–74 подтверждают private draft ID, base/remainder, SU set и binding к sender/chain. Источник должен доставлять draft кошельку; публичный hash не доказывает secrecy маленького preimage. Проверка реального L2 payload/seed entropy остаётся открытой и явно заявлена.
- `compute.rs:118`–144 подтверждает current nominal formula, positive VWAPs, U512 arithmetic, отказ при positive amount→zero nominal. `parse_canonical_amount:160`–177 подтверждает u64 base и remainder<M. Отсюда `a<=u*M<2^104` и `S<2^134` для миллиарда корректны. Тип uint256 сохраняется; произвольный U256 source — иной профиль.
- `tribute/runtime.rs:334`–352 подтверждает текущие numeric counter/event updates. В target admission same-C binding, duplicate/SU checks, pending/accepted distinction и атомарная durable запись обязательны. Это правильно описано; готовой реализации не показано.

### R03–R07: хранение, rotation, S и поздние группы

- Pedersen VSS с двумя polynomial families и `A_0=C(a)` математически связывает admitted commitment с долями при корректных subgroup/canonical checks и неизвестном `log_G(H)`. Shares равномерны в scalar field; 104-битное packing отдельной share неверно.
- `f<t`, `Q−f−d>=t`, отдельное условие достижимости Q и общий full-coverage roster правильно разделены. Контрпример отсутствия общих holders не проигнорирован: `AGGREGATION:75`–103 даёт explicit weighted redistribution и Ready gate.
- Алгебра `B_h,0=λ_h*E_i(h)` и сумма новых полиномов сохраняют `(a,r)`. Согласованный helper set, fresh retries, attempt/epoch binding и запрет смешивания разных полиномов необходимы и указаны. Маленькая проверка над F101 восстановила constants `[17,23]`; это только sanity check линейной алгебры.
- Здесь нет полного malicious/mobile handoff theorem для этой конкретной адаптации. CHURP/DPSS подтверждают существование подходящих семейств в собственных моделях; secure erasure, backups/WAL, old/new overlap, forward-secret channels и durable cutover нельзя заменить подписью ACK. Исследование это корректно признаёт.
- VSS раскрывает числовой S интерполяцией; commitment alone или lifted ElGamal дал бы точку. No-wrap достаточен при verified current source/count. Paillier имеет большой plaintext domain и numeric decrypt, но ciphertext linkage и same-key proactive handoff остаются самостоятельными обязанностями. Static Tiresias proof не закрывает mobile model.
- Late league требует сохранить различимость records до snapshot. Один S недостаточен. Открытие S_l должно быть связано с точным owner partition, snapshots и sum commitments; одного равенства `sum S_l=S` недостаточно. Это всё присутствует.

### R06: Fidelity

- `fidelity-math/lib.rs:48`–88 подтверждает A/D, sold-history contribution, nested floors и league clamp; один current balance не заменяет историю. Условная формула с public times правильно сохраняет nested floors.
- Независимо повторён скрипт: **137117 cases**, включая исчерпывающую малую область и 20000 больших deterministic cases. Он проверяет целочисленную идентичность, не MPC privacy, не current U256 failure semantics и не actual cohort transition.
- `slot>=m iff A*K>=ceil(ceil(m*w/4096)*K/z)*D` верно при `D,z,w>0`. До 12 comparisons относится к поиску по 4096 slots. Нулевые ветки должны обрабатываться корректно; если D скрыт, нельзя публично раскрывать `D==0` лишь ради shortcut. Дополнительные zero guards и широкая arithmetic не становятся бесплатными от этого тождества.
- Public timestamps/padded slots требуют отдельной принятой leakage policy. Lifetime sold history и 325+ bit intermediates нельзя заменять nominal bound. Authenticated VSS→MPC input conversion и privacy distributed prover обязательны. Correct final SNARK не доказывает безопасность процесса его распределённого создания.
- Snapshot source сейчас монолитно перечисляет день (`snapshot.rs:35`–49), Fidelity runtime копирует все entries (`runtime.rs:108`–120). Миллиардный production путь потребует chunked/provably complete snapshot, согласованного с одним state root/time. Исследование уже ставит full-job/DA/Fidelity scaling gate; 256-record Lysis tasks сами по себе этот bottleneck не устраняют.

### R08–R11: денежный kernel, Nod и conservation

- При раскрытых S_l обычный deterministic kernel достаточен. Exact `g18=a6*f6*M`, `c18=a6*f6*p6` и `G18=sum S_l*f_l*M` алгебраически корректны. Public monetary normalization должна проверять именно новую exact модель; старые per-value floors переносить нельзя.
- `phases.rs:358`–458 подтверждает текущие target/Fidelity/price checks и numeric calculation. `output_finalize:673`–707 подтверждает независимый contributor stream с nominal. Замена на commitment нужна и там, даже если дальнейший Intex вне scope.
- `finalizer.rs:255`–267 и `activation.rs:323`–357 подтверждают реальные additional totals и consumers. Cost зависит от цены; `S_l` не заменяет mixed-price weighted sum. Numeric prefixes могут быть избыточны для одного `prefix<=B` при completeness/nonnegative G, но их снятие требует новой certification/schema проверки.
- Nod descriptor может переиспользовать owner-created C(a); offline worker не обязан знать opening или выпускать новый amount commitment. Он всё равно обязан доказанно выпустить ровно все допустимые права и проверить term binding.
- Cost/asset/zero policy до выпуска ещё не выбрана. Нельзя переносить ошибку на будущую невозможность claim. Это уже явно открытый gate, не найденная новая арифметическая ошибка.

### R12–R14: claim, private payment, Gratis и forfeit

- `nodfactory/runtime.rs:183`–236 и `:262`–288 подтверждают owner/PoW/qualification/deadline, payment spender/asset/cost binding и реальные plaintext event deltas. Target должен изменить весь этот путь.
- Payment subledger18 с публичным deposit6→private units18, exact spending и остатком реализуем как новая accounting модель. Это требует принятия протокольного изменения и полного backed-reserve/conservation интерфейса. Текущий PayNote сам этого не предоставляет.
- Четыре 64-bit balance limbs и salted state commitment допускают полный U256 account. Old-state authorization, initial zero proof, overflow, replay/version и atomic payment/Fidelity update правильно перечислены. Сопутствующий global supply — C-03.
- `gratisfactory/runtime.rs:151`–163 действительно ещё умножает Gratis6 при выводе COEN18; для target Gratis18 повторный множитель недопустим.
- Initial residual groups можно сформировать из existing shares после фиксации коэффициентов/terms. Fresh debit sharing не требует от claim owner знать текущие group polynomials; требуется proof одного amount, availability/canonical epoch и атомарное обновление residual root вместе с payment/Nod/Gratis/Fidelity.
- Сжатие до O(K_active) корректно только при одинаковой future selection/lifecycle семантике. K может быть O(N). `nod/called.rs:241`–295 реально обрабатывает bounded member subsets и возвращает credit за каждый проход; агрегат целой группы не обслуживает произвольный partial pass без дополнительного состояния или принятого timing change. Это уже отражено в research.
- `0<=F<=G<=B<=0.32*S18` даёт заявленный безопасный native-field bound при current rules. Независимо рассчитанные максимумы даже теснее консервативных: billion budget имеет 172 бита, full-u32-count budget — 175 бит; отчётные `<2^174`/`<2^176` безопасны. Не следует называть их ошибкой.

## Полная матрица Q1–Q7

| Q | Теория в предъявленном scope | Полнота/обязательное продолжение |
|---|---|---|
| Q1 source/P_link | Native field и CP_link пригодны при exact integer relation, correct codec, same source fields, subgroup checks и setup assumptions | Full cold RAM не измерен; источник/seed/SU bound открыты; внутренний Lego blinder C-01; registry/layout adapter ещё не реализован |
| Q2 S | VSS numeric recovery/no-wrap и algebraic coverage repair подтверждаются | Malicious/private repair, mobile theorem выбранной интеграции, durable cutover и throughput открыты |
| Q3 late S_l | Индивидуально различимое retained state + authenticated partition достаточно | Packed regrouping/rotation cost; authorized-output inference C-02; ни DA, ни late snapshot не выполнены на масштабе |
| Q4 Fidelity | Exact arithmetic и условная comparison identity корректны | История, bounds, zero guards, hidden indexing, VSS→MPC binding, publicly verifiable private prover, сохранение API C-04 |
| Q5 Nod/claim | Same C(a), immutable terms, exact g/c и U256 limb account обоснованы | Cost/zero/denomination policy, полный payment+Fidelity transition, global supply C-03; платежный subledger ещё не выбран |
| Q6 forfeit | Residual и linked private debit достаточны для согласованных групп | Granularity/timing F, completeness, atomic expiry/claim, retention и joint-output leakage C-02 |
| Q7 verifier/DA | Versioned suite/VK bundle, exact public layout и DA checkpoints необходимы; не противоречат кандидатам | Нет полного wire schema, runtime limits, replay/bootstrap/DA evidence, выполняемых certificates и migration plan |

## Полная матрица C01–C14

| C | Результат проверки |
|---|---|
| C01 убрать TEE | Архитектурно возможно; реализованного полного замещения нет. Fidelity evaluator/private state и API C-04 остаются в checklist |
| C02 скрытые individual values | Commitments/proofs подходят условно; source hashes/payment/supply/events требуют secrecy audit. Безусловная secrecy конфликтует с inference из разрешённых outputs: C-01/C-02/C-03 |
| C03 точный S | Подтверждён VSS numeric method с same-C, completeness, no-wrap; operational/mobile obligations открыты |
| C04 deferred Nod | Descriptor с исходным C(a), аутентифицированными f/p/terms корректен; invalid cost/zero/completeness ещё требуют protocol policy |
| C05 proofs пользователя, offline после admission | Для выпуска Nod/expiry сеть может иметь достаточные shares. Offline Fidelity требует сетевого вычисления/proof/certificate; blanket «ноды только проверяют» не описывает всех ролей |
| C06 individual uint256 | Current proof domain математически уже: 104 бита, что соответствует разрешённому source-derived bound. Arbitrary U256 source нельзя впускать в один scalar без иной encoding |
| C07 до миллиарда | Арифметика расходов корректна; реальной измеренной системы на миллиарде нет |
| C08 50h и rotation | Retention, 12h waiting, overlap, repair и мобильный adversary обозначены. Hourly rotation — сценарий, не deployment fact |
| C09 fixed6→fixed18 | Exact formulas верны; costs/payment/Fidelity widths и simultaneous migration consumers остаются отдельными obligations |
| C10 ≤512000000 B | Не подтверждено: новые полные P_link не измерены. Source-only 398.049 MB не full-proof result |
| C11 публичный COEN | Balance subtraction с public x реализуемо; всё atomic, multiplier6→18 убрать. Public withdrawal/league/inference входит в leakage policy |
| C12 SEAL исключён | Соблюдено; оснований возвращать его в shortlist нет |
| C13 individual и day uint256 | Current source+count гарантирует day bound; field wrap не используется как обход. Другой input profile требует иной admission invariant |
| C14 открыть S/S_l | Соблюдено; kernel может оставаться public. Разрешение не доказывает отсутствие inference и не распространяется само на любые cost/prefix/F totals |

## Масштаб, измерения и параллелизм

Независимо воспроизведены расчёты: 5555.56 admission/s для одного 50h окна; 11574.07/s для миллиарда в каждый календарный день; 3906250 tasks по 256 **records**; 32 GB одиночных commitments; 64 GB share pairs на holder; 1.024 TB для 16 holders как пример; 36.5N record moves для оговорённого hourly сценария. Формулы full Tribute A/B правильно учитывают C(a) один раз, а P_L2/meta/receipts отдельно. 128 B Groth16 и 224 B Lego — cryptographic elements, не wire report.

Эти числа не задают принятые n/t/f/Q, threshold6, 16 validators или SU cap4. Prover capacity, verification, DA, durable writes, common coverage и rotation могут ограничивать ingest независимо. Один account и один residual group вводят state-version conflicts; parallel wallets/workers не означают независимые записи в shared state. Исследование учитывает account conflicts; при измерениях следует также считать group/residual update contention.

Оценка 190 B Nod descriptor — иллюстрация, а не готовая self-contained запись. Terms root требует доступных данных. 960 B Merkle path при миллиарде — порядок размера пути, не обязательные отдельные bytes хранения для каждого Nod. Retention должен включать overlapping days, Fidelity history, residual groups, current witnesses, DA/replay и obsolete secret erasure. Одного дневного proof/opening для pruning недостаточно.

## Уже раскрытые ограничения, которые не переименовываю в новые находки

1. Full P_link cold RAM и реальный runtime/device не проверены.
2. Не выбран полный malicious/mobile VSS/repair/handoff protocol; Shamir algebra, CHURP reference и ACK не являются его доказательством.
3. Fidelity history format, bounds, exact private evaluation и публичная проверяемость ещё не замкнуты.
4. Деноминация18 приватного платежа, cost/zero/asset policy и closed transition не утверждены.
5. Forfeit timing/granularity и весь residual lifecycle ещё не реализованы.
6. Wire, DA/replay/bootstrap/certification и нагрузочные SLA не подтверждены.
7. Expanded CRS/security target и canonical adapters требуют review; ни одна библиотечная лицензия/README не означает аудит Outbe composition.

Отсутствие этих benchmark/реализаций не опровергает алгебру. Но оно исключает verdict «методы уже покрывают все условия как законченная безопасная система».

## Независимо просмотренные первичные источники

- [Pedersen VSS, 1991](https://cgi.di.uoa.gr/~aggelos/crypto/page8/assets/Pedersen-VSS.PDF): original VSS source; PDF в web выдаёт скан без текстового extraction. Детальный security theorem по скану в этой проверке не реконструировался; собственная algebra check не подменяет его.
- [CHURP, авторская страница](https://www.fanzhang.me/publications/19-churp/): changing committees, optimistic communication и наличие formal proof в своей модели; [DPSS 2015/304](https://eprint.iacr.org/2015/304): amortized dynamic proactive sharing и corruption-rate framing.
- [Tiresias 2023/998, §1.2 и §1.3](https://eprint.iacr.org/2023/998.pdf): static malicious corruption, threshold numeric decryption и integer key sharing. Не доказательство proactive Outbe handoff.
- [LegoSNARK 2019/142](https://eprint.iacr.org/2019/142): CP-SNARK framework; [Dock pinned prover](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/prover.rs), [verifier](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/verifier.rs), [structures](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/data_structures.rs): фактические API, public d, caller-supplied blinding, input-length upper bound и два VK arguments.
- [ERC-2494](https://eips.ethereum.org/EIPS/eip-2494): coordinate/scalar distinction, subgroup order/cofactor; [pinned arkworks curve](https://github.com/arkworks-rs/algebra/blob/4d708c733d9340dedf3b3a8b41718d3370d8cf46/curves/ed_on_bn254/src/curves/mod.rs), [native Fq alias](https://github.com/arkworks-rs/algebra/blob/4d708c733d9340dedf3b3a8b41718d3370d8cf46/curves/ed_on_bn254/src/fields/fq.rs): реальная нормализованная модель a=1 и Fq=BN254 Fr.
- [Malicious Security in Collaborative zk-SNARKs, CRYPTO 2025](https://eprint.iacr.org/2025/1026): invalid-witness и compiler-composition privacy pitfalls; positive results имеют собственные условия. Correct final proof не означает private distributed proving.
- [RFC 9180, §9.7](https://datatracker.ietf.org/doc/html/rfc9180#section-9.7): границы HPKE, включая отсутствие встроенных replay/forward-secrecy гарантий. Это транспортный компонент, не DA/VSS/backup recovery theorem.

Изменён только этот новый отчёт. Все выводы ограничены source/algebra/API evidence, перечисленными assumptions и явно неисполненными runtime/security gates.
