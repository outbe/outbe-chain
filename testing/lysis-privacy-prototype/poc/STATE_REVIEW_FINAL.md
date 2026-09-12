# Финальная узкая перепроверка RR-A-01/02/03

2026-09-11, reviewer A. Проверены только исправленные consumer/certificate boundaries. Source fallback; graph tools отсутствуют. Код и текущий lifecycle parent не изменялись. Passive MPC, доверенный single-host controller и отсутствие production consensus приняты как заявленный экспериментальный профиль.

**Окончательный disposition после последнего патча: RR-A-01/02/03 закрыты в указанном scope; оба найденных при перепроверке остаточных случая RF-A-01/02 исправлены. Новых blockers в этих границах не найдено.** Ниже сохранены точные контрпримеры промежуточного snapshot и доказательства их закрытия, чтобы closure не опирался только на штатный сценарий.

| Finding | Disposition |
|---|---|
| RR-A-01: source set/arity/identity | **Закрыт в проверенном consumer.** `run_lifecycle.py:205–213` требует exact kind/id/owner, cached verified statement, equality input/output IDs с context, unique IDs и operation-specific arity/asset. `transition:188–194` наполняет cache после proof verification и registered-wallet signature. Пересечение input/output IDs дополнительно отвергает SQLite primary key существующей note, с rollback. |
| RR-A-02: fail-open cohort | **Закрыт.** `run_lifecycle.py:214–216,257–258` требует cohort для каждого Gratis asset writer и передаёт actual statement. `cohort_bridge.py:103–111` проверяет его digest, domain и exact-time/root/version в той же SQL transaction. |
| RR-A-03: metadata certificate | **Закрыт для заявленного passive actor report.** `poc-vss.rs:139–158` различает operation types и связывает request; 178–247 проверяет next root, qualified/version, ordered metadata, полные preserved sold rows и output-id→polynomial hash mapping против actor report layout из `mpc_worker.py:130–136`. |

## RF-A-01 — Закрыт: hash конкретного monetary statement теперь связан

**Промежуточный дефект до последнего патча:** `run_lifecycle.py:207,220,257–258`; прежние `cohort_bridge.py:49,81,97,99–104`.

Cache доказывает, что данный statement отдельно прошёл proof/signature verification. Pending certificate фиксирует только context, next_state и polynomial hashes. Ни producer, ни consumer не связывает его с hash конкретного monetary statement.

**Контрпример:** существуют два корректных withdraw statements для одной old note, amount=3 и amount=4. Оба используют одинаковые operation_id, owner, input/output IDs, recipient, candidate time/root/version и потому одинаковый context hash. Их new commitment различен; оба могут быть честно доказаны и подписаны до расходования old note. Если pending cohort рассчитан для 3, а cache содержит также statement для 4, commit второго statement проходит и применяет next cohort state первого. Exact IDs и timestamp не определяют amount или output commitments.

**Выполнено:** на отдельной SQLite `:memory:` вызваны настоящие `Run.commit_transition` и `CohortBridge.commit` с двумя вручную установленными cache receipts и подготовленным pending body. Результат: COEN=4 и next cohort marker от debit3. Использованы placeholder commitments и simulated preverified receipts; Groth16/MPC не запускались. Это execution consumer mismatch, не заявление о ложном cryptographic proof.

**Исправление подтверждено:** `cohort_bridge.py:49` создаёт request `{domain:wallet-statement,digest:sha(statement)}`; 65 включает его в MPC plan; `mpc_worker.py:92` переносит request в actor report; `cohort_bridge.py:83` включает его в signed body; `poc-vss.rs:139–144` требует equality report/body request для update. `run_lifecycle.py:258` передаёт actual statement; `cohort_bridge.py:105–107` сравнивает его digest и не допускает wallet certificate через authority-only call.

**Повторный executed check на окончательном source:** два cache-approved statements с одинаковым context и amounts 3/4; pending для3, commit4 теперь отвергнут с `cohort certificate does not bind this monetary statement/authority domain`. Проверены rollback old-note spent, отсутствие COEN и неизменность cohort state. Authority-only вызов с wallet certificate тоже отвергнут. Matching statement3 проходит: COEN=3 и cohort version увеличена атомарно. Это изолированный consumer execution с simulated cryptographic receipts, не full proof execution.

## RF-A-02 — Закрыт: report другого operation type не может подписать next_state

**Промежуточный дефект до последнего патча:** прежние `src/bin/poc-vss.rs:139–145,166,237–239`.

`computed` принимает `independent_floor=true` либо `private_limit_return=true`. При `money_cohort_conservation!=true` весь новый блок next_state validation пропускается, но подписывается полный body, включая произвольный `next_state`, если он передан.

**Source-derived counterexample:** body содержит next_state; report имеет matching context, checked_u256_valid=true, independent_floor=true, без money_cohort_conservation. Передать соответствующие output shares/polynomial hashes. Общие проверки проходят, next_state metadata/root/layout не читаются, signer подписывает body. Это не подделка MPC result: отсутствует type/domain requirement у certificate API. В штатном controller body/report сейчас подбираются согласованно; замечание относится к границе самостоятельной проверки signer.

**Исправление подтверждено source review:** `mpc_worker.py:73` объявляет operation из исполненного plan. `poc-vss.rs:139–158` требует equality body/report operation; update требует money_cohort_conservation, next_state и equality request. Intex/expiry требуют свой validity flag и отсутствие next_state. Значит, прежний non-update report с next_state отвергается до подписи. Для update money flag обязателен, поэтому блок полной metadata/root/mapping проверки 178–247 нельзя пропустить. `cohort_bridge.py:85–96` содержит native negative cases request/cross-operation; здесь они не запускались.

## Проверки и границы вывода

Executed lightweight consumer checks:

- duplicate input → `noncanonical input/output identity set`;
- mint без второго burn input → `operation asset/arity contract`;
- отсутствующий cohort pending → `mandatory money/cohort certificate missing`;
- renamed operation → `operation kind/id/owner binding`;
- stale timestamp → `stale money/cohort root, version or exact execution timestamp`, денежные writes откатились;
- statement mutation без receipt → `statement has no exact proof and registered-owner signature receipt`;
- другой **допущенный cache** statement при прежнем pending/context → принимался на промежуточном snapshot; после окончательного патча отвергнут с rollback, RF-A-01 закрыт;
- matching statement/certificate → принят, money/cohort обновлены в одной transaction;
- authority-only вызов wallet certificate → отвергнут.

Новый native signer не запускался; его closure и RF-A-02 проверены по полному source match arm. Проверка означает согласованность с report доверенного passive actor и корректным controller-provided plan. Она не доказывает аутентификацию MPC report против malicious host или независимую authority проверку всех прямых SQL writers. Full lifecycle, actual certificates, benchmark/RAM, MPC protocol security, wallet isolation и production support этим отчётом не подтверждаются.

| Snapshot file | SHA-256 |
|---|---|
| `run_lifecycle.py` | `3e7f09b71fa3b031a1961071064015320c6e349f0eeec505031e8c58f10846c7` |
| `cohort_bridge.py` | `d0c4c9eb8a6ae54290de4a485562aac9375d0e009e9ea95bccfb85b46f8fad7b` |
| `mpc_worker.py` | `f0f30a7028c79ad818bc38d6dab00f7216140bbdd7209991c46d04c369a1dfe2` |
| `src/bin/poc-vss.rs` | `7f74b030944cf51fd4e2c8fa98c36f2214f8935d7479a1ff1c47c531239381aa` |
