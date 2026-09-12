# Проверка исследования и оставшаяся работа

Дата: 2026-09-11. Текущая редакция после трёх независимых проверок и внесения подтверждённых исправлений. Подробный разбор — [REVIEW_REMEDIATION.md](REVIEW_REMEDIATION.md); исходный audit — [SYNTHESIS.md](independent-review-2026-09-11/SYNTHESIS.md).

**Замечания к полноте документов учтены; полный протокол ещё требует выбора перечисленных интерфейсов и проверки реализации.** Нельзя считать завершение сравнительного research подтверждением сквозной конфиденциальности, 512 МБ или миллиарда записей.

## 1. Что теперь покрывает trace

| Область | Где описана | Что ещё требуется |
|---|---|---|
| Источник, nominal, P_link; C01–C06/C10/C13 | Trace R00–R02; main §4 | Source privacy/bounds, setup/security target, полный P_link A/B и cold peak |
| Численные S/S_l и rotation; C03/C08/C14 | Trace R03–R08; main §5; aggregation appendix | Выбранный malicious/mobile protocol, repair/liveness, DA и измерения |
| Deferred Nod, claim/forfeit; C04/C09/C11 | Trace R09–R14; main §6 | Payment18, cost/zero policy, accepted forfeit disclosure/timing |
| Intex contributor payout | Trace R15 и downstream R15 | Private backed payout/denominator, checked floor/remainder, visibility и lifecycle |
| Все writers Gratis/Fidelity | Trace R16 и downstream R16 | Compartment format, external writer authority, offline MPC mutation, recovery и связанные Credis public amounts |
| Promis mint и supply | Trace R17; main §6.5; downstream R17 | Полный mint/import profile и приватный conversion adapter, precision reserve |
| Fidelity state/time/query | Trace R06/R18; main §§7.4–7.5; downstream R18 | Exact-time validity до commit, state bounds, private-output/локальный API и witness availability |
| Кто создаёт commitments и хранит данные | Trace §7; main §3; downstream operation tables | Конкретные encodings/proofs и checkpoint/recovery implementation |
| Bytes, admission/s, Lysis 256 records, Nod/s, parallelism; C07 | Main §10 | End-to-end measurements; прежние components не являются TPS |
| SEAL; C12 | Main §§2/9 | Исключение сохранено |

Ранее исправленные требования также сохранены: per-record receipts не дают common coverage сами по себе; нужен Ready/repair. Для Lego verifier обязательны точное число public inputs и единый зарегистрированный VK bundle. Новое уточнение prover — независимый свежий internal v, помимо external blinder.

## 2. Evidence и пределы проверки

- Существенные findings перепроверены по точным исходникам. Дополнительные downstream paths перечислены в supplement; их snapshot входит в текущий [evidence](DEEP_RESEARCH_EVIDENCE.json). Это не аудит всей экономики Credis/Intex/Promis.
- Первоначальные независимые отчёты, addendum C и их frozen manifests не изменены. Их input hashes относятся к прежней редакции документов, текущие — к новой.
- Сохранены результаты exact Fidelity threshold identity: 137 117 случаев. После изменения текстовых интерфейсов они не переименованы в новые MPC/security tests.
- Дополнительно проверены ограниченный lifetime supply bound, floor payout counterexample и conversion bounds. Успешная арифметика не подтверждает ciphertext/proof implementation.
- При проверке исправлений найден и учтён exact-time overflow gate: time-free payload proof не разрешает денежный commit до successful evaluation на фактическом времени.
- Source-only peak 398 049 280 B и отдельный wide-component peak 497 418 240 B остаются историческими измерениями компонентов. Новый full proof, distributed attack, мобильный prover и миллиард записей не запускались.
- Graph MCP недоступен; использован direct-source fallback. Generation/coverage графа и deployment state не заявляются. Production и прежние measurements не менялись.

## 3. Открытые gates

| Gate | Минимальное свидетельство завершения | Статус |
|---|---|---|
| G0 Protocol interfaces | Принятые contracts R15–R18, supply/import profile, payment18, forfeit, Fidelity exact-time/failure semantics, wallet/source/security/committee parameters | Конкретные варианты описаны; выбор правил не завершён |
| G1 Full P_link / wallet | Полный source→nominal→C(a) proof с реальным P_L2/VSS binding, negative cases и correct adapters; cold process ≤512 000 000 B | Native Baby и Lego ещё не измерены полностью |
| G2 Private lifecycle | Разные inputs, repair/минимум две ротации, offline owners, S/S_l, Nod, residual debit/forfeit, crash/reorg | Transcript есть; сквозного execution evidence нет |
| G3 Shared-state consumers | Offline collateral void до READY; third-party release/recovery; exact inclusion time/overflow; Intex payout/remainder; Promis conversion; exact RCFI query; hidden payment/change18 | Trace расширен; новая реализация и проверка открыты |
| G4 Scale | Измеренные admission/s, t256, Nod/s, full wire bytes, DB/WAL, repair/handoff и overlapping days, включая новое downstream state | Только формулы/исторические component measurements |
| G5 Verifier/DA | Canonical wire/VK registry, bounded verification, complete manifests, encrypted witness binding/availability, replay/bootstrap | Требования описаны; production integration нет |

Следующий шаг начинается с G0, затем полные сравнимые P_link A/B в рамках G1 и lifecycle G2/G3. Запуск огромного circuit без закрытого statement и ограничителя памяти не требуется. Прохождение G1 отдельно не закрывает G0/G2–G5.
