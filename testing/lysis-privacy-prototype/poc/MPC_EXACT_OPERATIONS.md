# MPyC: точные операции и локальные Shamir shares

2026-09-11. Проверена установленная `.venv-mpc`: MPyC **0.11.2**, `direct_url.json` фиксирует upstream commit `38f06a7af688231fca4defe1613d01a2aa8bcbfb`. Ниже `mpyc/...` означает `testing/lysis-privacy-prototype/poc/.venv-mpc/lib/python3.14/site-packages/mpyc/...`. Использован exact-source fallback; graph tools отсутствуют, generation/coverage не заявляются. Примеры относятся к host PoC и passive `n=3,t=1`; 32 SU — основной workload profile, не protocol cap и не bound lifetime Fidelity history.

## 1. Деление

| Выражение над `SecInt` | Семантика установленного MPyC |
|---|---|
| `divmod(a, public_int)`, `a // public_int`, `a % public_int` | Точные integer quotient/remainder для публичного положительного делителя и корректных ranges |
| `a >> public_bits` | Точное `a // 2**public_bits` |
| `a / b`, `mpc.div(a,b)`, `mpc.reciprocal(b)` | Деление/inverse **в поле**. Не integer floor; даже при публичном `b` |
| `a // secret_b`, `a % secret_b` | Не поддержанная задача: реализация modulo читает локальную долю `b` как публичный divisor, а не выполняет private integer division. Нельзя рассчитывать на безопасный отказ API |
| `SecFxp` division, `mpc.trunc` | Не использовать для consensus floor: fixed-point reciprocal approximation / probabilistic truncation |

Основание: [`sectypes.py:197–241,279–285`](https://github.com/lschoe/mpyc/blob/38f06a7af688231fca4defe1613d01a2aa8bcbfb/mpyc/sectypes.py#L197), [`runtime.py:1170–1224,1825–1880`](https://github.com/lschoe/mpyc/blob/38f06a7af688231fca4defe1613d01a2aa8bcbfb/mpyc/runtime.py#L1170). `runtime.mod` вызывает `gather(b)`, то есть получает именно local share; public-only требование находится в docstrings, не в надёжной проверке происхождения аргумента.

```python
from mpyc.runtime import mpc

Wide = mpc.SecInt(512)

def divmod_public(a, divisor):
    # Для используемых здесь неотрицательных bounded integers.
    if type(divisor) is not int or not 0 < divisor < 2**510:
        raise ValueError("positive public divisor required")
    return divmod(a, divisor)  # два SecureInteger; ничего не раскрывает

# Например: amount18, rem = divmod_public(numerator, 10**6)
```

Неотрицательное `a` и все intermediate должны помещаться в declared signed range. Здесь `<2**510` оставляет запас для `SecInt(512)`; это ограничение helper profile, а не изменение `uint256` протокола. Public `a // b` вычисляет `r=a%b`, затем `(a-r)*b^{-1}`: точная делимость объясняет, почему field inverse на **этом** шаге даёт integer quotient.

Для **секретного положительного** делителя реализуемое решение — restoring binary long division. Цикл фиксирован public bit bound; ветвей Python по secret boolean нет:

```python
def divmod_secret_positive(a, b, a_bits):
    T = type(a)
    if type(b) is not T or T.frac_length != 0:
        raise TypeError("same secure integer type required")
    if not 1 <= a_bits <= T.bit_length - 2:
        raise ValueError("public numerator bit bound too large")
    # Preconditions proved/checked elsewhere:
    # 0 <= a < 2**a_bits; 0 < b < 2**(T.bit_length-2).
    bits = mpc.to_bits(a, l=a_bits)  # little-endian secret bits
    quotient, remainder = T(0), T(0)
    for bit in reversed(bits):
        remainder = 2*remainder + bit
        take = remainder >= b
        remainder = remainder - take*b
        quotient = 2*quotient + take
    return quotient, remainder
```

Инвариант после каждого шага: обработанный prefix равен `quotient*b+remainder`, `0<=remainder<b`. До вычитания `remainder<2*b`; поэтому достаточно одного secret compare/subtract. Результат точный и удовлетворяет `a=q*b+r`. Цена — `a_bits` последовательных сравнений и умножений; это baseline, не оптимизированный секретный divider. `to_bits`/`if_else` предоставлены runtime (`runtime.py:4337,2339`).

Если нулевой denominator допустим как input, сначала вычислить secret `valid=b>0`, заменить `b` на `mpc.if_else(valid,b,T(1))`, а затем связать `valid` с требуемым результатом/отказом **до записи денег**. Сам helper не доказывает preconditions и при `b=0` не имеет обещанной семантики. Не раскрывать intermediate numerator, remainder или guard ради отладки. Для league предпочтительнее уже выведенные threshold comparisons, когда они устраняют division; точный divider остаётся нужен для иных формул.

## 2. Импорт и экспорт в scalar field Baby-Jubjub

Точное `q` берётся из Rust `poc/src/crypto.rs:3,14–24`, где `Scalar=ark_ed_on_bn254::Fr`. В установленном ark-ed-on-bn254 0.5.0 `src/fields/fr.rs:4`:

```python
q = 2736030358979909402780800718157159386076813972158567259200215660948447373041
Fq = mpc.SecFld(order=q, signed=False)

def import_local_share(y):
    if type(y) is not int or not 0 <= y < q:
        raise ValueError("noncanonical local scalar share")
    value = Fq()
    value.set_share(Fq.field(y))
    return value

async def export_bounded_scalar_share(value_wide):
    # Precondition: represented SECRET integer is in [0,q).
    value_q = mpc.convert(value_wide, Fq)
    local = await mpc.gather(value_q)
    return int(local.value).to_bytes(32, "big")
```

`set_share` устанавливает локальную долю. `mpc.gather` ждёт local share Future, **не собирает доли других участников**; `mpc.output` выполняет reconstruction и здесь не нужен. `convert` выполняет protocol conversion с masked opening. Evidence: `asyncoro.py:144–166,250–273`, `runtime.py:66,691–787`, [`upstream local-share API`](https://github.com/lschoe/mpyc/blob/38f06a7af688231fca4defe1613d01a2aa8bcbfb/mpyc/asyncoro.py#L144).

Direct import требует `x_i=pid+1`, того же prime/epoch/state и polynomial degree `<=mpc.threshold`. Holder до импорта проверяет исходное Pedersen evaluation. Произвольные holder coordinates нельзя заменить на PID без корректного resharing. Эти API не добавляют malicious input authentication.

**Произвольный uint256 экспортируется четырьмя связанными limbs**, поскольку один Baby scalar не кодирует весь его диапазон:

```python
async def export_u256_local_shares(value_wide):
    # Precondition proved/checked: 0 <= value_wide < 2**256.
    bits = mpc.to_bits(value_wide, l=256)
    limbs = [mpc.from_bits(bits[i:i+64]) for i in range(0, 256, 64)]
    scalar_limbs = mpc.convert(limbs, Fq)
    local = await mpc.gather(scalar_limbs)
    return [int(v.value).to_bytes(32, "big") for v in local]

def import_u256_local_shares(encoded):
    if len(encoded) != 4 or any(len(v) != 32 for v in encoded):
        raise ValueError("four canonical scalar shares required")
    local = [import_local_share(int.from_bytes(v, "big")) for v in encoded]
    limbs = mpc.convert(local, Wide)
    return sum(limbs[k] * 2**(64*k) for k in range(4))
```

Здесь limb order — least significant first; внутри каждого scalar envelope используется fixed 32-byte big-endian. Import не проверяет hidden limb `<2**64`; это отдельное доказанное свойство admitted state. `to_bits(...,256)` возвращает младшие биты и тоже не является range proof: без precondition возникнет truncation alias. Нельзя делать `mpc.convert(arbitrary_u256,Fq)` и считать дальнейшее восстановление корректным. `convert` прямо предполагает попадание результата в target range и выбирает mask width по меньшему типу.

Новый sharing polynomial после conversion/арифметики обычно отличается от исходного. Exported `y_i` не должен проверяться против старых `C_k`/evaluation commitments. Для durable Pedersen VSS нужно получить также согласованные blinding shares и **новые** coefficient/evaluation commitments с binding к вычисленному output. Сохранить только bytes из helper — корректный MPyC checkpoint в passive профиле, но ещё не complete authenticated forced write. Private files содержат только долю своего процесса; `await mpc.output(value,receivers=[coordinator])` этот контракт нарушает.

## 3. Per-party network counters

```python
def peer_refs():
    # Вызывать после await mpc.start().
    return {p.pid: p.protocol for p in mpc.parties if p.pid != mpc.pid}

def sent_row(refs):
    return {pid: peer.nbytes_sent for pid, peer in refs.items()}

# refs = peer_refs()
# before = sent_row(refs)
# ... await mpc.gather(stage_results) ...
# after = sent_row(refs)
# delta = {j: after[j] - before[j] for j in before}
# await mpc.shutdown()
# final_including_shutdown = sent_row(refs)
```

`MessageExchanger.nbytes_sent` считает каждый framed message как `payload_size+12`: 8-byte program counter + 4-byte length (`asyncoro.py:54–64`). Initial PID/PRSS-key handshake идёт напрямую через transport и в этот счётчик не входит (`39–52`); TLS/TCP/IP headers/retransmissions также не входят. Корректное имя метрики: **MPyC framed application bytes sent**, не wire bytes.

`nbytes_received` в этом source отсутствует; `peer.bytes` — текущий входной buffer, не накопленный счётчик. Для recipient `i` cumulative received framed bytes можно получить из публичной directed matrix: `received[i]=Σ sent[j][i]`. Матрицу собирать из отдельных результатов процессов после окончания измеряемой работы. Сумма всех sent rows считает application transmission bytes один раз; сумма sent+received удваивает этот объём.

`shutdown()` печатает bytes **до** финального `transfer(self.pid)` (`runtime.py:299–325`). Если нужны bytes с завершением, сохранить peer references и прочитать после `await shutdown()`. Local `mpc.barrier()` ждёт pending computation; это не общий network barrier. Если добавлен `mpc.transfer` для согласованной границы фаз, отдельно учитывать его трафик. Определённые границы важнее сравнения двух несовместимых счётчиков.

## Выполненная проверка и ограничения

Проверка source и helper smoke test учитываются отдельно. Выполнены три отдельных процесса установленного Python/MPyC с `-M3 -T1 -I {0,1,2}`, `SecInt(512)`, localhost и общим 50-second deadline. Исходный sandbox запретил loopback bind (`PermissionError: Operation not permitted`); первый запуск остановлен watchdog. Разрешённый повтор вне этого ограничения завершился: **10/10 checks PASS на каждом процессе, exit 0**. Все процессы завершены, отдельные code/fixture files не создавались.

| Проверенная операция | Synthetic fixtures |
|---|---|
| Secret positive division, тело helper выше | `(0,1), (7,3), (1023,31), (999,1), (3,100)`; public numerator bound 10 bits, тип SecInt512; обе компоненты сравнивались с Python `divmod` |
| Public exact division | `a=2**255+1234567890123456789012345`, делители `1`, `10**18`, `2**64` |
| Direct q-share import → wide → q-share export → import | Значение `2**103+117`, degree-1 fixture polynomial, проверено восстановленное значение |
| Uint256 → 4 limbs → q shares → import → wide uint256 | То же 256-bit `a`, проверено полное восстановление |

Input fixtures преднамеренно синтетические и известны test oracle. MPC outputs раскрывались для проверки именно этих fixtures; никаких пользовательских inputs не использовалось. Это исполнимая проверка арифметики/API и межпроцессного обмена, не experimental evidence конфиденциальности реальных данных. Проверка не покрывает весь 512-bit numerator space, invalid preconditions или malicious tampering.

Фактические `nbytes_sent` всей серии после `shutdown()`:

| Отправитель → получатель | Bytes |
|---|---:|
| 0 → 1 | 4,019,713 |
| 0 → 2 | 3,955,857 |
| 1 → 0 | 3,955,777 |
| 1 → 2 | 4,019,633 |
| 2 → 0 | 4,019,233 |
| 2 → 1 | 3,955,377 |
| Все directed transmissions | **23,925,590** |

Финальный shutdown добавил 17 bytes на каждое направление, суммарно 102 bytes. Эти числа относятся к **всей** проверке с тестовыми input/output/conversion операциями; это не стоимость одного division и не wire bytes. RSS и throughput здесь не измерялись. Полный lifecycle, новые Pedersen commitments, persistence/erasure, malicious model и распределённая публичная проверка не исследовались этой запиской.
