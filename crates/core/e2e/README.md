# outbe-e2e

Cross-module end-to-end tests for Outbe runtime flows live in this crate.

Current coverage is limited to the production governance, vote and update
flows in `tests/governance_lifecycle.rs`, `tests/governance_vote_flow.rs` and
`tests/update_flow_spec.rs`.

The former direct `WWD -> synchronous Lysis` scenarios were removed when the
fresh-devnet contract made verified OCOMP the sole populated positive-gratis
Metadosis path. Keeping those tests would preserve a forbidden no-profile
execution semantics and require raw Metadosis fixture writes. The replacement
request/finality/open/vote/activation/replay evidence lives in
`crates/blockchain/evm/tests/ocomp_request_lifecycle.rs`; public fresh-devnet
evidence remains in `crates/testing/e2e-harness`.

Run the remaining suite with:

```sh
cargo test -p outbe-e2e
```
