use alloy_primitives::{Address, U256};

/// BCD form of [`MAX_ISO_CODE`]; addresses above it are token addresses.
const MAX_ISO_CODE_BCD: u16 = 0x0999;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairType {
    /// Native token i.e. COEN
    Native,
    /// Arbitrary ERC20 token address
    ERC20(Address),
    /// ISO 4217 currency code
    IsoCurrency(u16),
}

impl From<PairType> for Address {
    /// A currency code is packed as BCD into the last two bytes, so ISO 840
    /// becomes `0x0000…0840`. `ERC20` addresses inside that reserved range
    /// decode back as `IsoCurrency`.
    fn from(value: PairType) -> Self {
        match value {
            PairType::Native => Address::ZERO,
            PairType::ERC20(address) => address,
            PairType::IsoCurrency(code) => {
                Address::left_padding_from(&bcd_encode(code).to_be_bytes())
            }
        }
    }
}

impl From<Address> for PairType {
    fn from(value: Address) -> Self {
        let raw = U256::from_be_slice(value.as_slice());
        if raw.is_zero() {
            PairType::Native
        } else if let Some(code) = bcd_decode(raw) {
            PairType::IsoCurrency(code)
        } else {
            PairType::ERC20(value)
        }
    }
}

pub struct PairKey {
    pub address1: Address,
    pub address2: Address,
}

impl PairKey {
    pub fn new(address1: Address, address2: Address) -> Self {
        assert_ne!(address1, address2);
        if address1 > address2 {
            Self {
                address1: address2,
                address2: address1,
            }
        } else {
            Self { address1, address2 }
        }
    }

    pub fn pair(self) -> (PairType, PairType) {
        (PairType::from(self.address1), PairType::from(self.address2))
    }

}

/// One decimal digit per nibble: `840 -> 0x0840`. Only the four lowest decimal
/// digits fit, which is exact for every code in `0..=MAX_ISO_CODE`.
fn bcd_encode(code: u16) -> u16 {
    let mut rest = code % 10_000;
    let mut packed = 0u16;
    for shift in [0u32, 4, 8, 12] {
        packed |= (rest % 10) << shift;
        rest /= 10;
    }
    packed
}

/// Inverse of [`bcd_encode`], or `None` when `raw` is not the BCD form of a
/// value in `0..=MAX_ISO_CODE`.
fn bcd_decode(raw: U256) -> Option<u16> {
    if raw > U256::from(MAX_ISO_CODE_BCD) {
        return None;
    }
    let packed = raw.saturating_to::<u16>();
    let mut code = 0u16;
    for shift in [8u32, 4, 0] {
        let digit = (packed >> shift) & 0xf;
        if digit > 9 {
            return None;
        }
        code = code * 10 + digit;
    }
    Some(code)
}

#[cfg(test)]
mod tests {
    use crate::types::PairType;
    use alloy_primitives::{address, Address};

    /// Highest ISO 4217 numeric currency code.
    const MAX_ISO_CODE: u16 = 999;

    #[test]
    fn native_round_trips_through_the_zero_address() {
        assert_eq!(Address::from(PairType::Native), Address::ZERO);
        assert_eq!(PairType::from(Address::ZERO), PairType::Native);
    }

    #[test]
    fn iso_currency_encodes_each_decimal_digit_as_a_nibble() {
        assert_eq!(
            Address::from(PairType::IsoCurrency(840)),
            address!("0x0000000000000000000000000000000000000840"),
        );
        assert_eq!(
            Address::from(PairType::IsoCurrency(8)),
            address!("0x0000000000000000000000000000000000000008"),
        );
        assert_eq!(
            Address::from(PairType::IsoCurrency(MAX_ISO_CODE)),
            address!("0x0000000000000000000000000000000000000999"),
        );
    }

    #[test]
    fn iso_currency_round_trips_every_code_in_range() {
        for code in 1..=MAX_ISO_CODE {
            let pair = PairType::IsoCurrency(code);
            assert_eq!(PairType::from(Address::from(pair)), pair, "iso code {code}");
        }
    }

    #[test]
    fn erc20_round_trips_through_its_own_address() {
        let token = address!("0x000000000000000000000000000000000000099a");
        assert_eq!(Address::from(PairType::ERC20(token)), token);
        assert_eq!(PairType::from(token), PairType::ERC20(token));
    }

    #[test]
    fn an_address_above_the_iso_range_decodes_as_erc20() {
        let token = address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
        assert_eq!(PairType::from(token), PairType::ERC20(token));
    }

    #[test]
    fn an_address_with_a_non_decimal_nibble_decodes_as_erc20() {
        for token in [
            address!("0x00000000000000000000000000000000000008a0"),
            address!("0x000000000000000000000000000000000000084f"),
            address!("0x0000000000000000000000000000000000001000"),
            address!("0x0000000000000000000000000000000000010840"),
        ] {
            assert_eq!(PairType::from(token), PairType::ERC20(token), "{token}");
        }
    }
}
