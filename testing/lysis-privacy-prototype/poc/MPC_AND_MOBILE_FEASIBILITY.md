# Исполнимый MPC baseline и границы мобильных измерений

Дата: 2026-09-11. Назначение: узкая интеграционная записка для текущего PoC, не новый полный аудит. Проверены локальная доступность инструментов, точные upstream API импорта долей и первичная мобильная документация. Этот исследователь не устанавливал backend, не запускал MPC, prover, сборку или телефонный benchmark. Только этот файл принадлежит автору; параллельные изменения PoC не проверялись целиком.

## Практическое решение

**Для первого настоящего многопроцессного исполнения рекомендован MPyC 0.11.2**, upstream commit `38f06a7af688231fca4defe1613d01a2aa8bcbfb`: три процесса, Shamir degree 1, каждый читает только собственные shares, вычисление и преобразование полей остаются внутри MPC. Это **passive / honest-majority baseline**, а не выполнение требования malicious privacy. Библиотека заявляет `t < n/2` пассивно повреждённых участников и не требует Python dependencies. [Pinned README](https://github.com/lschoe/mpyc/blob/38f06a7af688231fca4defe1613d01a2aa8bcbfb/README.md#L9), [package API](https://github.com/lschoe/mpyc/blob/38f06a7af688231fca4defe1613d01a2aa8bcbfb/mpyc/__init__.py#L1).

**Для malicious измерения — MP-SPDZ `malicious-shamir-party.x`**, либо MASCOT при другом corruption profile. Это конкретные доступные реализации, но они сейчас требуют установки зависимостей и native build. Их запуск сам по себе ещё не связывает inputs с source Pedersen VSS и не даёт publicly verifiable proof. Master уже требует эти дополнительные связи: `DEEP_RESEARCH_IMPLEMENTATION.md:349–351,393–402`.

| Кандидат | Проверенная локальная доступность | Что можно утверждать после соответствующего исполнения |
|---|---|---|
| MPyC | Python 3.14.6 / macOS arm64 есть; `find_spec('mpyc')` отсутствует. Upstream pure Python | Реальный обмен shares и закрытая арифметика при passive `n=3,t=1`; не malicious, не proactive/mobile theorem |
| MP-SPDZ | Не найден executable/checkout в проверенных roots. GMP/OpenSSL присутствуют в Homebrew; Boost/libsodium там не найдены | После secure native build: выбранная malicious модель с abort; не автоматическая VSS integration/DA/handoff |
| co-snarks | Не найден executable/checkout в проверенных roots; Rust доступен | Кандидат отдельного distributed proving шага. Circom/Noir integration, shared witness и source binding требуют работы; README experimental/un-audited |
| Android/iOS instrumentation | `adb` отсутствует; `xcrun xctrace list devices` завершился «unable to find utility xctrace» | Телефонные измерения этим окружением не подтверждены |
| WASM | `wasm-pack`, Node/npm есть; `emcc` не найден | Наличие toolchain не доказывает сборку данного prover и browser memory peak |

Отрицательные утверждения ограничены `PATH`, первым уровнем `/private/tmp`, `/Users/sakor/.cache`, `/Users/sakor/Library/Caches/pip`, `/Users/sakor/.cargo/git/checkouts`, cargo registry и Homebrew Cellar. Полная файловая система не обследовалась. Docker daemon не запускался и его состояние не проверялось. Graph MCP недоступен; использован task-directed source fallback, без заявления index/coverage.

## Импорт существующих shares без центрального раскрытия

В MPyC **нет необходимости заново вводить plaintext nominal владельцем**. `SecureObject.set_share` документирован как непосредственная установка локальной доли. Shamir координата процесса `pid` равна `pid+1`. Следующий фрагмент — проверенный по исходнику API sketch, ещё не выполненный интеграционный тест:

```python
from mpyc.runtime import mpc

# q — именно scalar modulus source Pedersen VSS, не BN254 pairing scalar.
Fq = mpc.SecFld(order=q, signed=False)
Wide = mpc.SecInt(l=512)  # экспериментальная ширина; bounds проверяются отдельно

def import_local_q_share(y_local):
    assert 0 <= y_local < q  # не допускаем молчаливое reduction парсером
    value = Fq()
    value.set_share(Fq.field(y_local))
    return value

async def main():
    await mpc.start()
    # Каждый процесс сам читает private/party-<pid>/...; общего plaintext файла нет.
    nominal_q = import_local_q_share(my_verified_nominal_share)
    nominal = mpc.convert(nominal_q, Wide)
    # Дальнейшая арифметика использует nominal как SecureInteger.
    # mpc.output(nominal) здесь НЕ вызывается.
    await mpc.shutdown()

mpc.run(main())
```

API evidence: [`asyncoro.py:154–166`](https://github.com/lschoe/mpyc/blob/38f06a7af688231fca4defe1613d01a2aa8bcbfb/mpyc/asyncoro.py#L154), [`sectypes.py:366–387,568–627`](https://github.com/lschoe/mpyc/blob/38f06a7af688231fca4defe1613d01a2aa8bcbfb/mpyc/sectypes.py#L366), [`thresha.py:23–43`](https://github.com/lschoe/mpyc/blob/38f06a7af688231fca4defe1613d01a2aa8bcbfb/mpyc/thresha.py#L23).

Условия корректного direct import:

1. Все процессы используют тот же prime `q`, одно committed polynomial/state version и epoch. Local shares проходят `G*y_i + H*z_i = Σ C_k*x_i^k` у текущего holder.
2. Degree не выше `mpc.threshold`; для baseline `n=3,t=1`. Shares с исходными произвольными holder IDs сначала проверяемо reshared на `x=1,2,3`; переименовать файлы недостаточно.
3. Исходное значение имеет однозначную целочисленную интерпретацию. Nominal104 помещается в `q`; произвольный `uint256` в Baby-Jubjub scalar не помещается. Для него импортируются четыре отдельно связанные 64-bit limb shares, затем внутри MPC вычисляется `Σ 2^(64*k)*limb[k]`.
4. Owner fixture creation и controller не должны затем читать все party files. Процессы на одном OS account дают протокольное разделение, но не изоляцию от администратора общей машины.

`mpc.convert(Fq_value, Wide)` реализован. Он открывает **замаскированное** значение и приватно убирает маску/производит reduction; coordinator plaintext не получает. Выход должен помещаться в target type. `SecInt(l=512,p=q251)` не является обходом ограничения: constructor отклоняет prime, если его bit length не превышает `l + security_parameter + 1`. Использовать отдельное широкое поле или limbs. [Pinned conversion](https://github.com/lschoe/mpyc/blob/38f06a7af688231fca4defe1613d01a2aa8bcbfb/mpyc/runtime.py#L691), [prime width guard](https://github.com/lschoe/mpyc/blob/38f06a7af688231fca4defe1613d01a2aa8bcbfb/mpyc/sectypes.py#L673).

Альтернативный импорт для иного committee layout: каждый старый holder приватно вводит свою долю через `mpc.input(..., senders=j)`, а новый MPC вычисляет `Σ λ_j*[y_j]` **внутри `Fq`**. Это реконструкция секретного значения в sharing representation, а не раскрытие. Нельзя интерпретировать старые `λ_j mod q` как обычные веса другого поля без private modular conversion. Этот вариант также требует binding введённых долей и membership/coverage checks.

## Что именно связно с Pedersen VSS

При passive baseline честное выполнение direct import переносит именно локально проверенную долю исходного commitment. Это условная интеграционная связь, достаточная для честно помеченного passive benchmark. `set_share` не делает VSS validation, MAC или proof; злонамеренный holder может подставить другую долю после локальной проверки.

Для malicious пути вход каждого старого holder должен быть связан с публичным `E_j=Σ C_k*x_j^k`. Два конкретных направления реализации: проверка `G*[y_j]+H*[z_j]=E_j` внутри active-secure MPC, либо proof equality между Pedersen evaluation opening и новым authenticated MPC input commitment. В обоих случаях `epoch,holder-id,source-id,field,old-root` входят в проверяемый контекст. Подпись сообщения «я проверил долю», финальный SNARK результата и наличие Shamir в обеих библиотеках по отдельности эту связь не устанавливают.

Входные cohort limbs, hidden flags/timestamps и authority forced operation должны быть связаны не только с nominal commitment, но и с **соответствующим account/Fidelity root**. Иначе правильно посчитанная лига может относиться к произвольной выдуманной истории. R16 authority, debit/burn и новый root проходят одну атомарную запись (`PROTOCOL_TRACE_AND_REQUIREMENTS.md:367–373`, `DEEP_RESEARCH_IMPLEMENTATION.md:412–425`).

## Exact Fidelity и forced writes

Для первой измеримой программы допустим отдельный **public-coefficient experimental profile**: закрытые размеры cohorts, заданные публичные `T_i`, `z`, `w`; `A=Σ a_i*T_i`, `D=A+Σ sold_i*T'_i`. Он проверяет multiplication/comparison/import cost. Публичность времён/структуры не следует из публичного execution timestamp: §7.2 и §7.4 master оставляют этот формат условным. Этот профиль нельзя отмечать как full hidden-history R06 PASS.

Практичный exact baseline:

1. Импортировать bounded limbs и converted wide integers. Для fixture с суммарно не более 16 cohort slots, каждым размером `<2^256`, `T,z,w<2^69` все `A,D` меньше `2^329`, `A*10^18<2^389`, а `k_m*D<2^458` при `z>=1`, `k_m=ceil(ceil(m*w/4096)*10^18/z)`. Signed `SecInt(512)` имеет запас **для этой области**, не для неограниченной lifetime history. Это алгебраический bound, не измерение.
2. Использовать сравнения математических целых, `mpc.if_else` и фиксированные/padded циклы. Не применять `SecFxp`, floating point или probabilistic truncation как замену consensus floor.
3. Для league использовать identity master `A*K >= k_m*D`. Самый простой закрытый baseline делает 4095 сравнений с публичной таблицей и суммирует secret bits, раскрывая только `league=slot+1`. Отдельно измерить 12 адаптивных сравнений: их disclosed direction bits не дают больше финального slot только при доказанной эквивалентности и одинаковой обработке нулевых случаев; это нужно явно учитывать в transcript policy.
4. Не потерять исходные checked failures. Широкий MPC позволяет вычислить математическое значение, но current source может законно отклонить операцию раньше: `crates/core/fidelity-math/src/lib.rs:48–78,83–88`. Если сохранение этих отказов требуется, проверить исходные intermediate `<2^256` в тех же точках **до debit/finality**. Успешная wide league не заменяет этот guard.
5. Для full hidden-history ветки импортировать времена и flags, выполнить exact `t_dec`, LIFO и sold splits внутри MPC с фиксированным trace. Source использует 63 fixed factors и целые floor (`crates/core/fidelity-math/src/lib.rs:91–175`), а не вещественную экспоненту. Slots, exhaustion и zero-amount ветки не должны становиться публичными из-за Python control flow.

Forced write без owner исполним как private input state → MPC conservation/authority/LIFO → новые **shares**, commitment и evidence. Одной league-программы для него недостаточно. MPC должен сохранить новые private witnesses; T2a evidence привязано к точному candidate timestamp/context; T2b проверяет совпадение и атомарно фиксирует log и деньги. При stale context/retry вычисление повторяется; полный witness не отправляется в runtime (`DEEP_RESEARCH_IMPLEMENTATION.md:410–427`).

**Private output:** `await mpc.output(x, receivers=[j])` раскрывает только участвующему online party `j`; прочие получают `None`. Это не API доставки отсутствующему владельцу. [`runtime.py:512–601`](https://github.com/lschoe/mpyc/blob/38f06a7af688231fca4defe1613d01a2aa8bcbfb/mpyc/runtime.py#L512).

Для owner offline минимальный recovery путь — каждый holder сохраняет свою новую долю и шифрует её под зарегистрированный owner recovery key; владелец позже получает достаточное число пакетов и проверяет восстановленный payload относительно canonical commitment/root. В malicious профиле пакет должен иметь проверяемую связь с new output share/commitment и durable receipt до finality. Это **интеграция поверх MPC**, не встроенная гарантия MPyC. Нельзя получить recovery package через `output(new_balance, receivers=[coordinator])`, а затем считать баланс скрытым от committee. Смена committee требует состояния долей, корректного handoff и новой канальной криптографии; перезапуск процесса не доказывает erasure PRSS seeds, backups или старых долей.

## План запуска, без выдачи его за выполненный benchmark

Следующие команды предназначены исполнителю PoC после создания программы; этим исследователем не запускались:

```sh
python3 -m venv testing/lysis-privacy-prototype/poc/.venv-mpc
testing/lysis-privacy-prototype/poc/.venv-mpc/bin/pip install --no-deps 'git+https://github.com/lschoe/mpyc.git@38f06a7af688231fca4defe1613d01a2aa8bcbfb'
testing/lysis-privacy-prototype/poc/.venv-mpc/bin/python testing/lysis-privacy-prototype/poc/mpc_fidelity.py -M3 -T1
```

`-M3` запускает три localhost процесса; это штатный upstream demo mode. При ручном запуске отдельным процессам задаётся `-I <pid>`, чтобы каждый читал собственный private directory. Для remote nodes нужны authenticated encrypted channels; loopback-run не измеряет WAN и не подтверждает channel privacy. [Official multiprocess example](https://github.com/lschoe/mpyc/blob/38f06a7af688231fca4defe1613d01a2aa8bcbfb/demos/helloworld.py#L1).

Сохранять отдельно: per-party elapsed/CPU/peak RSS, bytes sent/received, число rounds/barriers по фактической инструментализации, read/write bytes, max live cohort slots, field bits, security parameter, import/convert/compute/persist timings. Общий runtime/controller может хранить counts/digests/public outputs; private fixture oracle остаётся отдельным test actor. Для correctness сравнить explicit boundary fixtures с current Rust arithmetic; без malformed-share rejection теста не заявлять active input binding. Для owner-offline сценария завершить wallet process до READY/forcedOut и восстановить witness из сохранённых пакетов после перехода.

## Malicious альтернатива и публичная проверка

Upstream master pin для MP-SPDZ: `892ac0e2a2a9edabbe0249febc0b316ca649b479` (`DEEP_RESEARCH_LIBRARY_PINS.json:48–52`). Official docs подтверждают Apple Silicon, `malicious-shamir-party.x`, параметры `-N/-T`, encrypted channels и режимы secure preprocessing. Практический build target — только `make -j2 malicious-shamir-party.x`, после scoped установки GMP/libsodium/OpenSSL/Boost; MASCOT дополнительно требует libOTe. Не запускать общий `make` всех протоколов для этого вопроса. Программа компилируется `./compile.py -F <integer-bits> [-P <prime>] <program>`. [Official build/protocol guide](https://mp-spdz.readthedocs.io/en/latest/readme.html).

Для широкого поля проверить compile-time `GFP_MOD_SZ`, фактический prime и comparison requirements. Pinned CONFIG содержит настройку размера поля в 64-bit limbs; отсутствие ошибки small tutorial не подтверждает wide Fidelity. Не использовать `-DINSECURE`, `Fake-Offline` или повтор preprocessing material для private end-to-end PASS. [Pinned CONFIG](https://github.com/data61/MP-SPDZ/blob/892ac0e2a2a9edabbe0249febc0b316ca649b479/CONFIG#L82).

Malicious MPC даёт выбранную privacy/correctness-with-abort модель участвующим сторонам, а не готовый публичный proof состояния. Threshold certificate — отдельный доверительный профиль; collaborative SNARK — отдельный private distributed proving этап. co-snarks pin `23217ff78fc52f806420fd6d5c27563bea9c74bd` предоставляет Circom/Noir tooling, но сам README объявляет экспериментальность; наличие proof output не закрывает malicious prover privacy. [co-snarks README](https://github.com/TaceoLabs/co-snarks#disclaimer), [CRYPTO 2025 privacy pitfalls](https://eprint.iacr.org/2025/1026).

## Native/WASM и устройство с 2/4 GB

Wallet limit остаётся **512,000,000 B = 488.28125 MiB** для full cold P_link вместе с загрузкой параметров, witness preparation, proving и serialization (`poc/PROFILE.md:5`). 2/4 GB — вся физическая память устройства. Из неё не следует ни фиксированный app budget, ни успешность proof под desktop лимитом. Android различает managed heap limit и physical footprint/PSS; managed heap limit не равен native process RSS. [Android memory model](https://developer.android.com/topic/performance/memory-overview).

| Режим | Что измерять | Что этот замер не заменяет |
|---|---|---|
| Desktop ARM cold native | Новый процесс; full proof capacities 1/4/16/64; peak RSS, wall/CPU, parameter/proof bytes | Телефонный RAM pressure, allocator/ABI, thermal throttling |
| Android native | Та же circuit/parameters, release arm64; process RSS/high-water evidence плюс PSS/native heap; холодный процесс, затем серия proof; устройство/OS/build/threads | PSS или heap-only число не являются автоматически peak RSS |
| Browser WASM | Worker/renderer process memory и WASM linear-memory high water, JS buffers, parameter fetch/decode/copies; cold browser context и full serialization | `memory.buffer.byteLength` не включает весь процесс и не доказывает соблюдение wallet cap |
| iOS/native или WebKit | Реальное устройство, выбранный native/WebKit runtime и OS memory instrumentation | Android/desktop результаты не переносятся автоматически |

Android предоставляет `dumpsys meminfo` и `procstats` с RSS/PSS/USS. Их sampled maxima нужно помечать как sampled: короткий peak может быть пропущен. Для cap следует добавить high-water инструмент самого процесса или OS trace; сохранять raw metrics и интервал выборки. [Official dumpsys guide](https://developer.android.com/tools/dumpsys#meminfo).

Wasm32 может адресовать до 4 GiB linear memory в описанном V8 режиме; это верхняя адресная возможность, а не выделенная RAM телефона. Pthreads/SharedArrayBuffer требуют соответствующей browser поддержки и COOP/COEP. Поэтому native parallel proving и single-thread WASM должны иметь отдельные профили; число worker stacks и JS/WASM copies входит в footprint. [V8 4GB explanation](https://v8.dev/blog/4gb-wasm-memory), [Emscripten pthread requirements](https://emscripten.org/docs/porting/pthreads.html), [Chrome cross-origin isolation](https://web.dev/articles/coop-coep).

Продолжительность первого proof и устойчивый throughput отличаются: серия proof должна записывать thermal state/headroom и условия питания/охлаждения. Android Thermal API предоставляет соответствующие сигналы; они не заменяются температурой desktop host. [Official Thermal API](https://developer.android.com/games/optimize/adpf/thermal).

**Текущая граница:** установленные инструменты и upstream source подтверждены; исполненный MPC, full P_link mobile peak, device viability, active source binding, publicly verifiable forced writes и proactive recovery этим отчётом не подтверждаются. MPyC route конкретно реализуем для passive multiprocess baseline; полный requested lifecycle должен отдельно показывать execution/security status каждого R00–R18, как требует `poc/PROFILE.md:20–24`.
