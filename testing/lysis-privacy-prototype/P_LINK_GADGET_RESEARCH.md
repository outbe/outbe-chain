# P_link: P-384 Pedersen внутри BN254 R1CS

Дата: 2026-09-11. Область: связь скрытого uint256 nominal с тем же commitment, который использует wide-field VSS. Канонический draft hash, integer economics, L2 admission и Groth16 runner реализуются отдельно. Источники проверены по первоисточникам и локальным пакетам с фиксированными версиями. Это не аудит библиотек или production-протокола.

## Решение

BN254 scalar field служит полем R1CS, арифметика координат P-384 выражается через `EmulatedFpVar<P384Fq, ark_bn254::Fr>`. Нужен собственный `P384Var`: готовый `ark_r1cs_std::groups::curves::short_weierstrass::ProjectiveVar` 0.5.0 жестко связывает constraint field с `BasePrimeField<P>` и не подходит для P-384 поверх BN254. Проверено по объявлению структуры и trait bounds в закреплённом исходнике. [ark-r1cs-std, commit dc48c66](https://github.com/arkworks-rs/r1cs-std/blob/dc48c66e27a9d6d0f4356c0bf27b54cbd5459853/src/groups/curves/short_weierstrass/mod.rs#L49).

Reference implementation: [p384_gadget.rs](measurements/p-link/src/p384_gadget.rs). Это R1CS constraints для `C = aG + rH`; native вычисление точки служит независимым reference и подготовкой public input, не заменяя circuit constraints.

Версии: `ark-ff = 0.5.0`, `ark-bn254 = 0.5.0`, `ark-r1cs-std = 0.5.0`, `ark-relations = 0.5.1`, `p384 = 0.13.1`, `sha2 = 0.10.9`. Переход на arkworks 0.6 не требуется. Документация `latest` уже может относиться к 0.6; выводы о 0.5 сделаны по package source и его `.cargo_vcs_info.json`.

## Три разных поля

| Поле | Назначение | Модуль |
|---|---|---|
| BN254 Fr | R1CS и существующий draft hash | Определён `ark_bn254::Fr` |
| P-384 поле координат | Формулы сложения точек | `fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff` |
| P-384 поле скаляров | Pedersen/VSS, blinding | `ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973` |

Имя Rust-типа `P384Fq` означает поле координат по соглашению arkworks. Нельзя подставлять scalar modulus VSS в этот тип. Для coordinate-field `MontConfig` multiplicative generator равен 19; у scalar field RustCrypto generator равен 2. Кривая имеет `a = -3` и cofactor 1. [Standards for Efficient Cryptography 2 (SEC 2), §2.5.1](https://www.secg.org/sec2-v2.pdf), [RustCrypto field source](https://github.com/RustCrypto/elliptic-curves/blob/409fd29e00f91cea6cb4f7b632326670e0162388/p384/src/arithmetic/field.rs#L128).

`a` поступает как 256 constrained Boolean bits: точное целое меньше обоих модулей P-384. `r` поступает как 384 bits; caller дополнительно доказывает `r < q`, например canonical decomposition scalar-field `EmulatedFpVar` либо bounded-bit comparison. Равенство BN254 residues не доказывает равенство uint256. Нужны те же bounded limbs/bits, которые участвуют в вычислении nominal.

## Полное сложение и infinity

Представление homogeneous `(X:Y:Z)` означает `x = X/Z`, `y = Y/Z`, infinity `(0:1:0)`. Это не Jacobian `(X/Z²,Y/Z³)`.

Используется полная формула Renes–Costello–Batina, Algorithm 1: она применима к prime-order short Weierstrass P-384 и обрабатывает `I+I`, `P+I`, `P+P`, `P+(-P)` без деления на `x₂−x₁`. [Renes, Costello, Batina, 2015/1060](https://eprint.iacr.org/2015/1060). Формула адаптирована из ark-r1cs-std 0.5.0, строки 634–670; attribution сохранён в Rust-файле.

В рабочем gadget каждая исходная точка выбирается constrained bits из фиксированной валидной таблицы, затем используются только полные сложения. Инвариант валидной точки сохраняется по построению. Нельзя заменить таблицу произвольными `new_witness(x,y,z)` без дополнительных проверок. `NonZeroAffineVar` не используется: библиотека предупреждает о недостаточных constraints/unsatisfiability при infinity. [Закреплённый исходник](https://github.com/arkworks-rs/r1cs-std/blob/dc48c66e27a9d6d0f4356c0bf27b54cbd5459853/src/groups/curves/short_weierstrass/mod.rs#L30).

Для nonidentity public commitment `C=(cx,cy)` caller накладывает:

```text
Z != 0
X = cx * Z
Y = cy * Z
```

Все проверки выполняются constraints в emulated coordinate field. Два равенства без `Z != 0` недостаточны: `(0,0,0)` удовлетворяет им для любого C. Compressed SEC1 C канонически декодирует проверяющая сторона; именно его координаты становятся public inputs. Отдельные prover coordinates без сравнения с C VSS неприемлемы.

Reference admission требует nonidentity C; кошелёк пересэмплирует r, если получился identity. Внутренние infinity разрешены. Поддержка identity в публичном wire format требует отдельного канонического представления и соответствующей ветки constraints.

## Fixed-base окна

Для двухбитного окна и `B = G` либо `H` строится постоянная таблица:

```text
T_i = [ I, 2^(2i)B, 2*2^(2i)B, 3*2^(2i)B ]
digit_i = bit[2i] + 2*bit[2i+1]
selected_i = T_i[digit_i]
C = sum(selected_i for a/G and r/H)
```

`EmulatedFpVar::two_bit_lookup` выбирает каждую координату из четырёх констант; X, Y и Z используют одни и те же Boolean variables. API 0.5 принимает little-endian bits. [field_var.rs](https://github.com/arkworks-rs/r1cs-std/blob/dc48c66e27a9d6d0f4356c0bf27b54cbd5459853/src/fields/emulated_fp/field_var.rs#L352).

Полный диапазон требует 128 окон nominal и 192 окна blinding: 320 additions. Таблицы публичны, детерминированы и общие для всех Tribute. Это число операций алгоритма, не throughput или число R1CS constraints. Следующая возможная оптимизация — четырёхбитные окна с constrained table selection; до измерения их нельзя объявлять быстрее.

H точно совпадает с wide-vss. Suite — `P384_XMD:SHA-384_SSWU_RO_`; message — `Pedersen blinding generator for Outbe aggregate research; not production parameters`; DST — `OUTBE-RESEARCH-VSS-v1-P384_XMD:SHA-384_SSWU_RO_`. Используется hash-to-curve, а не `H = known_scalar * G`. [RFC 9380, §8.3](https://www.rfc-editor.org/rfc/rfc9380.html#section-8.3).

Expected compressed H:

```text
0338678c78c59cf7b985c6cf9c2980028848e48c90a6e47399ac4b488350fd9cdd319a8526d7871792072fa67827813f23
```

## Обязательства P_link caller

- Связать canonical draft hash с тем же `a` через integer economics, без BN254 wraparound.
- Ограничить integer witnesses, промежуточные произведения, quotient и remainder. `N = qD+r`, `0 <= r < D`, `D > 0` должны выполняться над целыми нужной ширины.
- Проверить L2 proof и совпадение его public hash с public hash P_link.
- Привязать chain/day/offer/Oracle snapshot/owner и version domain к разрешённому admission context.
- Проверить canonical public C и обеспечить один C и один H/version у P_link и VSS.
- Использовать разрешённый Groth16 verifying key; single-party research setup с OsRng не является production ceremony. Fixed fixture witness/blinding опубликован в коде и не подходит для реальных секретов.

Host assertions не заменяют эти constraints и проверки. Межгрупповой Schnorr shortcut не используется: равенство residues двух групп разных порядков само по себе не связывает полное исходное целое.

## Проверка reference implementation

В модуле есть тесты с реальными witness bits и emulated variables: exceptional additions; Pedersen 0/1 и случайные пары против RustCrypto; неверный public point; полный `a = 2^256−1`, `r = q−1`; H fixture и отклонение неканонического native scalar. Фактически выполненные проверки и итоговые измерения полного P_link фиксируются ниже и в основном benchmark отчёте. Само наличие теста не означает его исполнение.

`cs.is_satisfied()` подтверждает проверенные fixtures, не заменяя adversarial review. Proving time, proof bytes и verification time измеряются на полном P_link circuit, не выводятся из native P-384 benchmark.

В отдельной release test harness, подключающей тот же Rust-файл по абсолютному path, выполнены и прошли:

- `complete_addition_handles_identity_doubling_and_inverse`: все семь exceptional cases, 0.87 s.
- `wrong_public_point_is_unsatisfied`: неправильный C отвергнут constraints, 1.07 s.
- `h_matches_vss_fixture_and_native_rejects_noncanonical_scalar`: H совпал; `r = q` и identity admission отвергнуты native reference.

Это локальные smoke checks, не статистический benchmark. Полноразрядный standalone gadget test и 64-bit random test на момент этой записи не подтверждены отдельным завершённым запуском; чтобы не удваивать расход памяти, полный circuit измеряет основной runner.

## Проверка integer linkage

При независимом просмотре полного circuit обнаружена и исправлена недостающая проверка padding в `UInt::mul`: при произведении 128-bit значения и 64-bit константы с результатом шириной 256 bits старый цикл проверял только три младших limb. Четвёртый limb мог добавить `t*2^192` к `u*10^6`, изменив nominal при неизменном draft hash. Исправленный цикл покрывает максимум размеров произведения и результата.

[Adversarial regression](measurements/p-link/tests/adversarial_integer.rs) меняет непосредственно witness assignment старшего limb и соответствующего Boolean bit. После исправления обе ширины результата, 256 и 384, отвергают такую подстановку; честный padding принимается. Два теста прошли в release, 0.01 s. Setup начатый до исправления был остановлен; ключи и результаты должны относиться к исправленному circuit.

Для текущих UInt widths carry bounds не допускают BN254 wraparound: multiplication column содержит не более 16 произведений 64-bit limbs и 72-bit carry, то есть левая часть меньше `2^133`, правая — меньше `2^136`. Addition с 1-bit carry меньше `2^65`. Все эти границы существенно меньше BN254 Fr modulus. Remainder ограничен `r < D`, а ненулевые цены и выбор `max(vr,sc)` обеспечивают `D > 0`.
