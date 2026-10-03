# Остаток плана рефакторинга blockchain

Дата: 2026-10-03. Ветка: `refactor/blockchain-remaining-plan`.
База: `b6b2d85e18368a174476f5816dcbfc115f97b444` (main).
Учёт: Beads epic `outbe-chain-v4jg`; исходные номера взяты из
`qlty-blockchain-refactor-plan.md`.

Пользователь разрешил небольшие изменения; крупные изменения отложены в
отдельные задачи. Epoch R43/R44 исключён из этой очереди. Ниже «выполнено»
означает завершение согласованного объёма, а не отсутствие всех статических
замечаний в соответствующем модуле.

| Пункт | Результат | Commit |
|---|---|---|
| R26 | Strict header/bitmap и общий lenient decoder адресов; сохранены правила truncation/trailing bytes | `cb13c9e7` |
| R31 | Контексты DKG storage/material/boundary, signer committee и replay; сокращены частные интерфейсы | `d3341908` |
| R32 | Контексты reporter dependencies/committee и mux retry policy; старые публичные API сохранены | `c44b0ebc` |
| R34 | Контексты late-finalize vote/committee и recovered block/material; старые публичные API сохранены | `02a98702` |
| R35 | Разделена CLI/engine args validation; общий status reporter сохраняет порядок чтения и вывод | `25249f76` |
| R36 | Общая обработка TEE metadata и signer material; сохранены порядок ошибок и wire format | `e65ffccb` |
| R37 | Проверено и оставлено: короткие journal initializers сохраняют отдельные OnceLock, paths и logging; исходный план допускает этот результат | Без изменения кода |
| R38 | Удалены два лишних forwarding wrapper; потребители используют уже существующий общий bootstrap projection | `d1aff9ef` |
| R39 | Общие canonical-result, EVM qualification/borrowed-code и marshal archive fixtures; независимые assertions сохранены | `2b96b598` |
| R40 | Малый объём выполнен: source walker, общий trie-root projection и cycle genesis, локальные mint fixtures, явная Radicle voting matrix | Commit с этим отчётом |
| R41 | Rust test paths выделены в Qlty config; тесты проверяются с `--include-tests` | `835f7105` |
| R42 | Проверено и оставлено: небольшие типизированные конструкторы понятны без искусственных Args structs; это соответствует исходному плану | Без изменения кода |

## Крупные изменения и отдельные решения

| Beads | Что осталось |
|---|---|
| `outbe-chain-kjf2` | R36: публичное представление signer custody/factory |
| `outbe-chain-50iq` | R32: удаление старых публичных reporter/mux API после решения о совместимости |
| `outbe-chain-ug9o` | R34: удаление старых late-vote/resolve/recovered-record API после решения о совместимости |
| `outbe-chain-d1rl` | R40: перестройка длинных lifecycle test harnesses и remaining structural fixture similarities; сохранить сценарную историю, fault injection и независимые expected values |
| `outbe-chain-m4vd` | Отдельный baseline failure: IVote ABI golden hash в primitives integration test; test и ABI JSON не менялись этим рефакторингом |

Ранее отложенные решения остаются вне этой очереди:
`outbe-chain-9twy`, `outbe-chain-u8km`, `outbe-chain-dzch`,
`outbe-chain-f9lc`, `outbe-chain-uwgz`, `outbe-chain-hiic`,
`outbe-chain-i8z6`, `outbe-chain-zy2n`, `outbe-chain-pn9o`,
`outbe-chain-noe4`, `outbe-chain-fka3`.

## Проверки

Каждая реализация проверена соответствующими release-тестами и Clippy,
свежими Qlty smells и Repowise native/live diff health. Детальные команды,
состав проверок и ограничения записаны в дочерних Beads задачах; результаты
не означают проверки всей репозитории или отсутствия прежних замечаний.

Для последних R39/R40 прошли 446 consensus lib tests, 26 EVM integration
tests, 6 local-result tests, 113 EVM executor tests, 4 consensus integration
tests и 8 Radicle startup integration tests. Временная differential fixture
проверка подтвердила совпадение всех полей и canonical bytes и затем удалена.

Свежий scoped Repowise анализ R39: 14/14 файлов, 21 resolved,
0 introduced / 0 worsened; R40: 6/6 файлов, 3 resolved,
0 introduced / 0 worsened. Финальный release Clippy для lib/tests
consensus, node, EVM и Radicle прошёл с `-D warnings`.
Qlty include-tests подтверждает удаление точных
повторов canonical-result, borrowed-code, qualification и archive setup.
Оставшиеся замечания включают независимые сценарии, structural similarities
между crates, старые публичные compatibility signatures и большие test
harnesses; существенные изменения перечислены отдельно выше.

После каждого implementation commit отправлено сообщение в Telegram.
