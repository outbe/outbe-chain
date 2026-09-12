# Downstream private state: Intex, Gratis/Credis, Promis и Fidelity query

Дата: 2026-09-11. Дополнение к [master trace](PROTOCOL_TRACE_AND_REQUIREMENTS.md) и [исследованию реализации](DEEP_RESEARCH_IMPLEMENTATION.md). Основание — подтверждённые IR-01, IR-02, IR-05, IR-06 из [исторического synthesis](independent-review-2026-09-11/SYNTHESIS.md). Исторические отчёты и manifests этим документом не изменяются.

**Статус:** ниже сначала описан действующий код, затем отдельно предложены контракты без TEE. Трассировка исправляет пропуски producers/consumers и единиц измерения; она не является реализацией, доказательством безопасности полной композиции или подтверждением её производительности. Публичность COEN на согласованном выходе Gratis → COEN не означает автоматического разрешения публиковать промежуточные Intex rewards, collateral movements, Promis burns или новые агрегаты.

Проверка выполнена чтением точных исходников и поиском callers/writers в `crates/` и `bin/`. Graph MCP недоступен: index generation и graph coverage не заявляются. HEAD при проверке — `177a72ddbea9f2e52eef094405481292ecd56046`. Существующие сторонние изменения в рабочем дереве не редактировались. Область — перечисленные денежные переходы и необходимые им state consumers; это не аудит всего Credis/Promis/Gem/auction/oracle protocol.

Обозначения: `M=10^6`, `K=10^12`; суффикс `6` означает protocol raw units с шестью знаками, `18` — raw units с восемнадцатью. В действующем коде Promis, Gratis, pledge collateral и loads имеют **6** знаков; native COEN — **18**. Это явно задаёт [units.rs:28–49](../../crates/blockchain/primitives/src/units.rs#L28). Цель исследования меняет Gratis на 18, но сама по себе не меняет Promis, stablecoin principal, oracle prices и PromisLimit.

## 1. Действующие producers, consumers и хранилища

<a id="r15"></a>

### R15 / IR-01. Contributor → funded Intex payout

#### 1.1. Authority и приход денег

[Lysis `phases.rs:701–707`](../../crates/core/lysis/src/program_v1/phases.rs#L701) создаёт contributor `{owner, source_tribute_id, nominal_amount_minor}`, если `exclude_from_intex_issuance=false`. Это отдельный consumer nominal помимо Nod. [Activation:333–338,366–368](../../crates/core/metadosis/src/ocomp/activation.rs#L333) устанавливает certified root, contributor count и `eligible_nominal_total`. Обозначим последнее `H6=Σ eligible a_i6`; оно не обязано совпадать с разрешёнными к раскрытию `S6` или `S_l6`.

| Переход | Исполнитель / вход | Проверка и результат | Где остаются данные |
|---|---|---|---|
| Certified generation | Lysis/result producer; authenticated eligible contributors | Root и точные count/total входят в установленную authority | Root/count/H6 на chain; contributor bodies в authenticated result chunks: [schema.rs:334–338](../../crates/core/intex/src/schema.rs#L334) |
| Proceeds credit | OriginRouter; day, source chain, **native COEN18 `msg.value`** | Production caller gate, nonzero, credit pot; публичное событие amount: [`distribute:324–359`](../../crates/core/intexfactory/src/runtime.rs#L324) | Native balance IntexFactory; per-day pot, expected/arrived chain flags |
| Fan-in | Ожидаемые winning chains и deadline | Повторный chain credit суммирует деньги, но counts arrival один раз; ready, когда все expected chains прибыли: [`api.rs:588–645`](../../crates/core/intex/src/api.rs#L588) | Deadline и awaiting set; это state поступлений, не список claims |
| Open certified round | Runtime при ready или достижении deadline | Замораживает pot `P18`; contributor_count=0 → ownerless burn; pot=0 → finalize без round | `CertifiedPayoutRound {wwd,amount,paid_so_far,paid_leaf_count,active}`: [schema.rs:304–331](../../crates/core/intex/src/schema.rs#L304) |

`try_settle_proceeds` ждёт ready либо deadline. Если root ещё не установлен и legacy total нулевой, до deadline pot удерживается. После открытия certified round fan-in завершается безусловно. Новый pot после его открытия сгорает: **у certified day один round**, поздние поступления не создают top-up. См. [runtime.rs:366–452](../../crates/core/intexfactory/src/runtime.rs#L366).

#### 1.2. Exact payout, floor и остаток

Любой отправитель может принести chunk-aligned leaves и membership proof. [`pay_contributor_batch:460–535`](../../crates/core/intexfactory/src/runtime.rs#L460) проверяет неоплаченный bitmap, certified leaf range и затем для **каждого**, включая последний, считает:

```text
y_i18 = floor(P18 * a_i6 / H6)
paid_next18 = paid_prev18 + Σ batch y_i18 <= P18
```

Текущий `checked_mul(P18,a_i6)` отвергает overflow U256 до деления. При замене на широкий integer product меняется множество принимаемых крайних входов; это надо зафиксировать явно. Валидная certified authority должна обеспечивать `H6>0` для непустого payable набора: target proof не должен полагаться только на предполагаемую корректность denominator.

Затем выполняются публичные native transfers `INTEX_FACTORY_ADDRESS → leaf.owner`, выставляется bitmap, увеличиваются paid/count и эмитится публичный `paidAmount`. Все действия batch находятся в `with_checkpoint`. [`api.rs:355–425`](../../crates/core/intex/src/api.rs#L355) связывает anti-replay именно с day и leaf indices, а не с транзакцией отправителя.

Остаток `R18=P18−Σ all y_i18` сгорает **только когда оплачены все certified leaves**. До этого оставшиеся деньги включают невыплаченные права, которые нельзя назвать rounding dust. Round и bitmap сохраняют итоговые counters; в `CertifiedPayoutRound` нет claim-expiry. Fan-in deadline не разрешает конфисковать ещё не выплаченный leaf. См. [`close_round_if_complete:538–567`](../../crates/core/intexfactory/src/runtime.rs#L538), [schema.rs:304–331](../../crates/core/intex/src/schema.rs#L304).

Legacy path отличается: [`pay_chunk:657–726`](../../crates/core/intexfactory/src/runtime.rs#L657) платит floor промежуточным contributors, **последнему отдаёт весь остаток**, а incomplete fan-in может потребовать поздние top-up rounds. Нельзя переносить правило последнего leaf или retention legacy map на certified path. Cutover должен либо поддержать незавершённые legacy rounds, либо отдельно завершить/мигрировать их без изменения прав.

#### 1.3. Что раскрывается и почему дневных shares недостаточно

В действующем пути открыты `a_i6`, `H6`, payout `y_i18`, суммы batch и burn. Замена `a_i6` в leaf на commitment не скрывает публичный payout: при `a=(7,13)`, `H6=20`, `P18=20` выплаты `(7,13)` точно раскрывают nominal. Это арифметический пример, не исполненная transaction/exploit.

Даже при скрытых individual payouts публикация `R18` или `Σy_i18` раскрывает дополнительную функцию скрытых nominal. Разрешение раскрыть `S/S_l` не распространяется автоматически на неё. Для `P18=10` и трёх `a_i6=1` каждый получает 3, остаток равен 1; агрегатный `H6=3` сам по себе недостаточен для иных разбиений на individuals и их floors.

До фиксации pot нужен доступ к authenticated `a_i6` и denominator; после pot можно закрыто материализовать все `y_i18`. Публичный root не восстанавливает ни nominal, ни opening. Если владелец offline, permissionless execution в новой схеме требует shares/другого доступного private witness. Удаление per-Tribute shares после Lysis или Nod-forfeit допустимо лишь после отдельного преобразования Intex obligations в recoverable rights либо завершения всех потребителей.

<a id="r16"></a>

### R16 / IR-02. Все writers общего Gratis/Fidelity state

#### 1.4. Какие compartments действительно существуют

[Gratis schema:17–38](../../crates/core/gratis/src/schema.rs#L17) содержит liquid `balance_ct`, active `pledged_ct`, отдельные pending `pledge_lock_tickets`, owner modify `op_nonce`, публичные `total_supply` и `pledged_total_supply`. **Pending pledge уже списан с liquid, но ещё не входит в active pledged.** `pledged_total_supply` считает pending **и** active. Один и тот же залог нельзя прибавлять дважды.

Обозначим liquid `L6`, сумму живых tickets `T6`, active pledged `A6`. Денежный инвариант корректной истории:

```text
account_economic_Gratis6 = L6 + T6 + A6
global_total6 = Σ owners (L6 + T6 + A6)
global_pledged_total6 = Σ owners (T6 + A6)
```

Это выведенный из переходов invariant, а не отдельная исполняемая проверка всех accounts в текущем коде. AEAD blobs создаёт enclave; runtime сохраняет bytes и supply deltas. [Gratis ABI:18–52](../../crates/core/gratis/src/precompile.rs#L18) предоставляет ciphertext reads и публичные supplies; transfer/approve запрещены, денежные writers проходят cross-module API.

#### 1.5. Полная таблица write-операций в ограниченном scope

Здесь перечислены варианты, имеющиеся в Gratis/Fidelity API, и их найденные production consumers. Low-level варианты без Fidelity не приравниваются к публичному пользовательскому mint.

| Writer / authority | Изменение compartments и supply | Fidelity | Точный источник |
|---|---|---|---|
| Mint; owner modify auth + factory source authority | `L+=x; total+=x`; owner nonce advances | Factory mint добавляет In(x,execution time) | [`mint_impl:120–150`](../../crates/core/gratis/src/runtime.rs#L120); [`GratisFactory mint:129–142`](../../crates/core/gratisfactory/src/runtime.rs#L129). Production sources: [`NodFactory:236`](../../crates/core/nodfactory/src/runtime.rs#L236), [`PromisFactory:80`](../../crates/core/promisfactory/src/runtime.rs#L80) |
| Liquid burn; owner modify auth | `L-=x; total-=x`; owner nonce advances | `mine_coen` добавляет Out; публично mint COEN18 | [`burn_impl:177–207`](../../crates/core/gratis/src/runtime.rs#L177); [`mine_coen:145–173`](../../crates/core/gratisfactory/src/runtime.rs#L145) |
| Pledge; owner modify auth binds **stables amount**, transaction binds cap/asset | `L-=g; T+=g; pledged_total+=g`; новый ticket, nonce advances | Probe league, **не Out** | [`pledge_impl:238–276`](../../crates/core/gratis/src/runtime.rs#L238); [factory:63–110](../../crates/core/gratisfactory/src/runtime.rs#L63) |
| Unpledge ещё pending ticket; owner + ticket equality | `T-=g; L+=g; pledged_total-=g`; ticket удалён, nonce advances | Нет In/Out | [`unpledge:309–347`](../../crates/core/gratis/src/runtime.rs#L309); [enclave:462–482](../../bin/outbe-tee-enclave/src/gratis.rs#L462) |
| Consume ticket; CCA request + spend_auth, bound to smartAccount | `T-=g; A+=g`; ticket удалён; **оба supplies неизменны** | Нет In/Out | [CredisFactory:40–89](../../crates/core/credisfactory/src/runtime.rs#L40); [`consume_pledge:388–417`](../../crates/core/gratis/src/runtime.rs#L388); [enclave:489–536](../../bin/outbe-tee-enclave/src/gratis.rs#L489) |
| Release active collateral; **любой payer**, платящий собственные stablecoins за position | `A-=x; L+=x; pledged_total-=x; total` прежний | Нет In: collateral продолжал стареть | [CredisFactory `settle:176–252`](../../crates/core/credisfactory/src/runtime.rs#L176); [`release_to_eoa:423–457`](../../crates/core/gratis/src/runtime.rs#L423) |
| Forced burn; called position, notice deadline, protocol scheduler | `A-=x; total-=x; pledged_total-=x` | Out(x,execution time), затем PromisLimit credit | [CredisFactory `void_position:259–294`](../../crates/core/credisfactory/src/runtime.rs#L259); [`burn_pledged_impl:464–513`](../../crates/core/gratis/src/runtime.rs#L464); [daily dispatcher:151–172](../../crates/core/credisfactory/src/called.rs#L151) |
| Standalone Fidelity In/Out API | Денежный Gratis state сам не меняет | Cohort op + persisted outcome | [`Fidelity runtime:63–95`](../../crates/core/fidelity/src/runtime.rs#L63); [`api.rs:17–34`](../../crates/core/fidelity/src/api.rs#L17). В targeted production caller search отдельные вызовы In/Out не найдены; тестовые вызовы есть |
| `apply_fidelity_outcome` | Не меняет liquid/pledged | Записывает cohorts blob; set-once global anchor | [`apply_outcome:49–60`](../../crates/core/fidelity/src/runtime.rs#L49); production callers — два GratisFactory пути и Credis void из таблицы |

`reveal_owner` — read, а не writer: из pending ticket или `eoa_ct` восстанавливает адрес для последующих операций. Ему также нужна замена без TEE. Active Credis position хранит sealed EOA; spend auth на ticket запрещает перенаправить loan другому smartAccount. [enclave `apply_consume_pledge:505–535`](../../bin/outbe-tee-enclave/src/gratis.rs#L505).

Для consume/release/forced burn **нет нового owner modify authorization и обновления owner op_nonce**. Enclave проверяет ticket binding либо sufficiency активного pledged; authority суммы release/void приходит от Credis position schedule. См. [Gratis runtime:388–497](../../crates/core/gratis/src/runtime.rs#L388), [enclave:539–604](../../bin/outbe-tee-enclave/src/gratis.rs#L539). Поэтому существующий `op_nonce` нельзя использовать как единственную версию будущего account commitment: внешнее изменение может сделать wallet witness устаревшим, не изменив этот nonce.

Клиент, знающий только opening `H(balance limbs,salt)`, не может обеспечить автоматический void после своего ухода offline. Executor, знающий только release amount, также не может изменить этот hash без opening старого liquid. Shares для чтения league не определяют shares/authority для записи нового balance, position и Fidelity state.

#### 1.6. Pricing, partial repayment, deadline и дальнейший consumer burn

Существующий pledge price — `g6=ceil(stables6*M/rate6)`, где rate берётся из fresh COEN oracle, а terms фиксируются в ticket. Проверяются nonzero asset/stables и `g6<=max_gratis`. Probe читает league, но реальный eligibility check оставлен TODO; текущая проверка лишь отвергает `u16::MAX`. Это не готовое правило кредитной eligibility. [GratisFactory:45–110](../../crates/core/gratisfactory/src/runtime.rs#L45).

В `request_credis` CCA registry пока stub, считающий адреса active; deployment smartAccount проверяется. Production код требует **точное** `native stake18=g6*K`, после чего передаёт весь native stake smartAccount; залог Gratis остаётся pledged владельца. Public principal выдаётся stablecoin-переводом через vault. См. [CredisFactory:54–70,93–106,126–151](../../crates/core/credisfactory/src/runtime.rs#L54). Это два разных актива/потока, не custody одного залога.

[`Credis::settle:211–308`](../../crates/core/credis/src/runtime.rs#L211) принимает payment сначала на accrued interest, затем principal. Сумма меньше interest отвергается; сверх необходимого не изымается. Для partial principal payment `p6` release равен `floor(original_G6*p6/original_P6)`. На **финальном** погашении возвращается весь `collateral_locked`, включая остатки предыдущих floors. Поэтому сумма всех releases ровно original collateral; нельзя заменить final release новым floor. Interest использует целые UTC days, anchor переносится только на начисленные дни, а не на произвольное время платежа.

Void допустим только для Called position с ненулевым outstanding при `now >= called_at + sealed notice period`. Он сжигает **весь текущий collateral_locked**, а не пересчитанный `floor(G*outstanding/P)`. Обе величины могут различаться из-за накопленных floors. См. [`settlement_deadline:92–100`](../../crates/core/credis/src/runtime.rs#L92), [`void_position:311–365`](../../crates/core/credis/src/runtime.rs#L311). Scheduled scan может сделать это без владельца; ограничение текущего daily run — 64 voids: [called.rs:21–39](../../crates/core/credisfactory/src/called.rs#L21).

После burn и Fidelity Out выполняется ещё один consumer: `PromisLimit.add_to_total_unallocated(void.gratis_burned)` **raw 1:1 в прежних protocol units6**. Это capacity/reserve accounting, не mint Promis владельцу. [CredisFactory:292–294](../../crates/core/credisfactory/src/runtime.rs#L292), [PromisLimit runtime:14–15](../../crates/core/promislimit/src/runtime.rs#L14).

#### 1.7. Amount visibility у соседних интерфейсов

| Текущий публичный канал | Следствие для предлагаемого скрытого state |
|---|---|
| `GratisMinted/Burned`, pledge/unpledge events, `totalSupply` и `pledgedTotalSupply` | Ciphertext balance не скрывает exact operation amount и supply delta. Owner=zero в release/void event не скрывает amount: [runtime:449–455,489–495](../../crates/core/gratis/src/runtime.rs#L449) |
| Pledge calldata `amountStables`, `asset`, `maxGratis`; публичный oracle | При сохранении terms известный quote определяет `g6`; удалить только `gratisAmount` из event недостаточно: [precompile:37–66,83–99](../../crates/core/gratisfactory/src/precompile.rs#L37) |
| Публичный CCA `msg.value` и native transfer | Даже без quote раскрывает `g6=stake18/K`: [CredisFactory:93–106,143–148](../../crates/core/credisfactory/src/runtime.rs#L93) |
| Credis `Position.collateral`, `collateral_locked`; settlement/void logs | Открыты размер залога и release/burn: [schema:78–93](../../crates/core/credis/src/schema.rs#L78), [runtime:284–290,350–356](../../crates/core/credis/src/runtime.rs#L284) |
| Прямое приращение public PromisLimit после void | Если прежний и новый total наблюдаемы, разность раскрывает burned collateral даже после удаления events |

Эта таблица **не вводит запрет на все публичные кредитные terms**. В исходных требованиях owner/IDs/public terms допустимы. Нужно явно определить, какие collateral amounts и выводимые из допустимых terms величины считаются разрешённым раскрытием. Известный залог даёт информацию об изменении compartments/нижней границе средств, но сам по себе не доказывает восстановление всего Gratis balance. При требовании скрыть и эти amounts прежние public stake, stable transfers, positions и reserve deltas несовместимы с таким требованием; они входят в необходимую границу изменений.

<a id="r17"></a>

### R17 / IR-05. Promis6 → Gratis6 сегодня; Gratis18 в целевой схеме

[`PromisFactory::mine_gratis:63–82`](../../crates/core/promisfactory/src/runtime.rs#L63) вызывает `promis::burn(account,amount,promis_auth)`, затем `GratisFactory::mint(account,amount,gratis_auth)`. Оба amount сейчас fixed6, поэтому raw equality означает экономическое 1:1. Два MAC/opNonce относятся к двум independently keyed ledgers. Mint Gratis добавляет новый Fidelity In по `storage.timestamp()`.

Это доступный публичный маршрут: [`precompile.rs:42–58`](../../crates/core/promisfactory/src/precompile.rs#L42) принимает sender, amount и оба auth. Promis source нельзя объявить отсутствующим на основании того, что prototype начинает с Tribute/Nod.

| Actor / вход | Текущее действие | Выход / storage / disclosure |
|---|---|---|
| Владелец Promis и Gratis; два authorizations | Burn Promis6, mint столько же Gratis6 в одном внешнем precompile call | Два encrypted balances, два op_nonces, два public supply updates, Fidelity acquisition |
| Promis runtime | Проверяет nonce, вызывает enclave, пишет new balance и checked supply decrease | [`PromisBurned(account,amount,remainingSupply):105–133`](../../crates/core/promis/src/runtime.rs#L105) раскрывает amount даже при скрытом целевом mint |
| GratisFactory/runtime | Проверяет свой auth/nonce и mint authority через caller path | `GratisMinted` и total раскрывают тот же amount; [mint:129–142](../../crates/core/gratisfactory/src/runtime.rs#L129) |
| Соседний `mine_coen` | Burn Promis6, explicit `*K`, публичный native COEN18 | [runtime:38–60](../../crates/core/promisfactory/src/runtime.rs#L38); это отдельный путь, а не уже существующая conversion внутри `mine_gratis` |

Непосредственные production sources Promis mint найдены в двух местах: [GemFactory `mine_promis:514–547`](../../crates/core/gemfactory/src/runtime.rs#L514) проверяет owner/Settled/PoW, burns Gem и mints `promis_load_minor`; [IntexFactory `mine_promis:983–1043`](../../crates/core/intexfactory/src/runtime.rs#L983) проверяет Settled balance/PoW, burns Settled NFT и mints `series.promis_load_minor * amount`. Они используют enclave Promis mint и публикуют minted amount. Это необходимый источник authority для оценки Promis supply, но здесь **не проверены все upstream правила возникновения Gem/Settled и genesis/import**. Наличие этих callers не является доказательством достижимого overflow.

#### 1.8. Какой lifetime bound действительно доказуем

Для ограниченного профиля `zero initial state + current-source Nod-only + один бюджет на каждый u32 day + no replay/import/alternative mint`:

```text
a_max6 <= (2^64*M - 1)*M < 2^104
N_day <= 2^32-1  => S_day6 < 2^136
S_day18 = K*S_day6 < 2^176
minted_from_day18 <= budget_day18 <= floor(0.32*S_day18) < 2^176
number_of_distinct_WorldwideDay <= 2^32
cumulative_minted18 < 2^208 < 2^256
live_supply18 <= cumulative_minted18
```

Source-format и nominal bound приведены с exact source в [master R01](PROTOCOL_TRACE_AND_REQUIREMENTS.md#r01-исходная-сумма--nominal--commitment); day type — [`WorldwideDay(u32):144`](../../crates/blockchain/primitives/src/time.rs#L144). Budget/claim conservation остаются условиями R05/R10, а не выводом из типа U256. Ограничение count до 10^9 для этой гарантии не требуется.

**Для этого профиля не нужен отдельный global MPC range check только ради overflow.** Нельзя опровергать его недостижимым без источника примером `T_old=U256_MAX`. Однако текущий Promis→Gratis mint исключён предпосылками этого bound. Для полной поддерживаемой системы требуется source ledger, включающий genesis/migration/import, Nod, Promis conversion и все будущие разрешённые sources. Recycling через burns/reserves нельзя считать fresh unconstrained mint и нельзя объявлять безопасным без его conservation proof. Upstream lifetime proof Promis здесь не установлен.

<a id="r18"></a>

### R18 / IR-06. Exact owner Fidelity query

Текущий [IFidelity.sol:5–19](../../contracts/precompiles/src/IFidelity.sol#L5) имеет `getFidelityIndex(account,expiry,signature)` и `getFidelityIndexAt(account,timestamp,expiry,signature)`. [Dispatch:32–42](../../crates/core/fidelity/src/precompile.rs#L32) возвращает именно `rcfi:uint256`; enclave внутренне возвращает также efficiency/league. Публичная league snapshot или результат 12 comparisons не заменяет числовой ответ этих методов.

[`query_index_at:155–172`](../../crates/core/fidelity/src/runtime.rs#L155) берёт **текущий cohort blob выбранного execution state**, public first-qualified anchor, query timestamp, chain ID, expiry, signature и current block timestamp; вызывает enclave. `query_index_now` использует current block timestamp как query timestamp. Owner signature — EIP-191 над domain/chain/account/expiry; она не привязана к одному query timestamp или state root.

Enclave проверяет resident chain ID, expiry относительно переданного host block timestamp и recovered signer==account, затем decrypt/evaluate: [query_index:317–370](../../bin/outbe-tee-enclave/src/fidelity.rs#L317). Возвращаемое значение plaintext проходит через host/RPC. Это owner-authorized чтение, но не end-to-end ciphertext только владельцу. Источник сам указывает ограничение expiry: скомпрометированный host без trusted clock может повторить старую настоящую подпись, подставив timestamp=0. Здесь это описано как существующая граница, не новая отдельно исполненная атака.

История содержит active cohorts и sold slices с исходным acquired_at и sold_at; Out расходует LIFO, In=0 не устанавливает qualified_start. [CohortState:154–222](../../bin/outbe-tee-enclave/src/fidelity.rs#L154). Формула использует checked integer arithmetic, saturating time differences и nested floors: [fidelity-math:48–88](../../crates/core/fidelity-math/src/lib.rs#L48).

`getFidelityIndexAt(t)` **не выбирает исторический state root автоматически**. Он применяет `t` к текущему ledger выбранного вызовом состояния. Это отличается от восстановления состояния до позднейших mint/burn; при запросе исторического block state нужны и соответствующий blob, и соответствующий global anchor. Например, sold slice остаётся в текущем sold list при evaluation на время до продажи. Поэтому target wallet/API должен явно различать `(state_version, evaluation_time)` и не обещать более сильную историческую семантику под прежним именем без решения.

В текущем Fidelity schema/ABI не найден публичный метод выдачи владельцу открытой полной истории: [schema:6–28](../../crates/core/fidelity/src/schema.rs#L6), [precompile:31–48](../../crates/core/fidelity/src/precompile.rs#L31). Наличие query signature либо ключа аккаунта не восстанавливает private witness будущего commitment-only state. Для exact local query после стороннего settlement/void требуется новая recoverable история этих переходов.

## 2. Предлагаемые контракты без TEE — не утверждённая реализация

### 2.1. Общая граница proof, state и доступности данных

Для всех четырёх маршрутов transaction/proof должен связывать `chain, protocol/version, action, authoritative source ID, owner/destination, asset/units, old roots+versions, new commitments, unique transition ID`. Внешний executor не получает права mint/burn лишь потому, что умеет построить доказательство арифметики: source/position/round authority проверяется в том же statement и runtime transition.

Wallet создаёт commitments и private payload для инициируемой им операции. Для owner-offline перехода их создаёт выбранный authorized distributed executor над authenticated shares. Runtime проверяет proof либо **отдельно принятый** threshold certificate, freshness old roots, anti-replay и availability, затем атомарно устанавливает новые roots. Выбор malicious-secure MPC/coSNARK, threshold `n/t/f`, public verification model и recovery implementation остаётся открытым; корректный final SNARK сам по себе не доказывает privacy его distributed generation.

Время операций задаётся контрактом [основного исследования §7.4](DEEP_RESEARCH_IMPLEMENTATION.md#74-кто-фиксирует-время-проверенный-переход-и-timestamped-log): prover связывает amount/order/payload `C_delta` и old roots; runtime добавляет **фактический** `t_exec` в canonical opaque event leaf. Fold воспроизводит In/Out/LIFO и qualified_start по этим временам. Owner не угадывает время включения, runtime не подменяет timestamp внутри готового proof.

Перед final admission/финализацией соответствующего перехода нужны не только commitments, но и данные:

| Данные | Кто создаёт | Кто хранит / использует | Что проверяет возвращающийся owner |
|---|---|---|---|
| Opening нового account/compartment commitment | Wallet либо MPC | Wallet backup и/или shares у committee по выбранной custody модели | Canonical limbs/salt/version действительно открывают chain commitment |
| Amount/order payload `C_delta` и recovery package | Тот же prover; MPC не должен раскрывать witness одному участнику ради шифрования | Durable data service/committee; chain хранит authenticated descriptor/root | Recovery plaintext совпадает с `C_delta`, action/source/owner; нет пропусков sequence |
| Public log и exact execution times | Runtime | Chain/DA; wallet и MPC для replay/fold | Canonical root/finality/cutoff, порядок и timestamp каждой операции |
| Snapshots/checkpoints полной Fidelity history | Wallet/MPC после известных timestamps | Padded authenticated storage, shares для future READY/forced Out; recovery для owner | Checkpoint доказан как fold предыдущего log; активные и sold cohorts не потеряны |

Receipt доступности не доказывает правильность plaintext. Нужна связь recovery ciphertext/shares с доказанным amount/opening: verifiable encryption либо доказанный/проверяемый recovery протокол, и durable availability. Простое AEAD-шифрование attacker-controlled unrelated backup не закрывает требование. HPKE — возможный transport component; стандарт не даёт replay protection вне своего контекста, скрытия длины или forward secrecy против поздней компрометации recipient key. Поэтому domain/version/replay binding, padding и recovery-key lifetime — отдельные обязанности приложения. [RFC 9180 §9.7.3–9.7.6](https://www.rfc-editor.org/rfc/rfc9180.html#section-9.7.3).

### 2.2. Кандидат R15: private COEN-backed payout rights

Сохраняем денежную формулу и asset: Intex proceeds обеспечивают **COEN18 payout**, а не новый Gratis. Ни materialization reward, ни его wallet recovery сами по себе не создают Fidelity In. Для private destination потребуется отдельный COEN-backed account/note/right layer; назвать его Gratis и вызвать GratisFactory mint означало бы заменить актив и экономику.

1. **Funding:** runtime фиксирует certified eligible root/count, commitment `C_H` к H6, pot P18 и single-round ID. Родительский proof связывает `C_H` с точной суммой того же eligible набора. Публичность H6, если её хотят сохранить, требует отдельного разрешения: это не обязательно S/S_l.
2. **Materialization:** MPC с retained shares либо owner с openings доказывает membership/eligibility и integer relation `P18*a_i6=y_i18*H6+r_i`, `H6>0`, `0<=r_i<H6`, ranges и отсутствие wrap. Если H6 скрыт, знание только своего a_i6 недостаточно для wallet proof: требуется разрешённая выдача denominator witness либо MPC-generated payout right с proof. Denominator plaintext нельзя разослать всем владельцам под видом нулевого раскрытия.
3. **Atomic accounting:** один leaf порождает ровно одно funded right. Bitmap/nullifier связывается с day/root/leaf index; одновременно debit unmaterialized round liability, credit private COEN right и update hidden paid accumulator. Нельзя считать materialization и последующий withdrawal двумя списаниями pot. Нулевое entitlement отмечается завершённым без расхода денег; это не должно оставлять round навечно открытым.
4. **Owner offline:** разрешённый executor материализует rights без нового wallet proof; поэтому shares a/H нужны до этого шага, а новые amount/opening и inclusion evidence — в recoverable storage. Альтернатива owner-only materialization откладывает round completion до возвращения всех owners и не сохраняет прежнюю permissionless progress semantics без явного решения.
5. **Remainder:** после обработки всех leaves проверяется `P18=Σrights18+R18`. Exact floor dust и поздние proceeds имеют разные причины burn. Невостребованный уже созданный right — обязательство владельцу, не dust. Срок погашения такого right отсутствует в нынешнем certified payout; добавить expiry/forfeit — новое правило. Для legacy rounds нужен отдельный adapter с last-leaf remainder/top-up semantics.

**Открытая output/economic граница:** текущий native burn R18 публичен, а его величина — новая функция скрытых inputs. Сохранить его exact public representation можно лишь с явным разрешением такого агрегата. Оставить R18 навсегда на публичном reserve account и пометить внутреннюю liability погашенной не тождественно native `decrease_balance`: это иной supply/burn contract. Private payout/pool implementation должна определить backing, burn, permitted withdrawals и наблюдаемые aggregate deltas. Unmixed public withdrawal y_i18 на прежний owner снова может раскрыть nominal; отдельный private right ещё не доказывает приватность cash-out.

### 2.3. Кандидат R16: единый account с проверяемыми compartments

Минимально полное состояние содержит bounded uint256 limbs для liquid `L18`, суммы pending pledge tickets `T18`, active pledged `A18`, commitments каждой live ticket/position, owner authorization nonce **и отдельную state version всех writers**, Fidelity log/root и recovery registration. Если вводится pending inbound credit `I18`, его надо отличать от pending pledge ticket T18; это дополнительный compartment, а не другое название того же залога.

```text
E18 = L18 + T18 + A18 [+ I18]
total18 = Σ E18
pledged_total18 = Σ (T18 + A18)
```

Все суммы — integers с ranges/carries, а не равенства только modulo scalar field. Ticket consume доказывает удаление из T и ровно одно добавление в A; per-position collateral и account A связаны общей authority. Release и forced burn не могут выбрать чужой owner/root или уже закрытую position. Position spend/nullifier и account version обновляются атомарно, включая stable payment result для settlement и PromisLimit delta для void.

| Переход | Кто доказывает / что обязан иметь | Денежный / Fidelity результат без смены прежних правил |
|---|---|---|
| Pledge/unpledge | Owner: current account witness, ticket data, oracle/terms authority | `L↔T`; ни In, ни Out; ticket owner/asset/stables/cap и exact quote связаны с proof |
| Consume ticket | CCA с заранее выданным owner spend authorization + executor, имеющий private ticket/A witness | `T→A`; loan terms frozen, owner не переавторизует origination; same ticket cannot cancel and consume |
| Third-party settlement | Payer предоставляет payment; authorized executor имеет position/A и, для прямого credit, L witness | `A→L`; release не создаёт новую acquisition. Сумма и final-remainder определяются position schedule |
| Scheduled void | Executor имеет position/A и **полную актуальную Fidelity history/shares** | `A-=x`, supply-=x, Out(x,t_exec), reserve credit по согласованным units; owner offline не задерживает переход |
| Mint/burn | Owner/source adapter либо заранее разрешённый executor | Один связанный source debit/credit; In/Out ровно один раз и по execution time |

Хранение достаточных account openings/shares для прямого external `A→L` — конкретная обязанность выбранного executor. Если вместо этого external release создаёт `I18` и wallet позже делает proof merge `I→L`, fidelity остаётся прежней: release и merge не являются In. Но текущий код сразу делает liquid spendable; необходимость wallet merge меняет интерфейс/доступность средств. Такой вариант нужно принять явно, а не считать незаметной деталью хранения. Его outstanding credits входят в total и recovery до merge.

Fidelity учитывает economic holdings, включая locked collateral: active cohort sum в корректной истории следует за mint/burn, а не за перемещением `L/T/A/I`. Forced Out обязан исполнять прежний LIFO по полной истории, даже если burned collateral давно связан с конкретной position. Вычитание «из cohort этой position» подменило бы текущую семантику. Defensive clamp существующего engine не разрешает monetary overspend; для новых корректных состояний нужны monetary/cohort conservation, а несогласованные imports требуют отдельной migration policy.

Есть обязательный guard до final money: текущий [`apply_cohort_section:234–277`](../../bin/outbe-tee-enclave/src/fidelity.rs#L234) после In/Out/Probe выполняет checked `state.evaluate(section.timestamp,effective_first)` **до** создания успешного outcome. Его overflow/error отвергает combined operation. Поэтому opaque timestamped log нельзя сначала финализировать вместе с деньгами, а проверку допустимости Fidelity отложить до READY. По T2a/T2b основного исследования закрытый executor сначала готовит evidence успешного перехода/evaluation для точных time/root/global context; runtime устанавливает log и денежные roots только при совпадении с фактическим execution context и успехе всех guards. Retry и обеспечение такой evidence в нужном блоке остаются открытым liveness/runtime вопросом; owner повторно не вызывается.

Публичный owner допускается исходной задачей, а анонимность не является её требованием. Поэтому основной предлагаемый adapter заменяет `eoa_ct` на authenticated public owner, связанный с ticket/position; повторное согласование публичности owner для этой схемы не требуется. Это фиксирует отличие от legacy скрытия EOA↔smartAccount. Если впоследствии понадобится сохранить и эту связь скрытой, потребуется отдельный private lookup/binding proof или threshold recovery. Удалить `reveal_owner` и потерять destination authority нельзя.

#### 2.3.1. Обязательная таблица перехода 6 → 18 для collateral

| Величина | Вариант точного сохранения прежнего округления | Вариант более мелкой точности, требующий решения |
|---|---|---|
| Новый pledge | `g18 = ceil(stables6*M/rate6)*K` | `g18 = ceil(stables6*10^18/rate6)` |
| Partial release | `release18 = floor(G6*principal_paid6/P6)*K` | `release18 = floor(G18*principal_paid6/P6)` |
| Final release / void | Полный `remaining_locked18`; сохранённые amounts кратны K | Также весь remaining; может быть некратен K |
| CCA native stake | `stake18=g18`; **никакого второго `*K`** | То же equality, но другая pledge rounding может изменить stake |
| PromisLimit6 credit после void | `burn18 % K == 0`, затем `reserve_delta6=burn18/K` | Нужны PromisLimit18 или отдельный residual ledger/rounding policy; молчаливый floor теряет capacity |
| Migrated ticket/position | Versioned old amount6 → amount18 ровно один раз; stables/prices остаются6 | Для already frozen terms нельзя заново repricing/rounding |

Разница реальна: для `stables6=1, rate6=3*M` первый pledge вариант даёт `10^12` Gratis18, второй — `333333333334`. Для collateral `G6=10`, principal P6=3 и partial payment1 прежний release даёт `3*K`, direct floor18 — `3333333333333`, не кратный K. Эти варианты сохраняют разные точности; ни один здесь не объявляется новым утверждённым economic rule. Ranges должны отдельно покрыть multiplication/conversion и все intermediate carries; нельзя заменить existing fixed6 checked operations raw fixed18 арифметикой без пересмотра overflow boundaries.

Если public loan terms/collateral outputs остаются разрешёнными, их можно проверять как public inputs, сохранив скрытыми остальную историю и баланс. Если required amount privacy распространяется и на них, потребуются закрытые stake/payment/position/reserve consumers из §1.7. Один account commitment эту границу не закрывает.

### 2.4. Кандидат R17: source-complete Promis conversion

При сохранении Promis6 target transition должен доказать:

```text
authorized Promis debit = z6
Gratis credit18 = z6*K
0 < z6 <= floor((2^256-1)/K)
Promis balance_old6 >= z6
Gratis balance_next18 <= 2^256-1
same owner/source ID/transition, no replay
```

Owner строит linkage proof двух ledger transitions либо предоставляет связанные shares разрешённому executor. Новые source/target commitments и correct recovery payloads создаются одним producer; runtime атомарно меняет оба balances/versions, соответствующие supply commitments, source spent accounting и Fidelity In. Ошибка target overflow/proof/payment/DA не должна оставлять source burn состоявшимся. Простое raw equality после увеличения Gratis precision уменьшит экономический credit в K раз; повторное scaling увеличит его в K раз.

Исходный Promis MAC/TEE не является proof adapter без TEE. Нужно либо заменить Promis balance transition на authenticated private state, либо явно ограничить поддерживаемый профиль и определить миграцию/судьбу уже существующих Promis rights. Если no-TEE заявляется также для происхождения Promis, enclave mint из Gem/Intex должен получить отдельный source adapter. Этот документ определяет границу, но не заменяет полный upstream issuance audit.

Публичный amount в `mineGratis`, `PromisBurned`, обоих supplies и source mint events раскрывает credit даже при скрытом target. Поэтому amounts, event/return shapes и query APIs source и target входят в adapter. Сохранение публичного Promis amount возможно только как явное разрешение вывести из него Gratis credit, а не как implementation сохранённой amount privacy.

Для общего supply нужен либо source-complete lifetime bound с initial state и всеми mint budgets, либо закрытое bounded supply state с proof updates. Nontransferability сама по себе не ограничивает aggregate mint. Public exact total after каждой private mint не служит безопасной заменой скрытого conservation. Выбор между доказанной source-bound оптимизацией и общим committed supply — после фиксации поддерживаемых sources; доказательства reachable overflow здесь нет.

### 2.5. Кандидат R18: exact local query и альтернативный private-output API

**Основной предлагаемый интерфейс:** wallet получает canonical state/log version, recoverable полный private history witness и public global anchor соответствующего state; проверяет commitments/fold, затем локально вычисляет exact RCFI при выбранном evaluation time. По source formulas совместное scaling всех cohort sizes на K сокращается в ratio и сохраняет результат, пока intermediate arithmetic определена; это не оправдывает U256 overflow. Новая история с units18 и её bounds входят в target profile.

Локальное чтение не требует отправлять RCFI валидаторам или получать публичное доказательство собственного результата. Но authenticity roots и completeness recovery обязательны: пропущенный offline void меняет результат, даже если остальная локальная история корректна. Snapshot, достаточный только для одной league в READY, может не содержать данных для произвольного дальнейшего exact query.

Сохранение сетевого сервиса требует другого контракта: owner авторизует chain/account/state root/evaluation time/request ID и recipient encryption key; MPC читает authenticated latest/canonical history и возвращает **ciphertext результата только этому recipient**, привязанный к тому же request/root. Верификация результата требует доказательства либо отдельно принятой доверительной модели. Это сильнее текущего plaintext-to-host ответа и уже изменение wire/API; нельзя сохранить прежний Solidity `returns(uint256)` на публично исполняемом пути и одновременно считать ответ скрытым от исполнителя. Signature срок должен проверяться по принятому canonical freshness context, а не произвольному host timestamp. Разрешить прежнюю широкую подпись на несколько времен или сузить её до одного query — отдельный API choice.

Во всех вариантах требуется определить `(state_version,evaluation_time)` и retention исторических checkpoints. Owner запрашивает exact RCFI по актуальной восстановленной истории; настоящий query «состояние на прошлом блоке» требует retained historical snapshot/log prefix и тогдашнего global context. Отказ от старых методов либо их перенос в wallet/RPC должен быть документирован, не скрыт за готовой 12-comparison league circuit.

## 3. Retention, rotation и решения перед реализацией

| Обязательство | Когда данные ещё нужны | Условие безопасного удаления / преобразования |
|---|---|---|
| Intex a_i/H, membership bodies | До materialization funded rights; legacy может требовать top-up | Все будущие rounds/rights определены и backed, новые openings recoverable; отсутствие active Nod недостаточно |
| Intex private rights и paid evidence | Пока есть неприменённые/непогашенные обязательства; текущий certified claim expiry отсутствует | Выплачены/погашены по принятому правилу; permanent anti-replay/final accounting сохранён |
| Pending pledge ticket | До cancel либо consume | Удаление и replacement account/position state атомарно завершены; archived authority достаточна для recovery |
| Active position/account openings | До окончательного settlement/void, затем latest account всё ещё нужен | Новые commitments и recoverable openings приняты; старый witness не нужен для незавершённых proofs/rollback |
| Fidelity active/sold history | Future league, forced Out и exact owner query могут требовать её намного позже Tribute window | Только доказанный sufficient representation/checkpoint для **всех** этих consumers; простого current balance недостаточно |
| Global source/supply accounting | Lifetime supported assets, imports и conversions | Сохраняется anti-replay и source-complete conservation; суточное закрытие не удаляет obligation |

Следовательно, 50h offering + 12h waiting и пример hourly committee rotation не задают срок жизни всех private данных. Приватные Intex obligations, активные кредиты и Fidelity sold history могут жить дольше. Их shares, recovery ciphertexts, re-sharing и full replay/checkpoints добавляют storage/communication/workload. Прежние estimates per-Tribute или measurements на 256 records не измеряют эти дополнительные contracts; из этого не следует ни невозможность, ни доказанная доступность при 512 МБ. Жёсткий wallet limit `512000000 B`, включая cold proving key, остаётся отдельным gate.

| Решение | Что сохраняется по умолчанию в анализе | Что пока не выбрано |
|---|---|---|
| D15. Intex asset/round | Native COEN backing, certified per-leaf floor, один funded round, late burn, отсутствие claim expiry | Private destination/cash-out; видимость H/paid/remainder; representation burn; legacy cutover |
| D16. Compartments | Pending/active/liquid conservation; owner-offline release/void; LIFO и execution times | Direct external update либо pending inbound merge; public collateral terms/stake/reserve disclosure (owner в основном кандидате публичен) |
| D16U. Units | Fixed6 source terms не изменяются молча; new Gratis fixed18 | Preserve six-decimal collateral grid либо новые floors18; PromisLimit precision/residual handling |
| D17. Sources | Promis conversion существует и экономически 1:1; new credit=z6*K | Полный private source adapter/cutover; upstream lifetime proof; committed supply/query API |
| D18. Owner read | Exact integer RCFI, выбранные state и evaluation time | Wallet-only/changed ABI либо private-output service; historical retention и recipient authorization |
| D-DA. Witness custody | Offline mandatory writers не зависят от нового owner proof | Конкретный MPC/coSNARK, shares/commitment/encryption binding, repair/erasure model, DA SLA и benchmark |

## 4. Что проверено, а что остаётся предпосылкой

**Исполнено в этой дополнительной проверке:** прямое чтение приведённых source sections; targeted caller/writer search по Gratis API/state setters, Fidelity writers, PromisFactory mint; малый Python arithmetic check для `104/136/176/208`-битных source/lifetime bounds, Intex exact-leak/floor-dust examples, различий collateral ceil/floor6→18 и conversion U256 cap. Все числовые assertions прошли. Использован primary RFC 9180 для ограничений предложенного transport encryption.

**Source-only:** authority/call order, текущие storage writes, runtime checkpoint в Intex batch, events/ABI, TEE calls, scheduled void и formulas. Полный execution rollback в EVM, source authenticity всего Promis issuance, правильность oracle/deployment, genesis/import semantics и privacy сети здесь не доказаны.

**Не исполнено:** новые no-TEE transitions, malicious distributed prover, verifiable recovery encryption, committee repair/rotation, crash/reorg/DA сценарии, новый полный wallet proof или benchmark до 512 МБ. Исторические arithmetic/prototype measurements не переименованы в validation этих новых downstream contracts. Production код этим дополнением не изменён.

Документ делает конкретными обязательные consumers и варианты их замены. Решения из таблицы D15–D-DA остаются открытыми; до их принятия и проверки полного исполнения весь маршрут без TEE и с требуемой amount privacy не объявляется завершённым.
