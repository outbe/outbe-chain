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
| R40 | Initial source-walker/root/cycle/mint/voting cleanup and the approved full lifecycle follow-up are complete; see the staged record below | `84aa1ae4`, `7e9e1aa6`, `0ea38ab0`, this follow-up report commit |
| R41 | Rust test paths выделены в Qlty config; тесты проверяются с `--include-tests` | `835f7105` |
| R42 | Проверено и оставлено: небольшие типизированные конструкторы понятны без искусственных Args structs; это соответствует исходному плану | Без изменения кода |

## Крупные изменения и отдельные решения

| Beads | Что осталось |
|---|---|
| `outbe-chain-kjf2` | R36: публичное представление signer custody/factory |
| `outbe-chain-50iq` | R32: удаление старых публичных reporter/mux API после решения о совместимости |
| `outbe-chain-ug9o` | R34: удаление старых late-vote/resolve/recovered-record API после решения о совместимости |
| `outbe-chain-m4vd` | Отдельный baseline failure: IVote ABI golden hash в primitives integration test; test и ABI JSON не менялись этим рефакторингом |

Ранее отложенные решения остаются вне этой очереди:
`outbe-chain-9twy`, `outbe-chain-u8km`,
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

## R40 lifecycle follow-up — 2026-10-03

The user selected full implementation in three stages. Branch:
`refactor/blockchain-r40-test-harnesses`, based on `84aa1ae4`.
Beads: `outbe-chain-d1rl` and its three stage tasks. The other fourteen
architecture/compatibility decisions remain deferred; the separate IVote ABI
baseline failure is outside R40.

| Stage | Delivered test-only seam | Commit and validation |
|---|---|---|
| A | Shutdown vote collection/consistency; native history persistence; missing-prerequisite and restart stages; shared signer arguments | `7e9e1aa6`: 267 engine lib tests with snapshot-integration, 6 shutdown tests, release Clippy; Repowise 6/6 files, 25 resolved, 0 introduced/worsened |
| B | Prior DKG committee, player/dealer-only launch, signed log collection; metadata construction/publication/verification | `0ea38ab0`: 446 consensus lib tests, release Clippy; Repowise 4/4 files, 4 resolved, 0 introduced/worsened |
| C | Issuance liquidity/oracle/note/proof/payout/retry/settlement stages; delegation call inputs and controls; block state/receipt/contract observations; CE and gas fixtures | This report's commit: 246 EVM lib tests, 11 delegation tests, real-proof issuance test, release Clippy for all EVM test targets; Repowise 4/4 files, 7 resolved, 0 introduced/worsened |

All 304 original EVM assertions and the consensus-stage assertions remain.
Unexpected delegation results and recoverable fixture failures now propagate to
scenario boundaries, which still fail the test. Real proofs, independent
expected values, fault injection, delivery/publication/transaction order and
shutdown/root-hook lifetimes are preserved. The CE observer stores the last
positive cleared-slot count atomically; zero is unset, and the original positive
count assertion still runs before hook detachment.

Fresh Qlty includes tests explicitly. Stage C has zero Qlty findings in its four
files; stage B lowers removed-dealer complexity from 29 to 19. Residual scoped
findings in earlier stages include independent scenario/setup similarities and
cross-crate adapter shapes. Native health still flags some long scenario drivers
that own a single EVM/service lifetime; no lint suppressions, test exclusions or
production/API/protocol changes were used to clear the diff. The original
consensus source-walker and Radicle voting-matrix parts were already delivered in
`84aa1ae4` and required no repeated edits.

Each implementation stage has its own verified commit and Telegram notification.
Beads is the authoritative task record; this document records the delivered scope
and its verification, not a new task queue.


## R20 — Tempo durability contract

The user approved the two-stage Outbe adapter on 2026-10-03 after three independent
compatibility audits of Tempo `61c979a5` and Commonware `d476a23`. Epoch/DKG,
continuity anchors and exact-parent accounting retain their existing owners.
No upstream patches or block/wire changes are introduced.

The first slice replaces unconditional certification with round-bound digest
recovery and a `marshal.certified` durability barrier. It runs outside the bounded
application mailbox and abandons cancelled/shutdown requests without a false
validity vote. Locally built proposals are withheld if their existing durable
acknowledgement is unavailable. Verification verdicts and canonicalization remain
unchanged. The real-marshal restart test starts from a network-buffer-only block:
certification itself must write the recoverable archive record.

The first slice is committed as `95b75219`. The second slice is delivered below.
Beads `outbe-chain-dzch` and its two children are authoritative for delivery status.

First-slice validation: all 451 consensus release unit tests and release Clippy
for consensus/engine all targets passed. The two-axis review found no remaining
Standards or Spec findings after correcting inherited relay documentation and
strengthening the buffer-only crash test. Native diff health analyzed all eight
changed Rust files with no skipped paths, introduced findings or worsened findings;
33 inherited findings belong to the separately recorded larger tasks. Qlty
includes the new tests and reports zero findings in the four targeted logic/test
files. No findings were suppressed.


### Second functional slice — staged publication

Branch: `refactor/blockchain-r20-durability`, based on `bd8493a6`.
The publication coordinator registers the candidate and its exact `(Round, Digest)`
durability gate before releasing the proposal digest. Relay consumes the staged
candidate atomically through public `marshal.proposed`, preserving the requested
recipients. Repeated relays use digest forwarding. Certification flushes an
unrelayed candidate through `verified_deferred`, waits for completed sync and
falls back to exact notarized-candidate recovery if a gate is missing or abandoned.

The gate and independent sync observer outlive response cancellation and
same-process Simplex restarts. Storage failures retain the fatal policy; an unavailable acknowledgement or
closed/aborted sync alone cannot authorize a vote. Exact recovery must establish
durability before a positive vote; runtime shutdown abandons the request. Marshal finalized `Update::Tip` retires earlier gates; nullified or
cancelled views do not. The pre-build persisted-round guard withholds a new
candidate after recovery instead of rebuilding it under changed context.
Epoch/DKG, execution validation, verification/canonicalization and pacing retain
existing ownership. Speculative block-cache entries remain unrelated to durability.

Final ordinary acceptance: 462 consensus and 258 engine release library tests
passed (720 total), including the real delayed `certify` response/cancellation,
identity, relay, absent-relay, shutdown, fatal-sync and unclean-restart scenarios.
All-target release Clippy for consensus and engine passed with `-D warnings`.
The Standards and Spec review axes each have zero remaining findings.
All nine copied-native snapshot-recovery tests passed with `snapshot-integration`,
using real release storage fixtures (729 passing tests across both acceptance runs).

Fresh native Repowise diff health covered all 15 changed Rust paths, with no
skips or worsened findings and 40 inherited markers. Simple interface, locking,
dispatch and setup findings were corrected. Seven introduced static markers are
recorded explicitly: two scenario-similarity markers, three fixture assertions,
constructor-versus-instance LCOM4=2, and the required fatal-sync guard. Scenario
assertions and the fatal failure policy were preserved; these results do not
claim zero static findings. The old broad verification/startup refactors remain
in their existing decision tasks.

Qlty explicitly included all changed test files. The new publication seam has
zero findings. Seven existing findings remain across the verification matrix,
startup driver and independent scenario similarities in the inherited large
handler test module. No checks, files, tests or findings were suppressed.
