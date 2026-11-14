//! Domain-specific elevated sponsored gas ceilings for EIP-7702 paymaster transactions.

use alloy_primitives::Address;
use outbe_system_common::addresses::TRIBUTE_FACTORY_ADDRESS;

/// Maximum sponsored gas limit for `TributeFactory.offerTribute`.
pub const FREE_TX_TRIBUTE_FACTORY_GAS_LIMIT: u64 = 8_000_000;

/// `offerTribute(bytes,bytes,uint256,uint32,uint16,uint16,bytes,uint32,string,bytes,bytes,bytes)`.
const OFFER_TRIBUTE_SELECTOR: [u8; 4] = [0x7c, 0x65, 0xe0, 0x88];

/// Operation-specific sponsored gas ceilings, keyed by `(target, selector)`.
pub const ELEVATED_SPONSORED_GAS: &[(Address, [u8; 4], u64)] = &[(
    TRIBUTE_FACTORY_ADDRESS,
    OFFER_TRIBUTE_SELECTOR,
    FREE_TX_TRIBUTE_FACTORY_GAS_LIMIT,
)];
