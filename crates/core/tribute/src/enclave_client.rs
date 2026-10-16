//! Private amount access; public Tribute queries never call this module.

use alloy_primitives::U256;
use outbe_primitives::{
    tribute_day_encryption::EncryptedTributeDayAmountV2,
    tribute_encryption::{EncryptedTributeV2, TributeAmountsV2},
};
use outbe_tee::{tribute_day::TributeDayOpRequestV2, TransportError};

pub(crate) fn apply_day_operation(
    request: TributeDayOpRequestV2,
) -> Result<EncryptedTributeDayAmountV2, TransportError> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) = test_enclave::with_key(|key| {
        outbe_tee_enclave::tribute_day::apply_day_operation(key, &request)
    }) {
        return result.map_err(crate::errors::enclave_transport_error);
    }
    outbe_tee::tribute_day_client::apply_day_operation(request)
}

pub(crate) fn read_day_amount(
    record: &EncryptedTributeDayAmountV2,
) -> Result<U256, TransportError> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) =
        test_enclave::with_key(|key| outbe_tee_enclave::tribute_day::read_day_amount(key, record))
    {
        return result.map_err(crate::errors::enclave_transport_error);
    }
    outbe_tee::tribute_day_client::read_day_amount(record)
}

pub(crate) fn read_amounts(
    record: &EncryptedTributeV2,
) -> Result<TributeAmountsV2, TransportError> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) = test_enclave::with_key(|key| {
        outbe_tee_enclave::tribute_encryption::decrypt_tribute(key, record)
    }) {
        return result.map_err(crate::errors::enclave_transport_error);
    }
    outbe_tee::tribute_client::read_tribute_amounts(std::slice::from_ref(record))?
        .into_iter()
        .next()
        .ok_or(TransportError::UnexpectedResponse)
}

/// Explicit test seam using the real cryptographic engines, without transport.
/// Release builds contain neither this fixture secret nor this bypass.
#[cfg(any(test, feature = "test-enclave"))]
pub mod test_enclave {
    use std::cell::Cell;
    thread_local! { static INSTALLED: Cell<bool> = const { Cell::new(false) }; }
    pub const NETWORK_SECRET: [u8; 32] = [0x5a; 32];
    pub struct Guard {
        previous: bool,
        _thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            INSTALLED.set(self.previous);
        }
    }
    /// Captures only the test routing flag, without moving secret material.
    #[derive(Clone, Copy)]
    pub struct Context {
        installed: bool,
    }
    pub fn capture() -> Context {
        Context {
            installed: INSTALLED.get(),
        }
    }
    impl Context {
        pub fn install(self) -> Guard {
            Guard {
                previous: INSTALLED.replace(self.installed),
                _thread: std::marker::PhantomData,
            }
        }
    }
    pub fn scope() -> Guard {
        Context { installed: true }.install()
    }
    pub fn install() {
        INSTALLED.set(true);
    }
    pub fn uninstall() {
        INSTALLED.set(false);
    }
    /// Seeds encrypted storage for legacy numerical calculation fixtures.
    pub fn seed_day_totals(
        contract: &mut crate::TributeContract<'_>,
        totals: &crate::DayTotals,
    ) -> outbe_primitives::error::Result<()> {
        install();
        let previous = contract.day_nominal_amount(totals.worldwide_day, false)?;
        let next = totals.tribute_nominal_total_minor;
        if next != previous {
            let add = next > previous;
            let delta = if add {
                next - previous
            } else {
                previous - next
            };
            contract.apply_day_amount(
                totals.worldwide_day,
                outbe_tee::tribute_day::TributeDayOperationV2::AdjustTransient {
                    nominal_amount_minor: delta,
                    add,
                },
                alloy_primitives::B256::repeat_byte(0x53),
            )?;
        }
        contract.store_day_totals(totals)
    }
    pub fn encrypted_fixture(
        tribute: &crate::TributeData,
        chain_id: u64,
    ) -> outbe_primitives::error::Result<outbe_primitives::tribute_encryption::EncryptedTributeV2>
    {
        use outbe_primitives::tribute_encryption::{TributeAmountsV2, TributeContextV2};
        outbe_tee_enclave::tribute_encryption::encrypt_tribute(
            &NETWORK_SECRET,
            &outbe_tee_enclave::crypto::x25519_public(&[0x77; 32]),
            TributeContextV2 {
                chain_id,
                tribute_id: tribute.tribute_id,
                owner: tribute.owner,
                worldwide_day: tribute.worldwide_day,
                issuance_currency: tribute.issuance_currency,
                reference_currency: tribute.reference_currency,
                tribute_price_minor: tribute.tribute_price_minor,
                offer_input_hash: alloy_primitives::keccak256(tribute.tribute_id.as_slice()),
            },
            &TributeAmountsV2 {
                issuance_amount_minor: tribute.issuance_amount_minor,
                nominal_amount_minor: tribute.nominal_amount_minor,
            },
        )
        .map_err(crate::errors::body_read_corruption)
    }

    pub(super) fn with_key<T>(f: impl FnOnce(&[u8; 32]) -> T) -> Option<T> {
        INSTALLED.get().then(|| f(&NETWORK_SECRET))
    }
}
