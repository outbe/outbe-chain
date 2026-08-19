//! ABI decode, dispatch and encode for the CCA registry precompile.
//!
//! A Credis Card Agent originates Credis positions on behalf of card owners.
//! `CredisFactory` needs to gate origination on the agent's standing, so the
//! query side of that contract — [`ICca`] — is published and routed now; the
//! registry that would answer it truthfully is not built yet.
//!
//! Until it is, every address reads back `Active`, which keeps the pre-registry
//! behaviour (nobody is gated) while callers migrate onto the real ABI.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};

use outbe_primitives::dispatch::{dispatch_call, reject_value, view};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!("../../../contracts/precompiles/src/ICca.sol");

/// Storage is unused while the registry is a stub; the parameter stays to match
/// the route table's dispatch signature.
pub fn dispatch(
    _storage: StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    dispatch_call(data, ICca::ICcaCalls::abi_decode, |call| {
        use ICca::ICcaCalls::*;
        match call {
            getCcaState(c) => view(c, |c| Ok(cca_state(c.cca))),
            supportsInterface(c) => view(c, |c| {
                let id: [u8; 4] = c.interfaceId.0;
                Ok(id == ERC165_INTERFACE_ID)
            }),
        }
    })
}

/// Registration state of `cca`.
///
/// TODO(cca): implement the registry. This must become a storage lookup that
/// returns [`ICca::State::Unknown`] for an address that never registered, and
/// the recorded state otherwise. `Unknown` exists in the ABI precisely so an
/// unregistered agent is distinguishable from an active one — the stub cannot
/// make that distinction, so callers must not treat `Active` as proof of
/// registration until this is replaced.
fn cca_state(_cca: Address) -> ICca::State {
    ICca::State::Active
}
