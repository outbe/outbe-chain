# Независимая проверка арифметики PoC — reviewer A

Дата: 2026-09-11. Проверка read-only кода; создан только этот отчёт. Полный chain harness во время проверки ещё строился другим агентом. Отчёт не подтверждает его интеграцию, production support или отсутствие замечаний во всём протоколе.

**Вердикт по арифметике:** для канонических ограниченных входов проверенные limb equations сохраняют целые суммы, связывают один и тот же nominal с source commitment и claim и отвергают переполнение uint256. Обхода этих equations через редукцию по полю не найдено. Есть дефект каноничности host witness и два ограничения относительно целевого протокола, перечисленные ниже.

**Вердикт по полноте:** это отдельные relations. Их наличие не доказывает source authority, одноразовость расходования, asset backing, state freshness, Fidelity, приватный Promis conversion или RAM полного prover. Данные свойства требуют проверки потребителя/harness; в данной задаче он не аудировался.

## Проверенный snapshot и границы

Полностью прочитаны:

| Файл | SHA-256 |
|---|---|
| `src/state.rs` | `219f72b5f6ea47709e0815004d047b17ac41e26e25da5658a9af31d46ed763af` |
| `src/link.rs` | `048706c7812a637b8e11fcd9583dadc9aa8452cc9c7c9250f733677589b8faa5` |
| `../measurements/p-link/src/integer.rs` | `12d2c064731095ade2ce5d02dab5691764f7834b88b95260aa66327e07fffaab` |

Дополнительно прочитаны необходимые для binding helpers `crypto.rs:14–61`, `vss.rs:31–34`, producer fractions `crates/core/lysis/src/algorithm.rs:20–307`, текущие R17 и aggregation bound, dependency pins и локальный исходник полей/bit decomposition arkworks 0.5.0. Graph MCP отсутствует; использованы точные исходники и `rg`, без заявления generation/coverage.

## Замечания

### SC-A-01 — Medium: allocator молча отбрасывает полные старшие limbs входного BigUint

**Место:** `../measurements/p-link/src/integer.rs:67–87`, `UInt::alloc`; потребители `src/state.rs:64,77`, `src/link.rs:340`.

`UInt::alloc(value,width)` извлекает только `ceil(width/64)` limbs и сохраняет исходное полное `value` отдельно. Отсутствует проверка `value.bits() <= width`. Поэтому `alloc(2^256+7,256)` получает ровно те же constrained limbs, что `alloc(7,256)`, а `alloc(2^128+123456,104)` — что `alloc(123456,104)`. Это не просто отсутствие удобной валидации: поле `UInt.value` перестаёт обозначать целое, представленное constrained limbs.

Конкретный source-derived witness: взять корректный `withdraw` с old=7, new=4, public amount=3 и заменить только приватную строку old.value на `2^256+7`, сохранив public commitments и blinders. `state.rs:64–68` откроет прежние limbs, а `state.rs:90` сравнит сумму 4+3 с ними. В проверенных equations нет условия, отвергающего изменённую строку. Выполнение такого R1CS/proof здесь не запускалось. `Note::fresh` и `Note::commitments` отвергают широкий вход, но `generate_constraints` их не вызывает для private notes и принимает десериализованный `Transition`.

Это **не доказательство создания средств**: committed integer остаётся 7, conservation low limbs сохраняется. Риск — неоднозначность witness/API и расхождение с host accounting, если потребитель доверится исходной строке. Аналогично `opening` в `state.rs:48–49` берёт лишь 251 low bits строки blinder: условие `<q` относится к этим bits, а не ко всей строке.

**Минимальное исправление:** отвергать `value.bits()>width` в `UInt::alloc` до allocation; привести `bits`/blinder parsing к явному контракту и проверять scalar строку через canonical `scalar()`. Проверить boundary cases `2^256`, `2^256+7`, `2^128+123456` при width104; сохранить отрицательные overflow checks арифметических операций, адаптируя их к возможному раннему `Err`. Это исправление представления данных, не новая протокольная граница.

### SC-A-02 — Medium для целевого R17: mint amount публичен

**Место:** `src/state.rs:30–31,39–44,70–74,91`; целевой контракт `../PROTOCOL_TRACE_AND_REQUIREMENTS.md:375–379`, `../DEEP_RESEARCH_IMPLEMENTATION.md:335`.

`Public.amount` раскладывается в public inputs[6..10]. Для `kind="mint"` эти четыре limbs однозначно раскрывают `m6`; mint18 затем равен `m6*10^12`. Скрытые old/new note commitments не скрывают delta. Это допустимая arithmetic fixture, но не приватный Promis→Gratis route R17: research прямо требует заменить публичный amount или явно ограничить профиль.

**Минимальное исправление для полного R17:** поместить m6 в private witness, связать его с authenticated private burn commitment/source и nullifier, а mint18 — с тем же integer через overflow-checked multiplication. Public context должен фиксировать source/version/asset и потребитель должен атомарно проверить burn authority и одноразовость. Если задача пока только проверить conversion arithmetic, явно назвать эту relation публичным mint fixture и не засчитывать private R17. Для withdraw раскрытие amount соответствует public COEN exit и само по себе не является этим дефектом.

### SC-A-03 — Medium, неподтверждённая полнота диапазона: claim вводит fraction≤10^6

**Место:** `src/state.rs:31,39,70–71,80`; `../DEEP_RESEARCH_AGGREGATION.md:209`.

В research явно не принято `f_l6≤10^6`; PoC проверяет именно этот предел и 20-bit fraction, а `a*f` помещает в 128 bits. Следовательно, сейчас это ограниченная relation, а не доказанно полный интерфейс final fractions. Механически public fraction=1_000_001 отвергается ещё до circuit synthesis.

**Не доказано:** что именно такой коэффициент достижим текущей полной Lysis policy. Малый самостоятельный поиск по 30 000 distributions с integer корнями, signed truncation и normalization не нашёл fraction>10^6; это не доказательство отсутствия. Замечание касается не обмана arithmetic proof, а необоснованного сужения относительно принятого контракта.

**Минимальное исправление:** либо доказать достаточный bound для точного producer и зафиксировать его как source-derived bound, либо убрать новый cap и использовать width, выведенный из реального integer type/producer, расширив `a*f` соответственно. Например, поддержка всего нынешнего host `u64 fraction` требует 64 bits для f и 168 bits для a*f; это всё ещё не подтверждает, что u64 покрывает полный source U256. Финальные g/c остаются uint256 с явным отказом при overflow. Не вводить протокольный cap ради меньшего circuit.

## Что подтверждено в ограниченном scope

| Требование | Вывод и доказательство |
|---|---|
| Hidden uint256 balances | `state.rs:62–68`: четыре независимых 64-bit opening. Целый balance не кодируется одним scalar и может превышать q. `Note::fresh` выбирает четыре свежих blinders. Каноничность host строк требует SC-A-01. |
| Conservation move | `state.rs:89`: обе суммы 2×uint256 представлены 257 bits. Верхний carry учитывается; сумма не редуцируется modulo q/p. Proof relation сама по себе не проверяет owner/asset/source consumption. |
| Overflow add/mul | `integer.rs:142–158,177–190`: все output/high columns проверяются, недостающий output limb равен нулю, финальный carry равен нулю. Поэтому c>U256_MAX или newBalance>U256_MAX не превращается в меньшую сумму. |
| No-wrap в R1CS | B=2^64. В mul максимум 16 произведений на колонку при width≤1024: LHS<16(B−1)^2+2^72<2^133; RHS<(2^72)B+B<2^137. Оба меньше BN254 Fr. Addition LHS<2^65; RHS<2^65. Range bits исключают отрицательные carries. |
| Nominal104 | `link.rs:320–351`: u6=base64*M+atto20, atto<M; VI,VR>0, effective=max(VR,SC). a=floor(u6*M*VR/(VI*effective)), поэтому a≤u6*M<2^104. Quotient/remainder equation и r<den доказывают точное floor. |
| Link commitment | `link.rs:340,349–364`: те же a.bits участвуют в quotient и aG+rH. a104<qBaby; нет независимого reduced nominal witness. `state.rs:77–82` открывает публичный source point тем же a, который умножается на f/p. Связь между двумя proofs требует равенства source/commitment у verifier. |
| Claim units | `state.rs:80–87`: g18=a6*f6*10^6; c18=a6*f6*p6. Это точные fixed6→18 formulas без лишнего floor. p — positive uint256; c output256 является явной cost bound, не предположением, что любой p допустим. Old payment = change+c, escrow=c, new Gratis=old Gratis+g. |
| Conversion | `state.rs:91`: m6*10^12 и old+mint проверяются в uint256. Формула overflow-safe для допустимого m6; приватность SC-A-02 и burn authorization отдельно. |
| Public input binding | `link.rs:94–122,237–285` воспроизводят одинаковый ordered vector и domain hash; `state.rs:38–44,57–59` связывает context, terms и все note/source points. Native hash/circuit hash и Poseidon assumptions остаются dependency assumptions. |
| P_link source SU32 | `link.rs:38–39,70–88,262–265,308–318`: capacity задаётся длиной vector и circuit shape; source_count≤capacity, inactive slots zero, active IDs строго упорядочены. В relation нет cap32. `fixture:173` ограничивает только helper 1..1024; u16 source_count остаётся представлением PoC, а не утверждённым protocol max. |
| Low RAM shape | State имеет фиксированное число notes (claim5, move4, withdraw/mint2), по четыре fixed64 opening. P_link растёт с выбранной source capacity. Нет circuit loop по миллиарду records. Из этого не следует peak≤512000000B: необходим full proof/cold PK benchmark. |

Для full public-input binding verifier обязан самостоятельно выбрать зарегистрированный VK для kind/capacity и собрать inputs из принятого state/terms. `context` — opaque field: circuit не интерпретирует account, nonce, asset, deadline, old state roots, accepted oracle или `P_L2` authority. `link.rs:245` включает merkle_root в контекст, но не проверяет membership. Это ожидаемая граница отдельного P_link, если consumer действительно проверяет независимый P_L2 и exact shared values; эта consumer проверка здесь не выполнена. Комментарий `state.rs:84` об atomic consumption не следует из equations и должен подтверждаться runtime.

## Первичные источники и dependency assumptions

Прочитан официальный [ark-groth16 0.5.0 verifier](https://github.com/arkworks-rs/groth16/blob/v0.5.0/src/verifier.rs#L22-L34): `prepare_inputs` требует точную длину input vector; `verify_proof` использует этот vector. Это библиотечная проверка формы, не authority supplied values.

Локальные pinned crate sources `ark-bn254-0.5.0/src/fields/fr.rs` и `ark-ed-on-bn254-0.5.0/src/fields/fr.rs` подтверждают p и q; первичные опубликованные source pages: [BN254 Fr](https://docs.rs/ark-bn254/0.5.0/src/ark_bn254/fields/fr.rs.html), [Baby scalar](https://docs.rs/ark-ed-on-bn254/0.5.0/src/ark_ed_on_bn254/fields/fr.rs.html). Прочитан `ark-r1cs-std-0.5.0/src/fields/fp/mod.rs:498–503`: `to_bits_le` дополнительно вызывает `Boolean::enforce_in_field_le`; сравнения source IDs и decomposition draft_id опираются на canonical field representation.

Локальный `crypto.rs:26–44` получает H через domain-separated SHA256 point decoding и cofactor clearing, а не через опубликованный scalar×G; `decode:55–61` использует validated canonical deserialize и отвергает trailing bytes. Безопасность hiding/binding остаётся зависимой от свежих blinders, неизвестности log_G(H), DLP и корректности curve/serialization gadgets. Это не новый аудит hash-to-curve suite. Groth16 setup, RNG, side channels, proving-key RAM, transport, erasure и distributed prover privacy данной проверкой не подтверждены.

## Выполненные проверки

- Прочитаны все строки трёх основных файлов и точные перечисленные dependencies; зафиксированы hashes.
- Запущены малые Python integer assertions для p/q, колонок carry, source a104 bound, mint max и allocator aliases. Все assertions прошли.
- Запущен самостоятельный точный integer поиск 30 000 Lysis distributions только для проверки потенциального fraction counterexample; counterexample не найден. Это не Rust execution и не theorem.
- Не запускались Rust build, R1CS satisfiability, proof generation/verification или memory benchmark: harness/benchmark параллельно выполняет parent. Заявления о satisfiable alias выше основаны на полном просмотре соответствующих equations, не на executed proof.

После правок требуется повторно проверить изменённые участки; этот отчёт привязан к указанным hashes.
