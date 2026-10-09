//! ABI dispatch for the zero-fee paymaster precompile.
//!
//! The precompile exposes only view methods. `recordUse` is **not** an ABI
//! method. Only the executor pre-fee hook mutates the counter, through the
//! direct Rust function [`crate::record_sponsorship_use`]. A successful
//! [`crate::authorize_sponsorship`] gates that call. Out-of-band
//! `recordUse` calls would let a sponsored signer burn their own quota
//! through a regular sub-call. That sub-call would race the executor's
//! pre-fee write.

use alloy_primitives::Address;
use alloy_sol_types::sol;
#[allow(unused_imports)]
use outbe_macros::{contract_dispatch, contract_public, contract_view};
use outbe_primitives::{addresses::ZEROFEE_ADDRESS, error::Result, time::timestamp_to_date_key};

use crate::{constants::FREE_TX_DAILY_LIMIT, schema::ZeroFeeContract};

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IZeroFee.sol"
);

/// ABI surface for the ZeroFee paymaster precompile.
///
/// Two view methods, both anchored to the current block's UTC day so a
/// caller never has to supply or reconcile the day themselves:
///   - [`authorizeSponsorship`] - the bool "can this signer send a free
///     tx right now" predicate.
///   - [`getCounter`] - the effective `(day, count)` for today, with the
///     lazy day-reset already applied.
///
/// The raw packed slot (`date_key << 32 | count`) is still readable via
/// `eth_getStorageAt(ZEROFEE_ADDRESS, slot)` for anyone who needs the
/// pre-reset value. It is intentionally not a precompile method, because
/// it is trivially derivable and callers actually want the reset-applied
/// view.
#[contract_dispatch]
impl ZeroFeeContract<'_> {
    /// Returns `true` if `signer` would be admitted to the sponsored
    /// path for this block. Mirrors the executor's pre-fee gate exactly:
    /// rejects self-sponsorship and requires `effective_count <
    /// FREE_TX_DAILY_LIMIT` for today's UTC day key
    /// (`timestamp_to_date_key(block.timestamp)`).
    ///
    /// This is the canonical "may this signer use a free tx now?" RPC
    /// for off-chain wallets. A wallet can call it before it submits a
    /// sponsored transaction. The wallet can then surface `false` as a UX
    /// warning instead of waiting for a soft-failure receipt.
    #[contract_public("authorizeSponsorship(address) view returns (bool)")]
    #[contract_view]
    fn _abi_authorize_sponsorship(&mut self, signer: Address) -> Result<bool> {
        if signer == ZEROFEE_ADDRESS {
            return Ok(false);
        }
        let used = self.effective_count(signer, self.current_day()?)?;
        Ok(used < FREE_TX_DAILY_LIMIT)
    }

    /// Returns the EFFECTIVE `(day, count)` for `signer` as of the
    /// current block, with the lazy day-reset already applied. `day` is
    /// always today's UTC day key. `count` is 0 if the stored slot
    /// belongs to an earlier day (or was never written). A caller can
    /// therefore compute remaining free txs as
    /// `FREE_TX_DAILY_LIMIT - count` directly, without knowing or
    /// comparing the stored day.
    #[contract_public("getCounter(address) view returns (uint32,uint32)")]
    #[contract_view]
    fn _abi_get_counter(
        &mut self,
        signer: Address,
    ) -> Result<__ZeroFeeContractAbi::getCounterReturn> {
        let today = self.current_day()?;
        let count = self.effective_count(signer, today)?;
        Ok(__ZeroFeeContractAbi::getCounterReturn {
            _0: today,
            _1: count,
        })
    }
}

impl ZeroFeeContract<'_> {
    /// Current UTC day key derived from the block timestamp. Shared by
    /// the two view methods so both apply the lazy reset against the
    /// same day the executor would use.
    fn current_day(&self) -> Result<u32> {
        let now_secs = self.storage.timestamp()?.saturating_to::<u64>();
        Ok(timestamp_to_date_key(now_secs))
    }
}

#[cfg(test)]
mod tests {
    //! ABI dispatch round-trip tests. These exercise the generated
    //! `dispatch` entrypoint (selector decode -> method -> ABI encode).
    //! That code path is distinct from the runtime helpers. In
    //! particular, `authorizeSponsorship` reimplements the gate inline.
    //! These tests must verify it independently of `runtime::authorize_sponsorship`.

    use alloy_primitives::{address, Address, U256};
    use alloy_sol_types::SolCall;
    use outbe_primitives::{
        addresses::ZEROFEE_ADDRESS,
        storage::{hashmap::HashMapStorageProvider, StorageHandle},
        test_utils::sol_interface::sol_function_canonical,
    };

    use crate::schema::{pack_counter, ZeroFeeContract};

    // Private `sol!` interface the dispatch macro generated for this contract.
    use super::__ZeroFeeContractAbi as abi;

    const SIGNER: Address = address!("0x1111111111111111111111111111111111111111");
    // 2026-04-01 00:00:00 UTC -> date_key 20260401.
    const BLOCK_TS: u64 = 1_775_001_600;
    const BLOCK_DAY: u32 = 20_260_401;

    fn dispatch(storage: StorageHandle<'_>, data: &[u8]) -> Vec<u8> {
        super::dispatch(storage, data, Address::ZERO, U256::ZERO)
            .expect("dispatch should succeed")
            .to_vec()
    }

    #[test]
    fn get_counter_dispatch_returns_today_and_count_for_same_day() {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_timestamp(U256::from(BLOCK_TS));
        StorageHandle::enter(&mut provider, |storage| {
            ZeroFeeContract::new(storage.clone())
                .counter
                .write(&SIGNER, pack_counter(BLOCK_DAY, 5))
                .unwrap();
            let call = abi::getCounterCall { signer: SIGNER }.abi_encode();
            let out = dispatch(storage, &call);
            let ret = abi::getCounterCall::abi_decode_returns(&out).unwrap();
            assert_eq!(ret._0, BLOCK_DAY, "day must be today's UTC day key");
            assert_eq!(ret._1, 5, "same-day count is returned verbatim");
        });
    }

    #[test]
    fn get_counter_dispatch_applies_lazy_reset_across_day_boundary() {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_timestamp(U256::from(BLOCK_TS));
        StorageHandle::enter(&mut provider, |storage| {
            // Stored slot belongs to a PRIOR day -> getCounter must
            // report today with count 0 (lazy reset applied on read),
            // NOT the stale (day-1, 8) raw slot. This is the whole
            // reason getCounter is timestamp-anchored rather than raw.
            ZeroFeeContract::new(storage.clone())
                .counter
                .write(&SIGNER, pack_counter(BLOCK_DAY - 1, 8))
                .unwrap();
            let call = abi::getCounterCall { signer: SIGNER }.abi_encode();
            let out = dispatch(storage, &call);
            let ret = abi::getCounterCall::abi_decode_returns(&out).unwrap();
            assert_eq!(ret._0, BLOCK_DAY, "day must roll forward to today");
            assert_eq!(ret._1, 0, "stale-day count must lazily reset to 0 on read");
        });
    }

    #[test]
    fn get_counter_dispatch_zero_for_fresh_signer() {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_timestamp(U256::from(BLOCK_TS));
        StorageHandle::enter(&mut provider, |storage| {
            let call = abi::getCounterCall { signer: SIGNER }.abi_encode();
            let out = dispatch(storage, &call);
            let ret = abi::getCounterCall::abi_decode_returns(&out).unwrap();
            assert_eq!(ret._0, BLOCK_DAY);
            assert_eq!(ret._1, 0);
        });
    }

    #[test]
    fn authorize_sponsorship_dispatch_true_for_funded_under_quota() {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_balance(SIGNER, U256::from(1));
        provider.set_timestamp(U256::from(BLOCK_TS));
        StorageHandle::enter(&mut provider, |storage| {
            let call = abi::authorizeSponsorshipCall { signer: SIGNER }.abi_encode();
            let out = dispatch(storage, &call);
            let ok = abi::authorizeSponsorshipCall::abi_decode_returns(&out).unwrap();
            assert!(ok, "funded under-quota signer must be authorized");
        });
    }

    #[test]
    fn authorize_sponsorship_dispatch_true_for_zero_balance() {
        let mut provider = HashMapStorageProvider::new(1);
        // No balance set: native balance is not an eligibility signal.
        provider.set_timestamp(U256::from(BLOCK_TS));
        StorageHandle::enter(&mut provider, |storage| {
            let call = abi::authorizeSponsorshipCall { signer: SIGNER }.abi_encode();
            let out = dispatch(storage, &call);
            let ok = abi::authorizeSponsorshipCall::abi_decode_returns(&out).unwrap();
            assert!(ok, "zero-balance signer must be authorized under quota");
        });
    }

    #[test]
    fn authorize_sponsorship_dispatch_false_for_self_and_for_exhausted() {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_balance(SIGNER, U256::from(1));
        provider.set_balance(ZEROFEE_ADDRESS, U256::from(1));
        provider.set_timestamp(U256::from(BLOCK_TS));
        StorageHandle::enter(&mut provider, |storage| {
            // Self-sponsorship -> false.
            let self_call = abi::authorizeSponsorshipCall {
                signer: ZEROFEE_ADDRESS,
            }
            .abi_encode();
            let self_out = dispatch(storage.clone(), &self_call);
            assert!(
                !abi::authorizeSponsorshipCall::abi_decode_returns(&self_out).unwrap(),
                "paymaster must not authorize itself"
            );

            // Quota exhausted for today -> false.
            ZeroFeeContract::new(storage.clone())
                .counter
                .write(&SIGNER, pack_counter(BLOCK_DAY, crate::FREE_TX_DAILY_LIMIT))
                .unwrap();
            let call = abi::authorizeSponsorshipCall { signer: SIGNER }.abi_encode();
            let out = dispatch(storage, &call);
            assert!(
                !abi::authorizeSponsorshipCall::abi_decode_returns(&out).unwrap(),
                "exhausted-quota signer must NOT be authorized"
            );
        });
    }

    #[test]
    fn unknown_selector_is_rejected() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            // `recordUse(address,uint32)` selector is deliberately NOT in
            // the ABI. Any unknown selector must fail to dispatch, so a
            // signer cannot burn quota out-of-band.
            let bogus = [0xde, 0xad, 0xbe, 0xef];
            let res = super::dispatch(storage, &bogus, Address::ZERO, U256::ZERO);
            assert!(res.is_err(), "unknown selector must not dispatch");
        });
    }

    /// Drift guard between `contracts/precompiles/src/IZeroFee.sol` and the
    /// `#[contract_public(...)]` annotations on `ZeroFeeContract`. ZeroFee uses
    /// macro-driven dispatch, so the .sol file is a documentation /
    /// abi-export mirror rather than the dispatch source. This test fails if
    /// either side changes without the other.
    #[test]
    fn izerofee_sol_matches_contract_public_annotations() {
        const SOL: &str = include_str!("../../../../contracts/precompiles/src/IZeroFee.sol");
        let expected = [
            ("authorizeSponsorship", "address", true, "bool"),
            ("getCounter", "address", true, "uint32,uint32"),
        ];
        for (name, args_types, is_view, ret_types) in expected {
            let canon = sol_function_canonical(SOL, name)
                .unwrap_or_else(|| panic!("IZeroFee.sol is missing `function {name}(...)`"));
            assert_eq!(canon.arg_types, args_types, "{name}: arg types differ");
            assert_eq!(canon.is_view, is_view, "{name}: view-modifier differs");
            assert_eq!(canon.ret_types, ret_types, "{name}: return types differ");
        }
    }
}
