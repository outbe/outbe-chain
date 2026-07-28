//! Narrow Factory-only mutation surface used by the compile-time Vote adapter.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::schema::{ReservationRecord, StablecoinFactoryContract};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactoryReservation {
    pub proposal_id: U256,
    pub token_id: B256,
    pub ticker: String,
    pub token: Address,
}

pub struct StablecoinFactoryApi;

impl StablecoinFactoryApi {
    pub fn reserve(storage: StorageHandle<'_>, reservation: &FactoryReservation) -> Result<()> {
        StablecoinFactoryContract::new(storage).reserve(reservation)
    }

    pub fn release(storage: StorageHandle<'_>, proposal_id: U256) -> Result<ReservationRecord> {
        StablecoinFactoryContract::new(storage).release(proposal_id)
    }

    pub fn consume(storage: StorageHandle<'_>, proposal_id: U256) -> Result<ReservationRecord> {
        StablecoinFactoryContract::new(storage).consume_and_register(proposal_id)
    }

    pub fn token_id_of(storage: StorageHandle<'_>, token: Address) -> Result<Option<B256>> {
        StablecoinFactoryContract::new(storage).registered_token_id(token)
    }
}
