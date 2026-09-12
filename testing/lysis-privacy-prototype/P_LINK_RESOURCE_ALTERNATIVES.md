# P_link: кандидат под лимит wallet RAM 512 MB

> Работа остановлена по указанию пользователя: сначала [схема и требования](PROTOCOL_TRACE_AND_REQUIREMENTS.md), затем новый research. Wide spike не прошёл полный цикл verification/composition/VSS. Его latest measurements сохранены в artifacts; это не принятое решение.

Дата: 2026-09-11. Жёсткий критерий: отдельный wallet proving process, включая загрузку PK, не превышает **512,000,000 bytes**. Предыдущий P-384-в-BN254 вариант остаётся функционально проверенным, но отклонённым по ресурсам baseline. Этот документ не меняет production-кривую и не объявляет новую схему готовой к deployment.

## Проверенные параметры

`ark-ed-on-bw6-761 = 0.5.0` буквально реэкспортирует Edwards-кривую `ark-ed-on-cp6-782 = 0.5.0`. Её поле координат совпадает с `ark_bw6_761::Fr` и с `ark_bls12_377::Fq`; соответствующий `EdwardsVar` работает через native `FpVar`, без эмуляции P-384. Это проверено в скачанных официальных crate archives 0.5.0, VCS commit `df907e8c1601a898c2903ed7ab7bbbb10607f36b`. [arkworks curve family](https://github.com/arkworks-rs/algebra/blob/master/curves/README.md), [закреплённый ed-on-bw6 source](https://github.com/arkworks-rs/algebra/blob/df907e8c1601a898c2903ed7ab7bbbb10607f36b/ed_on_bw6_761/src/lib.rs).

| Параметр | Значение |
|---|---|
| Coordinate field | 377 bits; `258664426012969094010652733694893533536393512754914660539884262666720468348340822774968888139573360124440321458177` |
| Scalar field q | 374 bits; `32333053251621136751331591711861691692049189094364332567435817881934511297123972799646723302813083835942624121493` |
| Кривая | `−x² + y² = 1 + 79743 x²y²` |
| Cofactor | 8 |
| `10⁹ × (2²⁵⁶−1)` | 286 bits; строго меньше q |
| Canonical compressed Edwards commitment | 48 bytes, измерено serializer |
| Secret scalar / VSS pair | 48 / 96 bytes |

Capacity проверена обычной целочисленной арифметикой, без модульного сокращения. Поэтому одна scalar sum сохраняет весь допустимый агрегат; limb totals не нужны. Целевые uint256-ограничения протокола отдельно остаются constraints/policy.

Для этих параметров `−1` — quadratic residue, `79743` — nonresidue: проверено возведением в `(p−1)/2`. Это условия полноты Edwards addition, поэтому библиотечные affine formulas не требуют исключать identity или inverse pairs. [Twisted Edwards Curves](https://eprint.iacr.org/2008/013).

## Почему BW6-761, а не CP6-782

Оба pairing backend допускают одну и ту же native Edwards-кривую. У CP6-782 нет дополнительного выигрыша для самого commitment gadget. BW6-761 предложена как оптимизированная замена CP6-782 для этого семейства, с анализом pairing security. Это мотивирует выбор spike; опубликованные speedup не являются измерениями Outbe. [El Housni–Guillevic, 2020/351](https://eprint.iacr.org/2020/351), [Zexe](https://eprint.iacr.org/2018/962).

Сериализованный Groth16 proof состоит из двух G1 и одного G2 элемента. Для BW6-761 оба типа имеют coordinate base field 761 bits и по 96 bytes в compressed arkworks representation, поэтому proof ожидается 288 bytes; фактический serializer проверяется spike. CP6-782 использует G2 над Fq3 и не нужен для текущего эксперимента. Размеры proof не включают C, digest, контекст или существующий L2 FullProof. [Groth16](https://eprint.iacr.org/2016/260).

## Связь двух proof через одинаковые байты

Перенос всего circuit на wide field сделал бы существующий BN254 Poseidon2 non-native. Вместо этого кандидат использует два независимых proof:

```text
P_source / BN254:
  existing canonical draft hash + economics → a_uint256
  D = SHA256(domain || BE32(a) || secret_salt32)

P_commit / BW6-761:
  C = aG + rH on native wide Edwards
  D = SHA256(domain || BE32(a) || secret_salt32)

Verifier: verifies both proof, checks identical raw D, then uses C for VSS.
```

Точный domain: ASCII `OUTBE-P-LINK-BRIDGE-v1`. Nominal всегда 32 bytes, big-endian с ведущими нулями; salt всегда 32 bytes. Public D передаётся в каждом proof как **два 128-bit big-endian integers**, не как один field residue. Оба circuit связывают SHA input bytes с теми же bits nominal, которые используются в economics/commitment. Salt остаётся witness и в реальном кошельке должен генерироваться свежим CSPRNG.

Обоснование binding — наш вывод из knowledge soundness двух proof и collision resistance SHA-256: если извлекаемые пары `(a₁,s₁)` и `(a₂,s₂)` различаются, но дают один D при однозначном encoding/domain, это коллизия. Поэтому принимаемая пара proof связывает одно целое a, без предположения равенства residues разных полей. ZK скрывает witnesses самих proof. [Groth16](https://eprint.iacr.org/2016/260), [FIPS 180-4, SHA-256](https://nvlpubs.nist.gov/nistpubs/FIPS/NIST.FIPS.180-4.pdf).

**Hiding salted hash не следует из одной collision resistance.** Здесь требуется обычное дополнительное предположение о SHA-256 как скрывающем hash commitment при неизвестном случайном 256-bit salt; в random-oracle модели это естественная оценка. Публичный либо предсказуемый salt не обеспечивает такого свойства. Production review должен отдельно принять это предположение, domain/context правила и CRS trust model. Свежий salt также не доказывается самим circuit.

`Sha256Gadget<ConstraintF: PrimeField>` в `ark-crypto-primitives = 0.5.0` реализует SHA над UInt8/UInt32/Boolean и подходит к обоим полям. API: `Sha256Gadget::digest(&[UInt8<F>]) -> DigestVar<F>`; `DigestVar.0` содержит 32 байта. Features: `crh`, `r1cs`, `std`. Источник проверен локально, VCS `455d599aae0aa81acf1c2faadf37def63a25a291`. [Закреплённый SHA gadget](https://github.com/arkworks-rs/crypto-primitives/blob/455d599aae0aa81acf1c2faadf37def63a25a291/src/crh/sha256/constraints.rs).

## H и subgroup

Spike [wide-commit-proof](measurements/wide-commit-proof/src/main.rs) использует публичный детерминированный research algorithm: SHA-512 от domain и counter → y в поле координат → decompress Edwards → умножение на cofactor 8 → исключение identity/G/−G. Проверяется subgroup. Это не `known_scalar × G`: рецепт не выдаёт discrete log H относительно G.

Данный try-and-increment рецепт **не является стандартизованным RFC 9380 suite** и не объявляется готовыми production parameters. Нужна фиксация и отдельная проверка распределения/map/DST, canonical encoding и generator derivation. Для публичной однократной генерации H время перебора не зависит от кошельковых секретов. [RFC 9380](https://www.rfc-editor.org/rfc/rfc9380.html).

Все VSS commitments должны быть points правильной prime-order subgroup, потому что cofactor теперь 8, а не 1 как у P-384. CanonicalDeserialize с validation/subgroup check обязателен для сетевых points. В circuit используются фиксированные валидные subgroup G/H и constrained selection их кратных. Результат связывается с public x/y; произвольные public coordinates не принимаются на доверии.

## Исполняемый spike и пределы проверки

Standalone crate: [Cargo.toml](measurements/wide-commit-proof/Cargo.toml). Native APIs: `EdwardsVar::precomputed_base_scalar_mul_le`, `Groth16<BW6_761>`, `CanonicalSerialize/Deserialize`. a ограничен 256 bits, r ограничен точным scalar q; 315 двухбитных окон для `a256 + r374`.

Режимы `synth`, `setup`, `prove`, `verify`, `negative` запускаются отдельно. [measure_process.py](measurements/wide-commit-proof/measure_process.py) включает PK load/validation в wallet prove process, записывает kernel peak RSS через `wait4` и использует `ps` sampling для раннего kill выше 512,000,000 bytes. Финальный PASS определяется kernel high-water, не только sampling. Setup отдельно и не входит в wallet cap.

Shared fixture публичен: `a = 4086512338`, salt `8866fe02254f4bbbbbad6967caa884b7f5181a27f3e865c1716fed89078eeda9`, D `44e5a353ba1c91a296016a162afd2548ca85520588fd1e7fdf822f9cdcc07c95`. Он подтверждает совместимость encoding двух реализаций, не приватность реальных денег. Proving/setup randomness использует OsRng; CRS создан одним исследовательским процессом, без production ceremony.

Перед запуском wallet prove synthesis прошёл: **81,077 constraints**, 4 public inputs, **150,798,336 bytes** kernel peak RSS. Из этого ещё не следует wallet PASS: финальное proving измерение фиксируется отдельно. L2 verifier, источник Oracle, admission, DA, сетевой handoff и мобильный браузер не тестируются этим компонентом.
