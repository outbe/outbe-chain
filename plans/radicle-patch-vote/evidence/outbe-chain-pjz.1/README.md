# P0 evidence — candidate контракта, не production feature

Task: `outbe-chain-pjz.1`. Source base: `42d08693a`; предыдущий investigation commit:
`c965901802e735f402ba3e5e438baa66cdf9f90a`.
Exact reviewed SHA/spec/artifact hashes передаются в `review-candidate.json` после commit.
Ни один P0 review PASS здесь заранее не объявлен. P1–P8 не реализованы.

## Acceptance matrix

| AC | Source и решение | Проверяемое evidence |
| --- | --- | --- |
| AC1 | design.md §§1–7: выбран native BLS + external finalized publication policy; static key, exact current membership, authenticated material, genesis и lifecycle определены | source-audit.json: S01–S33/C01–C06; decision model; source-backed ограничения и assumptions прямо указаны |
| AC2 | design.md §2: отдельно identity blob OID, COB tree OID и canonical refs bytes; RDB1 framing/meta/policy context; §6 dependency order без self-reference | S05–S10/S16/S28; primitive namespace/subject negative checks. Сам RDB1 production parser ещё задача P2 |
| AC3 | design.md §§3–5: existing canonical DKG material + узкий fixed-domain signer; q current identity endorsements для initial material/descriptor→RID, CAS binding, no operator owner key | S05/S19–S24/S26/S33, C01–C06; настоящие 3-of-4 primitive vectors; отдельная схема bootstrap/material checkpoints. Anonymous fixture не называется distributed DKG |
| AC4 | design.md §4: raw old-key signature не даёт новое canonical write; обязательный current quorum + immutable chain checkpoint; history и Promote различаются | design-check.log, publication.rs: actual MinPk signatures и reduced state model для retired identity/epoch race/t-1/duplicate/tamper/exact history. Production fetch gate не объявляется существующим |
| AC5 | design.md §7 и task-specs.json: новые seams/carrier/runtime/policy/ABI/files/tests разнесены P1–P8; source limits 256/64KiB и coexistence с current OART учтены | actual Beads descriptions P1–P8 и их review children обновлены одинаковым delta; beads-current.jsonl сохраняет actual snapshot; graph/lint проверки ниже |

## Воспроизведение

Из корня feature worktree:

```sh
python3 plans/radicle-patch-vote/p0-check/audit_sources.py
CARGO_TARGET_DIR=/private/tmp/outbe-radicle-p0-target cargo run --offline --locked --quiet --manifest-path plans/radicle-patch-vote/p0-check/Cargo.toml
rustfmt --edition 2021 --check plans/radicle-patch-vote/p0-check/src/main.rs plans/radicle-patch-vote/p0-check/src/publication.rs
bd --readonly dep cycles --json
```

Standalone Cargo.lock закрепляет dependency resolution. Commonware source revision тот
же, что в root Cargo.lock; target directory находится вне основного checkout. Private
shares и secret keys не выводятся. Вывод — public keys/signature и PASS выбранных checks.

`audit_sources.py` — inventory/extraction, не source-string regression test поведения.
`publication.rs` — reduced executable **проектная модель** с настоящими identity signatures,
не будущий handler/finality verifier. Она не проверяет omitted fields полного PublicationBody,
gas, pipeline wiring или реальную finality. Эти проверки обязательны в P3–P8.

## Threat traces и доказательство на уровне контракта

| Попытка | Где отклоняется в выбранном production контракте | Проверка P0 |
| --- | --- | --- |
| Old shares создают valid native signature | Нет q current identity endorsements/checkpoint; receiver Policy не даёт Promote | Primitive old-key validity + модель отказа retired identity/no receipt |
| Epoch меняется до принятия последней attestation | Execution-state epoch/material mismatch; старые pending slots inactive | Модель `activate` между последовательными checkpoints |
| Exact package уже зафиксирован до смены epoch | Разрешена immutable history; same receipt/package hash; canonical rollback запрещён | Модель exact history остаётся valid, другое package отвергается; Promote правило в design §4 |
| Подмена manifest, repo, цепи, subject/domain, индекса | Fixed encoding + registered key/index + native/policy message binding | Реальные primitive/model negative assertions |
| Дубликаты участников | Один slot/index на sequence; q только distinct | Primitive recover и модель duplicate/t-1 |
| RPC/peer объявляет свой group key/current set | Trust root — own validating node state и current identity quorum, не self-signature | Source contract; production test P3/P4/P6 |
| Collector умер после threshold или partial publication | Durable package до checkpoint; replay exact committed package; CAS/quarantine | Design §6; реальные crash tests обязательны P6/P7 |
| Максимальная committee/budget | ≤256, одна identity verification/block, fixed attestation и bounded slots, общий 64KiB artifact limit | Static source S29–S31; реальные gas/size tests обязательны P3 |

Существенные assumptions: unforgeability BLS/MinPk и используемых hashes, корректная
local finality, не более f Byzantine current participants. Это не обещание безопасности
при компрометации current quorum или local validating process. Начальная signature
proof-of-ownership и current publication permission — разные predicates.

## Граница закрытия P0

P0 — design/spike задача: требуется source-grounded, конкретный осуществимый контракт,
а не готовые production P1–P8 tests. Три reviewer обязаны проверить каждый AC, source,
форматы/циклы/assumptions и sufficient experiment evidence. Если хотя бы одно решение
неосуществимо или меняет требование пользователя, task остаётся открытой до исправления.
Production отсутствие само по себе не считается выполненным P1–P8 и не закрывает feature.
