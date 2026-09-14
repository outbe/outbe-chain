//! Persistent CCA records and the current active-agent index.
use crate::precompile::ICca;
use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::{
    addresses::CCA_REGISTRY_ADDRESS,
    storage::types::{Storable, StorableType},
};

#[derive(Debug, Clone)]
#[storage_record(exists_field = name)]
pub struct CcaRecord {
    #[key]
    pub cca: Address,
    #[attribute(order = 0)]
    pub name: String,
    #[attribute(order = 1, default = ICca::State::Bonding)]
    pub state: ICca::State,
    /// Native COEN atomic units, retained during deregistration until claimed.
    #[attribute(order = 2)]
    pub bonded_amount: U256,
    /// Unix seconds; checked conversion from the execution timestamp.
    #[attribute(order = 3)]
    pub unbond_unlocks_after: u64,
}

#[storage_schema]
#[contract(addr = CCA_REGISTRY_ADDRESS)]
pub struct CcaContract {
    #[attribute(order = 0)]
    pub records: outbe_primitives::storage::dsl::Map<Address, CcaRecord>,
    #[attribute(order = 1)]
    pub active: outbe_primitives::storage::dsl::Set<Address>,
    /// Six-decimal net GRATIS per UTC day. At most one of weight/deficit is nonzero.
    #[attribute(order = 2)]
    pub gratis_sum_per_utc_day: outbe_primitives::storage::dsl::Map<B256, U256>,
    /// Excess burns offset later openings for the same CCA and UTC day only.
    #[attribute(order = 3)]
    pub gratis_deficits_per_utc_day: outbe_primitives::storage::dsl::Map<B256, U256>,
    /// Claimable native COEN atomic units, independent of the bond.
    #[attribute(order = 4)]
    pub reward_amounts: outbe_primitives::storage::dsl::Map<Address, U256>,
}

pub(crate) fn address_day_key(cca: Address, day: u32) -> B256 {
    let mut bytes = [0u8; 24];
    bytes[..20].copy_from_slice(cca.as_slice());
    bytes[20..].copy_from_slice(&day.to_be_bytes());
    keccak256(bytes)
}

impl StorableType for ICca::State {
    const SLOTS: usize = 1;
}

impl Storable for ICca::State {
    fn from_word(word: U256) -> Self {
        // Check the full word fits u8 before decoding the enum; never truncate storage.
        // Storable is infallible, so validation rejects this sentinel at the record boundary.
        u8::try_from(word)
            .ok()
            .and_then(|value| Self::try_from(value).ok())
            .unwrap_or(Self::__Invalid)
    }

    fn to_word(&self) -> U256 {
        U256::from(u8::from(*self))
    }
}
