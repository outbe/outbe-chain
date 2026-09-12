# Сохранённый гибрид: Baby-Jubjub + Twisted ElGamal Gratis

**2026-09-12:** это предыдущая гибридная реализация с двумя группами. Для исходной цели сравнить полностью перенесённый приватный backend см. [исправленный Ristretto вариант](ristretto/README.md). Код и измерения гибрида оставлены для воспроизводимости.

Второй исполняемый backend для той же no-TEE цепочки. Baseline находится в [родительском каталоге](../README.md); его Rust/Python source, параметры и сохранённые результаты не изменены. Сравнение — [RESULTS](RESULTS.md). Это экспериментальная реализация, не код для production wallet/consensus.

## Что реализовано

1. Ristretto255 Twisted ElGamal: secret `s`, public `EK=s^-1 H`, ciphertext `(mG+rH,rEK)`. Каждая сумма uint256 представлена **16 независимыми chunks по 16 бит**. Нормализованная сумма восстанавливается по ключу и ciphertext без знания старых randomizers.
2. Bulletproofs подтверждают диапазоны fresh commitments каждого chunk. Σ-протокол связывает их с ciphertext через secret key: `H=sEK`, `C-F=sD-r_F H`. Это позволяет расходовать восстановленный баланс без старого encryption opening.
3. Сохранение денег доказано **локальными целочисленными равенствами с carries**. Для `left0+left1=right0+right1` применяется `t_i=u_i-128`, Bulletproofs на `u_i∈[0,256)`, `t_0=t_16=0`. Значение каждого локального выражения намного меньше порядка группы. Одного weighted equality modulo q здесь нет.
4. Связь Baby-Jubjub ↔ Ristretto: каждый из 256 бит имеет **joint OR proof** одного общего bit в обеих группах; затем доказаны weighted opening relations для четырёх Baby limbs и шестнадцати Ristretto chunks. Мост нужен при первом связывании нового Baby note/ciphertext. Для старого note используется уже проверенная registry entry.
5. Денежные `move` и `withdraw` проходят новый verifier. **Claim, mint и pledge сохраняют существующий Groth16 economics proof**, дополненный ciphertext/range/bridge proofs. Source P_L2/P_link, VSS S/S_l, Lysis/Nod и MPC Fidelity/Intex/expiry исполняются прежними компонентами.
6. Перевод между двумя владельцами: sender создаёт private debit и ciphertext суммы с двумя handles; recipient в отдельном процессе восстанавливает сумму своим ключом и создаёт credit proof. Нода связывает оба proof с одинаковой суммой и фиксирует два баланса и две истории Fidelity одной SQLite transaction.
7. Для cross-owner сумма доказанно положительна через отдельное `amount - aux = 1`, где оба значения — корректные uint256. Нулевой withdraw отвергается в verifier, как в baseline. Повторное чтение transfer proofs привязано к hashes **полностью проверенных Bundle**, сохранённым нодой в памяти.

## Точный маршрут source → COEN

```text
Source: настоящий P_L2
Wallet: canonical draft + nominal → P_link + Baby C(nominal)
Validators: P_L2/P_link check + VSS admission/rotation
Committee после close: публичные S и S_l
Lysis: публичные коэффициенты → Nod(Baby C(nominal), terms)
Wallet claim: Groth16 exact nominal×terms/payment → Baby notes
Wallet: Baby↔Ristretto bridge + encrypted notes + range/equality proofs
Node: все proofs + право Nod + money/Fidelity certificate → atomic commit
Wallet: private debit/credit → новые нормализованные encrypted notes
Wallet withdraw: proof допустимого списания публичной суммы
Node: закрытый остаток Gratis + публичный COEN output
```

P_link не перенесён на foreign-curve арифметику. Мост стоит **после economics proof claim**, поэтому исходный измеренный source circuit не разрастается. Цена такого выбора — дополнительные proofs и совместное хранение Baby/Ristretto представлений.

## Кто считает и хранит

| Роль | Вычисления | Хранение |
|---|---|---|
| Wallet owner | P_link, economics proof там, где сохранён; BP/Σ/bridge; exact uint256 arithmetic; recovery | Source witnesses/openings, собственный encryption DK, собственные private notes и данные восстановления Fidelity |
| Sender | Debit proof и два amount handles; positivity proof | Только собственный DK/баланс и известная ему сумма перевода; не DK/баланс получателя |
| Recipient | Decrypt amount, checked addition, credit proof | Только собственный DK/баланс и полученная сумма; не DK/баланс отправителя |
| Node/controller | Проверка proofs, owner signatures, key registration, note ownership/version, replay, atomic commit | Публичные proofs/ciphertexts, Baby commitments, registry, Nod, context, receipts; суммы withdraw/COEN публичны |
| VSS holders/MPC parties | S/S_l, late Fidelity, связанная история, Intex/expiry | Те же индивидуальные secret shares и committee state, что в baseline; полного DK пользователя у них нет |

**Money recovery и history recovery различаются.** DK+ciphertext достаточно, чтобы восстановить денежную сумму и строить DK-based equality proofs. Это не восстанавливает Baby blinders, source witnesses или Fidelity history. Для них сохранён прежний VSS/recovery/wallet witness маршрут. В JSON recovery reports выводятся только число восстановленных значений и время; сами числа записываются в `*.private.json` с mode0600.

Данные на этом host разделены по ролям/processes. Один OS administrator может читать все эти каталоги; это не аппаратная изоляция.

## Перевод между владельцами: профиль принятия

В этом эксперименте получатель **участвует в принятии**. Только после его proof отсутствия overflow нода атомарно меняет balances. Время Fidelity In/Out — момент этого commit. Это позволяет сохранить полный диапазон uint256 без предположения о максимальном lifetime supply и без принятия переполняющего account balance.

Непосредственное зачисление offline recipient в ограниченный uint256 available balance здесь не заявляется. Поток Aptos pending/available, caps и normalization для накопленных ненормализованных колонок — отдельная реализация. Базовый `add_columns` показывает алгебраический интерфейс, но не даёт права принять такую колонку как готовый uint256 balance. Исполненные денежные операции заменяют состояние на доказанно корректные нормализованные ciphertexts.

Production Gratis transfer сейчас запрещён. Cross-owner сценарий здесь — **явное экспериментальное расширение** с sender Fidelity Out / recipient Fidelity In. Он не меняет production precompile и не означает утверждение новой экономической политики. Основные сравниваемые money/Fidelity операции baseline сохранены.

## Воспроизведение

Нужна подготовленная среда [baseline](../README.md), включая native source helper, SRS, Groth16 parameters и MPyC. Из корня репозитория:

```sh
cargo build --offline --locked --release \
  --manifest-path testing/lysis-privacy-prototype/poc/variant2/Cargo.toml
cargo test --offline --locked --release \
  --manifest-path testing/lysis-privacy-prototype/poc/variant2/Cargo.toml \
  -- --test-threads=1
python3 testing/lysis-privacy-prototype/poc/variant2/run_variant.py \
  --count 256 --su 32 \
  --out testing/lysis-privacy-prototype/poc/variant2/runs/my-variant256
python3 testing/lysis-privacy-prototype/poc/variant2/compare.py \
  --run testing/lysis-privacy-prototype/poc/variant2/runs/my-variant256
```

Оба output каталога должны быть новыми; `compare.py` создаёт `public/paired-comparison` внутри существующего завершённого run. Для малого smoke profile достаточно `--count 4`. Минимум три owners: основной claimant, отдельный получатель и отдельный genesis-history fixture. 32 SU — измерительный профиль, не новый protocol maximum.

RSS limiter использует `ps`; MPC использует локальные TCP sockets. В среде, запрещающей эти операции, тест надо запускать с разрешением на локальные процессы/сокеты. Compilation/setup не входят в wallet budget; **proving, PK load, lookup-table initialization и self-verification входят**. Профиль RAM — 512 000 000 B на холодный wallet process; процессы последовательны. Browser/WASM/телефон не измеряются.

`compare.py` передаёт **те же private transition files** baseline prover, не читая их в controller. Для claim/mint/pledge уже выполненный retained Groth16 является точным paired baseline; повторять его не требуется. Сравнение времени включает сохранившийся Groth16, размера — его proof внутри нового bundle. Отдельно приведены node verify и MPC stages.

## Библиотеки и границы

Использованы обычные pinned Rust crates: `bulletproofs 5.0.0`, `curve25519-dalek 4.1.3`, `merlin 3.0.0`, arkworks0.5 из baseline; зависимости фиксирует самостоятельный Cargo.lock. Код Aptos/XELIS и их forks не копировался. Источники API: [Bulletproofs RangeProof](https://docs.rs/bulletproofs/5.0.0/bulletproofs/struct.RangeProof.html), [RistrettoPoint](https://docs.rs/curve25519-dalek/4.1.3/curve25519_dalek/ristretto/struct.RistrettoPoint.html); математическая проверка — [CRYPTO_DESIGN_REVIEW](CRYPTO_DESIGN_REVIEW.md).

Это собственная экспериментальная композиция примитивов. Joint OR bridge, transcript/domain adapters, registry и receiver acceptance не проходили внешний security audit. Committee остаётся passive honest-majority 2-of-3 с существующими production gates. Owner-key rotation, malicious MPC, consensus/reorg/DA, mobile side-channel/thermal testing и chunked threshold aggregation не реализованы этим вариантом. S/S_l по-прежнему получают через VSS, поэтому Ristretto ciphertext **не добавляется к каждому Tribute** до claim.

В `runs/` сохранены промежуточные registry snapshots, полные proofs и приватные fixtures. Это воспроизводимый эксперимент с избыточным retention, не готовая storage schema для миллиарда записей. Публичные отчётные evidence выбираются отдельно в `results/`; весь `runs/` публиковать нельзя.
