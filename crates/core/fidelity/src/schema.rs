use outbe_macros::contract;
use outbe_primitives::{addresses::FIDELITY_ADDRESS, storage::types::Slot};
/// Public aggregate anchor only. Private cohorts share the Gratis global journal.
#[contract(addr = FIDELITY_ADDRESS)]
pub struct FidelityContract {
    pub first_qualified_start: Slot<u64>,
}
