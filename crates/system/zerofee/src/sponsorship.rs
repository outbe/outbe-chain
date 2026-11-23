//! Domain-specific elevated sponsored gas ceilings for EIP-7702 paymaster transactions.

use alloy_primitives::Address;
use outbe_system_common::addresses::NOD_FACTORY_ADDRESS;

/// Maximum sponsored gas limit for `NodFactory.issueNod`.
pub const FREE_TX_ISSUE_NOD_GAS_LIMIT: u64 = 8_000_000;

/// `issueNod(bytes,bytes,uint256,uint32,uint16,uint16,bytes,uint32,string,bytes,bytes)`.
const ISSUE_NOD_SELECTOR: [u8; 4] = [0x8d, 0x62, 0x15, 0x42];

/// Operation-specific sponsored gas ceilings, keyed by `(target, selector)`.
pub const ELEVATED_SPONSORED_GAS: &[(Address, [u8; 4], u64)] = &[(
    NOD_FACTORY_ADDRESS,
    ISSUE_NOD_SELECTOR,
    FREE_TX_ISSUE_NOD_GAS_LIMIT,
)];
