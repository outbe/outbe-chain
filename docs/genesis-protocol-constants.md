# Параметры протокола в genesis

Outbe хранит изменяемые между сетями, но неизменяемые внутри одной сети
параметры времени в `genesis.json`. Это позволяет LocalNet проходить полный
путь Tribute → Metadosis → OCOMP → Lysis → NOD за минуты, сохраняя тот же
production-код и те же переходы состояний.

## Как это работает

1. В `config.outbeProtocol` указываются только нужные переопределения.
2. `outbe-chain constants genesis` дополняет отсутствующие поля production-
   значениями, проверяет набор и записывает его в immutable account
   `0x000000000000000000000000000000000000ee11`.
3. Account входит в genesis state root и genesis hash.
4. Только после этого создаются OCOMP bindings и регистрации валидаторов.
5. Во время исполнения `outbe-chain-constants` читает запись один раз,
   проверяет schema/hash и кэширует `Arc<GenesisProtocolParametersV1>` по
   `genesis_hash`. Metadosis, Cycle и OCOMP не знают, было поле задано явно или
   получено из default.

Runtime API для изменения значений отсутствует. Потерянная, повреждённая или
несовместимая запись является фатальной ошибкой конфигурации сети; runtime не
подставляет default молча.

## Поддерживаемые поля

| JSON path | Тип/единица | Default | Допустимо | Использует |
|---|---:|---:|---:|---|
| `metadosis.formingPeriodSeconds` | `u64`, секунды | 180000 (50 ч) | `1..=180000` | граница FORMING |
| `metadosis.lookbackDelaySeconds` | `u64`, секунды | 1807200 (502 ч) | `0..=1807200` | граница LOOKBACK |
| `metadosis.offeringPeriodSeconds` | `u64`, секунды | 180000 (50 ч) | `1..=180000` | окно Tribute offers |
| `metadosis.waitingPeriodSeconds` | `u64`, секунды | 43200 (12 ч) | `1..=43200` | переход WAITING → READY |
| `metadosis.bootstrapDurationSeconds` | `u64`, секунды | 1814400 (504 ч) | `1..=1814400` | bootstrap Metadosis |
| `metadosis.advanceIntervalSeconds` | `u64`, секунды | 43200 (production noon lane) | `1..=43200` | период dedicated WWD advancement в коротком genesis-профиле |
| `ocomp.computeVoteWindowBlocks` | `u64`, блоки | 1800 | `1..=1800` и capacity gates | общий срок вычисления и включения vote |

Значения могут только сокращать production-default в этой версии. Ноль
разрешён только для lookback. Неизвестные поля, неверный `schemaVersion`,
переполнение и небезопасные значения останавливают генерацию genesis.

## Пример production

Поля можно не указывать: отсутствие `outbeProtocol` означает полный набор
default, который всё равно материализуется в `EE11`.

```json
{
  "config": {
    "chainId": 54322346
  }
}
```

## Пример LocalNet

```json
{
  "config": {
    "outbeProtocol": {
      "schemaVersion": 1,
      "metadosis": {
        "formingPeriodSeconds": 60,
        "lookbackDelaySeconds": 0,
        "offeringPeriodSeconds": 120,
        "waitingPeriodSeconds": 30,
        "bootstrapDurationSeconds": 300,
        "advanceIntervalSeconds": 10
      },
      "ocomp": {
        "computeVoteWindowBlocks": 120
      }
    }
  }
}
```

`formingPeriodSeconds` считается от канонического UTC+14-начала WorldwideDay,
полученного как `WorldwideDay::from_timestamp(timestamp блока 1)`, а не от
сырой UTC-даты и не от момента запуска ноды. После 10:00 UTC ключ WorldwideDay
уже соответствует следующей календарной дате. Поэтому
LocalNet harness записывает в genesis не буквальный `60`, а
`seconds_since_that_wwd_start + 60`: FORMING заканчивается примерно через минуту
после блока 1, при этом смысл consensus-поля не меняется. Значение `60` в
примере применимо к genesis, созданному точно на этой границе WorldwideDay.

LocalNet не получает заранее созданный далеко истекающий OFFERING-день.
Первый WorldwideDay создаётся на block 1 и проходит обычный reducer по коротким
genesis-bound периодам.

## Порядок подготовки сети

```text
base/prefund
  → seed_genesis.py (config.outbeProtocol и обычный state seed)
  → outbe-chain constants genesis (immutable EE11)
  → outbe-chain ocomp bindings / keygen / genesis
  → outbe-chain tee genesis
```

`seed_genesis.py` поддерживает ключ `protocol_constants` в seed JSON и переносит
его в `config.outbeProtocol`, но не знает slot ordinals. Единственный владелец
storage-layout и hash — Rust crate `crates/blockchain/constants`.

Изменение параметра меняет genesis hash. Для существующей сети это не upgrade:
требуется wipe и новый genesis.
