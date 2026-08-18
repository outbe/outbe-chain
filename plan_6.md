# План перевода COEN, GRATIS, PROMIS и WCOEN на 6 decimals

## 1. Цель работы

Перевести существующую экономическую логику сети с исторического предположения:

```text
1 COEN = 10^18
```

на единую каноническую модель:

```text
1 COEN   = 1_000_000 unit
1 GRATIS = 1_000_000 GRATIS-unit
1 PROMIS = 1_000_000 PROMIS-unit
1 WCOEN  = 1_000_000 WCOEN-unit
```

Сеть создаётся с нуля, поэтому migration, compatibility layer и поддержка старой размерности не нужны. Но существующий код необходимо именно **перевести**, потому что сейчас старые `10^18` зашиты в константах, формулах, Oracle, INTEX, тестах, reference implementations и generated artifacts.

## 2. Какой результат должен получиться

Вся связанная экономическая цепочка должна работать в согласованных единицах:

```text
COEN
→ Tribute
→ GRATIS
→ Lysis / NOD
→ GEM
→ PROMIS
→ INTEX
→ WCOEN
→ settlement / refund
```

Отдельно:

- Oracle остаётся generic;
- все рынки `COEN/ISO` используют шестизначные price и COEN-volume, потому что
  ISO reference currencies принадлежат шестизначному stablecoin domain;
- VWAP, zero-volume sentinel и S-Curve используют шестизначную семантику для
  каждого `COEN/ISO`, включая `COEN/840` и `COEN/978`;
- non-ISO generic Oracle markets сохраняют существующий price/VWAP contract и
  не участвуют в S-Curve; OCOMP day-type state остаётся только у `COEN/840`;
- Metadosis production math не переписывается, потому что его арифметика scale-neutral;
- типы `U256/u128/u64`, ABI, wire/state layouts сохраняются;
- Tribute `base/atto` сохраняются;
- никакого нового Decimal framework, U512 или pair-decimals metadata не вводится.

## 3. Общее системное решение

### 3.1. Единицы

В `crates/blockchain/primitives/src/units.rs` закрепляются typed scale constants:

```text
SCALE_1E6_U64
SCALE_1E6_U128
SCALE_1E6_U256
```

Все равны `1_000_000` и владеют только numeric representation scale. Семантика
остаётся в имени поля или локальной величины: COEN amount, GRATIS amount,
PROMIS load, COEN/ISO rate или Credis annual rate. Равенство scale не делает
сами величины взаимозаменяемыми, но отдельные aliases с одинаковым значением
для каждого токена, price и rate не создаются.

`Units::in_units` становится преобразованием целого количества native COEN в `unit`. Сохраняющиеся независимые fixed-point consumers обязаны явно использовать собственную константу, а не маскироваться под token denomination.

### 3.2. Переиспользуемая математика

В primitives добавляются только действительно общие операции:

- `checked_mul_div_floor`;
- `checked_mul_div_ceil`;
- COEN/ISO price ↔ существующий `128.128 price bin`.

Vendor/Pancake `price_helper.rs` не переписывается. Для NOD, GEM и INTEX создаётся Outbe-owned шестизначный вход:

```text
price128   = price_units << 128 / 1_000_000
price_units = price128 * 1_000_000 >> 128
```

Специализированные алгоритмы остаются локальными:

- Emission Taylor — в Emission Limit;
- fraction normalization — в Lysis;
- S-Curve — в Oracle;
- payment conversion — в INTEX.

### 3.3. Арифметика сразу в целевой размерности

Если бы COEN, GRATIS, PROMIS, WCOEN и COEN/ISO prices изначально имели scale
`1_000_000`, scoped monetary formulas не использовали бы FP18 с последующим
делением на `10^12`. Cutover приводит production именно к этой модели:

- Tribute nominal считается как `base × 1_000_000 + atto`;
- Lysis load и NOD/GEM cost используют один checked `mul_div` со знаменателем
  `1_000_000`;
- COEN/ISO S-Curve считает `floor(peak6 × coefficient6 / 1_000_000)`;
- Emission Taylor работает сразу с COEN `unit`;
- Gratisfactory и INTEX выполняют единственную domain conversion с заранее
  зафиксированным floor/ceil.

Промежуточный product scale `10^12` допустим только как естественный результат
умножения двух scale-`10^6` величин перед одной checked division. Он не
хранится в state/wire и не является возвратом к decimal18. Q128.128 price bin
также остаётся только внутренним representation adapter. Запрещён шаблон
`scale6 → ×10^12 → historical FP18 formula → ÷10^12`; независимые
dimensionless FP18 contracts перечислены отдельно в разделе 9.3.

### 3.4. Округление

| Операция | Правило |
|---|---|
| Tribute nominal | floor |
| Lysis GRATIS load | floor |
| Lysis/NOD/GEM cost | floor |
| S-Curve | floor |
| Emission terms | floor |
| Gratisfactory payout | ceil |
| INTEX settlement/payment | ceil |

Положительное значение, превратившееся в экономически бессмысленный ноль, отклоняется там, где создаётся позиция, обязательство или платёж.

## 4. Организация работы

Работа доставляется как стек из двух independently-green PR, разделённых по
реально существующим fixed points ветки:

1. **Architecture-freeze PR** — commit `1925c6b6`, только план, transition
   matrix, ownership, file/evidence map и первоначальные PR boundaries;
   production behavior не меняется. Последующие user-approved re-plans после
   stop-trigger остаются документированными commits atomic cutover PR.
2. **Atomic cutover PR** — весь новый test/reference contract, подтверждённый
   RED, общие units/math, P2–P5, generated artifacts и финальный GREEN. Между
   его commits нет mergeable PR boundary: production становится шестизначным
   только как одно законченное изменение.

Отдельный dormant-foundation PR не вводится задним числом: P1 уже существует в
истории ветки как `08328b33`, а его выделение потребовало бы переписывания
последующих commits. До финального GREEN atomic cutover PR не сливается.

Внутри atomic cutover PR — последовательные смысловые коммиты:

```text
все тесты/reference/goldens
→ подтверждённый RED
→ общие units/math
→ Oracle COEN/ISO (S-Curve только для COEN/ISO markets)
→ Tribute/GRATIS/Lysis/NOD/GEM
→ PROMIS/INTEX/WCOEN
→ Emission/native economics/genesis
→ generated artifacts
→ полный GREEN и fresh-network scenario
```

Главное правило: **сначала переводится весь test/reference contract задачи, и
только потом production-код**.

## 5. Точная последовательность

### Шаг 0. Architecture freeze

До изменений фиксируются:

- полный scope и non-goals;
- таблица единиц;
- формулы;
- rounding/zero-result policy;
- Oracle generic boundary;
- неизменяемые ABI/wire/state shapes;
- owner каждого инварианта;
- production/test/reference/vector file map;
- hot files и допустимые проходы;
- порядок коммитов;
- stop-and-replan triggers.

### Коммит T1 — тестовый контракт токенов

```text
test(denomination): define six-decimal token unit contract
```

Меняются только тесты и fixtures:

- primitives units;
- COEN metadata;
- GRATIS;
- PROMIS;
- WCOEN;
- `MockWCOEN.sol`;
- `EscrowAdapter.decimals.t.sol`;
- staking/reward/bond fixtures;
- stablecoin creation bond fixtures.

Фиксируется:

```text
ONE_COEN   = 1_000_000
ONE_GRATIS = 1_000_000
ONE_PROMIS = 1_000_000
WCOEN decimals = 6
WCOEN raw wrapping = 1:1
stablecoin bond = 1_000_000 COEN = 1_000_000_000_000 unit
```

### Коммиты T2 — тестовый контракт Oracle COEN/ISO

```text
test(oracle): define six-decimal COEN840 price contract
test(oracle): extend six-decimal contract to every COEN ISO market
```

Меняются тесты:

- `crates/system/oracle-feeder` aggregator/vote builder;
- `crates/system/oracle/src/scurve.rs` test section;
- Oracle state/runtime/e2e/lifecycle;
- Oracle OCOMP openings;
- NOD/GEM/IntexFactory price-bin expectations;
- Desis/INTEX price fixtures;
- INTEX Solidity metadata expectations.

Фиксируется:

- каждый COEN/ISO price — `ISO stable-unit per COEN`, scale `1_000_000`;
- COEN volume — COEN `unit`;
- каждый COEN/ISO VWAP возвращает шестизначную цену;
- каждый COEN/ISO sentinel использует `SCALE_1E6_U256` как
  one-whole-COEN weight;
- S-Curve coefficients имеют знаменатель `1_000_000`;
- S-Curve запускается для каждого COEN/ISO и не запускается для non-ISO
  generic pairs; OCOMP day-type остаётся только у COEN/840;
- non-ISO generic Oracle price/VWAP пары не меняются;
- INTEX wire price больше не `10^9`.

Структурные codec golden vectors не меняются.

### Коммит T3 — тестовый контракт Tribute/GRATIS/Lysis

```text
test(economics): define Tribute-to-Lysis six-decimal behavior
```

Меняются:

- TEE Tribute compute/process/zk/transport tests;
- Tribute payload producer tests/fixtures в CLI, MCP и operational scripts;
- Tribute tests;
- Gratis/Gratisfactory tests;
- Lysis reference implementation;
- Lysis unit/program/reference/reducer tests;
- `crates/core/lysis/vectors/lysis-v1`;
- NOD/GEM/GemFactory cost tests;
- Fidelity fixtures, содержащие реальные GRATIS amounts.
- CredisFactory lifecycle consumer tests and TEE GRATIS pledge-ticket fixtures,
  где `entry_rate` является COEN/ISO price, collateral/balance — реальными
  GRATIS amounts, а `currency_rate` является отдельной annual rate (scale `1e6`).

Фиксируется:

- один canonical contract применяется одинаково в ZK и non-ZK путях;
- `amount_base` — canonical decimal `u64` в целых единицах без знака, дробной части и ведущих нулей, кроме самого `"0"`;
- `amount_atto` — canonical decimal `u64`, `amount_atto < 1_000_000`;
- `amount_minor = amount_base × 1_000_000 + amount_atto` с checked arithmetic;
- `base="1", atto="500000"` даёт `1_500_000` unit в обоих путях;
- дробный `amount_base="1.5"` и `amount_atto="1000000"` отклоняются в обоих путях;
- nominal Tribute amount;
- GRATIS amount в `GRATIS-unit`;
- Lysis fraction denominator `1_000_000`;
- `gratis_load = floor(amount × fraction / 1_000_000)`;
- cost делится на `SCALE_1E6_U256`;
- sequential и OCOMP дают одинаковый результат;
- exact-budget normalization.

User-approved controlled T3 recovery after the CredisFactory blocker:

- `crates/core/credisfactory/src/tests/e2e.rs` переводит только COEN/ISO
  `oracle_rate`, GRATIS collateral и ledger fixtures на scale `1e6`;
- `refi_rate`, `currency_rate` и debt multiplier переводятся на отдельный
  rate contract со scale `1e6` по последующему user-approved re-freeze;
- test section `bin/outbe-tee-enclave/src/gratis.rs` переводит только
  `PledgeTerms.entry_rate` fixtures на COEN/ISO price scale `1e6`;
- `crates/system/tee/src/protocol.rs` меняет только неверный unit comment для
  `PledgeTerms.entry_rate`; тип, codec, ABI и wire/state layout не меняются;
- production math Credis меняет только fixed-point denominator и literals;
  порядок двух floor-операций, CredisFactory pass-through, Gratisfactory и
  repayment lifecycle не меняются;
- пропущенный test-first контракт фиксируется отдельным late-RED evidence без
  переписывания уже опубликованной истории commits.

User-approved Credis-rate re-freeze supersedes только FP18-утверждения
предыдущего recovery commit `a144d9c1`:

- annualized ISO currency rate: `1.0 = 1_000_000`;
- `4.30% = 43_000`, default USD `3.63% = 36_300`;
- staged formula и текущий порядок rounding сохраняются:
  `term_rate = floor(rate_units × NUMBER_OF_ANADOSIS / 12)`, затем
  `total_debt = floor(principal_units × (1_000_000 + term_rate) / 1_000_000)`;
- объединять два floor в одну операцию запрещено: это было бы изменением
  кредитного алгоритма, а не denomination cutover;
- Credis fields и staged formula владеют annual-rate semantics; общий
  `SCALE_1E6_U256` задаёт только representation denominator;
- Oracle mapping slot, Credis `Position`, ABI `uint256`, field order и codecs
  не меняются; fresh network записывает только rate values with scale `1e6`.

Регрессия Lysis отдельно закрепляется:

```text
allocation = 4_800_000
raw projected load = 4_800_034
normalized load = 4_799_992
dust = 8
```

### Коммит T4 — тестовый контракт PROMIS/INTEX/WCOEN

```text
test(intex): define six-decimal PROMIS and WCOEN lifecycle
```

Меняются тесты:

- Promis/PromisFactory;
- GemFactory;
- Desis;
- IntexFactory;
- Intex core;
- Solidity INTEX metadata;
- Escrow;
- auctions;
- router;
- cross-chain semantic fixtures.

Фиксируется:

- PROMIS load — `10^6`;
- INTEX origin/wire/target prices — `10^6`;
- payment tokens с decimals `0/6/12/18`;
- settlement использует ceil;
- FX path;
- overflow;
- lock/refund/proceeds;
- WCOEN decimals `6`;
- commit bond в WCOEN units.

ABI widths, body versions и layouts не меняются.

### Коммит T5 — тестовый контракт Emission/native economics

```text
test(native): define six-decimal emission and network economics
```

Меняются:

- независимый Emission reference;
- `day_emission.rs` test section;
- monotonic/full-range tests;
- Metadosis amount fixtures;
- staking/rewards/fees;
- существующие EIP-4895 wire/type fixtures; behavioral withdrawal policy
  повторно замораживается корректирующим T5R ниже;
- genesis/e2e fixtures;
- CLI/MCP amount expectations.

MCP tests отдельно фиксируют обе стороны network boundary:

- Outbe native COEN input, balance и native fee используют `6` decimals;
- BNB/ETH и LayerZero fee на внешней 18-decimal chain сохраняют `18` decimals;
- generic external intent amounts сохраняют существующий token/domain contract и не
  переводятся глобально на scale `1e6`.

MCP Oracle presentation tests проходят через реальный `tools/util.ts::view`
boundary и фиксируют:

- direct `COEN/ISO` reads в обеих допустимых ориентациях используют scale `1e6`;
- canonical COEN/840 VWAP reads используют scale `1e6`, не добавляя reverse
  semantics методам, которые её не поддерживают on-chain;
- `getCoenExchangeRateFor` использует scale `1e6` по semantics самого метода;
- mixed aggregate responses форматируют каждую aligned row по её `base/quote`;
- generic non-ISO Oracle values остаются в существующем scale;
- human-readable presentation никогда не меняет raw integer.

EIP-4895 не является источником COEN в Outbe. `None` и пустой список
withdrawals допустимы и эквивалентны отсутствию withdrawal effects. Любой
non-empty withdrawals list отклоняется на общем Outbe validation seam до EVM
execution и любых state writes независимо от `Withdrawal.amount`. Поле
`Withdrawal.amount: uint64`, payload encoding и withdrawals root не меняются;
денежная Gwei→COEN конверсия не вводится.

### Корректирующий T5R — EIP-4895 rejection contract

Исторический T5 commit `4ddc0e3b` предшествует этому уточнению. До P5 новая
withdrawal policy проходит отдельный test-first recovery:

```text
docs(denomination): refreeze target-scale and withdrawal boundaries
test(native): reject non-empty EIP-4895 withdrawals
test(denomination): record EIP-4895 rejection RED recovery
```

- behavioral tests используют уже существующие Engine API, imported-block и
  executor entrypoints, поэтому компилируются без нового production helper;
- на committed fixed point `b2bd90c9` `None`/empty остаются GREEN, а каждый
  non-empty case обязан дать ожидаемый RED из-за отсутствующего общего reject;
- состав RED фиксируется в
  `testing/denomination/scale6-eip4895-rejection-red.tsv`;
- после фиксации RED expectations не меняются; P5 добавляет общий helper и
  адаптирует entrypoints под этот frozen contract;
- co-located unit tests нового helper добавляются вместе с helper в P5 и не
  заменяют pre-production behavioral RED.

Emission reference задаёт алгоритм в COEN `unit`:

```text
term₀ = INITIAL_DAY_EMISSION

termₖ =
    floor(termₖ₋₁ × K_NUM × day / (K_DEN × k))
```

Чётные terms прибавляются, нечётные вычитаются. Затем применяется emission floor.

Pins генерируются независимым reference, а не придумываются и не копируются механически из старых значений.

### Контрольная точка RED

После T1–T5:

- все новые тесты компилируются;
- старый production-код падает на старых `10^18/10^9`;
- unrelated tests зелёные;
- фиксируется список ожидаемых падений;
- тестовые ожидания замораживаются.

После этого значения тестов нельзя менять, чтобы подогнать их под production implementation. Изменение тестового контракта требует отдельного архитектурного обоснования.

## 6. Production implementation

### Коммит P1 — общая система единиц

```text
refactor(units): establish six-decimal denomination primitives
```

Файлы:

- `crates/blockchain/primitives/src/units.rs`;
- `crates/blockchain/primitives/src/chain.rs`;
- новый общий checked scaled-math helper;
- новый reference-price adapter;
- `math/mod.rs`.

Действия:

- COEN/GRATIS/PROMIS/WCOEN units → `1_000_000`;
- native decimals → `6`;
- добавляются checked floor/ceil;
- добавляется прямой six-decimal price-bin adapter;
- vendor `price_helper.rs` не меняется.

### Коммиты P2 — Oracle COEN/ISO

```text
feat(oracle): convert COEN840 prices and VWAP to six decimals
feat(oracle): extend six-decimal pricing to every COEN ISO market
```

Файлы и области:

- Oracle feeder aggregator/vote builder;
- `oracle/schema.rs`;
- `oracle/runtime.rs`;
- `oracle/state.rs`;
- `oracle/tally.rs`;
- `oracle/api.rs`;
- `oracle/genesis.rs`;
- `oracle/lifecycle.rs`;
- `oracle/scurve.rs`;
- `oracle/openings.rs`.

Действия:

- feeder выдаёт price и COEN-volume в `10^6` для каждого `COEN/ISO`;
- VWAP каждого COEN/ISO возвращает шестизначную цену;
- positive price, округлившийся в zero, отклоняется;
- positive volume, округлившийся в zero, становится `1 unit`;
- реальный zero volume остаётся zero;
- каждый COEN/ISO sentinel → `SCALE_1E6_U256`;
- S-Curve coefficients квантуются через `floor(old / 10^12)`;
- S-Curve result: `floor(peak × coefficient / 1_000_000)`;
- Oracle больше не передаёт ни один COEN/ISO market через старый decimal18
  price-bin contract; сами consumer-файлы NOD/GEM меняются только в P3,
  IntexFactory — только в P4;
- daily S-Curve lifecycle обрабатывает каждый COEN/ISO и пропускает non-ISO
  generic pairs; OCOMP day-type state остаётся только у COEN/840;
- `store_scurve_entry` и genesis import acceptance не ограничиваются этим
  corrective slice и сохраняют существующее поведение;
- non-ISO generic price/VWAP/cross-rate не переписываются.

Corrective P2R после трёхагентного финального аудита и явного уточнения
пользователя:

```text
docs(denomination): refreeze COEN ISO S-Curve eligibility
test(oracle): reject non-ISO markets from daily S-Curve processing
fix(oracle): restrict daily S-Curve processing to COEN ISO
```

- один lifecycle behavioral test сначала фиксирует RED: `COEN/840` и
  `COEN/978` создают peaks, active generic pair — нет;
- production change ограничен classifier guard в
  `crates/system/oracle/src/lifecycle.rs`;
- `store_scurve_entry`, genesis import, ABI, wire/state layouts и S-Curve math
  являются explicit non-goals этого corrective slice.

### Коммит P3 — Tribute/GRATIS/Lysis/NOD/GEM

```text
feat(economics): convert Tribute and GRATIS lifecycle to six decimals
```

Области:

- TEE Tribute compute/process/zk/transport;
- Tribute payload producers в CLI, MCP и operational scripts;
- Gratis state/runtime;
- Gratisfactory runtime;
- Lysis `algorithm.rs`;
- Lysis `program_v1/execute.rs`;
- Lysis `program_v1/phases.rs`;
- NOD state/hooks;
- GEM state/hooks;
- GemFactory runtime.

Действия:

- один TEE-local canonical parser проверяет `amount_base` и `amount_atto` до ветвления ZK/non-ZK и возвращает проверенные `base: u64`, `atto: u64`, `amount_minor: U256`;
- Tribute `base/atto` сохраняют существующие поля и wire shape, но оба пути используют единственную формулу `base × 1_000_000 + atto`;
- CLI, MCP и scripts либо формируют integer `amount_base` плюс fractional `amount_atto`, либо отклоняют неканонический пользовательский ввод; дробный `amount_base` больше не создаётся;
- Gratis monetary amounts → `GRATIS-unit`;
- Gratisfactory убирает исторический разрыв `10^12`;
- Lysis fraction/share/root denominator → `1_000_000`;
- U1024/I256 сохраняются;
- post-normalization выполняется один раз в `compute_fraction_map_from_groups`;
- sequential и OCOMP используют одну семантику;
- Lysis/NOD/GEM costs считают шестизначные monetary values;
- zero-result guards применяются по зафиксированной политике.

### Корректирующий коммит P3R — Credis annual rates

```text
feat(credis): convert interest and reference rates to six decimals
```

Действия:

- Oracle `reference_currency_rate` хранит и возвращает annual rate (scale `1e6`);
- default USD rate меняется с FP18 representation на `36_300`;
- Credis debt formula использует `SCALE_1E6_U256` как rate denominator;
- существующие staged floor, installment split, remainder и repayment FSM
  сохраняются;
- CredisFactory передаёт Oracle rate без дополнительной конверсии;
- types, slots, ABI, wire/state layouts и selectors не меняются.

### Коммит P4 — PROMIS/INTEX/WCOEN

```text
feat(intex): convert PROMIS WCOEN and price wire to six decimals
```

Rust:

- Promis state/runtime;
- PromisFactory;
- Desis schema/runtime/state/OCOMP budget;
- IntexFactory constants/config/runtime/state/qualified/called/schema;
- Intex core schema/api/state/certified.

Solidity:

- `WCOEN.sol`;
- deployment scripts;
- `IntexMetadata.sol`;
- `EscrowAdapter.sol`;
- interfaces;
- auctions/router semantic amount handling.

Действия:

- PROMIS → `10^6`;
- Oracle/INTEX wire divisor `10^9` удаляется;
- `to_wire_price` выполняет только checked `U256 → u64`;
- entry price и PROMIS load дают product scale `10^12`;
- payment conversion централизуется в INTEX helper;
- settlement сохраняет ceil;
- WCOEN decimals guard → `6`;
- commit bond → шестизначные WCOEN units;
- ABI/wire layouts не меняются.

### Коммит P5 — Emission и native economics

```text
feat(native): complete COEN six-decimal cutover
```

Области:

- `crates/system/emissionlimit/src/day_emission.rs`;
- `stablecoin_fork.rs`;
- staking/rewards;
- native fees;
- `crates/blockchain/primitives/src/payload.rs`,
  `crates/blockchain/node/src/{engine,consensus}.rs` и
  `crates/blockchain/evm/src/executor.rs` для единого EIP-4895 rejection seam;
- CLI/operator/OCOMP fee configuration;
- genesis scripts/JSON;
- MCP/CLI formatting;
- Outbe deployment metadata;
- Metadosis fixtures.

Действия:

- Emission Taylor работает непосредственно в COEN `unit`;
- знакопеременные positive/negative sums сохраняются;
- initial/floor amounts → `10^6`;
- stablecoin bond → `1_000_000_000_000 unit`;
- production Metadosis math не меняется;
- Outbe metadata → 6 decimals;
- ETH/BNB/LZ metadata остаются 18;
- MCP chain metadata и native formatting выбирают decimals по network domain:
  Outbe `COEN/6`, BSC `BNB/18`; generic external intent amounts не меняются;
- MCP stake/unstake/AgentReward human-readable COEN inputs преобразуются в
  `unit` через `parseUnits(..., 6)` только на Outbe-native write boundary;
- MCP Oracle presentation получает resolved contract, method arguments и decoded
  result в одном call-context adapter: direct `COEN/ISO`, method-owned
  `getCoenExchangeRateFor` и каждая row mixed aggregate response форматируются
  с scale `1e6`, а generic non-ISO rows сохраняют существующий scale;
- presentation adapter не меняет RPC arguments/results, ABI, raw integers,
  on-chain orientation rules и не вводит pair-decimals metadata;
- production `1 gwei` assumptions для Outbe устраняются;
- gas policy фиксируется в native `unit/gas`;
- один Outbe-owned helper принимает optional withdrawals slice и разрешает
  только `None` или empty; payload-attributes, Engine API block conversion,
  imported-block consensus validation и executor вызывают один interface и
  адаптируют его typed error к своему error domain;
- любой non-empty withdrawals list отклоняется до EVM execution и state writes;
  значение `Withdrawal.amount` не интерпретируется как COEN;
- upstream `post_block_balance_increments` никогда не получает non-empty
  withdrawals в валидном Outbe execution path; Ethereum ommer/DAO increments
  рассчитываются как прежде;
- `Withdrawal.amount: u64`, payload encoding и withdrawals root не меняются;
- genesis полностью переводится на новую модель.
- fresh-network entrypoints сохраняют уже установленный canonical OCOMP epoch
  default `300`; stale `120` в orchestration wrapper устраняется, а retention
  count, job deadlines и consensus semantics не меняются.
- fresh-network prefund CLI называется `--prefund-coen-units`; historical
  `--prefund-wei` удаляется без compatibility alias, поскольку сеть создаётся
  с нуля и этот аргумент является raw COEN-unit boundary.
- `prepare_network.py` передаёт текущему OCOMP key-registration CLI канонические
  validator address и consensus BLS MinPk из того же `validators.json`; это
  исправляет устаревший orchestration adapter, необходимый для fresh genesis,
  и не меняет registration protocol, state или wire shape.

Final audit integration correction inside P5:

- already-approved P3/P4 owners receive one mechanical second pass to replace
  token- or rate-owned aliases of `1_000_000` with a generic numeric scale;
- the correction does not change arithmetic, ABI, wire/state shapes or frozen
  economic expectations;
- MCP classifies only Outbe devnet/testnet as `COEN/6`; BSC remains `BNB/18`
  and every other external EVM/LZ chain retains the pre-cutover `ETH/18`
  fallback instead of being misclassified as Outbe.

## 7. Generated artifacts

```text
chore(denomination): regenerate semantic and genesis artifacts
```

Регенерируются только производные семантические данные:

- Lysis manifest/hashes из frozen T3 cases;
- Lysis semantics hash;
- OCOMP protocol/effect/correctness hashes;
- genesis-final;
- contract metadata, если зависит от decimals.

Canonical Lysis cases и ожидаемые economic outputs создаются независимым
reference и замораживаются в T3 до production implementation. Production
generator не имеет права переписывать эти expectations: после P5 он строит
только derived manifest/hashes. Изменение frozen case является re-plan, а не
artifact regeneration.

Не перегенерируются без причины:

- ABI layouts;
- codec shapes;
- произвольные bit-pattern goldens;
- cryptographic fixtures, не содержащие scoped amounts.

## 8. Финальная проверка

Порядок проверки:

1. Targeted Rust suites.
2. Oracle feeder и Oracle.
3. Solidity WCOEN.
4. Solidity INTEX.
5. Lysis independent-reference parity.
6. Emission full-range monotonic/reference sweep.
7. Fresh genesis build.
8. Fresh network boot.
9. Полный экономический сценарий:

```text
COEN
→ Tribute
→ GRATIS
→ Lysis/NOD
→ GEM
→ PROMIS
→ INTEX
→ WCOEN
→ settlement/refund
```

10. Финальный diff-аудит:

- нет scoped monetary `10^18`;
- нет INTEX `10^9`;
- no token-, price-, or rate-owned aliases duplicate the shared scale `1e6`;
  Rust code uses `SCALE_1E6_U64`, `SCALE_1E6_U128` or `SCALE_1E6_U256`;
- generic Oracle не переведён насильно;
- production Metadosis не переписан;
- типы, ABI и wire/state shapes не изменены;
- внешние ETH/BNB domains остались 18;
- каждый hot file изменён в назначенном production-коммите;
- тестовые ожидания после RED не подгонялись.

11. Source terminology audit:

- all cutover-related source comments, schema documentation, CLI/MCP help and
  user-facing messages are written in English;
- source text does not use opaque cutover shorthand such as `P6`, `T1` or `P1`;
- every affected quantity is named explicitly, for example
  `COEN/ISO rate (scale 1e6)`, `COEN amount in units` or
  `Native economics implementation`;
- the audit is semantic and scoped to cutover text: unrelated protocol names,
  stable code identifiers, artifact filenames and historical commit messages
  are not mechanically renamed.

Итог: вся существующая взаимосвязанная экономика scoped-токенов переводится на шестизначные минимальные единицы так, чтобы ни один downstream-модуль больше не интерпретировал эти суммы или COEN/ISO цены через исторические `10^18`.

## 9. Architecture freeze record

### 9.1. Fixed point и authority

- Ветка: `feat/coen-6-decimals-cutover`.
- Проверенный исходный commit: `c7a2c63e9049db381d0c5280552c33a17e09e326`.
- Единственный implementation contract этой ветки: этот файл и goal, который требует его полной реализации.
- Beads epic: `outbe-chain-0he`; freeze: `outbe-chain-0he.1`; последовательность работ: `outbe-chain-0he.2` … `outbe-chain-0he.14`.
- Старый graph `outbe-chain-y6w` не является authority для этой ветки: несмотря
  на частичное совпадение по Credis rate, он переводит другие независимые
  scales и задаёт другой scope и порядок работ.
- Сеть создаётся с нуля. Старые state и wire values не читаются, dual-scale режим отсутствует, activation/migration/compatibility code не добавляется.

### 9.2. Точный scope и non-goals

В scope входят только:

- COEN amounts, native balances, stake, fees, rewards, bonds и emission;
- GRATIS, PROMIS и WCOEN monetary amounts и metadata;
- Tribute issuance/nominal и canonical `amount_base`/`amount_atto`;
- Gratisfactory stablecoin-to-GRATIS conversion;
- Credis annual ISO currency rate и debt fixed-point denominator;
- Lysis fractions, shares, loads, costs и их sequential/OCOMP parity;
- COEN/ISO feeder price/volume, Oracle vote/tally/reciprocal и consumers;
- COEN/ISO VWAP, zero-volume sentinel и S-Curve; COEN/840 OCOMP day-type;
- NOD, GEM, Desis, INTEX и IntexFactory quantities, prices, price-bin seams и settlement;
- EIP-4895 non-empty-withdrawal rejection before execution/state writes;
- Outbe genesis, CLI/MCP/deployment metadata и denomination-dependent artifacts.

Не входят:

- production math Metadosis; изменяются только его amount/price fixtures и ожидания;
- Fidelity/RCFI algorithmic fixed-point; изменяются только fixtures, поля которых являются реальными GRATIS amounts;
- Oracle reward/slash bands, vote-validity ratios и остальные независимые dimensionless ratios;
- generic non-ISO market redesign и pair-decimals metadata;
- decimals самих stablecoin/ERC20 payment tokens;
- Ethereum, BNB и LayerZero external-native denominations;
- vendor/Pancake `math/price_helper.rs` и его decimal18 contract;
- ABI widths, storage order, codec field order, message/body versions и arbitrary bit-pattern goldens;
- cleanup, renaming или рефакторинг, не требуемый шестизначным cutover.

Если production-код из non-goals оказывается необходим для корректности, работа останавливается по blocker protocol до решения пользователя.

### 9.3. Canonical quantities и type shapes

| Семантика | Canonical value | Существующий тип/shape | Решение |
|---|---:|---|---|
| COEN amount | `1 COEN = 1_000_000 unit` | EVM balance `U256` | тип не меняется |
| GRATIS amount | `1 GRATIS = 1_000_000 GRATIS-unit` | `U256` и существующие encrypted blobs | shape не меняется |
| PROMIS amount | `1 PROMIS = 1_000_000 PROMIS-unit` | `U256`, Intex load `u128` | widths не меняются |
| WCOEN amount | `1 WCOEN = 1_000_000 WCOEN-unit` | Solidity `uint256` | wrap/unwrap raw `1:1` |
| COEN/ISO price | `ISO stable-unit per COEN`, scale `1_000_000` | Oracle `U256`; INTEX wire `u64` | без промежуточного `10^9` |
| COEN/ISO volume | COEN `unit`, scale `1_000_000` | Oracle vote/snapshot `U256` | zero остаётся zero |
| Credis annual ISO rate | `1.0 = 1_000_000` | Oracle mapping и Credis `U256` | отдельный semantic scale, shape не меняется |
| S-Curve coefficient | `1.0 = 1_000_000` | существующий `U256` table | `floor(old / 10^12)` |
| Lysis fraction/share/root | `1.0 = 1_000_000` | существующие `U256`; U1024/I256 internals | типы сохраняются |
| Tribute base | whole unsigned integer | encrypted JSON `String`, ZK `u64` | canonical lexical `u64` |
| Tribute atto | `0..999_999` | encrypted JSON `String`, ZK `u64` | имя/позиция сохраняются |
| Tribute issuance/nominal | COEN `unit` | `U256` state/receipt | shape не меняется |
| INTEX prices | ISO reference stable-units, scale `1_000_000` | Rust `U256`, wire/Solidity `uint64` | checked narrowing only |
| INTEX bid/clearing rate | `1.0 = 1_000_000` | существующий `u64/uint64` | dimensionless, без изменения |
| External settlement amount | raw payment-token units | `U256/uint256` | decimals `0/6/12/18` читаются у token |
| Emission | COEN `unit` | `U256` | Taylor terms сразу amount-domain |
| EIP-4895 withdrawals | field shape remains Ethereum-compatible | existing optional list of `Withdrawal { amount: u64, ... }` | `None`/empty allowed; non-empty rejected, no COEN conversion |
| Gas price | COEN `unit` per gas | EVM/RPC existing integers | no gwei floor |

Независимый `SCALE_1E18` остаётся только там, где поле действительно является
существующим dimensionless contract, либо принадлежит сохранённому non-ISO
generic Oracle market: Oracle reward/validity ratios, Fidelity/RCFI,
non-ISO generic rates и неизменяемый vendor price helper. Ни Emission,
ни COEN/ISO prices, ни S-Curve coefficients, ни Lysis fractions не используют
его после cutover.

Canonical public/state shapes фиксируются без изменений:

- `TributeInputPayload.amount_base: String`, `amount_atto: String`; ZK positions `base: u64`, `atto: u64` остаются прежними;
- Oracle rate/volume/VWAP/S-Curve storage остаётся в существующих `U256` slots; нового decimals field нет;
- Lysis inputs/actions/results и OCOMP codecs сохраняют существующие поля и widths;
- `ReferenceCurrencyPrice` сохраняет `uint64` price fields, `promisLoadMinor` сохраняет `uint128`;
- WCOEN ERC20 balances/allowances остаются `uint256`;
- `Withdrawal.amount` остаётся структурно существующим EIP-4895 `uint64` field;
  `withdrawals_root` и payload layout не меняются, но любой non-empty list
  является invalid Outbe payload.

### 9.4. Формулы, rounding и zero-result policy

| Владелец | Canonical formula | Rounding/error |
|---|---|---|
| Tribute | `amount = checked(base × 1_000_000 + atto)` | non-canonical base/atto и overflow reject |
| Tribute nominal для COEN/ISO | `floor(amount × 1_000_000 / tribute_price)` | positive amount/price с zero result reject |
| Gratisfactory | `ceil(stable_raw × 1_000_000 / coen_iso_price)` | collateral rounds in protocol's favour; overflow reject |
| Credis debt | `term_rate=floor(rate_units×months/12)`; `debt=floor(principal_units×(1_000_000+term_rate)/1_000_000)` | rate scale `1e6`; два существующих floor сохраняются; overflow/invalid amount reject до записи |
| Lysis load | `floor(nominal × fraction / 1_000_000)` | zero load для positive obligation reject |
| Lysis normalization | `projected = Σ floor(group_nominal × fraction / 1_000_000)`; если `projected > allocation`, `fraction = floor(fraction × allocation / projected)` | один owner seam, dust разрешён |
| Lysis/NOD/GEM cost | `floor(entry_price × load / 1_000_000)` | positive price/load с zero cost reject |
| COEN/ISO VWAP | `floor(Σ(price × volume) / Σvolume)` | checked products/sums; zero total volume follows existing no-data path; non-ISO generic VWAP сохраняет существующий contract |
| COEN/ISO reciprocal | `floor(1_000_000² / rate)` | zero rate reject; non-ISO generic reciprocal не меняется |
| COEN/ISO S-Curve | `floor(peak × coefficient / 1_000_000)` | daily lifecycle skips non-ISO generic pairs; overflow keeps existing deterministic zero/error policy |
| INTEX reference settlement | `ceil(price_units × promis_units × 10^d / 10^12)` | оба входа scale `1e6`; `d=0/6/12/18`, checked, one rounding |
| INTEX FX settlement | existing price ratio cancels common COEN/ISO scale; external decimals applied once | ceil, overflow/unsupported decimals reject |
| Emission | `term₀=INITIAL`; `termₖ=floor(termₖ₋₁×K_NUM×day/(K_DEN×k))`; even add, odd subtract | clamp to floor; monotonic reference sweep |
| EIP-4895 | `withdrawals ∈ {None, Some([])}` | любой non-empty list reject до execution/state write; amount не конвертируется |
| Native fee | `gas_used × effective_gas_price` | checked existing EVM accounting; raw `unit/gas` |

COEN/ISO feeder boundary:

- positive finite provider price that maps below one price unit is rejected;
- positive finite volume that maps below one COEN unit becomes `1 unit`;
- actual zero volume stays zero;
- every COEN/ISO zero-volume fallback sentinel becomes `SCALE_1E6_U256`;
- provider parsing and aggregation must be deterministic; `f64` may remain at provider ingestion only where existing interfaces require it, but expected integer outputs are frozen by decimal-string/reference tests.

Native gas policy:

- `MIN_PROTOCOL_BASE_FEE` remains the canonical raw `unit/gas` minimum;
- CLI/operator remove the historical `1 gwei` minimum and buffer from the protocol minimum instead;
- OCOMP signer cap is derived from the same owner and fixed to `2 × MIN_PROTOCOL_BASE_FEE` unless an existing lower protocol constraint rejects it;
- Ethereum/BNB client paths keep their external-chain decimal metadata.

### 9.5. Invariant ownership

| Инвариант | Единственный production owner |
|---|---|
| Token units, native decimals, checked floor/ceil | `crates/blockchain/primitives/src/units.rs` и bounded `math/scaled_math.rs` |
| COEN/ISO market classification и ↔ Q128.128 | `crates/blockchain/primitives/src/math/reference_price.rs` |
| Generic LB decimal18 port | существующий `math/price_helper.rs`, read-only |
| Feeder COEN/ISO integer normalization | `bin/outbe-feeder/src/aggregator.rs` |
| COEN/ISO vote/spot/VWAP/sentinel/reciprocal/S-Curve; COEN/840 OCOMP day-type | `crates/system/oracle` |
| Tribute canonicalization и nominal | `bin/outbe-tee-enclave/src/compute.rs`; process/ZK только потребляют результат |
| GRATIS/PROMIS metadata | соответствующий `state.rs` каждого token module |
| Stablecoin → GRATIS conversion | `crates/core/gratisfactory/src/runtime.rs` |
| Credis annual-rate denominator и staged debt rounding | `crates/core/credis/src/runtime.rs`; scale constant в `crates/blockchain/primitives/src/units.rs` |
| Oracle ISO annual-rate storage/default | `crates/system/oracle` reference-currency state |
| Lysis fractions/normalization/load/cost | `crates/core/lysis`; sequential и phases вызывают один semantic seam |
| NOD/GEM price bins и costs | NOD/GEM/GemFactory domain files в P3 |
| PROMIS load и INTEX wire/settlement | Desis/Intex/IntexFactory domain files в P4 |
| WCOEN decimals и raw wrapping | `contracts/tokens/src/native/WCOEN.sol` |
| EIP-4895 rejection policy | один helper/interface в `crates/blockchain/primitives/src/payload.rs`; Engine API, consensus и executor являются adapters этого seam |
| Emission amount-domain Taylor | `crates/system/emissionlimit/src/day_emission.rs` |
| Native fee floors | shared protocol minimum плюс CLI/operator/OCOMP adapters |
| Fresh-network values | `scripts/seed_genesis.py`, `scripts/prepare_network.py` и canonical wrapper `scripts/bootstrap-testnet.sh`; generated JSON не редактируется вручную |
| MCP Oracle presentation scale | `mcp/src/format.ts`; `mcp/src/tools/util.ts` только передаёт resolved Oracle/method/argument context |
| Lysis/OCOMP hashes | independent Lysis reference checker и standard `xtask ocomp final-artifacts` generator |

### 9.6. Transition matrix

| State | Event | Relevant actor status | Effect | Error/no-effect | Replay | Restart | Deadline behaviour |
|---|---|---|---|---|---|---|---|
| Fresh genesis | network bootstrap | configured validator/account | balances and stakes use token units with 6 decimals; Oracle COEN/840 rates use scale `1e6`; token metadata загружается один раз | malformed/out-of-range seed rejects genesis | same inputs generate byte-identical output | raw state reloads unchanged | existing genesis timestamp rules unchanged |
| Native account | transfer/paid tx | funded signer | debit value+fee and credit raw COEN units | insufficient/overflow rejects before partial commit | tx replay rules unchanged | balances persist exactly | EIP-1559 timing unchanged |
| Staking/reward/bond | stake, claim, slash, create stablecoin | existing eligibility rules | COEN amounts are conserved in COEN-units (`1 COEN = 1_000_000 unit`) | existing authorization/funds errors | nonce/event replay unchanged | queues/liabilities reload raw | unbonding and claim deadlines unchanged |
| Execution payload | EIP-4895 withdrawals | payload attributes или imported block | `None`/empty не создают effect | любой non-empty list отклоняется до EVM/state writes | одинаковый payload получает одинаковый verdict | после restart policy неизменна, withdrawal state отсутствует | post-transaction ordering не достигается для invalid payload |
| Oracle voting | provider aggregate → vote → tally | registered feeder/validator | every COEN/ISO spot rate, COEN volume, VWAP and zero-volume sentinel uses scale `1e6`; non-ISO markets keep their price/VWAP contract | invalid price, overflow and existing vote errors produce no partial update | same ballot gives same result | snapshots reload raw | vote/day windows unchanged |
| Oracle S-Curve | UTC day boundary | registered active vote-target pair | every COEN/ISO pair is processed; non-ISO generic pair has no S-Curve effect; COEN/840 remains the OCOMP day-type pair | existing processing/storage error aborts the block exactly as before | same closed-day snapshots give the same peak decision | stored entries and last-processed watermark reload unchanged | existing UTC boundary and expiry windows unchanged |
| Oracle opening | VWAP/S-Curve read | existing opening state | COEN/ISO nominal is `max(VWAP,S-Curve)` in its ISO stable-units | no-data/zero follows existing error contract | opening proof remains deterministic | stored values unchanged | WorldwideDay and active-S-Curve windows unchanged |
| Tribute offering | encrypted offer without/with ZK | existing offering and ZK eligibility | both paths parse one canonical base/atto and emit identical issuance/nominal | lexical, atto-bound, zero, price or proof error rejects before state | duplicate-id rules unchanged | stored Tribute reloads raw | OFFERING deadline unchanged |
| Gratisfactory | pledge/mine/unpledge | existing authorization/eligibility | stable raw amount converts once to GRATIS units with ceil | cap, oracle, overflow and auth errors leave state unchanged | ticket/nonce replay unchanged | tickets and totals persist | existing pledge lifecycle unchanged |
| Credis | request, repay, expire | valid pledge and ISO rate | annual rate (scale `1e6`) is pinned in position; staged scale-`1e6` debt calculation and installments conserve total | zero/missing rate, overflow and existing state errors write nothing | existing position/payment replay rules unchanged | rate, debt and schedule reload raw | existing monthly schedule unchanged |
| Lysis sequential | finalize allocation | eligible Tribute set | fractions, loads and costs respect allocation with bounded dust | zero/overflow/budget errors abort | same inputs give same actions | results persist raw | existing Lysis boundary unchanged |
| Lysis OCOMP | plan, phases, reduce, certify, materialize | accepted worker/quorum | byte-equivalent economics to sequential path | invalid receipt/root/amount rejected | receipt/adoption replay rules unchanged | certified artifacts/NOD reload identically | leases and terminal deadlines unchanged |
| NOD/GEM | issue/qualify/settle | existing owner/qualification | price-bin IDs and monetary fields derive from the price-scale-`1e6` adapter | zero/overflow/existing auth errors reject | IDs remain deterministic | buckets/items persist | qualification/settlement windows unchanged |
| Desis/INTEX | brief, bridge, auction, settle/refund | existing day/auction status | rates and PROMIS load use scale `1e6` across the existing wire; payment conversion occurs once | unsupported decimals/overflow/state error rejects | message/replay guards unchanged | Rust/Solidity state agrees after restart | commit/reveal/call windows unchanged |
| WCOEN | deposit/withdraw | holder with native/token balance | exact raw 1:1 mint/burn and native transfer | insufficient balance/transfer failure reverts atomically | ERC20 allowance/nonce semantics unchanged | supply/backing persist | no new deadline |
| Emission | query/allocate day limit | valid day | monotonic daily amount in COEN-units, clamped at floor | arithmetic is deterministic and bounded | same day same result | no hidden accumulator | day `>=2920` returns floor |
| MCP Oracle view | direct, method-owned or aggregate read | resolved Oracle contract and valid ABI arguments | COEN/ISO values render at scale `1e6` per pair/row; generic values retain their scale; raw integers are unchanged | invalid arguments/address and existing RPC errors propagate unchanged | read-only result is deterministic | no MCP state | no deadline |

### 9.7. Разрешённый production file map

Файл вне этого списка, потребовавший semantic production change, является blocker.

P1:

- `crates/blockchain/primitives/src/units.rs`;
- `crates/blockchain/primitives/src/chain.rs`;
- `crates/blockchain/primitives/src/math/mod.rs`;
- новый `crates/blockchain/primitives/src/math/scaled_math.rs`;
- новый `crates/blockchain/primitives/src/math/reference_price.rs`.

P2:

- `crates/blockchain/primitives/src/units.rs` и
  `crates/blockchain/primitives/src/math/reference_price.rs` — единственный
  integration-correction pass: `COEN840` owner names заменяются на `COEN_ISO`,
  добавляется structural market classifier без pair metadata;
- `bin/outbe-feeder/src/aggregator.rs`;
- `bin/outbe-feeder/src/vote_builder.rs`;
- `crates/system/oracle/src/{api,constants,genesis,openings,runtime,schema,scurve,state,tally}.rs`;
- `contracts/precompiles/src/IOracle.sol` — comments/declared units only;
- `crates/system/rewards/src/api.rs` — Oracle test-support literals/comments only, не reward production math.

Corrective P2R после финального аудита:

- `crates/system/oracle/src/lifecycle.rs` — единственный production hot file;
  daily S-Curve iteration добавляет structural `COEN/ISO` classifier guard;
- `crates/system/oracle/src/tests/lifecycle.rs` — lifecycle behavioral RED/GREEN;
- `plan_6.md` и `testing/denomination/{scale6-red-manifest.tsv,scale6-coverage-ledger.tsv}` —
  только синхронизация user-corrected invariant/evidence;
- `scurve.rs`, `genesis.rs`, store/import acceptance, ABI и state layout остаются
  read-only в этом slice.

P3:

- `bin/outbe-tee-enclave/src/{compute,payload,process,zk_claim}.rs`;
- `bin/outbe-cli/src/commands/tribute.rs`, `mcp/src/{crypto,tools/sign}.ts`, `scripts/tribute_offer.py`, `scripts/tributefactory/offer_tribute.py`;
- `crates/core/gratis/src/state.rs`;
- `crates/core/gratisfactory/src/runtime.rs`;
- `crates/core/lysis/src/algorithm.rs`;
- `crates/core/lysis/src/program_v1/{execute,phases,types}.rs`;
- `crates/core/lysis/src/runtime.rs` — unit comments/interface adaptation only;
- `crates/core/nod/src/{schema,state}.rs`;
- `crates/core/gem/src/state.rs`;
- `crates/core/gemfactory/src/runtime.rs`.
- `crates/system/tee/src/protocol.rs` — только unit comment для
  `PledgeTerms.entry_rate`; field shape/serde/codec не меняются.

P3R, user-approved Credis-rate correction:

- `crates/blockchain/primitives/src/units.rs` — shared typed constants
  `SCALE_1E6_U64`, `SCALE_1E6_U128` and `SCALE_1E6_U256`;
- `crates/system/oracle/src/{api,constants,genesis,schema,state}.rs` —
  reference-currency annual rate values/comments only;
- `crates/core/credis/src/{runtime,schema}.rs` — denominator/comments only;
- `crates/core/credisfactory/src/runtime.rs` — comments only if required;
- `contracts/precompiles/src/{IOracle,ICredis}.sol` — declared units/comments only.

`crates/system/oracle/src/precompile.rs` и `crates/core/credis/src/precompile.rs`
остаются raw `U256` pass-through и semantic production changes не требуют.

User-approved P3 re-plan after the `ZERO_COST` blocker: `types.rs` разрешён
только для внутреннего `ProgramErrorV1::ZeroCost { ordinal }`. Это расширяет
typed Rust error surface, но не меняет ABI, wire/state layouts, OCOMP codecs,
frozen vectors или экономическую семантику P3.

Superseded re-plan: `nod/hooks.rs` и `gem/hooks.rs` были добавлены из-за
ошибочного предположения, что non-840 ISO markets остаются FP18. Пользователь
уточнил, что все ISO reference currencies принадлежат шестизначным stablecoin
domain. Эти hooks снова вне P3 production map и не меняются; NOD/GEM получают
price scale `1e6` для любого валидного `reference_currency`.

P4:

- `crates/core/promis/src/state.rs`;
- `crates/core/promisfactory/src/runtime.rs` only if a scale assumption is proven by T4;
- `crates/core/desis/src/{schema,runtime,state,ocomp_budget}.rs`;
- `crates/core/intexfactory/src/{called,config,constants,qualified,runtime,schema,state}.rs`;
- `crates/core/intex/src/{api,certified,schema,state}.rs` only where semantic amounts/comments are owned;
- `contracts/tokens/src/native/WCOEN.sol`;
- `contracts/tokens/script/wcoen/WCOENDeploy.s.sol`;
- `contracts/intex/src/shared/libs/IntexMetadata.sol`;
- `contracts/intex/src/shared/interfaces/IIntexNFT1155.sol`;
- `contracts/intex/src/origin/interfaces/IOriginRouter.sol`;
- `contracts/intex/src/target/{EscrowAdapter,IntexAuction}.sol`;
- `contracts/intex/src/target/interfaces/{IEscrowAdapter,IIntexAuction}.sol`;
- `contracts/intex/scripts/shared/chains.ts`;
- `mcp/src/intex/registry.ts`, `mcp/src/tools/intex.ts`.

P5:

- `crates/system/emissionlimit/src/day_emission.rs`;
- `crates/blockchain/primitives/src/stablecoin_fork.rs`;
- `crates/blockchain/primitives/src/payload.rs` — единственный
  `None`/empty-only withdrawals validation interface;
- `crates/blockchain/node/src/{engine,consensus}.rs` — Engine API и imported
  block adapters этого interface;
- `crates/blockchain/evm/src/executor.rs`;
- `bin/outbe-cli/src/{commands/mod,tx}.rs` и denomination-dependent command output;
- `crates/blockchain/operator/src/tx.rs`;
- `bin/outbe-ocomp/src/vote_submitter.rs`;
- `mcp/src/{chain,format}.ts`, `mcp/src/intent/format.ts`;
- `mcp/src/tools/util.ts` — единственный call-context adapter для Oracle
  presentation scale; только resolved contract/method/arguments/result context,
  без изменения RPC, ABI, coercion, raw values или error behavior;
- `mcp/src/tools/sign.ts` только для Outbe-native stake/unstake/AgentReward
  input conversion;
- `mcp/src/tools/intent.ts` только для network-aware native decimals; ERC-20
  decimals, generic external intent amounts и 18-decimal BNB/ETH paths не меняются;
- `scripts/seed_genesis.py` для six-decimal native amounts и reference rates;
- `scripts/prepare_network.py` для COEN-unit prefund, canonical `300`-block
  epoch и current OCOMP key-registration CLI adapter, необходимого для fresh
  genesis generation; `scripts/bootstrap-testnet.sh` only for the same epoch
  default already owned by E2E and the retained Final fixture;
- `mcp/README.md` для public six-decimal COEN input contract;
- final-audit integration correction, one mechanical second pass only:
  `crates/core/lysis/src/algorithm.rs`,
  `crates/core/desis/src/{constants,runtime}.rs`,
  `crates/core/intexfactory/src/{constants,config}.rs`,
  `contracts/intex/src/shared/libs/{BridgeMsgCodec,IntexMetadata}.sol`,
  `contracts/intex/src/target/{IntexAuction.sol,interfaces/IIntexAuction.sol}`,
  matching INTEX unit tests, and `mcp/src/{chain.ts,intex/bid.ts,tools/intex.ts}`;
- `contracts/intex/scripts/shared/chains.ts` только один раз в P4; P5 его повторно не меняет;
- Metadosis production files read-only.

Final-review T3/E2E integration correction (second and final semantic pass):

- `bin/outbe-cli/src/commands/tribute.rs` exposes the already-frozen
  `amount_atto` payload field as a canonical `--amount-atto` argument; no
  payload/wire/state field is added or renamed;
- `testing/e2e-harness/src/{world/{rpc,ocomp},features/{ocomp,tribute_projection}}.rs`
  passes the canonical pair `amount_base="0"`, `amount_atto="6"` instead of
  encoding a decimal fraction inside `amount_base`;
- only E2E compilation and static/unit tests are run here; live E2E execution
  remains the user-owned final gate.

### 9.8. Тесты, references, vectors и generated evidence map

T1:

- inline tests `crates/blockchain/primitives/src/{units,stablecoin_fork}.rs`;
- `crates/blockchain/primitives/tests/stablecoin_abi_vectors.rs` и `testdata/stablecoin/v1/*` только semantic bond values;
- `crates/core/{gratis,promis}/src/tests.rs`;
- `crates/system/staking/src/tests.rs`, `crates/system/rewards/src/constants.rs` test section и `crates/system/rewards/tests/economics.rs` с реальными COEN amounts;
- `contracts/tokens/test/native/WCOEN.t.sol`;
- `contracts/intex/test/mocks/MockWCOEN.sol` и `EscrowAdapter.decimals.t.sol`.

T2:

- inline tests `crates/blockchain/primitives/src/{units,math/reference_price}.rs`
  для COEN/ISO classification и price-scale-`1e6` adapter;
- inline feeder tests `bin/outbe-feeder/src/{aggregator,vote_builder}.rs`;
- `crates/system/oracle/src/scurve.rs` test section;
- новый `crates/system/oracle/testdata/coen840-scurve-v1.json` с 128 canonical coefficients и product pins;
- `crates/system/oracle/src/tests/{common,e2e,lifecycle,state}.rs`;
- Oracle-facing tests in `crates/blockchain/{evm,txpool}` and `crates/system/rewards/src/api.rs` where literals represent COEN/ISO;
- Oracle-facing test sections in `crates/blockchain/node/src/payload_builder.rs`
  and `crates/blockchain/evm/tests/compressed_scope_wiring.rs` where literals
  represent COEN/ISO;
- `crates/core/{nod,gem,intexfactory}/src/*tests*` for price-bin expectations derived from rates at scale `1e6`;
- `crates/system/oracle/tests/ocomp_openings.rs`, если файл существует at T2 audit; отсутствие не создаёт новый файл без public-behaviour case.

T3:

- inline/unit/integration tests under `bin/outbe-tee-enclave/{src,tests,benches}` that carry Tribute amounts;
- `crates/core/{tribute,tributefactory,gratis,gratisfactory}/src/tests.rs` and existing integration tests;
- `crates/core/credisfactory/src/tests/e2e.rs` as consumer coverage: COEN/ISO
  entry price, GRATIS monetary fixtures и Credis annual rate используют scale `1e6`;
- test section `bin/outbe-tee-enclave/src/gratis.rs` only for
  `PledgeTerms.entry_rate` fixtures; arbitrary KAT/layout values remain unchanged;
- `crates/core/lysis/src/tests.rs`, `crates/core/lysis/tests/{planner_reducer_vectors,program_v1_reference}.rs`;
- `testing/lysis-v1-reference/src/{lib,main}.rs`;
- `crates/core/lysis/vectors/lysis-v1/{cases.jsonl,manifest.json}`;
- `crates/core/{nod,gem,gemfactory}/src/*tests*` and OCOMP/NOD materialization tests carrying monetary values;
- `crates/core/fidelity/reference/decay.py`, `crates/core/fidelity/tests/fixtures/rcfi_golden.json`, `bin/outbe-tee-enclave/src/fidelity.rs` test section — amount fields only, RCFI math unchanged;
- `outbe-plan/off-chain-poc-lysis-v1-semantics.md`.

T3R, Credis annual rate (scale `1e6`):

- `crates/blockchain/primitives/src/units.rs` tests;
- `crates/system/oracle/src/tests/{common,e2e,state}.rs`;
- `crates/core/credis/src/tests.rs` — exact staged-rounding vectors, zero rate,
  positive rate with zero interest, overflow and stored raw rate;
- `crates/core/credisfactory/src/tests/e2e.rs` — complete
  pledge → request → installments → zero lifecycle;
- `scripts/test_seed_genesis_protocol_constants.py` — default USD rate `36_300`
  in the existing mapping slot;
- `mcp/src/denomination.test.ts` — method/owner-aware formatting of Credis and
  Oracle annual rates with 6 decimals while generic Oracle rate stays unchanged;
- `testing/denomination/scale6-credis-rate-red.tsv`.

T4:

- `crates/core/{promis,promisfactory,desis,intex,intexfactory}/src/tests.rs` and `intex/src/certified.rs` fixtures;
- semantically affected `contracts/intex/test/foundry/**/*.t.sol`, especially auction, bond, metadata, local-loopback, lock parity, router/reentrancy and body validation cases;
- `contracts/tokens/test/native/WCOEN.t.sol`;
- structural `BridgeMsgCodecGolden.t.sol` arbitrary bit patterns stay unchanged;
- ABI exports are evidence-only unless a generator proves a denomination-dependent comment/value changed.

T5:

- новый независимый `testing/emission-reference/reference.py` и его `testing/emission-reference/vectors.json`; production не генерирует собственные pins;
- Emission pins `0,1,365,730,1460,2190,2919,2920` plus full monotonic sweep;
- native amount/fee fixtures in CLI/operator/txpool/staking/rewards/stablecoin tests;
- `crates/blockchain/node/tests/fee_history_system_gas.rs` and
  `testing/e2e/tests/update_flow_spec.rs` COEN/ISO/native fixtures;
- `mcp/src/denomination.test.ts`: Outbe native input/output uses 6 decimals,
  BSC native uses 18 decimals, generic external intent representation remains unchanged;
- `mcp/src/denomination.test.ts`: public `tools/util.ts::view` path covers
  COEN/ISO direct and reverse spot reads, canonical COEN/840 VWAP, method-owned
  `getCoenExchangeRateFor`, mixed aggregate rows, generic non-ISO rows,
  invalid ISO-like addresses and unchanged raw integers;
- `testing/denomination/scale6-mcp-oracle-view-red.tsv` records the corrected
  MCP view contract failing against the pre-implementation production path;
- Metadosis lifecycle fixtures only;
- `scripts/test_seed_genesis_protocol_constants.py`, `scripts/tests/test_prepare_network.py`;
  the latter proves both fresh-network entrypoints emit the canonical
  `epochLengthBlocks = 300` default without changing OCOMP retention/deadlines;
- `crates/blockchain/node/tests/assets/genesis.json`, `release/testnet-genesis.json`, seed profiles и E2E fixtures as generated expectations.

T5R, EIP-4895 rejection:

- pre-production behavioral tests в existing node/EVM test surfaces покрывают
  payload attributes, Engine API payload conversion, imported-block consensus
  validation и executor entrypoint без зависимости от нового helper;
- P5 co-located unit tests в `crates/blockchain/primitives/src/payload.rs`
  покрывают `None`, empty и non-empty list для единственного owner interface;
- каждый adapter test требует одинаковый pre-state reject для любого non-empty
  amount, включая `0`, `1`, `1_000` и `u64::MAX`;
- `testing/denomination/scale6-eip4895-rejection-red.tsv` фиксирует commands,
  actual stale behavior на `b2bd90c9`, expected rejection и последующий GREEN.

Generated artifacts after P5:

- `crates/system/ocomp-protocol/registry/semantic-artifacts-v1.tsv`;
- generated OCOMP registry/correctness outputs selected by
  `xtask ocomp final-artifacts`;
- Lysis manifest/hashes, полученные из неизменённых frozen T3 cases;
- fresh genesis/release/E2E generated JSON;
- denomination-dependent contract metadata.
- reference-currency slot changes in fresh genesis outputs and dependent OCOMP
  network/fork bindings; ABI/codec/correctness shape artifacts remain unchanged.

Generated shape registries, codec shape vectors and arbitrary bit-pattern goldens remain unchanged unless the generator produces a byte-identical rewrite.

RED evidence хранится в `testing/denomination/scale6-red-manifest.tsv`. Финальный requirement-to-evidence mapping хранится в `testing/denomination/scale6-coverage-ledger.tsv`. Это evidence artifacts, а не параллельный task tracker; статусы работ остаются только в Beads.

Пропущенный T3 consumer path фиксируется в
`testing/denomination/scale6-late-red.tsv`: corrected tests запускаются против
pre-P3 commit `181d3fd2`, actual stale-production results записываются как RED,
после чего те же tests обязаны быть GREEN на текущей ветке. Исходный RED
manifest и история commits не переписываются.

### 9.9. Hot files и semantic-pass budget

| Hot file/family | Pass 1 | Pass 2 | Третий pass |
|---|---|---|---|
| `units.rs` и shared math interfaces | P1 units/helpers | одна integration correction, если gate доказал необходимость | blocker |
| inline test+production files (`scurve.rs`, `tally.rs`, `executor.rs`, `day_emission.rs`) | T-stage test section | owning P-stage production | blocker |
| dedicated production files | owning P-stage | одна integration correction, если gate доказал необходимость | blocker |
| `mcp/src/tools/sign.ts` | P3 Tribute canonicalization | user-approved P5 native COEN input conversion | blocker |
| dedicated tests/references/vectors | owning T-stage | исправление test plumbing без изменения frozen expected semantics | blocker |
| `plan_6.md` | architecture freeze | только user-approved re-plan после blocker | blocker |
| generators | P5 source/config | generated-artifact run | blocker для третьей semantic правки |

User-approved Credis-rate exception to the pass budget:

- `units.rs` получает один user-approved integration-correction pass только
  для shared typed `SCALE_1E6_*`; дальнейший semantic pass запрещён;
- Oracle reference-currency files получают один узкий pass только для annual
  rate representation; COEN/ISO market math повторно не меняется;
- `mcp/src/format.ts` и `scripts/seed_genesis.py` включают annual rate в текущий
  незакоммиченный P5 pass;
- Credis runtime/schema получают первый semantic pass;
- любые дальнейшие изменения этих seams снова являются blocker.

User-approved MCP Oracle presentation exception after the completed blocker
protocol:

- `mcp/src/tools/util.ts` получает один semantic pass только как centralized
  Oracle call-context adapter;
- `mcp/src/format.ts` завершает свой текущий незакоммиченный native-economics
  pass method/pair/row-aware presentation logic;
- `mcp/src/denomination.test.ts` получает один финальный test-contract pass:
  commit `76d851c1` считается diagnostic helper probe и не заменяет public
  `view()` coverage;
- любое дальнейшее расширение MCP production paths или semantic изменение
  formatter contract снова является blocker.

Форматирование, generated byte-for-byte rewrite и механическое обновление imports не считаются semantic pass. Изменение expected value после RED всегда считается semantic re-plan, а не integration correction.

User-approved exception after the completed blocker protocol: the rejected
`840 vs non-840 ISO` model was recorded in an intermediate docs commit, but no
production code was committed under it. The next architecture commit supersedes
that model with the canonical `all COEN/ISO prices use scale 1e6` invariant before P3 is
committed. No NOD/GEM hook production change is permitted by the rejected model.

User-approved P2R exception after the three-agent final audit:

- `plan_6.md` receives the required stop-and-replan correction from
  `COEN/840-only` to `all COEN/ISO` VWAP/sentinel/S-Curve eligibility;
- previously untouched `crates/system/oracle/src/lifecycle.rs` receives one
  semantic pass limited to filtering daily S-Curve work by
  `is_coen_iso_market`;
- no pass is granted to `scurve.rs`, `genesis.rs`, store/import acceptance or
  any canonical shape.

### 9.10. Commit map и evidence gates

Stacked delivery имеет две PR boundaries, совпадающие с существующей историей
ветки и не требующие её переписывания:

**PR A — architecture freeze, GREEN**

1. `1925c6b6 docs(denomination): freeze six-decimal cutover architecture`.

PR A не меняет production или tests. User-approved re-plans, появившиеся после
stop-triggers, фиксируются отдельными docs commits уже в PR B.

**PR B — atomic six-decimal cutover, финальный GREEN**

1. `62d2798d test(denomination): define six-decimal token unit contract`;
2. `62205ec5 test(oracle): define six-decimal COEN840 price contract`;
3. `3e4d8c83 test(economics): define Tribute-to-Lysis six-decimal behavior`;
4. `a44bc2ee test(intex): define six-decimal PROMIS and WCOEN lifecycle`;
5. `4ddc0e3b test(native): define six-decimal emission and network economics`;
6. `b661b09b test(denomination): freeze expected scale6 red manifest`;
7. `08328b33 refactor(units): establish six-decimal denomination primitives`;
8. `bfa5fb9e feat(oracle): convert COEN840 prices and VWAP to six decimals`;
9. `352bee3b docs(denomination): extend P3 zero-cost error ownership`;
10. `183497a4 docs(denomination): make P3 price bins currency-aware` — superseded by
    the user correction below; code was not committed under this model;
11. `45771576 docs(denomination): classify every COEN ISO market as scale6`;
12. `d4f3685b test(oracle): extend six-decimal contract to every COEN ISO market`;
13. `181d3fd2 feat(oracle): extend six-decimal pricing to every COEN ISO market`;
14. `029a16a9 feat(economics): convert Tribute and GRATIS lifecycle to six decimals`;
15. `aa5575ef feat(intex): convert PROMIS WCOEN and price wire to six decimals`;
16. `2b09e814 docs(denomination): freeze MCP native network boundary`;
17. `eb174a4e test(native): cover MCP native network boundary`;
18. `a144d9c1 docs(denomination): add omitted GRATIS CREDIS boundary`;
19. `48902e54 docs(denomination): refreeze CREDIS rates at six decimals`;
20. `eb238e12 test(credis): define six-decimal GRATIS and interest contract`;
21. `f383aa2b test(denomination): record CREDIS rate RED recovery`;
22. `9ba9f122 feat(credis): convert interest and reference rates to six decimals`;
23. `62acc711 refactor(units): use typed six-decimal scale constants`;
24. `76d851c1 test(mcp): cover six-decimal COEN ISO view rates` — diagnostic helper
    probe, insufficient as production wiring evidence;
25. `acb2e493 docs(denomination): refreeze MCP Oracle view presentation`;
26. `ef0680c0 test(mcp): define context-aware Oracle view formatting`;
27. `b2bd90c9 test(denomination): record MCP Oracle view RED recovery`;
28. `docs(denomination): refreeze target-scale, withdrawal and fresh-network boundaries`;
29. `test(native): reject non-empty EIP-4895 withdrawals`;
30. `test(denomination): record EIP-4895 rejection RED recovery`;
31. `feat(native): complete COEN six-decimal cutover`;
32. `chore(denomination): regenerate semantic and genesis artifacts`.
33. `docs(denomination): refreeze COEN ISO S-Curve eligibility`;
34. `test(oracle): reject non-ISO markets from daily S-Curve processing`;
35. `fix(oracle): restrict daily S-Curve processing to COEN ISO`.

T1–T5 и RED commits внутри PR B намеренно не являются mergeable PR
boundaries. Ветка становится mergeable только после P2–P5, generated
artifacts и полного GREEN. Отдельный foundation PR после `08328b33` не
создаётся: это потребовало бы rebase/cherry-pick всей уже существующей цепочки
и не дало бы нового correctness boundary.

После каждого P-stage evidence содержит: executed commands, passed/failed tests, actual paths против allowed map, covered invariants/transition rows и hot-file pass count. Финальный coverage ledger связывает каждый пункт этого плана с test/reference, owner, commit и validation evidence.

### 9.11. Stop-and-replan triggers

Немедленная остановка обязательна, если:

- появляется новый invariant или state transition;
- нужен semantic production path вне раздела 9.7;
- требуется изменить canonical shape/type/ABI/wire layout;
- требуется production math Metadosis или Fidelity/RCFI либо Credis redesign
  за пределами утверждённого rate-scale denominator change;
- non-ISO generic Oracle нельзя сохранить без pair metadata или глобального scale change;
- hot file требует третьего semantic pass;
- frozen test expectation оказывается неверным;
- review делает текущую архитектуру некорректной;
- любой пункт плана нельзя выполнить буквально.

После остановки запускаются ровно три независимых analysis-only агента; после получения всех трёх выводов дальнейшие изменения ждут явного решения пользователя.
