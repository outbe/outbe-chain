# Variant 2: независимая проверка криптографической конструкции

Дата: 2026-09-11. Scope: предложенные Twisted ElGamal + Bulletproofs + Sigma, bitwise Baby↔Ristretto bridge, exact uint256 conservation, recovery и граница recipient acceptance. Это **design review**, не audit появляющегося параллельно backend, не выполненный proof test и не RAM benchmark. Изменён только этот файл. Graph tools отсутствуют; project/generation/coverage не заявляются. Research выполнен как независимая фоновая задача по research skill.

**Theory verdict: условно корректно.** Joint OR для одного общего бита в двух группах, локальные связи commitments и bounded carries дают понятный soundness argument без опасного общего integer response между разными scalar fields. Условия ниже должны проверяться verifier, а не подразумеваться honest prover.

**Completeness verdict: полного протокола пока нет.** Parent уточнил, что variant2 будет использовать recipient acceptance с атомарной финализацией, не обещая немедленное зачисление offline. Это корректная граница эксперимента. Сохранение source/Nod, VSS и Fidelity требует authenticated registry и прежней проверки money/history context; value-only recovery не заменяет Baby history openings. Изолированный cross-owner prover и экономическая семантика нового cross-owner Gratis transfer остаются отдельными вопросами.

## 1. Проверенная исходная граница

HEAD при чтении: `177a72ddbea9f2e52eef094405481292ecd56046`. Следующие файлы прочитаны непосредственно; ссылки указывают на живой workspace, а не утверждают неизменность параллельной реализации:

- [crypto.rs:14](../src/crypto.rs#L14): scalar conversion отвергает integer ≥ modulus; [generators():26](../src/crypto.rs#L26) выбирает Baby G/H с cofactor clearing; `commit` вычисляет `mG+rH`. Нужна неизвестность discrete log между G и H; deterministic derivation сама по себе не является доказательством этого предположения.
- [state.rs:29](../src/state.rs#L29): `Note` содержит uint256 value и четыре независимых Baby blinder; `commitments()` разбивает значение на четыре 64-bit limbs. [Transition:156](../src/state.rs#L156) range-checks эти limbs и проверяет openings.
- [state.rs:227](../src/state.rs#L227): claim/mint/pledge остаются отдельными целочисленными relations; [move:248](../src/state.rs#L248) сравнивает две суммы с 257-bit intermediate, не одну сумму по scalar modulus.
- [cohort_bridge.py:27](../cohort_bridge.py#L27): VSS constants должны в точности совпасть с `statement.notes`; [commit:103](../cohort_bridge.py#L103) проверяет monetary digest, account/history context, version/root и exact timestamp перед обновлением в общей SQLite transaction. Комментарий в начале файла явно ограничивает committee model passive/honest-majority.
- [Gratis precompile:16](../../../../crates/core/gratis/src/precompile.rs#L16) и [ветви ABI:48](../../../../crates/core/gratis/src/precompile.rs#L48) запрещают transfer/approve. Поэтому cross-owner confidential transfer — новый разрешённый пользователем криптографический эксперимент, а не уже существующая production Gratis economics.

Точный Baby scalar modulus прочитан в установленном `ark-ed-on-bn254-0.5.0/src/fields/fr.rs:4`:

`q_B = 2736030358979909402780800718157159386076813972158567259200215660948447373041`.

Ristretto scalar modulus — `q_R = 2^252 + 27742317777372353535851937790883648493`, по [RFC 9496 §4][ristretto]. Оба больше `2^128`. Ни один не кодирует произвольный uint256 без limbs.

## 2. Joint OR: один бит, две группы

Для каждой позиции j создаются независимые commitments:

\[
D^B_j=b_jG_B+\rho^B_jH_B,\quad D^R_j=b_jG_R+\rho^R_jH_R.
\]

Правильная relation:

\[
\bigl[\exists\rho_B,\rho_R: D_B=\rho_BH_B\ \land\ D_R=\rho_RH_R\bigr]
\quad\lor\quad
\bigl[\exists\rho_B,\rho_R: D_B-G_B=\rho_BH_B\ \land\ D_R-G_R=\rho_RH_R\bigr].
\]

Это **AND внутри OR**. Два независимых доказательства «Baby содержит бит» и «Ristretto содержит бит» допускают `b_B=0,b_R=1` и не дают bridge.

В ветви k∈{0,1} обе группы используют одно целочисленное challenge `c_k∈[0,2^128)`, но разные scalars responses. Для группы X∈{B,R}:

\[
z_{k,X}H_X=A_{k,X}+c_k(D_X-kG_X).
\]

Общий challenge `c=H128(transcript)` проверяется как **XOR 16-byte strings**: `c_0 XOR c_1=c`. Это не сложение в Baby field и не сумма modulo Ristretto order. Simulated ветвь строится выбором c,z и вычислением `A=zH−c(D−kG)`; real ветвь отвечает после получения c. Нужны независимые randomness и responses для двух групп.

Основание конструкции — [Cramer–Damgård–Schoenmakers, §3, §4 Theorem 8][cds]: распределение challenge по secret-sharing relation и композиция special-sound, honest-verifier ZK protocols. Визуально прочитаны PDF pages6–9, включая XOR sharing, условия theorem и extraction argument. Конкретная instantiation ниже — самостоятельный вывод для наших двух групп, не утверждение, что авторы анализировали Baby↔Ristretto.

**Extraction argument.** Возьмём два accepting transcripts с одинаковыми D и A, но разными общими challenges c,c′. Хотя бы для одной ветви k имеем `c_k≠c′_k`. Разность меньше обоих порядков по абсолютной величине, поэтому ненулевая и обратимая в каждой группе. Извлекаются

\[
\rho_{k,X}=(z_{k,X}-z'_{k,X})/(c_k-c'_k)\pmod {q_X}
\]

**для одного и того же k** в обеих группах. Никакого равенства численных blinder или responses между группами не требуется. Связь значения бита затем опирается на computational binding Pedersen: знание openings одновременно как0 и1 раскрывает discrete log между G/H.

Это доказывает special soundness интерактивной конструкции и объясняет её intended Fiat–Shamir instantiation в classical random-oracle model. Здесь нет полного reduction для всего ledger. `128-bit challenge` — параметр knowledge-error, не обещание общей128-bit security: 251-bit Baby subgroup даёт меньший generic-DLog security scale, а число proofs/hash queries влияет на concrete bound.

## 3. Как связать bits с исходными commitments

Необходимо **локальное**, а не общее 256-bit weighted equality. Для Baby limb ℓ и Ristretto chunk i:

\[
C^B_\ell-\sum_{j=0}^{63}2^jD^B_{64\ell+j}=\delta^B_\ell H_B,
\]
\[
F^R_i-\sum_{j=0}^{15}2^jD^R_{16i+j}=\delta^R_iH_R.
\]

Schnorr knowledge proof для каждой δ связывает исходные points с bit decomposition. Поскольку соответствующие message differences меньше `2^64` и `2^16`, scalar equality даёт integer equality при binding и существующих range guarantees. Нельзя заменить четыре Baby relations одной `Σ2^(64ℓ)C_ℓ`: это снова modulo-q отношение на uint256.

Verifier должен использовать ровно256 позиций, четыре64-bit limbs и шестнадцать16-bit chunks с одним fixed little-endian order. Нельзя принимать произвольные subset/duplicate/reordered indices, truncation или разные upper-bit policies на сторонах. Для отдельного104-bit nominal возможен иной **явно типизированный** bridge; его нельзя смешивать с generic256-bit note.

По сообщению parent, новые Baby notes связываются с fresh F; прежние Baby↔cipher связи сохраняются в verified registry. Это sound composition только если registry entry идентифицирует **тот же note/account, asset, key epoch, ciphertext bytes и Baby commitments** и создаётся после проверки полного bridge. Наличие произвольной записи в SQLite не делает её verified registry.

## 4. Старый ciphertext: recovery без старого r

Используется проверенная [Aptos convention][aptos-cipher]: `EK=s^-1 H`, `(C,D)=(aG+rH,rEK)`, `C−sD=aG`. Родитель уточнил подход для старого ciphertext:

\[
H=sEK,\qquad C-F=sD-r_FH,\qquad F=aG+r_FH.
\]

Один joint Sigma доказывает общий s в key и ciphertext relations, F range-proved и связан с арифметикой/bridge. Владелец восстанавливает a через DK и bounded DL, выбирает **новый** r_F; старый encryption r не нужен. Это устраняет ошибку конструкции, которая требовала бы неизвестное old-r opening исходного C.

Условие: эти старые ciphertext chunks должны быть **нормализованными16-bit chunks**, либо relation должна явно обрабатывать ненормализованный формат. Aptos pending chunks могут быть32-bit после накопления; напрямую доказать их равенство16-bit F невозможно. Их перенос требует weighted normalization relation с carries. Variant2 следует закрепить normalized-only codec/registry profile, если pending aggregation не реализуется.

Для новых ciphertexts требуется Sigma правильного encryption того же range-proved a под **конкретным owner EK**, а не только proof открытия C. Nonidentity keys, canonical valid points/scalars, subgroup membership Baby и rejection malformed encodings обязательны. Identity как zero-value commitment допустим там, где это разрешает relation; blanket rejection всех identity points сломает корректные нули.

## 5. Exact uint256 через signed local carries

Рассматриваем уточнённую parent relation `L0+L1=R0+R1`. Каждый operand состоит из16 range-proved chunks `<B`, `B=2^16`. Для каждого i=0…15:

\[
E_i=L_{0,i}+L_{1,i}+t_i-R_{0,i}-R_{1,i}-Bt_{i+1}=0,
\]

где `t_0=t_16=0`. Для внутренних carries parent выбрал `t_i=u_i−128`, BP8 доказывает `0≤u_i≤255`, то есть `−128≤t_i≤127`.

**No-wrap bound.** Из диапазонов operands и carries следует консервативное `|E_i|<2^25`, существенно меньше q_R. Поэтому групповая проверка локального message-zero equality не может скрыть ненулевой integer E_i. Умножив каждое равенство на B^i и сложив, получаем:

\[
L0+L1-R0-R1=B^{16}t_{16}-t_0=0
\]

как равенство **целых чисел**, даже если промежуточные суммы имеют257бит. Не нужно упаковывать весь operand/sum в scalar. При ranges digits и t0=0 сами равенства принуждают фактические carries к{-1,0,1}; более широкий BP8 интервал достаточен для no-wrap argument.

Для commitment U_i сообщения u_i положить `K_i=U_i−128G`, а endpoints задать как публичные zero commitments. Проверяемый excess:

\[
Z_i=F_{L0,i}+F_{L1,i}+K_i-F_{R0,i}-F_{R1,i}-BK_{i+1}=\eta_iH.
\]

Schnorr proof знания η_i вместе с ranges и binding устанавливает E_i=0. Независимо chosen η без знания correct excess opening не может обойти binding. **Нельзя** оставить carries произвольными field scalars, опустить endpoint или range-check только младшие биты. Например `t16=1` разрешил бы изменение total на `2^256`.

Для отдельных debit/credit relations `a_new+v+c_i=a_old+B c_(i+1)` достаточно unsigned BP8 carries; no-wrap имеет bound `<2^24`, затем индукция даёт carry0/1. Но нельзя смешивать этот unsigned convention со signed sum2=sum2: offset и знак являются частью proof type/transcript.

**Library boundary:** прочитан установленный `bulletproofs-5.0.0/src/range_proof/mod.rs:346–364`: verifier поддерживает n∈{8,16,32,64}, а не n=1, проверяет generator capacities. Поэтому BP8 для shifted carry — исполнимый API-профиль. Для четырёх operands×16 chunks одна BP16 batch требует capacity≥64, либо надо разделить proofs; default Aptos capacity16 недостаточна. [Primary aggregated range-proof documentation][bp-doc], [generators implementation][bp-gens]. Это source inspection, не запущенная проверка prover.

## 6. Transcript и композиция

Schnorr/OR proofs должны иметь фиксированный domain-separated transcript и непересекающиеся roles: bit bridge, Baby limb link, R chunk link, key possession, ciphertext decryption/validity, carry excess. [RFC8235 §§2.3,3.3,6][schnorr] подчёркивает encoding boundaries, statement/context binding и fresh nonces. Использование здесь128-bit challenge — отдельный явно выбранный профиль; RFC не заявляется как точная спецификация нашего wire format.

Минимальный связываемый context: protocol/version, обе group/generator identities, note/account/asset IDs, owner EK+epoch, old/new role, exact ordered commitments/ciphertexts, limb/chunk/bit indices, widths/sign/offset, operation ID, входные state versions/roots и monetary statement digest. Общий operation envelope должен связывать также source/Nod proof, VSS/cohort evidence, deadline и final execution context. Нельзя хешировать неоднозначную конкатенацию строк без lengths/types.

Challenge вычисляется после всех соответствующих first messages A. Для OR challenge include **обе ветви и обе группы**, а c_k имеют canonical16-byte encoding и интерпретируются одинаково. Responses — canonical scalars своих групп. Не допускается silent reduction arbitrary received bytes. Reuse Schnorr nonce на разных transcripts раскрывает witness.

Публичные доказательства отдельных components должны проверяться над **идентичными operands**, не просто быть валидными по отдельности. BP commitments должны совпасть с F, которые использованы в carry/ciphertext Sigma. Раздельные proofs для каждой локальной relation проще аудировать; при batching нужны независимые verifier coefficients или корректный FS transcript, включающий всё, что prover мог адаптировать. Этот документ не доказывает безопасность самодельного batched verifier.

## 7. Recipient acceptance и предел offline semantics

Если receiver balance может быть любым uint256, отправитель не знает скрытый текущий balance, а owner offline, обычные ElGamal linear updates и range-proof только **amount** не доказывают `receiver_old+v≤2^256−1`. Два состояния receiver —0 и `2^256−1` — неразличимы sender по privacy, но допустимость credit1 различается. Это объясняет недостающий witness; не является общей теоремой невозможности FHE/MPC решений.

Изначально рассматривавшийся escrow с individually bounded pending notes решает хранение notes, но меняет semantics: sender debit, ownership pending funds, момент Gratis credit, Fidelity acquisition time, expiry/refund, применение обязательных consumer operations. Capped queue ограничивает storage, но не гарантирует, что сумма notes когда-либо поместится в receiver available. Такая схема не должна называться сохранением текущего balance transition.

Parent выбрал **recipient acceptance + atomic commit**, без обещания offline immediate credit. Это допустимый ограниченный эксперимент: до acceptance есть intent; final balances, registry versions и соответствующий history effect устанавливаются вместе. Если sender balance/root сменился, proposal stale и должен отклониться либо доказуемо переподготавливаться. Если вводится reservation/escrow до acceptance, это уже отдельное денежное состояние с собственными cancellation/deadline правилами. При final commit нельзя применять timestamp первоначального intent как время acquisition без отдельного экономического решения.

**Более сильный путь без ожидания owner возможен в рамках прежнего PoC trust model:** существующий authenticated VSS/MPC может проверить recipient capacity и подготовить money/cohort transition перед atomic commit. Но для публичного BP+Sigma proof без раскрытия witnesses требуется конкретный distributed prover либо новый verifiable transition mechanism. Committee certificate сохраняет только заявленную passive/honest-majority модель; single-party BP proof не добавляет distributed privacy theorem. Альтернатива — source-complete proven lifetime supply bound `<2^256`, который исключает overflow каждого account по conservation; такого bound для всех sources здесь не установлено.

## 8. Cross-owner privacy, Fidelity и recovery

Один reference prover, которому передали оба old DK и оба balances, может подтвердить уравнения sum2=sum2. Это **не проверка изоляции кошельков**. Для полноценного two-owner flow предпочтительны отдельные sender debit и receiver credit proofs, связанные одним confidential amount commitment/dual-key ciphertext и подписанным operation context. Тогда каждый wallet знает собственный баланс и сумму перевода, но не баланс другого. Ещё вариант — доказанный distributed prover; он не следует из общего OR challenge.

DK recovery возвращает числовые ciphertext chunks. Она не восстанавливает Baby blinds, исходные Pedersen r, приватную историю Fidelity и source witnesses. Родитель отдельно сохраняет Baby history witness через existing VSS/recovery: это правильная декомпозиция, если shares durable, связаны с теми же registry notes и проверяются владельцем при recovery. [CohortBridge.prepare:38–43](../cohort_bridge.py#L38) действительно использует openings, а не только значение; money value-only backup недостаточен.

Baseline `move` — нейтральное перераспределение в одном owner scope: [prepare:30](../cohort_bridge.py#L30) выбирает direction neutral и один `self.owner` state. Для нового cross-owner transfer нельзя механически использовать это как update двух Fidelity histories. Нужно явно выбрать и проверить две авторизации, две history roots и принятую экономику — например, означают ли изменения Out/In, переносятся ли cohorts, либо primitive вообще не изменяет production Gratis. Этот review не выбирает такую политику за пользователя. Уже существующие claim/mint/pledge/forfeit/Intex paths должны по-прежнему использовать их source/economics proofs и точные consumer rules.

## 9. Итоговые gates и что уже уточнено

| ID | Условие/замечание | Текущий design status |
|---|---|---|
| V2-B01 | AND-of-two-groups внутри общего bit OR; local64/16 binding; canonical challenge/scalars | Конструкция условно sound; требуется сверка verifier implementation |
| V2-B02 | Signed carries range, offset128, endpoints0, все operands16-bit | Предложенный BP8 профиль sound по приведённому bound |
| V2-B03 | Не требовать old encryption-r; связывать old ciphertext с freshF через owner DK | Родитель уточнил правильную Sigma relation |
| V2-B04 | Offline automatic credit overflow и changing escrow semantics | Ограничено recipient acceptance; full offline feature не заявляется |
| V2-B05 | Two-owner wallet isolation, обе authorizations и history semantics | Отдельный gate; один общий local prover не является privacy test двух wallets |
| V2-B06 | Value-only recovery не даёт Baby/Fidelity openings | VSS recovery retained; необходимо проверить exact note/key/context binding и durability |
| V2-B07 | Registry не должен допускать unknown/unverified/stale old Baby→cipher links | Обязательный gate композиции, без source-only completion claim |
| V2-B08 | Source P_link/P_L2, Nod rules, VSS aggregates, Fidelity/Intex consumers | По заданию сохраняются; новые cipher proofs сами их не доказывают |

Теоретический wire-size ориентир, **не измерение**: при32-byte points/scalars per-bit bridge с двумя D, четырьмя A, четырьмя z и одним16-byte branch challenge занимает336B/bit, то есть86016B на256-bit note до framing. Ещё20 локальных Schnorr links при64B/proof добавляют1280B. Возможны другие безопасные layouts/optimizations, но нельзя переносить Aptos small-transfer sizes на этот bridge. Ни это число, ни небольшой BP не устанавливают wallet peak RAM с cold P_link/P_L2/32SU.

**Executed checks:** direct reads указанных local source paths; чтение установленного ark scalar и BP5 verifier/capacity source; первичные papers/specs/docs. CDS PDF имел испорченный text extraction, поэтому pages6–9 отрендерены локально и прочитаны. Benchmark/prover/cargo, production/remote mutations и доступ к secret fixtures не выполнялись. После подготовки документа следует отдельно сверить итоговую реализацию и выполненные scenarios с этими gates; этот отчёт не подменяет такую проверку.

[cds]: https://people.csail.mit.edu/rivest/voting/papers/CramerDamgardSchoenmakers-ProofsOfPartialKnowledge.pdf#page=8
[schnorr]: https://www.rfc-editor.org/rfc/rfc8235.html#section-2.3
[ristretto]: https://www.rfc-editor.org/rfc/rfc9496.html#section-4
[bp-doc]: https://github.com/dalek-cryptography/bulletproofs/blob/main/docs/range-proof-protocol.md
[bp-gens]: https://docs.rs/crate/bulletproofs/5.0.0/source/src/generators.rs
[aptos-cipher]: https://github.com/aptos-labs/aptos-ts-sdk/blob/da6319287572e8f62f38c73be0e2346ca7447e21/confidential-asset/src/crypto/twistedElGamal.ts#L65

<a id="executed-source-review"></a>

## 10. Executed source review: первая реализация, до исправлений

Дополнение от 2026-09-11. Это выполненное **чтение исходников и adversarial reasoning**, не исполнение adversarial transactions. Полностью прочитаны шесть файлов ниже. Для конкретных consumers дополнительно прочитаны `../src/{crypto,state,vss}.rs`, `../run_lifecycle.py::commit_transition`, `../cohort_bridge.py`, `../mpc_worker.py::main/update`, production `crates/core/fidelity/src/{runtime,tests}.rs`. Последние пути проверяют только необходимое правило zero In/Out; весь production Fidelity здесь повторно не аудировался.

Повторная проверка metadata доступных tools обнаружила только `mcp__open_design__list_projects`, относящийся к Open Design. Это не code graph. Graph project/generation/coverage отсутствуют; применён exact-source fallback. Cargo, prover, benchmarks и private fixture files при этом review не запускались и не читались. Сообщённые parent успешные tests и measurements не превращаются в independently executed evidence этого рецензента.

Snapshot прочитанной **pre-fix** реализации; последующие исправления должны иметь отдельный recheck:

| Файл относительно этого каталога | SHA-256 |
|---|---|
| `src/lib.rs` | `300888cb2656177e32a6cb332c9bf3ba619100a52e96995193bba89e94866294` |
| `src/money.rs` | `c1ffc6ca870c7c806d708e5a348567501c975d379953baaa4d0ec119a7a8be46` |
| `src/bridge.rs` | `5fb22d0e349436a3453d3e5e8ec1afce67957789eff00d5136e6df79ad66a312` |
| `src/main.rs` | `ff8b70bd7b3480b4b3f321259bd399ad84e1a7544891496a77ef7fb0f2e91400` |
| `run_variant.py` | `7b50d8a7fd3c9e24a6242022e2c01a7598f519fe2c97060ea85354327fbb80b4` |
| `cross_owner.py` | `e71b513ade5c0ff03ddc8b3ccee68005c3760ed76a4d4b26f502eabfb4c643ad` |

**Результат:** ошибок modulo arithmetic, signs/endpoints carries или joint-bit equality в этом ограниченном source review не найдено. Найдены две потери zero semantics и один разрыв связи между amount-link и ранее проверенными proof artifacts. Поэтому итог этой версии — **не «замечаний нет»**. Все три замечания переданы parent сразу после установления соответствующего пути.

### V2-B-S01 — P1: withdraw принимает amount=0

**Путь:** [main.rs:47, `relation`](src/main.rs#L47) → [money.rs:152, `verify`](src/money.rs#L152) → [run_lifecycle.py:201, `commit_transition`](../run_lifecycle.py#L201). Baseline [state.rs:252](../src/state.rs#L252) явно применяет `amount.enforce_nonzero()` для withdraw. `Public::inputs` проверяет shape и верхнюю границу amount, но не ненулевость.

**Контрпример:** корректный old note со значением x, fresh new note со значением x, `kind="withdraw"`, `amount="0"`. Cipher/fresh-F/bridge relations корректны. Carry relation с coefficients `[1,-1]`, rhs0 имеет все integer carries0. Variant2 verifier принимает этот statement, хотя baseline Groth16 relation его отвергает. Outer commit не содержит `amount>0`, создаёт запись public COEN amount0. Для Gratis [CohortBridge.prepare:45](../cohort_bridge.py#L45) передаёт delta0; [MPC update:103](../mpc_worker.py#L103) разрешает `change>=0`, так что нет последующего обязательного nonzero rejection.

Это source-proven domain regression; здесь не заявляется выполненный exploit или создание положительных денег из нуля. **Минимальное исправление:** reject zero в общей verifier-side `relation` для withdraw, до принятия proof. Проверка только в `prove` недостаточна. Parent подтвердил намерение добавить этот reject и отдельную негативную проверку; на данном snapshot статус **исправление согласовано, patch/recheck pending**.

### V2-B-S02 — P1: zero cross-owner credit инициализирует Fidelity qualification

**Путь:** [cross_owner.py:41](cross_owner.py#L41) готовит обычный `move`; [main.rs:223, `dual`](src/main.rs#L223), [money.rs:250, `dual_encrypt`](src/money.rs#L250) и `verify_handles` не требуют positive amount. [cross_owner.py:53](cross_owner.py#L53) запускает `history` с direction Out/In; [CohortBridge.compute:54](../cohort_bridge.py#L54) безусловно добавляет active row для In и задаёт `qualified=now`, если прежнее значение0. MPC принимает change0.

**Контрпример:** sender переводит0, recipient имеет old balance0 и empty cohort state. Обе monetary relations корректны; dual handles относятся к одному нулю. Receiver old/new balances равны0, `before=after=0`, все range/evaluation checks MPC удовлетворены. Сертификат содержит новый zero active cohort и `qualified=now`. Для sender Out добавляются zero sold slices. Уже обычное `floor(old/7)` в выбранном prepare даёт нулевой amount для old∈[1,6]; диапазон uint256 сам этого не исключает.

Production [Fidelity runtime.rs:70](../../../../crates/core/fidelity/src/runtime.rs#L70) явно возвращает no-op для zero In/Out; [tests.rs:58, `zero_amount_cohort_op_is_a_noop`](../../../../crates/core/fidelity/src/tests.rs#L58) проверяет, что zero acquisition не создаёт ciphertext/history и не устанавливает qualification anchor. Поэтому сохранение прежнего Fidelity rule здесь нарушено даже без денежной инфляции.

**Минимальное ограничение экспериментального cross-owner profile:** verifier должен доказывать private `amount>0` перед выдачей/принятием history certificate и commit. Альтернатива — реализовать exact zero no-op в MPC и metadata, что шире. Parent выбрал отдельное BP+Sigma доказательство `amount−aux=1`, где оба operands range-proved uint256 и `aux=amount−1` — немонетарный auxiliary ciphertext. Такое bounded integer relation действительно исключает amount0; его необходимо связать с **тем же** `Dual.sender`, transfer context и полноценной проверкой dual flow. Aux не становится source/credit, Baby bridge для него сам по себе не требуется. На данном snapshot статус **исправление согласовано, patch/recheck pending**. Same-owner move zero policy не предлагается изменять.

### V2-B-S03 — P1 на границе недоверенных artifacts: amount-link читает непроверенные Bundle

**Путь:** [run_variant.py:41, `prove_prepared`](run_variant.py#L41) корректно выполняет полный `verify` и проверяет exact statement/ciphers, но сохраняет разрешение как `verified_statements[sha(statement)]=owner`. Затем [main.rs:259, `verify-dual`](src/main.rs#L259) **заново читает** два бинарных Bundle, проверяет только их kind, два ciphertext slots и dual handles; полного `verify` этих прочитанных Bundle нет. [cross_owner.py:52](cross_owner.py#L52) сравнивает только возвращённые statements с sa/sb. Точные ранее проверенные Bundle bytes/hash на этом переходе не сравниваются.

**Конкретная подмена:** существуют два независимо корректных monetary statements: sender debit x и receiver credit y, где x≠y. Их собственные Baby/VSS/MPC relations могут быть корректны каждый относительно своего owner state. После их full verification изменить в переданных `verify-dual` Bundle только `ciphers[3]` sender и `ciphers[1]` receiver на две стороны одного корректного dual ciphertext; оставить statements прежними. Полные изменённые Bundle уже не проходят свои money/bridge proofs, но helper их не проверяет. `verify_handles` и сравнение двух slots проходят; returned statements остаются прежними и Python equality проходит. Теперь заключение о равенстве amount относится к подменённым ciphers, а monetary/cohort receipts относятся к исходным Baby amounts x/y.

**Предпосылка и предел вывода:** это acceptance gap при недоверенных или заменяемых proof artifacts между двумя проверками. Честный детерминированный runner сам такой подмены не делает. Здесь не заявляется выполненная инфляция в прошедшем parent scenario или обход OS filesystem permissions. Однако отдельный node/verifier interface не должен полагаться на то, что helper повторно прочтёт именно уже проверенный artifact, если это не проверяется. Отдельные корректные cohort updates не устанавливают x=y между владельцами.

**Минимальное исправление:** `verify-dual` канонически декодирует и полностью повторно проверяет оба Bundle с registry, trusted expected owner keys и parameters; либо возвращает hash каждого прочитанного Bundle и caller сравнивает их с exact receipts уже выполненного full verification. Сравнение только statement digest недостаточно. Для последующей независимой node boundary полезно также повторить baseline `statement.context == hash(ca/cb)` в cross-owner commit: текущий closure сравнивает ca/cb с подготовленными локальными objects, а явная связь opaque statement context с JSON context здесь отсутствует. Сообщено parent; статус на этом snapshot **patch/recheck pending**.

### Проверенные положительные свойства и точные границы

| Требование design review | Что подтверждено исходником |
|---|---|
| V2-B01: общий bit в двух группах | [bridge.rs:56](src/bridge.rs#L56) включает все paired points/first messages и local-link first messages в общий challenge; [verify:153](src/bridge.rs#L153) проверяет ровно256 bits,4 Baby limbs,16 R chunks, обе группы для одной OR branch, canonical responses и XOR challenge. Weighted links используют локальные степени внутри64/16 bits. |
| V2-B02: exact uint256 | [money.rs:20](src/money.rs#L20) ограничивает n≤8, coeff∈{−1,0,1}, rhs≤uint256. [verify:152](src/money.rs#L152) проверяет BP16 всех operands, identity padding, BP8 carries, последний U=128G и implicit начальный carry0. Нельзя скрыть top carry. Для этого API даже bound `8(B−1)+(B−1)+128+128B < 2^24` достаточен для local no-wrap; приведённый выше `<2^25` консервативен. |
| V2-B03: восстановление старых ciphertexts | [lib.rs:133, `decrypt`](src/lib.rs#L133) вычисляет C−sD и ищет каждый normalized16-bit chunk в bounded table; старые r не читаются. [equality_verify:267](src/lib.rs#L267) связывает один DK с ключом и всеми F chunks, проверяя `z_key EK=A_key+cH`, затем `z_key D−z_i H=A_i+c(C−F)`. |
| Cipher validity и canonical encodings | [lib.rs:27](src/lib.rs#L27) canonical Ristretto point/scalar APIs; `Cipher::validate` проверяет16 C/D и nonidentity EK. Baby decoding использует baseline `crypto::decode` с validated canonical deserialize и trailing-byte reject. Main full verify проверяет canonical bincode round-trip. Это последний guard отсутствует в pre-fix `verify-dual`, что входит в исправление S03. |
| V2-B05: раздельные owner DK | `cross_owner.py` вызывает sender prover только с sender DK; receiver `receive-input` получает receiver DK и public Dual, затем receiver prover только receiver DK. [money.rs:297](src/money.rs#L297) проверяет общий C и Schnorr equality randomness двух D handles. При **полной проверке именно этих Bundle** это связывает одинаковые16-bit messages; modulo ambiguity исключена ranges. Кодовой передачи обоих DK одному native prover здесь нет. Process/host isolation не измерялась этим review. |
| V2-B06: Baby/VSS history | [cross_owner.py:10](cross_owner.py#L10) экспортирует old0/new2 и amount3/1 отдельно каждому owner, сравнивает VSS polynomial constants с exact Baby `statement.notes`, выполняет acceptance holders. [CohortBridge.commit:103](../cohort_bridge.py#L103) связывает сертификат с exact monetary statement digest и exact root/version/timestamp. Value-only key recovery не восстанавливает Baby blinds; отдельный VSS witness path остаётся необходимым. |
| V2-B07: registry и owner/state | [main.rs:41](src/main.rs#L41) адресует математическую Baby→cipher связь по `(key,Baby commitments)`. Новый bridge создаёт связь, отсутствие bridge требует точного совпадения ciphertext с trusted registry. Owner identity регистрирует encryption key через подпись в [run_variant.py:27](run_variant.py#L27); актуальные owner/asset/unspent и input Baby commitments проверяет outer ledger. Registry сам не является UTXO/state ledger. |
| V2-B08: source/economics | [main.rs:79](src/main.rs#L79) для claim/mint/pledge требует retained Groth16 над exact `statement.inputs()` и trusted kind VK. Нулевые coefficients в MoneyProof здесь дают только ciphertext/range certification; источник и экономика не заменены этим proof. Baseline commit отдельно сверяет Nod owner/called/deadline/nominal commitment/fraction/price, одноразовый input и asset route. P_L2/P_link и full source lifecycle повторно не аудировались. |
| Atomic cross-owner + cohort | [cross_owner.py:59](cross_owner.py#L59) помещает оба spent updates, оба new balance notes, оба cohort root updates и operation marker в одну SQLite transaction. До неё подготовлены оба MPC certificates. [CohortBridge.commit:103](../cohort_bridge.py#L103) требует exact current timestamp/root/version, так что исключение второго commit откатывает денежные и оба cohort writes внутри DB transaction. Код stale-time/replay сценариев прочитан; самим рецензентом не исполнен. |

**Registry atomicity уточнена:** `run_variant.py:57` записывает verified binding cache до monetary/cohort commit. Поэтому утверждение «все registry writes атомарны с money» для этой версии неверно. Но это не обнаруженный inflation путь: такой cache содержит математические связи и не наделяет note spend authority; failed monetary operation может оставить корректную неиспользованную связь. Нужны trusted cache provenance и отдельный ledger lookup. Если registry позднее будет хранить current balance/key epoch/state authority, эту модель нельзя перенести без транзакционной доработки.

**Context/authority граница:** полный Rust verifier получает trusted registry, expected key и VK path от caller, а не проверяет consensus membership этих входов. Python runner создаёт owner registration и отмечает signed statement receipts. Это защищает рассматриваемую управляемую PoC композицию при доверенном controller/filesystem; не является готовым network ingress с adversarial persistence, authenticated key rotation или Byzantine committee. Проблема S03 относится именно к месту, где эта композиция перестаёт связывать заново прочитанные proof bytes с прежним receipt.

**Fixture замечание от parent:** recipient выбирался первым owner≠sender; parent обнаружил, что он пересекается с отдельным late-Fidelity genesis fixture, и сообщил о планируемом выборе owner, у которого действительно empty cohort/zero balance. Это не независимое новое finding этого рецензента и не доказательство общего recovery для arbitrary existing recipient state. Здесь проверена bounded crypto relation для arbitrary uint256 operands; конкретный fresh recipient lifecycle имеет отдельную fixture предпосылку.

После исправлений S01–S03 нужен exact-source recheck соответствующих guards и связывания Bundle, затем parent scenario/adversarial tests. Отсутствие новых benchmarks не является логической ошибкой криптографической конструкции; ресурсные conclusions относятся к отдельным parent measurements, а не к этому source-only дополнению.

<a id="source-recheck"></a>

## 11. Source recheck исправлений S01–S03

2026-09-11: повторно полностью прочитаны изменённые `src/main.rs`, `run_variant.py`, `cross_owner.py`; хеши `src/{lib,money,bridge}.rs` совпали с §10. Новые состояния исходников:

| Файл | SHA-256 после исправления |
|---|---|
| `src/main.rs` | `b215f7dd490512a0eb321b0e3bfc18bbbd1c3f83cba6cfe759a583ac436f2997` |
| `run_variant.py` | `9b3b0909f173a3055e36cf0ee2e33b1ad226eeaade802ca15e14cabe96b213cd` |
| `cross_owner.py` | `8198162f52a60f0a3077a56131e3b0a92087f28c6c94c2c0ad963f65c50369b2` |

**Актуальный ledger: все три конкретных P1 из §10 закрыты на уровне source recheck.** Исторические `patch/recheck pending` выше относятся к указанному pre-fix snapshot. Это не отметка об исполнении тестов рецензентом и не полный audit нового backend.

| Finding | Проверенное исправление | Статус и граница |
|---|---|---|
| V2-B-S01 | [main.rs:69, `relation`](src/main.rs#L69) отвергает amount0 в withdraw. Общую relation вызывает полный verifier, поэтому custom prover не обходит guard. | **Source-resolved.** Baseline positive-withdraw domain восстановлен. Прочитан unit `zero_withdraw_rejected_at_verifier_relation`; самим рецензентом не запускался. |
| V2-B-S02 | [main.rs:33, `verify_dual_amount`](src/main.rs#L33) проверяет dual handles, одинаковый owner key для sender/aux и полноценный MoneyProof над `[sender,minus_one]`, coefficients `[1,-1]`, rhs1. [receive-input:289](src/main.rs#L289) и [verify-dual:302](src/main.rs#L302) обязательно вызывают этот verifier. | **Source-resolved для positive cross-owner profile.** Теперь нулевой transfer не достигает history/commit по рассмотренному пути. Same-owner move не ограничен новым nonzero rule. |
| V2-B-S03 | [main.rs:46, `receipted_bundle`](src/main.rs#L46) проверяет canonical round-trip и SHA-256 полного Bundle против expected receipt. [run_variant.py:65](run_variant.py#L65) наполняет in-memory `verified_bundles` hash, возвращённым полным node verify, после проверки statement/ciphers и owner signature. [cross_owner.py:51](cross_owner.py#L51) передаёт именно эти два cached receipts. | **Source-resolved при trusted node receipt cache/controller.** Изменить ciphertext slots, сохранив statement, теперь недостаточно: изменится hash всего Bundle. Standalone CLI, которому недоверенный клиент сам задаёт expected receipt, по-прежнему не является самодостаточным ledger verifier; текущий caller соблюдает необходимую границу. |

**Почему positive proof достаточен.** MoneyProof transcript уже связывает точные два ciphertexts, coefficients, rhs и context; outer domain дополнительно связывает transfer context. Поэтому нельзя заменить `Dual.sender` или aux после proof. Старый проверенный carry/range verifier устанавливает `amount−aux=1` как integer equality, где оба значения∈[0,2^256−1]. При amount0 потребуется aux=−1, что исключено. Попытка aux=`2^256−1` даёт отличие ровно `2^256`; final carry0 его запрещает. Aux не регистрируется как денежный input/output и не нуждается в Baby/source authority: это только witness положительности того же ciphertext amount. Dual handles связывают этот positive sender amount с receiver amount; receipted Bundle связывают их с ранее проверенными Baby operands.

**Связывание context дополнено.** [cross_owner.py:60](cross_owner.py#L60) теперь самостоятельно вычисляет canonical JSON SHA-256 modulo circuit field для каждого ca/cb и сравнивает с opaque `statement.context`. Это закрывает отмеченную в S03 недостающую явную проверку monetary statement ↔ transfer/cohort JSON context. Она выполнена до общей SQLite transaction; прежние root/version/exact-time и replay checks сохранены.

**Дополнительные изменения прочитаны, без расширения verdict.** `prove_prepared` сохраняет `registry-before.json` и использует один snapshot для prove/verify; это повышает воспроизводимость exact old binding lookup. Cache по-прежнему записывается до monetary commit, и квалификация из §10 о его роли остаётся в силе. Recipient теперь выбирается отдельно от sender и genesis-history owner; CLI требует count≥3. Это устраняет отмеченное parent смешение fixture owners, но не является новым тестом arbitrary recipient history.

**Executed vs source-only:** повторное чтение трёх изменённых файлов и SHA-256 сверка шести исходников выполнены. Прочитаны три новых unit test bodies. Тест receipt действительно проверяет rejection изменённого ciphertext при неизменном statement и прежнем receipt; используемый fixture Bundle не доказывает сам по себе весь node lifecycle. После последнего усиления прочитан обновлённый [zero_cross_credit_cannot_pass_positive_proof:391](src/main.rs#L391): кроме `money::prove(...).is_err()`, он сначала успешно вызывает `verify_dual_amount` для amount1, затем заменяет sender/receiver ciphertexts и handles на корректный dual amount0, сохраняя прежний positive proof, и требует rejection **самого verifier**. Таким образом, прежнее ограничение «только prover-negative» устранено; negative проверяет привязку positive proof к точному ciphertext amount. Хеш main в таблице обновлён с `f63a067ac629e141c7364ab32b83c21a06d77ba4e422e30d492de97139735f94` после этого изменения `cfg(test)`; остальные пять source hashes совпали. По сообщению parent, runtime код не менялся, все6 Rust tests и release build прошли; рецензент эти запуски и логи независимо не проверял. Final lifecycle и ресурсные measurements остаются evidence parent. Завершённый pre-fix run не используется как подтверждение исправлений.

В исправленном ограниченном scope не осталось установленных этим рецензентом P0/P1. Сохраняются явно описанные предположения Pedersen binding/DLog, Bulletproofs/Fiat–Shamir, trusted receipt cache и passive/honest-majority history committee, а также граница recipient acceptance и отдельная VSS recovery Baby witnesses. Этот вывод не распространяется на production ingress, malicious distributed proving, key resharing или полный protocol completion.
