use alloy_primitives::{address, Address};

/// OriginMessenger on Outbe (outbound LayerZero sends).
/// CREATE3 proxy via outbe-intex Create3Factory, salt "outbe-intex:OriginMessenger:v1.0.0".
pub const ORIGIN_MESSENGER_ADDRESS: Address =
    address!("0xc9AdCCa96217c4329265b722c11a7186c2D85263");

/// Reference-currency ISO for settlement (COEN/0xUSD = 840).
pub const QUALIFIER_REFERENCE_ISO: u16 = 840;

/// Minimum-bid-quantity floor: 4% of the prior series' issued count (basis points).
pub const BID_QUANTITY_FLOOR_BPS: u32 = 400;

/// BNB-side auction phase timing: bid-reveal window before noon of the series day.
pub const REVEAL_WINDOW_SECONDS: u32 = 12 * 3600;
/// BNB-side auction phase timing: issuance window after noon of the series day.
pub const ISSUANCE_WINDOW_SECONDS: u32 = 24 * 3600;

/// Issuance config defaults forwarded to IntexFactory at clearing.
pub const DEFAULT_INTEX_CALL_PERIOD: u32 = 21 * 24 * 3600;
pub const DEFAULT_CALL_WINDOW_DAYS: u16 = 30;
pub const DEFAULT_CALL_THRESHOLD_DAYS: u16 = 20;
