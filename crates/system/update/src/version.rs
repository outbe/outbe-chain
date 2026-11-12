//! Const functions for protocol version parsing and formatting.
//!
//! The version type lives in `outbe-primitives`. This module re-exports it and
//! keeps the compile-time package-version parsers used by this crate.

pub use outbe_primitives::protocol_version::{
    encode_protocol_version, format_protocol_version, protocol_version_major,
    protocol_version_minor, try_parse_protocol_version, try_parse_protocol_version_major_component,
    try_parse_protocol_version_minor_component, ProtocolVersion, ProtocolVersionParseError,
};

pub(super) const fn parse_protocol_version_major_component(input: &str) -> u8 {
    match try_parse_protocol_version_major_component(input) {
        Ok(value) => value,
        Err(_) => panic!("invalid protocol version major component"),
    }
}

pub(super) const fn parse_protocol_version_minor_component(input: &str) -> u32 {
    match try_parse_protocol_version_minor_component(input) {
        Ok(value) => value,
        Err(_) => panic!("invalid protocol version minor component"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use outbe_primitives::storage::types::Storable;

    #[test]
    fn parse_protocol_version_is_const() {
        const VERSION: ProtocolVersion = const {
            match try_parse_protocol_version("1.2") {
                Ok(version) => version,
                Err(_) => panic!("invalid protocol version"),
            }
        };
        assert_eq!(VERSION, encode_protocol_version(1, 2));
    }

    #[test]
    fn parses_major_minor_and_raw_versions() {
        assert_eq!(
            try_parse_protocol_version("1.2").unwrap(),
            encode_protocol_version(1, 2)
        );
        assert_eq!(try_parse_protocol_version("65536").unwrap().raw(), 65536);
    }

    #[test]
    fn rejects_invalid_protocol_versions() {
        assert_eq!(
            try_parse_protocol_version("1.2.3").unwrap_err(),
            ProtocolVersionParseError::TooManyComponents
        );
        assert_eq!(
            try_parse_protocol_version("1.").unwrap_err(),
            ProtocolVersionParseError::Empty
        );
        assert_eq!(
            try_parse_protocol_version("256.0").unwrap_err(),
            ProtocolVersionParseError::Overflow
        );
    }

    #[test]
    fn protocol_version_errors_follow_component_scan_then_decimal_order() {
        use ProtocolVersionParseError::*;

        for (input, expected) in [
            ("", Empty),
            (".", Empty),
            (".a", Empty),
            ("a.", InvalidDigit),
            ("256.a", Overflow),
            ("a.4294967296", InvalidDigit),
            ("1..a", TooManyComponents),
            ("a.2.3", TooManyComponents),
            ("9999999999..", TooManyComponents),
            ("0.16777216", Overflow),
            ("4294967296", Overflow),
            ("4294967296x", Overflow),
        ] {
            assert_eq!(try_parse_protocol_version(input), Err(expected), "{input}");
        }
    }

    #[test]
    fn formats_protocol_version_with_raw_value() {
        assert_eq!(encode_protocol_version(1, 2).to_string(), "v1.2");
        assert_eq!(
            format_protocol_version(encode_protocol_version(1, 2)),
            "v1.2 (16777218)"
        );
    }

    #[test]
    fn from_word_saturates_oversized_values() {
        assert_eq!(
            ProtocolVersion::from_word(U256::from(u32::MAX)),
            ProtocolVersion::from_raw(u32::MAX)
        );
        assert_eq!(
            ProtocolVersion::from_word(U256::MAX),
            ProtocolVersion::from_raw(u32::MAX)
        );
    }
}
