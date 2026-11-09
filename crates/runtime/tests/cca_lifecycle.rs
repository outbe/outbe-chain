//! CCA custody, registration, and reward claims through real EVM transactions.

#[allow(dead_code, unused_imports)]
mod suite {
    include!("support/cca_harness.rs");
    include!("support/cca_bond_tests.rs");
    include!("support/cca_reward_tests.rs");
}
