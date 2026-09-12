# Уточнение C-02/C-03 после bounded reconciliation

2026-09-11. Первый `REVIEW_C.md` оставлен неизменным; его SHA-256 перед уточнением: `b2d137bdf06da5b8dc5a56ac96c5ba064bf7ebd2a16f0a2c55436ff64cd9994b`.

## C-02: accepted leakage, а не новый blocker

Моя прежняя формулировка о необходимости впервые определить privacy относительно разрешённых outputs была лишней: это **уже прямо определено** в `PROTOCOL_TRACE_AND_REQUIREMENTS.md:45`. C14 на `:26` и `:234` разрешает S/S_l.

- Singleton `S_l=a_i` — следствие принятого раскрытия. **Снимаю его как новое замечание/blocker**; отменять S_l или требовать новой политики лиг не нужно.
- `G−F=g_claimed`, когда погашено ровно одно право, остаётся верным примером последствий возможной F policy. Это **аргумент к уже открытому** вопросу granularity/timing F (`PROTOCOL_TRACE_AND_REQUIREMENTS.md:345`), а не новая атака на VSS и не недостающая общая privacy definition.

Итоговая классификация C-02: информационное уточнение accepted leakage + пример для существующего F-policy gate. Не самостоятельный Medium defect.

## C-03: простой lifetime bound действительно закрывает overflow в ограниченном профиле

Точный идентификатор дня — `pub struct WorldwideDay(u32)` в `crates/blockchain/primitives/src/time.rs:144`; `new` принимает u32 (`:147`), `value` возвращает u32 (`:151`). Поле u64 в source draft не расширяет множество принимаемых protocol day IDs.

Для профиля **нулевое начальное состояние, только current-source Nod mint, без внешних mint/import/migration и повторного использования дневного бюджета**:

```text
для каждого canonical day d: 0 <= minted_from_day(d) <= G_d < 2^176
число различных day IDs <= 2^32
cumulative minted < 2^32 * 2^176 = 2^208
0 <= live Gratis supply <= cumulative minted < 2^208 < 2^256
```

Это также меньше q251/q254 рассматриваемых commitment groups. Даже более грубое допущение 2^64 различных дней дало бы `<2^240`, всё ещё ниже U256 и q251. Отсюда **для такого профиля не нужен отдельный MPC range check supply только ради overflow**. Символический пример первого отчёта `T_old=U256_MAX` в этом профиле недостижим; он не доказывает дефект допустимой истории. Мой первоначальный C-03 не следует читать как невозможность простого bound-решения.

Чтобы bound закрывал именно арифметический guard, достаточно явно закрепить:

1. Начальный supply и все его составляющие согласованно нулевые; все будущие credits проходят только описанный mint. Для ненулевого genesis/import требуется доказанная начальная сумма и её включение в общий bound.
2. Каждый принятый день использует current source/R01 + checked u32 count + R05/R10, поэтому `G_d<2^176`; миграция denomination не применяет дополнительный множитель к уже fixed18 значениям.
3. Для каждого day lifetime суммарный mint не превосходит одного сертифицированного G_d: claim единственный, replay/reorg не создаёт повторного credit, forfeit не mint, старый day ID/бюджет не переоткрывается для независимой новой эмиссии.
4. Переходы сохраняют global conservation. Burn списывает только существующую сумму с bounded account/pledged state; переносы между balance, pending pledge ticket и pledged state не создают новую стоимость. Тогда supply nonnegative и прежний underflow guard также следует из инварианта, а не из одного верхнего bound.
5. Любой новый mint source, state import, genesis credit, denomination или day-ID recycling/version change пересматривает этот доказанный профиль либо получает отдельный доказанный вклад в bound.

Это условия безопасного удаления избыточного checked arithmetic, а не доказательство уже работающей target реализации. Полный current U256 account type остаётся допустимым представлением; конкретные достижимые значения этого профиля имеют более тесную границу.

## В текущем repo есть второй mint source, поэтому no-other-mint — существенное условие

В bounded writer/caller поиске по Rust `crates/` и `bin/` найден следующий прямой production путь:

| Участок | Exact source |
|---|---|
| Единственный найденный increase writer circulating supply | `crates/core/gratis/src/runtime.rs:120`–141, `mint_impl`; `checked_add` на `:139` |
| Его два внутренних wrappers | `crates/core/gratis/src/runtime.rs:154`–172; public Rust API `crates/core/gratis/src/api.rs:44`–63 |
| Factory mint с Fidelity | `crates/core/gratisfactory/src/runtime.rs:129`–141 вызывает `gratis::mint_with_fidelity` |
| Nod→Gratis caller | `crates/core/nodfactory/src/runtime.rs:236` |
| **Promis→Gratis caller** | `crates/core/promisfactory/src/runtime.rs:69`–82: burn Promis, затем `gratisfactory::api::mint` на `:80` |
| Публичная достижимость Promis conversion | `crates/core/promisfactory/src/precompile.rs:42`–58, `mineGratis`; route `crates/blockchain/evm/src/precompile_routes.rs:363` |
| Найденные circulating supply decrease writers | `crates/core/gratis/src/runtime.rs:194`–198 (`burn_impl`) и `:477`–481 (`burn_pledged_impl`) |

Gratis ABI сам read-only/nontransferable (`crates/core/gratis/src/precompile.rs:20`–24); прямой `mint` selector в нём отсутствует. Однако внутренний Rust mint API существует и его authorization не является самостоятельно source-derived G bound.

Promis conversion — реальный соседний consumer, а не гипотетическая миграция. Его вход может происходить от Gem и Intex: `crates/core/gemfactory/src/runtime.rs:536`, `crates/core/intexfactory/src/runtime.rs:1032` вызывают PromisFactory mint. Их суммарную lifetime economics я **не исследовал**. Поэтому для полной существующей системы нельзя вывести Gratis supply bound только из Lysis G; можно либо явно ограничить прототип Nod-only профилем, либо отдельно доказать общий bound с Promis-conversion вкладом. Наличие этого пути само по себе не доказывает достижимый overflow.

## Genesis, migration и API: что проверено и что не утверждается

- `scripts/seed_genesis.py:1922`–1928 отвергает прямые `gratis_balances/promis_balances`. Вместо этого `:1931`–1936 позволяет seed Settled Gems для будущего Gem→Promis→Gratis. Это ещё одна причина не приравнивать репозиторий в целом к Nod-only профилю.
- В прочитанных `release/testnet-genesis.json` и `crates/blockchain/node/tests/assets/genesis.json` entry по адресу Gratis `0x1003` отсутствует. Это проверка двух файлов, не утверждение о deployed genesis.
- Seeder также содержит `seed_tributes` с supplied nominal (`scripts/seed_genesis.py:885`–930) и вызов на `:1943`. Его совместимость с текущим runtime layout и достижимость импортированных записей здесь не проверялись. Current-source bound должен распространяться на любые реально допустимые imported rights либо профиль обязан их исключить.
- Отдельного Gratis state-migration writer в просмотренных прямых writer/caller paths и найденных genesis/migration файлах не установлено. Универсальные raw-storage imports/upgrades, deployment state и весь migration surface не аудировались; отсутствие таких путей не заявляю.
- Сохранение/изменение публичного `totalSupply()` (`crates/core/gratis/src/precompile.rs:39`) и событий со supply — **самостоятельное интерфейсное/privacy решение**. Lifetime bound закрывает arithmetic overflow, но не скрывает числовые delta уже опубликованного API.

Итоговая классификация C-03: арифметический guard разрешим простым доказанным bound в явно ограниченном профиле; остаются global conservation, граница других mint sources и API compatibility. Это obligation спецификации, не установленный current-source overflow defect.

Проверка только read-only source/арифметика; graph недоступен, whole-repo completeness не заявляется. Чужие review не читались, heavy runs не выполнялись. Создан только этот addendum.
