use alloy_primitives::{address, Address};

/// OriginMessenger on Outbe (outbound LayerZero sends).
/// CREATE3 proxy via outbe-intex Create3Factory, salt "outbe-intex:OriginMessenger:v1.0.0".
pub const ORIGIN_MESSENGER_ADDRESS: Address =
    address!("0x53c5DB9AAf0Ecf8A8c734b2d6C88fE2e56F2f955");

/// Reference-currency ISO for settlement (COEN/0xUSD = 840).
pub const QUALIFIER_REFERENCE_ISO: u16 = 840;

/// Issuance-currency ISO; fixed to USD (840) until multi-currency lands.
pub const QUALIFIER_ISSUANCE_ISO: u16 = 840;

/// Minimum-bid-quantity floor: 4% of the prior series' issued count (basis points).
pub const BID_QUANTITY_FLOOR_BPS: u32 = 400;

/// Promis load: 100k Promis per 1 Intex (scaled to 18-dec minor on the wire as `promis_load_minor`).
pub const PROMIS_LOAD: u128 = 100_000;

/// BNB-side auction phase timing: bid-reveal window before noon of the series day.
pub const REVEAL_WINDOW_SECONDS: u32 = 24 * 3600;
/// BNB-side auction phase timing: issuance window after noon of the series day.
pub const ISSUANCE_WINDOW_SECONDS: u32 = 12 * 3600;
