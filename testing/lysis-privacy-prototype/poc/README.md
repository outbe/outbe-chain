# No-TEE lifecycle PoC

Исполнимый host-эксперимент для [trace R00–R18](../PROTOCOL_TRACE_AND_REQUIREMENTS.md): реальный source ZKP и P_link, Pedersen VSS, смена держателей долей, раскрытие S/S_l, Lysis/Nod, приватные денежные transitions и связанная история Fidelity. Production-модули не меняются.

Основной профиль — **32 SU на TributeOffer**, без введения максимума протокола. Кошелёк ограничен **512 000 000 B peak RSS**. Все замеры относятся к текущему host; телефон с 2/4 GB — целевой класс устройства, не измеренная платформа.

- [Результаты и вывод о масштабе](RESULTS.md).
- [Полный Ristretto backend: исправленный вариант 2](variant2/ristretto/README.md). [Предыдущий гибрид](variant2/README.md) и [его измерения](variant2/RESULTS.md) сохранены отдельно.
- [Aptos/XELIS и Twisted ElGamal: исследование применимости](TWISTED_ELGAMAL_APPLICABILITY.md), [подробная проверка Aptos](APTOS_CONFIDENTIAL_ASSET_REVIEW.md). Исследовательские записки предшествуют реализации variant2; актуальные измерения приведены в его отдельном отчёте.
- [Кто считает, создаёт commitments и хранит данные; покрытие R00–R18](COVERAGE_AND_STORAGE.md).
- [Экспериментальные условия](PROFILE.md), [финальная проверка consumer/certificate](STATE_REVIEW_FINAL.md).
- [Исследование source P_L2](L2_SOURCE_FEASIBILITY.md), [MPC и мобильные ограничения](MPC_AND_MOBILE_FEASIBILITY.md), [точные MPC операции](MPC_EXACT_OPERATIONS.md). Это исследовательские записки; фактические запуски перечислены отдельно в RESULTS.

## Запуск в подготовленном host

Из корня `outbe-chain`:

```sh
python3 testing/lysis-privacy-prototype/poc/run_lifecycle.py \
  --count 256 --su 32 \
  --out testing/lysis-privacy-prototype/poc/runs/my-lifecycle256
```

Каталог `--out` должен быть новым. В `public/` находятся proofs, commitments, encrypted packets, receipts и метрики. В `private/` — ключи, wallet witnesses и отдельные каталоги держателей shares; в `control/` — задания и логи. `runs/`, `parameters/`, виртуальная среда и build artifacts исключены из Git. Не публикуйте весь каталог запуска: в нём есть приватные fixtures. Сохранённые отчётные данные в `results/` отобраны отдельно.

Реальные три MPC-процесса используют локальные TCP-порты от 17430. Два lifecycle запуска одновременно использовать эти порты не должны. Один процесс читает только свою VSS map; controller не читает `.private.` JSON. Отдельный **test oracle** намеренно имеет доступ к synthetic witnesses и возвращает только результаты сравнений. Одна OS-учётная запись не обеспечивает изоляцию от администратора host.

## Подготовка

Проверенная среда: macOS arm64, Rust/Cargo, Python 3.14, cached Barretenberg native library и локальный публичный SRS. Версии Rust dependencies фиксируют два Cargo.lock; source helper использует `outbe-circuits v0.14.0` (`984d57ed0d2f014a1a74d0b3b4b0769801957791`).

```sh
python3 -m venv testing/lysis-privacy-prototype/poc/.venv-mpc
testing/lysis-privacy-prototype/poc/.venv-mpc/bin/pip install \
  'mpyc @ git+https://github.com/lschoe/mpyc.git@38f06a7af688231fca4defe1613d01a2aa8bcbfb'
cargo build --offline --locked --release \
  --manifest-path testing/lysis-privacy-prototype/poc/Cargo.toml
```

Для source helper нужны совместимая native Barretenberg `.a` и `$HOME/.bb-crs/bn254_g1.dat`. Проверенный путь `.a` на этом host приведён ниже; hash каталога Cargo не переносим на другую машину. Укажите существующий подходящий `BB_LIB_DIR` при воспроизведении в другой среде. Offline build требует предварительно заполненного Cargo cache. Автоматическая установка native dependencies и скачивание SRS на чистой машине в этот PoC не входят.

```sh
BB_LIB_DIR="$PWD/target/release/build/barretenberg-rs-b78e1bff630b7ec2/out" \
CARGO_TARGET_DIR=target cargo build --offline --locked --release \
  --manifest-path testing/lysis-privacy-prototype/poc/source-helper/Cargo.toml
cargo test --offline --locked --release \
  --manifest-path testing/lysis-privacy-prototype/poc/Cargo.toml \
  --lib -- --test-threads=1
```

Groth16 setup автоматически создаётся отдельным server process при отсутствии нужных параметров. Это одноразовая **single-party test ceremony**, не production setup. Старые `parameters/claim` и аналогичные каталоги без `-v2` не используются текущими state circuits. Cold proof включает чтение и десериализацию PK, witness preparation, proving, самопроверку и serialization; setup учитывается отдельно.

## Дополнительные замеры

```sh
python3 testing/lysis-privacy-prototype/poc/measure_profiles.py growth \
  --out testing/lysis-privacy-prototype/poc/runs/my-su-growth
python3 testing/lysis-privacy-prototype/poc/measure_profiles.py parallel \
  --lifecycle testing/lysis-privacy-prototype/poc/runs/my-lifecycle256 \
  --out testing/lysis-privacy-prototype/poc/runs/my-parallel
```

Growth проверяет холодный P_link для 1/16/32/64/128/256 SU и останавливается на первом превышении RAM. Это конечные circuit capacities для эксперимента. Для неограниченного списка SU потребуется другой интерфейс доказательства — например, chunks с доказанной полнотой/уникальностью и финальной связью с исходным hash. Такой prover здесь не реализован.

Parallel сравнивает 32 разных P_link в одном и двух независимых процессах, по два Rayon threads на процесс. Каждый proof проверяется при создании. Это проверка параллелизма prover; она не измеряет TPS консенсуса или приёма сети.

## Код и границы доверия

`src/link.rs` доказывает canonical draft hash → exact nominal → **тот же** native Baby-Jubjub commitment. `source-helper` генерирует настоящий UltraHonk FullProof для Schnorr/IMT и проверяет его production verifier; четыре public поля должны совпасть с P_link. Локальный подписанный registry заменяет production root/owner registry и funding bridge.

`src/state.rs` доказывает денежные операции Groth16; каждое произвольное uint256 значение представлено четырьмя 64-bit limb commitments. `src/vss.rs` проверяет Pedersen shares и перенос между составами. `mpc_worker.py` исполняет точную целочисленную арифметику MPyC. `cohort_bridge.py` и SQLite связывают деньги и историю Fidelity одной атомарной записью с exact timestamp, account version, root и digest конкретного денежного statement.

MPC — **passive honest-majority, n=3, degree=1**, не malicious protocol. Комитетные подписи удостоверяют результат в этой модели; они не заменяют публичное доказательство вычисления. Генератор Pedersen H получен экспериментальным hash/decode/cofactor способом, не прошедшим криптографический аудит. Нет production consensus, OCOMP, reorg/Byzantine simulation, TLS/WAN, secure erasure, production trusted setup или мобильного порта. В отчёте эти границы отделены от реально выполненных криптографических проверок.
