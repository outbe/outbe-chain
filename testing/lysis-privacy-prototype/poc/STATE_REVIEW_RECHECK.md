# State и atomic integration: повторная независимая проверка A

2026-09-11. Read-only source review; изменён только этот отчёт. Graph MCP отсутствует. Passive MPC и доверенный controller на одном host приняты как экспериментальные условия. Ни одно замечание ниже не требует приписывать MPyC malicious security; проверяется, что именно подтверждают локальные границы вызовов и certificates. Текущий штатный lifecycle сам передаёт правильные аргументы: выявленные случаи относятся к недостающим проверкам интеграции, а не к доказанному сбою этого happy path.

## Исправления предыдущего отчёта

В `src/state.rs` исправления подтверждены source review:

- Private note value проверяется на 256 bits до `UInt::alloc` (184–188), nominal на 104 bits (229–233), blinder на `<q` до bit decomposition (143–148). Старый witness-alias counterexample для state закрыт.
- Fraction теперь canonical uint256 public limbs (74,97–113,204–225); cap1e6 устранён. Промежуточный `a*f` width256 (238) не исключает допустимый claim: g=`a*f*1e6` и c=`a*f*p` положительны и сами должны вмещаться в uint256. Если `a*f` уже переполнен, допустимого конечного g не существует.
- Mint использует private committed `ns[1]` и `ns[0]+ns[1]*1e12=ns[2]` (261–269). `poc-state.rs:145–148` загружает burn note; public amount остаётся 0 в этом prepare route. SC-A-02 закрыт на уровне relation. Source consumption зависит от замечания RR-A-01.

Общий helper `../measurements/p-link/src/integer.rs:67–87` по-прежнему не имеет host width guard. State защищает свои входы локально; глобально SC-A-01 не исправлен. `src/link.rs:342` также не проверяет полную host nominal строку до allocation. Это сохраняет API canonicality debt, но не найденный обход committed integer conservation.

## RR-A-01 — High для reusable commit boundary: inputs не привязаны к полному уникальному source set

**Место:** `run_lifecycle.py:187–227`, особенно 198–205,215–220. Проверка hash context на 194 не сравнивает `input_ids` с `context['input_ids']`, `kind` со `statement['kind']` и не задаёт required input/output cardinality или uniqueness.

**Контрпримеры:**

1. Валидная mint relation old Gratis=0, private burn=3, new Gratis=3e12 допускает commit с `input_ids=[old_gratis]`, без зарегистрированного Promis source. Цикл проверяет только один переданный input; `start=2` всё равно выпускает mint output. Никакой Promis note не расходуется.
2. Move relation может честно доказывать `old+old=new+zero`, используя один известный opening дважды. Список `input_ids=['same','same']` дважды проходит spent=0 до updates, после чего один underlying note помечается spent и создаётся doubled output. Это особенно непосредственно применимо к поддержанному asset `coen-backed`, у которого нет Gratis cohort sum guard.

Оба случая **воспроизведены** вызовом настоящего `Run.commit_transition` на отдельной SQLite `:memory:`. Использованы placeholder commitments; proof generation не запускалась. Это execution проверки consumer, не claim о принятом ложном Groth16 proof: описанные arithmetic relations могут быть истинны, недостаёт источников/уникальности у consumer.

**Минимальный patch:** проверять `kind==statement.kind`, exact bound input IDs из context, уникальные inputs/outputs и disjoint sets, required arity (`claim/mint=2`, `withdraw/pledge=1`, move=2 либо строго заданный single-input zero profile), exact output arity и operation-specific asset schema до updates. Bind operation ID, owner/asset policy и recipients либо непосредственно в context, либо через typed verified request. Число аргументов caller не должно определять число обязательных источников.

## RR-A-02 — High для заявленной связки money/cohort: отсутствие pending certificate разрешает денежный commit

**Место:** `run_lifecycle.py:170–181,231–232`; `cohort_bridge.py:88–93`.

Подготовка cohort зависит от hardcoded tag whitelist. Финальный commit вызывает exact-time/root/version gate только при `tag in self.cohort.pending`. Отсутствие pending записи не является ошибкой. Сам tag не входит в public statement context и не сравнивается с сертификатом как отдельный operation ID.

**Контрпример:** подготовлен валидный `withdraw` под tag `withdraw` с candidate_timestamp=T; время стало T+1. Передача того же statement/context в `commit_transition` с новым tag `renamed-withdraw` не вызывает cohort.commit, но расходует денежную note и создаёт COEN. Изолированный consumer check выполнен с cohort object, который обязательно отвергает stale commit: он не был вызван, денежная операция завершилась. Это не атака на MPC; это fail-open dispatch интеграции.

**Минимальный patch:** классифицировать обязательный cohort update по asset и operation semantics, а не по наличию pending/tag whitelist. Для каждого Gratis writer требовать соответствующий verified pending certificate и exact timestamp/root/version; отсутствие — ошибка. Отдельно явно разрешить операции иных assets без Gratis cohort. Связать operation ID и hash денежного statement с certificate; использовать тот же immutable проверенный объект внутри SQLite transaction.

Когда `cohort.commit` действительно вызван, денежные и cohort SQL writes находятся в одном `with self.db`, и exception откатывает обе части. Непосредственной ошибки этой SQL-атомарности не найдено. `prepare/compute` заранее записывает holder shares/recovery artifacts; это подготовленные данные, а не monetary finality, и их хранение/retry требует отдельного lifecycle.

## RR-A-03 — Medium: certify-mpc не связывает signed next_state metadata с вычисленным результатом

**Место:** `src/bin/poc-vss.rs:134–167`; producer `cohort_bridge.py:63–85`; `mpc_worker.py:73,123–128,226–234`.

Signer сверяет context и validity flags report, затем проверяет, что переданные local output shares открывают переданные polynomials и совпадает список polynomial hashes. Однако report не содержит digest полного plan/output manifest; signer не сверяет `body.next_state` с вычисленными output names, timestamps, qualified/version или прежним состоянием. `body.next_state` подписывается произвольно предоставленным JSON. Дополнительно критерий computed допускает любой из трёх boolean flags, не связывая report operation с видом certificate.

**Source-derived контрпример:** сохранить report, context и output polynomials/shares, но изменить `body.next_state.active[0].at` либо `qualified`, пересчитать `next_state.root`. Все условия 139–162 неизменны, поэтому signer подпишет изменённую Fidelity metadata. На следующем расчёте эта metadata изменяет age/decay. Исполнение подписи не запускалось; вывод следует из полного тела match arm.

В принятой модели доверенный controller сегодня строит согласованные metadata сам. Поэтому это не доказанная атака внутри passive threat model, а точная граница смысла certificate: подпись пока означает доверие controller-provided metadata, не независимую проверку следующего cohort state участником. Нельзя использовать такой artifact как свидетельство, что эта связь проверена.

**Минимальный patch:** report каждого actor должен фиксировать canonical plan digest, operation/domain, old state root/version, exact timestamp и ordered output IDs. Signer должен сверять report с локально исполненным plan и детерминированно проверять next_state metadata/row→output mapping, включая root и квалификацию. Bind полный monetary statement digest. Проверить negative mutation timestamps/qualified/output mapping и cross-operation report. Это обычная binding проверка поверх passive результата, не требование заменить MPC протокол.

## Evidence boundary

Прочитаны указанные основные файлы, их необходимые producers `poc-state.rs`, `mpc_worker.py`, `pledge_scenario.py`, `lifecycle_branches.py:12–36`. Вмешательства в parent lifecycle4-d, builds, proofs, benchmarks или private actor data не было. Три лёгких isolated SQLite consumer checks дали `DUPLICATE_INPUT_ACCEPTED`, `MINT_WITHOUT_REGISTERED_BURN_ACCEPTED`, `STALE_COHORT_SKIPPED`; они не эмулируют cryptographic verifier и не засчитываются за proof exploit.

Snapshot SHA-256:

| Файл | Hash |
|---|---|
| `src/state.rs` | `524b8cdee1d0a83cf65a56aba76a206a16187d3bffc80c536dade5b0cba74191` |
| `run_lifecycle.py` | `13e1a6abfeb935572ff4d3521c15e1decad2120ef38c8068e154d32766cf8c58` |
| `cohort_bridge.py` | `8e544adc98474df542f351d92fe225556207aac9b49b7012b017798729a7eb7d` |
| `src/bin/poc-vss.rs` | `c17e5c9a6e0e734db1e996165977e6cd1cb3c47b21db47bdc55309689cc54157` |
