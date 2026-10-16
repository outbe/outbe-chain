## Контекст и зачем

Vendored fork b76a1780 содержит Outbe sidecar delta поверх 16de8f19; runtime уже перенесён в crates/system/radicle. Нужна исправленная stable база перед BLS. Target — releases/1.10.3, peeled SHA 7e1cb406bb935c3de008d2171e8961f57b4336ea; master развивается как Radicle 2.

Feature: outbe-chain-pjz. Этап P1. План: plans/radicle-patch-vote/plan.md.

## Что меняем

- Сделать трёхсторонний перенос по реальному upstream ancestor и сохранить bounded disconnect, runtime limits, выключенный raw control по умолчанию и нужные config/Handle методы.
- Сохранить извлечённый runtime и единственный bin entrypoint; обновить dependency paths/lockfiles, сборочную metadata и UPSTREAM provenance с отдельным перечнем Outbe delta.
- Проверить MSRV/toolchain, API и storage/wire compatibility; не обновлять несвязанные зависимости или всю chain toolchain без доказанной необходимости.
- Воспроизвести и разрешить/обосновать по acceptance baseline concurrent-connection test. Проверить лимиты реальным socket сценарием в нужной среде; known baseline failure не является PASS.

## Файлы и затронутые callers

- vendor/heartwood/**; vendor/heartwood/UPSTREAM.md; vendor/heartwood/{Cargo.toml,Cargo.lock,build.rs}
- Cargo.toml; Cargo.lock; crates/system/radicle/{Cargo.toml,src/lib.rs,tests/sidecar.rs}; bin/outbe-radicle/
- bin/outbe-keygen/Cargo.toml; crates/blockchain/radicle/src/manager/adapters.rs; testing/e2e-harness/{Cargo.toml,src/artifacts.rs,src/world/localnet/radicle.rs}

Пути — source-grounded точки входа, новые явно обозначены. Перед реализацией уточнить actual callers/fixtures/wiring и обновить карту в evidence; нельзя считать список разрешением пропустить зависимый путь.

## Тесты и проверки

- Root и standalone cargo metadata --locked; cargo fmt --all -- --check; изменённые места проходят lint.
- cargo check --locked -p outbe-radicle-sidecar -p outbe-radicle-runtime -p outbe-keygen -p outbe-e2e-harness --features outbe-e2e-harness/ocomp-integration --all-targets
- cargo check --locked --manifest-path vendor/heartwood/Cargo.toml -p radicle-cli -p radicle-remote-helper --bins; cargo test --locked -p outbe-radicle-runtime --test sidecar
- Affected Heartwood protocol/node tests, E2E vendored install/version tests и реальный запуск sidecar/peer replication с bounded disconnect и resource limits.

## Acceptance Criteria

- [ ] AC1: Stable source SHA и полный retained Outbe delta проверяемы; ни одного утраченого контракта интеграции.
- [ ] AC2: Оба lockfile согласованы с локальными путями; повторная сборка не скачивает прежний Heartwood Git fork.
- [ ] AC3: Sidecar, CLI и remote helper работают совместно; metadata соответствует текущему checkout.
- [ ] AC4: Реальные tests подтверждают сохранение control/connection limits и replication; все обязательные checks зелёные либо нерешённое требование оставляет задачу открытой.

## Обязательный evidence этого этапа

До/после upstream ancestry/diff manifest, source hashes, exact toolchain/platform, build/test logs и peer transcript. Отдельный результат по ранее падавшему status_server_enforces_connection_and_request_deadline_bounds.

## Blocked by

outbe-chain-pjz.1

## Evidence и запрет преждевременного закрытия

До закрытия: exact base/reviewed SHA + spec hash; полный actual changed-files/callers/fixtures/wiring map; таблица каждого AC -> source file:line -> behavioral test/scenario -> реально существующий artifact/hash; точные argv/cwd/toolchain/platform/exit code; negative results; CI URL где требуется; remaining work и NOT VERIFIED должны отсутствовать для PASS. Приложить bundle/link в комментарий и notes задачи. Путь без файлов, компиляция вместо сценария, test-only bypass и декларация исполнителя evidence не заменяют.

После evidence задача остаётся in_progress. Координатор переводит ТОЛЬКО её R1/R2/R3 children из blocked в open. Три разных агента, не авторы реализации, независимо читают ВСЕ AC, source/diff и evidence одного exact SHA/spec hash, не читают чужие reviews до собственного отчёта. Каждый сдаёт AC-by-AC PASS/FAIL/NOT VERIFIED, source/artifact ссылки, воспроизведённые команды и findings. Каждый reviewer закрывает только свою review child после полного PASS без findings. Затем координатор проверяет evidence и три PASS и закрывает parent с их ссылками. Любая правка implementation/spec аннулирует PASS: новый evidence, переоткрыть reviews, три проверки заново. --force, удаление children и подмена статуса для обхода запрещены.

Открытые children защищают обычное закрытие в Beads; независимость/содержание отчётов проверяет координатор. Глобальные hooks/CLI не меняем и автоматической проверки семантики не заявляем. Review children не требуют рекурсивных review-of-review. Правила подробно: plans/radicle-patch-vote/plan.md. Сейчас это план, implementation evidence отсутствует.


## Уточнение P0 — обязательный контракт design.md

Сохранить invariants design.md; stable update предшествует BLS/policy changes.

Файлы:
- vendor/heartwood/UPSTREAM.md; root/standalone Cargo manifests и lockfiles

Проверки:
- Сверить весь retained Outbe delta и проверить actual build/sidecar/native replication после update.

Обоснование и точный контракт: plans/radicle-patch-vote/design.md. Все исходные AC и evidence/review gates сохраняются.
