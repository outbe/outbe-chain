# HyperlaneController

`outbe-hyperlanecontroller` is the governance-owned controller of the Hyperlane
bridge. It is the owner of every Hyperlane core contract on Outbe (Mailbox,
ProxyAdmin, IGP, gas oracle, ProtocolFee, InterchainAccountRouter,
StorageMessageIdMultisigIsm) and, through its Interchain Account (ICA), of the
same contracts on Ethereum / BSC. The single deployer key that owns them today
is replaced by this precompile.

Address: `0x000000000000000000000000000000000000EE14` (`HYPERLANE_CONTROLLER_ADDRESS`).

## State

| slot | field           | meaning                                                               |
|------|-----------------|-----------------------------------------------------------------------|
| 0    | `router`        | InterchainAccountRouter on Outbe; zero = not initialized              |
| 1    | `ism_by_domain` | `domain -> StorageMessageIdMultisigIsm`, Outbe included under its own domain (= chain id) |
| 2    | `domains`       | enumerable list of the configured domains                             |

Validator sets and thresholds are **not** stored here: the ISMs are the source
of truth, the controller only forwards owner calls to them.

## Direct selectors

- `initialize(router, domains[], isms[])` — one-shot. Caller must be the current
  owner of the Outbe ISM (the deployer that staged `transferOwnership` to the
  controller). The controller `acceptOwnership()`s the ISM (it is Ownable2Step),
  verifies it already owns `router`, and stores the table.
- `fund()` payable — tops up the balance that pays IGP fees for ICA dispatches.
- views: `router()`, `ismByDomain(domain)`, `domains()`.

## Owner operations (Rust methods, trigger not wired yet)

| method                                   | effect                                                                 |
|------------------------------------------|------------------------------------------------------------------------|
| `add_validator(v, threshold?)`           | reads the current set from the Outbe ISM, appends, pushes to all ISMs  |
| `remove_validator(v, threshold?)`        | same, removing                                                         |
| `set_threshold(t)`                       | same set, new threshold                                                |
| `set_validators_and_threshold(set, t)`   | full rotation: `callRemote` to every remote ISM, then the local setter; one checkpoint |
| `call_remote(domain, calls[])`           | generic ICA call on a remote chain (e.g. `acceptOwnership()` on a remote ISM) |
| `call_local(to, value, data)`            | generic owner call on Outbe (IGP, ProtocolFee, ProxyAdmin, router …)   |
| `add_domain(domain, ism)` / `remove_domain(domain)` | connect / disconnect a remote chain; the local domain is fixed at `initialize` |

Remote dispatch quotes `quoteGasPayment(domain)` on the router and pays it from
the controller's balance; an insufficient balance reverts before anything is sent.

## Deploy order (hyperline)

1. `mise run deploy-contracts` — stock contracts, owner = deployer key.
2. `mise run ica:enroll` — link the InterchainAccountRouters both ways.
3. `mise run owner:transfer` — Outbe contracts → controller, remote contracts →
   the controller's ICA (computed on the remote router). The ISM transfer is
   two-step: it stays *pending* until accepted.
4. `initialize(...)` from the deployer key (still `owner()` of the Outbe ISM).
5. `fund()`.
6. `call_remote(domain, [ism.acceptOwnership()])` per remote chain.
7. Test rotation; verify `validatorsAndThreshold` changed on the remote ISM.
