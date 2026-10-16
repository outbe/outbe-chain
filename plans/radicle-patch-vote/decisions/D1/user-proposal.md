# D1 — решение на согласование

Статус: **ожидает явного ответа пользователя**. Вариант ниже не реализован и не считается согласованным. Это выбор места проверки существующих требований P2, без расширения scope. Исходный `proposal.md` и snapshot сохранены без изменений.

## Подтверждённая проблема

В незавершённом P2 managed policy отклоняет owner BLS entry при прямой загрузке, но валидная Ed25519 entry, ссылающаяся на неё через Resource/Related, принимается `Repository::load` и `Op::load`. Три независимых агента воспроизвели это на реальных Git objects через production APIs. Чистая Ed entry принимается, как и требуется до Vote.

Это доказательство admission gap, а не выполненного Merge exploit. Обход policy через raw stream и отсутствие provenance в SQL cache подтверждены чтением реальных callers; runtime cache exploit этим repro не доказан.

Frozen base HEAD: `0c1c8773baa8424444744bccede6eadeb022e3eb`; snapshot SHA256 `22783b25124ef5bfb6dffa29d667ba748c0da5c06aa216b55a96494c16c5bb75` содержит exact hashes 21 source и 6 artifacts, включая незакоммиченные изменения. Все совпали при проверке координатором после трёх отчётов.

## Предлагаю A с условиями всех трёх проверок

1. **Одна общая проверка допустимости кандидата и всех его зависимостей.** Проверять реальные Git parents и значимые Resource/Related ссылки, включая nested, mixed-type и другой namespace, до type/range filtering. Для semantic references недостаточно предположить наличие Git parent: обходить их либо отклонять структурное несоответствие. Использовать существующие native parsers, проверку настоящих Ed/BLS signatures и trusted policy/receipt для actual owner entry. Обычные unsigned code commits не превращать в ошибочные COB; malformed native BLS и missing dependencies не пропускать как обычный код.
2. **Отказ по зависимой ветке.** Недопустимая owner entry и зависящие от неё кандидаты не поступают в evaluator/direct/stream. Независимые Ed tips и разрешённые exact revisions/prefix остаются доступны, в том числе рядом с плохим tip того же remote/COB. Legacy soft-prune не должен превращать отказ в допустимого disconnected descendant. Не вводить blanket отказ всему repo или namespace.
3. **Подключить настоящие callers.** Repository/DraftStore, direct Op, get/list/update, подготовленный local store/apply/ref update, Stream/OpsIter и CLI, временное хранилище и fetch/install используют тот же predicate. В fetch отдельно проверять точную подписанную refs map, admission COB tips и права canonical promotion. Нельзя удалить плохой ref из map и объявить остаток тем же подписанным snapshot. Quarantined raw data не является authoritative state.
4. **Привязать cache к реально проверенному source view.** Provenance создаётся при admitted evaluation и передаётся через фактические writers/Update/node materialization. Repo/object/type, exact admitted source tips и owner receipt/policy binding сохраняются согласованно с JSON. Оба StoreReader/StoreWriter, mutation reads, get/find/list/status/count и итераторы проверяют тот же view. Изменение source tips или недоступность/несоответствие required policy не допускает возврат прежней permissive row. Не создавать provenance из arbitrary JSON или envelope.context.
5. **Сохранить данные и read-only чтение.** Legacy managed rows без provenance требуют reevaluation из admitted Git; если источник/доказательство недоступны — явный отказ. Read-only reader может переоценить Git без записи либо вернуть ошибку; inability to rebuild не разрешает bypass. Migration не удаляет глобально caches или unrelated Ed/static BLS data. Не публиковать частично обновлённый materialized state и не смешивать два разных source/policy snapshots.
6. **История отдельно от нового продвижения refs.** Exact accepted historical entry с trusted historical receipt остаётся читаемой после обычной смены validator material. Это не даёт retired key новых прав, не делает old signed refs latest и не разрешает rollback. Finality/latest complete snapshot/freshness и CAS проверяются отдельно в фактической promotion boundary. Heartwood не получает Vote/epoch/validator логику.

Основные файлы: `radicle/src/storage/{publication.rs,git.rs,git/cob.rs,git/temp.rs}`, `radicle/src/cob/{op.rs,store.rs,stream.rs,stream/iter.rs,patch/cache.rs,issue/cache.rs}`, shared cache schema/migration и writers/Update, `radicle/src/profile.rs`, `radicle-cli/src/commands/{cob.rs,patch/cache.rs}`, `radicle-node/src/worker/fetch.rs`, `radicle-fetch/src/{state.rs,sigrefs.rs}` и их существующие callers. Пути относительно `vendor/heartwood/crates/`. Это карта затрагиваемых границ, не требование добавлять абстракцию в каждый файл.

Альтернатива B меняет generic ChangeGraph traversal/error/pruning semantics. Все три reviewers считают A предпочтительнее: B всё равно требует отдельного подключения direct/stream/cache и не является меньшим полным решением. Локальные typed errors внутри A допустимы; это не разрешение общего redesign graph.

## Независимые заключения

| Отчёт | Итог | SHA256 |
| --- | --- | --- |
| [R1](reviews/R1.md) | Проблема воспроизведена; A рекомендована с caller/provenance/classification условиями | `d5eb64d7f2245f3d867a942837d77fc1d1e8cdd048a137e90cbbece0d72fd5de` |
| [R2](reviews/R2.md) | Проблема воспроизведена; A рекомендована с exact signed-map/per-tip/read-only/history условиями | `c5ff1c666b6fd5644c9968ab9dbf087b0702190d1c7fd59e9c09ae4b765e72e3` |
| [R3](reviews/R3.md) | Проблема воспроизведена; A рекомендована с semantic dependency/cache producer/history условиями | `753ae5d076f2022508a5d4f05fe7c5c5fde14186de3e49913114dbfd0c1e9a95` |

Каждый проверил frozen hashes и независимо запустил exact repro до чтения peer reports. Exit0 означает воспроизведённый gap. Эти заключения не являются P2 acceptance PASS или пользовательским разрешением.

## Что докажет реализацию после согласования

Negative baseline становится regression: direct owner и Ed parent/Resource/Related/nested/cross-type/semantic-reference descendants reject; pure Ed и независимые разрешённые revisions остаются доступны. Дополнительно нужны реальные competing-tip, stream ranges, local write/fetch exact map, patch+issue SQL reader/writer/warm/legacy/reopen/rebuild/read-only/source-change и historical-versus-latest/CAS сценарии с проверкой authoritative state/ref side effects. Native suites, root consumers, production two-peer BLS workflow и три полных independent P2 acceptance остаются обязательными.

До явного ответа пользователя зависимая реализация остановлена. Scope/crypto/wire/genesis/Vote контракт не меняется. Goal остаётся активным; P2 не закрыт.
