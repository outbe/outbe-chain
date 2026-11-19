//! Private amount access; public Tribute queries never call this module.

use crate::confidential::{
    IssueNodFromTributeOutcomeV1, IssueNodFromTributeRequestV1, LysisFractionsRequestV1,
    LysisFractionsResultV1, TributeDayOpRequestV2,
};
use alloy_primitives::U256;
use outbe_core_common::{
    tribute_day_encryption::EncryptedTributeDayAmountV2,
    tribute_encryption::{EncryptedTributeV2, TributeAmountsV2},
};
use outbe_tee::TransportError;

pub(crate) fn apply_day_operation(
    request: TributeDayOpRequestV2,
) -> Result<EncryptedTributeDayAmountV2, TransportError> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) = test_enclave::with_keys(|key, state| {
        crate::confidential::apply_day_operation(key, state, &request)
    }) {
        return result.map_err(TransportError::enclave_error);
    }
    crate::confidential::host::apply_day_operation(request)
}

pub(crate) fn execute_lysis_fractions(
    request: LysisFractionsRequestV1,
) -> Result<LysisFractionsResultV1, TransportError> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) = test_enclave::with_keys(|key, state| {
        crate::confidential::execute_lysis_fractions(key, state, state, &request)
    }) {
        return result.map_err(TransportError::enclave_error);
    }
    crate::confidential::host::execute_lysis_fractions(request)
}

#[cfg(test)]
pub(crate) fn read_day_fractions(
    record: &outbe_core_common::tribute_day_encryption::EncryptedTributeDayFractionsV1,
) -> Result<std::collections::BTreeMap<u16, u64>, TransportError> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) =
        test_enclave::with_keys(|key, _| crate::confidential::read_day_fractions(key, record))
    {
        return result.map_err(TransportError::enclave_error);
    }
    crate::confidential::host::read_day_fractions(record.clone())
}

pub(crate) fn issue_nod_from_tribute(
    request: IssueNodFromTributeRequestV1,
) -> Result<IssueNodFromTributeOutcomeV1, TransportError> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) = test_enclave::with_keys(|key, state| {
        crate::confidential::issue_nod_from_tribute_pure(key, state, state, &request)
    }) {
        return result.map_err(TransportError::enclave_error);
    }
    crate::confidential::host::issue_nod_from_tribute(request)
}

pub(crate) fn read_day_amount(
    record: &EncryptedTributeDayAmountV2,
) -> Result<U256, TransportError> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) =
        test_enclave::with_keys(|key, _| crate::confidential::read_day_amount(key, record))
    {
        return result.map_err(TransportError::enclave_error);
    }
    crate::confidential::host::read_day_amount(record.clone())
}

pub(crate) fn read_amounts(
    record: &EncryptedTributeV2,
) -> Result<TributeAmountsV2, TransportError> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) =
        test_enclave::with_keys(|_, state| crate::confidential::decrypt_tribute(state, record))
    {
        return result.map_err(TransportError::enclave_error);
    }
    crate::confidential::host::read_tribute_amounts(vec![record.clone()])?
        .into_iter()
        .next()
        .ok_or(TransportError::UnexpectedResponse)
}

/// Explicit test seam using the real cryptographic engines, without transport.
/// Release builds contain neither this fixture secret nor this bypass: the crate
/// root rejects `all(feature = "test-enclave", feature = "production-enclave")`
/// at compile time and CI's `no-test-deps` gate rejects `test-enclave` in the
/// production dependency graph.
#[cfg(any(test, feature = "test-enclave"))]
pub mod test_enclave {
    use std::cell::Cell;
    thread_local! { static INSTALLED: Cell<bool> = const { Cell::new(false) }; }
    /// Opaque fixture capability; construction requires `outbe-tee/test-utils`.
    fn offer_key() -> outbe_tee::resident::OfferKeyHandle {
        outbe_tee::resident::OfferKeyHandle::for_test([0x5a; 32])
    }
    pub fn state_key() -> outbe_tee::resident::StateKeyHandle {
        outbe_tee::resident::StateKeyHandle::for_test([0x5a; 32])
    }
    pub struct Guard {
        previous: bool,
        _nod_guard: Option<outbe_nod::test_support::Guard>,
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
            let nod_guard = if self.installed {
                Some(outbe_nod::test_support::scope())
            } else {
                outbe_nod::test_support::uninstall();
                None
            };
            Guard {
                previous: INSTALLED.replace(self.installed),
                _nod_guard: nod_guard,
                _thread: std::marker::PhantomData,
            }
        }
    }
    pub fn scope() -> Guard {
        Context { installed: true }.install()
    }
    pub fn install() {
        INSTALLED.set(true);
        outbe_nod::test_support::install();
    }
    pub fn uninstall() {
        INSTALLED.set(false);
        outbe_nod::test_support::uninstall();
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
                crate::confidential::TributeDayOperationV2::AdjustTransient {
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
    ) -> outbe_primitives::error::Result<outbe_core_common::tribute_encryption::EncryptedTributeV2>
    {
        use outbe_core_common::tribute_encryption::{TributeAmountsV2, TributeContextV2};
        crate::confidential::encrypt_tribute(
            &state_key(),
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
        .map_err(|err| outbe_primitives::error::PrecompileError::FatalWithCause(Box::new(err)))
    }

    pub(super) fn with_keys<T>(
        f: impl FnOnce(&outbe_tee::resident::OfferKeyHandle, &outbe_tee::resident::StateKeyHandle) -> T,
    ) -> Option<T> {
        INSTALLED.get().then(|| f(&offer_key(), &state_key()))
    }
}
