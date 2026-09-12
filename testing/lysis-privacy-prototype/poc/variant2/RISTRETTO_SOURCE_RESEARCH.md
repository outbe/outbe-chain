# Полностью Ristretto privacy state: source P_link и точная арифметика

Дата: 2026-09-12. Независимый bounded research по исправленной постановке: source nominal, Nod, VSS/history и Gratis используют Ristretto commitments; Baby↔Ristretto bridge не считается выполнением задачи. Baseline и hybrid не изменялись. Изменён только этот документ. Research skill применён в уже выделенном background researcher, без дополнительных агентов. Graph tools отсутствуют: metadata показывает лишь unrelated Open Design `list_projects`; generation/coverage UNKNOWN, использован exact-source fallback.

**Рекомендация:** сохранить проверенные BN254 Poseidon2 и integer source relations, доказав opening **непосредственно Ristretto C** через небольшой nonnative Edwards25519 gadget. Если один circuit превышает бюджет, разбить source/opening на последовательные Groth16 proofs со скрытыми состояниями и строго связанными witness digests. Это конкретный путь с доступными primitive libraries; его memory/time требуется измерить. Замена всех privacy commitments не означает замену BN254 внутри обязательного существующего UltraHonk или pairing machinery Groth16.

Во время research parent сообщил, что первая монолитная synthesis для104+253 scalar bits остановлена limiter при≈641MB. Это **parent-reported прерванная synthesis**, не измеренный полный prover и не доказательство невозможности bounded slices. Этот рецензент никаких synthesis/build/prover/benchmark не запускал.

## 1. Какая source relation должна сохраниться

Непосредственно прочитаны [link.rs](../src/link.rs), [crypto.rs](../src/crypto.rs), [state.rs](../src/state.rs), manifests, existing variant2 design docs и pinned FullProof [main.nr](https://github.com/outbe/outbe-circuits/blob/984d57ed0d2f014a1a74d0b3b4b0769801957791/crates/outbe-zk-canonical/noir/outbe-full-circuit/src/main.nr). Checkout HEAD прочитан: `984d57ed0d2f014a1a74d0b3b4b0769801957791`; его локальный путь — `/Users/sakor/.cargo/git/checkouts/outbe-circuits-56de82d8dac53513/984d57e`.

FullProof проверяет Grumpkin ownership/Schnorr и Merkle inclusion и имеет ровно public `[owner,nft_hash,binding_hash,expected_merkle_root]`. P_link должен использовать **те же четыре canonical BN254 field values**. Полный preimage draft и nominal formula остаются задачей P_link; valid FullProof их автоматически не устанавливает. Root/owner registry authority остаётся отдельной admission проверкой. Подпись источника или локальный commitment вместо canonical hash proof не удовлетворяют этому контракту. Подробнее об уже проверенном source helper — [L2_SOURCE_FEASIBILITY.md](../L2_SOURCE_FEASIBILITY.md).

Точные relations в [link.rs:130](../src/link.rs#L130), [binding_hash:144](../src/link.rs#L144), [nominal:155](../src/link.rs#L155), [constraints:249](../src/link.rs#L249):

```text
u6 = base_u64 * 10^6 + atto,             0 <= atto < 10^6, u6 > 0
nft_hash = fold Poseidon2_hash2, starting at private draft_id,
           over owner,day,currency,base,atto,count,sorted_unique_active_SU_ids
binding_hash = Poseidon2_hash5(1,sender160,id_low128,id_high128,chain_id64)
effective = max(reference_vwap, reference_scurve)
N = u6 * 10^6 * reference_vwap
D = issuance_vwap * effective
a6 = floor(N / D),                       issuance_vwap,reference_vwap,a6 > 0
N = a6*D + remainder,                    0 <= remainder < D
C = a6*G_R + r*H_R,                      0 <= r < l_R
```

`u6<2^84`, а `D>=reference_vwap` при целочисленном положительном issuance_vwap. Поэтому `a6<=u6*10^6<2^104`. Это позволяет одному scalar commitment представлять **source nominal** точно. Circuit уже использует104-bit quotient и640-bit intermediate comparison; перенос на Ristretto не должен заменять их field division. Неактивные SU slots должны оставаться нулевыми, активные — строго возрастающими и ровно count штук. Capacity32 — профиль измерения, не новый economics cap.

Canonical BN254 source fields нельзя редуцировать в Ristretto scalar при смене wire codec. Это разные moduli: часть допустимых source IDs/hashes больше `l_R`. Их 32-byte canonical BN254 encoding либо фиксированные limbs сохраняются в source interface.

## 2. Прямой Ristretto opening в BN254 Groth16

Используются три разных математических domain:

| Domain | Значение / роль |
|---|---|
| Source/circuit field | BN254 Fr, прежние Poseidon2 constants и source hash |
| Edwards coordinate field | `p=2^255−19`; nonnative внутри BN254 circuit |
| Ristretto scalar field | `l_R=2^252+27742317777372353535851937790883648493`; **253-bit** canonical blinder, не прежние251 Baby bits |

[RFC9496 §§4.3–4.4](https://www.rfc-editor.org/rfc/rfc9496.html#section-4.3.1) задаёт canonical Ristretto decode, quotient equality и scalar encoding. В pinned Dalek4.1.3 это непосредственно реализовано в [`CompressedRistretto::decompress`](https://github.com/dalek-cryptography/curve25519-dalek/blob/5312a0311ec40df95be953eacfa8a11b9a34bc54/curve25519-dalek/src/ristretto.rs#L255) и [`RistrettoPoint::ct_eq`](https://github.com/dalek-cryptography/curve25519-dalek/blob/5312a0311ec40df95be953eacfa8a11b9a34bc54/curve25519-dalek/src/ristretto.rs#L835). Внутренняя EdwardsPoint обёртки имеет `pub(crate)` visibility; публичного доступа к её x/y через это поле нет.

Минимальный interface:

1. Public input содержит canonical compressed C. Verifier, а не недоверенный prover, декодирует его по RFC и **сам формирует** координатные limbs для Groth16 public inputs. Host decoder можно реализовать над `ark_ff` field p25519 и сверить с Dalek/RFC vectors; нельзя просто интерпретировать Ristretto bytes как Ed25519 compressed-y.
2. G/H декодируются из тех же fixed compressed generators, что реально используются money/VSS. Pinned [`PedersenGens::default`](https://github.com/dalek-cryptography/bulletproofs/blob/86eadbeeb4a96d8da41427137b45ead810d03b41/src/generators.rs#L45) задаёт Ristretto basepoint и SHA3-512 hash-to-Ristretto для blinding base. Произвольный новый H сломает linkage даже при корректных proofs.
3. Circuit вычисляет `a6*G+r*H` с exact quotient bits и canonical r<l_R. Fixed-base windows, например4-bit, выбирают ровно одну из16 предварительно проверенных констант. Lookup/bit order и покрытие104+253 bits фиксируются circuit profile.
4. С public decoded representative проверяется Ristretto equality:
   `x_actual*y_C == y_actual*x_C OR y_actual*y_C == x_actual*x_C`.
   Простое равенство обоих raw coordinates не эквивалентно Ristretto equality. Prime-subgroup lifting не требуется, если операции сохраняют допустимые representatives и применяется именно quotient relation.

[`ark-r1cs-std0.5 EmulatedFpVar`](https://github.com/arkworks-rs/r1cs-std/blob/dc48c66e27a9d6d0f4356c0bf27b54cbd5459853/src/fields/emulated_fp/mod.rs) уже доступен локально, поддерживает foreign field operations и reductions. Однако готовый [`twisted_edwards::AffineVar`](https://github.com/arkworks-rs/r1cs-std/blob/dc48c66e27a9d6d0f4356c0bf27b54cbd5459853/src/groups/curves/twisted_edwards/mod.rs) параметризован constraint field как `BasePrimeField<P>`: подставить `EmulatedFpVar<p25519,BN254Fr>` вместо native field простым alias нельзя. Нужен небольшой собственный curve gadget; новый полный EC crate не является необходимой предпосылкой.

Для него пригодны полные Edwards addition formulas; первичный источник формул и их coordinate assumptions — [EFD, twisted Edwards a=−1](https://www.hyperelliptic.org/EFD/g1p/auto-twisted-extended-1.html). Не смешивать strongly-unified и complete guarantees произвольной formula. Affine inverse должен быть constrained; если допускаются внешние/private промежуточные точки, нужны curve validity и ненулевые denominators либо явный inductive invariant. Для projective варианта дополнительно обязательны `Z!=0` и корректный `T`; zero tuple не должен делать quotient equality тривиально истинным.

## 3. Последовательные Groth16 slices: условно sound fallback

Это самостоятельный composition argument, **не утверждение о готовом audited protocol или автоматической recursive aggregation**. Parent предложил применять salted Poseidon witness/state digests, сохраняя все external amount commitments в Ristretto. Внутренние hash digests добавляют assumption о binding/hiding hash commitment; их нельзя называть perfectly hiding Pedersen commitments.

Пусть `ctx` связывает protocol version, source четыре public поля, source terms/IDs, target compressed C и circuit/schedule profile. Source proof устанавливает прежний draft/nominal relation и:

```text
Dw = H(domain_w, ctx, a104, canonical_r253, private_salt_w)
Di = H(domain_state, ctx, Dw, i, canonical_point_limbs(Qi), private_salt_i)
```

Каждый opening slice доказывает знание того же `(a,r,salt_w)` для Dw, openings входного/выходного Di и точное обновление точки назначенными битами `aG+rH`. Первый slice принуждает Q0 быть identity; последний связывает Qfinal с canonical C через RFC equality. Context, indices и точный bit schedule публичны и проверяются независимо от prover-selected data. Все limbs x/y — canonical integers `<p`, например4×64 с верхним limb≤63bits; **`x mod BN254Fr` не является injective encoding координаты**.

Node обязан проверить весь согласованный proof set: trusted VK для source и каждой роли slice, exact step count/order, отсутствие пропусков/повторов, один ctx/Dw, равенства adjacent Di, правильные endpoints и итоговую source authority. Нельзя выдавать admitted Nominal/Nod после одного source proof или отдельно успешного slice. Это не требует owner после final admission: вся последовательность завершается до неё.

**Soundness argument:** извлекаемые witnesses корректных SNARK relations открывают одинаковые digests. Если соседние witnesses отличаются в номинале/blinder/hidden point при одном digest, получена hash collision. При binding общие witnesses совпадают; индукция от constrained identity через все fixed slices даёт точное final `aG+rH`. Затем source proof связывает a с исходным canonical draft. Нужны выбранные SNARK knowledge-soundness/CRS assumptions и collision resistance конкретного hash; полный composable security theorem этой системы здесь не доказан.

**Privacy:** salts должны быть private, fresh и иметь достаточную entropy; хешировать только low-entropy amount недостаточно. Нельзя публиковать raw scalar-multiplication intermediate points: короткие участки scalar могут восстанавливаться bounded discrete log. Hidden point digests с salts и ZK slice proofs устраняют этот непосредственный leak при hash-hiding assumption. Public proof count/schedule/capacity остаются видимыми. Утверждение о ROM-style hiding salted Poseidon — отдельная security assumption, не следствие одной collision resistance.

**Memory:** отдельные processes/последовательное освобождение PK ограничивают одновременно живой circuit. Суммарная latency, число proofs и disk PK растут. Успех smaller slice не устанавливает общий cold peak с source proof, PK load, serialization и wrapper; требуется полный измеренный wallet path32SU. Не следует суммировать peaks последовательных процессов как одновременную RAM, но надо учитывать реально перекрывающиеся процессы/буферы. Число slices выбирается по результату prototype, не из одной асимптотики.

## 4. Сравнение альтернатив

| Путь | Что сохраняет / переносит | Конкретная доступность и вывод |
|---|---|---|
| Groth16 BN254 + nonnative Ristretto opening | Native source hash/integer gadgets; foreign только EC opening | Existing ark0.5 primitives доступны. Самый малый перенос source relation; monolithic RAM не предполагать. |
| Те же relations, последовательные salted-hash slices | Те же external Ristretto commitments; несколько согласованных proofs | Конкретный bounded fallback §3. Parent уже прототипирует; время/RAM полного результата pending. |
| Bulletproofs R1CS over Ristretto | Direct external Ristretto commitments, без EC gadget; **BN254 Poseidon становится foreign-field computation** | Pinned5.0 содержит `r1cs::Prover::commit/Verifier::commit`, `yoloproofs` feature и исходники. Требует нового foreign BN254 arithmetic/hash frontend, строгих ranges/reductions и32-SU constraints. Не самый малый source migration. |
| Ristretto Spartan | R1CS native field=l_R; native committed witness machinery | [Официальный Microsoft Spartan](https://github.com/microsoft/Spartan) использует Ristretto и сообщает отсутствие audit. BN254 hash всё равно foreign. В локальном cache Spartan не найден; готовая привязка именно существующего external scalar Pedersen C в его public API здесь не установлена. Не обещается drop-in CP-SNARK. |
| LegoGroth16 / pairing CP-SNARK | Прямая работа с commitment в pairing G1 со scalar field выбранной pairing curve | [Опубликованный typed API](https://docs.rs/legogroth16/0.18.0/legogroth16/prover/fn.verify_commitments.html) требует `E: Pairing`, witnesses∈`E::ScalarField`. Это не Ristretto CP backend. Перенос G1 commitment в Ristretto без дополнительного opening/link proof не следует из LegoSNARK. No-Baby требование не превращает pairing G1 в Ristretto. |

Первичный [Bulletproofs paper](https://web.stanford.edu/~buenz/pubs/bulletproofs.pdf) допускает committed inputs для arithmetic circuits и имеет linear prover/verification cost относительно circuit size. Это не измерение нового BN254-foreign circuit и не гарантия512MB. Source API локально прочитан: `bulletproofs-5.0.0/src/r1cs/prover.rs:279–305` создаёт external scalar commitment; `verifier.rs:231–252` принимает exact compressed commitment и включает его в transcript. Все committed inputs должны быть внесены до challenge-dependent constraints.

У `yoloproofs` в pinned README прямо experimental/deployment caveat. Строка README о невозможности published feature не согласуется с фактическими `Cargo.toml:153` (`yoloproofs=[]`) и `src/lib.rs:47–49`; доступность исходников/feature проверена непосредственно, успешная сборка с feature здесь не проверялась. Поэтому нельзя ни объявлять R1CS недоступным, ни переносить confidence range-proof API на произвольный новый R1CS frontend.

Также прочитан abstract первичного [Segev2025/327](https://eprint.iacr.org/2025/327) о completeness/soundness gap R1CS construction из Bünz thesis и расширении relation. Применимость этого результата к конкретной pinned Dalek реализации здесь **не установлена**; это не заявленный bug этой версии. Для production theorem необходимо сопоставить precise relation и implementation, а не сослаться только на название Bulletproofs. Lego paper PDF fetch был ограничен robots; вывод о библиотечной границе опирается на typed primary API, а не на якобы прочитанную security proof целиком.

## 5. Уточнение money plan: public coefficients вместо general R1CS

Parent уточнил, что retained claim/mint/pledge formulas линейны в скрытых amounts при **public** coefficients f,p. Это позволяет расширить BP+Sigma integer conservation, не вводя BP R1CS только ради этих relations. Вывод ниже — проверка предложенной конструкции, не review ещё не написанного verifier.

При B=2^16 каждый secret operand имеет16 range-proved digits, каждый signed public coefficient — максимум32 digits (|c|<2^512). Для i=0…47:

```text
Ei = sum_k sign(c_k) * sum_(j+h=i) c[k,j] * a[k,h]
     - rhs[i] + t[i] - B*t[i+1] = 0
t[0]=t[48]=0
t[i] = u[i] - 2^63,             BP64: 0 <= u[i] < 2^64
```

Не более8 operands и16 products на operand/column дают `|raw_i|<8*16*2^32=2^39`. С carry и digit rhs имеем консервативное `|Ei|<2^80`, тем более `<2^81<<l_R`. Значит локальная zero-message Schnorr relation вместе с ranges не может скрыть ненулевой Ei посредством modulo-l_R wrap. Сумма `Ei*B^i` telescopes в **integer** equality.48 columns покрывают256×512-bit convolution, включая выходной carry; большие отдельные положительные/отрицательные totals могут взаимно сократиться без потери integer soundness. Verifier должен проверять все columns и endpoint0, а не усечённый scalar total.

Coefficients/rhs должны быть **выведены verifier из bound operation context**, а не приняты как произвольная подсказка prover. RHS должен иметь canonical16-bit digits и помещаться в48 columns; старшие биты нельзя молча усекать. Lengths, signs, widths, offset и roles входят в transcript. Недостаточно range-check partial products только при построении witness. Ни в этой оценке, ни в применении линейности не предполагаются secret coefficients: скрытые price/fractions потребовали бы другой relation.

| Relation | Необходимые правила |
|---|---|
| Claim | `newG=oldG+a*f*10^6`, `payold=paynew+escrow`, `escrow=a*f*p`; public f,p>0; same admitted source a>0. Все notes uint256. При неотрицательных operands bound g и c также ограничивает af; прежние checked af/g/c domains не заменяются modular arithmetic. Nod owner/called/deadline/terms/once-only checks остаются обязательны. |
| Mint | `new=old+burn*10^12`; burn>0, например через exact `burn−aux=1` с uint256 ranges. Aux не становится денежным source/credit. Burn source authority/consumption отдельно проверяется ledger. |
| Pledge | `old=new+amount`, `ticket=amount`, amount>0; ticket/old amount и ownership должны относиться к тем же commitments. |
| Move/withdraw | Сохраняются прежние integer conservation и verifier-side positive withdraw; positive cross-owner transfer gate остаётся отдельным profile rule. |

**Source104 linkage:** single Ristretto equality `C_source−Σ2^(16i)F_i=δH` sound как integer linkage лишь при verifier-enforced a104. Из16 chunks: chunk6 имеет8 bits, chunks7…15 имеют **zero message** (проверяемое opening либо fixed identity F), chunks0…5 имеют16 bits. Только BP16 всех chunks и weighted equality допускают alias `a+k*l_R` в uint256 domain. P_link a>0 тогда действительно переносится на arithmetic operand; honest-prover width check недостаточен.

**VSS/history note linkage:** четыре Ristretto64-bit limb commitments можно связать с16 chunk F четырьмя локальными Schnorr equations:
`C_l−Σ_(j=0..3)2^(16j)F_(4l+j)=δ_l H`.
F ranges определяют значения limb<2^64. Это same-group linkage; joint-bit crosscurve proof не нужен. Нельзя заменить четыре equations одним weighted256-bit equality modulo-l_R. VSS field/modulus, canonical scalar encoding и recovery checks должны мигрировать на l_R вместе с commitments; наличие новых money proofs само эту миграцию не выполняет.

## 6. Минимальная проверка следующего прототипа

Для direct opening сравнить canonical C с Dalek для a∈{1,2^104−1}, r∈{0,1,l_R−1}; отвергнуть r=l_R и malformed/noncanonical C. Проверить identity/intermediate exceptional cases и наличие RFC quotient-equivalent representatives. Host-derived public coords необходимо протестировать против подмены независимо от compressed C.

Для slices нужны отрицательные случаи: другой nominal/blinder/salt/context, заменённый intermediate digest, wrong VK/role, skipped/reordered/duplicated chunk, неправильный start/final point, scalar bit truncation, aliased coordinate encoding. Для source сохранить реальные FullProof четыре shared fields, canonical32-SU hash и integer remainder checks. Для money отдельно проверить a+l_R alias, nonzero/zero boundaries, high carry, отрицательный и максимальный coefficient, несоответствие F/VSS limbs и подмену approved source. Это список необходимых meaningful checks, **не запись о выполненных tests**.

Полный cold wallet32SU measurement должен включать source/P_L2/P_link, sequential PK loads, reconstruction/witness, proof/self-verify/serialize и фактически удерживаемые buffers. После успешной synthesis нельзя объявлять полный путь512MB PASS. Baseline [RESULTS.md](../RESULTS.md) содержит100913 constraints и historical cold≈388–395MB для Baby profile32; это полезная исходная точка, не RSS новой конструкции и не основание линейной экстраполяции через FFT thresholds.

## 7. Pins и граница evidence

Первичные library copies прочитаны в `/Users/sakor/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`. Pins получены непосредственно из `.cargo_vcs_info.json`:

| Package | Version | VCS SHA |
|---|---|---|
| ark-r1cs-std |0.5.0|`dc48c66e27a9d6d0f4356c0bf27b54cbd5459853`|
| curve25519-dalek |4.1.3|`5312a0311ec40df95be953eacfa8a11b9a34bc54`|
| bulletproofs |5.0.0|`86eadbeeb4a96d8da41427137b45ead810d03b41`|

Ark0.5 field/Groth16 primitives и Dalek/BP source доступны локально. `ark-ed25519`, `ark-curve25519`, Spartan и Lego package в просмотренном registry cache не найдены; это ограниченное local availability observation, не утверждение об отсутствии библиотек вообще. Для собственного field/gadget достаточно existing ark-ff/r1cs primitives; установка нового EC crate не объявляется блокером.

Snapshot baseline source при чтении:

| Файл относительно poc | SHA-256 |
|---|---|
| `src/link.rs` |`28468e397b266c8f095c05b411c00c65a2fd1daeea1067a70a49e75db572ef83`|
| `src/crypto.rs` |`ef4bdf94c3cfd4250a7482d1d6facde9df04fa17495e4fd8a9377172a98cc4c3`|
| `src/state.rs` |`524b8cdee1d0a83cf65a56aba76a206a16187d3bffc80c536dade5b0cba74191`|

**Выполнено:** direct source reads, library feature/API/field-bound checks, source hashes/pins, RFC/Dalek/EFD сравнение, чтение первичных Bulletproofs и supplier docs, самостоятельные algebraic bounds/composition arguments. **Не выполнено:** новое code implementation, builds, adversarial execution, full prover/RAM/mobile measurement, malicious-MPC/privacy theorem, production deployment/audit. Parent одновременно прототипирует; его результаты должны иметь отдельные artifacts и не приписываются этому research.

<a id="executed-source-review"></a>
## 8. Executed source review: первая реализация Ristretto

Статус последнего повторного чтения — **§8.8**: RSR-B02 закрыт в source для проверенного P_link→admission маршрута, wide использует BP32; отдельно закрыт оставленный P_L2 snapshot concern и проверен proving в13 свежих процессах. Предыдущие subsections сохраняют ход проверки и hashes более ранних snapshots; их BP64 и pending формулировки не обозначают текущую реализацию.

Независимо прочитаны шесть запрошенных implementation files целиком: `edwards_gadget.rs`, `source_opening.rs`, `link.rs`, `note_link.rs`, `wide.rs`, `bin/ristretto-money.rs`. Для материальных зависимостей дополнительно прочитаны relevant `lib.rs`, `money.rs`, `crypto.rs`, `state.rs`, `wire.rs`, существующий Poseidon2 sponge и canonical bit conversion из pinned ark-r1cs-std. Это **выполненное чтение исходников**, не исполнение circuit/proof и не аудит всех потребителей. Snapshot сделан во время интеграции; новый код после приведённых SHA требует повторной проверки. Graph project/generation/coverage остаются UNKNOWN.

**Вердикт по этому snapshot:** конкретного способа принять ложное opening или ложную integer relation в просмотренной композиции не найдено. Алгебра ниже согласуется с реализацией при перечисленных library/security assumptions. Обнаружен и source-only перепроверен исправленный BP64 integration defect. Первоначально source/node orchestration ещё интегрировалась; дополнительное чтение нового main.rs в §8.5 подтверждает full source+opening verifier для неизменного input snapshot. Authority источника и атомарный commit cohort/money этим чтением не подтверждены. Это не заключение «весь протокол завершён».

### 8.1. Source witness и последовательность opening

[link.rs:330](ristretto/src/link.rs#L330) сохраняет nonzero guards и integer division relation; `a` имеет104 bits и строго положителен. [link.rs:358](ristretto/src/link.rs#L358) связывает именно quotient bits этой relation с `opening_binding`. Source proof сам по себе больше не доказывает curve opening. [source_opening.rs:28](ristretto/src/source_opening.rs#L28) хеширует `domain, context, a[2×64], r[4×64], salt`; canonical scalar ограничен `r<l_R`, а последние limbs имеют40 и61 meaningful bits соответственно. Native и constrained packing совпадают, без редукции nominal в другой scalar field.

В [schedule:37](ristretto/src/source_opening.rs#L37) четыре шага покрывают bits nominal `[0,32),[32,64),[64,96),[96,104)`, восемь следующих — blinder `[0,32),…,[224,253)`. Пропущенных или повторно учтённых bit ranges в schedule нет. Каждый [Step:64](ristretto/src/source_opening.rs#L64) заново доказывает **полный** тот же `a,r,salt` через общий digest, затем применяет лишь свою slice. State digest включает domain, context, witness digest, фиксированный step index, canonical coordinate limbs и ту же соль. Начальное состояние в step0 — circuit constant identity. Поэтому collision resistance digest связывает соседние proofs с одним exact Edwards representative, а не только с эквивалентным Ristretto encoding.

Обязательный внешний verifier contract — принять source proof и **ровно все12** opening proofs, под доверенными VK именно соответствующих индексов/circuit profiles, с одинаковыми `context/opening_binding` и одним массивом13 соседних digests. Индекс — circuit constant, не произвольный public parameter; «12 proofs под одним VK» не реализует этот контракт. Нельзя доверять присланным wallet coordinates: последние8 public limbs должны выводиться узлом из принятого compressed `C`. [step_inputs:51](ristretto/src/source_opening.rs#L51) предоставляет такую derivation, но helper сам не является node acceptance path. При просмотренном snapshot relevant поиск показывал circuit/helper, а не завершённого consumer всех12 proofs. Это известный integration gate, не найденный bypass уже готового gate.

### 8.2. Canonical coordinates, quotient equality и скрытая соль

[Point::decode:25](ristretto/src/edwards_gadget.rs#L25) сначала вызывает Dalek canonical `CompressedRistretto::decompress`, затем вычисляет affine representative по RFC formula. В частности, первоначальное `from_le_bytes_mod_order` не принимает неканонический encoding незаметно: Dalek validation выполняется раньше. Условия и quotient equality сверены с [RFC9496 §4.3.1–4.3.3](https://www.rfc-editor.org/rfc/rfc9496.html#section-4.3.1). [PointVar::enforce_same:67](ristretto/src/edwards_gadget.rs#L67) реализует именно `x1*y2=y1*x2 OR y1*y2=x1*x2`, а не обычное равенство двух представителей.

[PointVar::add:48](ristretto/src/edwards_gadget.rs#L48) использует complete affine twisted-Edwards relation для `a=-1`, `d=-121665/121666`. Нет отдельного inverse witness или guard знаменателя: корректность опирается на nonzero denominators для валидных curve points. [Step:75](ristretto/src/source_opening.rs#L75) явно проверяет equation входного point; цепь начинается identity, fixed bases проходят RFC decode, lookup table состоит из их действительных multiples. Это обеспечивает нужную provenance и для quotient equality. Одной curve equation произвольного isolated state недостаточно, чтобы объявить его допустимым Ristretto internal representative; вывод здесь использует полную цепь от identity. Формулы и условия completeness — [EFD twisted Edwards](https://www.hyperelliptic.org/EFD/g1p/auto-twisted-extended-1.html).

`point_vars/hash_state` используют `EmulatedFpVar::to_bits_le`, не unchecked host limbs. В pinned ark-r1cs-std `allocated_field_var.rs:644` эта функция выполняет reduction и проверяет bits≤p−1. Четыре64-bit limbs каждого255-bit coordinate поэтому имеют единственную допустимую integer запись. Отдельно public compressed C делится на два128-bit limbs; оба меньше BN254 Fr. Не обнаружен способ подменить p-representative или модульный alias через public limb packing.

Соль создаётся `Fr::rand(OsRng)` в [link.rs:204](ristretto/src/link.rs#L204). [Offer](ristretto/src/wire.rs#L7) содержит `opening_binding`, но не соль; [Wallet:83](ristretto/src/wire.rs#L83) содержит соль как private material. Step circuit выделяет её witness, не public input. Один свежий secret salt, общий для всей цепи данного LinkCircuit, не создаёт видимого линейного уравнения раскрытия: digests являются outputs Poseidon2 sponge с разными domains/indexes и известным context. Sponge включает input length в initialization ([hash:39](../../measurements/p-link/src/poseidon2.rs#L39)); длины witness/state messages не неоднозначны.

Однако корректная формулировка privacy здесь **условная**: требуется preimage/hash-hiding assumption для этого salted construction, дополнительно к Groth16 zero knowledge и collision resistance. Начальный digest identity даёт проверку догадки о соли; при свежей равномерной Fr salt это не малый словарь, но не превращает конструкцию в perfectly hiding commitment. Collision resistance сама по себе не доказывает скрытие. Из code shape нельзя вывести невозможность любого cryptanalytic leakage Poseidon или безопасность повторного глобального wallet salt во всех будущих protocols. Проверено создание свежего salt на экземпляр fixture/LinkCircuit и отсутствие его в Offer; логирование/private-file transport и future public artifact writer не охвачены этим выводом. Recovery должен сохранять соль вместе с witness, иначе повторная сборка этой chain невозможна.

### 8.3. Same-group note/source links и wide arithmetic

[note_link::targets:15](ristretto/src/note_link.rs#L15) для каждого64-bit note limb проверяет Schnorr knowledge of H-opening разности `C_i − Σ(j=0..3) 2^(16j)F_(4i+j)`. Это Ristretto→Ristretto relation. Ровно четыре openings и16 F обязательны. [money::verify:152](ristretto/src/money.rs#L152) сначала проверяет общий BP16 на те же F, ciphertext equalities и нулевой padding; [Bundle::verify:103](ristretto/src/bin/ristretto-money.rs#L103) затем передаёт их в note links. Поэтому integer каждого note limb меньше2^64, и modulo-l_R equality здесь означает integer equality при Pedersen binding/DLog assumption. Сам `note_link::verify` без outer ranges не является самостоятельным proof uint256.

Для source [verify_source:38](ristretto/src/note_link.rs#L38) добавляет BP8 на F6, zero-H openings F7…F15 и opening `sourceC − Σ(i=0..6)2^(16i)F_i`. Вместе с общим BP16 это ограничивает amount104 bits. `Bundle::verify` использует один и тот же extra ciphertext/F block и для source link, и для claim economics. Поэтому не обнаружен путь использовать разные source amounts в этих subproofs. Это binding к **переданному C**; genuine P_L2, full P_link, admission/nullifier и claim eligibility должны уже связывать тот же C с авторизованным source во внешнем node state.

[wide::validate:14](ristretto/src/wide.rs#L14) принимает1…8 operands, signed public coefficients с magnitude≤512 bits, rhs≤256 bits.48 base2^16 columns достаточно для256×512-bit product, включая его верхний carry. Coefficients раскладываются в32 signed digits одинакового sign; они не берутся из proof как свободная witness relation. Carry commitment `U_k` имеет BP64, обозначая `t_k=U_k−2^63` по message; `t_-1=0`, последний `t_47=0` закреплён verifier exact point `2^63 G`. Padding carry slots48…63 обязаны быть identity. [verify:44](ristretto/src/wide.rs#L44) проверяет все48 local zero-H openings.

Для любого принятого local column raw convolution содержит максимум `8×16` произведений digits<2^16: absolute contribution<2^39. С учётом rhs digit и двух bounded signed carries:

```text
E_k = column_k − rhs_k + t_(k−1) − 2^16*t_k
|E_k| < 2^39 + 2^16 + 2^63 + 2^79 < 2^80 < l_R.
```

Поэтому уравнение modulo l_R не может скрыть ненулевое integer E_k. Суммирование всех48 relations с весами2^(16k) телескопически даёт exact public-coefficient equality, без сведения всей768-bit суммы к одному scalar. Согласованные реальные convolution carries существенно меньше2^63; BP64 профиль имеет запас и не требует раскрытия carry. Это algebra/source analysis, не исполненный boundary test.

В [economics:82](ristretto/src/bin/ristretto-money.rs#L82) verifier сам выводит relations из `Public`: claim использует `f*10^6` и `f*p`, mint — `10^12`, pledge — public amount. `f,p` проверяются nonzero, Public ограничивает их uint256; `f*p` поэтому помещается в512 bits, `f*10^6` — в276 bits. Relation transcript включает statement, ciphertexts, MoneyProof, relation index и сами coefficients/rhs. Подмена proof coefficients не изменяет verifier-derived relation. Authority значений public `fraction/price/context` должна обеспечиваться node ledger, а не самим доказательством арифметики.

Claim source и mint burn имеют verifier-checked auxiliary proof `v − (v−1) = 1` с обоими uint256 ranges, а не только prover-side zero guard ([verify:111](ristretto/src/bin/ristretto-money.rs#L111)). Withdraw и pledge требуют положительный public amount. Claim equations `newG=oldG+a*f*10^6`, `oldPay=newPay+escrow`, `escrow=a*f*p` с bounded unsigned notes и положительными f,p не допускают overflow итогов; они также ограничивают положительный intermediate a*f величиной менее2^256. Same-owner move сохраняет прежний разрешённый zero policy. Cross-owner positivity отдельно вызывается в `verify_dual_amount`; полный cross-owner/cohort atomic consumer в этом новом backend повторно не аудирован.

### 8.4. Findings ledger и оставшаяся проверка

| ID | Статус / существенность | Exact evidence и вывод |
|---|---|---|
| RSR-B01 | **RESOLVED, source-only**; execution blocker, не false acceptance | Первоначально `wide.rs` запрашивал BP64, а оба wrappers `lib.rs` разрешали лишь8/16. Это отвергало все wide claim/mint/pledge. Сообщено parent немедленно. При повторном чтении [lib.rs:203](ristretto/src/lib.rs#L203), [lib.rs:222](ristretto/src/lib.rs#L222) оба разрешают8/16/64; generators capacity64. Исполнение исправленного пути не проводилось этим рецензентом. |
| RSR-G01 | **SOURCE-CONFIRMED в §8.7** для source/opening gate; authority отдельно | Обязательный source+12 per-step acceptance, exact adjacent digests и final verifier-derived C coordinates подтверждены в новом main.rs. Genuine FullProof/source authority и trust к VK directory не создаются этим gate. Первоначальная pending оценка superseded дополнительным чтением §§8.5,8.7. |
| RSR-G02 | **CONDITIONAL security assumption**, не найденный exploit | Shared secret salt виден только private witness/Wallet. Salted Poseidon hiding и Groth16 ZK нужны отдельно от hash binding; обще-протокольная privacy из этого чтения не следует. |
| RSR-G03 | **PENDING integration evidence**, внешний контракт | Registered owner key, trusted established note→cipher registry, public price/fraction/source authority, state version/nullifiers и атомарный cohort commit должны поступать из проверенного ledger. CLI proof verifier принимает эти inputs, сам их authority не создаёт. |

`Bundle::verify` сравнивает key каждого ciphertext с supplied expected key; fresh notes требуют proof либо exact существующую registry запись под `(key,note commitments)`. Это корректная локальная граница, если expected key и registry действительно принадлежат trusted node. `receipted_bundle` сверяет canonical full serialization и hash с supplied receipt; receipt не должен предоставляться атакующим как собственное утверждение о прохождении full verify. Предыдущий hybrid audit не считается исполненным acceptance test нового node/cohort implementation.

Следующие проверки имеют смысл после интеграции: valid source с одной удалённой/дублированной/переставленной step proof; разные salt/a/r в соседних proofs при общей D; изменённые Di и compressed C при старом final proof; boundary a=2^104−1 и a=2^104; r=l_R; noncanonical compressed point/coordinate; coeff512 boundary и отрицательный carry; верхний ненулевой terminal carry; zero claim/mint/withdraw/pledge; подмена sourceC, owner key, registry entry, публичных f/p после proof; сохранение exact full receipt/atomic context. Список задаёт adversarial evidence, а не заявляет, что эти тесты уже выполнены.

Source SHA-256 при завершении чтения; paths относительно `variant2/ristretto/src`:

```text
edwards_gadget.rs       b0b67d0727bd0993892f1a2b9b2a330c4d280814cfb438507033a4c6b03525cc
source_opening.rs       9ae0173d28a5156aa43411c7085a15c7199b32107364a40216ca54425a83c3c1
link.rs                 155562eff48eccf53a06acfc77e669876d8783f6378a194ea53a4ac0514f72fe
note_link.rs            83e5f9c3be0d2267c9c2765ffcc949dfa4c9bce34474b094ccc2fa253d70e524
wide.rs                 690348151a12dbe8db260108a2f04ddedf5896410040d03e8712e8e853592bc5
bin/ristretto-money.rs  b295eacdf049bdc89d3a94f74f1fd65c57b2f19bd9a7b4ef75996be0f89fc15d
lib.rs                  345acc94e07fa94990ef25de4b00602f7d5d6a23f2955132cf34f209f2d8e092
money.rs                c1ffc6ca870c7c806d708e5a348567501c975d379953baaa4d0ec119a7a8be46
crypto.rs               91a8f28883c0311b22dab670e2104dbef1dd009ce31e1c11196a0388e139b60d
wire.rs                 2cf07cc1f6b585ead0d78969eeef649c890bcea7fa5560eaa1f28de74351c5e9
state.rs                951ba1f2bf6fd487c33423cfd228e1fa0c4a567d4917f29e7f32961b3c716d63
```

Существующий `measurements/p-link/src/poseidon2.rs`: `98b7500274ae673cb37115873c9d1f831af6a74d775a0466e743c610710130d1`. Никакие builds, cargo tests, prover invocations или RAM measurements в этом review не запускались. Memory/time полного source+12 proofs с cold PK и32SU остаются результатом отдельного измерения; источник не даёт права объявить лимит512000000B выполненным или невозможным.

### 8.5. Дополнительный boundary check нового source CLI

После основного чтения parent добавил в scope новый [ristretto/src/main.rs](ristretto/src/main.rs). Он прочитан целиком,74 lines; SHA-256 `66c0a6377431e037d4159ec391b2379ecd35726ae03600d0cd08d6028fd88456`. Это обновляет **RSR-G01**: для неизменных входных artifacts и доверенного parameters directory полный source/opening gate теперь **source-confirmed**.

[batches:22](ristretto/src/main.rs#L22) проходит part0…12 под соответствующими `part-00/vk.bin`…`part-12/vk.bin`, требует `p_link.bin` ровно13×128 bytes и проверяет каждый segment. [public_digests:19](ristretto/src/main.rs#L19) требует version1, steps12, ровно13 canonical Fr digests. Source и opening inputs выводятся из одного формата Offer, включая context и opening_binding; последний step выводит coordinates из canonical Ristretto C. Таким образом, для fixed input files нельзя пройти gate, просто пропустив proof, передав укороченный digest vector или свободные final coordinates. Это source-level вывод; negative CLI tests этим рецензентом не исполнялись.

[bind-wallet:57](ristretto/src/main.rs#L57) пересчитывает witness digest после обновления actual four FullProof public fields, сохраняя private witness и соль; helper не заменяет сам genuine P_L2 verification. [wallet:58](ristretto/src/main.rs#L58) пишет соль в private wallet через `write_private_json`, а public Offer её не получает. Это закрывает проверенный здесь public writer salt boundary. `batches` также экспортирует только публичные digests и proofs.

**RSR-B02 / историческая integration finding, исправление перепроверено в §8.7:** main.rs:32 в указанном выше snapshot перечитывал Offer, digest file и весь proof file заново на каждом part. Если эти пути доступны для изменения недоверенной стороной во время verify, успешный цикл не удостоверяет один неизменный bundle: part0 может провериться по snapshot A, а последующие parts — по snapshot B. Для локальных immutable fixtures это явно допустимое окружение; для node intake нужна единая snapshot/receipt граница. Минимальное исправление — прочитать и удерживать public Offer/digests/13 proofs одного row один раз, затем проверять все части именно этого объекта; альтернативно проверять неизменные hashes каждого прочитанного artifact и связывать downstream receipt с ними. Объём public bundle мал и не требует удерживать все proving keys. Это не криптографический контрпример для стабильных файлов, не утверждение об исполненной файловой гонке и не уже доказанный remote exploit. Аналогично, VK доверяются как configured parameters directory: сам main.rs не сверяет внешний cryptographic pin manifest и не создаёт trust в переданных путях.

### 8.6. Проверка предлагаемого signed32 carry профиля

После чтения текущего BP64 snapshot parent предложил `OFFSET=2^31`, BP32. **Математически этого достаточно** для всего объявленного wide shape, не только свежей fixture. Пусть `B=2^16`, `K=128(B−1)+1=8,388,481<2^23`. Для максимум8×16 signed digit products и одного rhs digit:

```text
|t_prev| <= K
|column − rhs + t_prev| <= 128(B−1)^2 + (B−1) + K = B*K
=> |t_next| <= K.
```

Индукция начинается с t_prev=0; для корректной integer relation quotient carry является целым. Все honest carries помещаются в signed32. Для adversarial range-proved `t∈[−2^31,2^31−1]` локальный bound становится `|E|<2^39+2^16+2^31+2^47<2^48<l_R`; exactness и endpoint аргумент сохраняются. Нужны синхронные изменения OFFSET, prover/verify range bits, поддержка32 в обоих BP wrappers и прежние terminal-zero/padding checks. Пропуск умножений на public zero coefficient также не меняет relation. Эта секция подтверждает proposed optimization algebra; **она не утверждает, что соответствующий patch прочитан или исполнен**. Cold/warm verifier time можно измерять отдельно, сохраняя оба результата; warm result не заменяет cold512MB gate.

### 8.7. Повторная проверка freeze/receipt и реализованного BP32

Bounded Tier2 source fallback: повторно прочитаны `src/main.rs` и `src/wide.rs`, relevant `src/lib.rs`, `run_lifecycle.py:241–318`, его `sha/command`, а также сериализация `Offer` и `vss::digest`. Metadata discovery снова обнаружил только unrelated Open Design `list_projects`; graph tools отсутствуют, generation/coverage не подтверждены. Изменён только этот research-документ. Во время чтения Rust source был отформатирован parent; ссылки и новые hashes ниже относятся к отформатированному snapshot.

**RSR-B02 — RESOLVED, source-only.** [freeze:100](ristretto/src/main.rs#L100) один раз читает typed Offer,13 canonical digests и proof bytes ровно13×128B; proofs декодируются и сохраняются вместе с inputs в `Frozen`. [batches:120](ristretto/src/main.rs#L120) строит весь frozen vector перед verification, затем каждая part использует `f.proofs[part]` и inputs из **того же** `f.source/f.digests`, без повторного чтения wallet artifacts. Последовательная загрузка разных VK сохранена. Изменение input files после freeze больше не может смешать source proof одного Offer с opening proofs другого внутри этого запуска.

Receipt содержит path, SHA-256 exact proof bytes, canonical digest vector и hash typed Offer, приведённого к JSON Value. Он возвращается в `verified_rows` только после успешного завершения всех частей ([main.rs:199](ristretto/src/main.rs#L199)). `Offer` запрещает unknown fields; `to_public` проверяет canonical fields/points и ranges. Offer hash является hash сериализованного **содержимого**, не hash исходного JSON с пробелами: `vss::digest` использует SHA-256 от компактного JSON. В данном dependency profile serde_json map имеет sorted keys; Offer содержит только constrained ASCII numeric/hex strings, целые числа, bool и arrays. Это согласовано с Python `sha`, использующим sorted compact JSON ([run_lifecycle.py:38](ristretto/run_lifecycle.py#L38)); raw proof hash отдельно покрывает bytes.

[run_lifecycle.py:265](ristretto/run_lifecycle.py#L265) читает Offer один раз в список `offers`, требует точное число receipts и сверяет в каждом row path, Offer hash, proof SHA-256 и digests. Несовпадение вызывает `ValueError`. [admission:290](ristretto/run_lifecycle.py#L290) затем использует именно сохранённый объект `o` из этого списка, проверяет VSS commitment against `o["commitment"]` и пишет этот `o` в tribute table ([line316](ristretto/run_lifecycle.py#L316)). Повторного чтения Offer для admission здесь нет. Поэтому смена Offer-файла после receipt comparison не меняет admitted body; смена до comparison отвергается, если hash не совпал. Отдельные последующие reads proof/digest файлов нужны для проверки receipt, но их результат не подменяет сохранённый admitted Offer.

Freeze не требует atomically читать все три файла одновременно: даже если атакующий меняет их во время первоначального чтения, получившийся единый in-memory tuple целиком проходит все13 proofs. Требование заключается в совместной проверке одной tuple и использовании её Offer дальше. Не заявляется adversarial execution filesystem race; вывод получен чтением этих веток.

**BP32 patch — SOURCE-CONFIRMED.** [wide.rs:9](ristretto/src/wide.rs#L9) использует offset2^31; [prover:114](ristretto/src/wide.rs#L114) и [verifier:151](ristretto/src/wide.rs#L151) используют32 bits. Verifier сохраняет exact64 carry commitments,48 openings, zero padding48…63 и terminal point `2^31 G`. Оба [range wrappers](ristretto/src/lib.rs#L194) разрешают ровно8/16/32; [generators:32](ristretto/src/lib.rs#L32) имеют capacity32 и party capacity128. Relevant source search показал consumer profiles8,16,32, без оставшегося BP64 consumer в этом backend. Верхний declared profile128 operands в BP aggregation достаточен для padded128 F при8 monetary operands и padded64 wide carries; это profile consistency, не measured allocation.

Доказательство §8.6 применимо к фактическому коду без изменения: честный carry `|t|≤8,388,481<2^23`; у злонамеренного BP32-bounded carry local `|E|<2^48<l_R`. Prover хранит shifted values как u64, но accepted range задаёт именно BP32 verifier; отсутствие отдельного `u≤u32::MAX` host guard не расширяет принимаемый statement. Prover-side endpoint и verifier terminal identity сохранены.

[terms:51](ristretto/src/wide.rs#L51) пропускает multiplication только когда **публичный** coefficient digit равен0. Host carry/blinder arithmetic может по-прежнему прибавлять нулевые terms; результат тот же. Пропуск `point()` для operand с нулевым coefficient не убирает в составе MoneyProof проверку его ciphertext/F encoding и BP16 ranges. Самостоятельный `wide::verify` остаётся relation subproof, которому нужны outer ranges и valid points, как было указано в §8.3.

**Оставшиеся границы:** verifier report/receipt и VK directory должны контролироваться узлом, а не приниматься как самозаверение wallet. Receipt не имеет собственной подписи и не создаёт trust при произвольной записи в controller output directory; локальный PoC прямо исключает isolation от host administrator. Новый receipt покрывает P_link artifacts; он не покрывает bytes отдельного P_L2 файла, перечитываемого controller после `node-l2-verify` ([run_lifecycle.py:278](ristretto/run_lifecycle.py#L278)). Этот re-review не утверждает, что исправлен полный P_L2 intake/storage boundary или production source authority. К самим рассмотренным freeze/admission и BP32 исправлениям нового конкретного false-acceptance замечания не найдено.

Parent сообщил **5 PASS** после изменений. Это parent-reported execution, не независимое повторение и не evidence завершения256-run. Этот рецензент не запускал tests, Cargo, proofs, benchmark или full lifecycle; RAM/timing выводы не добавлены. Проверены source relations, hashes и существование локальных ссылок.

Новый SHA-256 snapshot, paths относительно `variant2/ristretto`:

```text
src/main.rs       b1d77bdb2cedb7bd8ce1d4abea915cf26ab5946617cb2a6425123676a84a1419
src/wide.rs       cc092d00d809dd22c9a4162cb4176fb9f85101a1c7600599d55a0d5b66318bd4
src/lib.rs        8733b33ed196338e331f33c449db2629a3db3b7b358283144dd18eb3439ae253
run_lifecycle.py  22e31ecbad3fc0aba4211074f69128111c9a1e3152c1ba8b4a0a72ee4576fc29
src/vss.rs        73fbd51827e58926068adcbfda3ba280749c1d56cd737ea768903e68e651b287
src/wire.rs       2cf07cc1f6b585ead0d78969eeef649c890bcea7fa5560eaa1f28de74351c5e9
src/money.rs      0f8c7094c34852ab1f3993320a1502a552e023b6979a447250ded98b49557a66
```

### 8.8. Повторная проверка proving по частям и P_L2 snapshot

Ещё один bounded source-only recheck охватил изменения `ristretto/src/main.rs`, `ristretto/run_lifecycle.py` и узкую существующую ветку `source-helper/src/main.rs:100–119`, чтобы проверить фактического потребителя нового manifest. Graph metadata снова показал только unrelated Open Design `list_projects`; использован Tier2 exact-source fallback, graph generation/coverage UNKNOWN. Прочитаны исходники, runtime не изменён, CPU tests/provers/benchmarks не запускались параллельно финальному256-run.

**Запуск по частям сохраняет полный verifier contract.** [batch-prove-part:303](ristretto/src/main.rs#L303) вызывает `batches(..., false, Some(part))`; [batches:120](ristretto/src/main.rs#L120) отвергает `part>12` и любой `verify=true` вместе с `only_part`. Обычные `verify/batch-verify` передают `None`, выполняют прежний freeze полного packet и проверяют все13 proofs. Частичный proving не создаёт `verified_rows` receipt: Frozen vector для него пуст. Part0 перезаписывает packet первым128-byte proof и public metadata, parts1…12 дописывают по128 bytes. Proving steps по отдельности не гарантируют порядок/полноту packet при произвольном ручном вызове CLI, но это не расширяет acceptance: полный verifier требует точный размер1664B, фиксированные offsets и правильный per-part VK для всех частей.

[source_prove:241](ristretto/run_lifecycle.py#L241) вызывает parts0…12 по порядку через `command/subprocess.run`. Следующий процесс начинается после завершения предыдущего; nonzero exit или превышение указанного RAM limit останавливает orchestration. Это освобождает address space завершившегося native процесса до следующего PK load; нет требования, чтобы allocator одного долгоживущего процесса возвращал arenas ОС. Wallet witness и intermediate public artifacts сохраняются между процессами на диске; consistency продолжает обеспечиваться full proof chain, а не доверием к одинаковому содержимому wallet files в каждом prover invocation.

После batch proving [run_lifecycle.py:284](ristretto/run_lifecycle.py#L284) всё равно вызывает полноценный node `batch-verify`; admission не опирается на partial-prove report. Поле `proof_bytes=1664` в partial report — **ожидаемый размер полного packet**, не размер созданной этой командой части и не утверждение о полной verification. `parts` одного такого report содержит только исполненную часть; summary controller собирает13 записей. Отдельный cold proving packet не является admission packet; в данном маршруте admission использует full-verified `link_rows`.

Code-level measurement profile явно суммирует wall time13 native child invocations и берёт максимум их high-water RSS; Python orchestration исключена из этой aggregated metric ([line252](ristretto/run_lifecycle.py#L252)). Это корректное описание собранной метрики, а не новое независимое измерение total host/wallet footprint данным рецензентом. Лимит и его фактический результат должны оставаться привязаны к соответствующим run artifacts.

**Оставленный в §8.7 P_L2 TOCTOU concern — CLOSED, source-only для этого controller-owned snapshot маршрута.** [run_lifecycle.py:276](ristretto/run_lifecycle.py#L276) один раз копирует каждый исходный `p_l2.bin` в новый `control/node-l2-input-i`; verifier manifest содержит именно эти directories. В [source-helper verify-batch:112](../source-helper/src/main.rs#L112) читается `out/p_l2.bin`, декодируются его public inputs и вызывается реальный `verify_circuit::<FullProof>(&bytes)`. В этой ветке helper не обращается к private wallet или первоначальному mutable offer path.

Перед admission [run_lifecycle.py:301](ristretto/run_lifecycle.py#L301) перечитывает **ту же controller-owned копию**, сравнивает исходный публичный artifact с ней, проверяет формат8900B/count4 и сравнивает четыре public fields с уже receipted in-memory Offer. Следовательно, изменение исходного `p_l2.bin` после copy не может подменить verified header: admission либо использует прежний snapshot, либо отвергает несовпадение оригинального публичного artifact. Source registry, VSS commitment check и вставка tribute продолжают использовать сохранённый Offer. Проверка header/count/four-fields реализована через Python `assert`; заключение относится к обычному запуску с включёнными assertions, а не к `python -O`/`PYTHONOPTIMIZE`. Контроль над `control` directory остаётся явной trust предпосылкой локального PoC; snapshot не заявляет isolation от host administrator или production authority для Merkle root/owner.

К двум этим изменениям нового конкретного false-acceptance замечания не найдено. Условие trusted VK/report/controller сохраняется; full production source registry/consensus и malicious-MPC theorem не входят в закрытую здесь файловую границу. RSR-B02 остаётся RESOLVED; BP32 source hashes совпали с предыдущим recheck, arithmetic повторно не исполнялась.

Parent сообщил завершённый малый full4 lifecycle, **5 Rust tests PASS** и **7 source-packet verifier cases PASS**, затем запуск полного256. Это parent-reported результаты; малый lifecycle предшествовал двум последним orchestration правкам. Я не повторял tests и не объявляю текущий256-run завершённым. Последние inspected hashes:

```text
variant2/ristretto/src/main.rs       946200cabb72b6a6bf854843dabca7a7d5f6f4479f77eb43f6091cd46da0bcfd
variant2/ristretto/run_lifecycle.py  0e39d9838eb1c3013ff8ef6476a459a620dc1a8b53e39223ec3af9efa7d59096
variant2/ristretto/src/wide.rs       cc092d00d809dd22c9a4162cb4176fb9f85101a1c7600599d55a0d5b66318bd4
variant2/ristretto/src/lib.rs        8733b33ed196338e331f33c449db2629a3db3b7b358283144dd18eb3439ae253
source-helper/src/main.rs           ff2fa9b8af46c0e7e83bc8023d8d566fba7dbc64242987e05760e428fcd2942e
```
