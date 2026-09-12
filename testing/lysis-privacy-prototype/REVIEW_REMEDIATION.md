# Замечания проверки и внесённые исправления

Дата: 2026-09-11. Основание — [сводка трёх независимых проверок](independent-review-2026-09-11/SYNTHESIS.md) и повторное чтение существенных consumers/pinned upstream source. Исправлены исследовательские документы; production-код и прежние benchmark results не менялись.

**Вывод:** Pedersen VSS позволяет получить численные S/S_l без раскрытия отдельных nominal, а deferred Nod позволяет отложить индивидуальный расчёт до пользовательского proof. Проверка не опровергла эти конструкции. Она обнаружила неполное описание их интеграции: соседние выплаты и writers, безопасный Lego adapter, точное время Fidelity, дополнительные mint sources и owner queries. Эти интерфейсы теперь включены в trace; остающиеся решения и неподтверждённые resource/security claims не объявлены закрытыми.

## 1. Что проверено и исправлено

| ID / важность | Проверенное замечание | Изменение документа | Остаточный статус |
|---|---|---|---|
| IR-01 / High | Intex после Lysis читает индивидуальный nominal и делает публичную пропорциональную выплату | Добавлен R15: certified denominator, exact floor/remainder, исполнители, приватное право, обеспечение, finalization и retention. Убрано исключение этого consumer из main scope | Trace исправлен. Выбор private payout asset/accounting, скрытого denominator protocol и публичности остатка ещё открыт |
| IR-02 / High | Credis settlement/void меняют тот же Gratis/Fidelity без wallet proof | Добавлен R16: inventory writers, liquid/incoming/pledged conservation, authority, forced MPC Out, witness recovery и atomic roots. Wallet-only hash account больше не представлен как полное решение | Trace исправлен. Нужны формат compartments, authenticated mutable state/proof, DA/recovery и решения для публичных Credis fields/расчётов |
| IR-03 / Medium | Lego API не выбирает внутренний v за caller; известный v раскрывает возможность проверки кандидатов a через D | Исправлен main §4.4: независимые `r_external` и `v_internal`, полный порядок вызова, CSPRNG ownership, хранение и проверки. Verifier contract сохранён | Пропуск рецепта закрыт на уровне документа; adapter и полный privacy/RAM run ещё не созданы |
| IR-04 / Medium | Wallet не знает фактический timestamp включения, используемый In/Out | Добавлен main §7.4: proof opaque delta → exact-time validity gate → atomic time attachment/log/money → verified fold → READY; nonce conflict, zero guards и recovery | Producer/consumer определены для предложенного формата. Wire/circuit/checkpoint/MPC реализация и стоимость открыты |
| IR-05 / scope/supply | Текущий Promis→Gratis — второй mint source; общий overflow не доказан достижимым | Добавлены R17/main §6.5: точный 6→18 conversion, supply conservation и отдельный Nod-only lifetime bound <2^208 | Неправильное обобщение снято. Upstream Promis/initial imports и полная no-TEE conversion boundary остаются задачами протокола |
| IR-06 / Low | Exact owner RCFI queries не заменяются публичной league | Добавлен R18/main §7.5: локальный exact evaluation по recoverable authenticated history, context/time, вариант private-output API | Требование и основной кандидат описаны; совместимость ABI/RPC требует явного решения |

«Trace исправлен» означает наличие конкретных входов, выходов, исполнителя и хранения. Это не утверждает выбранный криптографический протокол, production implementation или отсутствие всех замечаний.

## 2. Почему эти замечания приняты

### IR-01: commitment leaf не заменяет consumer payout

[IntexFactory pay_contributor_batch](../../crates/core/intexfactory/src/runtime.rs#L460) выполняет `w_i=floor(P*a_i/H)` и `transfer_balance(...,owner,w_i)`. Если P=H, w_i=a_i: публичный перевод может раскрыть скрываемое значение. Это арифметический контрпример, не выполненный exploit или утверждение о deployed pot.

[close_round_if_complete](../../crates/core/intexfactory/src/runtime.rs#L542) сжигает remainder только после обработки всех leaves. Fan-in deadline нельзя переименовать в expiry индивидуального claim. В R15 сохранены floor для каждого leaf, отсутствие «последнего, забирающего округление», обеспечение outstanding claims и отдельное решение для публичности remainder. Приватный COEN-backed payout не становится Gratis mint автоматически.

### IR-02: внешняя запись требует не только возможности прочитать league

[Credis settlement/void](../../crates/core/credisfactory/src/runtime.rs#L190) вызывают release/forced burn; [Gratis writers](../../crates/core/gratis/src/runtime.rs#L388) обновляют pledged/liquid state. Owner offline между Tribute и READY не освобождает runtime от forced Fidelity Out. Сохранить TEE или ждать нового proof владельца означало бы нарушить исходные требования.

При дополнительном чтении обнаружены смежные конкретные interfaces: [native stake](../../crates/core/credisfactory/src/runtime.rs#L93) равен collateral6×10^12 и переводится публично; Position/terms/events также содержат суммы. [Void возврат в PromisLimit](../../crates/core/credisfactory/src/runtime.rs#L292) сейчас raw6. R16 учитывает эти поля и conversion boundary: скрытого account commitment недостаточно для конфиденциальности всего consumer. Их изменение затрагивает payment/asset semantics и не считается уже согласованным.

### IR-03: проверен точный pinned API

В [Dock prover.rs](https://github.com/docknetwork/crypto/blob/224f195bb8babc2d0de5256135120e0aca9fbd19/legogroth16/src/prover.rs#L32) `v` и `link_v` приходят извне; r/s выбираются внутри. Публичный D использует v. Исправление относится к нашему рецепту использования этой версии, не к soundness всей upstream библиотеки. Новый полный proof с v=0 не запускался. Случайность нельзя подтвердить только фактом успешной проверки proof.

### IR-04: время принадлежит исполнению блока

[GratisFactory mint/mine_coen](../../crates/core/gratisfactory/src/runtime.rs#L129) берут actual block timestamp, [CohortState](../../bin/outbe-tee-enclave/src/fidelity.rs#L156) записывает acquisition/sale times. Новый контракт отделяет доказанный закрытый payload от публичного time attachment. Повторная проверка черновика дополнительно показала: `apply_cohort_section` выполняет checked evaluation до записи blob, поэтому обязательный T2a проверяет все time-dependent guards до monetary finality. При смене candidate timestamp/context evidence повторяется; зависимость от MPC/proof completion и её liveness явно остаётся открытой. Будущая история обязана сохранять исходное acquired_at при split, нулевые guards и точное cutoff ordering. Public event time не разрешает раскрывать выбранные LIFO slots.

### IR-05: bound должен относиться к достижимой истории

[WorldwideDay(u32)](../../crates/blockchain/primitives/src/time.rs#L138) и дневной bound дают <2^208 только при zero initial state, Nod-only, однократных бюджетах и отсутствии imports/re-mint. [PromisFactory.mine_gratis](../../crates/core/promisfactory/src/runtime.rs#L69) нарушает предпосылку Nod-only; поэтому теперь отдельно описан его вклад. Достижимый global overflow не заявляется, дополнительный MPC range check для уже ограниченного профиля не навязывается.

### IR-06: чтение состояния — отдельный интерфейс

[Fidelity query_index_at/now](../../crates/core/fidelity/src/runtime.rs#L155) возвращают точный owner-authorized RCFI. R18 различает текущую историю, вычисление на query timestamp и чтение исторического state root; не приравнивает вычисление 12 comparisons для league к exact RCFI query.

## 3. Что не является новым blocker

- Разрешение пользователя раскрывать S/S_l сохраняется; singleton-группы не возвращены в замечания и не требуют повторного согласования.
- SEAL остаётся исключённым; P-384 не стал обязательным.
- Предел кошелька — 512 000 000 B на полный cold P_link process. Старые 398/497 МБ — отдельные components, не подтверждение нового полного proof.
- VSS algebra, handoff counting bounds и weighted repair не превращены в доказательство собственной malicious/mobile композиции.
- «256» — Tribute records в shard Lysis, не блоки. Формулы объёма/скорости не переименованы в измеренные TPS.

## 4. Что осталось до реализации

1. Выбрать protocol contracts R15–R18: private Intex payout/backing/remainder, mutable compartments и recovery, Credis public value interfaces и reserve precision, Promis source boundary, Fidelity query API. Предложения теперь конкретны, но экономические правила не выбираются библиотекой.
2. Зафиксировать уже открытые параметры: security/setup/curve, source/cost/Fidelity bounds, wallet runtime, committee adversary/erasure/liveness, payment18, forfeit disclosure/timing, wire/VK/DA.
3. Создать одинаковые **полные** P_link A/B и проверить correctness, negative inputs и ≤512 МБ. Отдельно проверить Lego internal randomness/verifier adapters.
4. Выполнить lifecycle с offline owner, внешним collateral update до READY, восстановлением witness, timestamp с задержкой, Intex floor/remainder, Promis conversion, ротациями/repair/forfeit и exact RCFI read.
5. Только после этих contracts и correctness gates измерять admission/s, t256, Nod/s, actual bytes/DB/WAL/DA, handoff и перекрывающиеся дни.

Новых тяжёлых криптографических запусков эта правка не требует. Исправлены документы и проверены источники/арифметические следствия. [Точечные арифметические проверки](REVIEW_REMEDIATION_ARITHMETIC.json) фиксируют bound, floor и conversion cases. [Текущий evidence](DEEP_RESEARCH_EVIDENCE.json) фиксирует состав файлов и пределы проверки; [исходный независимый audit](independent-review-2026-09-11/REVIEW_EVIDENCE.json) остаётся историческим и неизменённым.
