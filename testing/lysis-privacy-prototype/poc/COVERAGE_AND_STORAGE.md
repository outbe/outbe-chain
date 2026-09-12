# Trace исполнения и хранение

Это описание **реализованного эксперимента**. Исходные требования и production trace остаются в [PROTOCOL_TRACE_AND_REQUIREMENTS.md](../PROTOCOL_TRACE_AND_REQUIREMENTS.md). Результат `experimental_lifecycle_executed` означает завершение перечисленных сценариев; `full_production_protocol_pass=false` сохраняет незакрытые production adapters и security gates.

## Кто и что считает

`a6` — nominal fixed6; `f6` — окончательный коэффициент; `p6` — цена; `g18=a6*f6*10^6`, `c18=a6*f6*p6`. Эти два произведения не требуют округления. Source nominal, коэффициенты, Fidelity и Intex по-прежнему имеют деления/сравнения: fixed integer не отменяет их.

| Trace | Исполнитель; вход → проверка/расчёт → выход | Выполненная граница |
|---|---|---|
| R00 source | Source helper: private canonical draft, signer, IMT path → настоящий P_L2 → 8900 B FullProof и четыре public поля | Реальная криптография; fresh one-leaf roots и авторизованный test registry, без production BLS/root adapter |
| R01 P_link | Wallet: source amount/draft ID + public SU/prices → canonical hash, integer quotient/remainder, range, C(a) → Groth16 proof | Реальный proof; C(a) создаёт wallet и передаёт тот же C в VSS |
| R02 admission | Node: P_L2/P_link → обе проверки, equality public fields, SU uniqueness, C(a)=VSS constant, signed holder receipts → запись Tribute | SQLite durable baseline; реальные shares/AEAD/signatures. В этом runner обязательны все 3 receipts; admission при недоступном holder не проверен. Нет consensus/reorg/DA challenge simulation |
| R03 rotation | Старые holders: собственные shares → weighted re-sharing → новые shares/commitments у нового состава | Две реальные смены состава: посередине offering и после последнего Tribute. 2 старых → 3 новых; старые epoch файлы удержаны. Нет adversarial churn/secure erasure |
| R04 S | Holders: shares принятого closed root → aggregate shares. Node проверяет signatures/root/coverage и открывает S | До close opening отклоняется; индивидуальные a не открываются |
| R05 budget | Node: S и заданный публичный scarce budget → B=.25*S18 | Ограниченный fixture Green при D=.32*S18, E=.25*S18. Полный Green/Red/A/K/Metadosis request adapter не портирован |
| R06 Fidelity | 3 MPC actors: shares cohorts, public времена/decay/global context → exact checked integer guards и league | Нетривиальная история одного owner, остальные initial-empty league1. Не benchmark 256 сложных Fidelity histories |
| R07 S_l | После league snapshot holders группируют retained shares → Node проверяет и открывает S_l | Реальный VSS aggregate; S=ΣS_l; private test oracle сравнивает с исходными fixtures |
| R08 fractions | Node: S_l/counts/B → **текущий Rust Lysis kernel**, f_avg=B/(S6*10^6), f_max=2*f_avg → final f_l, G18≤B | Exact public kernel плюс явно добавленная fixed18 reconciliation. Current production fixed6 behavior не выдан за fixed18 |
| R09 Nod | Worker: C(a), public terms/final fractions → descriptor и shard root | До 256 **записей**, fsync binary и SQLite commit. f=0 → NoEntitlement, не claimable Nod. Production OCOMP body не портирован |
| R10 budget conservation | Node: S_l/f_l → G18=Σ S_l*f_l*10^6, U=B−G → public result | Проверка точного бюджета; public certificate/OCOMP consensus и reuse receipt в production не реализованы |
| R11 call/terms | Test runtime: final price/floor/deadline/called → terms в Nod и проверка при claim | Call/auction/oracle dynamics заданы fixture; не полный production qualification/settlement machine |
| R12 claim | Wallet: source opening + old balance/payment openings + f/p → proof g/c и новых notes. Node: proof/signature/rights/payment + committee cohort cert → atomic claim | Реальные private g/c, payment change/escrow; test genesis funding. Fixed18 settlement asset — экспериментальная модель, bridge к asset с 6 decimals не портирован |
| R13 payment/COEN | Wallet: private balance → move proof, pending incoming, receive; затем withdraw proof → public COEN amount и private change | Реальные proof/conservation/replay checks. Payment — self-transfer через pending I; cross-owner encrypted delivery не проверена |
| R14 expiry | MPC: shares реально оставшихся прав + f → F18; private returned-limit limbs → certificates, закрытые rights и durable private limit note | F публично не открыт; shares сохранены у holders; oracle проверяет точность. **Возврат в следующий публичный PromisLimit не реализован** — policy/integration gate |
| R15 Intex | MPC: hidden eligible a/H, public funded P → floor(P*a/H) на право, private payout/remainder; owner recovery и COEN cashout | 2 разных права, exact secret division, private backed asset notes; independent floors, remainder burned. Funding genesis/round fixture, не весь production Intex |
| R16 writers | Wallet/MPC: L→T, authority T→A, release A→L, forced A burn → same account/cohort root | Реальные same-root money/history commits; private LIFO, offline forced writer, stale version rejection. Public collateral fixed6 — экспериментальная disclosure policy |
| R17 Promis | Wallet: committed private Promis source → burn proof и Gratis mint по *10^12 → same-root update | Source note расходуется один раз; ни TEE, ни MAC. Все upstream Gem/Intex origins не портированы |
| R18 query | Owner: decrypt/reconstruct новые cohort openings, проверить C → exact текущий Rust Fidelity на query timestamp | Exact RCFI сохраняется только в private wallet output; публично — результат проверки. Network private-query ABI не реализован |

Локальные процессные вызовы и сигнатуры — часть измеренного baseline. Они не подтверждают distributed availability, Byzantine recovery или отсутствие сговора threshold.

## Кто создаёт commitments

| Объект | Создатель | Как проверяется связь |
|---|---|---|
| C(a6), 32 B | Wallet, со случайным blinder | P_link доказывает тот же nominal, который связан с canonical source hash |
| VSS polynomial C0,C1 | Wallet при admission; старые holders при ротации | C0=C(a); share equation; при ротации weighted evaluation link и сохранение общего constant |
| C(a) в Nod | Новый commitment не создаётся | Копируется ровно из принятого Tribute |
| C(balance/payment/escrow/Promis), 4×32 B | Wallet | State proof проверяет четыре uint64 limbs, integer conservation/формулы и source inputs |
| Новые cohort/payout/limit commitments | MPC holders | Local share commitments → checked interpolation polynomial; каждый holder проверяет свою долю. Комитет подписывает output hashes и executed metadata/context |
| Cohort root, Nod shard root | Runtime | Hash канонического public manifest; amount openings не входят в public manifest |

MPC создаёт новые blind shares совместной случайностью. Owner получает encrypted recovery packets и самостоятельно восстанавливает значение и blinders, проверяя commitments. Coordinator получает только commitments, public metadata и validity/league outputs.

## Что видно публично

Публичны source/owner identifiers, source count и SU IDs, currencies/day, цены/коэффициенты, P_L2/P_link, C(a), VSS polynomial commitments, encrypted packets и receipts, final accepted root, S/S_l после close, league, B/G/U, Nod terms/deadline/spent state, commitments всех notes, операции и roots/version. В экспериментальном Fidelity profile видны времена, число и порядок padded slots; нулевые размеры и реально затронутые LIFO суммы скрыты. В pledge fixture публична сумма collateral. COEN cashout раскрывает выводимую сумму и recipient.

Скрыты source amount, nominal, Pedersen blinders, индивидуальные g/c, payment change, Gratis compartments и размер cohorts, точный RCFI, Intex denominator/payouts, F при expiry. Source и owner знают собственный source amount; публичные proofs не должны сопровождаться публикацией исходного private draft. Fixture использует случайный скрытый draft ID. Наличие public hashes само по себе не исправляет upstream публикацию amount/draft или слабую случайность identifiers.

Криптографическая конфиденциальность этого эксперимента условна: один passive holder не знает nominal, **два holders одного epoch могут восстановить его**. Для production нужны выбранная corruption model, active-secure input binding/MPC и проверяемый handoff с erasure. Подписи честного committee не превращают MPyC в malicious-secure протокол.

## Кто хранит и до какого момента

| Место | Данные | Retention / recovery |
|---|---|---|
| Wallet | Private source draft/base/atto/draft ID, a/blinder, source witness; balance/payment openings; owner encryption/signing keys; recovered cohort values/blinders | До прекращения соответствующего права; новые witnesses и history нужны после offline writes. Seed без encrypted payload/history не восстанавливает состояние |
| Wallet prover cache | PK соответствующего SU profile и state circuits | Общие публичные параметры, не per-Tribute payload; могут скачиваться заново. Cold RSS включает их загрузку |
| Каждый текущий holder | Своя nominal share+blind share; четыре shares/blinds на произвольную uint256 note; public polynomial/epoch/ID metadata; MPC outputs | Nominal нужен для поздней league grouping, expiry и Intex. Получение S не разрешает удалить per-right state |
| Старый holder | Старый checkpoint/shares до durable acknowledgement нового состава | В PoC **все старые эпохи остаются на диске**. Production erasure/repair policy не реализована |
| Public network/DA | Proofs, commitments, manifests/receipts, encrypted handoff/recovery packets, Nod descriptors, anti-replay markers и roots | Active rights/history должны быть доступны; что именно будет consensus state, DA или архивом — production storage adapter ещё предстоит выбрать |
| Owner recovery archive | Encrypted shares новых cohort/payout notes и authenticated public metadata | Удерживать, пока owner может восстановить актуальный witness, включая offline forced writes |
| Test-only oracle | Доступ к synthetic witnesses разных actors | Только тест, не роль сети. Не включается в production storage model |

В текущем коде holder хранит **целую JSON map**, загружает её в RAM и переписывает при каждом admission. Это O(N) resident state и O(N²) суммарной записи при последовательном добавлении; реализация является измерительным baseline и непригодна для миллиарда записей. Индексированное хранение/streaming снимает это конкретное ограничение реализации, но **не превращает нужные per-right shares в O(1)**.

Один nominal требует минимум двух scalar shares по 32 B = **64 B на holder**, плюс два public polynomial points = **64 B на record** в degree1 profile; IDs, indexes, signatures, encrypted transport и replicas добавляются. Произвольная uint256 note требует четырёх limbs: 128 B commitments и минимум 256 B private shares на holder. Реальные JSON размеры и projections приведены в RESULTS отдельно от этих нижних границ.

При ротации 2→3 для N retained nominal records передаётся минимум 2×3×64×N B свежих secret evaluations; commitments/AEAD/metadata увеличивают передачу. Для N=10⁹ это **384 GB только share-pair payload на одну смену**. PoC исполнил две локальные ротации небольшого набора; 50 часовых ротаций и WAN на миллиарде records не запускались.
