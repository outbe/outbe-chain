# Независимая проверка A: теория и полнота приватного денежного маршрута

Дата: 2026-09-11. Входы: `INPUT_MANIFEST.json`, HEAD `177a72ddbea9f2e52eef094405481292ecd56046`. Все 8 хешей исследовательских входов самостоятельно сверены и совпали. Проверка проведена без чтения прежних completion/review заключений и отчётов других рецензентов, без общения с другими рецензентами и без подагентов. Production не изменялся.

Graph MCP в моём доступном наборе отсутствует. Использован Tier Verify source fallback: прочитаны полные заданные исследовательские документы, материальные функции денежного маршрута и обнаруженные непосредственные потребители общего Gratis/Fidelity state. Это не аудит всего репозитория и не заявление о graph coverage. Память прежних обсуждений не использовалась.

## 1. Два отдельных verdict

**(a) Теоретическая корректность: базовые конструкции условно корректны; полного доказанного протокола пока нет.** Не обнаружено ошибки в current-source bounds, native Baby-Jubjub linkage как конструкции, commit-and-prove направлении LegoGroth16, линейной VSS агрегации, формуле weighted resharing, exact 6→18 формулах, residual-group преобразовании или тождестве Fidelity threshold comparisons. Однако эти положительные результаты действуют при явно заданных ranges, binding, accepted-set coverage, randomness, state authority и adversary assumptions. В частности, конкретному Lego prover adapter не хватает требования к внутренней blinding randomness — A-02. Malicious/mobile theorem для составного storage/repair/MPC протокола предъявлен не был, и документы правильно этого не обещают.

**(b) Полнота: нет, утверждать «всё необходимое учтено, замечаний нет» нельзя.** Помимо честно открытых prototype/security/payment/measurement gates найдены два конкретных незамкнутых интерфейса: внешние обновления тех же Gratis/Fidelity accounts без владельца (A-01) и создание доказанного cohort state с точным ещё неизвестным inclusion timestamp (A-03). Они требуют архитектурного ответа до сквозной реализации. Отсутствие нового полного 512 MB benchmark — непроверенное требование, а не логическая ошибка метода.

## 2. Новые замечания

### A-01 — High: owner-only account transition не покрывает уже существующие внешние Gratis/Fidelity mutations

**Где в исследовании:** `DEEP_RESEARCH_IMPLEMENTATION.md:292–317` предлагает wallet-owned hash commitment полного баланса и передачу shares Fidelity после owner transition; таблица R00–R14 (`:47–63`) содержит только claim/mint и COEN burn для этих переходов. В `:41` и `:262` честно исключён аудит остальных Intex/Credis consumers. Это ограничивает вывод о полноте: исключённые операции непосредственно изменяют тот же объект, который предлагается заменить.

**Exact source/consumer chain:**

- `crates/core/gratisfactory/src/runtime.rs:71` / `pledge_gratis` вызывает `gratis::pledge_with_fidelity` (`:105`), дебетует liquid balance и делает текущий Fidelity Probe; `unpledge_gratis` (`:116`) возвращает pending collateral.
- `crates/core/gratis/src/runtime.rs:423` / `release_to_eoa` читает **оба** текущих account blobs (`:435–436`), обновляет balance и pledged state (`:439`). Это не owner-authorized mint.
- `crates/core/credisfactory/src/runtime.rs:190` / `settle` разрешает платёж третьего лица; его документация `:180–186` явно утверждает этот контракт. После погашения он вызывает `release_to_eoa` (`:245–249`) владельцу collateral.
- `crates/core/credisfactory/src/runtime.rs:266` / `void_position` без владельца вызывает `burn_pledged_with_fidelity` (`:284–290`), изменяет **тот же** cohort state и возвращает сумму в PromisLimit (`:293–294`). Автоматический consumer — `crates/core/credisfactory/src/called.rs:63` / `run_daily` и `scan_and_call` (`:74`).
- `crates/core/gratis/src/runtime.rs:388` / `consume_pledge` также должен изменять pledged state после отдельной ticket authorization. `:356–377` восстанавливает EOA через enclave; его замена тоже необходима выбранному пути.

**Контрпример.** У owner есть 100 Gratis в cohorts. Он pledges 60: liquid balance становится 40, а сами active cohorts при pledge не продаются. Owner отправляет Tribute и уходит offline. До READY кредит истекает: текущий `void_position` обязан убрать 60 из pledged ledger и из Fidelity cohorts. Если сохраняется только предлагаемое «владелец доказывает каждое новое состояние», выполнить этот переход некому. Если его пропустить, следующий league snapshot вычисляется по неверной истории. При погашении кредита третьим лицом возникает другая форма того же дефекта: необходимо кредитовать liquid balance по `C_balance=H(..., limbs, salt)`, но payer/validator не знает старые limbs/salt владельца и не может сам сформировать корректный новый hash commitment.

Даже публичная сумма collateral не даёт opening прежнего hash commitment. Commitment к Fidelity и shares только для snapshot автоматически не определяют право/протокол обновления balance, pledged ledger, tickets и нового recoverable wallet witness.

**Минимальное исправление.** Добавить в state machine все writers этих accounts: pledge, unpledge, consume, third-party release, automatic collateral burn и соответствующие Fidelity transitions. Выбрать проверяемый механизм внешнего credit/debit: например, pending incoming credits, permissioned distributed transition по достаточному shared state или иную явно согласованную модель; зафиксировать авторизацию, conservation liquid+pending+pledged, изменение roots/versions, доставку обновлённого witness владельцу и units18. Альтернатива — явно изменить/ограничить эти входные пути отдельным решением, а не считать их совместимыми по умолчанию. Полного экономического аудита Credis для обнаружения этой зависимости не требуется.

**Что уже было открыто:** общий scope disclaimer Intex/Credis есть. Конкретные owner-offline external writers, их требуемые witnesses и конфликт с выбранной owner-only hash-account моделью в матрице Q1–Q7 не разобраны. Это замечание к композиции/полноте, не доказанный exploit production.

**Первичные свидетельства:** перечисленные текущие source functions; внешняя криптографическая статья не может подтвердить внутренний business consumer.

### A-02 — Medium, privacy-critical integration requirement: у LegoGroth16 нужны две blinding randomness, а пример задаёт только внешнюю

**Где:** `DEEP_RESEARCH_IMPLEMENTATION.md:124–137` задаёт `commit_witness_count=1`, первый witness `a`, внешние `[G,H]`, `link_v=r` и проверку CP_link. Не задан безопасный выбор отдельного `v` для внутреннего `proof.d`. Название `create_random_proof_incl_cp_link` легко принять за генерацию всей случайности.

**Проверенный pinned primary source:** Dock commit `224f195bb8babc2d0de5256135120e0aca9fbd19`, [prover.rs](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/prover.rs), `create_random_proof_incl_cp_link:32–47` принимает **оба** `v` и `link_v` от caller и самостоятельно выбирает только Groth16 `r,s`. В `create_proof_and_committed_witnesses_with_assignment:358–368`:

```text
D = a * gamma_abc_g1[first_committed_index] + v * eta_gamma_inv_g1
```

`D` публично присутствует в `ProofWithLink.groth16_proof.d`; [data_structures.rs:8–26](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/data_structures.rs). Независимый внешний `link_d` не скрывает утечку через D.

**Контрпример конфигурации.** Adapter следует приведённому примеру для `link_v`, а недокументированный `v` заполняет нулём. Тогда публично `D=a*K`, где K известен из VK. Наблюдатель проверяет каждую вероятную сумму `a*` сравнением `a* K == D`. Это детерминированный dictionary oracle для небольшого пространства денежных гипотез, даже при совершенно свежем внешнем `C(a)=aG+rH`. Случайность остальных частей Groth16 не изменяет D. Знание неизвестного discrete log между G и H для атаки не требуется.

Это не утверждение, что Dock обещает безопасность при неправильном v, и не заявленный выполненный full-proof exploit. Это конкретное omission в рецепте интеграции. В [LegoSNARK, §4.1, Theorem 4.1](https://eprint.iacr.org/2019/142.pdf) CP_link связывает разные Pedersen commitments вместе с их openings; наличие linking proof не заменяет hiding randomness каждого публикуемого commitment.

**Минимальное исправление.** Показать полный вызов adapter: свежий CSPRNG `v_internal` для каждого proof, отдельно `link_v=r_external`, и библиотечные `r,s`. Прятать опасные параметры в prover adapter, документировать домены/жизненный цикл/запрет fixed or reused v. Добавить source-level/API проверку этого контракта и regression на повторные proofs одинакового a: внутренний D не должен стать детерминированным. Верификатор не может доказать качество случайности prover постфактум.

**Что уже было открыто:** setup, witness order, wrong openings, canonical decoding и VK bundle обозначены. Отдельный caller-supplied internal hider в примере отсутствует. Сами поля/размеры ProofWithLink и общий CP_link подход подтверждены.

### A-03 — Medium: отсутствует контракт получения exact transition timestamp до wallet proof и VSS upload

**Где:** `PROTOCOL_TRACE_AND_REQUIREMENTS.md:203–205,306–318` сохраняет cohort timestamps `now` и user proof Fidelity transition; `DEEP_RESEARCH_IMPLEMENTATION.md:61,301–317` говорит о новом commitment/shared state после пользовательского proof. В `:542` clock binding назван проверкой, но не указан производитель доступного кошельку времени и способ привязать к нему новое состояние.

**Exact source:** `crates/core/gratisfactory/src/runtime.rs:137–140` и `:156–160` выбирает `now=storage.timestamp()` внутри исполняемого mint/burn. `bin/outbe-tee-enclave/src/fidelity.rs:156–167` записывает этот timestamp в новую active cohort; `:174–190` записывает его в sold slices. `crates/core/fidelity-math/src/lib.rs:48–76` затем использует именно эти времена в численных результатах.

**Контрпример интерфейса.** Wallet формирует private cohort witness и hash/shares нового state с acquired_at=T, строит proof и отправляет транзакцию. Её фактическое включение происходит в блоке с T+3. Строгий verifier actual `now` отвергнет заранее построенное состояние. Принять T без проверки — изменить cohort age semantics. Заменить поле на T+3 после доказательства без дополнительного механизма — потерять binding proof/new commitment/shared state. То же относится к sold_at при COEN withdrawal и может менять exact league на границе. Это не абстрактная проблема latency; в полностью скрытом формате timestamp входит в witness нового состояния.

**Минимальное исправление.** Задать точный transition contract. Возможный путь — wallet доказывает amount/order/update payload, а runtime канонически добавляет публичный inclusion timestamp к проверенным opaque cohort commitments и вычисляет root; будущему wallet и committee должны быть доступны соответствующие openings/metadata и проверенная структура. Если timestamps должны оставаться скрыты, нужен иной construction, например distributed completion нового state после inclusion. Ещё один вариант — утверждённый logical timestamp с точным правилом freshness; это уже изменение правил времени. Во всех вариантах нужно связать последующий snapshot с frozen state root, anchor и evaluation timestamp, не заставляя owner приходить после admission.

**Что уже было открыто:** public/private timestamps и clock binding честно названы. Отсутствует именно поток получения конкретного времени для описанного wallet-produced state, и ни padded history, ни 12-comparison identity сами по себе этот интерфейс не реализуют. Замечание не утверждает невозможность решения или дефект математического тождества.

## 3. Матрица Q1–Q7

Статусы: «покрыт» означает обоснованный результат в ограниченном scope; «условен» — конструкция есть, но необходимые предпосылки/интерфейс ещё не закрыты; «не подтверждён» — требуемого измерения/поставки/протокола нет.

| Пакет и trace | Проверенный результат | Статус / остаток |
|---|---|---|
| Q1, R00–R02 | Current codec/formula даёт 104-bit a; scalar Baby и BN254 больше суммы. Native-coordinate P_link и CP_link доступны как семейства. Same public hash/context composition допустима при knowledge soundness и canonical encodings. | Условен. P_L2 root authority всех путей, delivery/privacy draft seed, полный max SU statement и cold 512 MB не подтверждены; A-02. |
| Q2, R02–R05 | A0=C(a), polynomial checks, scalar interpolation с no-wrap достаточны для точного S известного accepted set. Q receipts и common coverage — разные условия; арифметика repair сохраняет a/r. | Условен. Конкретный malicious/mobile repair/resharing, liveness/corruption/erasure model и durable atomic service не предъявлены. Это честно открыто. |
| Q3, R06–R10 | Один S недостаточен для произвольного позднего owner→league. Individual/packed/ciphertext state и правильный snapshot позволяют получить S_l. Membership/coverage, а не только ΣS_l=S, обязательны. | Покрыт линейный принцип; retention/packing/handoff/certification на масштабе не подтверждены. |
| Q4, R06, R12–R13 | Cohort/LIFO state нужен помимо баланса. A,D линейны только при разрешённых публичных time weights; threshold identity корректна, поиск по 4096 slots требует до 12 сравнений при обработанных особых ветках. | Условен. A-01/A-03, lifetime bounds, hidden access, authenticated input, malicious MPC и публично проверяемый distributed prover остаются. |
| Q5, R08–R13 | g=a·f·10⁶ и c=a·f·p — точная конверсия fixed6→fixed18. G=ΣS_l·f_l·10⁶ и его нормализация не требуют индивидуальных g. Full uint256 hash/limb account возможен. | Условен. Cost/zero/unavailable terms policy, private payment18 и текущие внешние account writers; A-01/A-03. |
| Q6, R09–R14 | Closed residual groups с одинаковыми lifecycle/weights, fresh linked debit и nullifiers позволяют после удаления individual shares сделать offline-owner forfeit. F≤G≤B<q при current profile. | Условен. Точный issued/remaining set, availability на debit, forfeit granularity/timing и правило сохранения individual state до преобразования. Не закрыто новой runtime реализацией. |
| Q7, все R | Единый registry VK bundle, точный input count, subgroup/canonical bytes, roots, DA, rollback/versioning правильно требуются. Metadata/proof bytes не равны всей стоимости state. | Не подтверждён production путь. Wire schema, resource admission, replay/bootstrap, snapshot scheduling и complete state-machine adapters ещё нет. |

## 4. Матрица C01–C14

| ID | Результат независимой проверки |
|---|---|
| C01 | Условен. Для R00–R14 названы TEE-free направления; связанные writers Gratis/Fidelity не замкнуты — A-01. |
| C02 | Условен. Pedersen/hash hiding можно обеспечить; нельзя оставлять public source hash dictionary oracle, event/supply/payment delta. A-02 — дополнительный канал. Приватность относительна разрешённым outputs. |
| C03 | Покрыт теоретически для current source + принятого manifest + достаточного VSS state: восстанавливается integer S, не discrete log. |
| C04 | Покрыт как descriptor/claim интерфейс: прежний C(a), immutable terms; остаётся полная certification реализация. |
| C05 | Условен. S/S_l/Nod/forfeit могут работать без вернувшегося owner при retained state; Fidelity и внешние state transitions требуют дополнительных интерфейсов. |
| C06 | Покрыт для текущего source. uint256 — storage/верхний предел, а доказанный tighter bound — 104 бита. Arbitrary uint256 source не доказан этим профилем. |
| C07 | Не подтверждён нагрузочно. Расчёты действительно используют N records, 3 906 250 shards; billion-record исполнения нет. |
| C08 | Условен. Учтены 50 h, 12 h waiting и ротации; hourly cadence — сценарий. Mobile handoff implementation/theorem не выбран. |
| C09 | Формулы покрыты алгебраически; миграция всех денежных consumers и payment denomination не завершена. |
| C10 | Не подтверждён. Ни один новый полный P_link A/B с cold PK load не измерен ≤512 000 000 B. Source-only 398 MB нельзя зачесть. |
| C11 | Формула b_old=b_new+x и публичный COEN x корректна; требуется current state/range/auth и Fidelity transition. Повторный ×10¹² недопустим. |
| C12 | Покрыт: SEAL исключён. |
| C13 | Покрыт для current codec + checked count: S<2¹³⁶ уже ниже uint256. Для альтернативного arbitrary uint256 source нужен новый admission overflow contract. |
| C14 | Покрыт: после closed set/Snapshot открываются S/S_l и остаётся public integer Lysis kernel. Разрешение не распространяется автоматически на shard/epoch/reclaim totals. |

## 5. Что проверено без новых замечаний в ограниченном scope

**R00–R02 и source authority.** `compute.rs:118–177` подтверждает canonical u64 base, remainder<10⁶ и checked U512 nominal formula. Из e≥vR и vI≥1 действительно следует a≤u·10⁶, а не произвольное новое ограничение схемы. `zk_claim.rs:27–79` связывает draft hash и binding; `tributefactory/runtime.rs:104–128,282–307` подтверждает различие registered-enabled и bypass branches. Документы корректно требуют заменить bypass для приватного admission, не выдают подпись произвольного root за независимую истинность источника. L2 delivery/secret seed фактически не проверены в этом review. `tribute/state.rs:317–357` подтверждает checked count/total, `tribute/runtime.rs:319–414` — issue/burn и monetary event projection.

**Криптографические поля и P_link A/B.** Pinned [arkworks Baby curve](https://github.com/arkworks-rs/algebra/blob/4d708c733d9340dedf3b3a8b41718d3370d8cf46/curves/ed_on_bn254/src/curves/mod.rs) действительно имеет a=1, d=168696/168700, cofactor 8; [scalar modulus](https://github.com/arkworks-rs/algebra/blob/4d708c733d9340dedf3b3a8b41718d3370d8cf46/curves/ed_on_bn254/src/fields/fr.rs) совпадает с указанным q251. Native coordinates не отменяют r<q, subgroup checks или integer limbs/carries. Dock source подтверждает первые committed witnesses, внешние bases, два verifier VK arguments и пять G1+один G2 в ProofWithLink. Указанные 128 B/224 B корректны для соответствующего compressed BN254 encoding, но не для полного сообщения. Кроме A-02 новой ошибки в описанном adapter контракте не найдено. Trusted setup/extended CRS и security target остаются assumptions, а не результатом README.

**VSS, common coverage, rotation.** [Pedersen 1991, §4, p.133–134](https://cgi.di.uoa.gr/~aggelos/crypto/page8/assets/Pedersen-VSS.PDF) задаёт именно secret pair shares, coefficient commitments, проверку shares и интерполяцию opening. Same-C check необходим и описан. Q−f−d≥t обеспечивает per-record recovery, а common holder coverage необходимо отдельно; четырёхзаписный контрпример документов верен. [Groth 2021/339, §2.5, pp.7–8](https://eprint.iacr.org/2021/339.pdf) подтверждает sub-sharing + Lagrange recombination. Приведённая двойная Pedersen адаптация сохраняет constant commitments. Алгебра не подтверждает злоумышленную реализацию complaints, adaptive retries, forward secrecy или erasure. [CHURP, §2.2](https://eprint.iacr.org/2019/017.pdf) явно учитывает одновременный old/new corruption; [DPSS, §4](https://eprint.iacr.org/2015/304.pdf) содержит переходные ограничения степени/размера групп. Документы правильно не заменяют эти условия новым signing DKG.

**Альтернативы агрегации.** [Tiresias, §1.2](https://eprint.iacr.org/2023/998.pdf) формулирует static malicious adversary; citation не доказывает mobile same-key handoff. Paillier ciphertext при 3072-bit modulus имеет 6144-bit container, то есть 768 B; широкое plaintext пространство не заменяет связку R01/C(a)/ciphertext. Exponential ElGamal выдаёт group element, generic 134-bit interval DLog стоит порядка 2⁶⁷ операций. Packed state не гарантирует бесплатный arbitrary regrouping. Отрицательные выводы о применении этих семейств как готового полного Outbe протокола обоснованны; самостоятельного implementation/security audit всех названных библиотек я не делал.

**R05–R10.** `metadosis/settlement.rs:32–90` и `ocomp_budget.rs:41–86,133–159` подтверждают разницу local auction_base и authoritative request split, сохранение K и невозможность молча взять меньше receipt. `lysis/program_v1/execute.rs:392–459` действительно делает share rounding, public coefficient kernel и другую нынешнюю monetary normalization. Целевое g18 линейно без individual division; дополнительная exact normalization не увеличивает G. `phases.rs:358–476` подтверждает target availability, price, league equality, zero-load/cost checks и расчёт floor price. `finalizer.rs:241–267,566–663` действительно содержит eligible/cost totals, chunk counts/order/completeness; `activation.rs:360–407` связывает generation, receipts, retirement и budget credit. Документы обоснованно не предлагают заменить эти проверки одной равной суммой. Zero f, cost overflow и unavailable asset честно остаются решениями до выпуска права.

**Fidelity.** `fidelity-math/lib.rs:48–88,164–195` подтверждает A/D, saturating time differences, checked U256 intermediates, nested floors и exact T table arithmetic. `fidelity.rs:156–223,300–314` подтверждает LIFO, sold slices и общий snapshot. Тождество для m>0 выводится последовательно через floor(X/Y)≥k ⇔ X≥kY; особые D/z/w/qualified_start ветки необходимо сохранить отдельно. «12» — максимум бинарного поиска по slot, не общий benchmark MPC включая D==0 и input preparation. Public-time вариант условен: раскрытие cohort completion/splits может выдать больше league. A/D для полного uint256/lifetime history не помещаются автоматически в native scalar; нужны wide exact integers, не probabilistic truncation.

**MPC result verification.** Active-secure MPC certificate и publicly verifiable SNARK — разные trust models. [Garg et al., CRYPTO 2025, §1.1.1](https://eprint.iacr.org/2025/1026.pdf) действительно показывает invalid-witness и reactive-output privacy pitfalls; проверка одного готового SNARK не удостоверяет приватность distributed proving. Документы эту разницу проводят правильно. Поставки MP-SPDZ/co-snarks не запускались; их suitability для конкретных n,t,f/rotation не подтверждена.

**R11–R14, private payment и residuals.** `nodfactory/runtime.rs:169–238,255–289` подтверждает owner/PoW/deadline, bucket price, equality paid cost и общий checkpoint. `nod/called.rs:241–297` действительно читает individual load и возвращает сумму ограниченного прохода; ранний B−G этого consumer не закрывает. Преобразование в residual по одинаковым weights/lifecycle и fresh linked debit алгебраически достаточно; group size может быть O(N). Закрытое вычитание обязано быть атомарным с Nod nullifier, payment, Gratis/Fidelity и durable residual state. Разрешённый F<q следует из current R01/R05/R10 для одного дня, не из одного объявления uint256. Payment subledger18 с сохранённой дробью способен представить exact c; существующий PayNote этого пока не делает. Кроме A-01/A-03 новых ошибок в этих принципах не найдено.

**Storage/recovery/scale.** Кто создаёт C(a), C(balance), C(0), group commitments и debit commitments — в основных документах различено правильно. Commitment/root не восстанавливает witness и не заменяет DA. Двойной checkpoint, WAL/backups и obsolete transport keys должны входить в erasure/capacity model. Размеры 32N/64N, 320 GB A и 384 GB B при t=6, 190-byte пример Nod, 36.5N hourly retained-record transfers, 5555.56 single-window и 11574.07 steady daily admissions/s арифметически корректны с указанными исключениями. `planner.rs:17,243–244` подтверждает именно 256 records/shard. Это модели, не measured throughput; current snapshot ещё собирает owners/tributes целиком (`metadosis/ocomp/snapshot.rs:35–49`), поэтому будущий scale path должен отдельно решить bounded snapshot scheduling с frozen root/time. Я не классифицирую отсутствие такого benchmark как теоретическую невозможность.

## 6. Что исполнялось и пределы доказательства

| Evidence class | Выполнено |
|---|---|
| Executed, exact arithmetic | `python3 testing/lysis-privacy-prototype/research_arithmetic_checks.py`: PASS, 137 117 Fidelity equivalence cases; совпали указанная 104/134/136-bit арифметика, ingest и размеры. |
| Executed, независимая малая проверка | 200 resharing cases над реальным q Baby для (t_old,t_new)=(2,2),(3,2),(2,4),(6,6), разные helpers/recipients: сохранены a/r; 1000 randomized exact monetary normalization cases: G≤B. Это scalar/algebra tests, не security proof. |
| Executed, integrity | 8 research input hashes совпали с frozen manifest. Parent source-hash recheck 24 файлов принят как supplied evidence; материальные code claims дополнительно прочитаны в текущих исходниках. |
| Read, primary research | Получены и прочитаны указанные разделы Pedersen, LegoSNARK, Groth resharing, CHURP, DPSS, Tiresias, malicious collaborative SNARK paper. Pedersen §4 просмотрен как отрендеренная PDF страница; нужные места остальных извлечены pdftotext. |
| Read, pinned upstream | Dock prover/generator/verifier/data structures и arkworks Baby curve/scalar source по pinned SHA. Web fetch некоторых PDF/raw страниц был blocked/cache-miss; публичные файлы успешно прочитаны через curl. Поздние fetch README co-snarks/MP-SPDZ завершились DNS error; из них не выведен новый current implementation verdict. |
| Source-only findings | A-01 — достижимая цепь потребителей и отсутствующий witness; A-02 — exact API/formula counterexample v=0; A-03 — отсутствующий timestamp producer/commitment contract. Ни один не назван исполненным production exploit. |
| Не исполнялось | Новый A/B P_link, полный P_L2 composition, setup ceremony, mobile MPC/VSS service, malicious network/crash tests, browser/mobile RAM, billion-record workload, end-to-end payment/forfeit. Больших builds/provers не было. |

Рекомендация следующему этапу: дополнить specification тремя найденными интерфейсами, затем продолжить уже предложенные полные маленькие A/B и lifecycle эксперименты. Сейчас обоснован вывод «направления математически жизнеспособны при условиях»; вывод «полная композиция доказана и замечаний нет» не обоснован.
